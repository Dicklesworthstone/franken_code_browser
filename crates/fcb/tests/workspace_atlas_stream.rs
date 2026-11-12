#![forbid(unsafe_code)]
#![cfg(all(feature = "map", feature = "search", unix))]

//! Public production engines over actual open files; no mock matcher/decoder.
use std::{fs::{self, File}, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, FileId, SourceRevision, Size2D};
use fcb::map::{LayoutRevision, LayoutOptions, QueryGeneration, ResourceAllocationId, ResourceBudget, RootId};
use fcb::map::workspace::{WorkspaceAtlas, WorkspaceAtlasLimits};
use fcb::map::workspace::retained::RetainedWorkspaceAtlas;
use fcb::map::workspace::stream_search::{AtlasStreamError, AtlasStreamFileState, AtlasStreamLimits,
    AtlasStreamReport, AtlasStreamSearch, AtlasStreamState, AtlasStreamStop};
use fcb::search::{RawPath, SearchManifestId, StreamReadState, StreamReadStep, StreamingNeedle};
use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceLimits, WorkspaceStage};
use fcb::source::{CancelFlag, SourceError};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(7901).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn revision(n: u64) -> SourceRevision { SourceRevision::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-atlas-stream-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.0.join(path); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
    fn catalog(&self, budget: &ResourceBudget, max_files: usize) -> WorkspaceCatalog {
        let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&self.0));
        let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).unwrap(),
            FileId::new(owner(), 100).unwrap(), WorkspaceLimits { max_files, max_file_bytes: 0,
                max_source_bytes: 0, ..WorkspaceLimits::default() }, false, budget, allocation(1)).unwrap();
        let cancel = CancelFlag::new();
        for _ in 0..4097 {
            if catalog.stage() != WorkspaceStage::Discovering { break; }
            catalog.step(&cancel).unwrap();
        }
        assert_eq!(catalog.stage(), WorkspaceStage::Ready); catalog
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn atlas<'a>(catalog: &'a WorkspaceCatalog, budget: &ResourceBudget, alloc: u64) -> WorkspaceAtlas<'a> {
    WorkspaceAtlas::build(catalog, &fcb::map::workspace::AtlasScope::All, LayoutRevision::new(owner(), 1).unwrap(), Size2D::new(1024.0, 768.0).unwrap(),
        LayoutOptions::modest(), WorkspaceAtlasLimits::default(), budget, allocation(alloc), || false).unwrap()
}
fn run<'a, 'c>(atlas: &'a WorkspaceAtlas<'c>, literal: &str, limits: AtlasStreamLimits,
    budget: &ResourceBudget, base: u64) -> AtlasStreamReport<'a, 'c> {
    run_mode(atlas, literal, limits, budget, base, false)
}
fn run_mode<'a, 'c>(atlas: &'a WorkspaceAtlas<'c>, literal: &str, limits: AtlasStreamLimits,
    budget: &ResourceBudget, base: u64, count_all_matches: bool) -> AtlasStreamReport<'a, 'c> {
    let needle = StreamingNeedle::text(owner(), literal, budget, allocation(base)).unwrap();
    let mut job = if count_all_matches {
        AtlasStreamSearch::new_counting(atlas, &needle, revision(base * 100), generation(base), limits,
            budget, [allocation(base + 1), allocation(base + 2)])
    } else {
        AtlasStreamSearch::new(atlas, &needle, revision(base * 100), generation(base), limits,
            budget, [allocation(base + 1), allocation(base + 2)])
    }.unwrap();
    let root = atlas.catalog().grant().root_path().to_path_buf();
    for _ in 0..100_000 {
        if job.state() != AtlasStreamState::Pending { break; }
        job.step(StreamReadStep::default(), generation(base), |_, path|
            File::open(root.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable), || false).unwrap();
    }
    assert_eq!(job.state(), AtlasStreamState::Finished);
    job.finish().unwrap() // Report deliberately outlives compiled needle/worker.
}

#[test]
fn finds_a_hit_beyond_capture_limits_without_retaining_the_skipped_source() {
    let root = Fixture::new();
    let mut bytes = vec![b'a'; 2 * 1024 * 1024 + 7];
    let start = bytes.len() as u64; bytes.extend_from_slice(b"needle"); root.write("large.rs", &bytes);
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    assert_eq!(catalog.limits().max_file_bytes, 0);
    let report = run(&atlas, "needle", AtlasStreamLimits::default(), &budget, 10);
    assert!(report.is_complete()); assert_eq!(report.hits().len(), 1);
    assert_eq!(report.hits()[0].original_range().start().get(), start);
    assert_eq!(report.stats().bytes_read, bytes.len() as u64);
    assert!(report.stats().peak_input_buffer_bytes <= 16 * 1024);
    assert_eq!(report.retained_witness_bytes(), 6);
    assert!(report.reserved_bytes() < 128 * 1024, "source length must not enter retained admission");
    let selected = report.select_hit(0, &atlas, generation(10)).unwrap();
    assert_eq!(selected.original_bytes(), b"needle");
}

#[test]
fn scoped_occurrences_stream_large_files_and_count_without_retaining_extra_rows() {
    let root = Fixture::new();
    let mut bytes = vec![b'x'; 600 * 1024];
    bytes.extend_from_slice(b" foo foobar foo");
    root.write("a.rs", &bytes);
    let mut utf16 = vec![0xff, 0xfe];
    for unit in "foo".encode_utf16() { utf16.extend_from_slice(&unit.to_le_bytes()); }
    root.write("b.rs", &utf16);
    root.write("c.rs", b"foo foo foo");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::identifier(owner(), "foo", &budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new_scoped_counting(&atlas, &needle, revision(100), generation(10),
        AtlasStreamLimits { max_matches: 1, ..Default::default() }, &[0, 1],
        &budget, [allocation(11), allocation(12)]).unwrap();
    for _ in 0..10_000 {
        if job.state() != AtlasStreamState::Pending { break; }
        job.step(StreamReadStep::default(), generation(10), |_, path| {
            assert_ne!(path.raw().to_path_buf(), PathBuf::from("c.rs"));
            File::open(root.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable)
        }, || false).unwrap();
    }
    let report = job.finish().unwrap();
    assert_eq!(report.mode(), fcb::search::StreamingMode::Identifier);
    assert!(report.case_sensitive());
    assert!(report.is_complete());
    assert_eq!(report.excluded_files(), 1);
    assert_eq!(report.stats().matches_seen, 3);
    assert_eq!(report.hits().len(), 1);
    assert_eq!(report.matching_files_seen(), 2);
    assert!(report.hits_truncated());
    assert_eq!(report.retained_witness_bytes(), 3);
    assert_eq!(report.retained_matched_text_bytes(), 0);
    let selection = report.select_hit(0, &atlas, generation(10)).unwrap();
    assert_eq!(selection.matched_text(), "foo");
    assert_eq!(selection.original_bytes(), b"foo");
    assert_eq!(selection.hit().original_range().start().get(), 600 * 1024 + 1);
}

#[test]
fn cross_buffer_utf8_and_both_utf16_orders_keep_original_ranges_and_witnesses() {
    let root = Fixture::new(); let needle = "x😀y";
    let mut utf8 = vec![b'a'; 16 * 1024 - 2]; utf8.extend_from_slice(needle.as_bytes()); root.write("a.rs", &utf8);
    for (path, little) in [("b.rs", true), ("c.rs", false)] {
        let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in format!("{}{needle}", "a".repeat(8190)).encode_utf16() {
            bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
        }
        root.write(path, &bytes);
    }
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let report = run(&atlas, needle, AtlasStreamLimits::default(), &budget, 10);
    assert!(report.is_complete()); assert_eq!(report.hits().len(), 3);
    for (i, hit) in report.hits().iter().enumerate() {
        assert_eq!(hit.original_range().start().get(), 16 * 1024 - 2);
        let selected = report.select_hit(i, &atlas, generation(10)).unwrap();
        assert_eq!(selected.matched_text(), needle);
        let original = fs::read(root.0.join(atlas.entry(hit.node()).unwrap().path().raw().to_path_buf())).unwrap();
        let (a, b) = hit.original_range().as_usize_bounds().unwrap();
        assert_eq!(selected.original_bytes(), &original[a..b]);
    }
}

#[test]
fn byte_allowance_is_global_and_unexamined_files_are_not_negative_results() {
    let root = Fixture::new(); root.write("a.rs", b"nothing"); root.write("b.rs", b"needle"); root.write("c.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let report = run(&atlas, "needle", AtlasStreamLimits { max_bytes: 8, ..Default::default() }, &budget, 10);
    assert!(!report.is_complete()); assert_eq!(report.stats().bytes_read, 8);
    assert_eq!(report.files_examined(), 2); assert_eq!(report.unexamined_files(), 1);
    assert_eq!(report.stop_reason(), Some(AtlasStreamStop::ByteLimit)); assert!(report.hits().is_empty());
}

#[test]
fn read_call_allowance_is_global_and_checked_before_opening_another_file() {
    let root = Fixture::new(); root.write("a.rs", b"no"); root.write("b.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let report = run(&atlas, "needle", AtlasStreamLimits { max_read_calls: 1, ..Default::default() }, &budget, 10);
    assert_eq!(report.stats().read_calls, 1); assert_eq!(report.files_examined(), 1);
    assert_eq!(report.unexamined_files(), 1); assert_eq!(report.stop_reason(), Some(AtlasStreamStop::ReadCallLimit));
}

#[test]
fn a_full_result_buffer_does_not_falsely_truncate_until_cross_file_lookahead() {
    let root = Fixture::new(); root.write("a.rs", b"needle"); root.write("b.rs", b"nothing");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let limits = AtlasStreamLimits { max_matches: 1, ..Default::default() };
    let complete = run(&atlas, "needle", limits, &budget, 10);
    assert!(complete.is_complete()); assert_eq!(complete.stats().matches_seen, 1);
    root.write("b.rs", b"needle");
    let partial = run(&atlas, "needle", limits, &budget, 20);
    assert!(!partial.is_complete()); assert_eq!(partial.stats().matches_seen, 2);
    assert_eq!(partial.hits().len(), 1); assert_eq!(partial.stop_reason(), Some(AtlasStreamStop::MatchLimit));
    assert_eq!(complete.select_hit(0, &atlas, generation(10)).unwrap().original_bytes(), b"needle");
}

#[test]
fn counted_search_finishes_later_files_and_aggregates_unretained_occurrences_by_directory() {
    let root = Fixture::new();
    root.write("src/a.rs", b"aaaa"); root.write("src/deep/b.rs", b"aa");
    root.write("src/empty.rs", b""); root.write("src-old/c.rs", b"aaa");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let index = atlas.index(&budget, allocation(3), || false).unwrap();
    let limits = AtlasStreamLimits { max_matches: 1, ..Default::default() };
    let legacy = run(&atlas, "aa", limits, &budget, 10);
    assert!(!legacy.count_all_matches()); assert!(!legacy.is_complete());
    assert_eq!(legacy.stats().matches_seen, 2); assert_eq!(legacy.files_examined(), 1);
    let report = run_mode(&atlas, "aa", limits, &budget, 20, true);
    assert!(report.count_all_matches()); assert!(report.is_complete()); assert!(report.hits_truncated());
    assert_eq!(report.stop_reason(), None); assert_eq!(report.files_examined(), 4);
    assert_eq!(report.stats().matches_seen, 6); assert_eq!(report.matching_files_seen(), 3);
    assert_eq!(report.hits().len(), 1); assert_eq!(report.retained_witness_bytes(), 2);
    assert_eq!(report.retained_matches_in(index.root_node()).unwrap(), (1, 1));
    assert_eq!(report.counted_matches_in(index.root_node()).unwrap(), (6, 3));
    assert_eq!(report.counted_matches_in(index.find_path(b"src").unwrap()).unwrap(), (4, 2));
    assert_eq!(report.counted_matches_in(index.find_path(b"src/deep").unwrap()).unwrap(), (1, 1));
    assert_eq!(report.counted_matches_in(index.find_path(b"src/a.rs").unwrap()).unwrap(), (3, 1));
    assert_eq!(report.counted_matches_in(index.find_path(b"src/empty.rs").unwrap()).unwrap(), (0, 0));
    assert_eq!(report.counted_matches_in(index.find_path(b"src-old").unwrap()).unwrap(), (2, 1));
    assert_eq!(report.retained_matches_in(index.find_path(b"src").unwrap()).unwrap(), (0, 0));
    assert_eq!(report.select_hit(0, &atlas, generation(20)).unwrap().original_bytes(), b"aa");
    assert_eq!(report.reserved_bytes(), legacy.reserved_bytes(), "count totals use the admitted file table");
}

#[test]
fn counted_scoped_zero_retention_counts_utf16_and_overlaps_without_opening_excluded_files() {
    let root = Fixture::new(); root.write("a.rs", b"aaaa");
    root.write("b.rs", &[0xff, 0xfe, b'a', 0, b'a', 0, b'a', 0]);
    root.write("c.rs", b"excluded");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::text(owner(), "aa", &budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new_scoped_counting(&atlas, &needle, revision(100), generation(1),
        AtlasStreamLimits { max_matches: 0, ..Default::default() }, &[0, 1],
        &budget, [allocation(11), allocation(12)]).unwrap();
    let mut opens = 0;
    for _ in 0..100 {
        if job.state() != AtlasStreamState::Pending { break; }
        job.step(StreamReadStep::default(), generation(1), |_, path| {
            assert_ne!(path.as_bytes(), b"c.rs"); opens += 1;
            File::open(root.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable)
        }, || false).unwrap();
    }
    let report = job.finish().unwrap();
    assert_eq!(opens, 2); assert!(report.is_complete()); assert!(report.hits_truncated());
    assert_eq!(report.eligible_files(), 2); assert_eq!(report.excluded_files(), 1);
    assert_eq!(report.stats().matches_seen, 5); assert_eq!(report.matching_files_seen(), 2);
    assert!(report.hits().is_empty()); assert_eq!(report.retained_witness_bytes(), 0);
    assert_eq!(report.files()[0].matches_seen(), 3); assert_eq!(report.files()[1].matches_seen(), 2);
    assert_eq!(report.files()[1].hit_count(), 0); assert_eq!(report.stats().bytes_read, 12);
}

#[test]
fn counted_search_does_not_treat_io_caps_as_exhaustive_counts() {
    let root = Fixture::new(); root.write("a.rs", b"aaaa"); root.write("b.rs", b"aaaa");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    for (base, limits, reason) in [
        (10, AtlasStreamLimits { max_matches: 1, max_bytes: 4, ..Default::default() }, AtlasStreamStop::ByteLimit),
        (20, AtlasStreamLimits { max_matches: 1, max_read_calls: 1, ..Default::default() }, AtlasStreamStop::ReadCallLimit),
    ] {
        let report = run_mode(&atlas, "aa", limits, &budget, base, true);
        assert!(!report.is_complete()); assert!(report.hits_truncated());
        assert_eq!(report.stop_reason(), Some(reason)); assert_eq!(report.stats().matches_seen, 3);
        assert_eq!(report.files_examined(), 1); assert_eq!(report.unexamined_files(), 1);
        assert_eq!(report.hits().len(), 1); assert_eq!(report.matching_files_seen(), 1);
        assert_eq!(report.stats().bytes_read, 4); assert_eq!(report.stats().read_calls, 1);
    }
}

#[test]
fn counted_unsupported_file_after_row_cap_cannot_contribute_false_exact_totals() {
    let root = Fixture::new(); root.write("a.rs", b"aaaa");
    let mut invalid = b"aa".to_vec(); invalid.resize(16 * 1024, b'x'); invalid.push(0xff);
    root.write("b.rs", &invalid); root.write("c.rs", b"aaa");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let report = run_mode(&atlas, "aa", AtlasStreamLimits { max_matches: 1, ..Default::default() }, &budget, 10, true);
    assert!(!report.is_complete()); assert_eq!(report.stop_reason(), None);
    assert_eq!(report.stats().matches_seen, 5); assert_eq!(report.matching_files_seen(), 2);
    assert_eq!(report.stats().incomplete_files, 1); assert_eq!(report.files_examined(), 3);
    assert_eq!(report.files()[1].state(), AtlasStreamFileState::Scanned(StreamReadState::UnsupportedText));
    assert_eq!(report.files()[1].matches_seen(), 0); assert_eq!(report.hits().len(), 1);
}

#[test]
fn malformed_late_text_cannot_publish_earlier_file_hits_as_a_complete_text_answer() {
    let root = Fixture::new(); let mut bytes = b"needle".to_vec(); bytes.resize(16 * 1024, b'a'); bytes.push(0xff);
    root.write("a.rs", &bytes); root.write("b.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let report = run(&atlas, "needle", AtlasStreamLimits::default(), &budget, 10);
    assert!(!report.is_complete()); assert_eq!(report.hits().len(), 1);
    assert_eq!(report.files()[0].state(), AtlasStreamFileState::Scanned(StreamReadState::UnsupportedText));
    assert_eq!(report.files()[0].hit_count(), 0); assert_eq!(report.stats().bytes_read, (bytes.len() + 6) as u64);
    assert_eq!(atlas.entry(report.hits()[0].node()).unwrap().path().as_bytes(), b"b.rs");
}

#[test]
fn an_open_failure_does_not_hide_later_matches_or_become_a_no_match() {
    let root = Fixture::new(); root.write("a.rs", b"gone"); root.write("b.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    fs::remove_file(root.0.join("a.rs")).unwrap();
    let report = run(&atlas, "needle", AtlasStreamLimits::default(), &budget, 10);
    assert!(!report.is_complete()); assert_eq!(report.files_examined(), 2); assert_eq!(report.hits().len(), 1);
    assert!(matches!(report.files()[0].state(), AtlasStreamFileState::Unavailable(_)));
    assert_eq!(report.stats().bytes_read, 6); assert_eq!(report.stats().incomplete_files, 1);
}

#[test]
fn short_read_retains_only_proven_witnesses_and_spent_io_after_live_truncation() {
    let root = Fixture::new(); let mut bytes = b"needle".to_vec(); bytes.resize(64 * 1024, b'a'); root.write("a.rs", &bytes);
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::text(owner(), "needle", &budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new(&atlas, &needle, revision(100), generation(1), AtlasStreamLimits::default(),
        &budget, [allocation(11), allocation(12)]).unwrap();
    let mut opens = 0;
    job.step(StreamReadStep::default(), generation(1), |_, path| { opens += 1;
        File::open(root.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable) }, || false).unwrap();
    assert_eq!(job.report().stats().bytes_read, 16 * 1024);
    root.write("a.rs", b"");
    for _ in 0..10 {
        if job.state() != AtlasStreamState::Pending { break; }
        job.step(StreamReadStep::default(), generation(1), |_, _| panic!("must not reopen active source"), || false).unwrap();
    }
    let report = job.finish().unwrap(); assert_eq!(opens, 1); assert!(!report.is_complete());
    assert_eq!(report.files()[0].state(), AtlasStreamFileState::Scanned(StreamReadState::ShortRead));
    assert_eq!(report.stats().bytes_read, 16 * 1024);
    assert_eq!(report.select_hit(0, &atlas, generation(1)).unwrap().original_bytes(), b"needle");
}

#[test]
fn zero_work_stale_generation_and_cancellation_cannot_open_sources() {
    let root = Fixture::new(); root.write("a.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::text(owner(), "needle", &budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new(&atlas, &needle, revision(100), generation(1), AtlasStreamLimits::default(),
        &budget, [allocation(11), allocation(12)]).unwrap();
    for quantum in [StreamReadStep { max_bytes: 0, ..Default::default() },
        StreamReadStep { max_calls: 0, ..Default::default() }, StreamReadStep { max_hits: 0, ..Default::default() }] {
        assert_eq!(job.step(quantum, generation(1), |_, _| panic!("zero quantum"), || false).unwrap(), AtlasStreamState::Pending);
    }
    assert!(matches!(job.step(StreamReadStep::default(), generation(2), |_, _| panic!("stale"), || false), Err(AtlasStreamError::StaleQuery)));
    assert!(matches!(job.finish(), Err(AtlasStreamError::Canceled)));
    let mut job = AtlasStreamSearch::new(&atlas, &needle, revision(200), generation(3), AtlasStreamLimits::default(),
        &budget, [allocation(21), allocation(22)]).unwrap();
    assert!(matches!(job.step(StreamReadStep::default(), generation(3), |_, _| panic!("canceled"), || true), Err(AtlasStreamError::Canceled)));
}

#[test]
fn directory_counts_preserve_occurrences_files_and_raw_component_boundaries() {
    let root = Fixture::new(); root.write("src/a.rs", b"needle needle"); root.write("src/deep/b.rs", b"needle"); root.write("src-old/c.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let index = atlas.index(&budget, allocation(3), || false).unwrap();
    let before = atlas.layout().clone();
    let report = run(&atlas, "needle", AtlasStreamLimits::default(), &budget, 10);
    assert_eq!(report.retained_matches_in(index.root_node()).unwrap(), (4, 3));
    assert_eq!(report.retained_matches_in(index.find_path(b"src").unwrap()).unwrap(), (3, 2));
    assert_eq!(report.retained_matches_in(index.find_path(b"src/deep").unwrap()).unwrap(), (1, 1));
    assert_eq!(report.retained_matches_in(index.find_path(b"src/a.rs").unwrap()).unwrap(), (2, 1));
    assert_eq!(atlas.layout(), &before);
}

#[test]
fn witnesses_outlive_the_worker_and_cannot_rebind_to_another_atlas_or_query() {
    let root = Fixture::new(); root.write("a.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let first = atlas(&catalog, &budget, 2);
    let second = atlas(&catalog, &budget, 3);
    let report = run(&first, "needle", AtlasStreamLimits::default(), &budget, 10);
    root.write("a.rs", b"changed live source");
    assert!(matches!(report.validate_delivery(&second, generation(10)), Err(AtlasStreamError::WrongAtlas)));
    assert!(matches!(report.validate_delivery(&first, generation(11)), Err(AtlasStreamError::StaleQuery)));
    let extent = report.retain_hit(0, &first, generation(10), &budget, allocation(30)).unwrap();
    let view = BrowserSession::new(owner()).open_extent(extent).unwrap();
    assert_eq!(view.raw_selection(report.hits()[0].original_range()).unwrap(), b"needle");
    catalog.grant().revoke();
    assert!(report.select_hit(0, &first, generation(10)).is_err());
}

#[test]
fn many_files_share_one_worker_envelope_and_do_not_retain_full_decoder_leases() {
    let root = Fixture::new();
    for n in 0..64 { root.write(&format!("f{n:03}.rs"), b"needle"); }
    let budget = budget(); let catalog = root.catalog(&budget, 100); let atlas = atlas(&catalog, &budget, 2);
    let report = run(&atlas, "needle", AtlasStreamLimits::default(), &budget, 10);
    assert!(report.is_complete()); assert_eq!(report.hits().len(), 64);
    assert_eq!(report.retained_witness_bytes(), 64 * 6); assert!(report.reserved_bytes() < 128 * 1024);
}

#[test]
fn scoped_projection_skips_large_and_unavailable_files_and_preserves_original_identity() {
    let root = Fixture::new(); root.write("a-excluded.bin", &vec![b'x'; 2 * 1024 * 1024]);
    root.write("b.rs", b"needle"); root.write("c-excluded.rs", b"needle"); root.write("z.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 8); let atlas = atlas(&catalog, &budget, 2);
    fs::remove_file(root.0.join("c-excluded.rs")).unwrap();
    let needle = StreamingNeedle::text(owner(), "needle", &budget, allocation(10)).unwrap();
    let mut selected = vec![1, 3];
    let mut job = AtlasStreamSearch::new_scoped(&atlas, &needle, revision(100), generation(1),
        AtlasStreamLimits { max_bytes: 12, max_read_calls: 2, ..Default::default() },
        &selected, &budget, [allocation(11), allocation(12)]).unwrap();
    // Membership is retained independently of mutable caller scratch.
    selected[1] = 2;
    assert_eq!(job.report().eligible_files(), 2); assert_eq!(job.report().excluded_files(), 2);
    let mut opened = Vec::new();
    while job.state() == AtlasStreamState::Pending {
        job.step(StreamReadStep::default(), generation(1), |request, path| {
            let expected = [1, 3][opened.len()];
            assert_eq!(request.file(), catalog.file_id(expected).unwrap());
            assert_eq!(request.revision(), revision(100 + expected as u64));
            assert_eq!(path, catalog.entries()[expected].path());
            opened.push(expected);
            File::open(root.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable)
        }, || false).unwrap();
    }
    let report = job.finish().unwrap();
    assert_eq!(opened, [1, 3]); assert!(report.is_complete());
    assert_eq!(report.files_examined(), 2); assert_eq!(report.unexamined_files(), 0);
    assert_eq!(report.stats().bytes_read, 12); assert_eq!(report.stats().read_calls, 2);
    assert_eq!(report.stats().incomplete_files, 0);
    for (position, ordinal) in [1, 3].into_iter().enumerate() {
        let file = catalog.file_id(ordinal).unwrap();
        assert_eq!(report.hits()[position].file(), file);
        assert_eq!(report.hits()[position].node(), atlas.node_for_file(file).unwrap());
        assert_eq!(report.hits()[position].file_record(), position);
        assert_eq!(report.select_hit(position, &atlas, generation(1)).unwrap().original_bytes(), b"needle");
    }
}

#[test]
fn explicit_empty_projection_is_terminal_without_source_io_even_with_zero_scan_budget() {
    let root = Fixture::new(); root.write("a.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 8); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::text(owner(), "needle", &budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new_scoped(&atlas, &needle, revision(100), generation(1),
        AtlasStreamLimits { max_bytes: 0, max_read_calls: 0, ..Default::default() }, &[],
        &budget, [allocation(11), allocation(12)]).unwrap();
    assert_eq!(job.state(), AtlasStreamState::Finished);
    job.step(StreamReadStep::default(), generation(1), |_, _| panic!("empty scope cannot open"), || false).unwrap();
    let report = job.finish().unwrap();
    assert!(report.is_complete()); assert_eq!(report.eligible_files(), 0); assert_eq!(report.excluded_files(), 1);
    assert_eq!(report.files_examined(), 0); assert_eq!(report.unexamined_files(), 0);
    assert_eq!(report.stats().bytes_read, 0); assert_eq!(report.stats().read_calls, 0);
    assert!(report.hits().is_empty()); assert_eq!(report.stop_reason(), None);
}

#[test]
fn invalid_projection_and_denied_admission_do_not_leak_report_capacity() {
    let root = Fixture::new(); root.write("a.rs", b"needle"); root.write("b.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 8); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::text(owner(), "needle", &budget, allocation(10)).unwrap();
    let before = budget.accounting().reserved();
    for invalid in [vec![0, 0], vec![1, 0], vec![2], vec![usize::MAX]] {
        assert!(matches!(AtlasStreamSearch::new_scoped(&atlas, &needle, revision(100), generation(1),
            AtlasStreamLimits::default(), &invalid, &budget, [allocation(11), allocation(12)]),
            Err(AtlasStreamError::InvalidScope)));
        assert_eq!(budget.accounting().reserved(), before);
    }
    let denied = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(AtlasStreamSearch::new_scoped(&atlas, &needle, revision(100), generation(1),
        AtlasStreamLimits::default(), &[1], &denied, [allocation(11), allocation(12)]),
        Err(AtlasStreamError::ResourceDenied)));
    assert_eq!(denied.accounting().reserved().get(), 0);
}

#[test]
fn scoped_call_limit_and_cancellation_leave_only_eligible_files_unexamined() {
    let root = Fixture::new();
    for path in ["a.rs", "b.rs", "c.rs", "d.rs"] { root.write(path, b"needle"); }
    let budget = budget(); let catalog = root.catalog(&budget, 8); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::text(owner(), "needle", &budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new_scoped(&atlas, &needle, revision(100), generation(1),
        AtlasStreamLimits { max_read_calls: 1, ..Default::default() }, &[1, 3],
        &budget, [allocation(11), allocation(12)]).unwrap();
    let mut opens = 0;
    while job.state() == AtlasStreamState::Pending {
        job.step(StreamReadStep::default(), generation(1), |request, path| {
            opens += 1; assert_eq!(request.file(), catalog.file_id(1).unwrap());
            File::open(root.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable)
        }, || false).unwrap();
    }
    let report = job.finish().unwrap();
    assert_eq!(opens, 1); assert_eq!(report.stop_reason(), Some(AtlasStreamStop::ReadCallLimit));
    assert_eq!(report.files_examined(), 1); assert_eq!(report.unexamined_files(), 1);
    assert_eq!(report.eligible_files(), 2); assert_eq!(report.excluded_files(), 2);
    assert!(!report.is_complete());
    let mut canceled = AtlasStreamSearch::new_scoped(&atlas, &needle, revision(200), generation(2),
        AtlasStreamLimits::default(), &[3], &budget, [allocation(21), allocation(22)]).unwrap();
    assert!(matches!(canceled.step(StreamReadStep::default(), generation(2),
        |_, _| panic!("canceled scope cannot open"), || true), Err(AtlasStreamError::Canceled)));
    assert_eq!(canceled.report().stats().read_calls, 0);
}

fn retained(catalog: WorkspaceCatalog, budget: &ResourceBudget) -> RetainedWorkspaceAtlas {
    RetainedWorkspaceAtlas::build(catalog, LayoutRevision::new(owner(), 1).unwrap(),
        Size2D::new(1024.0, 768.0).unwrap(), LayoutOptions::modest(), WorkspaceAtlasLimits::default(),
        budget, [allocation(2), allocation(3)], || false).unwrap()
}
fn run_retained<'a>(atlas: &'a RetainedWorkspaceAtlas, budget: &ResourceBudget)
    -> AtlasStreamReport<'a, 'a, RetainedWorkspaceAtlas> {
    let needle = StreamingNeedle::text(owner(), "needle", budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new(atlas, &needle, revision(100), generation(1), AtlasStreamLimits::default(),
        budget, [allocation(11), allocation(12)]).unwrap();
    let root = atlas.catalog().grant().root_path().to_path_buf();
    while job.state() == AtlasStreamState::Pending {
        job.step(StreamReadStep::default(), generation(1), |_, path|
            File::open(root.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable), || false).unwrap();
    }
    job.finish().unwrap()
}

#[test]
fn retained_native_atlas_uses_the_existing_layout_and_bounded_literal_witness() {
    let root = Fixture::new();
    let mut source = vec![b'x'; 2 * 1024 * 1024 - 3];
    let offset = source.len() as u64;
    source.extend_from_slice(b"needle"); root.write("src/large.rs", &source);
    let budget = budget(); let atlas = retained(root.catalog(&budget, 8), &budget);
    let layout = atlas.layout().clone();
    let before = budget.accounting().reserved().get();
    let report = run_retained(&atlas, &budget);
    assert!(std::ptr::eq(report.atlas(), &atlas));
    assert!(report.is_complete());
    assert_eq!(report.stats().bytes_read, source.len() as u64);
    assert_eq!(report.hits()[0].original_range().start().get(), offset);
    assert_eq!(report.retained_witness_bytes(), 6);
    assert!(budget.accounting().reserved().get() - before < 128 * 1024);
    assert_eq!(atlas.layout(), &layout, "stream search cannot repack native geometry");
    root.write("src/large.rs", b"different current source");
    let extent = report.retain_hit(0, &atlas, generation(1), &budget, allocation(20)).unwrap();
    let view = BrowserSession::new(owner()).open_extent(extent).unwrap();
    assert_eq!(view.raw_selection(report.hits()[0].original_range()).unwrap(), b"needle");
    assert!(view.raw_selection(fcb::ByteRange::new(fcb::ByteOffset::new(0), fcb::ByteOffset::new(1)).unwrap()).is_err(),
        "a retained literal does not manufacture earlier context");
}

#[test]
fn borrowed_and_retained_filtered_layouts_are_refused_before_any_source_opens() {
    use fcb::map::workspace::{AtlasExtensionScope, AtlasScope};
    let root = Fixture::new(); root.write("a.rs", b"needle"); root.write("b.md", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 8);
    let scope = AtlasScope::Extensions(AtlasExtensionScope::from_extensions([b"rs".as_slice()]).unwrap());
    let needle = StreamingNeedle::text(owner(), "needle", &budget, allocation(10)).unwrap();
    {
        let scoped = WorkspaceAtlas::build(&catalog, &scope, LayoutRevision::new(owner(), 2).unwrap(),
            Size2D::new(1024.0, 768.0).unwrap(), LayoutOptions::modest(), WorkspaceAtlasLimits::default(),
            &budget, allocation(20), || false).unwrap();
        assert_eq!(scoped.file_count(), 1);
        assert!(matches!(AtlasStreamSearch::new(&scoped, &needle, revision(100), generation(1), AtlasStreamLimits::default(),
            &budget, [allocation(11), allocation(12)]), Err(AtlasStreamError::InvalidLimits)));
    }
    let base = retained(catalog, &budget);
    let scoped = RetainedWorkspaceAtlas::build_scoped(base.catalog_shared(), &scope,
        LayoutRevision::new(owner(), 2).unwrap(), Size2D::new(1024.0, 768.0).unwrap(),
        LayoutOptions::modest(), WorkspaceAtlasLimits::default(), &budget, [allocation(21), allocation(22)], || false).unwrap();
    assert!(matches!(AtlasStreamSearch::new(&scoped, &needle, revision(100), generation(1), AtlasStreamLimits::default(),
        &budget, [allocation(11), allocation(12)]), Err(AtlasStreamError::InvalidLimits)));
}

#[test]
fn retained_atlas_delivery_still_checks_exact_owner_query_and_revocation() {
    use fcb::map::workspace::{AtlasExtensionScope, AtlasScope};
    let root = Fixture::new(); root.write("a.rs", b"needle");
    let budget = budget(); let atlas = retained(root.catalog(&budget, 8), &budget);
    // This different layout happens to include every file. It can be searched,
    // but its identical file membership cannot authorize another report.
    let scope = AtlasScope::Extensions(AtlasExtensionScope::from_extensions([b"rs".as_slice()]).unwrap());
    let other = RetainedWorkspaceAtlas::build_scoped(atlas.catalog_shared(), &scope,
        LayoutRevision::new(owner(), 2).unwrap(), Size2D::new(1024.0, 768.0).unwrap(),
        LayoutOptions::modest(), WorkspaceAtlasLimits::default(), &budget, [allocation(21), allocation(22)], || false).unwrap();
    let report = run_retained(&atlas, &budget);
    assert!(matches!(report.select_hit(0, &other, generation(1)), Err(AtlasStreamError::WrongAtlas)));
    assert!(matches!(report.select_hit(0, &atlas, generation(2)), Err(AtlasStreamError::StaleQuery)));
    atlas.catalog().grant().revoke();
    assert!(report.select_hit(0, &atlas, generation(1)).is_err());
}

#[test]
fn zero_admission_and_partial_discovery_never_claim_repository_wide_absence() {
    let root = Fixture::new(); root.write("a.rs", b"no"); root.write("b.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 1); let atlas = atlas(&catalog, &budget, 2);
    let report = run(&atlas, "needle", AtlasStreamLimits { max_bytes: 0, ..Default::default() }, &budget, 10);
    assert!(!report.is_complete()); assert_eq!(report.stats().bytes_read, 0); assert_eq!(report.files_examined(), 0);
    let partial = run(&atlas, "needle", AtlasStreamLimits::default(), &budget, 20);
    assert!(!partial.is_complete()); assert!(!catalog.discovery_complete());
}

#[test]
fn empty_catalog_is_complete_even_with_zero_io_and_zero_retained_hits() {
    let root = Fixture::new(); let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let report = run(&atlas, "needle", AtlasStreamLimits { max_bytes: 0, max_read_calls: 0, max_matches: 0 }, &budget, 10);
    assert!(report.is_complete()); assert_eq!(report.stats().bytes_read, 0); assert!(report.files().is_empty());
}

#[test]
fn stopping_keeps_finalized_witnesses_and_charges_discarded_active_file_io() {
    let root = Fixture::new();
    root.write("a.rs", b"needle");
    let mut active = b"needle".to_vec(); active.resize(64 * 1024, b'x');
    root.write("b.rs", &active); root.write("c.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::text(owner(), "needle", &budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new(&atlas, &needle, revision(100), generation(1), AtlasStreamLimits::default(),
        &budget, [allocation(11), allocation(12)]).unwrap();
    let mut opens = 0;
    for _ in 0..2 {
        job.step(StreamReadStep::default(), generation(1), |_, path| {
            opens += 1;
            File::open(root.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable)
        }, || false).unwrap();
    }
    assert_eq!(opens, 2);
    assert_eq!(job.state(), AtlasStreamState::Pending);
    assert_eq!(job.report().files_examined(), 1);
    assert_eq!(job.report().unexamined_files(), 2);
    assert_eq!(job.report().hits().len(), 1, "an active file's tentative match is not published");
    let spent = job.report().stats();
    assert_eq!(spent.bytes_read, 6 + 16 * 1024);
    assert_eq!(spent.read_calls, 2);
    assert_eq!(spent.matches_seen, 1);
    let first = job.report().hits()[0];
    assert_eq!(job.stop_and_keep().unwrap(), AtlasStreamState::Finished);
    assert_eq!(job.stop_and_keep().unwrap(), AtlasStreamState::Finished, "Stop is idempotent");
    assert_eq!(job.step(StreamReadStep::default(), generation(1),
        |_, _| panic!("a stopped job must not reopen any source"), || false).unwrap(), AtlasStreamState::Finished);
    let report = job.finish().unwrap();
    assert_eq!(report.stop_reason(), Some(AtlasStreamStop::UserStopped));
    assert_eq!(report.stop_reason().unwrap().code(), "user-stopped");
    assert!(!report.is_complete());
    assert_eq!(report.stats(), spent);
    assert_eq!(report.files_examined(), 1);
    assert_eq!(report.unexamined_files(), 2);
    assert_eq!(report.hits(), &[first]);
    root.write("a.rs", b"changed live source");
    assert_eq!(report.select_hit(0, &atlas, generation(1)).unwrap().original_bytes(), b"needle");
    assert!(matches!(report.select_hit(1, &atlas, generation(1)), Err(AtlasStreamError::InvalidHit)));
}

#[test]
fn stop_before_first_open_is_partial_and_never_revives_cancellation_or_relabels_completion() {
    let root = Fixture::new(); root.write("a.rs", b"needle");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::text(owner(), "needle", &budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new(&atlas, &needle, revision(100), generation(1), AtlasStreamLimits::default(),
        &budget, [allocation(11), allocation(12)]).unwrap();
    assert_eq!(job.stop_and_keep().unwrap(), AtlasStreamState::Finished);
    let stopped = job.finish().unwrap();
    assert_eq!(stopped.files_examined(), 0);
    assert_eq!(stopped.unexamined_files(), 1);
    assert_eq!(stopped.stats(), Default::default());
    assert!(!stopped.is_complete());
    assert_eq!(stopped.stop_reason(), Some(AtlasStreamStop::UserStopped));

    let mut canceled = AtlasStreamSearch::new(&atlas, &needle, revision(200), generation(2), AtlasStreamLimits::default(),
        &budget, [allocation(21), allocation(22)]).unwrap();
    canceled.cancel();
    assert_eq!(canceled.stop_and_keep(), Err(AtlasStreamError::Canceled));
    assert!(matches!(canceled.finish(), Err(AtlasStreamError::Canceled)));

    for (base, limits, expected) in [
        (30, AtlasStreamLimits::default(), None),
        (40, AtlasStreamLimits { max_bytes: 1, ..Default::default() }, Some(AtlasStreamStop::ByteLimit)),
    ] {
        let mut finished = AtlasStreamSearch::new(&atlas, &needle, revision(base * 100), generation(base), limits,
            &budget, [allocation(base + 1), allocation(base + 2)]).unwrap();
        while finished.state() == AtlasStreamState::Pending {
            finished.step(StreamReadStep::default(), generation(base), |_, path|
                File::open(root.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable), || false).unwrap();
        }
        assert_eq!(finished.stop_and_keep().unwrap(), AtlasStreamState::Finished);
        let report = finished.finish().unwrap();
        assert_eq!(report.stop_reason(), expected);
        assert_eq!(report.is_complete(), expected.is_none());
    }

    let mut revoked = AtlasStreamSearch::new(&atlas, &needle, revision(500), generation(5), AtlasStreamLimits::default(),
        &budget, [allocation(51), allocation(52)]).unwrap();
    catalog.grant().revoke();
    assert!(revoked.stop_and_keep().is_err());
    assert!(matches!(revoked.finish(), Err(AtlasStreamError::Canceled)));
}

#[test]
fn pending_report_prefix_survives_a_later_files_malformed_text_without_exposing_tentative_hits() {
    let root = Fixture::new();
    let mut utf16 = vec![0xff, 0xfe];
    utf16.extend("needle\n".encode_utf16().flat_map(u16::to_le_bytes));
    root.write("a.rs", &utf16);
    let mut malformed = b"needle".to_vec(); malformed.resize(16 * 1024, b'x'); malformed.push(0xff);
    root.write("b.rs", &malformed);
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::text(owner(), "needle", &budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new(&atlas, &needle, revision(100), generation(1), AtlasStreamLimits::default(),
        &budget, [allocation(11), allocation(12)]).unwrap();
    let mut open = |_: fcb::search::CaptureRequest, path: &fcb::source::path::NormalizedPath|
        File::open(root.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable);
    job.step(StreamReadStep::default(), generation(1), &mut open, || false).unwrap();
    assert_eq!(job.report().files_examined(), 1);
    assert!(!job.report().is_complete());
    let first = job.report().hits()[0];
    let witness = job.report().select_hit(0, &atlas, generation(1)).unwrap().original_bytes().to_vec();
    assert_eq!(witness, "needle".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
    job.step(StreamReadStep::default(), generation(1), &mut open, || false).unwrap();
    assert_eq!(job.report().files_examined(), 1);
    assert_eq!(job.report().hits(), &[first]);
    assert_eq!(job.report().stats().matches_seen, 1);
    assert_eq!(job.report().stats().bytes_read, utf16.len() as u64 + 16 * 1024);
    job.step(StreamReadStep::default(), generation(1), &mut open, || false).unwrap();
    let report = job.finish().unwrap();
    assert_eq!(report.hits(), &[first]);
    assert_eq!(report.files()[1].state(), AtlasStreamFileState::Scanned(StreamReadState::UnsupportedText));
    assert_eq!(report.select_hit(0, &atlas, generation(1)).unwrap().original_bytes(), witness);
    assert!(!report.is_complete());
}

#[test]
fn lowercase_workspace_counts_beyond_retention_and_preserves_each_original_witness() {
    let root = Fixture::new();
    root.write("a.rs", "KK".as_bytes());
    let utf16: Vec<u8> = "\u{feff}kK".encode_utf16().flat_map(u16::to_be_bytes).collect();
    root.write("b.rs", &utf16);
    root.write("c.txt", b"KKK");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    for retained in [0, 1, 4] {
        let needle = StreamingNeedle::text_matching(owner(), "k", false, &budget, allocation(10)).unwrap();
        let mut job = AtlasStreamSearch::new_scoped_counting(&atlas, &needle, revision(100), generation(1),
            AtlasStreamLimits { max_matches: retained, ..Default::default() }, &[0, 1],
            &budget, [allocation(11), allocation(12)]).unwrap();
        let mut opened = Vec::new();
        while job.state() == AtlasStreamState::Pending {
            job.step(StreamReadStep { max_bytes: 1, max_calls: 1, max_hits: 1 }, generation(1), |_, path| {
                opened.push(path.raw().as_bytes().to_vec());
                File::open(root.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable)
            }, || false).unwrap();
        }
        let report = job.finish().unwrap();
        drop(needle);
        assert!(report.is_complete());
        assert!(!report.case_sensitive());
        assert_eq!(opened, [b"a.rs".to_vec(), b"b.rs".to_vec()]);
        assert_eq!(report.stats().bytes_read, 10);
        assert_eq!(report.stats().matches_seen, 4);
        assert_eq!(report.matching_files_seen(), 2);
        assert_eq!(report.hits().len(), retained);
        assert_eq!(report.eligible_files(), 2);
        assert_eq!(report.excluded_files(), 1);
        assert_eq!(report.hits_truncated(), retained < 4);
        let expected: [(&str, &[u8]); 4] = [("K", "K".as_bytes()), ("K", b"K"), ("k", &[0, 0x6b]), ("K", &[0x21, 0x2a])];
        for (position, &(text, bytes)) in expected.iter().take(retained).enumerate() {
            let hit = report.select_hit(position, &atlas, generation(1)).unwrap();
            assert_eq!(hit.matched_text(), text);
            assert_eq!(hit.original_bytes(), bytes);
        }
        assert_eq!(report.retained_witness_bytes(), expected.iter().take(retained).map(|(_, bytes)| bytes.len()).sum::<usize>());
    }
}

#[test]
fn lowercase_workspace_witnesses_survive_mutation_and_partial_files_do_not_count_as_negatives() {
    let root = Fixture::new();
    root.write("a.rs", "K K".as_bytes());
    root.write("b.rs", b"K\xff");
    let budget = budget(); let catalog = root.catalog(&budget, 10); let atlas = atlas(&catalog, &budget, 2);
    let needle = StreamingNeedle::text_matching(owner(), "k", false, &budget, allocation(10)).unwrap();
    let mut job = AtlasStreamSearch::new_counting(&atlas, &needle, revision(100), generation(1),
        AtlasStreamLimits::default(), &budget, [allocation(11), allocation(12)]).unwrap();
    while job.state() == AtlasStreamState::Pending {
        job.step(StreamReadStep::default(), generation(1), |_, path|
            File::open(root.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable), || false).unwrap();
    }
    let report = job.finish().unwrap();
    assert!(!report.is_complete());
    assert_eq!(report.stats().matches_seen, 2);
    assert_eq!(report.stats().incomplete_files, 1);
    assert_eq!(report.files()[1].state(), AtlasStreamFileState::Scanned(StreamReadState::UnsupportedText));
    root.write("a.rs", b"replacement");
    assert_eq!(report.select_hit(0, &atlas, generation(1)).unwrap().original_bytes(), "K".as_bytes());
    assert_eq!(report.select_hit(1, &atlas, generation(1)).unwrap().original_bytes(), b"K");
    assert_eq!(report.retained_witness_bytes(), 4);
}

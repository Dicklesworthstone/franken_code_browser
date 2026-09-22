#![forbid(unsafe_code)]
#![cfg(all(feature = "map", feature = "search", unix))]

//! Public production engines over actual open files; no mock matcher/decoder.
use std::{fs::{self, File}, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, FileId, SourceRevision, Size2D};
use fcb::map::{LayoutRevision, LayoutOptions, QueryGeneration, ResourceAllocationId, ResourceBudget, RootId};
use fcb::map::workspace::{WorkspaceAtlas, WorkspaceAtlasLimits};
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
    let needle = StreamingNeedle::text(owner(), literal, budget, allocation(base)).unwrap();
    let mut job = AtlasStreamSearch::new(atlas, &needle, revision(base * 100), generation(base), limits,
        budget, [allocation(base + 1), allocation(base + 2)]).unwrap();
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

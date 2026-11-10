#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! Production archive/extractor/reader regressions. Code-first cases for the
//! independent verifier; this file is not an execution receipt.

use std::{cell::RefCell, io::{self, Cursor, Read, Seek, SeekFrom}, rc::Rc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{QueryGeneration, ResourceAllocationId, ResourceBudget, SymbolNameMode};
use fcb::search::paged_snapshot::{PagedMemberData, PagedSnapshot};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::snapshot_structure::{StructureError, StructureMode, StructureOptions,
    StructureQuery, StructureReport, StructureState, MAX_STRUCTURE_SOURCE_BYTES};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2672).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap()
}
fn allocations() -> [ResourceAllocationId; 5] {
    [allocation(10), allocation(11), allocation(12), allocation(13), allocation(14)]
}
fn options(mode: StructureMode, limit: usize) -> StructureOptions {
    StructureOptions { generation: QueryGeneration::new(owner(), 1).unwrap(),
        first_file: FileId::new(owner(), 100).unwrap(),
        first_revision: SourceRevision::new(owner(), 200).unwrap(),
        mode, name_mode: SymbolNameMode::Exact, max_matches: limit,
        max_source_bytes: MAX_STRUCTURE_SOURCE_BYTES }
}
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn archive(entries: &[SnapshotEntry<'_>], complete: bool, budget: &ResourceBudget)
    -> PagedSnapshot<Cursor<Vec<u8>>> {
    let saved = SnapshotBytes::encode(owner(), complete, "structure-test-v1", entries,
        SnapshotLimits::default(), budget, allocation(1), || false).unwrap();
    PagedSnapshot::open(Cursor::new(saved.bytes().to_vec()), owner(), SnapshotLimits::default(),
        budget, allocation(2), || false).unwrap()
}
fn run<R: Read + Seek>(saved: &mut PagedSnapshot<R>, needle: &str, opts: StructureOptions,
    budget: &ResourceBudget) -> StructureReport {
    let max_steps = saved.directory().len() + 1;
    let mut query = StructureQuery::new(saved, needle, opts, budget, allocations()).unwrap();
    for _ in 0..max_steps {
        let before = query.stats();
        query.step(opts.generation, budget, || false).unwrap();
        assert!(query.stats().members_visited - before.members_visited <= 1);
        assert!(query.stats().members_loaded - before.members_loaded <= 1);
        if query.state() != StructureState::Pending { return query.finish().unwrap(); }
    }
    panic!("structural query exceeded its one-member-per-step bound");
}
fn encoded(text: &str, little: bool) -> Vec<u8> {
    let mut out = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        out.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    out
}

#[test]
fn declaration_name_matching_uses_real_extractors_and_raw_archive_paths() {
    let budget = budget();
    let rust = b"fn run() {}\nfn runner() {}\n// run is ordinary text here\n";
    let python = b"def run():\n    pass\n";
    let mut saved = archive(&[entry(b"a.rs", rust), entry(b"b\xff.py", python)], true, &budget);
    for (mode, names) in [(SymbolNameMode::Exact, vec!["run", "run"]),
        (SymbolNameMode::Prefix, vec!["run", "runner", "run"]),
        (SymbolNameMode::Contains, vec!["run", "runner", "run"])] {
        let mut opts = options(StructureMode::Symbols, 100); opts.name_mode = mode;
        let report = run(&mut saved, "run", opts, &budget);
        assert!(report.is_complete());
        assert_eq!(report.hits().iter().map(|h| h.name()).collect::<Vec<_>>(), names);
        assert_eq!(report.stats().files_searched, 2);
        assert_eq!(report.stats().members_loaded, 2);
        assert_eq!(report.hits()[0].language(), "rust");
        assert_eq!(report.hits().last().unwrap().language(), "python");
        for hit in report.hits() {
            let source: &[u8] = if hit.ordinal() == 0 { rust } else { python };
            let range = hit.original_range();
            assert_eq!(&source[range.start().get() as usize..range.end().get() as usize], hit.name().as_bytes());
            assert!(hit.evidence_range().start() <= range.start());
            assert!(hit.evidence_range().end() >= range.end());
            assert_eq!(hit.file().get(), 100 + hit.ordinal() as u64);
            assert_eq!(hit.revision().get(), 200 + hit.ordinal() as u64);
            assert_eq!(hit.generation(), opts.generation);
        }
    }
}

#[test]
fn utf16_declarations_preserve_original_endian_byte_coordinates() {
    let text = "// 🚀\nfn needle() {}\n";
    let prefix_units = text[..text.find("needle").unwrap()].encode_utf16().count();
    for little in [true, false] {
        let budget = budget(); let bytes = encoded(text, little);
        let mut saved = archive(&[entry(b"source.rs", &bytes)], true, &budget);
        let opts = options(StructureMode::Symbols, 10);
        let report = run(&mut saved, "needle", opts, &budget);
        assert!(report.is_complete()); assert_eq!(report.hits().len(), 1);
        let hit = &report.hits()[0]; let range = hit.original_range();
        assert_eq!(range.start().get(), 2 + prefix_units as u64 * 2);
        assert_eq!(range.end().get(), range.start().get() + 12);
        let wanted = encoded("needle", little);
        assert_eq!(&bytes[range.start().get() as usize..range.end().get() as usize], &wanted[2..]);
        let capture = hit.open(&mut saved, opts.generation, &budget,
            [allocation(30), allocation(31)], || false).unwrap();
        assert_eq!(capture.bytes(), bytes);
    }
}

#[test]
fn occurrence_mode_is_whole_token_text_including_comments_and_literals() {
    let text = "// needle\nlet needle = \"needle\";\nlet needle_suffix = 1;\n";
    for little in [None, Some(true), Some(false)] {
        let budget = budget();
        let bytes = little.map_or_else(|| text.as_bytes().to_vec(), |little| encoded(text, little));
        let mut saved = archive(&[entry(b"source.txt", &bytes)], true, &budget);
        let report = run(&mut saved, "needle", options(StructureMode::Occurrences, 100), &budget);
        assert!(report.is_complete()); assert_eq!(report.hits().len(), 3);
        let offsets: Vec<_> = text.match_indices("needle").take(3).map(|(offset, _)| offset).collect();
        for (hit, offset) in report.hits().iter().zip(offsets) {
            let start = little.map_or(offset as u64, |_| 2 + offset as u64 * 2);
            let length = if little.is_some() { 12 } else { 6 };
            assert_eq!(hit.original_range().start().get(), start);
            assert_eq!(hit.original_range().end().get(), start + length);
            assert_eq!(hit.original_range(), hit.evidence_range());
            assert_eq!(hit.kind(), "identifier-occurrence");
            assert_eq!(hit.language(), "text");
            assert_eq!(hit.heading_slug(), None);
        }
    }
}

fn limit_case(mode: StructureMode, path: &[u8], source: &[u8]) {
    let budget = budget();
    let mut saved = archive(&[entry(path, source), entry(b"z.rs", b"")], true, &budget);
    // An exact quota is complete when later eligible files contain no match.
    // Headings classify the non-Markdown member as unsupported.
    let report = run(&mut saved, "needle", options(mode, 1), &budget);
    assert_eq!(report.state(), StructureState::Complete);
    assert_eq!(report.matches_seen(), 1); assert_eq!(report.hits().len(), 1);
    assert_eq!(report.stats().members_visited, 2);
    if mode != StructureMode::Headings { assert!(report.is_complete()); }
    drop(report);
    let report = run(&mut saved, "needle", options(mode, 0), &budget);
    assert_eq!(report.state(), StructureState::Truncated);
    assert_eq!(report.matches_seen(), 1); assert!(report.hits().is_empty());
    assert!(!report.is_complete()); drop(report);
    let report = run(&mut saved, "absent", options(mode, 0), &budget);
    assert_eq!(report.state(), StructureState::Complete);
    assert_eq!(report.matches_seen(), 0); assert!(report.hits().is_empty());
}

#[test]
fn exact_caps_and_zero_capacity_probes_do_not_invent_truncation() {
    limit_case(StructureMode::Symbols, b"a.rs", b"fn needle() {}\n");
    limit_case(StructureMode::Occurrences, b"a.txt", b"needle\n");
    #[cfg(feature = "markdown")]
    limit_case(StructureMode::Headings, b"a.md", b"# needle\n");
    let budget = budget();
    let mut saved = archive(&[entry(b"a.rs", b"fn needle() {}\n"),
        entry(b"b.rs", b"fn needle() {}\n")], true, &budget);
    let report = run(&mut saved, "needle", options(StructureMode::Symbols, 1), &budget);
    assert_eq!(report.state(), StructureState::Truncated);
    assert_eq!(report.hits().len(), 1); assert_eq!(report.matches_seen(), 2);
    assert_eq!(report.stats().members_loaded, 2);
}

#[test]
fn unavailable_unsupported_oversized_and_incomplete_inputs_cannot_prove_absence() {
    let budget = budget();
    let too_large = vec![b' '; StructureMode::Symbols.source_limit() + 1];
    let mut saved = archive(&[entry(b"a.rs", b"fn unrelated() {}\n"),
        entry(b"b.txt", b"ordinary text"), entry(b"c.rs", b"\xff\xfe\x01"),
        entry(b"d.rs", &too_large), SnapshotEntry { path: b"e.rs", observed_bytes: 50,
            data: SnapshotData::Unavailable("MISSING") }], true, &budget);
    let report = run(&mut saved, "needle", options(StructureMode::Symbols, 10), &budget);
    assert_eq!(report.state(), StructureState::Complete); assert!(!report.is_complete());
    assert!(report.hits().is_empty());
    assert_eq!(report.stats().unsupported_files, 2);
    assert_eq!(report.stats().source_limited_files, 1);
    assert_eq!(report.stats().unavailable_files, 1);
    assert_eq!(report.stats().files_searched, 1);
    assert_eq!(report.stats().members_loaded, 2);
    assert_eq!(saved.load_stats().loaded_members, 2, "skip unsupported paths and oversize payloads before loading");
    drop(report); drop(saved);
    let mut saved = archive(&[], false, &budget);
    assert!(!run(&mut saved, "needle", options(StructureMode::Symbols, 10), &budget).is_complete());
}

#[test]
fn aggregate_source_budget_stops_before_loading_the_next_member() {
    let budget = budget(); let bytes = b"fn needle() {}\n";
    let mut saved = archive(&[entry(b"a.rs", bytes), entry(b"b.rs", bytes)], true, &budget);
    let mut opts = options(StructureMode::Symbols, 10); opts.max_source_bytes = bytes.len() as u64;
    let report = run(&mut saved, "needle", opts, &budget);
    assert_eq!(report.state(), StructureState::WorkLimit); assert!(!report.is_complete());
    assert_eq!(report.hits().len(), 1);
    assert_eq!(report.stats().members_loaded, 1); assert_eq!(saved.load_stats().loaded_members, 1);
    assert_eq!(report.stats().loaded_bytes, bytes.len() as u64);
    assert_eq!(report.stats().peak_source_bytes, bytes.len());
    let capture = report.hits()[0].open(&mut saved, opts.generation, &budget,
        [allocation(30), allocation(31)], || false).unwrap();
    assert_eq!(capture.bytes(), bytes);
}

#[test]
fn retained_results_release_all_member_buffers_between_steps() {
    let budget = budget(); let bytes = b"fn needle() {}\n";
    let mut saved = archive(&[entry(b"a.rs", bytes), entry(b"b.rs", bytes), entry(b"c.rs", bytes)], true, &budget);
    let baseline = budget.accounting().reserved().get();
    let opts = options(StructureMode::Symbols, 10);
    let mut query = StructureQuery::new(&mut saved, "needle", opts, &budget, allocations()).unwrap();
    let metadata = budget.accounting().reserved().get();
    for _ in 0..3 {
        query.step(opts.generation, &budget, || false).unwrap();
        assert_eq!(budget.accounting().reserved().get(), metadata, "only admitted result metadata survives a step");
    }
    let report = query.finish().unwrap(); assert_eq!(report.hits().len(), 3);
    assert_eq!(report.stats().loaded_bytes, bytes.len() as u64 * 3);
    assert_eq!(report.stats().peak_source_bytes, bytes.len());
    drop(report); assert_eq!(budget.accounting().reserved().get(), baseline);
}

#[test]
fn canceled_stale_and_resource_denied_candidates_never_publish_reports() {
    let budget = budget(); let mut saved = archive(&[entry(b"a.rs", b"fn needle() {}\n")], true, &budget);
    let opts = options(StructureMode::Symbols, 10);
    let baseline = budget.accounting().reserved().get();
    let mut query = StructureQuery::new(&mut saved, "needle", opts, &budget, allocations()).unwrap();
    assert!(matches!(query.step(opts.generation, &budget, || true), Err(StructureError::Canceled)));
    assert!(matches!(query.finish(), Err(StructureError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    let mut query = StructureQuery::new(&mut saved, "needle", opts, &budget, allocations()).unwrap();
    assert!(matches!(query.step(QueryGeneration::new(owner(), 2).unwrap(), &budget, || false), Err(StructureError::StaleQuery)));
    assert!(query.finish().is_err()); assert_eq!(budget.accounting().reserved().get(), baseline);
    let mut query = StructureQuery::new(&mut saved, "needle", opts, &budget, allocations()).unwrap();
    let remaining = budget.accounting().available().get();
    let blocker = budget.try_reserve_managed(owner(), allocation(40), ByteLength::new(remaining - 1)).unwrap();
    assert!(query.step(opts.generation, &budget, || false).is_err());
    assert!(matches!(query.state(), StructureState::Failed(_)));
    assert!(query.finish().is_err()); drop(blocker);
    assert_eq!(budget.accounting().reserved().get(), baseline);
}

#[test]
fn invalid_query_identity_names_and_allocations_fail_before_member_io() {
    let budget = budget(); let mut saved = archive(&[entry(b"a.rs", b"fn needle() {}\n")], true, &budget);
    let opts = options(StructureMode::Symbols, 10);
    for text in ["", "needle\0"] {
        assert!(matches!(StructureQuery::new(&mut saved, text, opts, &budget, allocations()), Err(StructureError::InvalidLimits)));
    }
    assert!(StructureQuery::new(&mut saved, "needle", opts, &budget, [allocation(10); 5]).is_err());
    let mut wrong = opts; wrong.generation = QueryGeneration::new(ArenaOwnerId::new(999).unwrap(), 1).unwrap();
    assert!(StructureQuery::new(&mut saved, "needle", wrong, &budget, allocations()).is_err());
    let mut references = options(StructureMode::Occurrences, 10); references.name_mode = SymbolNameMode::Contains;
    assert!(matches!(StructureQuery::new(&mut saved, "needle", references, &budget, allocations()), Err(StructureError::InvalidLimits)));
    references.name_mode = SymbolNameMode::Exact;
    assert!(StructureQuery::new(&mut saved, "not a token", references, &budget, allocations()).is_err());
    let mut oversized = opts; oversized.max_source_bytes = MAX_STRUCTURE_SOURCE_BYTES + 1;
    assert!(StructureQuery::new(&mut saved, "needle", oversized, &budget, allocations()).is_err());
    assert_eq!(saved.load_stats().loaded_members, 0);
}

#[derive(Clone)]
struct SharedArchive(Rc<RefCell<Cursor<Vec<u8>>>>);
impl Read for SharedArchive {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> { self.0.borrow_mut().read(bytes) }
}
impl Seek for SharedArchive {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> { self.0.borrow_mut().seek(position) }
}

#[test]
fn activation_revalidates_full_member_digest_and_retained_bytes_outlive_archive() {
    let budget = budget(); let bytes = b"fn needle() {}\n";
    let envelope = SnapshotBytes::encode(owner(), true, "structure-test-v1", &[entry(b"a.rs", bytes)],
        SnapshotLimits::default(), &budget, allocation(1), || false).unwrap();
    let backing = SharedArchive(Rc::new(RefCell::new(Cursor::new(envelope.bytes().to_vec()))));
    drop(envelope);
    let mut saved = PagedSnapshot::open(backing.clone(), owner(), SnapshotLimits::default(),
        &budget, allocation(2), || false).unwrap();
    let opts = options(StructureMode::Symbols, 10);
    let report = run(&mut saved, "needle", opts, &budget); let hit = &report.hits()[0];
    assert!(hit.open(&mut saved, QueryGeneration::new(owner(), 2).unwrap(), &budget,
        [allocation(30), allocation(31)], || false).is_err());
    let capture = hit.open(&mut saved, opts.generation, &budget,
        [allocation(30), allocation(31)], || false).unwrap();
    assert_eq!(capture.bytes(), bytes);
    let PagedMemberData::Captured { archive_offset, .. } = saved.directory().member(0).unwrap().data else { panic!("captured fixture") };
    // Alter a byte outside the selected name: activation must verify the whole
    // member, not merely compare the short displayed match.
    backing.0.borrow_mut().get_mut()[archive_offset as usize] = b'x';
    assert!(hit.open(&mut saved, opts.generation, &budget,
        [allocation(32), allocation(33)], || false).is_err());
    drop(report); drop(saved); drop(backing);
    assert_eq!(capture.bytes(), bytes);
}

#[cfg(feature = "markdown")]
#[test]
fn fmd_headings_keep_bom_original_syntax_and_duplicate_slugs() {
    let budget = budget();
    let bytes = b"\xef\xbb\xbf# Deploy!\r\n\r\nordinary Deploy! text\r\n\r\n# Deploy!\r\n";
    let mut saved = archive(&[entry(b"README.MD", bytes)], true, &budget);
    let report = run(&mut saved, "Deploy!", options(StructureMode::Headings, 10), &budget);
    assert!(report.is_complete()); assert_eq!(report.hits().len(), 2);
    assert_eq!(report.hits()[0].heading_slug(), Some("deploy"));
    assert_eq!(report.hits()[1].heading_slug(), Some("deploy-2"));
    assert_eq!(report.hits()[0].original_range().start().get(), 3);
    for hit in report.hits() {
        let range = hit.original_range(); let selected = &bytes[range.start().get() as usize..range.end().get() as usize];
        assert!(selected.starts_with(b"# Deploy!")); assert_ne!(selected, b"Deploy!");
        assert_eq!(hit.original_range(), hit.evidence_range());
        assert_eq!(hit.kind(), "heading"); assert_eq!(hit.language(), "markdown");
    }
}

#[cfg(feature = "markdown")]
#[test]
fn headings_with_unqualified_upstream_source_ranges_are_partial_not_false_negatives() {
    let budget = budget();
    let mut saved = archive(&[entry(b"a.md", b"---\ntitle: metadata\n---\n\n# Hidden\n"),
        entry(b"b.md", b"# Hidden\n")], true, &budget);
    let report = run(&mut saved, "Hidden", options(StructureMode::Headings, 10), &budget);
    assert!(!report.is_complete()); assert_eq!(report.stats().unsupported_files, 1);
    assert_eq!(report.stats().files_searched, 1); assert_eq!(report.hits().len(), 1);
    assert_eq!(report.hits()[0].ordinal(), 1);
}

#[cfg(not(feature = "markdown"))]
#[test]
fn snapshot_without_markdown_keeps_symbols_and_occurrences_but_refuses_heading_mode() {
    let budget = budget(); let mut saved = archive(&[entry(b"README.md", b"# needle\n")], true, &budget);
    assert!(matches!(StructureQuery::new(&mut saved, "needle", options(StructureMode::Headings, 10),
        &budget, allocations()), Err(StructureError::CapabilityUnavailable)));
    assert_eq!(saved.load_stats().loaded_members, 0);
}

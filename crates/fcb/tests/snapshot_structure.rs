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
    // Allow oversized-symbol negative controls to reach extraction admission.
    let limits = SnapshotLimits { max_file_bytes: 2 * 1024 * 1024, ..SnapshotLimits::default() };
    let saved = SnapshotBytes::encode(owner(), complete, "structure-test-v1", entries,
        limits, budget, allocation(1), || false).unwrap();
    PagedSnapshot::open(Cursor::new(saved.bytes().to_vec()), owner(), limits,
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

fn run_matching<R: Read + Seek>(saved: &mut PagedSnapshot<R>, needle: &str, opts: StructureOptions,
    skip: u64, case_sensitive: bool, count_all: bool, budget: &ResourceBudget) -> StructureReport {
    let max_steps = saved.directory().len() + 1;
    let mut query = StructureQuery::new_matching_page(saved, needle, opts, skip, case_sensitive,
        count_all, budget, allocations()).unwrap();
    let retained = budget.accounting().reserved().get();
    for _ in 0..max_steps {
        let before = query.stats();
        query.step(opts.generation, budget, || false).unwrap();
        assert!(query.stats().members_visited - before.members_visited <= 1);
        assert_eq!(budget.accounting().reserved().get(), retained,
            "member payloads, extractor state and transient matching tables must be released between steps");
        if query.state() != StructureState::Pending { return query.finish().unwrap(); }
    }
    panic!("matching structural query exceeded its one-member-per-step bound");
}

#[test]
fn insensitive_symbol_names_keep_original_utf8_and_utf16_anchors() {
    let text = "// 🚀\nfn Run() {}\nfn Runner() {}\nfn preRun() {}\n";
    for little in [None, Some(true), Some(false)] {
        let budget = budget();
        let bytes = little.map_or_else(|| text.as_bytes().to_vec(), |little| encoded(text, little));
        let mut saved = archive(&[entry(b"saved.rs", &bytes)], true, &budget);
        for (matching, needle, expected) in [(SymbolNameMode::Exact, "run", vec!["Run"]),
            (SymbolNameMode::Prefix, "rUN", vec!["Run", "Runner"]),
            (SymbolNameMode::Contains, "RuN", vec!["Run", "Runner", "preRun"])] {
            let mut opts = options(StructureMode::Symbols, 10); opts.name_mode = matching;
            let report = run_matching(&mut saved, needle, opts, 0, false, true, &budget);
            assert!(report.is_complete()); assert!(report.matches_seen_exact());
            assert!(!report.case_sensitive()); assert!(report.count_all_matches());
            assert_eq!(report.hits().iter().map(|hit| hit.name()).collect::<Vec<_>>(), expected);
            for hit in report.hits() {
                let range = hit.original_range();
                let wanted = little.map_or_else(|| hit.name().as_bytes().to_vec(),
                    |little| encoded(hit.name(), little)[2..].to_vec());
                assert_eq!(&bytes[range.start().get() as usize..range.end().get() as usize], wanted);
                let capture = hit.open(&mut saved, opts.generation, &budget,
                    [allocation(30), allocation(31)], || false).unwrap();
                assert_eq!(capture.bytes(), bytes);
            }
        }
        let sensitive = run(&mut saved, "run", options(StructureMode::Symbols, 10), &budget);
        assert!(sensitive.case_sensitive()); assert!(!sensitive.count_all_matches());
        assert_eq!(sensitive.matches_seen(), 0); assert!(sensitive.matches_seen_exact());
    }
}

fn complete_count_windows(mode: StructureMode, first: &[u8], second: &[u8], bytes: &[u8]) {
    let budget = budget();
    let mut saved = archive(&[entry(first, bytes), entry(second, bytes)], true, &budget);
    let opts = options(mode, 1);
    for skip in 0..6 {
        let report = run_matching(&mut saved, "needle", opts, skip, true, true, &budget);
        assert_eq!(report.state(), StructureState::Complete); assert!(report.is_complete());
        assert!(report.matches_seen_exact()); assert_eq!(report.matches_seen(), 6);
        assert_eq!(report.skipped_matches(), skip); assert_eq!(report.hits().len(), 1);
        assert_eq!(report.truncated(), skip < 5); assert_eq!(report.has_more_matches(), skip < 5);
        assert_eq!(report.next_match_offset(), if skip < 5 { Some(skip + 1) } else { None });
        assert_eq!(report.stats().members_loaded, 2);
    }
    let counted = run_matching(&mut saved, "needle", options(mode, 0), 0, true, true, &budget);
    assert_eq!(counted.matches_seen(), 6); assert!(counted.is_complete()); assert!(counted.truncated());
    assert!(counted.hits().is_empty()); assert!(!counted.has_more_matches()); assert_eq!(counted.next_match_offset(), None);
    drop(counted);
    let beyond = run_matching(&mut saved, "needle", opts, u64::MAX, true, true, &budget);
    assert_eq!(beyond.matches_seen(), 6); assert_eq!(beyond.skipped_matches(), 6);
    assert!(beyond.is_complete()); assert!(beyond.hits().is_empty()); assert!(!beyond.truncated());
    assert_eq!(beyond.next_match_offset(), None);
    drop(beyond);
    let absent = run_matching(&mut saved, "absent", options(mode, 0), 0, true, true, &budget);
    assert_eq!(absent.matches_seen(), 0); assert!(absent.is_complete()); assert!(!absent.truncated());
    drop(absent);
    let legacy = run(&mut saved, "needle", opts, &budget);
    assert_eq!(legacy.matches_seen(), 2); assert_eq!(legacy.state(), StructureState::Truncated);
    assert!(!legacy.matches_seen_exact()); assert_eq!(legacy.next_match_offset(), Some(1));
    drop(legacy);
    let probe = run(&mut saved, "needle", options(mode, 0), &budget);
    assert_eq!(probe.matches_seen(), 1); assert!(probe.truncated()); assert!(!probe.has_more_matches());
    assert_eq!(probe.next_match_offset(), None); assert!(!probe.matches_seen_exact());
}

#[test]
fn complete_counts_and_continuations_are_independent_of_retained_windows() {
    complete_count_windows(StructureMode::Symbols, b"a.rs", b"b.rs", b"fn needle() {}\nfn needle() {}\nfn needle() {}\n");
    complete_count_windows(StructureMode::Occurrences, b"a.txt", b"b.txt", b"needle needle needle\n");
    #[cfg(feature = "markdown")]
    complete_count_windows(StructureMode::Headings, b"a.md", b"b.md", b"# needle\n\n## needle\n\n### needle\n");
}

#[test]
fn occurrence_counts_exceed_row_limits_across_members_and_encoded_windows() {
    let text = "needle ".repeat(5003);
    for little in [None, Some(true), Some(false)] {
        let budget = budget();
        let bytes = little.map_or_else(|| text.as_bytes().to_vec(), |little| encoded(&text, little));
        let mut saved = archive(&[entry(b"a.txt", &bytes), entry(b"b.txt", b"needle needle needle")], true, &budget);
        let opts = options(StructureMode::Occurrences, 1);
        let report = run_matching(&mut saved, "needle", opts, 5001, true, true, &budget);
        assert!(report.is_complete()); assert!(report.matches_seen_exact()); assert_eq!(report.matches_seen(), 5006);
        assert_eq!(report.hits().len(), 1); assert_eq!(report.next_match_offset(), Some(5002));
        let range = report.hits()[0].original_range();
        assert_eq!(range.start().get(), if little.is_some() { 2 + 5001 * 14 } else { 5001 * 7 });
        assert_eq!(range.len().get(), if little.is_some() { 12 } else { 6 });
        drop(report);
        let later = run_matching(&mut saved, "needle", opts, 5003, true, true, &budget);
        assert_eq!(later.matches_seen(), 5006); assert_eq!(later.next_match_offset(), Some(5004));
        assert_eq!(later.hits()[0].ordinal(), 1); assert_eq!(later.hits()[0].original_range().start().get(), 0);
    }
}

#[test]
fn large_saved_occurrence_windows_keep_counts_and_exact_source_beyond_old_extractor_and_capture_limits() {
    let budget = budget();
    let mut bytes = vec![b' '; 2 * 1024 * 1024];
    bytes[..6].copy_from_slice(b"needle");
    bytes.extend_from_slice(b"needle needle");
    let limits = SnapshotLimits { max_file_bytes: 32 * 1024 * 1024, ..SnapshotLimits::default() };
    let encoded = SnapshotBytes::encode(owner(), true, "large-occurrence-v1",
        &[entry(b"large.txt", &bytes)], limits, &budget, allocation(1), || false).unwrap();
    let mut saved = PagedSnapshot::open(Cursor::new(encoded.bytes().to_vec()), owner(),
        SnapshotLimits::paged(), &budget, allocation(2), || false).unwrap();
    drop(encoded);
    let opts = options(StructureMode::Occurrences, 1);
    let first = run_matching(&mut saved, "needle", opts, 0, true, true, &budget);
    assert!(first.is_complete()); assert!(first.matches_seen_exact());
    assert_eq!(first.matches_seen(), 3); assert_eq!(first.next_match_offset(), Some(1));
    drop(first);
    let tail = run_matching(&mut saved, "needle", opts, 2, true, true, &budget);
    assert!(tail.is_complete()); assert!(tail.matches_seen_exact());
    assert_eq!(tail.matches_seen(), 3); assert_eq!(tail.skipped_matches(), 2);
    assert_eq!(tail.hits().len(), 1); assert_eq!(tail.next_match_offset(), None);
    let hit = &tail.hits()[0];
    assert_eq!(hit.original_range().start().get(), 2 * 1024 * 1024 + 7);
    assert_eq!(hit.original_range().end().get(), bytes.len() as u64);
    let capture = hit.open(&mut saved, opts.generation, &budget,
        [allocation(30), allocation(31)], || false).unwrap();
    assert_eq!(capture.bytes(), bytes);
}

#[test]
fn metadata_omissions_preserve_exact_counts_and_consumed_continuation_positions() {
    let budget = budget();
    let long_name = format!("needle_{}", "x".repeat(4096));
    let text = format!("fn {long_name}() {{}}\nfn needle_a() {{}}\nfn needle_b() {{}}\n");
    let mut saved = archive(&[entry(b"a.rs", text.as_bytes())], true, &budget);
    let mut opts = options(StructureMode::Symbols, 1); opts.name_mode = SymbolNameMode::Prefix;
    let report = run_matching(&mut saved, "needle_", opts, 0, true, true, &budget);
    assert_eq!(report.matches_seen(), 3); assert!(report.matches_seen_exact()); assert!(!report.is_complete());
    assert_eq!(report.stats().metadata_omitted, 1); assert_eq!(report.hits()[0].name(), "needle_a");
    assert_eq!(report.next_match_offset(), Some(2)); assert!(report.has_more_matches());
    drop(report);
    let next = run_matching(&mut saved, "needle_", opts, 2, true, true, &budget);
    assert_eq!(next.hits()[0].name(), "needle_b"); assert_eq!(next.matches_seen(), 3);
    assert!(next.is_complete()); assert!(!next.has_more_matches());
}

#[test]
fn counting_never_overrides_scope_coverage_work_limits_or_cancellation() {
    let budget = budget(); let source = b"fn needle() {}\nfn needle() {}\n";
    let mut saved = archive(&[entry(b"a.rs", source), entry(b"b.rs", source)], true, &budget);
    let mut opts = options(StructureMode::Symbols, 0); opts.max_source_bytes = source.len() as u64;
    let limited = run_matching(&mut saved, "needle", opts, 0, true, true, &budget);
    assert_eq!(limited.state(), StructureState::WorkLimit); assert_eq!(limited.matches_seen(), 2);
    assert!(!limited.matches_seen_exact()); assert!(!limited.is_complete()); assert!(limited.truncated());
    drop(limited);
    opts.max_source_bytes = MAX_STRUCTURE_SOURCE_BYTES;
    let baseline = budget.accounting().reserved().get();
    let mut query = StructureQuery::new_matching_page(&mut saved, "needle", opts, 0, true, true,
        &budget, allocations()).unwrap();
    assert_eq!(query.step(opts.generation, &budget, || false).unwrap(), StructureState::Pending);
    assert!(matches!(query.step(opts.generation, &budget, || true), Err(StructureError::Canceled)));
    assert!(matches!(query.finish(), Err(StructureError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    drop(saved);
    let oversized = vec![b'x'; StructureMode::Symbols.source_limit() + 1];
    let mut partial = archive(&[entry(b"a.rs", source), entry(b"b.bin", b"needle"), entry(b"c.rs", &oversized),
        SnapshotEntry { path: b"d.rs", observed_bytes: 1, data: SnapshotData::Unavailable("missing") }], false, &budget);
    let report = run_matching(&mut partial, "needle", opts, 0, true, true, &budget);
    assert_eq!(report.matches_seen(), 2); assert_eq!(report.state(), StructureState::Complete);
    assert!(!report.matches_seen_exact()); assert!(!report.is_complete());
    assert_eq!(report.stats().unsupported_files, 1); assert_eq!(report.stats().unavailable_files, 1);
    assert_eq!(report.stats().source_limited_files, 1);
}

#[test]
fn insensitive_occurrences_keep_whole_source_tokens_and_verified_encoded_anchors() {
    let text = "k K K kx _K $K 1K K\u{307}\nİ i\u{307} I\u{307} i I ß ẞ ss SS ΟΣ οσ ος É é e\u{301}";
    for little in [None, Some(true), Some(false)] {
        let budget = budget();
        let bytes = little.map_or_else(|| text.as_bytes().to_vec(), |little| encoded(text, little));
        let mut saved = archive(&[entry(b"saved.txt", &bytes)], true, &budget);
        for (needle, expected) in [
            ("K", vec!["k", "K", "K"]),
            ("i\u{307}", vec!["İ", "i\u{307}", "I\u{307}"]),
            ("i", vec!["i", "I"]), ("ß", vec!["ß", "ẞ"]),
            ("ss", vec!["ss", "SS"]), ("οσ", vec!["ΟΣ", "οσ"]),
            ("ος", vec!["ος"]), ("é", vec!["É", "é"]),
        ] {
            let opts = options(StructureMode::Occurrences, 16);
            let report = run_matching(&mut saved, needle, opts, 0, false, true, &budget);
            assert!(report.is_complete()); assert!(report.matches_seen_exact());
            assert!(!report.case_sensitive()); assert!(report.count_all_matches());
            assert_eq!(report.matches_seen(), expected.len() as u64);
            assert_eq!(report.hits().iter().map(|hit| hit.name()).collect::<Vec<_>>(), expected);
            for hit in report.hits() {
                let range = hit.original_range();
                let wanted = little.map_or_else(|| hit.name().as_bytes().to_vec(),
                    |little| encoded(hit.name(), little)[2..].to_vec());
                assert_eq!(&bytes[range.start().get() as usize..range.end().get() as usize], wanted);
                assert_eq!(hit.evidence_range(), range);
                let capture = hit.open(&mut saved, opts.generation, &budget,
                    [allocation(30), allocation(31)], || false).unwrap();
                assert_eq!(capture.bytes(), bytes);
            }
        }
        let legacy = run(&mut saved, "K", options(StructureMode::Occurrences, 16), &budget);
        assert!(legacy.case_sensitive());
        assert_eq!(legacy.hits().iter().map(|hit| hit.name()).collect::<Vec<_>>(), ["K"]);
    }
}

#[test]
fn lowercase_occurrence_counts_and_later_windows_span_members_without_retaining_tails() {
    let budget = budget();
    let text = "k K K";
    let utf16 = encoded(text, true);
    let mut saved = archive(&[entry(b"a.txt", text.as_bytes()), entry(b"b.txt", &utf16)], true, &budget);
    let opts = options(StructureMode::Occurrences, 1);
    for skip in 0..6 {
        let report = run_matching(&mut saved, "K", opts, skip, false, true, &budget);
        assert!(report.is_complete()); assert!(report.matches_seen_exact());
        assert_eq!(report.matches_seen(), 6); assert_eq!(report.skipped_matches(), skip);
        assert_eq!(report.hits().len(), 1);
        let hit = &report.hits()[0];
        assert_eq!(hit.name(), ["k", "K", "K"][skip as usize % 3]);
        assert_eq!(hit.ordinal(), skip as usize / 3);
        assert_eq!(report.next_match_offset(), if skip < 5 { Some(skip + 1) } else { None });
        assert_eq!(report.truncated(), skip < 5);
        assert_eq!(report.stats().members_loaded, 2);
    }
    let zero = run_matching(&mut saved, "K", options(StructureMode::Occurrences, 0), 0, false, true, &budget);
    assert!(zero.hits().is_empty()); assert_eq!(zero.matches_seen(), 6);
    assert!(zero.is_complete()); assert!(zero.truncated()); assert!(!zero.has_more_matches());
    assert_eq!(zero.next_match_offset(), None);
    drop(zero);
    let beyond = run_matching(&mut saved, "K", opts, u64::MAX, false, true, &budget);
    assert!(beyond.hits().is_empty()); assert_eq!(beyond.matches_seen(), 6);
    assert!(beyond.is_complete()); assert!(!beyond.truncated());
    drop(beyond);
    let page = run_matching(&mut saved, "K", opts, 1, false, false, &budget);
    assert_eq!(page.matches_seen(), 3); assert_eq!(page.hits()[0].name(), "K");
    assert_eq!(page.next_match_offset(), Some(2)); assert!(!page.matches_seen_exact());
}

#[test]
fn lowercase_source_names_wider_than_the_query_are_admitted_and_retained_exactly() {
    let budget = budget();
    let needle = "K".repeat(1024); let spelling = "K".repeat(1024);
    let bytes = encoded(&spelling, false);
    let mut saved = archive(&[entry(b"wide.txt", &bytes)], true, &budget);
    let report = run_matching(&mut saved, &needle, options(StructureMode::Occurrences, 1),
        0, false, true, &budget);
    assert!(report.is_complete()); assert_eq!(report.matches_seen(), 1);
    assert_eq!(report.hits()[0].name(), spelling);
    assert_eq!(report.stats().metadata_bytes, spelling.len());
    assert_eq!(report.stats().metadata_omitted, 0);
    assert_eq!(report.hits()[0].original_range().start().get(), 2);
    assert_eq!(report.hits()[0].original_range().end().get(), bytes.len() as u64);
}

#[cfg(feature = "markdown")]
#[test]
fn insensitive_headings_use_scalar_lowercase_without_folding_or_normalization() {
    let text = "# İDEA\n\n# Kelvin\n\n# Straße\n\n# École\n";
    for little in [None, Some(true), Some(false)] {
        let budget = budget();
        let bytes = little.map_or_else(|| text.as_bytes().to_vec(), |little| encoded(text, little));
        let mut saved = archive(&[entry(b"guide.md", &bytes)], true, &budget);
        for (mode, needle, expected) in [(SymbolNameMode::Exact, "i\u{307}dea", Some("İDEA")),
            (SymbolNameMode::Exact, "KELVIN", Some("Kelvin")),
            (SymbolNameMode::Prefix, "i", Some("İDEA")),
            (SymbolNameMode::Contains, "\u{307}d", Some("İDEA")),
            (SymbolNameMode::Contains, "aß", Some("Straße")),
            (SymbolNameMode::Exact, "STRASSE", None),
            (SymbolNameMode::Exact, "e\u{301}cole", None)] {
            let mut opts = options(StructureMode::Headings, 10); opts.name_mode = mode;
            let report = run_matching(&mut saved, needle, opts, 0, false, true, &budget);
            assert!(report.is_complete()); assert!(report.matches_seen_exact());
            assert_eq!(report.hits().first().map(|hit| hit.name()), expected);
            assert_eq!(report.hits().len(), usize::from(expected.is_some()));
            if let Some(hit) = report.hits().first() {
                let range = hit.original_range(); let selected = &bytes[range.start().get() as usize..range.end().get() as usize];
                let syntax = format!("# {}", hit.name());
                let expected = little.map_or_else(|| syntax.as_bytes().to_vec(), |little| encoded(&syntax, little)[2..].to_vec());
                assert!(selected.starts_with(&expected));
            }
        }
    }
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

#[test]
fn large_saved_occurrences_keep_bounded_windows_and_verified_original_members() {
    let budget = budget();
    let source = [vec![b' '; 768 * 1024], b"needle needle_suffix needle\n".to_vec()].concat();
    let mut saved = archive(&[entry(b"large.txt", &source)], true, &budget);
    let opts = options(StructureMode::Occurrences, 1);
    let report = run_matching(&mut saved, "needle", opts, 1, true, true, &budget);
    assert!(report.is_complete());
    assert!(report.matches_seen_exact());
    assert_eq!(report.matches_seen(), 2);
    assert_eq!(report.hits().len(), 1);
    assert_eq!(report.stats().source_limited_files, 0);
    assert_eq!(report.stats().peak_source_bytes, source.len());
    let hit = &report.hits()[0];
    assert_eq!(hit.original_range().start().get(), 768 * 1024 + 21);
    let opened = hit.open(&mut saved, opts.generation, &budget,
        [allocation(30), allocation(31)], || false).unwrap();
    let range = hit.original_range();
    assert_eq!(&opened.bytes()[range.start().get() as usize..range.end().get() as usize], b"needle");
    drop(saved);
    assert_eq!(opened.bytes(), source);
    assert_eq!(StructureMode::Occurrences.source_limit(), 32 * 1024 * 1024);
    assert_eq!(StructureMode::Symbols.source_limit(), 1024 * 1024);
}

#[test]
fn larger_saved_symbol_members_keep_exact_names_and_release_analysis_between_steps() {
    let prefix = "// 😀 retained context\r\n".repeat(5_000);
    let text = format!("{prefix}fn late_target() {{}}\r\n");
    for little in [true, false] {
        let budget = budget(); let source = encoded(&text, little);
        assert!(source.len() > 64 * 1024 && source.len() < StructureMode::Symbols.source_limit());
        let mut saved = archive(&[entry(b"large.rs", &source)], true, &budget);
        let opts = options(StructureMode::Symbols, 1);
        let report = run_matching(&mut saved, "late_target", opts, 0, true, true, &budget);
        assert!(report.is_complete()); assert!(report.matches_seen_exact());
        assert_eq!(report.matches_seen(), 1); assert_eq!(report.hits().len(), 1);
        let hit = &report.hits()[0];
        let range = hit.original_range();
        let expected = encoded("late_target", little)[2..].to_vec();
        assert_eq!(&source[range.start().get() as usize..range.end().get() as usize], expected);
        let capture = hit.open(&mut saved, opts.generation, &budget,
            [allocation(30), allocation(31)], || false).unwrap();
        drop(report); drop(saved);
        assert_eq!(capture.bytes(), source);
    }
}

#[test]
fn scoped_windows_freeze_archive_ordinals_before_limits_and_activation() {
    use fcb::search::snapshot_structure::StructureScope;
    let budget = budget();
    let target = b"needle needle";
    let mut saved = archive(&[
        entry(b"a", b"unrelated payload wider than our allowance"),
        entry(b"src-other", b"needle"), entry(b"src/a", target),
        entry(b"src/nested/b", b"needle"), entry(b"src0", b"needle")
    ], true, &budget);
    let mut opts = options(StructureMode::Occurrences, 1);
    opts.max_source_bytes = (target.len() + b"needle".len()) as u64;
    let mut query = StructureQuery::new_scoped_page(&mut saved, "needle", opts,
        StructureScope::Directory(b"src"), 2, true, true, &budget, allocations()).unwrap();
    assert_eq!(query.step(opts.generation, &budget, || false).unwrap(), StructureState::Pending);
    assert_eq!(query.step(opts.generation, &budget, || false).unwrap(), StructureState::Complete);
    let report = query.finish().unwrap();
    assert!(report.membership_complete()); assert!(report.is_complete());
    assert_eq!(report.selected_members(), 2); assert_eq!(report.stats().members_visited, 2);
    assert_eq!(report.stats().loaded_bytes, opts.max_source_bytes); assert_eq!(report.matches_seen(), 3);
    let hit = &report.hits()[0]; assert_eq!(hit.ordinal(), 3);
    let capture = hit.open(&mut saved, opts.generation, &budget,
        [allocation(80), allocation(81)], || false).unwrap();
    assert_eq!(capture.bytes(), b"needle");
}

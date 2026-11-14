#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::{io::Cursor, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{CaptureRequest, CompleteCapture, IndexLimits, ParsedQuery, QueryGeneration,
    QueryOptions, ReferenceScanOracle, ResourceAllocationId, ResourceBudget, SearchDocument,
    SearchMode, StreamReadStep, UnicodeNormalization};
use fcb::search::expression::{ExpressionError, ExpressionOptions, ExpressionPlan, ExpressionQuery,
    ExpressionReport, ExpressionState};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::snapshot_index::{IndexDecision, SnapshotIndex};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2684).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn options(limit: usize, work: u64) -> ExpressionOptions {
    ExpressionOptions { generation: QueryGeneration::new(owner(), 1).unwrap(),
        first_file: FileId::new(owner(), 100).unwrap(), first_revision: SourceRevision::new(owner(), 200).unwrap(),
        max_matches: limit, max_scan_bytes: work }
}
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn archive(entries: &[SnapshotEntry<'_>], budget: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    let saved = SnapshotBytes::encode(owner(), true, "expression-lowercase-test-v1", entries,
        SnapshotLimits::default(), budget, allocation(1), || false).unwrap();
    PagedSnapshot::open(Cursor::new(saved.bytes().to_vec()), owner(), SnapshotLimits::default(),
        budget, allocation(2), || false).unwrap()
}
fn run(archive: &mut PagedSnapshot<Cursor<Vec<u8>>>, text: &str, case_sensitive: bool,
    index: Option<&SnapshotIndex>, opts: ExpressionOptions, count_all: bool,
    budget: &ResourceBudget) -> ExpressionReport {
    let plan = ExpressionPlan::parse_with_matching(owner(), text, case_sensitive,
        budget, allocation(10), allocation(1000)).unwrap();
    assert_eq!(plan.case_sensitive(), case_sensitive);
    let ids = [allocation(11), allocation(12), allocation(13)];
    let mut query = match (index, count_all) {
        (None, false) => ExpressionQuery::new(archive, &plan, opts, budget, ids).unwrap(),
        (None, true) => ExpressionQuery::new_counting(archive, &plan, opts, budget, ids).unwrap(),
        (Some(index), false) => ExpressionQuery::new_indexed(archive, &plan, index, opts, budget, ids).unwrap(),
        (Some(index), true) => ExpressionQuery::new_indexed_counting(archive, &plan, index, opts, budget, ids).unwrap(),
    };
    for _ in 0..100_000 {
        if query.state() != ExpressionState::Pending {
            let report = query.finish().unwrap();
            assert_eq!(report.case_sensitive(), case_sensitive);
            assert_eq!(report.realized_mode(), plan.realized_mode());
            assert_eq!(report.count_all_matches(), count_all);
            return report;
        }
        // Split every multibyte scalar, UTF-16 unit and BOM across work quanta.
        query.step(StreamReadStep { max_bytes: 1, max_calls: 1, max_hits: 1 },
            opts.generation, budget, || false).unwrap();
        assert!(query.stats().last_step_scanned_bytes <= 1);
        assert!(query.stats().scanned_bytes <= opts.max_scan_bytes);
    }
    panic!("lowercase expression failed to finish within bounded fixture work");
}
fn coords(report: &ExpressionReport) -> Vec<(u64, u64, u64, u64)> {
    report.hits().iter().map(|hit| (hit.file().get(), hit.revision().get(),
        hit.original_range().start().get(), hit.original_range().end().get())).collect()
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn exact_default_stays_exact_and_all_lowercase_terms_use_one_policy() {
    let budget = budget();
    let plan = ExpressionPlan::parse(owner(), "ana required -forbidden", &budget,
        allocation(10), allocation(1000)).unwrap();
    assert!(plan.case_sensitive());
    assert_eq!(plan.realized_mode(), "decoded-text-expression");
    drop(plan);
    let entries = [entry(b"a.rs", b"BANANA\nREQUIRED"), entry(b"b.rs", b"BANANA required FoRbIdDeN"),
        entry(b"c.rs", b"banana"), entry(b"d.py", b"BANANA REQUIRED"), entry(b"e.rs", b"REQUIRED")];
    let mut archive = archive(&entries, &budget);
    let exact = run(&mut archive, "ana required -forbidden lang:rust", true, None,
        options(10, 10000), false, &budget);
    assert!(exact.is_complete()); assert_eq!(exact.matches_seen(), 0); drop(exact);
    let lower = run(&mut archive, "ana required -forbidden lang:rust", false, None,
        options(10, 10000), false, &budget);
    assert!(lower.is_complete()); assert_eq!(lower.realized_mode(), "unicode-lowercase-expression");
    assert_eq!(coords(&lower), [(100, 200, 1, 4), (100, 200, 3, 6)]);
    assert_eq!(lower.stats().predicate_rejections, 2);
    assert_eq!(lower.stats().metadata_excluded, 1);
    assert_eq!(lower.stats().members_loaded, 4, "all term scans share one verified member load");
}

#[test]
fn unicode_and_both_utf16_orders_agree_with_retained_capture_expression_search() {
    let budget = budget();
    let little = utf16("KELVIN İİ REQUIRED", true);
    let big = utf16("KELVIN İİ REQUIRED", false);
    let entries = [entry(b"a.rs", "KELVIN İİ REQUIRED".as_bytes()), entry(b"b.rs", &little),
        entry(b"c.rs", &big), entry(b"d.rs", "KELVIN İİ REQUIRED FoRbIdDeN".as_bytes())];
    let captures: Vec<_> = entries.iter().enumerate().map(|(ordinal, entry)| {
        let SnapshotData::Captured(bytes) = entry.data else { unreachable!() };
        CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), 100 + ordinal as u64).unwrap(),
            SourceRevision::new(owner(), 200 + ordinal as u64).unwrap()).unwrap(),
            ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
    }).collect();
    let docs: Vec<_> = captures.iter().zip(&entries).map(|(capture, entry)|
        SearchDocument::new(capture.request().file(), std::str::from_utf8(entry.path).unwrap(), capture)).collect();
    let mut archive = archive(&entries, &budget);
    for text in ["kelvin required -forbidden", "i required -forbidden", "i\u{307} required",
        "\u{307} required -forbidden", "\"kelvin i\u{307}\" required", "kelvin -REQUIRED"] {
        for (limit, counting) in [(1, false), (4, false), (100, false), (0, true), (1, true)] {
            let mut query_options = QueryOptions::new(options(limit, 0).generation).with_max_matches(limit);
            query_options.mode = SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::Exact };
            let syntax = ParsedQuery::parse(text).unwrap();
            let expected = if counting {
                ReferenceScanOracle::scan_collection_counting_with_cancel(&docs, &syntax, &query_options, || false)
            } else { ReferenceScanOracle::scan_collection(&docs, &syntax, &query_options) }.unwrap();
            let actual = run(&mut archive, text, false, None, options(limit, 100000), counting, &budget);
            let wanted: Vec<_> = expected.matches.iter().map(|hit| (hit.file_id.get(), hit.revision.get(),
                hit.original_byte_range.start().get(), hit.original_byte_range.end().get())).collect();
            assert_eq!(coords(&actual), wanted, "{text} limit={limit} counting={counting}");
            assert_eq!(actual.matches_seen(), expected.total_matches_counted as u64);
            assert_eq!(actual.is_complete(), expected.is_complete());
            assert_eq!(actual.hits_truncated(), actual.matches_seen() > actual.hits().len() as u64);
        }
    }
}

#[test]
fn exact_index_negative_is_not_a_lowercase_negative_and_counting_keeps_its_bounds() {
    let budget = budget();
    let mut archive = archive(&[entry(b"a.rs", b"NEEDLE REQUIRED"),
        entry(b"b.rs", b"NEEDLE NEEDLE required"), entry(b"c.rs", b"NEEDLE required BLOCKED"),
        entry(b"d.rs", b"no primary occurrence")], &budget);
    let index = SnapshotIndex::build(&mut archive, IndexLimits::default(), &budget,
        [allocation(20), allocation(21), allocation(22), allocation(23)], || false).unwrap();
    // Negative control: an exact prefilter would eliminate a valid case-insensitive hit.
    assert_eq!(index.text_decision(0, Some("needle")), IndexDecision::Excluded);
    for (limit, counting) in [(1, false), (10, false), (0, true), (1, true)] {
        let direct = run(&mut archive, "needle required -blocked", false, None,
            options(limit, 10000), counting, &budget);
        let expected = (coords(&direct), direct.matches_seen(), direct.is_complete()); drop(direct);
        let indexed = run(&mut archive, "needle required -blocked", false, Some(&index),
            options(limit, 10000), counting, &budget);
        assert_eq!((coords(&indexed), indexed.matches_seen(), indexed.is_complete()), expected);
        assert_eq!(indexed.stats().index_eliminated, 0);
        assert_eq!(indexed.stats().index_candidates, 0);
        assert_eq!(indexed.stats().index_fallbacks, indexed.stats().members_loaded);
        if counting {
            assert!(indexed.is_complete()); assert_eq!(indexed.matches_seen(), 3);
            assert_eq!(indexed.matching_files(), 2); assert!(indexed.hits_truncated());
            assert_eq!(indexed.hits().len(), limit);
        }
    }
}

#[test]
fn partial_lowercase_expansions_reopen_complete_original_source_units() {
    for encoding in 0..3 {
        let budget = budget(); let text = "K İİ REQUIRED";
        let bytes = match encoding { 0 => text.as_bytes().to_vec(), 1 => utf16(text, true), _ => utf16(text, false) };
        let mut archive = archive(&[entry(b"a.rs", &bytes)], &budget);
        let report = run(&mut archive, "\u{307} required", false, None, options(10, 10000), false, &budget);
        assert!(report.is_complete()); assert_eq!(report.matches_seen(), 2);
        assert_eq!(report.hits()[0].occurrence_id(), 1); assert_eq!(report.hits()[1].occurrence_id(), 2);
        for (ordinal, hit) in report.hits().iter().copied().enumerate() {
            let expected_start = if encoding == 0 { 4 } else { 6 } + 2 * ordinal as u64;
            assert_eq!(hit.original_range().start().get(), expected_start);
            assert_eq!(hit.original_range().len().get(), 2);
            let capture = hit.open(&mut archive, options(10, 0).generation, &budget,
                [allocation(30), allocation(31)], || false).unwrap();
            let start = expected_start as usize;
            let wanted = match encoding { 0 => vec![0xc4, 0xb0], 1 => vec![0x30, 0x01], _ => vec![0x01, 0x30] };
            assert_eq!(&capture.bytes()[start..start + 2], wanted);
        }
    }
}

#[test]
fn lowercase_does_not_promise_full_case_folding_or_canonical_equivalence() {
    let budget = budget();
    let mut archive = archive(&[entry(b"a.rs", "Straße REQUIRED e\u{301}".as_bytes())], &budget);
    for text in ["strasse required", "é required"] {
        let report = run(&mut archive, text, false, None, options(10, 10000), false, &budget);
        assert!(report.is_complete()); assert_eq!(report.matches_seen(), 0);
    }
    let report = run(&mut archive, "straße required", false, None, options(10, 10000), false, &budget);
    assert!(report.is_complete()); assert_eq!(report.matches_seen(), 1);
}

#[test]
fn work_limits_and_cancellation_cannot_turn_unknown_predicates_into_negative_proof() {
    let budget = budget();
    let mut archive = archive(&[entry(b"a.rs", b"NEEDLE................FORBIDDEN")], &budget);
    let report = run(&mut archive, "needle -forbidden", false, None, options(1, 8), true, &budget);
    assert_eq!(report.state(), ExpressionState::WorkLimit); assert!(!report.is_complete());
    assert!(report.hits().is_empty()); assert_eq!(report.matches_seen(), 0); drop(report);
    let plan = ExpressionPlan::parse_with_matching(owner(), "needle -forbidden", false,
        &budget, allocation(10), allocation(1000)).unwrap();
    let baseline = budget.accounting().reserved().get();
    let opts = options(1, 10000);
    for stale in [false, true] {
        let mut query = ExpressionQuery::new_counting(&mut archive, &plan, opts, &budget,
            [allocation(11), allocation(12), allocation(13)]).unwrap();
        // Admit the actual member and begin its exclusion predicate first.
        for _ in 0..3 {
            query.step(StreamReadStep { max_bytes: 1, max_calls: 1, max_hits: 1 },
                opts.generation, &budget, || false).unwrap();
        }
        assert_eq!(query.stats().members_loaded, 1);
        let generation = if stale { QueryGeneration::new(owner(), 2).unwrap() } else { opts.generation };
        let expected = if stale { ExpressionError::StaleQuery } else { ExpressionError::Canceled };
        assert_eq!(query.step(StreamReadStep::default(), generation, &budget, || !stale), Err(expected));
        assert!(query.finish().is_err());
        assert_eq!(budget.accounting().reserved().get(), baseline, "canceled source/matcher leases must drain");
    }
}

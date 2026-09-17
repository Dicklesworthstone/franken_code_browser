#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::{io::Cursor, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{CaptureRequest, CompleteCapture, IndexLimits, ParsedQuery, QueryGeneration,
    QueryOptions, ReferenceScanOracle, ResourceAllocationId, ResourceBudget, SearchDocument, StreamReadStep,
    ReaderLimits, ReadingTarget, ReadingSeekState, ReadingWindowOptions};
use fcb::search::expression::{ExpressionPlan, ExpressionQuery, ExpressionOptions, ExpressionReport,
    ExpressionState, ExpressionError};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::snapshot_index::SnapshotIndex;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2671).unwrap() }
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
fn archive(entries: &[SnapshotEntry<'_>], complete: bool, budget: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    let saved = SnapshotBytes::encode(owner(), complete, "expression-test-v1", entries,
        SnapshotLimits::default(), budget, allocation(1), || false).unwrap();
    PagedSnapshot::open(Cursor::new(saved.bytes().to_vec()), owner(), SnapshotLimits::default(), budget, allocation(2), || false).unwrap()
}
fn run(archive: &mut PagedSnapshot<Cursor<Vec<u8>>>, text: &str, index: Option<&SnapshotIndex>,
    opts: ExpressionOptions, budget: &ResourceBudget) -> ExpressionReport {
    let plan = ExpressionPlan::parse(owner(), text, budget, allocation(10), allocation(1000)).unwrap();
    let ids = [allocation(11), allocation(12), allocation(13)];
    let mut query = match index {
        None => ExpressionQuery::new(archive, &plan, opts, budget, ids).unwrap(),
        Some(index) => ExpressionQuery::new_indexed(archive, &plan, index, opts, budget, ids).unwrap(),
    };
    for _ in 0..100_000 {
        if query.state() != ExpressionState::Pending { return query.finish().unwrap(); }
        query.step(StreamReadStep { max_bytes: 7, max_calls: 4, max_hits: 2 }, opts.generation, budget, || false).unwrap();
        assert!(query.stats().last_step_scanned_bytes <= 7);
    }
    panic!("expression failed to finish within bounded fixture work");
}
fn coords(report: &ExpressionReport) -> Vec<(u64, u64, u64, u64)> {
    report.hits().iter().map(|h| (h.file().get(), h.revision().get(), h.original_range().start().get(), h.original_range().end().get())).collect()
}
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    bytes
}

#[test]
fn all_terms_are_document_wide_but_must_belong_to_the_same_capture() {
    let budget = budget();
    let entries = [entry(b"a.rs", b"banana\nrequired"), entry(b"b.rs", b"banana"),
        entry(b"c.rs", b"required"), entry(b"d.rs", b"banana required\nforbidden")];
    let mut archive = archive(&entries, true, &budget);
    let report = run(&mut archive, "ana required -forbidden lang:rust", None, options(100, 1_000_000), &budget);
    assert!(report.is_complete());
    assert_eq!(coords(&report), [(100, 200, 1, 4), (100, 200, 3, 6)]);
    assert_eq!(report.stats().predicate_rejections, 2);
    assert_eq!(report.stats().members_loaded, 4);
    assert_eq!(archive.load_stats().loaded_members, 4, "term scans must move the same verified buffer, not reload it");
}

#[test]
fn utf8_and_utf16_expressions_equal_the_existing_capture_query_engine() {
    let budget = budget(); let encoded = utf16("banana required");
    let entries = [entry(b"a.rs", b"banana required"), entry(b"b.rs", &encoded),
        entry(b"c.py", b"banana required forbidden"), entry(b"d.rs", b"not a match")];
    let captures: Vec<_> = entries.iter().enumerate().map(|(ordinal, entry)| {
        let SnapshotData::Captured(bytes) = entry.data else { unreachable!() };
        CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), 100 + ordinal as u64).unwrap(),
            SourceRevision::new(owner(), 200 + ordinal as u64).unwrap()).unwrap(), ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
    }).collect();
    let docs: Vec<_> = captures.iter().zip(&entries).map(|(capture, entry)|
        SearchDocument::new(capture.request().file(), std::str::from_utf8(entry.path).unwrap(), capture)).collect();
    let mut archive = archive(&entries, true, &budget);
    for expression in ["ana", "ana required", "ana -forbidden", "\"banana required\" lang:rust", "ana path:*.py", "ana missing", "ana -\"not present\""] {
        for limit in [1, 2, 3, 100] {
            let syntax = ParsedQuery::parse(expression).unwrap();
            let expected = ReferenceScanOracle::scan_collection(&docs, &syntax, &QueryOptions::new(options(limit, 0).generation).with_max_matches(limit)).unwrap();
            let actual = run(&mut archive, expression, None, options(limit, 1_000_000), &budget);
            let wanted: Vec<_> = expected.matches.iter().map(|h| (h.file_id.get(), h.revision.get(), h.original_byte_range.start().get(), h.original_byte_range.end().get())).collect();
            assert_eq!(coords(&actual), wanted, "{expression} limit={limit}");
            assert_eq!(actual.is_complete(), expected.is_complete(), "{expression} limit={limit}");
            assert_eq!(actual.matches_seen(), expected.total_matches_counted as u64);
        }
    }
}

#[test]
fn unavailable_members_outside_the_metadata_scope_do_not_poison_a_complete_answer() {
    let budget = budget();
    let entries = [entry(b"a.rs", b"needle"), SnapshotEntry { path: b"b.py", observed_bytes: 99, data: SnapshotData::Unavailable("MISSING") }];
    let mut archive = archive(&entries, true, &budget);
    let report = run(&mut archive, "needle lang:rust", None, options(100, 1000), &budget);
    assert!(report.is_complete()); assert_eq!(report.stats().scope_files, 1); assert_eq!(report.stats().unavailable_files, 0);
    assert_eq!(report.stats().metadata_excluded, 1); drop(report);
    let report = run(&mut archive, "absent", None, options(100, 1000), &budget);
    assert!(!report.is_complete()); assert_eq!(report.stats().unavailable_files, 1); assert!(report.hits().is_empty());
}

#[test]
fn unfinished_discovery_and_unsupported_predicates_cannot_prove_a_negative() {
    for complete in [false, true] {
        let budget = budget();
        let mut archive = archive(&[entry(b"a.rs", b"needle\xff")], complete, &budget);
        let report = run(&mut archive, "needle -forbidden", None, options(100, 1000), &budget);
        assert!(!report.is_complete()); assert_eq!(report.stats().unsupported_files, 1); assert!(report.hits().is_empty());
    }
    let budget = budget(); let mut archive = archive(&[], false, &budget);
    assert!(!run(&mut archive, "absent", None, options(1, 0), &budget).is_complete());
}

#[test]
fn work_limit_counts_every_term_and_does_not_publish_unproven_primary_hits() {
    let budget = budget();
    let mut archive = archive(&[entry(b"a.rs", b"needle................forbidden")], true, &budget);
    let report = run(&mut archive, "needle -forbidden", None, options(100, 8), &budget);
    assert_eq!(report.state(), ExpressionState::WorkLimit); assert!(!report.is_complete());
    assert!(report.hits().is_empty()); assert!(report.stats().scanned_bytes <= 8); drop(report);
    let report = run(&mut archive, "needle needle", None, options(100, 31), &budget);
    assert_eq!(report.state(), ExpressionState::WorkLimit); assert!(report.stats().scanned_bytes <= 31);
    assert_eq!(report.stats().members_loaded, 1);
}

#[test]
fn exact_limit_lookahead_ignores_predicate_rejections_and_zero_limit_is_truthful() {
    let budget = budget();
    let mut archive = archive(&[entry(b"a.rs", b"needle required"), entry(b"b.rs", b"needle")], true, &budget);
    let report = run(&mut archive, "needle required", None, options(1, 10000), &budget);
    assert!(report.is_complete()); assert_eq!(report.matches_seen(), 1); drop(report);
    let report = run(&mut archive, "absent required", None, options(0, 10000), &budget);
    assert!(report.is_complete()); assert_eq!(report.matches_seen(), 0); drop(report);
    let report = run(&mut archive, "needle required", None, options(0, 10000), &budget);
    assert_eq!(report.state(), ExpressionState::Truncated); assert_eq!(report.matches_seen(), 1); assert!(report.hits().is_empty());
}

#[test]
fn persistent_primary_prefilter_preserves_full_expression_answers_and_utf16_fallback() {
    let budget = budget(); let encoded = utf16("banana required");
    let mut archive = archive(&[entry(b"a.rs", b"banana required"), entry(b"b.rs", &encoded),
        entry(b"c.rs", b"banana forbidden required"), entry(b"d.rs", b"no hit")], true, &budget);
    let index = SnapshotIndex::build(&mut archive, IndexLimits::default(), &budget,
        [allocation(20), allocation(21), allocation(22), allocation(23)], || false).unwrap();
    for syntax in ["ana required -forbidden", "absent -banana", "ana lang:rust", "ana missing"] {
        let plain = run(&mut archive, syntax, None, options(100, 10000), &budget);
        let expected = (coords(&plain), plain.matches_seen(), plain.is_complete()); drop(plain);
        let indexed = run(&mut archive, syntax, Some(&index), options(100, 10000), &budget);
        assert_eq!((coords(&indexed), indexed.matches_seen(), indexed.is_complete()), expected);
        assert!(indexed.stats().index_eliminated > 0);
        assert_eq!(indexed.stats().index_fallbacks, 1);
    }
}

#[test]
fn exact_utf16_hit_reopens_into_existing_reader_and_retains_bytes_after_archive_close() {
    let budget = budget(); let bytes = utf16("head\r\nbanana required");
    let mut archive = archive(&[entry(b"\xff.rs", &bytes)], true, &budget);
    let report = run(&mut archive, "ana required lang:rs", None, options(100, 10000), &budget);
    let hit = report.hits()[0];
    assert_eq!(hit.original_range().start().get(), 16);
    assert!(hit.open(&mut archive, QueryGeneration::new(owner(), 99).unwrap(), &budget,
        [allocation(30), allocation(31)], || false).is_err());
    let capture = hit.open(&mut archive, options(100, 0).generation, &budget, [allocation(30), allocation(31)], || false).unwrap();
    drop(report); drop(archive);
    assert_eq!(capture.bytes(), bytes);
    let reader = capture.reader(ReaderLimits::default(), &budget, allocation(32)).unwrap();
    let seek = reader.seek(ReadingTarget::Byte(fcb::ByteOffset::new(0)), options(100, 0).generation).unwrap();
    let ReadingSeekState::Ready(anchor) = seek.state() else { panic!("start must be ready") };
    let window = reader.window(anchor, options(100, 0).generation, ReadingWindowOptions::default(), &budget, allocation(33), || false).unwrap();
    assert_eq!(window.line_text(0), Some("head")); assert_eq!(window.line_text(1), Some("banana required"));
}

#[test]
fn cancel_and_stale_delivery_release_private_candidate_state() {
    let budget = budget(); let mut archive = archive(&[entry(b"a", b"needle required")], true, &budget);
    let plan = ExpressionPlan::parse(owner(), "needle required", &budget, allocation(10), allocation(1000)).unwrap();
    let baseline = budget.accounting().reserved().get();
    let opts = options(10, 1000);
    let mut query = ExpressionQuery::new(&mut archive, &plan, opts, &budget, [allocation(11), allocation(12), allocation(13)]).unwrap();
    assert_eq!(query.step(StreamReadStep::default(), opts.generation, &budget, || true), Err(ExpressionError::Canceled));
    assert!(matches!(query.finish(), Err(ExpressionError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    let mut query = ExpressionQuery::new(&mut archive, &plan, opts, &budget, [allocation(11), allocation(12), allocation(13)]).unwrap();
    assert_eq!(query.step(StreamReadStep::default(), QueryGeneration::new(owner(), 2).unwrap(), &budget, || false), Err(ExpressionError::StaleQuery));
    assert!(query.finish().is_err()); assert_eq!(budget.accounting().reserved().get(), baseline);
}

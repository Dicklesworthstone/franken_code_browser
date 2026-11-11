#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! Real archive, persisted candidates and shared streaming verification. Row
//! retention must not bound exhaustive counts or manufacture complete negatives.
use std::io::Cursor;
use fcb::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, SourceRevision};
use fcb::search::{IndexLimits, ResourceAllocationId, ResourceBudget, StreamingNeedle, StreamReadStep};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::paged_snapshot::{IndexedNeedle, PagedQuery, PagedQueryOptions, PagedQueryState,
    PagedReport, PagedSearchError, PagedSnapshot};
use fcb::search::snapshot_index::{SnapshotIndex, paged::PagedPostings};
use fcb::search::expression::{ExpressionPlan, ExpressionQuery, ExpressionOptions, ExpressionReport, ExpressionState};

#[derive(Clone, Copy, Debug)]
enum Route { Direct, Forward, Postings, Paged }
const ROUTES: [Route; 4] = [Route::Direct, Route::Forward, Route::Postings, Route::Paged];
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(8861).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn archive(entries: &[SnapshotEntry<'_>], complete: bool, budget: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    let encoded = SnapshotBytes::encode(owner(), complete, "counted-saved-v1", entries,
        SnapshotLimits::default(), budget, id(1), || false).unwrap();
    PagedSnapshot::open(Cursor::new(encoded.bytes().to_vec()), owner(), SnapshotLimits::default(),
        budget, id(2), || false).unwrap()
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    format!("\u{feff}{text}").encode_utf16().flat_map(|unit|
        if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}
fn literal(route: Route, entries: &[SnapshotEntry<'_>], complete: bool,
    limit: usize, grams: usize, counted: bool) -> PagedReport {
    let budget = budget(); let mut source = archive(entries, complete, &budget);
    let index = SnapshotIndex::build(&mut source, IndexLimits { max_total_grams: grams, ..Default::default() },
        &budget, [id(3), id(4), id(5), id(6)], || false).unwrap();
    let postings = index.invert(&budget, id(7), || false).unwrap();
    let artifact = postings.encode_paged(&budget, id(8), || false).unwrap();
    let mut paged = PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(),
        source.directory(), 1, &budget, id(9), || false).unwrap();
    let plain = StreamingNeedle::text(owner(), "ana", &budget, id(10)).unwrap();
    let indexed = IndexedNeedle::text(owner(), "ana", &budget, id(11)).unwrap();
    let options = PagedQueryOptions { generation: generation(), first_file: FileId::new(owner(), 1).unwrap(),
        first_revision: SourceRevision::new(owner(), 1).unwrap(), max_matches: limit };
    let ids = [id(12), id(13), id(14)];
    let mut query = match (route, counted) {
        (Route::Direct, true) => PagedQuery::new_counting(&mut source, &plain, options, &budget, ids),
        (Route::Forward, true) => PagedQuery::new_indexed_counting(&mut source, &indexed, &index, options, &budget, ids),
        (Route::Postings, true) => PagedQuery::new_postings_counting(&mut source, &indexed, &postings, options, &budget, ids),
        (Route::Paged, true) => PagedQuery::new_paged_counting(&mut source, &indexed, &mut paged, options, &budget, ids),
        (Route::Direct, false) => PagedQuery::new(&mut source, &plain, options, &budget, ids),
        (Route::Forward, false) => PagedQuery::new_indexed(&mut source, &indexed, &index, options, &budget, ids),
        (Route::Postings, false) => PagedQuery::new_postings(&mut source, &indexed, &postings, options, &budget, ids),
        (Route::Paged, false) => PagedQuery::new_paged(&mut source, &indexed, &mut paged, options, &budget, ids),
    }.unwrap();
    for _ in 0..10_000 {
        if query.state() != PagedQueryState::Pending { break; }
        query.step(StreamReadStep { max_bytes: 7, max_calls: 2, max_hits: 1 }, generation(), &budget, || false).unwrap();
        assert!(query.stats().last_step_scanned_bytes <= 7);
    }
    let terminal = query.state(); assert_ne!(terminal, PagedQueryState::Pending);
    assert_eq!(query.step(StreamReadStep::default(), generation(), &budget, || false).unwrap(), terminal);
    query.finish().unwrap()
}

#[test]
fn exact_counts_and_prefixes_agree_across_every_persisted_candidate_route() {
    let le = utf16("banana banana", true); let be = utf16("banana", false);
    let entries = [entry(b"a.rs", &le), entry(b"b.rs", &be), entry(b"c.rs", b"banana"),
        entry(b"d.rs", b""), entry(b"e.rs", b"unrelated")];
    for route in ROUTES { for grams in [0, 100_000] { for limit in [0, 1, 5, 8] {
        let report = literal(route, &entries, true, limit, grams, true);
        assert!(report.count_all_matches() && report.is_complete(), "{route:?}, grams={grams}, limit={limit}");
        assert_eq!(report.matches_seen(), 8); assert_eq!(report.matching_files(), 3);
        assert_eq!(report.hits().len(), limit); assert_eq!(report.max_matches(), limit);
        assert_eq!(report.truncated(), limit < 8);
        assert_eq!(report.retained_matching_files(), match limit { 0 => 0, 1 => 1, 5 => 2, _ => 3 });
        if limit > 0 {
            assert_eq!(report.hits()[0].ordinal(), 0);
            assert_eq!(report.hits()[0].original_range().start().get(), 4);
            assert_eq!(report.hits()[0].original_range().end().get(), 10);
        }
        assert!(report.stats().peak_source_bytes <= le.len());
    } } }
    // Negative control: the legacy stored-row query cannot establish the same
    // total after observing only one retained and one extra occurrence.
    let legacy = literal(Route::Direct, &entries, true, 1, 0, false);
    assert!(!legacy.is_complete() && legacy.truncated()); assert_eq!(legacy.matches_seen(), 2);
    assert!(!legacy.count_all_matches());
}

#[test]
fn count_only_keeps_unavailable_unsupported_and_open_membership_explicit() {
    let unavailable = SnapshotEntry { path: b"z.rs", observed_bytes: 50, data: SnapshotData::Unavailable("READ_DENIED") };
    for route in ROUTES {
        let report = literal(route, &[entry(b"a.rs", b"banana"), unavailable], true, 0, 100_000, true);
        assert_eq!(report.matches_seen(), 2); assert_eq!(report.matching_files(), 1);
        assert!(!report.is_complete()); assert_eq!(report.unavailable_files(), 1);
        let report = literal(route, &[entry(b"a.rs", b"banana"), entry(b"b.rs", b"banana\xff")], true, 0, 0, true);
        assert_eq!(report.matches_seen(), 2); assert_eq!(report.stats().unsupported_files, 1);
        assert!(!report.is_complete());
        let report = literal(route, &[entry(b"a.rs", b"unrelated")], false, 0, 100_000, true);
        assert_eq!(report.matches_seen(), 0); assert!(!report.is_complete()); assert!(!report.truncated());
        let report = literal(route, &[], true, 0, 0, true);
        assert!(report.is_complete()); assert_eq!(report.matches_seen(), 0); assert!(!report.truncated());
    }
}

fn expression(entries: &[SnapshotEntry<'_>], query: &str, limit: usize, work: u64,
    indexed: bool) -> ExpressionReport {
    let budget = budget(); let mut source = archive(entries, true, &budget);
    let index = SnapshotIndex::build(&mut source, IndexLimits::default(), &budget,
        [id(3), id(4), id(5), id(6)], || false).unwrap();
    let plan = ExpressionPlan::parse(owner(), query, &budget, id(7), id(100)).unwrap();
    let options = ExpressionOptions { generation: generation(), first_file: FileId::new(owner(), 1).unwrap(),
        first_revision: SourceRevision::new(owner(), 1).unwrap(), max_matches: limit, max_scan_bytes: work };
    let mut query = if indexed {
        ExpressionQuery::new_indexed_counting(&mut source, &plan, &index, options, &budget, [id(8), id(9), id(10)])
    } else { ExpressionQuery::new_counting(&mut source, &plan, options, &budget, [id(8), id(9), id(10)]) }.unwrap();
    for _ in 0..10_000 {
        if query.state() != ExpressionState::Pending { break; }
        query.step(StreamReadStep { max_bytes: 7, max_calls: 2, max_hits: 1 }, generation(), &budget, || false).unwrap();
        assert!(query.stats().last_step_scanned_bytes <= 7);
    }
    assert_ne!(query.state(), ExpressionState::Pending);
    query.finish().unwrap()
}

#[test]
fn counted_expressions_apply_filters_and_predicates_before_counting_primary_overlaps() {
    let le = utf16("banana required", true); let be = utf16("banana required forbidden", false);
    let entries = [entry(b"src/a.rs", b"banana required"), entry(b"src/b.rs", &le),
        entry(b"src/c.rs", &be), entry(b"src/d.rs", b"banana"),
        SnapshotEntry { path: b"tests/unavailable.rs", observed_bytes: 50, data: SnapshotData::Unavailable("NOT_CAPTURED") }];
    for indexed in [false, true] { for limit in [0, 1, 4] {
        let report = expression(&entries, "ana required -forbidden path:src/", limit, 100_000, indexed);
        assert!(report.is_complete() && report.count_all_matches()); assert_eq!(report.state(), ExpressionState::Complete);
        assert_eq!(report.matches_seen(), 4); assert_eq!(report.matching_files(), 2);
        assert_eq!(report.hits().len(), limit); assert_eq!(report.hits_truncated(), limit < 4);
        assert_eq!(report.retained_matching_files(), if limit == 0 { 0 } else if limit == 1 { 1 } else { 2 });
        assert_eq!(report.stats().unavailable_files, 0); assert_eq!(report.stats().predicate_rejections, 2);
    } }
    let limited = expression(&[entry(b"a.rs", b"banana........forbidden")], "ana -forbidden", 0, 4, false);
    assert_eq!(limited.state(), ExpressionState::WorkLimit); assert!(!limited.is_complete());
    assert_eq!(limited.matches_seen(), 0); assert_eq!(limited.matching_files(), 0);
    assert!(limited.stats().scanned_bytes <= 4);
}

#[test]
fn counted_cancel_and_stale_steps_drop_private_work_without_a_publishable_report() {
    let budget = budget(); let bytes = vec![b'a'; 128 * 1024];
    let mut source = archive(&[entry(b"a.rs", &bytes)], true, &budget);
    let needle = StreamingNeedle::text(owner(), "a", &budget, id(3)).unwrap();
    let options = PagedQueryOptions { generation: generation(), first_file: FileId::new(owner(), 1).unwrap(),
        first_revision: SourceRevision::new(owner(), 1).unwrap(), max_matches: 0 };
    let baseline = budget.accounting().reserved().get();
    let mut query = PagedQuery::new_counting(&mut source, &needle, options, &budget, [id(4), id(5), id(6)]).unwrap();
    query.step(StreamReadStep { max_bytes: 16, max_calls: 1, max_hits: 2 }, generation(), &budget, || false).unwrap();
    assert_eq!(query.step(StreamReadStep::default(), generation(), &budget, || true), Err(PagedSearchError::Canceled));
    assert!(matches!(query.finish(), Err(PagedSearchError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    let mut query = PagedQuery::new_counting(&mut source, &needle, options, &budget, [id(4), id(5), id(6)]).unwrap();
    assert_eq!(query.step(StreamReadStep::default(), QueryGeneration::new(owner(), 2).unwrap(), &budget, || false),
        Err(PagedSearchError::StaleQuery));
    assert!(query.finish().is_err()); assert_eq!(budget.accounting().reserved().get(), baseline);
}

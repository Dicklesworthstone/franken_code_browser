#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! Later result windows must come from the same verified archive, query policy,
//! and original-byte coordinates as the first window. Candidate filters and
//! rejected members do not consume the offset into qualified matches.

use std::io::Cursor;
use fcb::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, SourceRevision};
use fcb::search::{IndexLimits, ResourceAllocationId, ResourceBudget, StreamingNeedle,
    StreamReadStep, SymbolNameMode};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::paged_snapshot::{IndexedNeedle, PagedCapture, PagedQuery, PagedQueryOptions,
    PagedQueryState, PagedReport, PagedSearchError, PagedSnapshot};
use fcb::search::snapshot_index::{SnapshotIndex, paged::PagedPostings};
use fcb::search::expression::{ExpressionOptions, ExpressionPlan, ExpressionQuery,
    ExpressionReport, ExpressionState};
use fcb::search::snapshot_structure::{StructureError, StructureMode, StructureOptions,
    StructureQuery, StructureReport, StructureState};

type Witness = (usize, u64, u64);

#[derive(Clone, Copy, Debug)]
enum Route { Direct, Paged }
const ROUTES: [Route; 2] = [Route::Direct, Route::Paged];

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(8864).unwrap() }
fn id(value: u64) -> ResourceAllocationId { ResourceAllocationId::new(value).unwrap() }
fn generation(value: u64) -> QueryGeneration { QueryGeneration::new(owner(), value).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn archive(entries: &[SnapshotEntry<'_>], complete: bool, budget: &ResourceBudget)
    -> PagedSnapshot<Cursor<Vec<u8>>> {
    let encoded = SnapshotBytes::encode(owner(), complete, "saved-result-windows-v1", entries,
        SnapshotLimits::default(), budget, id(1), || false).unwrap();
    PagedSnapshot::open(Cursor::new(encoded.bytes().to_vec()), owner(), SnapshotLimits::default(),
        budget, id(2), || false).unwrap()
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    format!("\u{feff}{text}").encode_utf16().flat_map(|unit|
        if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}
fn options(limit: usize, query: u64) -> PagedQueryOptions {
    PagedQueryOptions { generation: generation(query), first_file: FileId::new(owner(), 1).unwrap(),
        first_revision: SourceRevision::new(owner(), 1).unwrap(), max_matches: limit }
}
fn literal_rows(report: &PagedReport) -> Vec<Witness> {
    report.hits().iter().map(|hit| (hit.ordinal(), hit.original_range().start().get(),
        hit.original_range().end().get())).collect()
}
fn expression_rows(report: &ExpressionReport) -> Vec<Witness> {
    report.hits().iter().map(|hit| (hit.ordinal(), hit.original_range().start().get(),
        hit.original_range().end().get())).collect()
}
fn structure_rows(report: &StructureReport) -> Vec<Witness> {
    report.hits().iter().map(|hit| (hit.ordinal(), hit.original_range().start().get(),
        hit.original_range().end().get())).collect()
}

#[allow(clippy::too_many_arguments)]
fn literal(route: Route, entries: &[SnapshotEntry<'_>], complete: bool, needle: &str,
    case_sensitive: bool, skip: u64, limit: usize, counted: bool, grams: usize) -> PagedReport {
    let budget = budget(); let mut source = archive(entries, complete, &budget);
    let index = SnapshotIndex::build(&mut source, IndexLimits { max_total_grams: grams, ..Default::default() },
        &budget, [id(3), id(4), id(5), id(6)], || false).unwrap();
    let postings = index.invert(&budget, id(7), || false).unwrap();
    let artifact = postings.encode_paged(&budget, id(8), || false).unwrap();
    let mut paged = PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(),
        source.directory(), 1, &budget, id(9), || false).unwrap();
    let plain = StreamingNeedle::text_matching(owner(), needle, case_sensitive, &budget, id(10)).unwrap();
    let indexed = IndexedNeedle::text(owner(), needle, &budget, id(11)).unwrap();
    let mut query = match route {
        Route::Direct => PagedQuery::new_page(&mut source, &plain, options(limit, 1), skip,
            counted, &budget, [id(12), id(13), id(14)]),
        Route::Paged => {
            assert!(case_sensitive, "exact persisted negatives cannot filter lowercase matches");
            PagedQuery::new_paged_page(&mut source, &indexed, &mut paged, options(limit, 1),
                skip, counted, &budget, [id(12), id(13), id(14)])
        }
    }.unwrap();
    for _ in 0..20_000 {
        if query.state() != PagedQueryState::Pending { break; }
        query.step(StreamReadStep { max_bytes: 7, max_calls: 2, max_hits: 1 },
            generation(1), &budget, || false).unwrap();
        assert!(query.stats().last_step_scanned_bytes <= 7);
    }
    assert_ne!(query.state(), PagedQueryState::Pending);
    query.finish().unwrap()
}

#[test]
fn literal_windows_agree_across_members_encodings_overlaps_and_persisted_candidates() {
    let le = utf16("banana", true); let be = utf16("banana banana", false);
    let entries = [entry(b"a.rs", b"banana"), entry(b"b.rs", &le), entry(b"c.rs", &be),
        entry(b"d.rs", b"no match")];
    let oracle: [Witness; 8] = [(0, 1, 4), (0, 3, 6), (1, 4, 10), (1, 8, 14),
        (2, 4, 10), (2, 8, 14), (2, 18, 24), (2, 22, 28)];
    for route in ROUTES { for grams in [0, 100_000] { for counted in [false, true] {
        for skip in [0u64, 1, 2, 3, 7, 8, 9] { for limit in [1, 3, 8] {
            let report = literal(route, &entries, true, "ana", true, skip, limit, counted, grams);
            let start = (skip as usize).min(oracle.len());
            let end = (start + limit).min(oracle.len());
            assert_eq!(literal_rows(&report), oracle[start..end], "{route:?}, skip={skip}, limit={limit}");
            assert_eq!(report.skip_matches(), skip);
            assert_eq!(report.skipped_matches(), skip.min(8));
            assert_eq!(report.has_more_matches(), end < oracle.len());
            assert_eq!(report.next_match_offset(), (end < oracle.len()).then_some(end as u64));
            if counted || end == oracle.len() {
                assert_eq!(report.matches_seen(), 8);
                assert_eq!(report.matching_files(), 3);
                assert!(report.is_complete());
            } else {
                assert_eq!(report.matches_seen(), end as u64 + 1);
                assert!(!report.is_complete());
            }
            assert_eq!(report.count_all_matches(), counted);
            assert!(report.stats().peak_source_bytes <= be.len());
        } }
    } } }
}

#[test]
fn a_full_last_window_and_a_count_only_window_do_not_claim_later_results() {
    for route in ROUTES {
        let entries = [entry(b"a.rs", b"banana"), entry(b"b.rs", b"banana"), entry(b"z.rs", b"unrelated")];
        let last = literal(route, &entries, true, "ana", true, 2, 2, false, 100_000);
        assert_eq!(literal_rows(&last), [(1, 1, 4), (1, 3, 6)]);
        assert!(last.is_complete()); assert!(last.truncated());
        assert!(!last.has_more_matches()); assert_eq!(last.next_match_offset(), None);
        let counted = literal(route, &entries, true, "ana", true, 2, 0, true, 100_000);
        assert!(counted.hits().is_empty()); assert_eq!(counted.matches_seen(), 4);
        assert_eq!(counted.skipped_matches(), 2); assert!(counted.has_more_matches());
        assert_eq!(counted.next_match_offset(), None, "count-only output cannot advance past its offset");
        assert!(counted.is_complete());
        let beyond = literal(route, &entries, true, "ana", true, (1u64 << 53) + 7, 2, false, 100_000);
        assert_eq!(beyond.matches_seen(), 4); assert_eq!(beyond.skipped_matches(), 4);
        assert!(beyond.hits().is_empty() && beyond.is_complete());
        assert!(!beyond.has_more_matches()); assert_eq!(beyond.next_match_offset(), None);
    }
}

#[test]
fn lowercase_window_offsets_follow_qualified_original_spans() {
    let le = utf16("Kk K", true); let be = utf16("Kk K", false);
    let entries = [entry(b"a.txt", "Kk K".as_bytes()), entry(b"b.txt", &le), entry(b"c.txt", &be)];
    let report = literal(Route::Direct, &entries, true, "k", false, 2, 5, true, 0);
    assert_eq!(literal_rows(&report), [(0, 5, 6), (1, 2, 4), (1, 4, 6), (1, 8, 10), (2, 2, 4)]);
    assert_eq!(report.matches_seen(), 9); assert_eq!(report.skipped_matches(), 2);
    assert!(report.is_complete() && report.has_more_matches());
    assert_eq!(report.next_match_offset(), Some(7));
}

#[test]
fn unsupported_and_unavailable_members_never_consume_qualified_match_offsets() {
    let unavailable = SnapshotEntry { path: b"b.rs", observed_bytes: 40,
        data: SnapshotData::Unavailable("READ_DENIED") };
    for route in ROUTES {
        let report = literal(route, &[entry(b"a.rs", b"banana\xff"), unavailable,
            entry(b"c.rs", b"banana")], true, "ana", true, 1, 1, true, 100_000);
        assert_eq!(literal_rows(&report), [(2, 3, 6)]);
        assert_eq!(report.matches_seen(), 2); assert_eq!(report.matching_files(), 1);
        assert_eq!(report.skipped_matches(), 1); assert!(!report.has_more_matches());
        assert_eq!(report.stats().unsupported_files, 1); assert_eq!(report.unavailable_files(), 1);
        assert!(!report.is_complete());
        let open_membership = literal(route, &[entry(b"a.rs", b"banana")], false,
            "ana", true, 1, 1, true, 0);
        assert_eq!(open_membership.matches_seen(), 2); assert!(!open_membership.is_complete());
        assert!(!open_membership.has_more_matches());
    }
}

#[test]
fn skipped_literal_work_is_cancelable_and_cannot_publish_a_stale_generation() {
    let budget = budget(); let bytes = b"banana ".repeat(10_000);
    let mut source = archive(&[entry(b"a.rs", &bytes)], true, &budget);
    let needle = StreamingNeedle::text(owner(), "ana", &budget, id(3)).unwrap();
    let baseline = budget.accounting().reserved().get();
    let mut query = PagedQuery::new_page(&mut source, &needle, options(2, 7), 19_000,
        false, &budget, [id(4), id(5), id(6)]).unwrap();
    for _ in 0..3 {
        query.step(StreamReadStep { max_bytes: 7, max_calls: 1, max_hits: 1 },
            generation(7), &budget, || false).unwrap();
    }
    assert_eq!(query.state(), PagedQueryState::Pending);
    assert_eq!(query.step(StreamReadStep::default(), generation(7), &budget, || true),
        Err(PagedSearchError::Canceled));
    assert!(matches!(query.finish(), Err(PagedSearchError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    let mut query = PagedQuery::new_page(&mut source, &needle, options(2, 8), 19_000,
        false, &budget, [id(4), id(5), id(6)]).unwrap();
    assert_eq!(query.step(StreamReadStep::default(), generation(7), &budget, || false),
        Err(PagedSearchError::StaleQuery));
    assert!(query.finish().is_err()); assert_eq!(budget.accounting().reserved().get(), baseline);
}

#[test]
fn later_literal_witnesses_reopen_only_their_archive_and_query() {
    let budget = budget(); let mut source = archive(&[entry(b"a.rs", b"banana banana")], true, &budget);
    let needle = StreamingNeedle::text(owner(), "ana", &budget, id(3)).unwrap();
    let mut query = PagedQuery::new_page(&mut source, &needle, options(1, 17), 2,
        false, &budget, [id(4), id(5), id(6)]).unwrap();
    while query.state() == PagedQueryState::Pending {
        query.step(StreamReadStep::default(), generation(17), &budget, || false).unwrap();
    }
    let report = query.finish().unwrap(); let hit = report.hits()[0];
    assert_eq!(literal_rows(&report), [(0, 8, 11)]);
    assert!(PagedCapture::open_hit(&mut source, hit, generation(16), &budget,
        [id(7), id(8)], || false).is_err());
    let capture = PagedCapture::open_hit(&mut source, hit, generation(17), &budget,
        [id(7), id(8)], || false).unwrap();
    assert_eq!(capture.bytes(), b"banana banana");
    assert_eq!(capture.hit_bytes(hit).unwrap(), b"ana");
    assert_eq!(capture.source_digest(), hit.source_digest());
}

fn expression(entries: &[SnapshotEntry<'_>], needle: &str, skip: u64, limit: usize,
    counted: bool, work: u64, indexed: bool) -> ExpressionReport {
    let budget = budget(); let mut source = archive(entries, true, &budget);
    let index = SnapshotIndex::build(&mut source, IndexLimits::default(), &budget,
        [id(3), id(4), id(5), id(6)], || false).unwrap();
    let plan = ExpressionPlan::parse(owner(), needle, &budget, id(7), id(100)).unwrap();
    let options = ExpressionOptions { generation: generation(1), first_file: FileId::new(owner(), 1).unwrap(),
        first_revision: SourceRevision::new(owner(), 1).unwrap(), max_matches: limit, max_scan_bytes: work };
    let mut query = if indexed {
        ExpressionQuery::new_indexed_page(&mut source, &plan, &index, options, skip,
            counted, &budget, [id(8), id(9), id(10)])
    } else {
        ExpressionQuery::new_page(&mut source, &plan, options, skip,
            counted, &budget, [id(8), id(9), id(10)])
    }.unwrap();
    for _ in 0..20_000 {
        if query.state() != ExpressionState::Pending { break; }
        query.step(StreamReadStep { max_bytes: 7, max_calls: 2, max_hits: 1 },
            generation(1), &budget, || false).unwrap();
        assert!(query.stats().last_step_scanned_bytes <= 7);
    }
    assert_ne!(query.state(), ExpressionState::Pending); query.finish().unwrap()
}

#[test]
fn expression_windows_skip_only_after_all_predicates_and_metadata_filters_qualify() {
    let le = utf16("banana required", true);
    let entries = [entry(b"src/a.rs", b"banana required"), entry(b"src/b.rs", b"banana"),
        entry(b"src/c.rs", b"banana required forbidden"), entry(b"src/d.rs", &le),
        SnapshotEntry { path: b"tests/unavailable.rs", observed_bytes: 50,
            data: SnapshotData::Unavailable("NOT_CAPTURED") }];
    let oracle: [Witness; 4] = [(0, 1, 4), (0, 3, 6), (3, 4, 10), (3, 8, 14)];
    for indexed in [false, true] { for counted in [false, true] { for skip in [0u64, 1, 2, 3, 4, 9] {
        let report = expression(&entries, "ana required -forbidden path:src/", skip, 2, counted, 100_000, indexed);
        let start = (skip as usize).min(oracle.len()); let end = (start + 2).min(oracle.len());
        assert_eq!(expression_rows(&report), oracle[start..end]);
        assert_eq!(report.skip_matches(), skip); assert_eq!(report.skipped_matches(), skip.min(4));
        assert_eq!(report.has_more_matches(), end < oracle.len());
        assert_eq!(report.next_match_offset(), (end < oracle.len()).then_some(end as u64));
        assert_eq!(report.stats().unavailable_files, 0);
        if counted || end == oracle.len() {
            assert_eq!(report.matches_seen(), 4); assert_eq!(report.matching_files(), 2);
            assert_eq!(report.stats().predicate_rejections, 2); assert!(report.is_complete());
        } else { assert!(!report.is_complete()); }
    } } }
}

#[test]
fn expression_work_limit_before_predicate_proof_cannot_advance_the_match_offset() {
    let report = expression(&[entry(b"a.rs", b"banana..................forbidden")],
        "ana -forbidden", 10, 2, true, 4, false);
    assert_eq!(report.state(), ExpressionState::WorkLimit); assert!(!report.is_complete());
    assert_eq!(report.matches_seen(), 0); assert_eq!(report.skipped_matches(), 0);
    assert!(report.hits().is_empty()); assert!(!report.has_more_matches());
    assert_eq!(report.next_match_offset(), None); assert!(report.stats().scanned_bytes <= 4);
}

#[test]
fn expression_zero_row_windows_probe_after_the_offset_and_can_count_the_tail() {
    let entries = [entry(b"a.rs", b"banana required"), entry(b"b.rs", b"banana required")];
    for indexed in [false, true] {
        let probe = expression(&entries, "ana required", 2, 0, false, 100_000, indexed);
        assert!(probe.hits().is_empty()); assert_eq!(probe.skipped_matches(), 2);
        assert_eq!(probe.matches_seen(), 3); assert!(probe.has_more_matches());
        assert_eq!(probe.next_match_offset(), None, "an existence probe retains no advancing rows");
        assert!(!probe.is_complete());
        let counted = expression(&entries, "ana required", 2, 0, true, 100_000, indexed);
        assert!(counted.hits().is_empty()); assert_eq!(counted.skipped_matches(), 2);
        assert_eq!(counted.matches_seen(), 4); assert!(counted.has_more_matches());
        assert_eq!(counted.next_match_offset(), None, "a complete count still retains no advancing rows");
        assert!(counted.is_complete());
    }
}

#[test]
fn zero_row_saved_windows_never_offer_a_nonprogressing_continuation() {
    let entries = [entry(b"a.rs", b"banana required"), entry(b"b.rs", b"banana required")];
    for skip in [0, 2, 4, u64::MAX] {
        for route in ROUTES {
            let report = literal(route, &entries, true, "ana", true, skip, 0, true, 100_000);
            assert!(report.hits().is_empty()); assert!(report.is_complete());
            assert_eq!(report.matches_seen(), 4); assert_eq!(report.skipped_matches(), skip.min(4));
            assert_eq!(report.has_more_matches(), skip < 4);
            assert_eq!(report.next_match_offset(), None, "{route:?}, skip={skip}");
        }
        for indexed in [false, true] { for counted in [false, true] {
            let report = expression(&entries, "ana required", skip, 0, counted, 100_000, indexed);
            assert!(report.hits().is_empty());
            assert_eq!(report.skipped_matches(), skip.min(4));
            assert_eq!(report.has_more_matches(), skip < 4);
            assert_eq!(report.next_match_offset(), None, "indexed={indexed}, counted={counted}, skip={skip}");
            assert_eq!(report.is_complete(), counted || skip >= 4);
        } }
    }
}

fn structure(entries: &[SnapshotEntry<'_>], needle: &str, mode: StructureMode,
    name_mode: SymbolNameMode, skip: u64, limit: usize) -> StructureReport {
    let budget = budget(); let mut source = archive(entries, true, &budget);
    let options = StructureOptions { generation: generation(1), first_file: FileId::new(owner(), 1).unwrap(),
        first_revision: SourceRevision::new(owner(), 1).unwrap(), mode, name_mode,
        max_matches: limit, max_source_bytes: 64 * 1024 * 1024 };
    let mut query = StructureQuery::new_page(&mut source, needle, options, skip,
        &budget, [id(3), id(4), id(5), id(6), id(7)]).unwrap();
    for _ in 0..entries.len() + 2 {
        if query.state() != StructureState::Pending { break; }
        query.step(generation(1), &budget, || false).unwrap();
    }
    assert_ne!(query.state(), StructureState::Pending); query.finish().unwrap()
}

#[test]
fn identifier_windows_cross_the_old_per_member_retention_limit() {
    let text = "needle ".repeat(4_105);
    let entries = [entry(b"a.txt", text.as_bytes()), entry(b"b.txt", b"needle needle")];
    let middle = structure(&entries, "needle", StructureMode::Occurrences, SymbolNameMode::Exact, 4_096, 4);
    let expected: Vec<Witness> = (4_096..4_100).map(|n| (0, n * 7, n * 7 + 6)).collect();
    assert_eq!(structure_rows(&middle), expected);
    assert_eq!(middle.skipped_matches(), 4_096); assert!(middle.has_more_matches());
    assert_eq!(middle.next_match_offset(), Some(4_100)); assert!(!middle.is_complete());
    let end = structure(&entries, "needle", StructureMode::Occurrences, SymbolNameMode::Exact, 4_104, 3);
    assert_eq!(structure_rows(&end), [(0, 28_728, 28_734), (1, 0, 6), (1, 7, 13)]);
    assert_eq!(end.matches_seen(), 4_107); assert_eq!(end.skipped_matches(), 4_104);
    assert!(end.is_complete()); assert!(!end.has_more_matches()); assert_eq!(end.next_match_offset(), None);
    let beyond = structure(&entries, "needle", StructureMode::Occurrences, SymbolNameMode::Exact,
        (1u64 << 53) + 7, 1);
    assert!(beyond.hits().is_empty() && beyond.is_complete());
    assert_eq!(beyond.matches_seen(), 4_107); assert_eq!(beyond.skipped_matches(), 4_107);
    assert!(!beyond.has_more_matches()); assert_eq!(beyond.next_match_offset(), None);
}

#[test]
fn identifier_windows_preserve_utf16_boundaries_and_refuse_unsupported_members() {
    let le = utf16("needle needle2 _needle needle", true);
    let entries = [entry(b"a.txt", b"needle\xff"), entry(b"b.txt", &le), entry(b"c.txt", b"needle")];
    let report = structure(&entries, "needle", StructureMode::Occurrences, SymbolNameMode::Exact, 1, 2);
    assert_eq!(structure_rows(&report), [(1, 48, 60), (2, 0, 6)]);
    assert_eq!(report.matches_seen(), 3); assert_eq!(report.skipped_matches(), 1);
    assert_eq!(report.stats().unsupported_files, 1); assert!(!report.is_complete());
    assert!(!report.has_more_matches()); assert_eq!(report.next_match_offset(), None);
}

#[test]
fn declaration_windows_skip_matching_names_across_members_and_keep_generation_checks() {
    let entries = [entry(b"a.rs", b"fn skip() {}\nfn target_one() {}\n"),
        entry(b"b.rs", b"fn target_two() {}\nfn target_three() {}\n")];
    let report = structure(&entries, "target_", StructureMode::Symbols, SymbolNameMode::Prefix, 1, 2);
    assert_eq!(report.hits().iter().map(|hit| hit.name()).collect::<Vec<_>>(), ["target_two", "target_three"]);
    assert_eq!(report.matches_seen(), 3); assert_eq!(report.skipped_matches(), 1);
    assert!(report.is_complete()); assert!(!report.has_more_matches());
    let budget = budget(); let mut source = archive(&entries, true, &budget);
    assert!(matches!(report.hits()[0].open(&mut source, generation(2), &budget,
        [id(8), id(9)], || false), Err(StructureError::StaleQuery)));
    let capture = report.hits()[0].open(&mut source, generation(1), &budget,
        [id(8), id(9)], || false).unwrap();
    let (start, end) = report.hits()[0].original_range().as_usize_bounds().unwrap();
    assert_eq!(&capture.bytes()[start..end], b"target_two");
}

#[test]
#[cfg(feature = "markdown")]
fn heading_windows_keep_exact_source_evidence_after_skipped_matches() {
    let entries = [entry(b"a.md", b"# Target one\n\n# Other\n"),
        entry(b"b.md", b"# Target two\n\n# Target three\n")];
    let report = structure(&entries, "Target", StructureMode::Headings, SymbolNameMode::Prefix, 1, 2);
    assert_eq!(report.hits().iter().map(|hit| hit.name()).collect::<Vec<_>>(), ["Target two", "Target three"]);
    assert_eq!(report.matches_seen(), 3); assert_eq!(report.skipped_matches(), 1);
    assert!(report.is_complete()); assert!(!report.has_more_matches());
    assert!(report.hits().iter().all(|hit| hit.ordinal() == 1 && hit.heading_slug().is_some()
        && hit.evidence_range() == hit.original_range()));
}

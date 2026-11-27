#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::io::{Cursor, Read, Seek};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{IndexLimits, QueryGeneration, ResourceAllocationId, ResourceBudget,
    StreamingNeedle, StreamReadStep};
use fcb::search::expression::{ExpressionError, ExpressionOptions, ExpressionPlan,
    ExpressionQuery, ExpressionReport, ExpressionState};
use fcb::search::paged_snapshot::{IndexedNeedle, PagedCapture, PagedQuery, PagedQueryOptions,
    PagedQueryState, PagedReport, PagedSearchError, PagedSnapshot};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::snapshot_index::SnapshotIndex;
use fcb::search::snapshot_index::paged::PagedPostings;
use fcb::search::snapshot_scope::SnapshotScope;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(8612).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn missing(path: &[u8]) -> SnapshotEntry<'_> {
    SnapshotEntry { path, observed_bytes: 20, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }
}
fn archive(entries: &[SnapshotEntry<'_>], complete: bool, b: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    let encoded = SnapshotBytes::encode(owner(), complete, "scoped-search-tests", entries,
        SnapshotLimits::default(), b, id(1), || false).unwrap();
    PagedSnapshot::open(Cursor::new(encoded.bytes().to_vec()), owner(), SnapshotLimits::default(),
        b, id(2), || false).unwrap()
}
fn options(limit: usize) -> PagedQueryOptions {
    PagedQueryOptions { generation: generation(1), first_file: FileId::new(owner(), 100).unwrap(),
        first_revision: SourceRevision::new(owner(), 200).unwrap(), max_matches: limit }
}
fn expression_options(limit: usize, work: u64) -> ExpressionOptions {
    ExpressionOptions { generation: generation(1), first_file: FileId::new(owner(), 100).unwrap(),
        first_revision: SourceRevision::new(owner(), 200).unwrap(), max_matches: limit, max_scan_bytes: work }
}
fn finish<R: Read + Seek>(mut query: PagedQuery<'_, '_, R>, b: &ResourceBudget) -> PagedReport {
    for _ in 0..100_000 {
        if query.state() != PagedQueryState::Pending { return query.finish().unwrap(); }
        query.step(StreamReadStep { max_bytes: 7, max_calls: 4, max_hits: 2 }, generation(1), b, || false).unwrap();
        assert!(query.stats().last_step_loaded_bytes <= 7);
        assert!(query.stats().last_step_scanned_bytes <= 7);
    }
    panic!("scoped query did not terminate");
}
fn run(source: &mut PagedSnapshot<Cursor<Vec<u8>>>, scope: SnapshotScope<'_>,
    skip: u64, limit: usize, b: &ResourceBudget) -> PagedReport {
    let needle = StreamingNeedle::text(owner(), "needle", b, id(10)).unwrap();
    finish(PagedQuery::new_scoped_page(source, &needle, options(limit), scope, skip, true,
        b, [id(11), id(12), id(13)]).unwrap(), b)
}
fn coordinates(report: &PagedReport) -> Vec<(usize, u64, u64)> {
    report.hits().iter().map(|hit| (hit.ordinal(),
        hit.original_range().start().get(), hit.original_range().end().get())).collect()
}
fn utf16(text: &str, big_endian: bool) -> Vec<u8> {
    let mut bytes = if big_endian { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for unit in text.encode_utf16() {
        let encoded = if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() };
        bytes.extend_from_slice(&encoded);
    }
    bytes
}
fn expression(source: &mut PagedSnapshot<Cursor<Vec<u8>>>, index: Option<&SnapshotIndex>,
    scope: SnapshotScope<'_>, query: &str, opts: ExpressionOptions, skip: u64,
    b: &ResourceBudget) -> ExpressionReport {
    let plan = ExpressionPlan::parse(owner(), query, b, id(10), id(1000)).unwrap();
    let mut search = if let Some(index) = index {
        ExpressionQuery::new_indexed_scoped_page(source, &plan, index, opts, scope, skip, true,
            b, [id(11), id(12), id(13)]).unwrap()
    } else {
        ExpressionQuery::new_scoped_page(source, &plan, opts, scope, skip, true,
            b, [id(11), id(12), id(13)]).unwrap()
    };
    for _ in 0..100_000 {
        if search.state() != ExpressionState::Pending { return search.finish().unwrap(); }
        search.step(StreamReadStep { max_bytes: 7, max_calls: 4, max_hits: 2 }, opts.generation, b, || false).unwrap();
        assert!(search.stats().last_step_scanned_bytes <= 7);
    }
    panic!("scoped expression did not terminate");
}

#[test]
fn directory_boundaries_raw_paths_and_utf16_windows_keep_exact_navigation() {
    let b = budget(); let le = utf16("needle", false); let be = utf16("needle", true);
    let entries = [entry(b"outside.rs", b"needle"), entry(b"src/a.rs", b"needle needle"),
        entry(b"src/b.rs", &le), entry(b"src/c.rs", &be), entry(b"src/\xff.rs", b"needle"),
        entry(b"src2/a.rs", b"needle"), missing(b"z-unavailable")];
    let mut source = archive(&entries, true, &b);
    let report = run(&mut source, SnapshotScope::Directory(b"src"), 2, 2, &b);
    assert_eq!(coordinates(&report), [(2, 2, 14), (3, 2, 14)]);
    assert_eq!(report.selected_members(), 4); assert_eq!(report.captured_members(), 4);
    assert_eq!(report.unavailable_files(), 0); assert!(report.membership_complete()); assert!(report.is_complete());
    assert_eq!(report.matches_seen(), 5); assert_eq!(report.matching_files(), 4);
    assert_eq!(report.stats().members_visited, 4); assert_eq!(report.stats().loaded_bytes, 47);
    assert_eq!(report.skipped_matches(), 2); assert_eq!(report.next_match_offset(), Some(4));
    report.validate_delivery(source.directory().digest(), generation(1)).unwrap();
    assert_eq!(report.validate_delivery(source.directory().digest(), generation(2)), Err(PagedSearchError::StaleQuery));
    for &hit in report.hits() {
        assert_eq!(hit.file().get(), 100 + hit.ordinal() as u64);
        let opened = PagedCapture::open_hit(&mut source, hit, generation(1), &b,
            [id(20), id(21)], || false).unwrap();
        let expected = if hit.ordinal() == 2 { &le[2..] } else { &be[2..] };
        assert_eq!(opened.hit_bytes(hit).unwrap(), expected);
    }
    drop(report);
    let report = run(&mut source, SnapshotScope::Directory(b"src"), 4, 2, &b);
    assert_eq!(coordinates(&report), [(4, 0, 6)]);
    assert!(!report.has_more_matches()); assert_eq!(report.next_match_offset(), None);
    drop(report);
    let report = run(&mut source, SnapshotScope::Member(b"src/\xff.rs"), 0, 10, &b);
    assert_eq!(coordinates(&report), [(4, 0, 6)]);
    assert_eq!(report.stats().members_visited, 1); assert!(report.is_complete());
}

#[test]
fn exact_member_closes_partial_membership_but_missing_or_unavailable_never_proves_absence() {
    for complete in [false, true] {
        let b = budget();
        let mut source = archive(&[missing(b"outside/missing"), entry(b"src/a.rs", b"needle"),
            missing(b"src/b.rs"), entry(b"src2/a.rs", b"needle")], complete, &b);
        for (scope, known, captured, unavailable, closed, fully_searched) in [
            (SnapshotScope::Archive, 4, 2, 2, complete, false),
            (SnapshotScope::Directory(b"src"), 2, 1, 1, complete, false),
            (SnapshotScope::Member(b"src/a.rs"), 1, 1, 0, true, true),
            (SnapshotScope::Member(b"src/b.rs"), 1, 0, 1, true, false),
            (SnapshotScope::Member(b"src/missing.rs"), 0, 0, 0, false, false),
            (SnapshotScope::Directory(b"missing"), 0, 0, 0, complete, complete),
        ] {
            let report = run(&mut source, scope, 0, 10, &b);
            assert_eq!(report.selected_members(), known, "{scope:?}");
            assert_eq!(report.captured_members(), captured, "{scope:?}");
            assert_eq!(report.unavailable_files(), unavailable, "{scope:?}");
            assert_eq!(report.membership_complete(), closed, "{scope:?}");
            assert_eq!(report.is_complete(), fully_searched, "{scope:?}");
            assert_eq!(report.stats().members_visited, known, "{scope:?}");
        }
    }
}

#[test]
fn invalid_scopes_canceled_and_stale_queries_admit_no_source_or_publishable_report() {
    let b = budget(); let mut source = archive(&[entry(b"src/a.rs", b"needle")], true, &b);
    let needle = StreamingNeedle::text(owner(), "needle", &b, id(10)).unwrap();
    let retained = b.accounting().reserved().get();
    for path in [b"".as_slice(), b"/src", b"src/", b"src//a", b"src/../a", b"./src", b"src/\0"] {
        for scope in [SnapshotScope::Directory(path), SnapshotScope::Member(path)] {
            assert!(matches!(PagedQuery::new_scoped_page(&mut source, &needle, options(10), scope, 0, true,
                &b, [id(11), id(12), id(13)]), Err(PagedSearchError::InvalidScope)));
            assert_eq!(b.accounting().reserved().get(), retained);
        }
    }
    let too_long = vec![b'a'; 16_385];
    assert_eq!(SnapshotScope::Directory(&too_long).validate(), Err(PagedSearchError::InvalidScope));
    for stale in [false, true] {
        let mut query = PagedQuery::new_scoped_page(&mut source, &needle, options(10),
            SnapshotScope::Directory(b"src"), 0, true, &b, [id(11), id(12), id(13)]).unwrap();
        let result = query.step(StreamReadStep::default(), generation(if stale { 2 } else { 1 }), &b, || !stale);
        assert_eq!(result, Err(if stale { PagedSearchError::StaleQuery } else { PagedSearchError::Canceled }));
        assert!(matches!(query.finish(), Err(PagedSearchError::Canceled)));
    }
    assert_eq!(source.load_stats().loaded_members, 0);
    assert_eq!(source.load_stats().bytes_read, 0);
    assert_eq!(b.accounting().reserved().get(), retained);
}

#[test]
fn direct_segment_global_and_paged_indices_share_selected_counts_and_windows() {
    for grams in [0, 100_000] {
        let b = budget(); let encoded16 = utf16("needle", false);
        let mut source = archive(&[entry(b"a-outside", b"needle"), entry(b"src/a", b"needle needle"),
            entry(b"src/b", b"unrelated"), entry(b"src/c", &encoded16),
            entry(b"src/d", b"bad\xffneedle"), missing(b"src/z"),
            entry(b"src2/a", b"needle"), entry(b"z-outside", b"nothing")], true, &b);
        let index = SnapshotIndex::build(&mut source, IndexLimits { max_total_grams: grams, ..Default::default() },
            &b, [id(3), id(4), id(5), id(6)], || false).unwrap();
        let postings = index.invert(&b, id(7), || false).unwrap();
        let encoded = postings.encode_paged(&b, id(8), || false).unwrap();
        let mut paged = PagedPostings::open_pinned(Cursor::new(encoded.bytes()), encoded.digest(),
            source.directory(), 1, &b, id(9), || false).unwrap();
        for text in [false, true] {
            for scope in [SnapshotScope::Directory(b"src"), SnapshotScope::Member(b"src/a"),
                SnapshotScope::Member(b"src/missing")] {
                let plain = if text { StreamingNeedle::text(owner(), "needle", &b, id(10)).unwrap() }
                    else { StreamingNeedle::raw(owner(), b"needle", &b, id(10)).unwrap() };
                let report = finish(PagedQuery::new_scoped_page(&mut source, &plain, options(2),
                    scope, 1, true, &b, [id(11), id(12), id(13)]).unwrap(), &b);
                let wanted = coordinates(&report);
                let coverage = (report.selected_members(), report.captured_members(), report.unavailable_files(),
                    report.membership_complete(), report.is_complete(), report.matches_seen(), report.has_more_matches(),
                    report.next_match_offset(), report.stats().unsupported_files);
                drop(report); drop(plain);
                let needle = if text { IndexedNeedle::text(owner(), "needle", &b, id(10)).unwrap() }
                    else { IndexedNeedle::raw(owner(), b"needle", &b, id(10)).unwrap() };
                for route in 0..3 {
                    let query = match route {
                        0 => PagedQuery::new_indexed_scoped_page(&mut source, &needle, &index, options(2),
                            scope, 1, true, &b, [id(11), id(12), id(13)]),
                        1 => PagedQuery::new_postings_scoped_page(&mut source, &needle, &postings, options(2),
                            scope, 1, true, &b, [id(11), id(12), id(13)]),
                        _ => PagedQuery::new_paged_scoped_page(&mut source, &needle, &mut paged, options(2),
                            scope, 1, true, &b, [id(11), id(12), id(13)]),
                    }.unwrap();
                    let actual = finish(query, &b);
                    assert_eq!(coordinates(&actual), wanted, "route={route} text={text} grams={grams} {scope:?}");
                    assert_eq!((actual.selected_members(), actual.captured_members(), actual.unavailable_files(),
                        actual.membership_complete(), actual.is_complete(), actual.matches_seen(), actual.has_more_matches(),
                        actual.next_match_offset(), actual.stats().unsupported_files), coverage);
                    let stats = actual.stats();
                    assert!(stats.members_visited <= actual.selected_members());
                    assert_eq!(stats.index_candidates + stats.index_fallback_files + stats.index_eliminated_files,
                        actual.captured_members(), "negative proofs must use only selected captured members");
                }
            }
        }
    }
}

#[test]
fn expression_scope_intersects_metadata_and_work_budget_before_source_admission() {
    let b = budget(); let accepted = b"needle required needle"; let rejected = b"needle required forbidden";
    let negative = b"unrelated source";
    let mut source = archive(&[entry(b"outside/a.rs", b"needle required needle needle"),
        entry(b"src/a.rs", accepted), entry(b"src/b.py", b"needle required"),
        entry(b"src/c.rs", rejected), entry(b"src/d.rs", negative), missing(b"src/missing.py"),
        entry(b"src2/a.rs", b"needle required needle needle")], true, &b);
    let index = SnapshotIndex::build(&mut source, IndexLimits::default(), &b,
        [id(3), id(4), id(5), id(6)], || false).unwrap();
    let work = (accepted.len() + rejected.len() + negative.len()) as u64;
    for indexed in [false, true] {
        let report = expression(&mut source, indexed.then_some(&index), SnapshotScope::Directory(b"src"),
            "needle required -forbidden lang:rust", expression_options(1, work), 1, &b);
        assert!(report.is_complete()); assert_eq!(report.state(), ExpressionState::Complete);
        assert_eq!(report.selected_members(), 5); assert_eq!(report.captured_members(), 4);
        assert_eq!(report.stats().metadata_examined, 5); assert_eq!(report.stats().scope_files, 3);
        assert_eq!(report.stats().metadata_excluded, 2); assert_eq!(report.stats().unavailable_files, 0);
        assert_eq!(report.matches_seen(), 2); assert_eq!(report.hits().len(), 1);
        assert_eq!(report.hits()[0].ordinal(), 1);
        assert_eq!(report.hits()[0].original_range().as_usize_bounds().unwrap(), (16, 22));
        assert!(!report.has_more_matches()); assert_eq!(report.next_match_offset(), None);
        let capture = report.hits()[0].open(&mut source, generation(1), &b, [id(20), id(21)], || false).unwrap();
        assert_eq!(&capture.bytes()[16..22], b"needle");
        if indexed {
            assert_eq!(report.stats().index_eliminated, 1);
            assert_eq!(report.stats().scanned_bytes, (accepted.len() + rejected.len()) as u64);
        } else { assert_eq!(report.stats().scanned_bytes, work); }
    }
}

#[test]
fn expression_singleton_partial_archives_and_cancellation_keep_membership_truth() {
    let b = budget(); let bytes = b"needle required";
    let mut source = archive(&[missing(b"a-unavailable"), entry(b"src/a.rs", bytes),
        entry(b"src2/a.rs", b"needle required needle required")], false, &b);
    let report = expression(&mut source, None, SnapshotScope::Member(b"src/a.rs"), "needle required",
        expression_options(2, bytes.len() as u64), 0, &b);
    assert!(report.membership_complete()); assert!(report.is_complete());
    assert_eq!(report.selected_members(), 1); assert_eq!(report.stats().metadata_examined, 1);
    assert_eq!(report.stats().scanned_bytes, bytes.len() as u64);
    assert_eq!(report.matches_seen(), 1);
    assert_eq!(report.validate_delivery(source.directory().digest(), generation(2)), Err(ExpressionError::StaleQuery));
    assert!(matches!(report.hits()[0].open(&mut source, generation(2), &b, [id(20), id(21)], || false),
        Err(ExpressionError::StaleQuery)));
    drop(report);
    let report = expression(&mut source, None, SnapshotScope::Member(b"src/missing.rs"), "needle",
        expression_options(2, 0), 0, &b);
    assert!(!report.membership_complete()); assert!(!report.is_complete()); assert_eq!(report.selected_members(), 0);
    assert_eq!(report.stats().scanned_bytes, 0); drop(report);
    let plan = ExpressionPlan::parse(owner(), "needle required", &b, id(10), id(1000)).unwrap();
    let before = source.load_stats();
    for stale in [false, true] {
        let mut query = ExpressionQuery::new_scoped_page(&mut source, &plan, expression_options(2, 100),
            SnapshotScope::Directory(b"src"), 0, true, &b, [id(11), id(12), id(13)]).unwrap();
        assert_eq!(query.step(StreamReadStep::default(), generation(if stale { 2 } else { 1 }), &b, || !stale),
            Err(if stale { ExpressionError::StaleQuery } else { ExpressionError::Canceled }));
        assert!(matches!(query.finish(), Err(ExpressionError::Canceled)));
    }
    assert_eq!(source.load_stats().bytes_read, before.bytes_read);
}

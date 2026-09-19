#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! One query-wide limit across direct, forward-index, resident-posting and
//! demand-paged-posting routes. All routes use real archives and exact scanners.
use std::io::Cursor;
use fcb::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, SourceRevision};
use fcb::search::{IndexLimits, ResourceAllocationId, ResourceBudget, StreamingNeedle, StreamReadStep};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::paged_snapshot::{IndexedNeedle, PagedQuery, PagedQueryOptions, PagedQueryState, PagedReport, PagedSnapshot};
use fcb::search::snapshot_index::{SnapshotIndex, paged::PagedPostings};

#[derive(Clone, Copy, Debug)]
enum Route { Direct, Forward, Postings, Paged }
const ROUTES: [Route; 4] = [Route::Direct, Route::Forward, Route::Postings, Route::Paged];
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn run(route: Route, entries: &[SnapshotEntry<'_>], needle: &str, limit: usize) -> PagedReport {
    let owner = ArenaOwnerId::new(8801).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let limits = SnapshotLimits::default();
    let bytes = SnapshotBytes::encode(owner, true, "test", entries, limits, &budget, allocation(1), || false).unwrap();
    let mut archive = PagedSnapshot::open(Cursor::new(bytes.bytes()), owner, limits, &budget, allocation(2), || false).unwrap();
    // Force uncovered members to test the shared fallback, including negatives
    // after the cap. An index must not hide this bug by eliminating every suffix.
    let index = SnapshotIndex::build(&mut archive, IndexLimits { max_total_grams: 0, ..Default::default() },
        &budget, [allocation(3), allocation(4), allocation(5), allocation(6)], || false).unwrap();
    let postings = index.invert(&budget, allocation(7), || false).unwrap();
    let artifact = postings.encode_paged(&budget, allocation(8), || false).unwrap();
    let mut paged = PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(),
        archive.directory(), 4, &budget, allocation(9), || false).unwrap();
    let plain = StreamingNeedle::text(owner, needle, &budget, allocation(10)).unwrap();
    let indexed = IndexedNeedle::text(owner, needle, &budget, allocation(11)).unwrap();
    let generation = QueryGeneration::new(owner, 1).unwrap();
    let opts = PagedQueryOptions { generation, first_file: FileId::new(owner, 1).unwrap(),
        first_revision: SourceRevision::new(owner, 1).unwrap(), max_matches: limit };
    let ids = [allocation(12), allocation(13), allocation(14)];
    let mut query = match route {
        Route::Direct => PagedQuery::new(&mut archive, &plain, opts, &budget, ids),
        Route::Forward => PagedQuery::new_indexed(&mut archive, &indexed, &index, opts, &budget, ids),
        Route::Postings => PagedQuery::new_postings(&mut archive, &indexed, &postings, opts, &budget, ids),
        Route::Paged => PagedQuery::new_paged(&mut archive, &indexed, &mut paged, opts, &budget, ids),
    }.unwrap();
    let mut steps = 0;
    while query.state() == PagedQueryState::Pending {
        steps += 1; assert!(steps < 1000, "{route:?} failed to make progress");
        query.step(StreamReadStep::default(), generation, &budget, || false).unwrap();
    }
    query.finish().unwrap()
}
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}

#[test]
fn filling_the_table_then_examining_empty_and_nonmatching_files_is_complete() {
    let entries = [entry(b"a", b"needle"), entry(b"b", b""), entry(b"c", b"unrelated")];
    for route in ROUTES {
        let report = run(route, &entries, "needle", 1);
        assert!(report.is_complete(), "{route:?}"); assert!(!report.truncated());
        assert_eq!(report.hits().len(), 1); assert_eq!(report.matches_seen(), 1);
        assert_eq!(report.hits()[0].ordinal(), 0);
        // A fully indexed empty member is a sound negative; uncovered
        // nonempty suffixes still require verification after the cap.
        assert_eq!(report.stats().files_searched, if matches!(route, Route::Direct) { 3 } else { 2 });
    }
}

#[test]
fn a_later_extra_occurrence_proves_truncation_but_is_not_appended_or_overcounted() {
    let entries = [entry(b"a", b"needle"), entry(b"b", b"none"),
        entry(b"c", b"needle needle needle"), entry(b"d", b"needle")];
    for route in ROUTES {
        let report = run(route, &entries, "needle", 1);
        assert!(report.truncated(), "{route:?}"); assert!(!report.is_complete());
        assert_eq!(report.hits().len(), 1); assert_eq!(report.matches_seen(), 2);
        assert_eq!(report.hits()[0].ordinal(), 0);
    }
}

#[test]
fn utf16_overlaps_keep_original_ranges_and_a_negative_suffix_does_not_truncate() {
    let mut first = vec![0xff, 0xfe];
    for word in "banana".encode_utf16() { first.extend_from_slice(&word.to_le_bytes()); }
    let mut last = vec![0xff, 0xfe];
    for word in "unrelated".encode_utf16() { last.extend_from_slice(&word.to_le_bytes()); }
    let entries = [entry(b"a", &first), entry(b"b", &last)];
    for route in ROUTES {
        let report = run(route, &entries, "ana", 2);
        assert!(report.is_complete(), "{route:?}"); assert_eq!(report.matches_seen(), 2);
        let ranges: Vec<_> = report.hits().iter().map(|hit|
            (hit.original_range().start().get(), hit.original_range().end().get())).collect();
        assert_eq!(ranges, [(4, 10), (8, 14)]);
    }
}

#[test]
fn unavailable_suffix_is_incomplete_membership_not_a_truncated_result_buffer() {
    let entries = [entry(b"a", b"needle"), SnapshotEntry { path: b"b", observed_bytes: 9,
        data: SnapshotData::Unavailable("NOT_CAPTURED") }, entry(b"c", b"none")];
    for route in ROUTES {
        let report = run(route, &entries, "needle", 1);
        assert!(!report.is_complete(), "{route:?}"); assert!(!report.truncated());
        assert_eq!(report.unavailable_files(), 1); assert_eq!(report.matches_seen(), 1);
        assert_eq!(report.hits().len(), 1);
    }
}

#[test]
fn explicit_zero_limit_keeps_its_existing_no_scan_contract() {
    for route in ROUTES {
        let report = run(route, &[entry(b"a", b"needle")], "needle", 0);
        assert!(report.truncated(), "{route:?}"); assert!(report.hits().is_empty());
        assert_eq!(report.stats().scanned_bytes, 0); assert_eq!(report.matches_seen(), 0);
        let empty = run(route, &[], "needle", 0);
        assert!(empty.is_complete()); assert!(!empty.truncated());
    }
}

#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::io::Cursor;
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{IndexLimits, QueryGeneration, ResourceAllocationId, ResourceBudget, StreamReadStep, StreamingNeedle};
use fcb::search::snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData, SnapshotLimits};
use fcb::search::snapshot_index::{SnapshotIndex, SnapshotIndexError, IndexDecision};
use fcb::search::paged_snapshot::{PagedSnapshot, PagedQuery, PagedQueryOptions, PagedQueryState,
    PagedSearchError, PagedCapture, IndexedNeedle, PagedReport};
use fcb::search::snapshot::Sha256Digest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(861).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn options(limit: usize) -> PagedQueryOptions {
    PagedQueryOptions { generation: QueryGeneration::new(owner(), 3).unwrap(), first_file: FileId::new(owner(), 100).unwrap(),
        first_revision: SourceRevision::new(owner(), 200).unwrap(), max_matches: limit }
}
fn archive(entries: &[SnapshotEntry<'_>], complete: bool, budget: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    let bytes = SnapshotBytes::encode(owner(), complete, "test-v1", entries, SnapshotLimits::default(), budget, allocation(1), || false).unwrap();
    PagedSnapshot::open(Cursor::new(bytes.bytes().to_vec()), owner(), SnapshotLimits::default(), budget, allocation(2), || false).unwrap()
}
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn build(archive: &mut PagedSnapshot<Cursor<Vec<u8>>>, limits: IndexLimits, budget: &ResourceBudget) -> SnapshotIndex {
    SnapshotIndex::build(archive, limits, budget, [allocation(3), allocation(4), allocation(5), allocation(6)], || false).unwrap()
}
fn finish(query: &mut PagedQuery<'_, '_, Cursor<Vec<u8>>>, budget: &ResourceBudget) {
    for _ in 0..100_000 {
        if query.state() != PagedQueryState::Pending { return; }
        query.step(StreamReadStep { max_bytes: 7, max_calls: 4, max_hits: 2 }, query.generation(), budget, || false).unwrap();
        assert!(query.stats().last_step_scanned_bytes <= 7);
    }
    panic!("query did not terminate");
}
fn run(archive: &mut PagedSnapshot<Cursor<Vec<u8>>>, index: Option<&SnapshotIndex>, bytes: &[u8], text: bool,
    limit: usize, budget: &ResourceBudget) -> PagedReport {
    let opts = options(limit);
    if let Some(index) = index {
        let needle = if text { IndexedNeedle::text(owner(), std::str::from_utf8(bytes).unwrap(), budget, allocation(10)).unwrap() }
            else { IndexedNeedle::raw(owner(), bytes, budget, allocation(10)).unwrap() };
        let mut query = PagedQuery::new_indexed(archive, &needle, index, opts, budget, [allocation(11), allocation(12), allocation(13)]).unwrap();
        finish(&mut query, budget); query.finish().unwrap()
    } else {
        let needle = if text { StreamingNeedle::text(owner(), std::str::from_utf8(bytes).unwrap(), budget, allocation(10)).unwrap() }
            else { StreamingNeedle::raw(owner(), bytes, budget, allocation(10)).unwrap() };
        let mut query = PagedQuery::new(archive, &needle, opts, budget, [allocation(11), allocation(12), allocation(13)]).unwrap();
        finish(&mut query, budget); query.finish().unwrap()
    }
}
fn coordinates(report: &PagedReport) -> Vec<(usize, u64, u64)> {
    report.hits().iter().map(|hit| (hit.ordinal(), hit.original_range().start().get(), hit.original_range().end().get())).collect()
}
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    bytes
}

#[test]
fn reopen_uses_persisted_grams_and_loads_only_exact_candidates() {
    let budget = budget();
    let entries = [entry(b"a", b"banana"), entry(b"b", b"unrelated source"), entry(b"c", b"yet another source")];
    let mut archive = archive(&entries, true, &budget);
    let built = build(&mut archive, IndexLimits::default(), &budget);
    let encoded = built.encode(&budget, allocation(7), || false).unwrap();
    let pin = encoded.digest(); drop(built);
    let index = SnapshotIndex::decode_pinned(encoded.bytes(), pin, archive.directory(), &budget, allocation(3), || false).unwrap();
    assert_eq!(index.stats().build_source_bytes, 0, "reopen must not rebuild source grams");
    let before = archive.load_stats();
    let report = run(&mut archive, Some(&index), b"ana", true, 10, &budget);
    assert!(report.is_complete()); assert_eq!(coordinates(&report), [(0, 1, 4), (0, 3, 6)]);
    assert_eq!(report.stats().index_eliminated_files, 2);
    assert_eq!(report.stats().index_candidates, 1);
    assert_eq!(archive.load_stats().loaded_members - before.loaded_members, 1);
    assert_eq!(archive.load_stats().bytes_read - before.bytes_read, 6);
    let hit = report.hits()[0]; drop(report);
    let capture = PagedCapture::open_hit(&mut archive, hit, options(10).generation, &budget, [allocation(20), allocation(21)], || false).unwrap();
    assert_eq!(capture.hit_bytes(hit).unwrap(), b"ana");
}

#[test]
fn persisted_queries_match_direct_scans_for_all_small_substrings_and_limits() {
    let budget = budget();
    let text = b"banana bandana";
    let encoded16 = utf16("banana banana");
    let entries = [entry(b"a", text), entry(b"b", &encoded16), entry(b"c", b"\xff banana"), entry(b"d", b"")];
    let mut archive = archive(&entries, true, &budget);
    let index = build(&mut archive, IndexLimits::default(), &budget);
    for start in 0..text.len() {
        for end in start + 1..=(start + 5).min(text.len()) {
            for raw in [false, true] {
                for limit in [0, 1, 3, 32] {
                    let expected = run(&mut archive, None, &text[start..end], !raw, limit, &budget);
                    let expected_ranges = coordinates(&expected);
                    let expected_meta = (expected.is_complete(), expected.truncated(), expected.matches_seen(), expected.stats().unsupported_files);
                    drop(expected);
                    let actual = run(&mut archive, Some(&index), &text[start..end], !raw, limit, &budget);
                    assert_eq!(coordinates(&actual), expected_ranges, "start={start} end={end} raw={raw} limit={limit}");
                    assert_eq!((actual.is_complete(), actual.truncated(), actual.matches_seen(), actual.stats().unsupported_files), expected_meta);
                }
            }
        }
    }
}

#[test]
fn exact_limit_lookahead_crosses_eliminated_files_without_inventing_truncation() {
    let budget = budget();
    let mut archive = archive(&[entry(b"a", b"needle"), entry(b"b", b"different"), entry(b"c", b"")], true, &budget);
    let index = build(&mut archive, IndexLimits::default(), &budget);
    let report = run(&mut archive, Some(&index), b"needle", true, 1, &budget);
    assert!(report.is_complete()); assert!(!report.truncated()); assert_eq!(report.matches_seen(), 1);
    assert_eq!(report.stats().members_visited, 3);
}

#[test]
fn utf16_and_malformed_text_always_reach_the_real_decoder() {
    let budget = budget(); let utf16 = utf16("needle");
    let mut archive = archive(&[entry(b"a", &utf16), entry(b"b", b"bad\xff"), entry(b"c", b"nothing")], true, &budget);
    let index = build(&mut archive, IndexLimits::default(), &budget);
    assert_eq!(index.text_decision(0, Some("needle")), IndexDecision::Fallback);
    assert_eq!(index.text_decision(1, Some("needle")), IndexDecision::Fallback);
    let report = run(&mut archive, Some(&index), b"needle", true, 100, &budget);
    assert_eq!(coordinates(&report), [(0, 2, 14)]);
    assert_eq!(report.stats().unsupported_files, 1); assert!(!report.is_complete());
    assert_eq!(report.stats().index_fallback_files, 2);
}

#[test]
fn every_index_quota_keeps_uncovered_files_in_exact_search() {
    for limits in [IndexLimits { max_source_bytes_per_file: 0, ..IndexLimits::default() },
        IndexLimits { max_source_bytes_total: 0, ..IndexLimits::default() },
        IndexLimits { max_total_grams: 0, ..IndexLimits::default() },
        IndexLimits { max_grams_per_file: 0, ..IndexLimits::default() },
        IndexLimits { max_scratch_bytes: 0, ..IndexLimits::default() }] {
        let budget = budget(); let mut archive = archive(&[entry(b"a", b"banana")], true, &budget);
        let index = build(&mut archive, limits, &budget);
        assert_eq!(index.stats().uncovered_files, 1);
        let encoded = index.encode(&budget, allocation(7), || false).unwrap(); drop(index);
        let index = SnapshotIndex::decode_pinned(encoded.bytes(), encoded.digest(), archive.directory(), &budget, allocation(3), || false).unwrap();
        let report = run(&mut archive, Some(&index), b"ana", true, 100, &budget);
        assert!(report.is_complete()); assert_eq!(report.hits().len(), 2);
        assert_eq!(report.stats().index_fallback_files, 1);
    }
}

#[test]
fn unavailable_and_discovering_membership_survive_index_roundtrip() {
    for complete in [false, true] {
        let budget = budget();
        let entries = [entry(b"a", b"none"), SnapshotEntry { path: b"b", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
        let mut archive = archive(&entries, complete, &budget);
        let index = build(&mut archive, IndexLimits::default(), &budget);
        let encoded = index.encode(&budget, allocation(7), || false).unwrap(); drop(index);
        let index = SnapshotIndex::decode_pinned(encoded.bytes(), encoded.digest(), archive.directory(), &budget, allocation(3), || false).unwrap();
        assert_eq!(index.stats().unavailable_files, 1);
        let report = run(&mut archive, Some(&index), b"absent", true, 10, &budget);
        assert!(!report.is_complete()); assert_eq!(report.unavailable_files(), 1); assert!(report.hits().is_empty());
    }
}

#[test]
fn every_mutated_or_truncated_artifact_is_refused_under_the_retained_pin() {
    let budget = budget(); let mut archive = archive(&[entry(b"a", b"banana")], true, &budget);
    let index = build(&mut archive, IndexLimits::default(), &budget);
    let encoded = index.encode(&budget, allocation(7), || false).unwrap(); drop(index);
    for i in 0..encoded.bytes().len() {
        let mut corrupt = encoded.bytes().to_vec(); corrupt[i] ^= 1;
        assert!(SnapshotIndex::decode_pinned(&corrupt, encoded.digest(), archive.directory(), &budget, allocation(3), || false).is_err());
        assert!(SnapshotIndex::decode_pinned(&encoded.bytes()[..i], encoded.digest(), archive.directory(), &budget, allocation(3), || false).is_err());
    }
    assert!(matches!(SnapshotIndex::decode_pinned(encoded.bytes(), Sha256Digest::new([0; 32]), archive.directory(),
        &budget, allocation(3), || false), Err(SnapshotIndexError::PinMismatch)));
}

#[test]
fn correct_pin_for_another_snapshot_still_cannot_filter_this_source_universe() {
    let budget = budget();
    let mut old = archive(&[entry(b"a", b"banana")], true, &budget);
    let index = build(&mut old, IndexLimits::default(), &budget);
    let encoded = index.encode(&budget, allocation(7), || false).unwrap(); drop(index); drop(old);
    let new = archive(&[entry(b"a", b"needle")], true, &budget);
    assert!(matches!(SnapshotIndex::decode_pinned(encoded.bytes(), encoded.digest(), new.directory(), &budget,
        allocation(3), || false), Err(SnapshotIndexError::SourceMismatch)));
}

#[test]
fn empty_archives_are_valid_indexed_complete_negatives_without_source_loading() {
    let budget = budget(); let mut archive = archive(&[], true, &budget);
    let index = build(&mut archive, IndexLimits::default(), &budget);
    let artifact = index.encode(&budget, allocation(7), || false).unwrap(); drop(index);
    let index = SnapshotIndex::decode_pinned(artifact.bytes(), artifact.digest(), archive.directory(), &budget, allocation(3), || false).unwrap();
    let report = run(&mut archive, Some(&index), b"absent", true, 10, &budget);
    assert!(report.is_complete()); assert_eq!(report.stats().files_searched, 0);
    assert_eq!(archive.load_stats().bytes_read, 0);
}

#[test]
fn invalidated_queries_and_resource_refusals_do_not_release_an_existing_index() {
    let budget = budget(); let mut archive = archive(&[entry(b"a", b"banana")], true, &budget);
    let index = build(&mut archive, IndexLimits::default(), &budget);
    let baseline = budget.accounting().reserved().get();
    let canceled = SnapshotIndex::build(&mut archive, IndexLimits::default(), &budget,
        [allocation(4), allocation(5), allocation(6), allocation(7)], || budget.accounting().reserved().get() > baseline);
    assert!(matches!(canceled, Err(SnapshotIndexError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(index.encode(&tiny, allocation(1), || false), Err(SnapshotIndexError::ResourceDenied)));
    let needle = IndexedNeedle::text(owner(), "ana", &budget, allocation(10)).unwrap();
    let mut query = PagedQuery::new_indexed(&mut archive, &needle, &index, options(10), &budget,
        [allocation(11), allocation(12), allocation(13)]).unwrap();
    assert_eq!(query.step(StreamReadStep::default(), QueryGeneration::new(owner(), 4).unwrap(), &budget, || false), Err(PagedSearchError::StaleQuery));
    assert!(query.finish().is_err()); drop(needle);
    assert_eq!(budget.accounting().reserved().get(), baseline);
}

#[test]
fn foreign_needle_is_refused_even_when_every_file_would_be_eliminated() {
    let budget = budget(); let mut archive = archive(&[entry(b"a", b"nothing")], true, &budget);
    let index = build(&mut archive, IndexLimits::default(), &budget);
    let foreign = ArenaOwnerId::new(862).unwrap();
    let foreign_budget = ResourceBudget::new(foreign, ByteLength::new(1024 * 1024)).unwrap();
    let needle = IndexedNeedle::text(foreign, "absent", &foreign_budget, allocation(1)).unwrap();
    assert!(matches!(PagedQuery::new_indexed(&mut archive, &needle, &index, options(10), &budget,
        [allocation(11), allocation(12), allocation(13)]), Err(PagedSearchError::OwnerMismatch)));
}

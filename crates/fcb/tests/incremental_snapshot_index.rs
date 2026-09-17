#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::{cell::RefCell, io::{self, Cursor, Read, Seek, SeekFrom}, rc::Rc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{IndexLimits, QueryGeneration, ResourceAllocationId, ResourceBudget, StreamReadStep, StreamingNeedle};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::snapshot_index::{SnapshotIndex, SnapshotIndexError, IndexDecision};
use fcb::search::paged_snapshot::{PagedSnapshot, PagedSnapshotError, PagedMemberData,
    PagedQuery, PagedQueryOptions, PagedQueryState, PagedReport, IndexedNeedle};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2861).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn bytes(entries: &[SnapshotEntry<'_>], complete: bool) -> Vec<u8> {
    SnapshotBytes::encode(owner(), complete, "test-v1", entries, SnapshotLimits::default(), &budget(), id(1), || false)
        .unwrap().bytes().to_vec()
}
fn archive(entries: &[SnapshotEntry<'_>], complete: bool, b: &ResourceBudget, allocation: u64) -> PagedSnapshot<Cursor<Vec<u8>>> {
    PagedSnapshot::open(Cursor::new(bytes(entries, complete)), owner(), SnapshotLimits::default(), b, id(allocation), || false).unwrap()
}
fn build<R: Read + Seek>(a: &mut PagedSnapshot<R>, b: &ResourceBudget) -> SnapshotIndex {
    SnapshotIndex::build(a, IndexLimits::default(), b, [id(3), id(4), id(5), id(6)], || false).unwrap()
}
fn refresh<R: Read + Seek>(old: &SnapshotIndex, new: &mut PagedSnapshot<R>, limits: IndexLimits,
    b: &ResourceBudget) -> fcb::search::snapshot_index::SnapshotRefresh {
    old.refresh(new, limits, b, [id(20), id(21), id(22), id(23), id(24)], || false).unwrap()
}
fn query<R: Read + Seek>(archive: &mut PagedSnapshot<R>, index: Option<&SnapshotIndex>, needle: &[u8],
    text: bool, limit: usize, b: &ResourceBudget) -> PagedReport {
    let options = PagedQueryOptions { generation: QueryGeneration::new(owner(), 7).unwrap(),
        first_file: FileId::new(owner(), 100).unwrap(), first_revision: SourceRevision::new(owner(), 200).unwrap(), max_matches: limit };
    if let Some(index) = index {
        let needle = if text { IndexedNeedle::text(owner(), std::str::from_utf8(needle).unwrap(), b, id(30)).unwrap() }
            else { IndexedNeedle::raw(owner(), needle, b, id(30)).unwrap() };
        let mut query = PagedQuery::new_indexed(archive, &needle, index, options, b, [id(31), id(32), id(33)]).unwrap();
        while query.state() == PagedQueryState::Pending { query.step(StreamReadStep::default(), options.generation, b, || false).unwrap(); }
        query.finish().unwrap()
    } else {
        let needle = if text { StreamingNeedle::text(owner(), std::str::from_utf8(needle).unwrap(), b, id(30)).unwrap() }
            else { StreamingNeedle::raw(owner(), needle, b, id(30)).unwrap() };
        let mut query = PagedQuery::new(archive, &needle, options, b, [id(31), id(32), id(33)]).unwrap();
        while query.state() == PagedQueryState::Pending { query.step(StreamReadStep::default(), options.generation, b, || false).unwrap(); }
        query.finish().unwrap()
    }
}
fn signature(report: &PagedReport) -> (Vec<(usize, u64, u64)>, bool, bool, u64, usize) {
    (report.hits().iter().map(|h| (h.ordinal(), h.original_range().start().get(), h.original_range().end().get())).collect(),
        report.is_complete(), report.truncated(), report.matches_seen(), report.stats().unsupported_files)
}
fn utf16(text: &str) -> Vec<u8> {
    let mut out = vec![0xff, 0xfe];
    for u in text.encode_utf16() { out.extend_from_slice(&u.to_le_bytes()); } out
}

#[test]
fn reopened_prior_reuses_all_segments_and_produces_the_same_wire_image_as_a_fresh_build() {
    let b = budget(); let entries = [entry(b"a", b"banana"), entry(b"b", b""), entry(b"c", b"ab")];
    let mut old_archive = archive(&entries, true, &b, 1);
    let built = build(&mut old_archive, &b);
    let wire = built.encode(&b, id(7), || false).unwrap(); drop(built);
    let old = SnapshotIndex::decode_pinned(wire.bytes(), wire.digest(), old_archive.directory(), &b, id(3), || false).unwrap();
    let mut target = archive(&entries, true, &b, 2);
    let changed = refresh(&old, &mut target, IndexLimits::default(), &b);
    assert_eq!(changed.stats().reused_files, 3);
    assert_eq!(changed.stats().attempted_files, 0);
    assert_eq!(changed.index().stats().build_source_bytes, 0);
    assert_eq!(target.load_stats().loaded_members, 0);
    let encoded = changed.index().encode(&b, id(8), || false).unwrap();
    assert_eq!(encoded.bytes(), wire.bytes());
    drop(old); drop(wire); drop(old_archive);
    let result = query(&mut target, Some(changed.index()), b"ana", true, 10, &b);
    assert_eq!(result.hits().len(), 2); assert!(result.is_complete());
}

#[test]
fn digest_reuse_handles_moved_duplicate_and_reordered_content_but_not_equal_length_edits() {
    let b = budget();
    let mut old_archive = archive(&[entry(b"old", b"banana"), entry(b"same", b"AAAAAA"), entry(b"vanished", b"retired")], true, &b, 1);
    let old = build(&mut old_archive, &b);
    let mut target = archive(&[entry(b"copy", b"banana"), entry(b"moved", b"banana"), entry(b"same", b"needle")], true, &b, 2);
    let changed = refresh(&old, &mut target, IndexLimits::default(), &b);
    assert_eq!(changed.stats().reused_files, 2);
    assert_eq!(changed.stats().reused_source_bytes, 12);
    assert_eq!(changed.stats().rebuilt_files, 1);
    assert_eq!(changed.stats().loaded_source_bytes, 6);
    assert_eq!(target.load_stats().bytes_read, 6);
    assert_eq!(changed.index().stats().members, 3);
    let needle = query(&mut target, Some(changed.index()), b"needle", true, 10, &b);
    assert_eq!(needle.hits().len(), 1); assert_eq!(needle.hits()[0].ordinal(), 2); drop(needle);
    let retired = query(&mut target, Some(changed.index()), b"retired", true, 10, &b);
    assert!(retired.is_complete()); assert!(retired.hits().is_empty()); drop(retired);
    assert_eq!(old.text_decision(1, Some("needle")), IndexDecision::Excluded,
        "negative control: same-path/same-length reuse would wrongly suppress the edited target");
}

#[test]
fn refreshed_queries_equal_unfiltered_queries_across_encodings_short_needles_and_caps() {
    let b = budget(); let utf16 = utf16("banana 😀");
    let mut prior = archive(&[entry(b"a", b"banana bandana"), entry(b"b", &utf16), entry(b"c", b"bad\xff")], true, &b, 1);
    let old = build(&mut prior, &b);
    let mut target = archive(&[entry(b"a", b"new banana"), entry(b"d", &utf16), entry(b"e", b"bad\xff"), entry(b"f", b"")], true, &b, 2);
    let next = refresh(&old, &mut target, IndexLimits::default(), &b);
    for raw in [false, true] {
        for needle in [b"a".as_slice(), b"ana", b"banana", b"new", b"absent"] {
            for limit in [0, 1, 2, 100] {
                let expected = query(&mut target, None, needle, !raw, limit, &b);
                let expected = signature(&expected);
                let actual = query(&mut target, Some(next.index()), needle, !raw, limit, &b);
                assert_eq!(signature(&actual), expected, "raw={raw} needle={needle:?} limit={limit}");
            }
        }
    }
    assert_eq!(next.index().text_decision(1, Some("banana")), IndexDecision::Fallback);
    assert_eq!(next.index().text_decision(2, Some("banana")), IndexDecision::Fallback);
}

#[test]
fn zero_fresh_work_budget_still_reuses_complete_old_segments_and_scans_new_uncovered_members() {
    let b = budget(); let mut prior = archive(&[entry(b"a", b"banana")], true, &b, 1); let old = build(&mut prior, &b);
    let mut target = archive(&[entry(b"a", b"banana"), entry(b"b", b"bandana")], true, &b, 2);
    let next = refresh(&old, &mut target, IndexLimits { max_source_bytes_total: 0, max_scratch_bytes: 0, ..IndexLimits::default() }, &b);
    assert_eq!(next.stats().reused_files, 1); assert_eq!(next.stats().loaded_source_bytes, 0);
    assert_eq!(next.index().stats().uncovered_files, 1);
    let result = query(&mut target, Some(next.index()), b"ana", true, 100, &b);
    assert!(result.is_complete()); assert_eq!(result.hits().len(), 3); assert_eq!(result.stats().index_fallback_files, 1);
}

#[test]
fn reduced_retained_quotas_never_publish_a_prefix_of_an_old_gram_set() {
    for limits in [IndexLimits { max_total_grams: 1, ..IndexLimits::default() },
        IndexLimits { max_grams_per_file: 1, ..IndexLimits::default() },
        IndexLimits { max_source_bytes_per_file: 1, ..IndexLimits::default() }] {
        let b = budget(); let mut prior = archive(&[entry(b"a", b"banana")], true, &b, 1); let old = build(&mut prior, &b);
        let mut target = archive(&[entry(b"a", b"banana")], true, &b, 2);
        let next = refresh(&old, &mut target, limits, &b);
        assert_eq!(next.stats().reuse_quota_refusals, 1);
        assert_eq!(next.index().stats().unique_grams, 0);
        assert_eq!(next.index().raw_decision(0, b"ana"), IndexDecision::Fallback);
        assert_eq!(target.load_stats().loaded_members, 0);
        assert_eq!(query(&mut target, Some(next.index()), b"ana", true, 100, &b).hits().len(), 2);
    }
}

#[test]
fn missing_target_capture_and_partial_membership_do_not_inherit_old_completeness() {
    let b = budget(); let mut prior = archive(&[entry(b"a", b"banana"), entry(b"b", b"needle")], true, &b, 1); let old = build(&mut prior, &b);
    let entries = [entry(b"a", b"banana"), SnapshotEntry { path: b"b", observed_bytes: 6, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
    let mut target = archive(&entries, false, &b, 2);
    let next = refresh(&old, &mut target, IndexLimits::default(), &b);
    assert_eq!(next.index().stats().unavailable_files, 1);
    let report = query(&mut target, Some(next.index()), b"needle", true, 100, &b);
    assert!(!report.is_complete()); assert!(report.hits().is_empty());
    assert_eq!(target.directory().len(), 2);
}

#[test]
fn previously_uncovered_content_can_gain_complete_segments_in_a_later_refresh() {
    let b = budget(); let mut prior = archive(&[entry(b"a", b"banana")], true, &b, 1);
    let old = SnapshotIndex::build(&mut prior, IndexLimits { max_total_grams: 0, ..IndexLimits::default() },
        &b, [id(3), id(4), id(5), id(6)], || false).unwrap();
    let mut target = archive(&[entry(b"moved", b"banana")], true, &b, 2);
    let next = refresh(&old, &mut target, IndexLimits::default(), &b);
    assert_eq!(next.stats().reused_files, 0); assert_eq!(next.stats().rebuilt_files, 1);
    assert_eq!(next.index().stats().uncovered_files, 0); assert_eq!(target.load_stats().bytes_read, 6);
}

#[test]
fn canceled_refused_and_foreign_refreshes_preserve_the_prior_generation() {
    let b = budget(); let mut prior = archive(&[entry(b"a", b"banana")], true, &b, 1); let old = build(&mut prior, &b);
    let mut target = archive(&[entry(b"a", b"banana")], true, &b, 2);
    let baseline = b.accounting().reserved();
    assert!(matches!(old.refresh(&mut target, IndexLimits::default(), &b, [id(20), id(21), id(22), id(23), id(24)],
        || b.accounting().reserved() > baseline), Err(SnapshotIndexError::Canceled)));
    assert_eq!(b.accounting().reserved(), baseline);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(old.refresh(&mut target, IndexLimits::default(), &tiny, [id(20), id(21), id(22), id(23), id(24)], || false),
        Err(SnapshotIndexError::ResourceDenied)));
    assert!(matches!(old.refresh(&mut target, IndexLimits::default(), &b, [id(20), id(21), id(22), id(23), id(20)], || false),
        Err(SnapshotIndexError::Limits)));
    let foreign = ArenaOwnerId::new(2862).unwrap();
    let other_budget = ResourceBudget::new(foreign, ByteLength::new(1024 * 1024)).unwrap();
    let mut other = PagedSnapshot::open(Cursor::new(bytes(&[], true)), foreign, SnapshotLimits::default(), &other_budget, id(1), || false).unwrap();
    assert!(matches!(old.refresh(&mut other, IndexLimits::default(), &other_budget, [id(20), id(21), id(22), id(23), id(24)], || false),
        Err(SnapshotIndexError::OwnerMismatch)));
    assert_eq!(query(&mut prior, Some(&old), b"ana", true, 100, &b).hits().len(), 2);
}

struct MutableInput(Rc<RefCell<Cursor<Vec<u8>>>>);
impl Read for MutableInput { fn read(&mut self, into: &mut [u8]) -> io::Result<usize> { self.0.borrow_mut().read(into) } }
impl Seek for MutableInput { fn seek(&mut self, to: SeekFrom) -> io::Result<u64> { self.0.borrow_mut().seek(to) } }

#[test]
fn changed_target_during_fresh_member_load_aborts_without_releasing_prior_index() {
    let b = budget(); let mut prior = archive(&[entry(b"a", b"banana")], true, &b, 1); let old = build(&mut prior, &b);
    let input = Rc::new(RefCell::new(Cursor::new(bytes(&[entry(b"a", b"banana"), entry(b"b", b"needle")], true))));
    let mut target = PagedSnapshot::open(MutableInput(input.clone()), owner(), SnapshotLimits::default(), &b, id(2), || false).unwrap();
    let PagedMemberData::Captured { archive_offset, .. } = target.directory().member(1).unwrap().data else { panic!() };
    input.borrow_mut().get_mut()[archive_offset as usize] ^= 1;
    let baseline = b.accounting().reserved();
    assert!(matches!(old.refresh(&mut target, IndexLimits::default(), &b, [id(20), id(21), id(22), id(23), id(24)], || false),
        Err(SnapshotIndexError::Archive(PagedSnapshotError::Changed))));
    assert_eq!(b.accounting().reserved(), baseline);
    assert_eq!(old.text_decision(0, Some("ana")), IndexDecision::Verify);
}

#[test]
fn empty_and_old_generation_drop_leave_independent_new_owned_state() {
    let b = budget(); let mut prior = archive(&[entry(b"a", b"banana")], true, &b, 1); let old = build(&mut prior, &b);
    let mut target = archive(&[], true, &b, 2);
    let next = refresh(&old, &mut target, IndexLimits::default(), &b);
    assert_eq!(next.index().stats().members, 0);
    drop(old); drop(prior);
    let report = query(&mut target, Some(next.index()), b"ana", true, 100, &b);
    assert!(report.is_complete()); assert!(report.hits().is_empty());
    drop(report); drop(next); drop(target);
    assert_eq!(b.accounting().reserved().get(), 0);
}

#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! Public production path: validate two archives, merge native membership,
//! load verified source pairs, compare bytes, then read the retained versions.
use std::io::Cursor;
use fcb::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb::search::{CaptureRequest, QueryGeneration, ResourceAllocationId, ResourceBudget,
    ReaderLimits, ReadingTarget, ReadingSeekState, ReadingWindowOptions};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::snapshot_comparison::{SnapshotComparison, SnapshotPair, SnapshotChangeKind,
    SnapshotCompareError, ComparisonSide, ComparisonLimits, ComparisonQuality, CorrespondenceKind};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(956).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 30).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(32 * 1024 * 1024)).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn archive(entries: &[SnapshotEntry<'_>], complete: bool, policy: &str, budget: &ResourceBudget, id: u64) -> PagedSnapshot<Cursor<Vec<u8>>> {
    let encoded = SnapshotBytes::encode(owner(), complete, policy, entries, SnapshotLimits::default(), budget, allocation(id + 100), || false).unwrap();
    let input = Cursor::new(encoded.bytes().to_vec());
    PagedSnapshot::open(input, owner(), SnapshotLimits::default(), budget, allocation(id), || false).unwrap()
}
fn requests() -> [CaptureRequest; 2] {
    [CaptureRequest::new(FileId::new(owner(), 10).unwrap(), SourceRevision::new(owner(), 11).unwrap()).unwrap(),
     CaptureRequest::new(FileId::new(owner(), 20).unwrap(), SourceRevision::new(owner(), 21).unwrap()).unwrap()]
}
fn raw(start: u64, end: u64) -> ByteRange { ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap() }

#[test]
fn sorted_merge_preserves_empty_missing_changed_added_and_removed_members() {
    let budget = budget();
    let old = archive(&[entry(b"a", b"same"), entry(b"b", b"old"), entry(b"d", b"gone"),
        SnapshotEntry { path: b"e", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") },
        entry(b"g", b"")], true, "policy-v1", &budget, 1);
    let new = archive(&[entry(b"a", b"same"), entry(b"b", b"new"), entry(b"c", b"added"), entry(b"e", b"now present"), entry(b"g", b"")], true, "policy-v1", &budget, 2);
    let mut cursor = SnapshotComparison::new(old.directory(), new.directory(), generation()).unwrap();
    let mut kinds = Vec::new(); let mut paths = Vec::new();
    while let Some(change) = cursor.next(old.directory(), new.directory(), generation(), || false).unwrap() {
        paths.push(change.path(old.directory(), new.directory()).unwrap().to_vec()); kinds.push(change.kind());
    }
    assert_eq!(paths, [b"a", b"b", b"c", b"d", b"e", b"g"]);
    assert_eq!(kinds, [SnapshotChangeKind::Unchanged, SnapshotChangeKind::Changed, SnapshotChangeKind::Added,
        SnapshotChangeKind::Removed, SnapshotChangeKind::Unavailable, SnapshotChangeKind::Unchanged]);
    assert!(cursor.finished()); assert!(cursor.membership_comparable());
    assert_eq!(cursor.stats().known_differences(), 3); assert_eq!(cursor.stats().uncertain_paths(), 1);
}

#[test]
fn incomplete_or_different_policy_scopes_never_invent_absence() {
    let budget = budget();
    for complete in [false, true] { for policy in ["same", "different"] {
        let old = archive(&[entry(b"before", b"old")], complete, "same", &budget, 1);
        let new = archive(&[entry(b"after", b"new")], complete, policy, &budget, 2);
        let mut cursor = SnapshotComparison::new(old.directory(), new.directory(), generation()).unwrap();
        let first = cursor.next(old.directory(), new.directory(), generation(), || false).unwrap().unwrap();
        let second = cursor.next(old.directory(), new.directory(), generation(), || false).unwrap().unwrap();
        let comparable = complete && policy == "same";
        assert_eq!(first.kind(), if comparable { SnapshotChangeKind::Added } else { SnapshotChangeKind::OnlyAfter });
        assert_eq!(second.kind(), if comparable { SnapshotChangeKind::Removed } else { SnapshotChangeKind::OnlyBefore });
        assert_eq!(cursor.membership_comparable(), comparable);
    } }
}

#[test]
fn exact_pair_reading_and_correspondence_survive_archive_release() {
    fn utf16(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xfe];
        for unit in text.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); } bytes
    }
    let budget = budget();
    let old_bytes = utf16("head\nold tail"); let new_bytes = utf16("head\nnew tail");
    let mut old = archive(&[entry(b"raw\xff.rs", &old_bytes)], true, "same", &budget, 1);
    let mut new = archive(&[entry(b"raw\xff.rs", &new_bytes)], true, "same", &budget, 2);
    let mut cursor = SnapshotComparison::new(old.directory(), new.directory(), generation()).unwrap();
    let change = cursor.next(old.directory(), new.directory(), generation(), || false).unwrap().unwrap();
    let pair = SnapshotPair::load(&mut old, &mut new, change, requests(), generation(), &budget,
        [allocation(3), allocation(4)], || false).unwrap();
    drop(old); drop(new);
    assert_eq!(pair.bytes(ComparisonSide::Before), old_bytes);
    assert_eq!(pair.bytes(ComparisonSide::After), new_bytes);
    let diff = pair.compare(generation(), ComparisonLimits::default(), &budget, allocation(5), || false).unwrap();
    assert_eq!(diff.quality(), ComparisonQuality::Exact);
    assert_eq!(diff.corresponding_after(raw(20, 28)), Some(raw(20, 28)));
    for side in [ComparisonSide::Before, ComparisonSide::After] {
        let reader = pair.reader(side, ReaderLimits::default(), &budget, allocation(6)).unwrap();
        let seek = reader.seek(ReadingTarget::Byte(ByteOffset::new(0)), generation()).unwrap();
        let ReadingSeekState::Ready(at) = seek.state() else { panic!("first line must be ready") };
        let window = reader.window(at, generation(), ReadingWindowOptions::default(), &budget, allocation(7), || false).unwrap();
        assert_eq!(window.line_text(0), Some("head"));
        assert_eq!(window.line_text(1), Some(if side == ComparisonSide::Before { "old tail" } else { "new tail" }));
    }
    diff.validate_delivery(requests()[0], requests()[1], generation()).unwrap();
    assert!(diff.validate_delivery(requests()[1], requests()[0], generation()).is_err());
    drop(diff); drop(pair); assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn unchanged_regions_are_verified_and_unresolved_regions_still_partition_sources() {
    let budget = budget();
    let mut old = archive(&[entry(b"a", b"prefix old suffix")], true, "same", &budget, 1);
    let mut new = archive(&[entry(b"a", b"prefix new suffix")], true, "same", &budget, 2);
    let mut cursor = SnapshotComparison::new(old.directory(), new.directory(), generation()).unwrap();
    let change = cursor.next(old.directory(), new.directory(), generation(), || false).unwrap().unwrap();
    let pair = SnapshotPair::load(&mut old, &mut new, change, requests(), generation(), &budget,
        [allocation(3), allocation(4)], || false).unwrap();
    for max_work in 0..80 {
        let diff = pair.compare(generation(), ComparisonLimits { max_work, ..Default::default() }, &budget, allocation(5), || false).unwrap();
        let (mut a, mut b) = (0, 0);
        for (i, span) in diff.spans().iter().enumerate() {
            assert_eq!(span.before().start().get(), a); assert_eq!(span.after().start().get(), b);
            if span.kind() == CorrespondenceKind::Equal { assert_eq!(diff.before_bytes(i), diff.after_bytes(i)); }
            a = span.before().end().get(); b = span.after().end().get();
        }
        assert_eq!(a, 17); assert_eq!(b, 17); assert!(diff.stats().work_units <= max_work);
    }
}

#[test]
fn native_path_equality_does_not_infer_renames_or_merge_case_and_lossy_aliases() {
    let budget = budget();
    let old = archive(&[entry(b"A", b"same"), entry(b"name\xff", b"same")], true, "same", &budget, 1);
    let new = archive(&[entry(b"a", b"same"), entry("name�".as_bytes(), b"same")], true, "same", &budget, 2);
    let mut cursor = SnapshotComparison::new(old.directory(), new.directory(), generation()).unwrap();
    while let Some(change) = cursor.next(old.directory(), new.directory(), generation(), || false).unwrap() {
        assert!(matches!(change.kind(), SnapshotChangeKind::Added | SnapshotChangeKind::Removed));
    }
    assert_eq!(cursor.stats().added, 2); assert_eq!(cursor.stats().removed, 2); assert_eq!(cursor.stats().unchanged, 0);
}

#[test]
fn stale_archive_query_and_reused_capture_identity_are_refused() {
    let budget = budget();
    let mut old = archive(&[entry(b"a", b"old")], true, "same", &budget, 1);
    let mut new = archive(&[entry(b"a", b"new")], true, "same", &budget, 2);
    let other = archive(&[entry(b"a", b"different")], true, "same", &budget, 3);
    let mut cursor = SnapshotComparison::new(old.directory(), new.directory(), generation()).unwrap();
    let change = cursor.next(old.directory(), new.directory(), generation(), || false).unwrap().unwrap();
    assert!(matches!(change.validate(old.directory(), other.directory(), generation()), Err(SnapshotCompareError::Stale)));
    let replaced = QueryGeneration::new(owner(), 31).unwrap();
    assert!(matches!(cursor.next(old.directory(), new.directory(), replaced, || false), Err(SnapshotCompareError::Stale)));
    let reused = [requests()[0], requests()[0]];
    assert!(matches!(SnapshotPair::load(&mut old, &mut new, change, reused, generation(), &budget,
        [allocation(4), allocation(5)], || false), Err(SnapshotCompareError::InvalidPair)));
}

#[test]
fn unavailable_side_is_not_replaced_with_empty_bytes() {
    let budget = budget();
    let mut old = archive(&[SnapshotEntry { path: b"a", observed_bytes: 0,
        data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }], true, "same", &budget, 1);
    let mut new = archive(&[entry(b"a", b"")], true, "same", &budget, 2);
    let mut cursor = SnapshotComparison::new(old.directory(), new.directory(), generation()).unwrap();
    let change = cursor.next(old.directory(), new.directory(), generation(), || false).unwrap().unwrap();
    assert_eq!(change.kind(), SnapshotChangeKind::Unavailable);
    assert!(matches!(SnapshotPair::load(&mut old, &mut new, change, requests(), generation(), &budget,
        [allocation(3), allocation(4)], || false), Err(SnapshotCompareError::InvalidPair)));
}

#[test]
fn canceled_and_denied_pair_loads_leave_no_owned_payload_or_lease() {
    let budget = budget();
    let mut old = archive(&[entry(b"a", b"old")], true, "same", &budget, 1);
    let mut new = archive(&[entry(b"a", b"new")], true, "same", &budget, 2);
    let mut cursor = SnapshotComparison::new(old.directory(), new.directory(), generation()).unwrap();
    let change = cursor.next(old.directory(), new.directory(), generation(), || false).unwrap().unwrap();
    let baseline = budget.accounting().reserved().get();
    assert!(matches!(SnapshotPair::load(&mut old, &mut new, change, requests(), generation(), &budget,
        [allocation(3), allocation(4)], || budget.accounting().reserved().get() > baseline),
        Err(SnapshotCompareError::Canceled | SnapshotCompareError::Archive(_))));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(SnapshotPair::load(&mut old, &mut new, change, requests(), generation(), &tiny,
        [allocation(3), allocation(4)], || false), Err(SnapshotCompareError::ResourceDenied)));
}

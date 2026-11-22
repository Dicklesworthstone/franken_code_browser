#![forbid(unsafe_code)]

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, ResourceAllocationId,
    ResourceBudget, ResourceLease, SourceRevision};
use fcb_source::{CaptureRequest, confined::range::{ExtentConsistency, ObservedExtent}};

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
}
fn request(start: u64, end: u64) -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(1), 2).unwrap(), SourceRevision::new(owner(1), 3).unwrap())
        .unwrap().with_range(range(start, end)).unwrap()
}
fn lease(who: u64, charge: u64) -> ResourceLease {
    ResourceBudget::new(owner(who), ByteLength::new(8192)).unwrap()
        .try_reserve_managed(owner(who), ResourceAllocationId::new(1).unwrap(), ByteLength::new(charge)).unwrap()
}

#[test]
fn restore_keeps_short_prefix_and_original_request_above_four_gib() {
    let base = (1u64 << 40) + 7;
    let extent = ObservedExtent::restore_recorded(request(base, base + 10), range(base, base + 4),
        ByteLength::new(base + 20), Some(ByteLength::new(base + 4)), ExtentConsistency::ShortRead,
        b"old\0", lease(1, 4096)).unwrap();
    assert_eq!(extent.request().range(), Some(range(base, base + 10)));
    assert_eq!(extent.range(), range(base, base + 4));
    assert_eq!(extent.observed_length().get(), base + 20);
    assert_eq!(extent.final_length().unwrap().get(), base + 4);
    assert_eq!(extent.consistency(), ExtentConsistency::ShortRead);
    assert!(!extent.request_filled());
    assert_eq!(extent.range_bytes(range(base + 1, base + 4)).unwrap(), b"ld\0");
    assert!(extent.range_bytes(range(base, base + 5)).is_err());
}

#[test]
fn changed_and_missing_metadata_are_not_promoted_to_host_supplied() {
    for (consistency, final_length) in [
        (ExtentConsistency::ChangedDuringRead, Some(ByteLength::new(9))),
        (ExtentConsistency::ChangedDuringRead, Some(ByteLength::new(4))),
        (ExtentConsistency::MetadataUnavailable, None),
        (ExtentConsistency::MetadataUnavailable, Some(ByteLength::new(4))),
        (ExtentConsistency::UnchangedMetadata, Some(ByteLength::new(4))),
        (ExtentConsistency::HostSupplied, Some(ByteLength::new(4))),
    ] {
        let extent = ObservedExtent::restore_recorded(request(0, 4), range(0, 4), ByteLength::new(4),
            final_length, consistency, b"data", lease(1, 4096)).unwrap();
        assert_eq!(extent.consistency(), consistency);
        assert_eq!(extent.final_length(), final_length);
        assert!(extent.request_filled());
    }
}

#[test]
fn hostile_prefix_counts_consistency_and_undercharged_leases_are_refused() {
    for (captured, bytes, consistency, final_length) in [
        (range(1, 4), b"abc".as_slice(), ExtentConsistency::ShortRead, Some(ByteLength::new(4))),
        (range(0, 5), b"abcde", ExtentConsistency::ShortRead, Some(ByteLength::new(4))),
        (range(0, 3), b"abcd", ExtentConsistency::ShortRead, Some(ByteLength::new(4))),
        (range(0, 3), b"abc", ExtentConsistency::HostSupplied, Some(ByteLength::new(4))),
        (range(0, 4), b"abcd", ExtentConsistency::ShortRead, Some(ByteLength::new(4))),
        (range(0, 4), b"abcd", ExtentConsistency::UnchangedMetadata, Some(ByteLength::new(3))),
        (range(0, 4), b"abcd", ExtentConsistency::ChangedDuringRead, None),
        (range(0, 4), b"abcd", ExtentConsistency::MetadataUnavailable, Some(ByteLength::new(5))),
    ] {
        assert!(ObservedExtent::restore_recorded(request(0, 4), captured, ByteLength::new(4),
            final_length, consistency, bytes, lease(1, 4096)).is_err());
    }
    for admission in [lease(2, 4096), lease(1, 1)] {
        assert!(ObservedExtent::restore_recorded(request(0, 4), range(0, 4), ByteLength::new(4),
            Some(ByteLength::new(4)), ExtentConsistency::HostSupplied, b"data", admission).is_err());
    }
}

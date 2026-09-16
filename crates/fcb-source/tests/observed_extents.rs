#![forbid(unsafe_code)]

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId,
    ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::{CaptureRequest, SourceError};
use fcb_source::confined::range::{ExtentConsistency, ExtentError, ExtentReadState,
    ExtentStepBudget, ExtentWindowRequest, FileRangeReader, ObservedExtent,
    MAX_OBSERVED_EXTENT_BYTES};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1211).unwrap() }
fn file() -> FileId { FileId::new(owner(), 1).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(8 * 1024 * 1024)).unwrap() }
fn range(start: u64, end: u64) -> ByteRange { ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap() }
fn request(revision: u64, start: u64, end: u64) -> CaptureRequest {
    let request = CaptureRequest::new(file(), SourceRevision::new(owner(), revision).unwrap()).unwrap();
    if start == end { request } else { request.with_range(range(start, end)).unwrap() }
}

#[test]
fn retained_extent_serves_actual_bytes_and_never_fills_unknown_holes() {
    let budget = budget();
    let extent = ObservedExtent::from_bytes(request(1, 100, 106), ByteLength::new(1000), b"banana",
        &budget, allocation(1)).unwrap();
    assert_eq!(extent.range_bytes(range(101, 104)).unwrap(), b"ana");
    assert!(extent.range_bytes(range(101, 101)).unwrap().is_empty());
    assert_eq!(extent.range_bytes(range(99, 104)), Err(ExtentError::Source(SourceError::CaptureUnavailable)));
    assert_eq!(extent.range_bytes(range(104, 107)), Err(ExtentError::Source(SourceError::CaptureUnavailable)));
    assert!(extent.request_filled()); assert!(!extent.covers_whole_observation());
    assert_eq!(extent.consistency(), ExtentConsistency::HostSupplied);
}

#[test]
fn full_width_offsets_are_subtracted_before_local_conversion() {
    let budget = budget();
    let start = u64::MAX - 6;
    let extent = ObservedExtent::from_bytes(request(u64::MAX, start, u64::MAX), ByteLength::new(u64::MAX),
        b"abcdef", &budget, allocation(1)).unwrap();
    assert_eq!(extent.range_bytes(range(start + 1, start + 4)).unwrap(), b"bcd");
    assert_eq!(extent.request().revision().get(), u64::MAX);
    assert_eq!(extent.range().end().get(), u64::MAX);
}

#[test]
fn extent_clones_share_payload_and_its_charge_until_last_release() {
    let budget = budget();
    let first = ObservedExtent::from_bytes(request(1, 0, 4), ByteLength::new(5000), b"data", &budget, allocation(1)).unwrap();
    let charge = budget.accounting().reserved().get();
    let retained = first.clone();
    assert_eq!(first.bytes().as_ptr(), retained.bytes().as_ptr());
    drop(first);
    assert_eq!(budget.accounting().reserved().get(), charge);
    assert_eq!(retained.bytes(), b"data");
    drop(retained);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn invalid_metadata_and_denied_capacity_do_not_publish_an_extent() {
    let budget = budget();
    assert!(matches!(ObservedExtent::from_bytes(request(1, 0, 5), ByteLength::new(5), b"four",
        &budget, allocation(1)), Err(ExtentError::Source(SourceError::MetadataMismatch))));
    assert!(matches!(ObservedExtent::from_bytes(request(1, 4, 8), ByteLength::new(7), b"four",
        &budget, allocation(1)), Err(ExtentError::Source(SourceError::RangeOutOfBounds))));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(ObservedExtent::from_bytes(request(1, 0, 4), ByteLength::new(4), b"four",
        &tiny, allocation(1)), Err(ExtentError::ResourceDenied)));
    assert_eq!(budget.accounting().reserved().get(), 0);
    assert_eq!(tiny.accounting().reserved().get(), 0);
}

#[test]
fn context_window_planning_is_bounded_at_origin_eof_and_u64_max() {
    for length in [0, 1, 17, 65536, u64::MAX] {
        for offset in [0, length / 2, length] {
            for width in [4, 17, 4096] {
                let plan = ExtentWindowRequest::new(ByteOffset::new(offset), width, ByteLength::new(length)).unwrap();
                assert!(plan.capture.start() <= plan.visible.start());
                assert!(plan.capture.end() >= plan.visible.end());
                assert!(plan.capture.end().get() <= length);
                assert!(plan.capture.len().get() <= width as u64 + 16);
                assert_eq!(plan.visible.start().get(), offset);
            }
        }
    }
    assert!(ExtentWindowRequest::new(ByteOffset::new(0), MAX_OBSERVED_EXTENT_BYTES, ByteLength::new(u64::MAX)).is_err());
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::{fs::{self, File, OpenOptions}, io::{Seek, SeekFrom}, os::unix::fs::FileExt,
        path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    fn fixture() -> (PathBuf, File) {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("fcb-observed-extent-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("source.bin");
        let file = OpenOptions::new().create_new(true).read(true).write(true).open(&path).unwrap();
        (path, file)
    }
    fn finish(read: &mut fcb_source::confined::range::FileExtentRead<'_>) {
        for _ in 0..1024 {
            if read.state() != ExtentReadState::Pending { return; }
            read.step(ExtentStepBudget { max_bytes: 5, max_calls: 1 }, || false).unwrap();
            assert!(read.stats().last_step_bytes <= 5);
            assert!(read.stats().last_step_calls <= 1);
        }
        panic!("bounded range read did not finish");
    }
    #[test]
    fn real_sparse_file_beyond_four_gib_reads_only_the_requested_bytes() {
        let (_, mut handle) = fixture();
        let offset = (1u64 << 32) + 71;
        handle.set_len(offset + 100).unwrap();
        handle.write_all_at(b"actual needle", offset).unwrap();
        handle.seek(SeekFrom::Start(9)).unwrap();
        let mut reader = FileRangeReader::new(file(), handle.try_clone().unwrap()).unwrap();
        let budget = budget();
        let mut read = reader.begin(request(1, offset, offset + 13), &budget, allocation(1)).unwrap();
        assert_eq!(read.stats().bytes_read, 0);
        assert_eq!(read.step(ExtentStepBudget { max_bytes: 0, max_calls: 1 }, || false).unwrap(), ExtentReadState::Pending);
        assert_eq!(read.stats().read_calls, 0);
        finish(&mut read);
        assert_eq!(read.stats().bytes_read, 13);
        let extent = read.finish(|| false).unwrap();
        assert_eq!(extent.bytes(), b"actual needle");
        assert_eq!(extent.observed_length().get(), offset + 100);
        assert_eq!(extent.range(), range(offset, offset + 13));
        assert_eq!(handle.stream_position().unwrap(), 9, "positioned reads must not disturb a shared file cursor");
        assert!(budget.accounting().reserved().get() < 4096, "sparse file size is not a payload allocation");
    }
    #[test]
    fn pathname_replacement_does_not_reopen_the_new_object() {
        let (path, handle) = fixture();
        handle.write_all_at(b"original", 0).unwrap();
        let mut reader = FileRangeReader::new(file(), handle).unwrap();
        let moved = path.with_file_name("retained-original.bin");
        fs::rename(&path, &moved).unwrap();
        fs::write(&path, b"replaced").unwrap();
        let budget = budget();
        let mut read = reader.begin(request(1, 0, 8), &budget, allocation(1)).unwrap();
        finish(&mut read);
        let extent = read.finish(|| false).unwrap();
        drop(reader);
        assert_eq!(extent.bytes(), b"original");
        assert_eq!(fs::read(&path).unwrap(), b"replaced");
    }
    #[test]
    fn truncation_publishes_only_observed_prefix_with_explicit_short_read() {
        let (_, handle) = fixture();
        handle.write_all_at(b"abcdefghij", 0).unwrap();
        let mut reader = FileRangeReader::new(file(), handle.try_clone().unwrap()).unwrap();
        let budget = budget();
        let mut read = reader.begin(request(1, 0, 10), &budget, allocation(1)).unwrap();
        read.step(ExtentStepBudget { max_bytes: 3, max_calls: 1 }, || false).unwrap();
        handle.set_len(5).unwrap();
        finish(&mut read);
        let extent = read.finish(|| false).unwrap();
        assert_eq!(extent.bytes(), b"abcde");
        assert_eq!(extent.range(), range(0, 5));
        assert_eq!(extent.consistency(), ExtentConsistency::ShortRead);
        assert!(!extent.request_filled());
        assert_eq!(extent.final_length(), Some(ByteLength::new(5)));
        assert!(extent.range_bytes(range(4, 6)).is_err());
    }
    #[test]
    fn growth_is_reported_without_extending_the_buffer_or_read_scope() {
        let (_, handle) = fixture();
        handle.write_all_at(b"abcdef", 0).unwrap();
        let mut reader = FileRangeReader::new(file(), handle.try_clone().unwrap()).unwrap();
        let budget = budget();
        let mut read = reader.begin(request(1, 0, 6), &budget, allocation(1)).unwrap();
        let reserved = budget.accounting().reserved().get();
        handle.set_len(1u64 << 33).unwrap();
        finish(&mut read);
        assert_eq!(read.stats().bytes_read, 6);
        let extent = read.finish(|| false).unwrap();
        assert_eq!(extent.bytes(), b"abcdef");
        assert_eq!(extent.consistency(), ExtentConsistency::ChangedDuringRead);
        assert_eq!(budget.accounting().reserved().get(), reserved);
    }
    #[test]
    fn later_observations_do_not_relabel_old_retained_bytes() {
        let (_, handle) = fixture();
        handle.write_all_at(b"before", 0).unwrap();
        let mut reader = FileRangeReader::new(file(), handle.try_clone().unwrap()).unwrap();
        let budget = budget();
        let mut read = reader.begin(request(1, 0, 6), &budget, allocation(1)).unwrap();
        finish(&mut read); let old = read.finish(|| false).unwrap();
        handle.write_all_at(b"after!", 0).unwrap();
        assert!(matches!(reader.begin(request(1, 0, 6), &budget, allocation(2)), Err(ExtentError::StaleObservation)));
        let mut read = reader.begin(request(2, 0, 6), &budget, allocation(2)).unwrap();
        finish(&mut read); let new = read.finish(|| false).unwrap();
        assert_eq!(old.bytes(), b"before"); assert_eq!(new.bytes(), b"after!");
        assert_ne!(old.request().revision(), new.request().revision());
    }
    #[test]
    fn pending_canceled_and_failed_admission_candidates_do_not_escape() {
        let (_, handle) = fixture(); handle.write_all_at(b"abcdef", 0).unwrap();
        let mut reader = FileRangeReader::new(file(), handle).unwrap();
        let budget = budget();
        let read = reader.begin(request(1, 0, 6), &budget, allocation(1)).unwrap();
        assert!(matches!(read.finish(|| false), Err(ExtentError::Pending)));
        assert_eq!(budget.accounting().reserved().get(), 0);
        let mut read = reader.begin(request(2, 0, 6), &budget, allocation(1)).unwrap();
        read.step(ExtentStepBudget { max_bytes: 2, max_calls: 1 }, || false).unwrap();
        assert_eq!(read.step(ExtentStepBudget::default(), || true), Err(ExtentError::Canceled));
        assert_eq!(read.step(ExtentStepBudget::default(), || false), Err(ExtentError::Canceled));
        assert_eq!(read.stats().bytes_read, 2);
        assert!(matches!(read.finish(|| false), Err(ExtentError::Canceled)));
        assert_eq!(budget.accounting().reserved().get(), 0);
        assert!(reader.begin(request(3, 0, 7), &budget, allocation(1)).is_err());
        assert!(matches!(reader.begin(request(3, 0, 6), &budget, allocation(1)), Err(ExtentError::StaleObservation)));
    }
    #[test]
    fn empty_file_and_nonregular_handles_have_distinct_outcomes() {
        let (path, handle) = fixture();
        let mut reader = FileRangeReader::new(file(), handle).unwrap();
        let budget = budget();
        let mut read = reader.begin(request(1, 0, 0), &budget, allocation(1)).unwrap();
        read.step(ExtentStepBudget::default(), || false).unwrap();
        let extent = read.finish(|| false).unwrap();
        assert!(extent.bytes().is_empty()); assert!(extent.request_filled());
        assert!(extent.covers_whole_observation());
        let dir = File::open(path.parent().unwrap()).unwrap();
        assert!(matches!(FileRangeReader::new(file(), dir), Err(ExtentError::Source(SourceError::SpecialObject))));
    }
}

#![forbid(unsafe_code)]

use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb_source::{CaptureRequest, CompleteCapture, SourceError};

fn request(owner: u64) -> CaptureRequest {
    let owner = ArenaOwnerId::new(owner).unwrap();
    CaptureRequest::new(FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap()).unwrap()
}
fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
}

#[test]
fn explicit_identity_rebinding_retains_one_allocation_until_the_last_capture_drops() {
    let bytes: Arc<[u8]> = Arc::from(&b"\xff\xfea\0\r\0\n\0\xff"[..]);
    let lifetime = Arc::downgrade(&bytes);
    let first = CompleteCapture::new(request(9101), ByteLength::new(bytes.len() as u64), bytes).unwrap();
    let second = first.share_with_request(request(9102)).unwrap();
    assert_ne!(first.request(), second.request());
    assert!(std::ptr::eq(first.bytes().as_ptr(), second.bytes().as_ptr()),
        "explicit sharing must not allocate another source buffer");
    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.declared_length(), second.declared_length());
    assert_eq!(first.range_bytes(range(1, 8)).unwrap(), second.range_bytes(range(1, 8)).unwrap());
    assert_eq!(lifetime.strong_count(), 2);
    drop(first);
    assert_eq!(lifetime.strong_count(), 1);
    assert_eq!(second.bytes(), b"\xff\xfea\0\r\0\n\0\xff");
    drop(second);
    assert!(lifetime.upgrade().is_none());
}

#[test]
fn equal_content_does_not_implicitly_share_a_cache_or_identity() {
    let first = CompleteCapture::new(request(9103), ByteLength::new(5), Arc::from(&b"equal"[..])).unwrap();
    let independent = CompleteCapture::new(request(9104), ByteLength::new(5), Arc::from(&b"equal"[..])).unwrap();
    // Negative control: equal bytes and digests alone cannot satisfy the
    // allocation-identity oracle used by the explicit-sharing test.
    assert_eq!(first.bytes(), independent.bytes());
    assert_eq!(first.digest(), independent.digest());
    assert!(!std::ptr::eq(first.bytes().as_ptr(), independent.bytes().as_ptr()));
    assert_ne!(first.request(), independent.request());
    let shared = first.share_with_request(request(9105)).unwrap();
    assert!(std::ptr::eq(first.bytes().as_ptr(), shared.bytes().as_ptr()));
    assert!(!std::ptr::eq(independent.bytes().as_ptr(), shared.bytes().as_ptr()));
}

#[test]
fn rebinding_preserves_range_extent_including_explicit_subsets() {
    let original_range = range(10, 14);
    let first = CompleteCapture::new(request(9106).with_range(original_range).unwrap(),
        ByteLength::new(4), Arc::from(&b"part"[..])).unwrap();
    let next = first.share_with_request(request(9107).with_range(original_range).unwrap()).unwrap();
    assert_eq!(next.request().range(), Some(original_range));
    assert!(std::ptr::eq(first.bytes().as_ptr(), next.bytes().as_ptr()));
    assert_eq!(first.share_with_request(request(9108)), Err(SourceError::InvalidRange),
        "sharing a captured subset must not promote it into a whole-file observation");
    assert_eq!(first.share_with_request(request(9108).with_range(range(20, 24)).unwrap()),
        Err(SourceError::InvalidRange), "equal length does not authorize another source range");
    let whole = CompleteCapture::new(request(9109), ByteLength::new(4), Arc::from(&b"part"[..])).unwrap();
    assert_eq!(whole.share_with_request(request(9110).with_range(original_range).unwrap()),
        Err(SourceError::InvalidRange));
}

#[test]
fn empty_capture_rebinding_remains_a_complete_empty_observation() {
    let first = CompleteCapture::new(request(9111), ByteLength::new(0), Arc::from(&b""[..])).unwrap();
    let next = first.share_with_request(request(9112)).unwrap();
    assert!(next.bytes().is_empty());
    assert_eq!(next.declared_length(), ByteLength::new(0));
    assert_eq!(next.request().range(), None);
    assert_eq!(next.digest(), first.digest());
    assert_ne!(next.request(), first.request());
}

#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::{fcb_saved_create, fcb_saved_open, fcb_saved_info, fcb_saved_close};
use std::{ffi::{CStr, CString}, path::PathBuf};
use fcb::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}, trail::{TrailBytes, TrailEntry}};
use fcb::store::{Sha256, Sha256Digest};

const SOURCE: &[u8] = b"Source from the saved trail capture.\n";
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected an owned response");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn fixture() -> (PathBuf, CString, CString) {
    let root = std::env::temp_dir().join(format!("fcb-saved-trail-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(9021).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let bytes = SnapshotBytes::encode(owner, true, "saved-trail-c", &[SnapshotEntry {
        path: b"a-\xff.rs", observed_bytes: SOURCE.len() as u64, data: SnapshotData::Captured(SOURCE) }],
        Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let archive = root.join("saved.fcbs"); std::fs::write(&archive, bytes.bytes()).unwrap();
    let item = TrailEntry { archive: bytes.digest(), source: Sha256::digest(SOURCE), source_length: SOURCE.len() as u64,
        path: b"a-\xff.rs", range: ByteRange::new(ByteOffset::new(7), ByteOffset::new(26)).unwrap(), rationale: "Exact user rationale" };
    let trail = TrailBytes::encode(owner, "Native trail", None, &[item,
        TrailEntry { archive: Sha256Digest::new([6; 32]), rationale: "Other archive remains visible", ..item }],
        &budget, ResourceAllocationId::new(2).unwrap(), || false).unwrap();
    let path = root.join("tour.fcbt"); std::fs::write(&path, trail.bytes()).unwrap();
    (root, CString::new(archive.to_str().unwrap()).unwrap(), CString::new(path.to_str().unwrap()).unwrap())
}

#[test]
fn c_trail_abi_preserves_raw_paths_order_owned_strings_and_reader_lifetime() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, u64, *const c_char) -> *mut c_char = fcb_saved_trail_attach;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_trail_page;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_saved_trail_detach;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_open_trail_reader;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["trail_attach", "trail_page", "trail_detach", "open_trail_reader"] {
        assert_eq!(header.matches(&format!("fcb_saved_{name}(")).count(), 1);
    }
    let (root, archive, path) = fixture();
    let saved = fcb_saved_create(); assert_ne!(saved, 0);
    assert!(take(unsafe { fcb_saved_open(saved, archive.as_ptr()) }).contains("\"status\":\"ok\""));
    let attached = take(unsafe { fcb_saved_trail_attach(saved, 1, path.as_ptr()) });
    assert_eq!(number(&attached, "total_items"), 2);
    assert_eq!(number(&attached, "ready_items"), 1);
    assert!(attached.contains("\"readiness\":\"other-archive\""));
    assert!(attached.contains("\"hex\":\"612dff2e7273\""));
    let retained_page = fcb_saved_trail_page(saved, 1, 0, 32);
    std::fs::rename(root.join("tour.fcbt"), root.join("moved.fcbt")).unwrap();
    std::fs::rename(root.join("saved.fcbs"), root.join("moved.fcbs")).unwrap();
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_saved_open_trail_reader(saved, reader, 1, 0));
    assert!(opened.contains("\"command\":\"trail-open-reader\""));
    assert!(opened.contains("\"live_source_reopened\":false"));
    assert!(opened.contains("\"selection\":{\"start\":\"7\",\"end\":\"26\"}"));
    assert!(opened.contains("Source from the saved trail capture."));
    assert!(take(fcb_saved_trail_detach(saved, 2)).contains("\"accepted_trail_generation\":null"));
    assert_eq!(fcb_saved_close(saved), 1);
    let old_page = take(retained_page);
    assert_eq!(number(&old_page, "trail_generation"), 1);
    assert!(old_page.contains("Exact user rationale"));
    assert!(take(crate::reader_ffi::fcb_reader_window(reader, 0, 256)).contains("Source from the saved trail capture."));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
}

#[test]
fn c_trail_pointer_stale_and_page_validation_do_not_install_or_read_unready_sources() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (_, archive, path) = fixture(); let saved = fcb_saved_create();
    take(unsafe { fcb_saved_open(saved, archive.as_ptr()) });
    assert!(take(unsafe { fcb_saved_trail_attach(saved, 1, std::ptr::null()) }).contains("INVALID_ARGUMENT"));
    let invalid_utf8 = [0xffu8, 0];
    assert!(take(unsafe { fcb_saved_trail_attach(saved, 1, invalid_utf8.as_ptr().cast()) }).contains("INVALID_ARGUMENT"));
    assert!(take(unsafe { fcb_saved_trail_attach(saved, 1, path.as_ptr()) }).contains("\"status\":\"ok\""));
    for (generation, start, limit) in [(0, 0, 1), (2, 0, 1), (1, 0, 0), (1, 0, 33), (1, u64::MAX, 1)] {
        assert!(take(fcb_saved_trail_page(saved, generation, start, limit)).contains("\"status\":\"error\""));
    }
    let before = take(fcb_saved_info(saved));
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    for (generation, item) in [(2, 0), (1, 1), (1, 2), (1, u64::MAX)] {
        assert!(take(fcb_saved_open_trail_reader(saved, reader, generation, item)).contains("\"status\":\"error\""));
        assert!(take(crate::reader_ffi::fcb_reader_info(reader)).contains("READER_HANDLE_NOT_OPEN"));
    }
    let after = take(fcb_saved_info(saved));
    assert_eq!(number(&before, "member_bytes_read"), number(&after, "member_bytes_read"));
    assert_eq!(number(&before, "member_read_calls"), number(&after, "member_read_calls"));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
    assert_eq!(fcb_saved_close(saved), 1);
    let unknown = take(fcb_saved_trail_page(u64::MAX, 1, 0, 1));
    assert!(unknown.len() < 4096 && unknown.contains("SAVED_HANDLE_UNKNOWN"));
}

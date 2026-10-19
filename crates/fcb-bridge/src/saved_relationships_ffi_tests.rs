#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::ffi::{CStr, CString};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget, snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn archive() -> CString {
    let root = std::env::temp_dir().join(format!("fcb-links-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("saved.fcbs"); let owner = ArenaOwnerId::new(8841).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let doc = b"Read [the exact code](code.rs).\n"; let source = b"fn from_archive() {}\n";
    let entries = [
        SnapshotEntry { path: b"README.md", observed_bytes: doc.len() as u64, data: SnapshotData::Captured(doc) },
        SnapshotEntry { path: b"code.rs", observed_bytes: source.len() as u64, data: SnapshotData::Captured(source) },
    ];
    let bytes = SnapshotBytes::encode(owner, true, "relationship-c", &entries, Default::default(), &budget,
        ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    std::fs::write(&path, bytes.bytes()).unwrap(); CString::new(path.to_str().unwrap()).unwrap()
}

#[test]
fn actual_c_backlinks_reader_handoff_and_projection_close_obey_abi_and_lifetime_contracts() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_prepare_relationships;
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_saved_relationships;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_saved_clear_relationships;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_saved_open_relationship_reader;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["prepare_relationships", "relationships", "clear_relationships", "open_relationship_reader"] {
        assert_eq!(header.matches(&format!("fcb_saved_{name}(")).count(), 1);
    }
    let path = archive(); let saved = fcb_saved_create(); assert_ne!(saved, 0);
    assert!(take(unsafe { fcb_saved_open(saved, path.as_ptr()) }).contains("\"status\":\"ok\""));
    assert!(take(fcb_saved_prepare_relationships(saved, 1, 64, 4096)).contains("\"retained_links\":\"1\""));
    for (direction, member) in [(3, 0), (0, 1), (2, u64::MAX)] {
        assert!(take(fcb_saved_relationships(saved, 1, direction, member, 0, 64)).contains("\"status\":\"error\""));
    }
    let retained_page = fcb_saved_relationships(saved, 1, 2, 1, 0, 64);
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    assert!(take(fcb_saved_open_relationship_reader(saved, reader, 1, 1, 9)).contains("\"status\":\"error\""));
    let opened = take(fcb_saved_open_relationship_reader(saved, reader, 1, 1, 1));
    assert!(opened.contains("\"live_source_reopened\":false"));
    assert!(opened.contains("from_archive"));
    assert!(take(fcb_saved_clear_relationships(saved, 2)).contains("\"accepted_relationship_generation\":null"));
    assert_eq!(fcb_saved_close(saved), 1);
    let retained = take(retained_page);
    assert!(retained.contains("\"source_member\":\"0\""));
    assert!(retained.contains("\"target_member\":\"1\""));
    let window = take(crate::reader_ffi::fcb_reader_window(reader, 0, 64));
    assert!(window.contains("from_archive"));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
    let unknown = take(fcb_saved_prepare_relationships(u64::MAX, 1, 64, 4096));
    assert!(unknown.len() < 4096 && unknown.contains("\"status\":\"error\""));
}

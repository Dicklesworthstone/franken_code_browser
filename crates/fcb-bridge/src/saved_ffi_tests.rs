#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use super::*;
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}

#[test]
fn entrypoint_types_and_header_declarations_agree() {
    let _: extern "C" fn() -> u64 = fcb_saved_create;
    let _: unsafe extern "C" fn(u64, *const c_char) -> *mut c_char = fcb_saved_open;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_saved_info;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_saved_members;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, *const c_char) -> *mut c_char = fcb_saved_attach_index;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_saved_detach_index;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64) -> *mut c_char = fcb_saved_search;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_results;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_saved_clear_results;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_open_hit_reader;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_saved_open_member_reader;
    let _: extern "C" fn(u64) -> u8 = fcb_saved_cancel;
    let _: extern "C" fn(u64) -> u8 = fcb_saved_close;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["create", "open", "info", "members", "attach_index", "detach_index", "search", "results",
        "clear_results", "open_hit_reader", "open_member_reader", "cancel", "close"] {
        assert_eq!(header.matches(&format!("fcb_saved_{name}(")).count(), 1, "{name}");
    }
}

#[test]
fn pins_are_exactly_32_bytes_of_hex_and_never_decoded_by_utf8_slicing() {
    for text in ["ab".repeat(32), "AB".repeat(32)] { assert_eq!(pin(&text).unwrap(), Sha256Digest::new([0xab; 32])); }
    for text in ["a".repeat(63), "g".repeat(64), "é".repeat(32), "a".repeat(65), String::new()] {
        assert_eq!(pin(&text).err(), Some(AccessError::InvalidArgument));
    }
}

#[test]
fn unknown_handles_return_independently_owned_bounded_errors_without_source_leakage() {
    let needle = CString::new("private fixture phrase").unwrap();
    let first = fcb_saved_info(u64::MAX);
    let second = unsafe { fcb_saved_search(u64::MAX, 1, needle.as_ptr(), 10) };
    let third = unsafe { fcb_saved_open(u64::MAX, std::ptr::null()) };
    for pointer in [first, second, third, fcb_saved_open_hit_reader(u64::MAX, 0, 1, 1)] {
        let error = take(pointer); assert!(error.len() < 4096);
        assert!(error.contains("\"status\":\"error\"")); assert!(!error.contains("private fixture phrase"));
    }
    assert_eq!(fcb_saved_cancel(u64::MAX), 0); assert_eq!(fcb_saved_close(u64::MAX), 0);
}

#[test]
#[cfg(unix)]
fn actual_c_open_search_paging_clear_and_close_preserve_returned_string_ownership() {
    use fcb::{ArenaOwnerId, ByteLength};
    use fcb::search::{ResourceAllocationId, ResourceBudget, snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData}};
    let root = std::env::temp_dir().join(format!("fcb-saved-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap(); let path = root.join("source.fcbs");
    let owner = ArenaOwnerId::new(8830).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let bytes = SnapshotBytes::encode(owner, true, "test", &[SnapshotEntry {
        path: b"source.rs", observed_bytes: 6, data: SnapshotData::Captured(b"needle") }], Default::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    std::fs::write(&path, bytes.bytes()).unwrap();
    let path = CString::new(path.to_str().unwrap()).unwrap(); let needle = CString::new("needle").unwrap();
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    let opened = take(unsafe { fcb_saved_open(handle, path.as_ptr()) }); assert!(opened.contains("\"status\":\"ok\""));
    let members = take(fcb_saved_members(handle, 0, 10)); assert!(members.contains("\"captured\":true"));
    let searched = take(unsafe { fcb_saved_search(handle, 1, needle.as_ptr(), 10) });
    assert!(searched.contains("\"retained_hits\":\"1\""));
    let retained = fcb_saved_results(handle, 1, 0, 10);
    assert!(take(fcb_saved_clear_results(handle, 2)).contains("\"accepted_query_generation\":null"));
    assert_eq!(fcb_saved_close(handle), 1);
    assert!(take(retained).contains("\"retained_hits\":\"1\""));
    assert!(take(fcb_saved_info(handle)).contains("\"status\":\"error\""));
}

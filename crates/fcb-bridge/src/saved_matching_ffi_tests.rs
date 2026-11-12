#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::ffi::{CStr, CString};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn field(text: &str, name: &str) -> u64 {
    text.split(&format!("\"{name}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}

#[test]
fn matching_saved_abi_freezes_policy_and_hands_off_actual_utf16_spelling() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64) -> *mut c_char = fcb_saved_search_matching;
    assert_eq!(include_str!("../include/fcb_saved_repository.h").matches("fcb_saved_search_matching(").count(), 1);
    let root = std::env::temp_dir().join(format!("fcb-matching-saved-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(88851).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let le: Vec<_> = "\u{feff}BANANA".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = SnapshotBytes::encode(owner, true, "matching-c", &[
        SnapshotEntry { path: b"a.rs", observed_bytes: le.len() as u64, data: SnapshotData::Captured(&le) },
        SnapshotEntry { path: b"b.rs", observed_bytes: 6, data: SnapshotData::Captured(b"BANANA") },
    ], Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = root.join("saved.fcbs"); std::fs::write(&path, encoded.bytes()).unwrap();
    let path = CString::new(path.to_str().unwrap()).unwrap();
    let needle = CString::new("ana").unwrap();
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));
    let first = take(unsafe { fcb_saved_search_matching(handle, 1, needle.as_ptr(), 0, 1, 1) });
    assert!(first.contains("\"status\":\"ok\"") && first.contains("\"search_complete\":true"), "{first}");
    assert!(first.contains("\"mode\":\"unicode-lowercase-literal\"") && first.contains("\"case_sensitive\":false"));
    assert!(first.contains("\"count_all_matches\":true") && first.contains("\"truncated\":true"));
    assert_eq!(field(&first, "matches_seen"), 4); assert_eq!(field(&first, "retained_hits"), 1);
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_saved_open_hit_reader(handle, reader, 1, 1));
    assert!(opened.contains("\"text\":\"ANA\"") && opened.contains("\"original_hex\":\"41004e004100\""), "{opened}");
    assert!(opened.contains("\"case_sensitive\":false") && opened.contains("\"live_source_reopened\":false"));
    for (case_sensitive, count_all) in [(2, 1), (0, 2)] {
        let error = take(unsafe { fcb_saved_search_matching(handle, 2, needle.as_ptr(), case_sensitive, count_all, 1) });
        assert!(error.contains("SAVED_HANDLE_INVALID_ARGUMENT"), "{error}");
        let page = take(fcb_saved_results(handle, 1, 0, 1));
        assert!(page.contains("\"case_sensitive\":false"));
        assert_eq!(field(&page, "matches_seen"), 4);
    }
    // The old exact ABI retains its behavior and shares the query namespace.
    let exact = take(unsafe { fcb_saved_search_counted(handle, 2, needle.as_ptr(), 1) });
    assert!(exact.contains("\"case_sensitive\":true") && exact.contains("\"mode\":\"exact-decoded-literal\""));
    assert_eq!(field(&exact, "matches_seen"), 0);
    let counted = take(unsafe { fcb_saved_search_matching(handle, 3, needle.as_ptr(), 0, 1, 0) });
    assert_eq!(field(&counted, "matches_seen"), 4); assert_eq!(field(&counted, "retained_hits"), 0);
    let stopped = take(unsafe { fcb_saved_search_matching(handle, 4, needle.as_ptr(), 0, 0, 1) });
    assert!(stopped.contains("\"count_all_matches\":false") && stopped.contains("\"search_complete\":false"));
    assert_eq!(field(&stopped, "matches_seen"), 2);
    assert_eq!(fcb_saved_close(handle), 1);
    assert!(take(crate::reader_ffi::fcb_reader_copy_range(reader, 4, 10))
        .contains("\"original_hex\":\"41004e004100\""));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
}

#[test]
fn matching_abi_invalid_flags_and_null_needles_return_independently_owned_private_errors() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let needle = CString::new("private matching query").unwrap();
    let first = unsafe { fcb_saved_search_matching(u64::MAX, 1, needle.as_ptr(), 2, 1, 1) };
    let second = unsafe { fcb_saved_search_matching(u64::MAX, 1, std::ptr::null(), 0, 1, 1) };
    for pointer in [first, second] {
        let error = take(pointer);
        assert!(error.len() < 4096 && error.contains("\"status\":\"error\""));
        assert!(!error.contains("private matching query"));
    }
}

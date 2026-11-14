#![deny(unsafe_op_in_unsafe_fn)]

// Real C boundary/host/core coverage; offsets below are independent byte math.
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
fn final_window(text: &str, retained: u64) {
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    assert_eq!(field(text, "result_offset"), 4096);
    assert_eq!(field(text, "skipped_matches"), 4096);
    assert_eq!(field(text, "matches_seen"), 4100);
    assert_eq!(field(text, "retained_hits"), retained);
    assert!(text.contains("\"has_more_matches\":false"), "{text}");
    assert!(text.contains("\"next_match_offset\":null"), "{text}");
    assert!(text.contains("\"matches_seen_exact\":true"), "{text}");
}

#[test]
fn later_saved_windows_cross_the_retention_cap_and_keep_original_witnesses() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64, u64) -> *mut c_char = fcb_saved_search_page;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64, u64, u64) -> *mut c_char = fcb_saved_expression_search_page;
    let _: unsafe extern "C" fn(u64, u64, u64, u64, *const c_char, u64, u64, u64) -> *mut c_char =
        structure::fcb_saved_structure_search_page;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["fcb_saved_search_page(", "fcb_saved_expression_search_page(", "fcb_saved_structure_search_page("] {
        assert_eq!(header.matches(name).count(), 1);
    }
    let root = std::env::temp_dir().join(format!("fcb-saved-windows-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("saved.fcbs");
    let owner = ArenaOwnerId::new(88761).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let body = b"foo ".repeat(4100);
    let encoded = SnapshotBytes::encode(owner, true, "window-c", &[
        SnapshotEntry { path: b"many.rs", observed_bytes: body.len() as u64, data: SnapshotData::Captured(&body) },
    ], Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    std::fs::write(&path, encoded.bytes()).unwrap();
    let path = CString::new(path.to_str().unwrap()).unwrap();
    let needle = CString::new("foo").unwrap();
    let expression = CString::new("foo -absent").unwrap();
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));

    let first = take(unsafe { fcb_saved_search_page(handle, 1, needle.as_ptr(), 1, 0, 4096, 0) });
    assert_eq!(field(&first, "retained_hits"), 4096);
    assert_eq!(field(&first, "matches_seen"), 4097);
    assert_eq!(field(&first, "next_match_offset"), 4096);
    assert!(first.contains("\"has_more_matches\":true"));
    let last = take(unsafe { fcb_saved_search_page(handle, 2, needle.as_ptr(), 1, 1, 4096, 4096) });
    final_window(&last, 4);
    assert!(last.contains("\"truncated\":true"), "skipped prefix remains omitted rows");
    final_window(&take(fcb_saved_results(handle, 2, 0, 2)), 4);
    assert!(take(fcb_saved_results(handle, 1, 0, 2)).contains("\"status\":\"error\""));
    let literal_reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(literal_reader, 0);
    let opened = take(fcb_saved_open_hit_reader(handle, literal_reader, 2, 4));
    assert!(opened.contains("\"start\":\"16396\",\"end\":\"16399\""), "{opened}");
    assert!(opened.contains("\"original_hex\":\"666f6f\"") && opened.contains("\"live_source_reopened\":false"));

    let expression_last = take(unsafe {
        fcb_saved_expression_search_page(handle, 1, expression.as_ptr(), 1, 0, 4096, 100_000, 4096)
    });
    final_window(&expression_last, 4);
    let expression_reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(expression_reader, 0);
    let opened = take(fcb_saved_open_expression_hit_reader(handle, expression_reader, 1, 1));
    assert!(opened.contains("\"start\":\"16384\",\"end\":\"16387\""), "{opened}");
    assert!(opened.contains("\"selection_namespace\":\"saved-expression\""));

    let structure_last = take(unsafe {
        structure::fcb_saved_structure_search_page(handle, 1, 4, 0, needle.as_ptr(), 4096, 100_000, 4096)
    });
    final_window(&structure_last, 4);
    assert!(structure_last.contains("\"truncated\":false"), "exact structural tail has no later lookahead");
    let structure_reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(structure_reader, 0);
    let opened = take(structure::fcb_saved_open_structure_hit_reader(handle, structure_reader, 1, 4));
    assert!(opened.contains("\"start\":\"16396\",\"end\":\"16399\""), "{opened}");
    assert!(opened.contains("\"selection_namespace\":\"saved-structure\""));

    let beyond = take(unsafe { fcb_saved_search_page(handle, 3, needle.as_ptr(), 1, 1, 4096, u64::MAX) });
    assert_eq!(field(&beyond, "result_offset"), u64::MAX);
    assert_eq!(field(&beyond, "skipped_matches"), 4100);
    assert_eq!(field(&beyond, "retained_hits"), 0);
    assert!(beyond.contains("\"has_more_matches\":false"));
    let stale_reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(stale_reader, 0);
    assert!(take(fcb_saved_open_hit_reader(handle, stale_reader, 2, 1)).contains("\"status\":\"error\""));
    assert_eq!(crate::reader_ffi::fcb_reader_close(stale_reader), 1);
    assert!(take(unsafe { fcb_saved_search_page(handle, 4, needle.as_ptr(), 2, 1, 4096, 0) })
        .contains("\"status\":\"error\""));
    assert!(take(fcb_saved_results(handle, 3, 0, 64)).contains("\"status\":\"ok\""));
    assert!(take(unsafe { fcb_saved_search_page(handle, 5, needle.as_ptr(), 1, 0, 0, 4096) })
        .contains("\"status\":\"error\""), "noncounting zero retention must be refused by the saved host");
    assert!(take(fcb_saved_results(handle, 3, 0, 64)).contains("\"status\":\"ok\""));

    assert_eq!(fcb_saved_close(handle), 1);
    for reader in [literal_reader, expression_reader, structure_reader] {
        assert!(take(crate::reader_ffi::fcb_reader_copy_range(reader, 16384, 16387))
            .contains("\"original_hex\":\"666f6f\""));
        assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
    }
}

#[test]
fn window_entrypoints_reject_invalid_foreign_inputs_without_disclosing_query() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let needle = CString::new("private saved window query").unwrap();
    for pointer in [
        unsafe { fcb_saved_search_page(u64::MAX, 1, needle.as_ptr(), 1, 0, 4096, u64::MAX) },
        unsafe { fcb_saved_expression_search_page(u64::MAX, 1, needle.as_ptr(), 0, 1, 4096, 10, 1) },
        unsafe { structure::fcb_saved_structure_search_page(u64::MAX, 1, 4, 0, std::ptr::null(), 1, 10, 1) },
    ] {
        let error = take(pointer);
        assert!(error.len() < 4096 && error.contains("\"status\":\"error\""));
        assert!(!error.contains("private saved window query"));
    }
}

#![deny(unsafe_op_in_unsafe_fn)]
#![cfg(unix)]

use super::*;
use std::{ffi::{CStr, CString}, fs, sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a complete JSON or error handoff");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    text
}
fn source(bytes: &[u8]) -> (std::path::PathBuf, u64) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-selection-ffi-{}-{now}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::write(&path, bytes).unwrap();
    let label = CString::new(path.to_str().unwrap()).unwrap();
    let handle = fcb_reader_create(); assert_ne!(handle, 0);
    let reply = take(unsafe { fcb_reader_open(handle, label.as_ptr(), 4096) });
    assert!(reply.contains("\"status\":\"ok\""), "{reply}");
    (path, handle)
}

#[test]
fn c_resolves_the_presented_capture_after_working_tree_replacement() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let bytes = b"left chosen right";
    let (path, handle) = source(bytes);
    let page = take(fcb_reader_window(handle, 0, 64));
    assert!(page.contains("\"text\":\"left chosen right\""));
    fs::write(&path, b"new live source").unwrap();
    let reply = take(fcb_reader_resolve_selection(handle, 0, bytes.len() as u64, 5, 11));
    assert!(reply.contains("\"status\":\"ok\""), "{reply}");
    assert!(reply.contains(&format!("\"owner\":\"{handle}\"")));
    assert!(reply.contains("\"command\":\"resolve-selection\""));
    assert!(reply.contains("\"selection_namespace\":\"native-text\""));
    assert!(reply.contains("\"window_original_range\":{\"start\":\"0\",\"end\":\"17\"}"));
    assert!(reply.contains("\"window_utf8_range\":{\"start\":\"5\",\"end\":\"11\"}"));
    assert!(reply.contains("\"original_range\":{\"start\":\"5\",\"end\":\"11\"}"));
    assert!(reply.contains("\"original_hex\":\"63686f73656e\""));
    assert!(reply.contains("\"contains_replacements\":false"));
    assert!(reply.contains("\"additional_source_bytes_read\":\"0\""));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_utf16_decoded_selection_resolves_surrogates_and_combining_marks_to_original_units() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let bytes: Vec<u8> = "\u{feff}A🦀e\u{301}Z".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let (_, handle) = source(&bytes);
    let reply = take(fcb_reader_resolve_selection(handle, 2, bytes.len() as u64, 1, 8));
    assert!(reply.contains("\"status\":\"ok\""), "{reply}");
    assert!(reply.contains("\"encoding\":\"utf16be\""));
    assert!(reply.contains("\"original_range\":{\"start\":\"4\",\"end\":\"12\"}"));
    assert!(reply.contains("\"text\":\"🦀e\u{301}\""));
    assert!(reply.contains("\"original_hex\":\"d83edd8000650301\""));
    assert!(reply.contains("\"contains_replacements\":false"));
    assert!(take(fcb_reader_resolve_selection(handle, 2, bytes.len() as u64, 2, 8))
        .contains("\"status\":\"error\""));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_malformed_text_selection_returns_original_hex_and_selected_replacement_provenance() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, handle) = source(b"x\xffy");
    let reply = take(fcb_reader_resolve_selection(handle, 0, 3, 1, 4));
    assert!(reply.contains("\"status\":\"ok\""), "{reply}");
    assert!(reply.contains("\"original_hex\":\"ff\""));
    assert!(reply.contains("\"text\":\"\u{fffd}\""));
    assert!(reply.contains("\"contains_replacements\":true"));
    let ordinary = take(fcb_reader_resolve_selection(handle, 0, 3, 4, 5));
    assert!(ordinary.contains("\"text\":\"y\""));
    assert!(ordinary.contains("\"contains_replacements\":false"));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_resolver_refuses_invalid_ranges_unknown_and_empty_handles_without_changing_capture() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let empty = fcb_reader_create(); assert_ne!(empty, 0);
    assert!(take(fcb_reader_resolve_selection(empty, 0, 1, 0, 1)).contains("READER_HANDLE_NOT_OPEN"));
    assert_eq!(fcb_reader_close(empty), 1);
    assert!(take(fcb_reader_resolve_selection(empty, 0, 1, 0, 1)).contains("READER_HANDLE_UNKNOWN"));
    let (_, handle) = source(b"abc");
    for (first, last, start, end) in [(0, 3, 1, 1), (0, 3, 2, 1),
        (0, 4, 0, 1), (0, 3, 0, u64::MAX), (u64::MAX, u64::MAX, 0, 1)] {
        assert!(take(fcb_reader_resolve_selection(handle, first, last, start, end))
            .contains("\"status\":\"error\""));
    }
    assert!(take(fcb_reader_resolve_selection(handle, 0, 3, 0, 3)).contains("\"text\":\"abc\""));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_selection_json_owns_its_bytes_after_reader_close_and_matches_header_signature() {
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_reader_resolve_selection;
    let header = include_str!("../include/fcb_reader_sessions.h");
    assert!(header.contains("char *fcb_reader_resolve_selection(uint64_t handle, uint64_t window_start,"));
    assert!(header.contains("#define FCB_READER_MAX_SELECTION_WINDOW_BYTES UINT64_C(262160)"));
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, handle) = source(b"retained");
    assert_eq!(fcb_reader_cancel(handle), 1);
    let reply = fcb_reader_resolve_selection(handle, 0, 8, 0, 8);
    assert_eq!(fcb_reader_close(handle), 1);
    assert!(take(reply).contains("\"text\":\"retained\""));
}

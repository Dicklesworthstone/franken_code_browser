#![deny(unsafe_op_in_unsafe_fn)]
#![cfg(unix)]

use super::*;
use std::{ffi::{CStr, CString}, fs, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a complete JSON or error handoff");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    text
}
fn file(bytes: &[u8]) -> (std::path::PathBuf, CString) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-reader-ffi-{}-{now}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::write(&path, bytes).unwrap();
    let cpath = CString::new(path.to_str().unwrap()).unwrap(); (path, cpath)
}
fn open(path: &CString) -> u64 {
    let handle = fcb_reader_create(); assert_ne!(handle, 0);
    let response = take(unsafe { fcb_reader_open(handle, path.as_ptr(), 4 * 1024 * 1024) });
    assert!(response.contains("\"status\":\"ok\""), "{response}");
    assert!(response.contains(&format!("\"owner\":\"{handle}\"")));
    handle
}

#[test]
fn c_large_file_capture_pages_finds_and_forks_without_widening_foreign_transfers() {
    use fcb_app::host::{MAX_HOST_TEXT_BYTES, reader::MAX_READER_SOURCE_BYTES};
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    assert_eq!(MAX_HOST_TEXT_BYTES, 4 * 1024 * 1024);
    assert_eq!(MAX_READER_SOURCE_BYTES, 32 * 1024 * 1024);
    let mut bytes = vec![b'x'; MAX_HOST_TEXT_BYTES + 16];
    bytes.extend_from_slice(b"\r\nneedle\r\n");
    let start = (bytes.len() - 8) as u64;
    let (path, cpath) = file(&bytes);
    let handle = fcb_reader_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_reader_open(handle, cpath.as_ptr(), MAX_READER_SOURCE_BYTES as u64 + 1) })
        .contains("\"status\":\"error\""));
    assert!(take(fcb_reader_info(handle)).contains("READER_HANDLE_NOT_OPEN"));
    let info = take(unsafe { fcb_reader_open(handle, cpath.as_ptr(), MAX_READER_SOURCE_BYTES as u64) });
    assert!(info.contains("\"status\":\"ok\""));
    assert!(info.contains(&format!("\"captured_bytes\":\"{}\"", bytes.len())));
    fs::write(&path, b"replacement").unwrap();
    let line = take(fcb_reader_lines(handle, 2, 1, 64));
    assert!(line.contains("\"text\":\"needle\\r\\n\""));
    assert!(line.len() < 4096, "the whole source escaped through a bounded page");
    let needle = CString::new("needle").unwrap();
    let found = take(unsafe { fcb_reader_find(handle, 1, needle.as_ptr(), 1, bytes.len() as u64) });
    assert!(found.contains("\"search_complete\":true"));
    assert!(found.contains(&format!("\"original_range\":{{\"start\":\"{start}\",\"end\":\"{}\"}}", start + 6)));
    assert!(take(fcb_reader_copy_hit(handle, 1, 0)).contains("\"original_hex\":\"6e6565646c65\""));
    let pin = fcb_reader_create(); assert_ne!(pin, 0);
    // A live-reader allowance is not permission to copy a bigger foreign buffer.
    assert!(take(unsafe { supplied::fcb_reader_open_bytes(pin, cpath.as_ptr(),
        bytes.as_ptr(), bytes.len() as u64) }).contains("READER_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_reader_info(pin)).contains("READER_HANDLE_NOT_OPEN"));
    assert!(take(supplied::fcb_reader_fork(handle, pin)).contains("\"status\":\"ok\""));
    assert!(take(comparison::fcb_reader_compare(handle, pin, 1, 256, 1_000_000)).contains("COMPARISON_LIMIT"));
    assert_eq!(fcb_reader_close(handle), 1);
    let retained = take(fcb_reader_window(pin, start, 64));
    assert!(retained.contains("\"text\":\"needle\\r\\n\""));
    assert!(retained.contains("\"additional_source_bytes_read\":\"0\""));
    assert_eq!(fcb_reader_close(pin), 1);
}

#[test]
fn c_open_find_hit_copy_stays_on_the_original_source_after_disk_changes() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (path, cpath) = file(b"before needle after\r\n");
    let handle = open(&cpath);
    fs::write(&path, b"different working tree").unwrap();
    let found = {
        let needle = CString::new("needle").unwrap();
        take(unsafe { fcb_reader_find(handle, 1, needle.as_ptr(), 10, 4096) })
    }; // The foreign query string does not back any retained match.
    assert!(found.contains("\"retained_hits\":\"1\""));
    let window = take(fcb_reader_hit(handle, 1, 0, 256));
    assert!(window.contains("before needle after\\r\\n"));
    let copy = take(fcb_reader_copy_hit(handle, 1, 0));
    assert!(copy.contains("\"original_hex\":\"6e6565646c65\""));
    assert_eq!(fcb_reader_close(handle), 1);
    let stale = take(fcb_reader_window(handle, 0, 100));
    assert!(stale.contains("READER_HANDLE_UNKNOWN")); assert_eq!(fcb_reader_close(handle), 0);
}

#[test]
fn c_windows_accept_utf16_and_preserve_embedded_nul_as_json() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let utf16: Vec<u8> = "\u{feff}first\r\n🦀 needle\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let (_, path) = file(&utf16); let handle = open(&path);
    let lines = take(fcb_reader_lines(handle, 2, 1, 1024));
    assert!(lines.contains("🦀 needle\\n")); assert!(lines.contains("\"encoding\":\"utf16le\""));
    assert_eq!(fcb_reader_close(handle), 1);
    let (_, path) = file(b"a\0b"); let handle = open(&path);
    assert!(take(fcb_reader_window(handle, 0, 100)).contains("a\\u0000b"));
    assert!(take(fcb_reader_copy_range(handle, 0, 3)).contains("610062"));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_generation_validation_never_selects_a_different_querys_row() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, path) = file(b"aaa bbb"); let handle = open(&path);
    let a = CString::new("aaa").unwrap(); let b = CString::new("bbb").unwrap();
    take(unsafe { fcb_reader_find(handle, 1, a.as_ptr(), 10, 100) });
    take(unsafe { fcb_reader_find(handle, 2, b.as_ptr(), 10, 100) });
    assert!(take(fcb_reader_copy_hit(handle, 1, 0)).contains("READER_SESSION_STALE_QUERY"));
    assert!(take(fcb_reader_copy_hit(handle, 2, 0)).contains("626262"));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_find_options_count_beyond_rows_and_keep_original_utf16_witnesses() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let bytes: Vec<u8> = "\u{feff}K K k\r\n".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let (path, cpath) = file(&bytes);
    let handle = open(&cpath);
    fs::write(path, b"changed disk content").unwrap();
    let needle = CString::new("k").unwrap();
    let counted = take(unsafe { fcb_reader_find_with_options(handle, 1, needle.as_ptr(), 1, bytes.len() as u64, 3) });
    assert!(counted.contains("\"mode\":\"unicode-lowercase-literal\""), "{counted}");
    assert!(counted.contains("\"search_complete\":true"));
    assert!(counted.contains("\"match_count_complete\":true"));
    assert!(counted.contains("\"rows_truncated\":true"));
    assert!(counted.contains("\"matches_seen\":\"3\""));
    assert!(counted.contains("\"retained_hits\":\"1\""));
    assert!(counted.contains("\"matched_text\":\"K\""));
    assert!(counted.contains("\"original_hex\":\"212a\""));
    assert!(counted.contains("\"original_range\":{\"start\":\"2\",\"end\":\"4\"}"));
    let hit = take(fcb_reader_hit(handle, 1, 0, 64));
    assert!(hit.contains("K K k\\r\\n"));
    assert!(hit.contains("\"additional_source_bytes_read\":\"0\""));
    assert!(take(fcb_reader_copy_hit(handle, 1, 0)).contains("\"original_hex\":\"212a\""));
    // Failed flag admission must not replace an accepted generation.
    let invalid = take(unsafe { fcb_reader_find_with_options(handle, 2, needle.as_ptr(), 1, 100, 4) });
    assert!(invalid.contains("READER_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_reader_copy_hit(handle, 1, 0)).contains("212a"));
    let bounded = take(unsafe { fcb_reader_find_with_options(handle, 2, needle.as_ptr(), 1, 100, 1) });
    assert!(bounded.contains("\"state\":\"match-limit\""));
    assert!(bounded.contains("\"matches_seen\":\"2\""));
    assert!(bounded.contains("\"match_count_complete\":false"));
    assert!(take(fcb_reader_copy_hit(handle, 1, 0)).contains("READER_SESSION_STALE_QUERY"));
    let zero = take(unsafe { fcb_reader_find_with_options(handle, 3, needle.as_ptr(), 0, 100, 3) });
    assert!(zero.contains("\"matches_seen\":\"3\""));
    assert!(zero.contains("\"retained_hits\":\"0\""));
    assert!(zero.contains("\"search_complete\":true"));
    assert!(take(fcb_reader_copy_hit(handle, 3, 0)).contains("READER_SESSION_MISSING_HIT"));
    assert!(take(unsafe { fcb_reader_find_with_options(handle, 4, std::ptr::null(), 1, 100, 3) })
        .contains("READER_HANDLE_INVALID_ARGUMENT"));
    assert_eq!(fcb_reader_close(handle), 1);
    assert!(take(unsafe { fcb_reader_find_with_options(handle, 4, needle.as_ptr(), 1, 100, 3) })
        .contains("READER_HANDLE_UNKNOWN"));
}

#[test]
fn c_typed_positions_keep_utf16_coordinates_on_the_retained_capture() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let bytes: Vec<u8> = "\u{feff}x\r\nA🦀e\u{301}\tZ\r\n"
        .encode_utf16().flat_map(u16::to_le_bytes).collect();
    let (path, cpath) = file(&bytes);
    let handle = open(&cpath);
    fs::write(&path, b"a changed live file").unwrap();
    for (kind, column, label) in [(1, 3, "unicode_scalar"),
        (2, 4, "utf16_code_unit"), (3, 6, "decoded_utf8_byte")] {
        let page = take(fcb_reader_position(handle, 2, column, kind, 64));
        assert!(page.contains("\"command\":\"position\""), "{page}");
        assert!(page.contains("\"selection\":null"), "a position must not grant query authority");
        assert!(page.contains(&format!("\"column_kind\":\"{label}\"")));
        assert!(page.contains(&format!("\"column\":\"{column}\"")));
        assert!(page.contains("\"original_offset\":\"14\""), "{page}");
        assert!(page.contains("\"encoding\":\"utf16le\""));
        assert!(page.contains("\"additional_source_bytes_read\":\"0\""));
    }
    let end = take(fcb_reader_position(handle, 2, 7, 1, 64));
    assert!(end.contains("\"original_offset\":\"22\""), "{end}");
    for (line, column, kind) in [(2, 3, 2), (2, 3, 3), (2, 8, 1),
        (3, 1, 1), (0, 1, 1), (1, 0, 1), (1, 1, 0), (1, 1, u64::MAX)] {
        let refused = take(fcb_reader_position(handle, line, column, kind, 64));
        assert!(refused.contains("\"status\":\"error\""), "{refused}");
    }
    assert!(take(fcb_reader_window(handle, 0, 64)).contains("A🦀e"));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_typed_position_reaches_a_far_column_with_a_bounded_page() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let mut bytes = vec![b'x'; 120_000];
    bytes.extend_from_slice("🦀tail\n".as_bytes());
    let (_, path) = file(&bytes);
    let handle = open(&path);
    let page = take(fcb_reader_position(handle, 1, 120_002, 1, 256));
    assert!(page.contains("\"status\":\"ok\""), "{page}");
    assert!(page.contains("\"original_offset\":\"120004\""), "{page}");
    assert!(page.contains("tail"));
    assert!(page.len() < 12 * 1024, "far navigation leaked an unbounded source transfer");
    assert_eq!(fcb_reader_close(handle), 1);
    assert!(take(fcb_reader_position(handle, 1, 1, 1, 256)).contains("READER_HANDLE_UNKNOWN"));
}

#[test]
fn c_null_unknown_and_wide_integer_inputs_return_errors_without_panicking() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let handle = fcb_reader_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_reader_open(handle, std::ptr::null(), 100) }).contains("READER_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_reader_info(handle)).contains("READER_HANDLE_NOT_OPEN"));
    let (_, path) = file(b"needle"); take(unsafe { fcb_reader_open(handle, path.as_ptr(), 100) });
    assert!(take(fcb_reader_window(handle, u64::MAX, 100)).contains("\"status\":\"error\""));
    assert!(take(fcb_reader_lines(handle, 1, u64::MAX, 100)).contains("\"status\":\"error\""));
    assert!(take(unsafe { fcb_reader_find(handle, 1, std::ptr::null(), 10, 100) }).contains("READER_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_reader_info(u64::MAX)).contains("READER_HANDLE_UNKNOWN"));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_cancellation_keeps_the_capture_available_and_strings_outlive_close() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, path) = file(b"retained"); let handle = open(&path);
    assert_eq!(fcb_reader_cancel(handle), 1);
    let pointer = fcb_reader_window(handle, 0, 100);
    assert_eq!(fcb_reader_close(handle), 1);
    // Closing a session never frees a JSON string already handed to its host.
    assert!(take(pointer).contains("retained"));
    assert_eq!(fcb_reader_cancel(handle), 0);
}

#[test]
fn c_symbol_function_types_match_the_public_header() {
    let _: extern "C" fn() -> u64 = fcb_reader_create;
    let _: unsafe extern "C" fn(u64, *const c_char, u64) -> *mut c_char = fcb_reader_open;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_reader_info;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_reader_window;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_lines;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_reader_position;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64) -> *mut c_char = fcb_reader_find;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64) -> *mut c_char = fcb_reader_find_with_options;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_hit;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_reader_copy_range;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_reader_copy_hit;
    let _: extern "C" fn(u64) -> u8 = fcb_reader_cancel;
    let _: extern "C" fn(u64) -> u8 = fcb_reader_close;
}

#![deny(unsafe_op_in_unsafe_fn)]
#![cfg(unix)]

use super::*;
use std::{ffi::{CStr, CString}, fs, sync::{Mutex, atomic::{AtomicU64, Ordering}}, time::{SystemTime, UNIX_EPOCH}};

// Serialize only these tests' use of the one C registry; ordinary registry
// consumer tests have independent, intentionally concurrent registry owners.
static TEST_LOCK: Mutex<()> = Mutex::new(());
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
fn c_open_find_hit_copy_stays_on_the_original_source_after_disk_changes() {
    let _guard = TEST_LOCK.lock().unwrap();
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
    let _guard = TEST_LOCK.lock().unwrap();
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
    let _guard = TEST_LOCK.lock().unwrap();
    let (_, path) = file(b"aaa bbb"); let handle = open(&path);
    let a = CString::new("aaa").unwrap(); let b = CString::new("bbb").unwrap();
    take(unsafe { fcb_reader_find(handle, 1, a.as_ptr(), 10, 100) });
    take(unsafe { fcb_reader_find(handle, 2, b.as_ptr(), 10, 100) });
    assert!(take(fcb_reader_copy_hit(handle, 1, 0)).contains("READER_SESSION_STALE_QUERY"));
    assert!(take(fcb_reader_copy_hit(handle, 2, 0)).contains("626262"));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_null_unknown_and_wide_integer_inputs_return_errors_without_panicking() {
    let _guard = TEST_LOCK.lock().unwrap();
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
    let _guard = TEST_LOCK.lock().unwrap();
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
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64) -> *mut c_char = fcb_reader_find;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_hit;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_reader_copy_range;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_reader_copy_hit;
    let _: extern "C" fn(u64) -> u8 = fcb_reader_cancel;
    let _: extern "C" fn(u64) -> u8 = fcb_reader_close;
}

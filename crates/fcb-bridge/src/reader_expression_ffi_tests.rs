#![deny(unsafe_op_in_unsafe_fn)]
#![cfg(unix)]

//! Real exported C entry points, shared reader registry and exact source bytes.
use super::*;
use std::{ffi::{CStr, CString}, fs, sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected JSON response");
    let response = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    response
}
fn open(bytes: &[u8]) -> (std::path::PathBuf, u64) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-reader-expression-{}-{now}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::write(&path, bytes).unwrap();
    let name = CString::new(path.to_str().unwrap()).unwrap();
    let handle = fcb_reader_create(); assert_ne!(handle, 0);
    let info = take(unsafe { fcb_reader_open(handle, name.as_ptr(), 1024 * 1024) });
    assert!(info.contains("\"status\":\"ok\""), "{info}");
    (path, handle)
}
fn find(handle: u64, generation: u64, raw: &str, skip: u64, rows: u64,
    bytes: u64, flags: u64) -> String {
    let raw = CString::new(raw).unwrap();
    take(unsafe { fcb_reader_find_expression_page(handle, generation, raw.as_ptr(),
        skip, rows, bytes, flags) })
}

#[test]
fn exported_expression_pages_preserve_capture_witnesses_and_literal_flags() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let bytes: Vec<u8> = "\u{feff}bananana PERMIT".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let (path, handle) = open(&bytes);
    fs::write(&path, b"replaced live file").unwrap();
    let first = find(handle, 1, "ana permit -blocked", 0, 1, 1024, 3);
    assert!(first.contains("\"status\":\"ok\""), "{first}");
    assert!(first.contains("\"mode\":\"decoded-text-expression\""));
    assert!(first.contains("\"requested_mode\":\"expression\""));
    assert!(first.contains("\"predicate_scope\":\"retained-source-capture\""));
    assert!(first.contains("\"expression_qualified\":true"));
    assert!(first.contains("\"matches_seen\":\"3\""));
    assert!(first.contains("\"matched_text\":\"ana\""));
    assert!(first.contains("\"has_more_matches\":true"));
    assert!(first.contains("\"original_hex\":\"61006e006100\""));
    let second = find(handle, 2, "ana permit -blocked", 2, 1, 1024, 3);
    assert!(second.contains("\"occurrence_id\":\"3\""));
    assert!(second.contains("\"page_start_after\":\"2\""));
    assert!(second.contains("\"skipped_matches\":\"2\""));
    assert!(second.contains("\"has_more_matches\":false"));
    // The temporary foreign query allocation is already gone.
    let copy = take(fcb_reader_copy_hit(handle, 2, 0));
    assert!(copy.contains("\"original_hex\":\"61006e006100\""), "{copy}");
    assert!(take(fcb_reader_hit(handle, 2, 0, 64))
        .contains("\"additional_source_bytes_read\":\"0\""));
    assert!(take(fcb_reader_copy_hit(handle, 1, 0)).contains("READER_SESSION_STALE_QUERY"));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn exported_expression_failure_and_pointer_admission_preserve_accepted_hits() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, handle) = open(b"banana permit");
    let literal = CString::new("ana").unwrap();
    assert!(take(unsafe { fcb_reader_find_page(handle, 1, literal.as_ptr(), 0, 1, 64, 2) })
        .contains("\"retained_hits\":\"1\""));
    for (index, (raw, code)) in [
        ("ana path:src/", "READER_SESSION_EXPRESSION_SCOPE_UNSUPPORTED"),
        ("ana type:txt", "READER_SESSION_EXPRESSION_SCOPE_UNSUPPORTED"),
        ("\"ana", "QUERY_SYNTAX_ERROR"),
        ("regex:a.*", "QUERY_REGEX_UNQUALIFIED"),
        ("", "QUERY_EMPTY"),
    ].into_iter().enumerate() {
        let error = find(handle, index as u64 + 2, raw, 0, 1, 64, 2);
        assert!(error.contains(code), "{error}");
        assert!(take(fcb_reader_copy_hit(handle, 1, 0)).contains("\"original_hex\":\"616e61\""));
    }
    let null = take(unsafe { fcb_reader_find_expression_page(handle, 20,
        std::ptr::null(), 0, 1, 64, 0) });
    assert!(null.contains("READER_HANDLE_INVALID_ARGUMENT"));
    let invalid = [0xff_u8, 0];
    let utf8 = take(unsafe { fcb_reader_find_expression_page(handle, 20,
        invalid.as_ptr().cast(), 0, 1, 64, 0) });
    assert!(utf8.contains("READER_HANDLE_INVALID_ARGUMENT"));
    assert!(find(handle, 20, "ana permit", 0, 1, 64, 4)
        .contains("READER_HANDLE_INVALID_ARGUMENT"));
    assert!(find(handle, 20, "ana permit", u64::MAX, 1, 64, 2)
        .contains("READER_SESSION_INVALID_LIMITS"));
    assert!(find(handle, 20, "ana permit", 0, u64::MAX, 64, 2)
        .contains("\"status\":\"error\""));
    assert!(take(fcb_reader_copy_hit(handle, 1, 0)).contains("\"original_hex\":\"616e61\""));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn exported_expression_incomplete_false_and_independent_capture_results_are_distinct() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, one) = open(b"banana permit blocked");
    let (_, two) = open(b"banana permit");
    let partial = find(one, 1, "ana permit -blocked", 0, 1, 12, 2);
    assert!(partial.contains("\"expression_qualified\":null"), "{partial}");
    assert!(partial.contains("\"search_complete\":false"));
    assert!(partial.contains("\"matches_seen\":\"0\""));
    assert!(partial.contains("\"hits\":[]"));
    let rejected = find(one, 2, "ana permit -blocked", 0, 1, 64, 2);
    assert!(rejected.contains("\"expression_qualified\":false"));
    assert!(rejected.contains("\"search_complete\":true"));
    let accepted = find(two, 1, "ana permit -blocked", 0, 1, 64, 0);
    assert!(accepted.contains("\"expression_qualified\":true"));
    assert!(accepted.contains("\"count_all_matches\":false"));
    assert!(accepted.contains("\"match_count_complete\":true"));
    assert!(accepted.contains("\"matches_seen\":\"2\""));
    assert_eq!(fcb_reader_close(one), 1);
    assert!(take(fcb_reader_copy_hit(two, 1, 0)).contains("\"original_hex\":\"616e61\""));
    assert_eq!(fcb_reader_close(two), 1);
}

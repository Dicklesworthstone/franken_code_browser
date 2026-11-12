#![deny(unsafe_op_in_unsafe_fn)]

//! The real additive C selection entry point, not a fabricated wire fixture.
use super::*;
use std::{ffi::{CStr, CString}, fs, sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH}};
use crate::reader_ffi::FFI_TEST_LOCK;

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    result
}
fn root(bytes: &[u8]) -> (std::path::PathBuf, CString) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fcb-observed-selection-ffi-{}-{}-{}",
        std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap();
    fs::write(path.join("source"), bytes).unwrap();
    let cpath = CString::new(path.to_str().unwrap()).unwrap();
    (path, cpath)
}
fn open(root: &CString) -> u64 {
    let handle = fcb_observed_reader_create();
    assert_ne!(handle, 0);
    let path = CString::new("736f75726365").unwrap();
    let response = take(unsafe { fcb_observed_reader_open(handle, root.as_ptr(), path.as_ptr(),
        None, std::ptr::null_mut()) });
    assert!(response.contains("\"status\":\"ok\""), "{response}");
    handle
}
fn window(handle: u64, encoding: u64) -> String {
    take(unsafe { fcb_observed_reader_window(handle, 0, 128, encoding, None, std::ptr::null_mut()) })
}

#[test]
fn c_selection_uses_the_retained_utf16_observation_and_reply_survives_close() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let bytes: Vec<u8> = "\u{feff}A🦀B\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let (path, cpath) = root(&bytes);
    let handle = open(&cpath);
    assert!(window(handle, 2).contains("\"text\":\"A🦀B\\r\\n\""));
    fs::write(path.join("source"), b"now different").unwrap();
    let reply = fcb_observed_reader_resolve_selection(handle, 1, 1, 5);
    assert_eq!(fcb_observed_reader_close(handle), 1);
    let selected = take(reply);
    for expected in [
        "\"status\":\"ok\"", "\"command\":\"resolve-selection\"",
        "\"selection_namespace\":\"native-text\"", "\"encoding\":\"utf16le\"",
        "\"observation\":\"1\"", "\"whole_file_immutable\":false",
        "\"window_original_range\":{\"start\":\"2\",\"end\":\"14\"}",
        "\"window_utf8_range\":{\"start\":\"1\",\"end\":\"5\"}",
        "\"original_range\":{\"start\":\"4\",\"end\":\"8\"}",
        "\"text\":\"🦀\"", "\"original_hex\":\"3ed880dd\"",
        "\"contains_replacements\":false", "\"additional_source_bytes_read\":\"0\"",
        "\"additional_read_calls\":\"0\"",
    ] { assert!(selected.contains(expected), "missing {expected}: {selected}"); }
    assert!(take(fcb_observed_reader_resolve_selection(handle, 1, 1, 5))
        .contains("OBSERVED_READER_HANDLE_UNKNOWN"));
}

#[test]
fn c_selection_rejects_unavailable_ids_and_invalid_utf8_boundaries_without_evicting() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_path, cpath) = root("A🦀B".as_bytes());
    let handle = open(&cpath);
    assert!(take(fcb_observed_reader_resolve_selection(handle, 1, 1, 5))
        .contains("OBSERVED_READER_OBSERVATION_UNAVAILABLE"));
    assert!(window(handle, 1).contains("\"status\":\"ok\""));
    for (start, end) in [(0, 0), (5, 1), (2, 5), (1, 4), (0, 100), (u64::MAX - 1, u64::MAX)] {
        let rejected = take(fcb_observed_reader_resolve_selection(handle, 1, start, end));
        assert!(rejected.contains("\"status\":\"error\""), "{rejected}");
    }
    assert!(take(fcb_observed_reader_resolve_selection(handle, 0, 0, 1))
        .contains("OBSERVED_READER_OBSERVATION_UNAVAILABLE"));
    let unopened = fcb_observed_reader_create();
    assert_ne!(unopened, 0);
    assert!(take(fcb_observed_reader_resolve_selection(unopened, 1, 1, 5))
        .contains("OBSERVED_READER_HANDLE_NOT_OPEN"));
    assert_eq!(fcb_observed_reader_close(unopened), 1);
    assert_eq!(fcb_observed_reader_cancel(handle), 1);
    let selected = take(fcb_observed_reader_resolve_selection(handle, 1, 1, 5));
    assert!(selected.contains("\"text\":\"🦀\""));
    assert!(take(fcb_observed_reader_info(handle)).contains("\"retained_observations\":[\"1\"]"));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

#[test]
fn c_selection_discloses_replacement_text_and_keeps_the_malformed_bytes() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_path, cpath) = root(b"A\xffB");
    let handle = open(&cpath);
    assert!(window(handle, 1).contains("\"status\":\"ok\""));
    let selected = take(fcb_observed_reader_resolve_selection(handle, 1, 1, 4));
    assert!(selected.contains("\"text\":\"�\""), "{selected}");
    assert!(selected.contains("\"original_hex\":\"ff\""));
    assert!(selected.contains("\"contains_replacements\":true"));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

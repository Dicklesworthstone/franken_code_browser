use super::*;
use std::ffi::{CString, CStr};
use crate::reader_ffi::{fcb_reader_create, fcb_reader_close, supplied::fcb_reader_open_bytes, FFI_TEST_LOCK};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    result
}

#[test]
fn exported_pair_api_preserves_both_roles_and_refuses_stale_or_invalid_calls() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let before = fcb_reader_create(); let after = fcb_reader_create();
    assert_ne!(before, 0); assert_ne!(after, 0);
    let label = CString::new("retained.bin").unwrap();
    for (handle, bytes) in [(before, &b"old\0\xff"[..]), (after, &b"new\0\xfe"[..])] {
        let info = take(unsafe { fcb_reader_open_bytes(handle, label.as_ptr(), bytes.as_ptr(), bytes.len() as u64) });
        assert!(info.contains("\"status\":\"ok\""));
    }
    let compared = take(fcb_reader_compare(before, after, 1, 256, 1_000_000));
    assert!(compared.contains("\"command\":\"compare\""));
    assert!(compared.contains(&format!("\"before_source\":{{\"owner\":\"{before}\"")));
    assert!(compared.contains(&format!("\"after_source\":{{\"owner\":\"{after}\"")));
    assert!(compared.contains("\"comparison_complete\":true"));
    assert!(take(fcb_reader_compare(before, after, 1, 256, 1_000_000)).contains("COMPARISON_STALE"));
    assert!(take(fcb_reader_compare(before, after, 2, u64::MAX, 1_000_000)).contains("\"status\":\"error\""));
    assert!(take(fcb_reader_compare(before, after, 2, 256, 0)).contains("\"comparison_quality\":\"work-limit\""));
    assert_eq!(fcb_reader_close(after), 1);
    assert!(take(fcb_reader_compare(before, after, 3, 256, 1_000_000)).contains("READER_HANDLE_UNKNOWN"));
    assert_eq!(fcb_reader_close(before), 1);
}

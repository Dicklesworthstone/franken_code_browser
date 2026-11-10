use super::*;
use std::ffi::CStr;

fn text(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    // Production functions return a live NUL-terminated allocation. The same
    // library frees it exactly once after the bounded assertion copy.
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { super::super::fcb_free_string(pointer) };
    result
}

#[test]
fn numeric_city_abi_requires_presentation_and_preserves_full_width_ids() {
    let handle = fcb_city_create(); assert_ne!(handle, 0);
    let id = (1u64 << 53) + 11;
    let rect = [0.0, 0.0, 100.0, 100.0];
    let bytes = 1024u64; let known = 1u8;
    let invalid = text(unsafe { fcb_city_open(handle, 1, std::ptr::null(), rect.as_ptr(), &bytes, &known, 1, 1) });
    assert!(invalid.contains("CITY_HANDLE_INVALID_ARGUMENT"));
    let over_limit = text(unsafe { fcb_city_open(handle, 1, &id, rect.as_ptr(), &bytes, &known, 20_001, 1) });
    assert!(over_limit.contains("CITY_HANDLE_INVALID_ARGUMENT"));
    let opened = text(unsafe { fcb_city_open(handle, 1, &id, rect.as_ptr(), &bytes, &known, 1, 1) });
    assert!(opened.contains("\"schema\":\"fcb.city/1\""));
    assert!(opened.contains(&format!("\"file_id\":\"{id}\"")));
    assert_eq!(text(fcb_city_info(handle)), opened);
    let view = text(fcb_city_view(handle, 1, 100.0, 100.0, 50.0, 50.0, 1.0, 0.0, 0.0));
    assert!(view.contains("\"matrix\":["));
    assert!(text(fcb_city_pick(handle, 1, 1, 50.0, 50.0)).contains("CITY_SESSION_MISSING_VIEW"));
    assert!(text(fcb_city_present(handle, 1, 1, 1)).contains("\"acknowledgment\":\"host-declared\""));
    let picked = text(fcb_city_pick(handle, 1, 1, 50.0, 50.0));
    assert!(picked.contains(&format!("\"file_id\":\"{id}\"")));
    assert!(picked.contains("\"source_payload_read\":false"));
    text(fcb_city_pan(handle, 2, 1, 500.0, 0.0));
    assert_eq!(text(fcb_city_pick(handle, 1, 1, 50.0, 50.0)), picked);
    assert_eq!(fcb_city_cancel(handle), 1);
    assert!(text(fcb_city_present(handle, 2, 2, 1)).contains("CITY_SESSION_MISSING_VIEW"));
    assert_eq!(text(fcb_city_pick(handle, 1, 1, 50.0, 50.0)), picked);
    assert_eq!(fcb_city_close(handle), 1); assert_eq!(fcb_city_close(handle), 0);
    assert!(text(fcb_city_info(handle)).contains("CITY_HANDLE_UNKNOWN"));
}

#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::ffi::CStr;
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}

#[test]
fn index_construction_function_types_and_header_declarations_agree() {
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_index_begin;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_index_step;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_index_progress;
    let header = include_str!("../include/fcb_atlas_index.h");
    for name in ["fcb_atlas_index_begin(", "fcb_atlas_index_step(", "fcb_atlas_index_progress("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}

#[test]
fn unknown_or_wide_construction_requests_return_independently_owned_errors() {
    // No shared global slots are consumed; real success/lifecycle tests use
    // independent production registries rather than a second simulated engine.
    let responses = [fcb_atlas_index_begin(u64::MAX, 1, 10, 1024, 65536, 1000),
        fcb_atlas_index_step(u64::MAX, 1), fcb_atlas_index_progress(u64::MAX, 1),
        fcb_atlas_index_begin(u64::MAX, u64::MAX, u64::MAX, u64::MAX, u64::MAX, u64::MAX)];
    for response in responses {
        let error = take(response);
        assert!(error.contains("\"status\":\"error\"")); assert!(error.len() < 4096);
        assert!(!error.contains("\"status\":\"ok\""));
    }
}

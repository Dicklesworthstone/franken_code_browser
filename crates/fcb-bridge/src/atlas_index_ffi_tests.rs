#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::ffi::{CStr, CString};
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}

#[test]
fn all_five_index_entrypoint_types_and_header_declarations_agree() {
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_index_prepare;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_atlas_index_info;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_index_clear;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, u64, u64) -> *mut c_char = fcb_atlas_search_indexed;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, u64, u64) -> *mut c_char = fcb_atlas_search_indexed_begin;
    let header = include_str!("../include/fcb_atlas_index.h");
    for name in ["fcb_atlas_index_prepare(", "fcb_atlas_index_info(",
        "fcb_atlas_index_clear(", "fcb_atlas_search_indexed(", "fcb_atlas_search_indexed_begin("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}

#[test]
fn unknown_index_handles_return_bounded_independently_owned_error_strings() {
    let first = fcb_atlas_index_info(u64::MAX);
    let second = fcb_atlas_index_clear(u64::MAX, 1);
    for pointer in [first, second, fcb_atlas_index_prepare(u64::MAX, 1, 100, 1024, 65536, 1024)] {
        let error = take(pointer);
        assert!(error.contains("\"status\":\"error\""));
        assert!(error.len() < 4096);
    }
}

#[test]
fn null_and_valid_foreign_queries_on_unknown_handles_never_produce_success_or_source_payloads() {
    // Registry workflows exercise successful admission/step/activation without
    // competing for process-global C handle capacity during parallel tests.
    for call in [fcb_atlas_search_indexed, fcb_atlas_search_indexed_begin] {
        let error = take(unsafe { call(u64::MAX, 1, 1, std::ptr::null(), 10, 1000) });
        assert!(error.contains("\"status\":\"error\""));
        let query = CString::new("private fixture phrase").unwrap();
        let error = take(unsafe { call(u64::MAX, 1, 1, query.as_ptr(), 10, 1000) });
        assert!(error.contains("\"status\":\"error\""));
        assert!(!error.contains("private fixture phrase"));
    }
}

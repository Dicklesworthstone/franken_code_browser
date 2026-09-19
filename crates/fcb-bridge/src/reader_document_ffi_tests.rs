#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::ffi::{CStr, CString};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}

#[test]
fn all_document_entrypoint_types_and_public_header_names_agree() {
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_reader_document;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_window;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_headings;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64) -> *mut c_char = fcb_reader_document_heading;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_from_source;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_source;
    let _: extern "C" fn(u64, u64, u64, u64, u8) -> *mut c_char = fcb_reader_document_copy;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_reader_document_clear;
    let header = include_str!("../include/fcb_reader_document.h");
    for name in ["fcb_reader_document(", "fcb_reader_document_window(", "fcb_reader_document_headings(",
        "fcb_reader_document_heading(", "fcb_reader_document_from_source(", "fcb_reader_document_source(",
        "fcb_reader_document_copy(", "fcb_reader_document_clear("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}

#[test]
fn copy_modes_are_explicit_not_truthy_boolean_aliases() {
    assert_eq!(copy_mode(0).unwrap(), DocumentCopyMode::RenderedText);
    assert_eq!(copy_mode(1).unwrap(), DocumentCopyMode::EnclosingMarkdown);
    for mode in 2..=u8::MAX { assert_eq!(copy_mode(mode).err(), Some(AccessError::InvalidArgument)); }
}

#[test]
fn unknown_handles_null_slugs_and_wide_offsets_return_owned_error_strings() {
    // No global handle capacity is consumed by these marshaling checks. Positive
    // source/layout workflows exercise the actual registry in document sessions.
    let slug = CString::new("heading").unwrap();
    let pointers = [
        fcb_reader_document(u64::MAX, 1, 100, 65536, 8192, 8192, 4096),
        fcb_reader_document_window(u64::MAX, 1, u64::MAX, 1),
        fcb_reader_document_headings(u64::MAX, 1, 0, 1),
        unsafe { fcb_reader_document_heading(u64::MAX, 1, slug.as_ptr(), 1) },
        unsafe { fcb_reader_document_heading(u64::MAX, 1, std::ptr::null(), 1) },
        fcb_reader_document_from_source(u64::MAX, 1, u64::MAX, 1),
        fcb_reader_document_source(u64::MAX, 1, 0, u64::MAX, 0),
        fcb_reader_document_copy(u64::MAX, 1, 0, 1, 255),
        fcb_reader_document_clear(u64::MAX, 1),
    ];
    for pointer in pointers {
        let error = take(pointer);
        assert!(error.contains("\"status\":\"error\""), "{error}");
        assert!(!error.contains("\"document_ready\":true"));
    }
}

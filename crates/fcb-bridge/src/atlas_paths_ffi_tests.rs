#![deny(unsafe_op_in_unsafe_fn)]
use super::*;

#[test]
fn file_finder_c_signatures_and_header_names_agree() {
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u8, u8) -> *mut c_char = fcb_atlas_find_files;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u8, u8,
        Option<crate::search_ffi::SearchCancellationCallback>, *mut std::ffi::c_void) -> *mut c_char = fcb_atlas_find_files_cancelable;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_file_results;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_atlas_file_select;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_file_focus;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_file_open_reader;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_file_clear;
    let header = include_str!("../include/fcb_atlas_paths.h");
    for name in ["fcb_atlas_find_files(", "fcb_atlas_find_files_cancelable(", "fcb_atlas_file_results(", "fcb_atlas_file_select(",
        "fcb_atlas_file_focus(", "fcb_atlas_file_open_reader(", "fcb_atlas_file_clear("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}

#[test]
fn wire_modes_are_explicit_and_unknown_values_are_not_reinterpreted() {
    for (wire, expected) in [(0, PathMatchMode::Fuzzy), (1, PathMatchMode::Exact), (2, PathMatchMode::Prefix)] {
        assert_eq!(options(100, wire, 0).unwrap().mode, expected);
    }
    assert_eq!(options(1, 0, 0).unwrap().case, PathCase::UnicodeLowercase);
    assert_eq!(options(4096, 0, 1).unwrap().case, PathCase::Sensitive);
    assert!(options(0, 0, 0).is_err()); assert!(options(4097, 0, 0).is_err());
    assert!(options(100, 3, 0).is_err()); assert!(options(100, 0, 2).is_err());
}

#[test]
fn unknown_handle_c_responses_are_owned_strings_and_never_source_grants() {
    // No global session slots are occupied, so this test cannot interfere with
    // concurrent FFI lifecycle tests exercising the process-wide capacity limit.
    use std::ffi::CStr;
    for pointer in [unsafe { fcb_atlas_find_files(0, 1, std::ptr::null(), 100, 0, 0) },
        fcb_atlas_file_results(0, 1, 0, 10), fcb_atlas_file_select(0, 1, 1),
        fcb_atlas_file_focus(0, 1, 1, 1), fcb_atlas_file_open_reader(0, 0, 1, 1, 1024),
        fcb_atlas_file_clear(0, 2)] {
        assert!(!pointer.is_null());
        let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap();
        assert!(text.contains("\"status\":\"error\""), "{text}");
        unsafe { crate::fcb_free_string(pointer) };
    }
}

#[test]
fn cancelable_file_query_polls_before_inputs_and_refuses_unknown_handles() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    unsafe extern "C" fn cancel(context: *mut std::ffi::c_void) -> i32 {
        let polls = unsafe { &*context.cast::<AtomicUsize>() };
        polls.fetch_add(1, Ordering::Relaxed);
        1
    }
    let polls = AtomicUsize::new(0);
    let context = std::ptr::from_ref(&polls).cast_mut().cast();
    let result = unsafe { fcb_atlas_find_files_cancelable(0, 1, std::ptr::null(),
        100, 0, 0, Some(cancel), context) };
    assert!(result.is_null());
    assert_eq!(polls.load(Ordering::Relaxed), 1);
    let query = std::ffi::CString::new("src").unwrap();
    for (mode, case_mode) in [(0, 0), (3, 0), (0, 2)] {
        assert!(unsafe { fcb_atlas_find_files_cancelable(0, 2, query.as_ptr(), 100,
            mode, case_mode, None, std::ptr::null_mut()) }.is_null());
    }
}

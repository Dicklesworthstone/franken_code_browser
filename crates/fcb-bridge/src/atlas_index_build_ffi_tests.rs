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
    let _: unsafe extern "C" fn(u64, u64, u64, u64, u64, u64,
        *const c_char, *const c_char, u64, *const c_char) -> *mut c_char = fcb_atlas_index_begin_scoped;
    let _: unsafe extern "C" fn(u64, u64, u64, u64, u64, u64,
        *const c_char, *const c_char, u64, *const c_char) -> *mut c_char = fcb_atlas_index_prepare_scoped;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_index_step;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_index_progress;
    let header = include_str!("../include/fcb_atlas_index.h");
    for name in ["fcb_atlas_index_begin(", "fcb_atlas_index_begin_scoped(",
        "fcb_atlas_index_prepare_scoped(", "fcb_atlas_index_step(", "fcb_atlas_index_progress("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}

#[test]
fn c_scoped_capture_selects_before_quotas_and_keeps_its_basis_across_query_and_refresh() {
    use std::{ffi::CString, fs};
    use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close};
    use crate::reader_ffi::{fcb_reader_create, fcb_reader_copy_range, fcb_reader_close, FFI_TEST_LOCK};
    use super::super::{fcb_atlas_search_page, fcb_atlas_search_open_reader};
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-index-capture-scope-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a-outside.rs"), b"unrelated ".repeat(20)).unwrap();
    fs::write(root.join("z.rs"), b"needle").unwrap();
    fs::write(root.join("z.txt"), b"different type").unwrap();
    let root_c = CString::new(root.to_str().unwrap()).unwrap();
    let needle = CString::new("needle").unwrap();
    let atlas = fcb_atlas_create(); assert_ne!(atlas, 0);
    assert!(take(unsafe { fcb_atlas_open(atlas, root_c.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    {
        let types = CString::new("rs").unwrap();
        let region = CString::new("7a2e7273").unwrap(); // z.rs, after the unrelated member.
        let begin = take(unsafe { fcb_atlas_index_begin_scoped(atlas, 1, 1, 1024, 6, 65536,
            types.as_ptr(), std::ptr::null(), 3, region.as_ptr()) });
        assert!(begin.contains("\"capture_search_scope\":\"file-types\""), "{begin}");
        assert!(begin.contains("\"capture_scope_path_kind\":\"visible\""));
        assert!(begin.contains("\"capture_scope_region_files\":\"1\""));
        assert!(begin.contains("\"capture_eligible_files\":\"1\""));
        assert!(begin.contains("\"capture_excluded_files\":\"2\""));
        assert!(begin.contains("\"initial_source_bytes_read\":\"0\""));
        // Both foreign buffers end before the worker resumes construction.
    }
    let malformed = CString::new("7A2e7273").unwrap();
    let rejected = take(unsafe { fcb_atlas_index_begin_scoped(atlas, 2, 1, 1024, 6, 65536,
        std::ptr::null(), std::ptr::null(), 3, malformed.as_ptr()) });
    assert!(rejected.contains("INVALID_SCOPE"), "{rejected}");
    let done = take(fcb_atlas_index_step(atlas, 1));
    assert!(done.contains("\"capture_complete\":true"), "{done}");
    assert!(done.contains("\"captured_files\":\"1\""));
    assert!(done.contains("\"examined_files\":\"1\""));
    assert!(done.contains("\"pending_files\":\"0\""));
    assert!(done.contains("\"initial_source_bytes_read\":\"6\""));
    assert!(done.contains("\"capture_scope_region_missing_files\":\"0\""));
    // Invalid marshaling did not consume generation 2 or displace the builder.
    let path = CString::new("z.rs").unwrap();
    let selected = take(unsafe { fcb_atlas_search_indexed_path_scoped(atlas, 2, 1, needle.as_ptr(),
        std::ptr::null(), std::ptr::null(), 2, path.as_ptr(), 10, 6) });
    assert!(selected.contains("\"scope_path_kind\":\"file\""), "{selected}");
    assert!(selected.contains("\"capture_scope_path_kind\":\"visible\""));
    assert!(selected.contains("\"search_complete\":true"));
    assert!(selected.contains("\"retained_hits\":\"1\""));
    fs::write(root.join("z.rs"), b"replacement").unwrap();
    let broad = take(unsafe { fcb_atlas_search_indexed(atlas, 3, 1, needle.as_ptr(), 10, 4096) });
    assert!(broad.contains("\"search_complete\":false"), "{broad}");
    assert!(broad.contains("\"stop_reason\":\"capture-scope\""));
    assert!(broad.contains("\"eligible_files\":\"3\""));
    assert!(broad.contains("\"pending_files\":\"2\""));
    assert!(broad.contains("\"retained_hits\":\"1\""));
    assert!(broad.contains("\"source_bytes_read\":\"0\""));
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    assert!(take(fcb_atlas_search_open_reader(atlas, reader, 3, 1)).contains("\"source_reopened\":false"));
    let refreshed = take(unsafe { fcb_atlas_index_prepare_scoped(atlas, 4, 100, 1024, 4096, 65536,
        std::ptr::null(), std::ptr::null(), 0, std::ptr::null()) });
    assert!(refreshed.contains("\"capture_scope_path_kind\":\"workspace\""), "{refreshed}");
    assert!(refreshed.contains("\"capture_eligible_files\":\"3\""));
    assert!(refreshed.contains("\"capture_complete\":true"));
    let old_page = take(fcb_atlas_search_page(atlas, 3, 0, 10));
    assert!(old_page.contains("\"index_generation\":\"1\""), "{old_page}");
    assert!(old_page.contains("\"capture_scope_path_kind\":\"visible\""));
    assert!(old_page.contains("\"capture_eligible_files\":\"1\""));
    let empty = take(unsafe { fcb_atlas_index_prepare_scoped(atlas, 5, 100, 1024, 4096, 65536,
        std::ptr::null(), std::ptr::null(), 3, std::ptr::null()) });
    assert!(empty.contains("\"capture_scope_region_files\":\"0\""), "{empty}");
    assert!(empty.contains("\"capture_eligible_files\":\"0\""));
    assert!(empty.contains("\"capture_complete\":true"));
    assert!(empty.contains("\"initial_source_bytes_read\":\"0\""));
    assert_eq!(fcb_atlas_close(atlas), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 6)).contains("\"original_hex\":\"6e6565646c65\""));
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn unknown_or_wide_construction_requests_return_independently_owned_errors() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
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

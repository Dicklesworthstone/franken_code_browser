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
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, *const c_char, *const c_char, u64, u64)
        -> *mut c_char = fcb_atlas_search_indexed_scoped;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, *const c_char, *const c_char, u64, u64)
        -> *mut c_char = fcb_atlas_search_indexed_begin_scoped;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, *const c_char, *const c_char,
        u64, *const c_char, u64, u64) -> *mut c_char = fcb_atlas_search_indexed_path_scoped;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, *const c_char, *const c_char,
        u64, *const c_char, u64, u64) -> *mut c_char = fcb_atlas_search_indexed_begin_path_scoped;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, *const c_char, *const c_char,
        u64, *const c_char, u64, u64, u64) -> *mut c_char = fcb_atlas_search_indexed_begin_counted_matching;
    let header = include_str!("../include/fcb_atlas_index.h");
    for name in ["fcb_atlas_index_prepare(", "fcb_atlas_index_info(",
        "fcb_atlas_index_clear(", "fcb_atlas_search_indexed(", "fcb_atlas_search_indexed_begin(",
        "fcb_atlas_search_indexed_scoped(", "fcb_atlas_search_indexed_begin_scoped(",
        "fcb_atlas_search_indexed_path_scoped(", "fcb_atlas_search_indexed_begin_path_scoped(",
        "fcb_atlas_search_indexed_begin_counted_matching("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}

#[test]
fn unknown_index_handles_return_bounded_independently_owned_error_strings() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
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
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
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

#[cfg(unix)]
#[test]
fn counted_index_abi_preserves_rows_scope_cancellation_and_original_captures() {
    use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close, fcb_atlas_cancel};
    use crate::atlas_search_ffi::{fcb_atlas_search_step, fcb_atlas_search_page, fcb_atlas_search_open_reader};
    use crate::reader_ffi::{fcb_reader_create, fcb_reader_copy_range, fcb_reader_close, FFI_TEST_LOCK};
    use std::fs;

    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-index-counted-ffi-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.rs"), b"banana").unwrap();
    fs::write(root.join("b.rs"), b"banana banana").unwrap();
    let root_c = CString::new(root.to_str().unwrap()).unwrap();
    let needle = CString::new("ana").unwrap();
    let atlas = fcb_atlas_create(); assert_ne!(atlas, 0);
    assert!(take(unsafe { fcb_atlas_open(atlas, root_c.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    assert!(take(fcb_atlas_index_prepare(atlas, 1, 100, 1024, 4096, 65536))
        .contains("\"capture_complete\":true"));
    let begin = take(unsafe { fcb_atlas_search_indexed_begin_counted_matching(atlas, 2, 1,
        needle.as_ptr(), std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 1, 1, 4096) });
    assert!(begin.contains("\"count_all_matches\":true"), "{begin}");
    assert!(begin.contains("\"verification_source_bytes\":\"0\""));
    let first = take(fcb_atlas_search_step(atlas, 2));
    assert!(first.contains("\"matches_seen\":\"2\""), "{first}");
    assert!(first.contains("\"search_in_progress\":true"));
    assert!(first.contains("\"truncated\":true"));
    // Rejected case policy must not supersede the already running generation.
    let invalid = take(unsafe { fcb_atlas_search_indexed_begin_counted_matching(atlas, 3, 1,
        needle.as_ptr(), std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 0, 1, 4096) });
    assert!(invalid.contains("INVALID_ARGUMENT"), "{invalid}");
    fs::write(root.join("a.rs"), b"replacement").unwrap();
    fs::write(root.join("b.rs"), b"replacement").unwrap();
    let terminal = take(fcb_atlas_search_step(atlas, 2));
    for expected in ["\"matches_seen\":\"6\"", "\"retained_hits\":\"1\"",
        "\"search_complete\":true", "\"match_count_complete\":true", "\"truncated\":true",
        "\"source_bytes_read\":\"0\"", "\"read_calls\":\"0\""] {
        assert!(terminal.contains(expected), "missing {expected}: {terminal}");
    }
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    assert!(take(fcb_atlas_search_open_reader(atlas, reader, 2, 1)).contains("\"status\":\"ok\""));
    // The same API scopes before counting; the second file has four original hits.
    let path = CString::new("b.rs").unwrap();
    let scoped = take(unsafe { fcb_atlas_search_indexed_begin_counted_matching(atlas, 3, 1,
        needle.as_ptr(), std::ptr::null(), std::ptr::null(), 2, path.as_ptr(), 1, 1, 4096) });
    assert!(scoped.contains("\"eligible_files\":\"1\""), "{scoped}");
    let scoped = take(fcb_atlas_search_step(atlas, 3));
    assert!(scoped.contains("\"matches_seen\":\"4\""), "{scoped}");
    assert!(scoped.contains("\"match_count_complete\":true"));
    let begin = take(unsafe { fcb_atlas_search_indexed_begin_counted_matching(atlas, 4, 1,
        needle.as_ptr(), std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 1, 1, 4096) });
    assert!(begin.contains("\"search_in_progress\":true"));
    assert!(take(fcb_atlas_search_step(atlas, 4)).contains("\"search_in_progress\":true"));
    assert_eq!(fcb_atlas_cancel(atlas), 1);
    assert!(take(fcb_atlas_search_step(atlas, 4)).contains("\"status\":\"error\""));
    assert!(take(fcb_atlas_search_page(atlas, 3, 0, 64)).contains("\"matches_seen\":\"4\""));
    assert_eq!(fcb_atlas_close(atlas), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 6)).contains("62616e616e61"));
    assert_eq!(fcb_reader_close(reader), 1);
}

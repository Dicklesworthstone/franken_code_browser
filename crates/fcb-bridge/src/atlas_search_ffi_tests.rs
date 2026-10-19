#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close, fcb_atlas_present, fcb_atlas_info, fcb_atlas_cancel};
use crate::reader_ffi::{fcb_reader_create, fcb_reader_copy_range, fcb_reader_close, fcb_reader_info, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString}, fs};
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}

#[test]
fn c_repository_search_paging_overlay_focus_and_retained_reader_workflow() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-search-ffi-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap(); fs::write(root.join("a.rs"), "needle ".repeat(80)).unwrap();
    let root_c = CString::new(root.to_str().unwrap()).unwrap();
    let query = CString::new("needle").unwrap();
    let atlas = fcb_atlas_create(); assert_ne!(atlas, 0);
    assert!(take(unsafe { fcb_atlas_open(atlas, root_c.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    assert!(take(unsafe { fcb_atlas_search(atlas, 1, std::ptr::null(), 100, 100, 1024, 8192) }).contains("INVALID_ARGUMENT"));
    let response = take(unsafe { fcb_atlas_search(atlas, 1, query.as_ptr(), 100, 100, 1024, 8192) });
    assert!(response.contains("\"search_complete\":true"), "{response}");
    assert!(response.contains("\"next_offset\":\"64\""));
    let page = take(fcb_atlas_search_page(atlas, 1, 64, 128));
    assert!(page.contains("\"hit_id\":\"65\""));
    assert!(page.contains("\"next_offset\":null"));
    assert!(take(fcb_atlas_search_page(atlas, 1, 0, 0)).contains("INVALID_LIMITS"));
    let overlay = take(fcb_atlas_search_overlay(atlas, 1));
    assert!(overlay.contains("\"retained_occurrences\":\"80\""));
    assert_eq!(overlay.matches("\"node\":").count(), 1);
    fs::rename(root.join("a.rs"), root.join("moved.rs")).unwrap();
    let focus = take(fcb_atlas_search_focus(atlas, 1, 1, 1));
    assert!(focus.contains("\"native_presented\":false"));
    assert!(take(fcb_atlas_info(atlas)).contains("\"presented_plan_generation\":null"));
    assert!(take(fcb_atlas_present(atlas, 1, 1, 1)).contains("host-declared-not-observed"));
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    assert!(take(fcb_atlas_search_open_reader(atlas, reader, 2, 1)).contains("STALE_QUERY"));
    assert!(take(fcb_reader_info(reader)).contains("NOT_OPEN"));
    let linked = take(fcb_atlas_search_open_reader(atlas, reader, 1, 80));
    assert!(linked.contains("\"source_reopened\":false"), "{linked}");
    assert!(take(fcb_atlas_search_open_reader(atlas, reader, 1, 1)).contains("ALREADY_OPEN"));
    assert!(take(fcb_atlas_search_clear(atlas, 2)).contains("\"retained_hits\":\"0\""));
    assert!(take(fcb_atlas_search_overlay(atlas, 1)).contains("NO_QUERY"));
    assert_eq!(fcb_atlas_close(atlas), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 6)).contains("6e6565646c65"));
    assert_eq!(fcb_reader_close(reader), 1);
    assert!(take(fcb_atlas_search_page(atlas, 1, 0, 10)).contains("UNKNOWN"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn c_progressive_search_activates_first_hit_then_cancel_restores_finished_generation() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-step-ffi-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.rs"), b"needle original").unwrap(); fs::write(root.join("b.rs"), b"needle b").unwrap();
    let root_c = CString::new(root.to_str().unwrap()).unwrap(); let query = CString::new("needle").unwrap();
    let a = fcb_atlas_create(); assert_ne!(a, 0);
    assert!(take(unsafe { fcb_atlas_open(a, root_c.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    assert!(take(unsafe { fcb_atlas_search(a, 1, query.as_ptr(), 100, 100, 1024, 8192) }).contains("\"search_complete\":true"));
    let begin = take(unsafe { fcb_atlas_search_begin(a, 2, query.as_ptr(), 100, 100, 1024, 8192) });
    assert!(begin.contains("\"source_bytes_read\":\"0\"")); assert!(begin.contains("\"search_in_progress\":true"));
    let progress = take(fcb_atlas_search_step(a, 2));
    assert!(progress.contains("\"last_step_files\":\"1\"")); assert!(progress.contains("\"hit_id\":\"1\""));
    assert!(progress.contains("\"search_in_progress\":true"));
    fs::write(root.join("a.rs"), b"changed").unwrap();
    let r = fcb_reader_create(); assert_ne!(r, 0);
    assert!(take(fcb_atlas_search_open_reader(a, r, 2, 1)).contains("\"source_reopened\":false"));
    assert_eq!(fcb_atlas_cancel(a), 1);
    assert!(take(fcb_atlas_search_step(a, 2)).contains("STALE_QUERY"));
    assert!(take(fcb_atlas_search_page(a, 2, 0, 10)).contains("STALE_QUERY"));
    assert!(take(fcb_atlas_search_page(a, 1, 0, 10)).contains("\"search_complete\":true"));
    assert_eq!(fcb_atlas_close(a), 1);
    assert!(take(fcb_reader_copy_range(r, 0, 6)).contains("6e6565646c65"));
    assert_eq!(fcb_reader_close(r), 1); fs::remove_dir_all(root).unwrap();
}

#[test]
fn c_begin_owns_query_text_but_defers_source_and_finishes_unavailable_coverage() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-step-text-ffi-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.rs"), b"needle a").unwrap(); fs::write(root.join("b.rs"), b"needle b").unwrap();
    let root_c = CString::new(root.to_str().unwrap()).unwrap(); let a = fcb_atlas_create(); assert_ne!(a, 0);
    assert!(take(unsafe { fcb_atlas_open(a, root_c.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    {
        let query = CString::new("needle").unwrap();
        assert!(take(unsafe { fcb_atlas_search_begin(a, 1, query.as_ptr(), 100, 100, 1024, 8192) }).contains("\"source_bytes_read\":\"0\""));
    }
    fs::remove_file(root.join("a.rs")).unwrap();
    assert!(take(unsafe { fcb_atlas_search_begin(a, 2, std::ptr::null(), 100, 100, 1024, 8192) }).contains("INVALID_ARGUMENT"));
    let first = take(fcb_atlas_search_step(a, 1));
    assert!(first.contains("\"unavailable_files\":\"1\"")); assert!(first.contains("\"search_in_progress\":true"));
    let end = take(fcb_atlas_search_step(a, 1));
    assert!(end.contains("\"search_in_progress\":false")); assert!(end.contains("\"search_complete\":false"));
    assert!(end.contains("\"retained_hits\":\"1\"")); assert!(end.contains("\"stop_reason\":\"all-files-examined\""));
    assert_eq!(fcb_atlas_close(a), 1); fs::remove_dir_all(root).unwrap();
}

#[test]
fn c_search_signatures_and_header_declarations_agree() {
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_begin;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, *const c_char, *const c_char, u64, u64, u64, u64)
        -> *mut c_char = fcb_atlas_search_scoped;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, *const c_char, *const c_char, u64, u64, u64, u64)
        -> *mut c_char = fcb_atlas_search_begin_scoped;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, *const c_char, *const c_char, u64,
        *const c_char, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_path_scoped;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, *const c_char, *const c_char, u64,
        *const c_char, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_begin_path_scoped;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_search_step;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_page;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_search_overlay;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_search_clear;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_focus;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_open_reader;
    let header = include_str!("../include/fcb_atlas_search.h");
    for function in ["fcb_atlas_search(", "fcb_atlas_search_begin(", "fcb_atlas_search_step(",
        "fcb_atlas_search_scoped(", "fcb_atlas_search_begin_scoped(",
        "fcb_atlas_search_path_scoped(", "fcb_atlas_search_begin_path_scoped(",
        "fcb_atlas_search_page(", "fcb_atlas_search_overlay(", "fcb_atlas_search_clear(",
        "fcb_atlas_search_focus(", "fcb_atlas_search_open_reader("] {
        assert_eq!(header.matches(function).count(), 1, "{function}");
    }
}

#[test]
fn c_scoped_live_and_indexed_search_freeze_lists_before_hit_and_source_limits() {
    use super::index::{fcb_atlas_index_prepare, fcb_atlas_search_indexed_begin_scoped,
        fcb_atlas_search_indexed_scoped};
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-scope-ffi-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.txt"), b"needle ".repeat(2000)).unwrap();
    fs::write(root.join("z.rs"), b"needle").unwrap();
    let root_c = CString::new(root.to_str().unwrap()).unwrap();
    let needle = CString::new("needle").unwrap();
    let atlas = fcb_atlas_create(); assert_ne!(atlas, 0);
    assert!(take(unsafe { fcb_atlas_open(atlas, root_c.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    let old = take(unsafe { fcb_atlas_search(atlas, 1, needle.as_ptr(), 1, 1, 32768, 65536) });
    assert!(old.contains("a.txt")); assert!(!old.contains("z.rs"));
    {
        let invalid = CString::new(".rs").unwrap();
        let rejected = take(unsafe { fcb_atlas_search_begin_scoped(atlas, 2, needle.as_ptr(),
            invalid.as_ptr(), std::ptr::null(), 1, 1, 32768, 65536) });
        assert!(rejected.contains("INVALID_SCOPE"));
        let scope = CString::new("RS,rs").unwrap();
        let begin = take(unsafe { fcb_atlas_search_begin_scoped(atlas, 2, needle.as_ptr(),
            scope.as_ptr(), std::ptr::null(), 1, 1, 32768, 65536) });
        assert!(begin.contains("\"scope_extensions\":\"rs\""));
        assert!(begin.contains("\"source_bytes_read\":\"0\""));
    }
    let live = take(fcb_atlas_search_step(atlas, 2));
    assert!(live.contains("\"search_complete\":true"), "{live}");
    assert!(live.contains("z.rs")); assert!(!live.contains("a.txt"));
    assert!(live.contains("\"eligible_files\":\"1\""));
    assert!(live.contains("\"excluded_files\":\"1\""));
    assert!(take(fcb_atlas_index_prepare(atlas, 3, 100, 32768, 65536, 65536)).contains("\"capture_complete\":true"));
    {
        let scope = CString::new("rs").unwrap();
        let begin = take(unsafe { fcb_atlas_search_indexed_begin_scoped(atlas, 4, 3,
            needle.as_ptr(), scope.as_ptr(), std::ptr::null(), 1, 6) });
        assert!(begin.contains("\"eligible_captured_files\":\"1\""));
        assert!(begin.contains("\"verification_source_bytes\":\"0\""));
    }
    let indexed = take(fcb_atlas_search_step(atlas, 4));
    assert!(indexed.contains("\"search_complete\":true"), "{indexed}");
    assert!(indexed.contains("z.rs")); assert!(!indexed.contains("a.txt"));
    assert!(indexed.contains("\"verification_source_bytes\":\"6\""));
    let empty = CString::new("").unwrap();
    let none = take(unsafe { fcb_atlas_search_indexed_scoped(atlas, 5, 3,
        needle.as_ptr(), empty.as_ptr(), std::ptr::null(), 1, 0) });
    assert!(none.contains("\"search_complete\":true"));
    assert!(none.contains("\"eligible_files\":\"0\""));
    assert!(none.contains("\"retained_hits\":\"0\""));
    assert_eq!(fcb_atlas_close(atlas), 1);
}

#[test]
fn c_structure_query_reuses_progress_and_exact_reader_import_without_text_fallback() {
    use super::structure::fcb_atlas_structure_begin;
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-structure-ffi-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("outside.md"), b"# Start!\n").unwrap();
    fs::write(root.join("docs/README.md"), b"# Start!\n\n# Start!\n").unwrap();
    let root_c = CString::new(root.to_str().unwrap()).unwrap();
    let atlas = fcb_atlas_create(); assert_ne!(atlas, 0);
    assert!(take(unsafe { fcb_atlas_open(atlas, root_c.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    let needle = CString::new("Start!").unwrap(); let path = CString::new("docs").unwrap();
    for (mode, name_mode) in [(0, 0), (3, 0), (2, 3)] {
        let rejected = take(unsafe { fcb_atlas_structure_begin(atlas, 1, mode, name_mode,
            needle.as_ptr(), std::ptr::null(), std::ptr::null(), 1, path.as_ptr(), 10, 10, 65536, 65536) });
        assert!(rejected.contains("INVALID_ARGUMENT"));
    }
    let begin = take(unsafe { fcb_atlas_structure_begin(atlas, 1, 2, 0, needle.as_ptr(),
        std::ptr::null(), std::ptr::null(), 1, path.as_ptr(), 10, 10, 65536, 65536) });
    assert!(begin.contains("\"source_bytes_read\":\"0\""));
    assert!(begin.contains("\"eligible_files\":\"1\""));
    let result = take(fcb_atlas_search_step(atlas, 1));
    assert!(result.contains("\"search_complete\":true"), "{result}");
    assert!(result.contains("\"retained_hits\":\"2\""));
    assert!(result.contains("\"heading_slug\":\"start-2\""));
    assert!(!result.contains("outside.md"));
    fs::write(root.join("docs/README.md"), b"live replacement").unwrap();
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let receipt = take(fcb_atlas_search_open_reader(atlas, reader, 1, 2));
    assert!(receipt.contains("\"source_reopened\":false"));
    assert!(receipt.contains("\"heading_slug\":\"start-2\""));
    assert!(take(fcb_atlas_search_clear(atlas, 2)).contains("\"retained_hits\":\"0\""));
    assert_eq!(fcb_atlas_close(atlas), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 8)).contains("2320537461727421"));
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn c_path_scope_freezes_exact_components_and_rejects_invalid_replacements_before_admission() {
    use super::index::{fcb_atlas_index_prepare, fcb_atlas_search_indexed_begin_path_scoped,
        fcb_atlas_search_indexed_path_scoped};
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-path-scope-ffi-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    for directory in ["dir", "dir-old"] { fs::create_dir_all(root.join(directory)).unwrap(); }
    fs::write(root.join("a.rs"), b"needle ".repeat(1001)).unwrap();
    fs::write(root.join("dir/file.rs"), b"needle").unwrap();
    fs::write(root.join("dir/skip.txt"), b"needle ".repeat(1001)).unwrap();
    fs::write(root.join("dir-old/file.rs"), b"needle ".repeat(1001)).unwrap();
    let root_c = CString::new(root.to_str().unwrap()).unwrap();
    let needle = CString::new("needle").unwrap(); let atlas = fcb_atlas_create(); assert_ne!(atlas, 0);
    assert!(take(unsafe { fcb_atlas_open(atlas, root_c.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    let broad = take(unsafe { fcb_atlas_search(atlas, 1, needle.as_ptr(), 1, 1, 32768, 65536) });
    assert!(broad.contains("a.rs")); assert!(!broad.contains("dir/file.rs"));
    assert!(broad.contains("\"scope_path_kind\":\"workspace\""));
    assert!(broad.contains("\"scope_path_hex\":\"\""));
    {
        let types = CString::new("RS").unwrap(); let path = CString::new("dir").unwrap();
        let begin = take(unsafe { fcb_atlas_search_begin_path_scoped(atlas, 2, needle.as_ptr(),
            types.as_ptr(), std::ptr::null(), 1, path.as_ptr(), 1, 1, 32768, 6) });
        assert!(begin.contains("\"source_bytes_read\":\"0\""), "{begin}");
        assert!(begin.contains("\"eligible_files\":\"1\""));
        assert!(begin.contains("\"scope_path_kind\":\"directory\""));
        assert!(begin.contains("\"scope_path_hex\":\"646972\""));
        // Both foreign buffers are destroyed here; the pending query owns its
        // validated exact bytes and selected FileIds until it is replaced.
    }
    for (kind, path) in [(0, "dir"), (1, ""), (1, "/dir"), (1, "dir/"),
        (1, "dir//sub"), (2, "../file.rs"), (2, "dir/./file.rs"), (2, "dir/../file.rs")] {
        let path = CString::new(path).unwrap();
        let rejected = take(unsafe { fcb_atlas_search_begin_path_scoped(atlas, 3, needle.as_ptr(),
            std::ptr::null(), std::ptr::null(), kind, path.as_ptr(), 1, 1, 32768, 65536) });
        assert!(rejected.contains("INVALID_SCOPE"), "{rejected}");
    }
    let too_long = CString::new("x".repeat(4097)).unwrap();
    let rejected = take(unsafe { fcb_atlas_search_begin_path_scoped(atlas, 3, needle.as_ptr(),
        std::ptr::null(), std::ptr::null(), 2, too_long.as_ptr(), 1, 1, 32768, 65536) });
    assert!(rejected.contains("INVALID_SCOPE"));
    let invalid_utf8 = CString::new(vec![0xff]).unwrap();
    for (kind, path) in [(99, std::ptr::null()), (2, invalid_utf8.as_ptr())] {
        let rejected = take(unsafe { fcb_atlas_search_begin_path_scoped(atlas, 3, needle.as_ptr(),
            std::ptr::null(), std::ptr::null(), kind, path, 1, 1, 32768, 65536) });
        assert!(rejected.contains("INVALID_ARGUMENT"));
    }
    let live = take(fcb_atlas_search_step(atlas, 2));
    assert!(live.contains("\"search_complete\":true"), "{live}");
    assert!(live.contains("\"eligible_files\":\"1\""));
    assert!(live.contains("\"excluded_files\":\"3\""));
    assert!(live.contains("\"source_bytes_read\":\"6\""));
    assert!(live.contains("dir/file.rs")); assert!(!live.contains("dir-old/file.rs"));
    assert!(take(fcb_atlas_index_prepare(atlas, 3, 100, 32768, 65536, 65536)).contains("\"capture_complete\":true"));
    {
        let path = CString::new("dir/file.rs").unwrap();
        let begin = take(unsafe { fcb_atlas_search_indexed_begin_path_scoped(atlas, 4, 3,
            needle.as_ptr(), std::ptr::null(), std::ptr::null(), 2, path.as_ptr(), 1, 6) });
        assert!(begin.contains("\"scope_path_kind\":\"file\""));
        assert!(begin.contains("\"search_scope\":\"all\""));
        assert!(begin.contains("\"verification_source_bytes\":\"0\""));
    }
    fs::write(root.join("dir/file.rs"), b"replacement").unwrap();
    let indexed = take(fcb_atlas_search_step(atlas, 4));
    assert!(indexed.contains("\"search_complete\":true"), "{indexed}");
    assert!(indexed.contains("\"verification_source_bytes\":\"6\""));
    assert!(indexed.contains("\"source_bytes_read\":\"0\""));
    let path = CString::new("dir/file.rs").unwrap();
    let current = take(unsafe { fcb_atlas_search_path_scoped(atlas, 5, needle.as_ptr(),
        std::ptr::null(), std::ptr::null(), 2, path.as_ptr(), 10, 10, 32768, 65536) });
    assert!(current.contains("\"search_complete\":true"));
    assert!(current.contains("\"retained_hits\":\"0\""));
    let retained = take(unsafe { fcb_atlas_search_indexed_path_scoped(atlas, 6, 3,
        needle.as_ptr(), std::ptr::null(), std::ptr::null(), 2, path.as_ptr(), 1, 6) });
    assert!(retained.contains("\"search_complete\":true"));
    assert!(retained.contains("\"retained_hits\":\"1\""));
    assert_eq!(fcb_atlas_close(atlas), 1);
}

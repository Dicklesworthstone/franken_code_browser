#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close, fcb_atlas_present, fcb_atlas_info};
use crate::reader_ffi::{fcb_reader_create, fcb_reader_copy_range, fcb_reader_close, fcb_reader_info};
use std::{ffi::{CStr, CString}, fs};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}

#[test]
fn c_repository_search_paging_overlay_focus_and_retained_reader_workflow() {
    // One handle/reader at a time; independent registry tests cover capacity
    // pressure without exhausting the process-global FFI registries.
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
}

#[test]
fn c_search_signatures_and_header_declarations_agree() {
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_page;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_search_overlay;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_search_clear;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_focus;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_open_reader;
    let header = include_str!("../include/fcb_atlas_search.h");
    for function in ["fcb_atlas_search(", "fcb_atlas_search_page(", "fcb_atlas_search_overlay(",
        "fcb_atlas_search_clear(", "fcb_atlas_search_focus(", "fcb_atlas_search_open_reader("] {
        assert_eq!(header.matches(function).count(), 1, "{function}");
    }
}

#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::{fcb_atlas_search, fcb_atlas_search_step, fcb_atlas_search_page,
    fcb_atlas_search_open_reader};
use super::super::index::{fcb_atlas_index_prepare, fcb_atlas_index_clear};
use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close};
use crate::reader_ffi::{fcb_reader_create, fcb_reader_copy_range, fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString}, fs, path::PathBuf};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let value = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; value
}
fn workspace(files: &[(&str, &[u8])]) -> (PathBuf, u64) {
    let root = std::env::temp_dir().join(format!("fcb-atlas-expression-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    for (name, source) in files {
        let path = root.join(name); fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    let root_text = CString::new(root.to_str().unwrap()).unwrap();
    let handle = fcb_atlas_create(); assert_ne!(handle, 0);
    let opened = take(unsafe { fcb_atlas_open(handle, root_text.as_ptr(), 100) });
    assert!(opened.contains("\"status\":\"ok\""), "{opened}"); (root, handle)
}
fn begin(handle: u64, generation: u64, expression: &CString, scan_bytes: u64) -> String {
    take(unsafe { fcb_atlas_search_begin_expression(handle, generation, expression.as_ptr(),
        std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 1, 100, 100, 8192, 65536, scan_bytes) })
}
fn finish(handle: u64, generation: u64) -> String {
    for _ in 0..100 {
        let step = take(fcb_atlas_search_step(handle, generation));
        assert!(step.contains("\"status\":\"ok\""), "{step}");
        if step.contains("\"search_in_progress\":false") { return step; }
    }
    panic!("bounded expression did not finish");
}

#[test]
fn c_live_expression_keeps_scope_case_predicates_and_retained_utf16_navigation() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let source: Vec<_> = "\u{feff}NEEDLE\r\nReQuIrEd\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let (root, handle) = workspace(&[("src/source.rs", &source),
        ("src/excluded.rs", b"needle required forbidden"), ("docs/source.rs", b"needle required")]);
    let expression = CString::new("needle required -forbidden path:src/ lang:rust").unwrap();
    let extensions = CString::new("rs").unwrap(); let path = CString::new("src").unwrap();
    let admitted = take(unsafe { fcb_atlas_search_begin_expression(handle, 1, expression.as_ptr(),
        extensions.as_ptr(), std::ptr::null(), 1, path.as_ptr(), 0, 100, 100, 8192, 65536, 65536) });
    assert!(admitted.contains("\"command\":\"begin-expression\""), "{admitted}");
    assert!(admitted.contains("\"source_bytes_read\":\"0\""));
    let mut expired_input = expression.into_bytes_with_nul(); expired_input.fill(b'x');
    let found = finish(handle, 1);
    for field in ["\"retained_hits\":\"1\"", "\"search_complete\":true",
        "\"mode\":\"decoded-text-expression\"", "\"case_sensitive\":false",
        "\"primary_needle\":\"needle\"", "\"matched_text\":\"NEEDLE\"",
        "\"query_syntax\":\"fcb-bounded-expression/1\"",
        "\"predicate_scope\":\"same-complete-capture\""] {
        assert!(found.contains(field), "missing {field}: {found}");
    }
    fs::write(root.join("src/source.rs"), b"replacement").unwrap();
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_atlas_search_open_reader(handle, reader, 1, 1));
    assert!(opened.contains("\"source_reopened\":false"), "{opened}");
    let saved_page = fcb_atlas_search_page(handle, 1, 0, 100);
    assert_eq!(fcb_atlas_close(handle), 1);
    assert!(take(saved_page).contains("needle required -forbidden path:src/ lang:rust"));
    assert!(take(fcb_reader_copy_range(reader, 2, 14)).contains("\"original_hex\":\"4e004500450044004c004500\""));
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn c_indexed_expression_reuses_capture_and_ordinary_query_generation_namespace() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, handle) = workspace(&[("a.rs", b"needle required"), ("b.rs", b"needle forbidden")]);
    assert!(take(fcb_atlas_index_prepare(handle, 1, 100, 8192, 65536, 65536)).contains("\"capture_complete\":true"));
    fs::write(root.join("a.rs"), b"changed live bytes").unwrap();
    let expression = CString::new("needle required -forbidden").unwrap();
    let admitted = take(unsafe { fcb_atlas_search_indexed_begin_expression(handle, 2, 1, expression.as_ptr(),
        std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 1, 100, 65536) });
    assert!(admitted.contains("\"command\":\"begin-indexed-expression\""), "{admitted}");
    let found = finish(handle, 2);
    assert!(found.contains("\"retained_hits\":\"1\""), "{found}");
    assert!(found.contains("\"source_bytes_read\":\"0\""));
    let literal = CString::new("needle").unwrap();
    let stale = take(unsafe { fcb_atlas_search(handle, 2, literal.as_ptr(), 100, 100, 8192, 65536) });
    assert!(stale.contains("STALE_QUERY"), "{stale}");
    let wrong_index = take(unsafe { fcb_atlas_search_indexed_begin_expression(handle, 3, 999, expression.as_ptr(),
        std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 1, 100, 65536) });
    assert!(wrong_index.contains("\"status\":\"error\""));
    assert!(take(fcb_atlas_search_page(handle, 2, 0, 100)).contains("\"retained_hits\":\"1\""));
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    assert!(take(fcb_atlas_search_open_reader(handle, reader, 2, 1)).contains("\"source_reopened\":false"));
    assert!(take(fcb_atlas_index_clear(handle, 4)).contains("\"status\":\"ok\""));
    assert_eq!(fcb_atlas_close(handle), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 6)).contains("\"original_hex\":\"6e6565646c65\""));
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn c_parser_and_marshaling_errors_do_not_replace_verified_results() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, handle) = workspace(&[("a.rs", b"needle required")]);
    let valid = CString::new("needle required").unwrap(); begin(handle, 1, &valid, 65536); finish(handle, 1);
    for (generation, expression, code) in [(2, "needle regex:a.*", "QUERY_REGEX_UNQUALIFIED"),
        (3, "\"needle", "QUERY_SYNTAX_ERROR"), (4, "path:src/", "QUERY_EMPTY"), (5, "", "QUERY_EMPTY")] {
        let expression = CString::new(expression).unwrap();
        let error = begin(handle, generation, &expression, 65536);
        assert!(error.contains(code), "{error}");
        assert!(take(fcb_atlas_search_page(handle, 1, 0, 100)).contains("\"retained_hits\":\"1\""));
    }
    let invalid_utf8 = [0xff_u8, 0]; let over_limit = [b'x'; MAX_QUERY_LEN + 1];
    for pointer in [std::ptr::null(), invalid_utf8.as_ptr().cast(), over_limit.as_ptr().cast()] {
        let error = take(unsafe { fcb_atlas_search_begin_expression(handle, 6, pointer,
            std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 1, 100, 100, 8192, 65536, 65536) });
        assert!(error.contains("INVALID_ARGUMENT"), "{error}");
    }
    let invalid_case = take(unsafe { fcb_atlas_search_begin_expression(handle, 6, valid.as_ptr(),
        std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 2, 100, 100, 8192, 65536, 65536) });
    assert!(invalid_case.contains("INVALID_ARGUMENT"));
    // Rejected foreign strings/flags never entered host attempt admission.
    assert!(begin(handle, 6, &valid, 65536).contains("\"status\":\"ok\""));
    assert!(finish(handle, 6).contains("\"retained_hits\":\"1\""));
    assert!(begin(handle, 7, &valid, 0).contains("\"status\":\"ok\""));
    let no_proof = finish(handle, 7);
    assert!(no_proof.contains("\"search_complete\":false") && no_proof.contains("verification-byte-limit"));
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn expression_abi_types_public_header_and_foreign_string_bounds_agree() {
    let _: unsafe extern "C" fn(u64, u64, *const c_char, *const c_char, *const c_char,
        u64, *const c_char, u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_begin_expression;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, *const c_char, *const c_char,
        u64, *const c_char, u64, u64, u64) -> *mut c_char = fcb_atlas_search_indexed_begin_expression;
    let header = include_str!("../include/fcb_atlas_expression.h");
    for name in ["fcb_atlas_search_begin_expression(", "fcb_atlas_search_indexed_begin_expression("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
    let limit = CString::new("x".repeat(MAX_QUERY_LEN)).unwrap();
    assert_eq!(unsafe { bounded_expression(limit.as_ptr()) }.unwrap().len(), MAX_QUERY_LEN);
    let unterminated = [b'x'; MAX_QUERY_LEN + 1];
    assert_eq!(unsafe { bounded_expression(unterminated.as_ptr().cast()) }.err(), Some(AccessError::InvalidArgument));
    let empty = CString::new("").unwrap();
    assert_eq!(unsafe { bounded_expression(empty.as_ptr()) }.unwrap(), "");
}

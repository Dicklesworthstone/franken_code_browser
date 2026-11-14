#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::reader_ffi::{fcb_reader_create, fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString, OsString}, fs, os::unix::ffi::OsStringExt,
    sync::atomic::{AtomicU64, Ordering}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected bounded catalog-stream JSON");
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; result
}
fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-catalog-stream-ffi-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn number(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn open(handle: u64, root: &CStr, query: &[u8], mode: u64, case_sensitive: u64) -> String {
    take(unsafe { fcb_catalog_stream_open(handle, root.as_ptr(), 11, query.as_ptr(), query.len() as u64,
        mode, case_sensitive, 1, 0, std::ptr::null(), std::ptr::null(), 0, std::ptr::null(),
        32, 64 * 1024 * 1024, 1_048_576) })
}
fn drain(handle: u64) -> String {
    let mut report = take(fcb_catalog_stream_info(handle));
    for _ in 0..10_000 {
        assert!(report.contains("\"status\":\"ok\""), "{report}");
        if report.contains("\"more\":false") { return report; }
        report = take(fcb_catalog_stream_advance(handle, 11, number(&report, "revision"), 256));
    }
    panic!("bounded C query failed to finish: {report}");
}

#[test]
fn actual_catalog_stream_c_api_retains_raw_path_utf16_witnesses_after_close() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root(); let path = root.join(OsString::from_vec(b"raw-\xff.rs".to_vec()));
    let bytes: Vec<u8> = "\u{feff}before needle after\r\n".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fs::write(&path, bytes).unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let handle = fcb_catalog_stream_create(); assert_ne!(handle, 0);
    let mut query = b"needle".to_vec();
    let initial = open(handle, &croot, &query, 0, 1);
    assert!(initial.contains("\"schema\":\"fcb.catalog-stream/1\""));
    assert_eq!(number(&initial, "revision"), 1);
    assert_eq!(number(&initial, "payload_bytes_read"), 0);
    query.fill(b'x'); // The immutable query must own its foreign bytes.
    let complete = drain(handle); let revision = number(&complete, "revision");
    assert_eq!(number(&complete, "matches_seen"), 1, "{complete}");
    assert!(complete.contains("\"search_complete\":true"));
    let page = take(fcb_catalog_stream_page(handle, 11, revision, 0, 32));
    assert!(page.contains("7261772dff2e7273"), "{page}");
    assert!(!page.contains("node_ordinal"));
    fs::write(path, b"changed after search").unwrap();
    let pointer = fcb_catalog_stream_hit(handle, 11, revision, 0);
    assert_eq!(fcb_catalog_stream_close(handle), 1);
    let hit = take(pointer);
    assert!(hit.contains("\"encoding\":\"utf16be\""), "{hit}");
    assert!(hit.contains("\"original_hex\":\"006e006500650064006c0065\""), "{hit}");
    assert!(hit.contains("\"text\":\"needle\""));
    assert!(hit.contains("\"additional_source_bytes_read\":\"0\""));
    assert_eq!(fcb_catalog_stream_cancel(handle), 0);
    assert!(take(fcb_catalog_stream_info(handle)).contains("CATALOG_STREAM_HANDLE_UNKNOWN"));
}

#[test]
fn catalog_stream_c_scalar_bounds_precede_pointer_reads_and_handles_do_not_alias() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let handle = fcb_catalog_stream_create(); assert_ne!(handle, 0);
    let reader = fcb_reader_create(); assert_ne!(reader, 0); assert_ne!(handle, reader);
    assert!(take(fcb_catalog_stream_info(reader)).contains("CATALOG_STREAM_HANDLE_UNKNOWN"));
    let dangling = std::ptr::NonNull::<u8>::dangling().as_ptr();
    for length in [0, 1025, u64::MAX] {
        let error = take(unsafe { fcb_catalog_stream_open(handle, dangling.cast(), 11, dangling,
            length, 0, 1, 1, 0, dangling.cast(), dangling.cast(), 0, dangling.cast(), 32, 1024, 1024) });
        assert!(error.contains("CATALOG_STREAM_HANDLE_INVALID_ARGUMENT"));
    }
    for (mode, case_sensitive, count_all, path_kind, max_matches, max_bytes, max_calls) in [
        (3, 1, 1, 0, 32, 1024, 1024), (0, 2, 1, 0, 32, 1024, 1024),
        (0, 1, 2, 0, 32, 1024, 1024), (0, 1, 1, 4, 32, 1024, 1024),
        (0, 1, 1, 0, 257, 1024, 1024), (0, 1, 1, 0, 32, 0, 1024),
        (0, 1, 1, 0, 32, 1024, 1_048_577),
    ] {
        let error = take(unsafe { fcb_catalog_stream_open(handle, dangling.cast(), 11, dangling,
            1, mode, case_sensitive, count_all, 0, dangling.cast(), dangling.cast(), path_kind,
            dangling.cast(), max_matches, max_bytes, max_calls) });
        assert!(error.contains("CATALOG_STREAM_HANDLE_INVALID_ARGUMENT"));
    }
    let root = root(); let croot = CString::new(root.to_str().unwrap()).unwrap();
    let invalid_utf8 = [0xff];
    let invalid = open(handle, &croot, &invalid_utf8, 0, 1);
    assert!(invalid.contains("CATALOG_STREAM_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_catalog_stream_info(handle)).contains("CATALOG_STREAM_HANDLE_NOT_OPEN"));
    let initial = open(handle, &croot, b"missing", 0, 1);
    assert!(initial.contains("\"status\":\"ok\""));
    let complete = drain(handle); let revision = number(&complete, "revision");
    assert!(complete.contains("\"search_complete\":true"), "{complete}");
    assert_eq!(number(&complete, "matches_seen"), 0);
    assert!(take(fcb_catalog_stream_page(handle, 12, revision, 0, 32)).contains("\"status\":\"error\""));
    assert!(take(fcb_catalog_stream_page(handle, 11, revision, 0, 33)).contains("\"status\":\"error\""));
    assert!(take(fcb_catalog_stream_hit(handle, 11, revision, 0)).contains("\"status\":\"error\""));
    assert!(take(fcb_catalog_stream_advance(handle, 11, revision, 0)).contains("\"status\":\"error\""));
    assert!(take(fcb_catalog_stream_advance(handle, 11, revision, 257)).contains("\"status\":\"error\""));
    assert_eq!(fcb_catalog_stream_close(handle), 1); assert_eq!(fcb_reader_close(reader), 1);
    for pointer in [fcb_catalog_stream_info(u64::MAX), fcb_catalog_stream_page(u64::MAX, 1, 1, 0, 1),
        fcb_catalog_stream_hit(u64::MAX, 1, 1, 0), fcb_catalog_stream_advance(u64::MAX, 1, 1, 1),
        fcb_catalog_stream_stop(u64::MAX, 1)] {
        let reply = take(pointer);
        assert!(reply.len() < 4096); assert!(reply.contains("\"status\":\"error\""));
    }
}

#[test]
fn catalog_stream_c_modes_case_scope_and_ordinal_windows_reach_shared_search() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root();
    fs::write(root.join("source.rs"), b"needle needle_tail NEEDLE\n").unwrap();
    fs::write(root.join("source.txt"), b"needle\n").unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    for (mode, case_sensitive, expected) in [(0, 1, 3), (0, 0, 4), (1, 1, 2), (2, 1, 3)] {
        let handle = fcb_catalog_stream_create(); assert_ne!(handle, 0);
        let initial = open(handle, &croot, b"needle", mode, case_sensitive);
        assert!(initial.contains("\"status\":\"ok\""), "{initial}");
        let complete = drain(handle);
        assert_eq!(number(&complete, "matches_seen"), expected, "{complete}");
        assert_eq!(fcb_catalog_stream_close(handle), 1);
    }
    let extensions = CString::new("rs").unwrap();
    let handle = fcb_catalog_stream_create(); assert_ne!(handle, 0);
    let initial = take(unsafe { fcb_catalog_stream_open(handle, croot.as_ptr(), 11, b"needle".as_ptr(),
        6, 0, 1, 1, 1, extensions.as_ptr(), std::ptr::null(), 0, std::ptr::null(), 1, 1024, 1024) });
    assert!(initial.contains("\"status\":\"ok\""), "{initial}");
    let complete = drain(handle);
    assert_eq!(number(&complete, "matches_seen"), 2, "{complete}");
    assert_eq!(number(&complete, "skipped_matches"), 1);
    assert_eq!(number(&complete, "retained_hits"), 1);
    assert_eq!(number(&complete, "eligible_files"), 1);
    assert_eq!(number(&complete, "excluded_files"), 1);
    assert_eq!(fcb_catalog_stream_close(handle), 1);
}

#[test]
fn catalog_stream_c_signatures_match_public_header() {
    let _: extern "C" fn() -> u64 = fcb_catalog_stream_create;
    let _: unsafe extern "C" fn(u64, *const c_char, u64, *const u8, u64, u64, u64, u64,
        u64, *const c_char, *const c_char, u64, *const c_char, u64, u64, u64) -> *mut c_char = fcb_catalog_stream_open;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_catalog_stream_info;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_catalog_stream_advance;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_catalog_stream_page;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_catalog_stream_hit;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_catalog_stream_stop;
    let _: extern "C" fn(u64) -> u8 = fcb_catalog_stream_cancel;
    let _: extern "C" fn(u64) -> u8 = fcb_catalog_stream_close;
    let header = include_str!("../include/fcb_catalog_stream.h");
    for name in ["create", "open", "info", "advance", "page", "hit", "stop", "cancel", "close"] {
        let symbol = format!("fcb_catalog_stream_{name}(");
        assert_eq!(header.matches(&symbol).count(), 1, "{symbol}");
    }
}

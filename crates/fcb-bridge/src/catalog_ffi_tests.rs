#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::reader_ffi::{fcb_reader_create, fcb_reader_copy_range, fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString, OsString}, fs, os::unix::ffi::OsStringExt,
    sync::atomic::{AtomicU64, Ordering}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a bounded JSON reply");
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; result
}
fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-catalog-ffi-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}

#[test]
fn actual_catalog_c_api_opens_raw_named_utf16_rows_and_owns_returned_data() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root(); let path = root.join(OsString::from_vec(b"raw-\xff.rs".to_vec()));
    let bytes: Vec<u8> = "\u{feff}αβ\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(&path, &bytes).unwrap();
    let handle = fcb_catalog_create(); assert_ne!(handle, 0);
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    assert!(take(unsafe { fcb_catalog_open(handle, croot.as_ptr()) }).contains("\"latest_page\":\"1\""));
    let mut retired = croot.into_bytes_with_nul(); retired.fill(b'x');
    let page_pointer = fcb_catalog_page(handle, 1);
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let receipt = take(fcb_catalog_open_reader(handle, reader, 1, 1, 1024));
    assert!(receipt.contains("7261772dff2e7273"), "{receipt}");
    assert!(receipt.contains("\"encoding\":\"utf16le\""), "{receipt}");
    assert!(receipt.contains("\"source_observation\":\"new-capture-after-metadata-selection\""));
    fs::write(&path, b"different source now").unwrap();
    assert_eq!(fcb_catalog_close(handle), 1);
    let page = take(page_pointer);
    assert!(page.contains("7261772dff2e7273"));
    assert!(page.contains("\"discovery_source_bytes_read\":\"0\""));
    let copied = take(fcb_reader_copy_range(reader, 0, bytes.len() as u64));
    assert!(copied.contains("\"original_hex\":\"fffeb103b2030d000a00\""), "{copied}");
    assert_eq!(fcb_reader_close(reader), 1);
    assert_eq!(fcb_catalog_cancel(handle), 0);
    assert!(take(fcb_catalog_info(handle)).contains("CATALOG_HANDLE_UNKNOWN"));
}

#[test]
fn c_catalog_errors_are_bounded_and_empty_complete_is_not_failed_open() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    for pointer in [fcb_catalog_info(u64::MAX), fcb_catalog_page(u64::MAX, 1),
        fcb_catalog_advance(u64::MAX, 1), fcb_catalog_stop(u64::MAX),
        fcb_catalog_open_reader(u64::MAX, u64::MAX, 1, 1, 1024)] {
        let text = take(pointer); assert!(text.len() < 4096);
        assert!(text.contains("\"status\":\"error\""));
    }
    let handle = fcb_catalog_create(); assert_ne!(handle, 0);
    let failed = take(unsafe { fcb_catalog_open(handle, std::ptr::null()) });
    assert!(failed.contains("CATALOG_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_catalog_info(handle)).contains("CATALOG_HANDLE_NOT_OPEN"));
    let root = root(); let croot = CString::new(root.to_str().unwrap()).unwrap();
    let info = take(unsafe { fcb_catalog_open(handle, croot.as_ptr()) });
    assert!(info.contains("\"state\":\"complete\""), "{info}");
    let page = take(fcb_catalog_page(handle, 1));
    assert!(page.contains("\"files\":[]")); assert!(page.contains("\"discovery_complete\":true"));
    assert!(take(fcb_catalog_page(handle, 0)).contains("\"status\":\"error\""));
    assert!(take(fcb_catalog_advance(handle, u64::MAX)).contains("\"status\":\"error\""));
    assert!(take(unsafe { fcb_catalog_open(handle, croot.as_ptr()) }).contains("CATALOG_HANDLE_ALREADY_OPEN"));
    assert_eq!(fcb_catalog_close(handle), 1);
}

#[test]
fn c_catalog_signatures_match_the_public_header() {
    let _: extern "C" fn() -> u64 = fcb_catalog_create;
    let _: unsafe extern "C" fn(u64, *const c_char) -> *mut c_char = fcb_catalog_open;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_catalog_info;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_catalog_page;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_catalog_advance;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_catalog_stop;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_catalog_open_reader;
    let _: extern "C" fn(u64) -> u8 = fcb_catalog_cancel;
    let _: extern "C" fn(u64) -> u8 = fcb_catalog_close;
    let header = include_str!("../include/fcb_project_catalog.h");
    for symbol in ["fcb_catalog_create", "fcb_catalog_open", "fcb_catalog_info", "fcb_catalog_page",
        "fcb_catalog_advance", "fcb_catalog_stop", "fcb_catalog_open_reader", "fcb_catalog_cancel", "fcb_catalog_close"] {
        assert_eq!(header.matches(&format!("{symbol}(")).count(), 1, "{symbol}");
    }
}

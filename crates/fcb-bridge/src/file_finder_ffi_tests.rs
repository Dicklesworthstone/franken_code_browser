#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::reader_ffi::{fcb_reader_create, fcb_reader_copy_range, fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString, OsString}, fs, os::unix::ffi::OsStringExt,
    sync::atomic::{AtomicU64, Ordering}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected bounded finder JSON");
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; result
}
fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-file-finder-ffi-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn number(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

#[test]
fn actual_file_finder_c_api_finds_raw_names_and_opens_exact_independent_utf16_reader() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root(); let path = root.join(OsString::from_vec(b"raw-\xff.rs".to_vec()));
    let bytes: Vec<u8> = "\u{feff}αβ\r\n".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fs::write(&path, &bytes).unwrap();
    let handle = fcb_file_finder_create(); assert_ne!(handle, 0);
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let mut query = b"raw-\xff.rs".to_vec();
    let initial = take(unsafe { fcb_file_finder_open(handle, croot.as_ptr(), 11,
        query.as_ptr(), query.len() as u64, 0, 1, 32) });
    assert_eq!(number(&initial, "revision"), 1);
    assert!(initial.contains("\"pending_batch\":true"));
    query.fill(b'x'); // Query bytes have been independently retained.
    let report = take(fcb_file_finder_advance(handle, 11, 1, 16));
    assert_eq!(number(&report, "retained_hits"), 1);
    assert!(report.contains("7261772dff2e7273"), "{report}");
    assert!(report.contains("\"discovery_source_bytes_read\":\"0\""));
    let revision = number(&report, "revision"); let file = number(&report, "file_id");
    let pointer = fcb_file_finder_page(handle, 11, revision);
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let receipt = take(fcb_file_finder_open_reader(handle, reader, 11, revision, file, 1024));
    assert!(receipt.contains("\"encoding\":\"utf16be\""), "{receipt}");
    assert!(receipt.contains("\"source_observation\":\"new-capture-after-metadata-selection\""));
    fs::write(path, b"changed after selection").unwrap();
    assert_eq!(fcb_file_finder_close(handle), 1);
    assert!(take(pointer).contains("7261772dff2e7273"));
    let copied = take(fcb_reader_copy_range(reader, 0, bytes.len() as u64));
    assert!(copied.contains("\"original_hex\":\"feff03b103b2000d000a\""), "{copied}");
    assert_eq!(fcb_reader_close(reader), 1);
    assert_eq!(fcb_file_finder_cancel(handle), 0);
    assert!(take(fcb_file_finder_info(handle)).contains("FILE_FINDER_HANDLE_UNKNOWN"));
}

#[test]
fn file_finder_c_input_bounds_precede_pointer_reads_and_empty_search_is_complete() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    for pointer in [fcb_file_finder_info(u64::MAX), fcb_file_finder_page(u64::MAX, 1, 1),
        fcb_file_finder_advance(u64::MAX, 1, 1, 16), fcb_file_finder_stop(u64::MAX, 1),
        fcb_file_finder_open_reader(u64::MAX, u64::MAX, 1, 1, 1, 1024)] {
        let reply = take(pointer);
        assert!(reply.len() < 4096); assert!(reply.contains("\"status\":\"error\""));
    }
    let handle = fcb_file_finder_create(); assert_ne!(handle, 0);
    let root = root(); let croot = CString::new(root.to_str().unwrap()).unwrap();
    let dangling = std::ptr::NonNull::<u8>::dangling().as_ptr();
    for length in [257, u64::MAX] {
        let reply = take(unsafe { fcb_file_finder_open(handle, croot.as_ptr(), 1,
            dangling, length, 2, 0, 32) });
        assert!(reply.contains("FILE_FINDER_HANDLE_INVALID_ARGUMENT"));
    }
    for (mode, case) in [(3, 0), (0, 2)] {
        let reply = take(unsafe { fcb_file_finder_open(handle, croot.as_ptr(), 1,
            dangling, 1, mode, case, 32) });
        assert!(reply.contains("FILE_FINDER_HANDLE_INVALID_ARGUMENT"));
    }
    assert!(take(fcb_file_finder_info(handle)).contains("FILE_FINDER_HANDLE_NOT_OPEN"));
    let query = b"missing";
    let initial = take(unsafe { fcb_file_finder_open(handle, croot.as_ptr(), 1,
        query.as_ptr(), query.len() as u64, 2, 0, 32) });
    assert!(initial.contains("\"discovery_complete\":true"));
    assert!(initial.contains("\"search_complete\":false"));
    let complete = take(fcb_file_finder_advance(handle, 1, 1, 16));
    assert!(complete.contains("\"search_complete\":true"), "{complete}");
    assert!(complete.contains("\"hits\":[]"));
    assert!(complete.contains("\"more\":false"));
    assert!(take(fcb_file_finder_page(handle, 2, number(&complete, "revision"))).contains("\"status\":\"error\""));
    assert_eq!(fcb_file_finder_close(handle), 1);
}

#[test]
fn file_finder_c_modes_use_exact_prefix_fuzzy_and_explicit_case_mapping() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root();
    for name in ["nd", "nd_tail", "n_e_d", "ND"] { fs::write(root.join(name), b"source").unwrap(); }
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let query = b"nd";
    for (mode, match_case, expected) in [(0, 1, 1), (1, 1, 2), (2, 1, 3), (0, 0, 2)] {
        let handle = fcb_file_finder_create(); assert_ne!(handle, 0);
        let initial = take(unsafe { fcb_file_finder_open(handle, croot.as_ptr(), 1,
            query.as_ptr(), query.len() as u64, mode, match_case, 32) });
        assert!(initial.contains("\"status\":\"ok\""), "{initial}");
        let report = take(fcb_file_finder_advance(handle, 1, 1, 16));
        assert_eq!(number(&report, "matches_seen"), expected, "{report}");
        assert_eq!(fcb_file_finder_close(handle), 1);
    }
}

#[test]
fn file_finder_c_signatures_match_public_header() {
    let _: extern "C" fn() -> u64 = fcb_file_finder_create;
    let _: unsafe extern "C" fn(u64, *const c_char, u64, *const u8, u64, u32, u8, u64) -> *mut c_char = fcb_file_finder_open;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_file_finder_info;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_file_finder_page;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_file_finder_advance;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_file_finder_stop;
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_file_finder_open_reader;
    let _: extern "C" fn(u64) -> u8 = fcb_file_finder_cancel;
    let _: extern "C" fn(u64) -> u8 = fcb_file_finder_close;
    let header = include_str!("../include/fcb_file_finder.h");
    for name in ["create", "open", "info", "page", "advance", "stop", "open_reader", "cancel", "close"] {
        let symbol = format!("fcb_file_finder_{name}(");
        assert_eq!(header.matches(&symbol).count(), 1, "{symbol}");
    }
}

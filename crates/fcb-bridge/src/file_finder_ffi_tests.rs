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

fn policy_finder_drain(handle: u64) -> String {
    let mut report = take(fcb_file_finder_info(handle));
    for _ in 0..64 {
        assert!(report.contains("\"status\":\"ok\""), "{report}");
        if report.contains("\"more\":false") { return report; }
        let revision = number(&report, "revision");
        report = take(fcb_file_finder_advance(handle, 11, revision, 16));
        assert_eq!(take(fcb_file_finder_advance(handle, 11, revision, 16)), report);
    }
    panic!("small policy fixture did not finish within its discovery bound");
}

#[test]
fn file_finder_policy_opt_in_discovers_and_captures_generated_source_without_broadening_default_queries() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root();
    fs::write(root.join(".gitignore"), b"ignored.rs\n").unwrap();
    fs::write(root.join("visible.rs"), b"ordinary source").unwrap();
    fs::write(root.join("ignored.rs"), b"ignored source").unwrap();
    fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("target/generated.rs"), b"generated source").unwrap();
    fs::create_dir(root.join(".fcb-cache")).unwrap();
    fs::write(root.join(".fcb-cache/private.rs"), b"owned artifact").unwrap();
    std::os::unix::fs::symlink(root.join("ignored.rs"), root.join("alias.rs")).unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let ordinary = fcb_file_finder_create(); assert_ne!(ordinary, 0);
    let initial = take(unsafe { fcb_file_finder_open(ordinary, croot.as_ptr(), 11,
        b".rs".as_ptr(), 3, 2, 1, 32) });
    assert!(initial.contains("\"policy\":\"repository-rules-v1\""));
    let default_report = policy_finder_drain(ordinary);
    assert_eq!(number(&default_report, "matches_seen"), 1, "{default_report}");

    let handle = fcb_file_finder_create(); assert_ne!(handle, 0); assert_ne!(handle, ordinary);
    let initial = take(unsafe { fcb_file_finder_open_with_policy(handle, croot.as_ptr(), 11,
        b".rs".as_ptr(), 3, 2, 1, 32, 1) });
    assert!(initial.contains("\"status\":\"ok\""), "{initial}");
    assert!(initial.contains("\"policy\":\"all-regular-files/no-rule-files-v1\""));
    let report = policy_finder_drain(handle);
    assert!(report.contains("\"search_complete\":true"), "{report}");
    assert!(report.contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
    assert_eq!(number(&report, "matches_seen"), 3);
    assert_eq!(number(&report, "retained_hits"), 3);
    assert_eq!(number(&report, "symlinks_not_followed"), 1);
    assert_eq!(number(&report, "discovery_source_bytes_read"), 0);
    let revision = number(&report, "revision");
    let file = report.split("\"file_id\":\"").skip(1).find_map(|row| {
        row.contains("\"hex\":\"7461726765742f67656e6572617465642e7273\"")
            .then(|| row.split('"').next().unwrap().parse::<u64>().unwrap())
    }).unwrap();
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let limited = take(fcb_file_finder_open_reader(handle, reader, 11, revision, file, 4));
    assert!(limited.contains("\"status\":\"error\""), "{limited}");
    let receipt = take(fcb_file_finder_open_reader(handle, reader, 11, revision, file, 1024));
    assert!(receipt.contains("\"status\":\"ok\""), "{receipt}");
    fs::write(root.join("target/generated.rs"), b"changed after capture").unwrap();
    assert_eq!(fcb_file_finder_close(handle), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 9)).contains("\"original_hex\":\"67656e657261746564\""));
    assert_eq!(fcb_reader_close(reader), 1);
    let still_default = take(fcb_file_finder_info(ordinary));
    assert!(still_default.contains("\"policy\":\"repository-rules-v1\""));
    assert_eq!(number(&still_default, "matches_seen"), 1);
    assert_eq!(fcb_file_finder_close(ordinary), 1);
}

#[test]
fn file_finder_policy_rejects_unknown_values_before_pointers_and_keeps_limits_without_rule_reads() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root();
    fs::write(root.join(".gitignore"), [0xff]).unwrap();
    fs::write(root.join("kept-one.rs"), b"first source").unwrap();
    fs::write(root.join("kept-two.rs"), b"second source").unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let ordinary = fcb_file_finder_create(); assert_ne!(ordinary, 0);
    let initial = take(unsafe { fcb_file_finder_open(ordinary, croot.as_ptr(), 11,
        b"kept".as_ptr(), 4, 1, 1, 32) });
    assert!(initial.contains("RULE_INVALID_ENCODING"), "{initial}");
    let report = policy_finder_drain(ordinary);
    assert!(report.contains("\"search_complete\":false"));
    assert_eq!(number(&report, "matches_seen"), 0);
    assert_eq!(fcb_file_finder_close(ordinary), 1);

    let handle = fcb_file_finder_create(); assert_ne!(handle, 0);
    let dangling = std::ptr::NonNull::<u8>::dangling().as_ptr();
    for policy in [2, u64::MAX] {
        let error = take(unsafe { fcb_file_finder_open_with_policy(handle, dangling.cast(), 11,
            dangling, 1, 0, 1, 32, policy) });
        assert!(error.len() < 4096);
        assert!(error.contains("FILE_FINDER_HANDLE_INVALID_ARGUMENT"), "{error}");
        assert!(take(fcb_file_finder_info(handle)).contains("FILE_FINDER_HANDLE_NOT_OPEN"));
    }
    for (length, mode) in [(257, 0), (1, 3)] {
        let error = take(unsafe { fcb_file_finder_open_with_policy(handle, dangling.cast(), 11,
            dangling, length, mode, 1, 32, 1) });
        assert!(error.contains("FILE_FINDER_HANDLE_INVALID_ARGUMENT"), "{error}");
    }
    let initial = take(unsafe { fcb_file_finder_open_with_policy(handle, croot.as_ptr(), 11,
        b"kept".as_ptr(), 4, 1, 1, 1, 1) });
    assert!(initial.contains("\"status\":\"ok\""), "{initial}");
    assert!(initial.contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
    assert!(!initial.contains("configuration_bytes_read"));
    let report = policy_finder_drain(handle);
    assert!(report.contains("\"search_complete\":true"), "{report}");
    assert!(report.contains("\"truncated\":true"));
    assert_eq!(number(&report, "matches_seen"), 2);
    assert_eq!(number(&report, "retained_hits"), 1);
    assert_eq!(number(&report, "max_results"), 1);
    assert!(take(unsafe { fcb_file_finder_open_with_policy(handle, croot.as_ptr(), 11,
        b"kept".as_ptr(), 4, 1, 1, 32, 0) }).contains("FILE_FINDER_HANDLE_ALREADY_OPEN"));
    assert!(take(fcb_file_finder_info(handle)).contains("\"policy\":\"all-regular-files/no-rule-files-v1\""));
    assert_eq!(fcb_file_finder_close(handle), 1);

    let _: unsafe extern "C" fn(u64, *const c_char, u64, *const u8, u64, u32, u8,
        u64, u64) -> *mut c_char = fcb_file_finder_open_with_policy;
    assert_eq!(include_str!("../include/fcb_file_finder.h").matches("fcb_file_finder_open_with_policy(").count(), 1);
}

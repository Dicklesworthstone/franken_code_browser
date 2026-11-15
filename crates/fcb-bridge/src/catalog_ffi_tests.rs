#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::reader_ffi::{fcb_reader_create, fcb_reader_open, fcb_reader_info,
    fcb_reader_copy_range, fcb_reader_close, FFI_TEST_LOCK};
use super::super::reader_ffi::document::{fcb_reader_document, fcb_reader_document_links_prepare};
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
        fcb_catalog_open_reader(u64::MAX, u64::MAX, 1, 1, 1024),
        fcb_catalog_open_document_link(u64::MAX, 1, 2, 1, 1, 1, 1, 1024),
        fcb_catalog_open_verified_document_link(u64::MAX, 1, 2, 1, 1, 1024)] {
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
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_catalog_open_document_link;
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_catalog_open_verified_document_link;
    let _: extern "C" fn(u64) -> u8 = fcb_catalog_cancel;
    let _: extern "C" fn(u64) -> u8 = fcb_catalog_close;
    let header = include_str!("../include/fcb_project_catalog.h");
    for symbol in ["fcb_catalog_create", "fcb_catalog_open", "fcb_catalog_info", "fcb_catalog_page",
        "fcb_catalog_advance", "fcb_catalog_stop", "fcb_catalog_open_reader", "fcb_catalog_open_document_link",
        "fcb_catalog_open_verified_document_link", "fcb_catalog_cancel", "fcb_catalog_close"] {
        assert_eq!(header.matches(&format!("{symbol}(")).count(), 1, "{symbol}");
    }
}

#[test]
fn actual_c_catalog_link_transfer_keeps_origin_and_target_after_catalog_closure() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root();
    fs::write(root.join("README.md"), b"# Origin\n\n[Next](next.md#details)\n").unwrap();
    let bytes = b"\xef\xbb\xbfIntro.\r\n\r\n# Details\r\n\r\nLinked capture.\r\n";
    let heading = bytes.windows(b"# Details".len()).position(|part| part == b"# Details").unwrap() as u64;
    fs::write(root.join("next.md"), bytes).unwrap();
    let handle = fcb_catalog_create(); assert_ne!(handle, 0);
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let page = take(unsafe { fcb_catalog_open(handle, croot.as_ptr()) });
    let source = page.split("\"file_id\":\"").skip(1).find_map(|row| {
        row.contains("\"hex\":\"524541444d452e6d64\"")
            .then(|| row.split('"').next().unwrap().parse::<u64>().unwrap())
    }).unwrap();
    let origin = fcb_reader_create(); let target = fcb_reader_create();
    assert_ne!(origin, 0); assert_ne!(target, 0);
    assert!(take(fcb_catalog_open_reader(handle, origin, 1, source, 4096)).contains("\"status\":\"ok\""));
    assert!(take(fcb_reader_document(origin, 1, 80, 64 * 1024, 8192, 8192, 4096)).contains("\"status\":\"ok\""));
    assert!(take(fcb_reader_document_links_prepare(origin, 1, 16)).contains("\"retained_links\":\"1\""));
    fs::write(root.join("README.md"), b"[Changed](unobserved.md)").unwrap();
    let receipt_pointer = fcb_catalog_open_document_link(handle, origin, target, 1, source, 1, 1, 4096);
    assert_eq!(fcb_catalog_close(handle), 1);
    fs::write(root.join("next.md"), b"new live target").unwrap();
    let receipt = take(receipt_pointer);
    assert!(receipt.contains("\"source_observation\":\"new-capture-after-document-link\""), "{receipt}");
    assert!(receipt.contains("\"fragment_status\":\"resolved\""), "{receipt}");
    assert!(receipt.contains(&format!("\"initial_original_offset\":\"{heading}\"")), "{receipt}");
    assert!(receipt.contains("Linked capture."), "{receipt}");
    assert!(take(fcb_reader_copy_range(origin, 0, 8)).contains("\"original_hex\":\"23204f726967696e\""));
    assert!(take(fcb_reader_copy_range(target, heading, heading + 9)).contains("\"original_hex\":\"232044657461696c73\""));
    assert!(take(fcb_reader_document(target, 1, 80, 64 * 1024, 8192, 8192, 4096)).contains("\"status\":\"ok\""));
    assert_eq!(fcb_reader_close(target), 1); assert_eq!(fcb_reader_close(origin), 1);
}

#[test]
fn c_verified_links_bind_an_independent_reader_to_current_source_and_keep_old_bytes() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root();
    let original = b"# Origin\n\n[Next](later.md#details)\n";
    fs::write(root.join("README.md"), original).unwrap();
    let handle = fcb_catalog_create(); assert_ne!(handle, 0);
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    assert!(take(unsafe { fcb_catalog_open(handle, croot.as_ptr()) }).contains("\"status\":\"ok\""));
    // This absolute-label reader was not opened by the catalog. Its target was
    // not present during discovery either: verification must not require a row.
    let origin = fcb_reader_create(); let target = fcb_reader_create();
    assert_ne!(origin, 0); assert_ne!(target, 0);
    let source = CString::new(root.join("README.md").to_str().unwrap()).unwrap();
    assert!(take(unsafe { fcb_reader_open(origin, source.as_ptr(), 4096) }).contains("\"status\":\"ok\""));
    assert!(take(fcb_reader_document(origin, 1, 80, 64 * 1024, 8192, 8192, 4096)).contains("\"status\":\"ok\""));
    assert!(take(fcb_reader_document_links_prepare(origin, 1, 16)).contains("\"status\":\"ok\""));
    fs::write(root.join("later.md"), b"Intro.\n\n# Details\n\nExact target.\n").unwrap();
    let receipt = take(fcb_catalog_open_verified_document_link(handle, origin, target, 1, 1, 4096));
    assert!(receipt.contains("\"source_authority\":\"verified-current-source\""), "{receipt}");
    assert!(receipt.contains("\"target_resolution\":\"root-grant-path\""), "{receipt}");
    assert!(receipt.contains("\"target_page_id\":\"0\""), "{receipt}");
    assert!(receipt.contains("\"fragment_status\":\"resolved\""), "{receipt}");
    assert!(receipt.contains("Exact target."), "{receipt}");
    let empty = fcb_reader_create(); assert_ne!(empty, 0);
    fs::write(root.join("README.md"), b"# Changed\n\n[Other](other.md)\n").unwrap();
    let stale = take(fcb_catalog_open_verified_document_link(handle, origin, empty, 1, 1, 4096));
    assert!(stale.contains("CATALOG_SESSION_ORIGIN_CAPTURE_CHANGED"), "{stale}");
    assert!(take(fcb_reader_info(empty)).contains("READER_HANDLE_NOT_OPEN"));
    assert!(take(fcb_reader_copy_range(origin, 0, 8)).contains("\"original_hex\":\"23204f726967696e\""));
    assert!(take(fcb_reader_copy_range(target, 8, 17)).contains("\"original_hex\":\"232044657461696c73\""));
    assert_eq!(fcb_catalog_close(handle), 1);
    assert_eq!(fcb_reader_close(empty), 1); assert_eq!(fcb_reader_close(target), 1);
    assert_eq!(fcb_reader_close(origin), 1);
}

fn policy_number(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

fn policy_catalog_pages(handle: u64, mut page: String) -> Vec<String> {
    let mut pages = Vec::new();
    for _ in 0..64 {
        assert!(page.contains("\"status\":\"ok\""), "{page}");
        assert!(policy_number(&page, "retained_pages") <= 4);
        assert!(policy_number(&page, "retained_files") <= 1024);
        let more = page.contains("\"more\":true");
        let after = policy_number(&page, "latest_page");
        pages.push(page);
        if !more { return pages; }
        page = take(fcb_catalog_advance(handle, after));
        // Replaying an accepted predecessor must not consume another page.
        assert_eq!(take(fcb_catalog_advance(handle, after)), page);
    }
    panic!("small policy fixture did not finish within its discovery bound");
}

#[test]
fn catalog_policy_opt_in_opens_ignored_and_generated_files_but_keeps_source_boundaries() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let outside = root();
    fs::write(outside.join("escape.rs"), b"outside root").unwrap();
    // Keep the socket fixture short enough for macOS's sockaddr_un path.
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcbpc-{}-{stamp}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join(".gitignore"), b"ignored.rs\n").unwrap();
    fs::write(root.join("visible.rs"), b"ordinary source").unwrap();
    fs::write(root.join("ignored.rs"), b"retained ignored source").unwrap();
    fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("target/generated.rs"), b"generated source").unwrap();
    fs::create_dir(root.join(".fcb-cache")).unwrap();
    fs::write(root.join(".fcb-cache/private.rs"), b"owned artifact").unwrap();
    fs::write(root.join(".fcb.db"), b"owned database").unwrap();
    std::os::unix::fs::symlink(outside.join("escape.rs"), root.join("escape.rs")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(root.join("s")).unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let ignored_hex = "\"hex\":\"69676e6f7265642e7273\"";
    let generated_hex = "\"hex\":\"7461726765742f67656e6572617465642e7273\"";

    let ordinary = fcb_catalog_create(); assert_ne!(ordinary, 0);
    let pages = policy_catalog_pages(ordinary, take(unsafe { fcb_catalog_open(ordinary, croot.as_ptr()) }));
    assert!(pages.iter().all(|page| page.contains("\"policy\":\"repository-rules-v1\"")));
    assert!(pages.iter().all(|page| !page.contains(ignored_hex) && !page.contains(generated_hex)));
    assert_eq!(policy_number(pages.last().unwrap(), "catalogued_files"), 2);

    let handle = fcb_catalog_create(); assert_ne!(handle, 0); assert_ne!(handle, ordinary);
    let pages = policy_catalog_pages(handle, take(unsafe { fcb_catalog_open_with_policy(handle, croot.as_ptr(), 1) }));
    assert!(pages.iter().all(|page| page.contains("\"policy\":\"all-regular-files/no-rule-files-v1\"")));
    assert!(pages.iter().all(|page| page.contains("\"rule_files_enabled\":false,\"rule_policy\":null")));
    assert!(pages.iter().any(|page| page.contains(generated_hex)));
    let complete = pages.last().unwrap();
    assert!(complete.contains("\"discovery_complete\":true"), "{complete}");
    assert_eq!(policy_number(complete, "catalogued_files"), 4);
    assert_eq!(policy_number(complete, "symlinks_not_followed"), 1);
    assert_eq!(policy_number(complete, "special_objects"), 1);
    assert_eq!(policy_number(complete, "discovery_source_bytes_read"), 0);
    assert!(complete.contains("\"page_entries\":\"256\""));
    let page = pages.iter().find(|page| page.contains(ignored_hex)).unwrap();
    let file = page.split("\"file_id\":\"").skip(1).find_map(|row| {
        row.contains(ignored_hex).then(|| row.split('"').next().unwrap().parse::<u64>().unwrap())
    }).unwrap();
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let capture = take(fcb_catalog_open_reader(handle, reader, policy_number(page, "page_id"), file, 1024));
    assert!(capture.contains("\"status\":\"ok\""), "{capture}");
    fs::write(root.join("ignored.rs"), b"changed after capture").unwrap();
    assert_eq!(fcb_catalog_close(handle), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 8)).contains("\"original_hex\":\"72657461696e6564\""));
    assert_eq!(fcb_reader_close(reader), 1);
    // The opt-in never widens the still-open default catalog's authority.
    assert!(take(fcb_catalog_info(ordinary)).contains("\"policy\":\"repository-rules-v1\""));
    assert_eq!(fcb_catalog_close(ordinary), 1);
}

#[test]
fn catalog_policy_bypasses_malformed_ignore_configuration_and_rejects_unknown_policy_before_pointers() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root();
    fs::write(root.join(".gitignore"), [0xff]).unwrap();
    fs::write(root.join("kept.rs"), b"retained source").unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let ordinary = fcb_catalog_create(); assert_ne!(ordinary, 0);
    let failed_rules = take(unsafe { fcb_catalog_open(ordinary, croot.as_ptr()) });
    assert!(failed_rules.contains("RULE_INVALID_ENCODING"), "{failed_rules}");
    assert!(failed_rules.contains("\"discovery_complete\":false"));
    assert_eq!(fcb_catalog_close(ordinary), 1);

    let handle = fcb_catalog_create(); assert_ne!(handle, 0);
    let dangling = std::ptr::NonNull::<c_char>::dangling().as_ptr();
    for policy in [2, u64::MAX] {
        let error = take(unsafe { fcb_catalog_open_with_policy(handle, dangling, policy) });
        assert!(error.len() < 4096);
        assert!(error.contains("CATALOG_HANDLE_INVALID_ARGUMENT"), "{error}");
        assert!(take(fcb_catalog_info(handle)).contains("CATALOG_HANDLE_NOT_OPEN"));
    }
    let pages = policy_catalog_pages(handle, take(unsafe { fcb_catalog_open_with_policy(handle, croot.as_ptr(), 1) }));
    let complete = pages.last().unwrap();
    assert!(complete.contains("\"discovery_complete\":true"), "{complete}");
    assert!(complete.contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
    assert!(!complete.contains("configuration_bytes_read"));
    assert_eq!(policy_number(complete, "catalogued_files"), 2);
    assert!(take(unsafe { fcb_catalog_open_with_policy(handle, croot.as_ptr(), 0) }).contains("CATALOG_HANDLE_ALREADY_OPEN"));
    assert!(take(fcb_catalog_info(handle)).contains("\"policy\":\"all-regular-files/no-rule-files-v1\""));
    assert_eq!(fcb_catalog_close(handle), 1);

    let _: unsafe extern "C" fn(u64, *const c_char, u64) -> *mut c_char = fcb_catalog_open_with_policy;
    assert_eq!(include_str!("../include/fcb_project_catalog.h").matches("fcb_catalog_open_with_policy(").count(), 1);
}

#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::reader_ffi::{fcb_reader_create, fcb_reader_info, fcb_reader_copy_range,
    fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString, OsString}, fs, os::unix::ffi::OsStringExt,
    sync::atomic::{AtomicU64, Ordering}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected bounded structural-search JSON");
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; result
}
fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-catalog-structure-ffi-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn number(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn open(handle: u64, root: &CStr, query: &[u8], mode: u64) -> String {
    take(unsafe { fcb_catalog_structure_open(handle, root.as_ptr(), 11, query.as_ptr(), query.len() as u64,
        mode, 2, 0, std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 32, 64 * 1024 * 1024, 1_048_576) })
}
fn drain(handle: u64) -> String {
    let mut report = take(fcb_catalog_structure_info(handle));
    for _ in 0..10_000 {
        assert!(report.contains("\"status\":\"ok\""), "{report}");
        if report.contains("\"more\":false") { return report; }
        report = take(fcb_catalog_structure_advance(handle, 11, number(&report, "revision"), 16));
    }
    panic!("bounded C structural query did not finish: {report}");
}

#[test]
fn actual_c_api_retains_raw_path_utf16_symbols_and_installs_an_independent_reader() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root(); let path = root.join(OsString::from_vec(b"raw-\xff.rs".to_vec()));
    let bytes: Vec<u8> = "\u{feff}fn target() {}\n".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fs::write(&path, &bytes).unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let handle = fcb_catalog_structure_create(); assert_ne!(handle, 0);
    let mut query = b"target".to_vec();
    fs::write(root.join("raw-�.rs"), b"fn target() {}\n").unwrap();
    let scope = CString::new(b"raw-\xff.rs".to_vec()).unwrap();
    let initial = take(unsafe { fcb_catalog_structure_open(handle, croot.as_ptr(), 11,
        query.as_ptr(), query.len() as u64, 0, 2, 0, std::ptr::null(), std::ptr::null(),
        2, scope.as_ptr(), 32, 64 * 1024 * 1024, 1_048_576) });
    drop(scope);
    assert!(initial.contains("\"scope_path\":null"), "{initial}");
    assert!(initial.contains("\"scope_path_hex\":\"7261772dff2e7273\""), "{initial}");
    assert!(initial.contains("\"schema\":\"fcb.catalog-structure/1\""));
    assert!(initial.contains("\"name_case\":\"sensitive\""));
    assert!(initial.contains("\"case_sensitive\":true"));
    assert_eq!(number(&initial, "payload_bytes_read"), 0);
    query.fill(b'x');
    let complete = drain(handle); let revision = number(&complete, "revision");
    assert_eq!(number(&complete, "matches_seen"), 1, "{complete}");
    assert!(complete.contains("\"search_complete\":true"));
    let page = take(fcb_catalog_structure_page(handle, 11, revision, 0, 32));
    assert!(page.contains("7261772dff2e7273"));
    assert!(page.contains("\"encoding\":\"utf16be\""));
    assert!(page.contains("\"realized_mode\":\"heuristic-outline-candidate\""));
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    assert!(take(fcb_catalog_structure_open_hit_reader(handle, reader, 12, revision, 0))
        .contains("CATALOG_STRUCTURE_STALE_QUERY"));
    assert!(take(fcb_reader_info(reader)).contains("READER_HANDLE_NOT_OPEN"));
    fs::remove_file(path).unwrap();
    let receipt = take(fcb_catalog_structure_open_hit_reader(handle, reader, 11, revision, 0));
    assert!(receipt.contains("\"activation_source\":\"retained-search-capture\""), "{receipt}");
    assert_eq!(number(&receipt, "reader_owner"), reader);
    let pointer = fcb_catalog_structure_hit(handle, 11, revision, 0);
    assert_eq!(fcb_catalog_structure_close(handle), 1);
    let hit = take(pointer);
    assert!(hit.contains("\"original_hex\":\"007400610072006700650074\""), "{hit}");
    assert!(hit.contains("\"additional_source_bytes_read\":\"0\""));
    let copied = take(fcb_reader_copy_range(reader, 0, bytes.len() as u64));
    let expected = bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    assert!(copied.contains(&format!("\"original_hex\":\"{expected}\"")));
    assert_eq!(fcb_reader_close(reader), 1);
    assert_eq!(fcb_catalog_structure_cancel(handle), 0);
}

#[test]
fn invalid_scalars_precede_all_pointer_reads_and_cross_family_handles_refuse() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let handle = fcb_catalog_structure_create(); let reader = fcb_reader_create();
    assert_ne!(handle, 0); assert_ne!(reader, 0); assert_ne!(handle, reader);
    assert!(take(fcb_catalog_structure_info(reader)).contains("CATALOG_STRUCTURE_HANDLE_UNKNOWN"));
    let dangling = std::ptr::NonNull::<u8>::dangling().as_ptr();
    for (generation, length, mode, name_mode, offset, path_kind, hits, bytes, calls, policy) in [
        (0, 1, 0, 2, 0, 0, 32, 1024, 1024, 0),
        (1, 0, 0, 2, 0, 0, 32, 1024, 1024, 0),
        (1, 1025, 0, 2, 0, 0, 32, 1024, 1024, 0),
        (1, 1, 2, 2, 0, 0, 32, 1024, 1024, 0),
        (1, 1, 0, 3, 0, 0, 32, 1024, 1024, 0),
        (1, 1, 0, 2, u64::MAX, 0, 32, 1024, 1024, 0),
        (1, 1, 0, 2, 0, 4, 32, 1024, 1024, 0),
        (1, 1, 0, 2, 0, 0, 257, 1024, 1024, 0),
        (1, 1, 0, 2, 0, 0, 32, 0, 1024, 0),
        (1, 1, 0, 2, 0, 0, 32, 1024, 1_048_577, 0),
        (1, 1, 0, 2, 0, 0, 32, 1024, 1024, 2),
    ] {
        let error = take(unsafe { fcb_catalog_structure_open_with_policy(handle,
            dangling.cast(), generation, dangling, length, mode, name_mode, offset,
            dangling.cast(), dangling.cast(), path_kind, dangling.cast(), hits, bytes, calls, policy) });
        assert!(error.contains("CATALOG_STRUCTURE_HANDLE_INVALID_ARGUMENT"), "{error}");
    }
    for invalid_case in [2, u64::MAX] {
        let error = take(unsafe { fcb_catalog_structure_open_matching_with_policy(handle,
            dangling.cast(), 11, dangling, 1, 0, 2, 0, dangling.cast(), dangling.cast(),
            0, dangling.cast(), 32, 1024, 1024, invalid_case, 0) });
        assert!(error.contains("CATALOG_STRUCTURE_HANDLE_INVALID_ARGUMENT"), "{error}");
    }
    assert!(take(fcb_catalog_structure_info(handle)).contains("CATALOG_STRUCTURE_HANDLE_NOT_OPEN"));
    assert_eq!(fcb_catalog_structure_close(handle), 1);
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn c_heading_search_counts_beyond_rows_and_preserves_discovery_policy_and_scope() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root(); fs::create_dir(root.join("docs")).unwrap();
    fs::write(root.join(".gitignore"), b"docs/ignored.md\n").unwrap();
    fs::write(root.join("docs/ignored.md"), b"# target\n\n## target\n\n### target\n").unwrap();
    fs::write(root.join("outside.md"), b"# target\n").unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let extensions = CString::new("md").unwrap(); let directory = CString::new("docs").unwrap();
    let handle = fcb_catalog_structure_create();
    let initial = take(unsafe { fcb_catalog_structure_open_with_policy(handle, croot.as_ptr(), 11,
        b"target".as_ptr(), 6, 1, 2, 0, extensions.as_ptr(), std::ptr::null(), 1, directory.as_ptr(),
        1, 1024, 1024, 1) });
    assert_eq!(number(&initial, "payload_bytes_read"), 0);
    assert!(initial.contains("\"name_case\":\"sensitive\""));
    assert!(initial.contains("\"rule_files_enabled\":false"));
    let complete = drain(handle);
    assert_eq!(number(&complete, "matches_seen"), 3, "{complete}");
    assert_eq!(number(&complete, "retained_hits"), 1);
    assert_eq!(number(&complete, "eligible_files"), 1);
    assert!(complete.contains("\"search_complete\":true"));
    assert!(complete.contains("\"match_count_complete\":true"));
    assert!(complete.contains("\"truncated\":true"));
    assert!(complete.contains("\"realized_mode\":\"fmd-heading\""));
    assert!(complete.contains("all-regular-files/no-rule-files-v1"));
    assert!(take(fcb_catalog_structure_page(handle, 11, 1, 0, 1)).contains("CATALOG_STRUCTURE_STALE_REVISION"));
    assert_eq!(fcb_catalog_structure_close(handle), 1);
}

#[test]
fn explicit_case_policy_counts_unicode_names_and_preserves_retained_original_bytes() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root();
    fs::write(root.join(".gitignore"), b"ignored.rs\n").unwrap();
    let source: Vec<u8> = "\u{feff}fn Target() {}\nfn TARGET_more() {}\n"
        .encode_utf16().flat_map(u16::to_be_bytes).collect();
    fs::write(root.join("ignored.rs"), &source).unwrap();
    fs::write(root.join("headings.md"), "# CAFÉ\n\n## Café\n").unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    for (mode, query, extension, expected_hex) in [
        (0, "target", "rs", "005400610072006700650074"),
        (1, "café", "md", "2320434146c389"),
    ] {
        let extension = CString::new(extension).unwrap();
        let handle = fcb_catalog_structure_create(); assert_ne!(handle, 0);
        let initial = take(unsafe { fcb_catalog_structure_open_matching_with_policy(handle,
            croot.as_ptr(), 11, query.as_ptr(), query.len() as u64, mode, 1, 0,
            extension.as_ptr(), std::ptr::null(), 0, std::ptr::null(), 1, 1024, 1024, 0, 1) });
        assert_eq!(number(&initial, "payload_bytes_read"), 0);
        assert!(initial.contains("\"name_case\":\"unicode-lowercase\""));
        assert!(initial.contains("\"case_sensitive\":false"));
        assert!(initial.contains("\"rule_files_enabled\":false"));
        let complete = drain(handle);
        assert_eq!(number(&complete, "matches_seen"), 2, "{complete}");
        assert_eq!(number(&complete, "retained_hits"), 1);
        assert_eq!(number(&complete, "eligible_files"), 1);
        assert!(complete.contains("\"match_count_complete\":true"));
        assert!(complete.contains("\"page_has_more_matches\":true"));
        let hit = take(fcb_catalog_structure_hit(handle, 11, number(&complete, "revision"), 0));
        assert!(hit.contains(&format!("\"original_hex\":\"{expected_hex}")), "{hit}");
        assert_eq!(fcb_catalog_structure_close(handle), 1);
    }
}

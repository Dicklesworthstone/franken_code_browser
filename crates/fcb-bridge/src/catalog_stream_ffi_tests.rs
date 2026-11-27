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
    let root = root();
    let directory = root.join(OsString::from_vec(b"src-\xff".to_vec()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join(OsString::from_vec(b"raw-\xff.rs".to_vec()));
    let sibling = root.join(OsString::from_vec(b"src-\xff-old".to_vec()));
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("decoy.rs"), b"needle").unwrap();
    let bytes: Vec<u8> = "\u{feff}before needle after\r\n".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fs::write(&path, bytes).unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let handle = fcb_catalog_stream_create(); assert_ne!(handle, 0);
    let mut query = b"needle".to_vec();
    let scope = CString::new(b"src-\xff".to_vec()).unwrap();
    let initial = take(unsafe { fcb_catalog_stream_open(handle, croot.as_ptr(), 11,
        query.as_ptr(), query.len() as u64, 0, 1, 1, 0, std::ptr::null(), std::ptr::null(),
        1, scope.as_ptr(), 32, 64 * 1024 * 1024, 1_048_576) });
    drop(scope);
    assert!(initial.contains("\"scope_path\":null"), "{initial}");
    assert!(initial.contains("\"scope_path_hex\":\"7372632dff\""), "{initial}");
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

#[test]
fn catalog_stream_policy_searches_excluded_sources_with_exact_witnesses_and_unchanged_hit_limits() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    // Keep the socket fixture short enough for macOS's sockaddr_un path.
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcbps-{}-{stamp}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join(".gitignore"), b"ignored.rs\n").unwrap();
    fs::write(root.join("visible.rs"), b"needle ordinary").unwrap();
    fs::write(root.join("ignored.rs"), b"needle ignored").unwrap();
    fs::create_dir(root.join("target")).unwrap();
    let generated: Vec<u8> = "\u{feff}needle generated".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fs::write(root.join("target/generated.rs"), generated).unwrap();
    fs::create_dir(root.join(".fcb-cache")).unwrap();
    fs::write(root.join(".fcb-cache/private.rs"), b"needle owned artifact").unwrap();
    std::os::unix::fs::symlink(root.join("ignored.rs"), root.join("alias.rs")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(root.join("s")).unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let ordinary = fcb_catalog_stream_create(); assert_ne!(ordinary, 0);
    assert!(open(ordinary, &croot, b"needle", 0, 1).contains("\"discovery_policy\":\"repository-rules-v1\""));
    let default_report = drain(ordinary);
    assert_eq!(number(&default_report, "matches_seen"), 1, "{default_report}");
    assert_eq!(fcb_catalog_stream_close(ordinary), 1);

    let extensions = CString::new("rs").unwrap();
    for limit in [1, 32] {
        let handle = fcb_catalog_stream_create(); assert_ne!(handle, 0);
        let initial = take(unsafe { fcb_catalog_stream_open_with_policy(handle, croot.as_ptr(), 11,
            b"needle".as_ptr(), 6, 0, 1, 1, 0, extensions.as_ptr(), std::ptr::null(),
            0, std::ptr::null(), limit, 1024, 1024, 1) });
        assert!(initial.contains("\"status\":\"ok\""), "{initial}");
        assert!(initial.contains("\"discovery_policy\":\"all-regular-files/no-rule-files-v1\""));
        assert!(initial.contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
        assert_eq!(number(&initial, "payload_bytes_read"), 0);
        let report = drain(handle);
        assert!(report.contains("\"search_complete\":true"), "{report}");
        assert_eq!(number(&report, "matches_seen"), 3);
        assert_eq!(number(&report, "retained_hits"), limit.min(3));
        assert_eq!(number(&report, "max_matches"), limit);
        assert_eq!(number(&report, "symlinks_not_followed"), 1);
        assert_eq!(number(&report, "special_objects"), 1);
        if limit == 1 {
            assert!(report.contains("\"truncated\":true"));
            assert_eq!(fcb_catalog_stream_close(handle), 1);
            continue;
        }
        assert!(report.contains("\"hex\":\"7461726765742f67656e6572617465642e7273\""));
        assert!(report.contains("\"encoding\":\"utf16be\""));
        let ignored = report.split("\"index\":\"").skip(1).find_map(|row| {
            row.contains("\"hex\":\"69676e6f7265642e7273\"")
                .then(|| row.split('"').next().unwrap().parse::<u64>().unwrap())
        }).unwrap();
        let revision = number(&report, "revision");
        fs::write(root.join("ignored.rs"), b"changed after the search").unwrap();
        let pointer = fcb_catalog_stream_hit(handle, 11, revision, ignored);
        assert_eq!(fcb_catalog_stream_close(handle), 1);
        let witness = take(pointer);
        assert!(witness.contains("\"original_hex\":\"6e6565646c65\""), "{witness}");
        assert!(witness.contains("\"additional_source_bytes_read\":\"0\""));
    }
}

#[test]
fn catalog_stream_policy_rejects_unknown_values_before_pointers_and_ignores_malformed_configuration() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root();
    fs::write(root.join(".gitignore"), [0xff]).unwrap();
    fs::write(root.join("kept.rs"), b"needle").unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let ordinary = fcb_catalog_stream_create(); assert_ne!(ordinary, 0);
    let initial = open(ordinary, &croot, b"needle", 0, 1);
    assert!(initial.contains("RULE_INVALID_ENCODING"), "{initial}");
    let report = drain(ordinary);
    assert!(report.contains("\"search_complete\":false"));
    assert_eq!(number(&report, "matches_seen"), 0);
    assert_eq!(fcb_catalog_stream_close(ordinary), 1);

    let handle = fcb_catalog_stream_create(); assert_ne!(handle, 0);
    let dangling = std::ptr::NonNull::<u8>::dangling().as_ptr();
    for policy in [2, u64::MAX] {
        let error = take(unsafe { fcb_catalog_stream_open_with_policy(handle, dangling.cast(), 11,
            dangling, 1, 0, 1, 1, 0, dangling.cast(), dangling.cast(), 0, dangling.cast(),
            32, 1024, 1024, policy) });
        assert!(error.len() < 4096);
        assert!(error.contains("CATALOG_STREAM_HANDLE_INVALID_ARGUMENT"), "{error}");
        assert!(take(fcb_catalog_stream_info(handle)).contains("CATALOG_STREAM_HANDLE_NOT_OPEN"));
    }
    // The broader discovery scope cannot relax the pre-pointer scalar bounds.
    for (length, limit, bytes) in [(1025, 32, 1024), (1, 257, 1024), (1, 32, 0)] {
        let error = take(unsafe { fcb_catalog_stream_open_with_policy(handle, dangling.cast(), 11,
            dangling, length, 0, 1, 1, 0, dangling.cast(), dangling.cast(), 0, dangling.cast(),
            limit, bytes, 1024, 1) });
        assert!(error.contains("CATALOG_STREAM_HANDLE_INVALID_ARGUMENT"), "{error}");
    }
    let extensions = CString::new("rs").unwrap();
    let initial = take(unsafe { fcb_catalog_stream_open_with_policy(handle, croot.as_ptr(), 11,
        b"needle".as_ptr(), 6, 0, 1, 1, 0, extensions.as_ptr(), std::ptr::null(),
        0, std::ptr::null(), 32, 1024, 1024, 1) });
    assert!(initial.contains("\"status\":\"ok\""), "{initial}");
    assert!(initial.contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
    assert!(!initial.contains("configuration_bytes_read"));
    let report = drain(handle);
    assert!(report.contains("\"search_complete\":true"), "{report}");
    assert_eq!(number(&report, "matches_seen"), 1);
    assert_eq!(number(&report, "payload_bytes_read"), 6);
    assert_eq!(fcb_catalog_stream_close(handle), 1);

    let _: unsafe extern "C" fn(u64, *const c_char, u64, *const u8, u64, u64, u64,
        u64, u64, *const c_char, *const c_char, u64, *const c_char, u64, u64, u64,
        u64) -> *mut c_char = fcb_catalog_stream_open_with_policy;
    assert_eq!(include_str!("../include/fcb_catalog_stream.h").matches("fcb_catalog_stream_open_with_policy(").count(), 1);
}

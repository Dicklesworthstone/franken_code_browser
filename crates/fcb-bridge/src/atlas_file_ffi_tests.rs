#![deny(unsafe_op_in_unsafe_fn)]

//! Exact-member ABI covers preflight, original grants and capture ownership.
use super::*;
use crate::reader_ffi::{FFI_TEST_LOCK, fcb_reader_create, fcb_reader_close, fcb_reader_copy_range};
use std::{ffi::{CStr, CString, OsString, c_void}, fs,
    os::unix::{ffi::OsStringExt, fs::symlink}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};

fn fixture_root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!("fcb-file-search-ffi-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
struct Poll { count: AtomicU64, stop_at: u64 }
unsafe extern "C" fn poll(context: *mut c_void) -> i32 {
    let state = unsafe { &*context.cast::<Poll>() };
    i32::from(state.count.fetch_add(1, Ordering::Relaxed) + 1 >= state.stop_at)
}
fn context(state: &Poll) -> *mut c_void { std::ptr::from_ref(state).cast_mut().cast() }
fn open(handle: u64, root: &CString, file: &[u8], policy: u64) -> *mut c_char {
    unsafe { fcb_atlas_open_file_with_policy_cancelable(handle, root.as_ptr(),
        file.as_ptr(), file.len() as u64, 1, policy, None, std::ptr::null_mut()) }
}

#[test]
fn file_abi_preflight_rejects_invalid_numbers_without_polling_or_mutating_the_handle() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, *const c_char, *const u8, u64, u64, u64,
        Option<crate::search_ffi::SearchCancellationCallback>, *mut c_void) -> *mut c_char
        = fcb_atlas_open_file_with_policy_cancelable;
    assert_eq!(include_str!("../include/fcb_atlas_sessions.h")
        .matches("fcb_atlas_open_file_with_policy_cancelable(").count(), 1);
    let handle = fcb_atlas_create(); assert_ne!(handle, 0);
    let empty = take(fcb_atlas_info(handle));
    let state = Poll { count: AtomicU64::new(0), stop_at: u64::MAX };
    let root = CString::new("/unused-file-open").unwrap(); let file = [b'x'; 2049];
    for (length, files, policy) in [(1, 1, 2), (1, 1, u64::MAX), (0, 1, 0),
        (2049, 1, 0), (u64::MAX, 1, 1), (1, 0, 0), (1, 20_001, 1), (1, u64::MAX, 0)] {
        assert!(unsafe { fcb_atlas_open_file_with_policy_cancelable(handle,
            root.as_ptr(), file.as_ptr(), length, files, policy,
            Some(poll), context(&state)) }.is_null());
    }
    assert!(unsafe { fcb_atlas_open_file_with_policy_cancelable(handle, root.as_ptr(),
        std::ptr::null(), 1, 1, 0, Some(poll), context(&state)) }.is_null());
    assert_eq!(state.count.load(Ordering::Relaxed), 0);
    assert_eq!(take(fcb_atlas_info(handle)), empty);
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn raw_file_abi_keeps_policy_receipts_and_original_capture_after_the_file_changes() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let path = fixture_root();
    let file = b"C:\\raw\xff.rs"; let native = OsString::from_vec(file.to_vec());
    fs::write(path.join(&native), b"original needle\n").unwrap();
    fs::write(path.join(".gitignore"), b"*.rs\n").unwrap();
    let root = CString::new(path.to_str().unwrap()).unwrap();
    let rules = fcb_atlas_create(); let included = fcb_atlas_create();
    assert_ne!(rules, 0); assert_ne!(included, 0);
    let narrow = take(open(rules, &root, file, 0));
    assert!(narrow.contains("\"discovery_scope\":\"file\",\"discovery_directory\":null"));
    assert!(narrow.contains("\"catalogued_files\":\"0\""));
    assert!(narrow.contains("\"discovery_complete\":true"));
    assert!(narrow.contains("\"discovery_excluded_entries\":\"1\""));
    let broad = take(open(included, &root, file, 1));
    assert!(broad.contains("\"discovery_file\":{\"encoding\":\"unix-bytes\",\"hex\":\"433a5c726177ff2e7273\""));
    assert!(broad.contains("\"path_basis\":\"project-root\""));
    assert!(broad.contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
    assert!(broad.contains("\"catalogued_files\":\"1\""));
    let needle = CString::new("needle").unwrap();
    let found = take(unsafe { super::search::fcb_atlas_search(included, 1, needle.as_ptr(),
        10, 16, 1024, 8192) });
    assert!(found.contains("\"retained_hits\":\"1\""));
    assert!(found.contains("\"discovery_scope\":\"file\""));
    fs::write(path.join(&native), b"new live source").unwrap();
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let captured = take(super::search::fcb_atlas_search_open_reader(included, reader, 1, 1));
    assert!(captured.contains("\"source_reopened\":false"));
    assert!(captured.contains("\"discovery_scope\":\"file\""));
    let before = take(fcb_atlas_info(included));
    assert!(open(included, &root, b".gitignore", 1).is_null());
    assert_eq!(take(fcb_atlas_info(included)), before);
    assert_eq!(fcb_atlas_close(included), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 16))
        .contains("\"original_hex\":\"6f726967696e616c206e6565646c650a\""));
    assert!(take(fcb_atlas_info(rules)).contains("\"discovery_excluded_entries\":\"1\""));
    assert_eq!(fcb_reader_close(reader), 1); assert_eq!(fcb_atlas_close(rules), 1);
}

#[test]
fn malformed_missing_directory_and_symlink_targets_do_not_install_empty_file_catalogs() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let path = fixture_root();
    fs::create_dir(path.join("selected")).unwrap();
    fs::write(path.join("selected/source.rs"), b"needle").unwrap();
    symlink(path.join("selected/source.rs"), path.join("alias.rs")).unwrap();
    symlink(path.join("selected"), path.join("alias-dir")).unwrap();
    let root = CString::new(path.to_str().unwrap()).unwrap();
    let handle = fcb_atlas_create(); assert_ne!(handle, 0);
    for file in [b"".as_slice(), b"/selected/source.rs", b"selected/", b".", b"..",
        b"selected/../escape", b"selected//source.rs", b"selected/\0source.rs",
        b"missing.rs", b"selected", b"alias.rs", b"alias-dir/source.rs"] {
        assert!(open(handle, &root, file, 1).is_null(), "{file:?}");
        assert!(take(fcb_atlas_info(handle)).contains("ATLAS_HANDLE_NOT_OPEN"));
    }
    let too_deep = std::iter::repeat_n("a", 33).collect::<Vec<_>>().join("/");
    assert!(open(handle, &root, too_deep.as_bytes(), 1).is_null());
    let accepted = take(open(handle, &root, b"selected/source.rs", 0));
    assert!(accepted.contains("\"discovery_complete\":true"));
    assert!(accepted.contains("\"catalogued_files\":\"1\""));
    assert!(accepted.contains("\"discovery_scope\":\"file\""));
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn file_open_cancellation_keeps_early_retry_and_late_installed_handle_ownership() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let path = fixture_root();
    fs::write(path.join("selected.rs"), b"needle").unwrap();
    let root = CString::new(path.to_str().unwrap()).unwrap(); let file = b"selected.rs";
    for policy in [0, 1] {
        let call = |handle, state: &Poll| unsafe {
            fcb_atlas_open_file_with_policy_cancelable(handle, root.as_ptr(),
                file.as_ptr(), file.len() as u64, 1, policy, Some(poll), context(state))
        };
        let handle = fcb_atlas_create(); assert_ne!(handle, 0);
        let probe = Poll { count: AtomicU64::new(0), stop_at: u64::MAX };
        take(call(handle, &probe));
        let total = probe.count.load(Ordering::Relaxed); assert!(total > 2);
        assert_eq!(fcb_atlas_close(handle), 1);
        for stop_at in [1, 2, total] {
            let handle = fcb_atlas_create(); assert_ne!(handle, 0);
            assert!(call(handle, &Poll { count: AtomicU64::new(0), stop_at }).is_null());
            let info = take(fcb_atlas_info(handle));
            if stop_at < total {
                assert!(info.contains("ATLAS_HANDLE_NOT_OPEN"), "{info}");
                assert!(take(open(handle, &root, file, policy)).contains("\"status\":\"ok\""));
            } else {
                assert!(info.contains("\"discovery_scope\":\"file\""),
                    "final-handoff cancellation does not undo installation: {info}");
            }
            assert_eq!(fcb_atlas_close(handle), 1);
        }
    }
}

#![deny(unsafe_op_in_unsafe_fn)]

//! Additive retained directory opener: preflight, raw identity and ownership.
use super::*;
use crate::reader_ffi::{FFI_TEST_LOCK, fcb_reader_create, fcb_reader_close, fcb_reader_copy_range};
use std::{ffi::{CStr, CString, OsString, c_void}, fs,
    os::unix::{ffi::OsStringExt, fs::symlink}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!("fcb-directory-search-ffi-{}-{}-{}",
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
fn open(handle: u64, root: &CString, directory: &[u8], policy: u64) -> *mut c_char {
    unsafe { fcb_atlas_open_directory_with_policy_cancelable(handle, root.as_ptr(),
        directory.as_ptr(), directory.len() as u64, 16, policy, None, std::ptr::null_mut()) }
}

#[test]
fn directory_abi_header_and_numeric_preflight_preserve_an_empty_handle_without_polling() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, *const c_char, *const u8, u64, u64, u64,
        Option<crate::search_ffi::SearchCancellationCallback>, *mut c_void) -> *mut c_char
        = fcb_atlas_open_directory_with_policy_cancelable;
    let header = include_str!("../include/fcb_atlas_sessions.h");
    assert_eq!(header.matches("fcb_atlas_open_directory_with_policy_cancelable(").count(), 1);
    let handle = fcb_atlas_create(); assert_ne!(handle, 0);
    let empty = take(fcb_atlas_info(handle));
    let state = Poll { count: AtomicU64::new(0), stop_at: u64::MAX };
    let unused_root = CString::new("/unused-directory-open").unwrap();
    let directory = [b'x'; 2049];
    for (length, files, policy) in [(1, 16, 2), (1, 16, u64::MAX), (0, 16, 0),
        (2049, 16, 0), (u64::MAX, 16, 1), (1, 0, 0), (1, 20_001, 1), (1, u64::MAX, 0)] {
        assert!(unsafe { fcb_atlas_open_directory_with_policy_cancelable(handle,
            unused_root.as_ptr(), directory.as_ptr(), length, files, policy,
            Some(poll), context(&state)) }.is_null());
    }
    assert!(unsafe { fcb_atlas_open_directory_with_policy_cancelable(handle, unused_root.as_ptr(),
        std::ptr::null(), 1, 16, 0, Some(poll), context(&state)) }.is_null());
    assert_eq!(state.count.load(Ordering::Relaxed), 0);
    assert_eq!(take(fcb_atlas_info(handle)), empty);
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn raw_folder_open_uses_original_root_paths_selected_policy_and_retained_capture() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let path = root(); let directory = b"C:\\raw\xff";
    let native = OsString::from_vec(directory.to_vec());
    fs::create_dir(path.join(&native)).unwrap();
    fs::write(path.join(&native).join("source.rs"), b"original needle\n").unwrap();
    fs::write(path.join(".gitignore"), b"*.rs\n").unwrap();
    let root = CString::new(path.to_str().unwrap()).unwrap();
    let rules = fcb_atlas_create(); let included = fcb_atlas_create();
    assert_ne!(rules, 0); assert_ne!(included, 0);
    let narrow = take(open(rules, &root, directory, 0));
    assert!(narrow.contains("\"discovery_scope\":\"directory\""));
    assert!(narrow.contains("\"catalogued_files\":\"0\""));
    assert!(narrow.contains("\"discovery_policy\":\"repository-rules-v1"));
    let broad = take(open(included, &root, directory, 1));
    assert!(broad.contains("\"discovery_directory\":{\"encoding\":\"unix-bytes\",\"hex\":\"433a5c726177ff\""));
    assert!(broad.contains("\"path_basis\":\"project-root\""));
    assert!(broad.contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
    assert!(broad.contains("\"catalogued_files\":\"1\""));
    let needle = CString::new("needle").unwrap();
    let found = take(unsafe { super::search::fcb_atlas_search(included, 1, needle.as_ptr(),
        10, 16, 1024, 8192) });
    assert!(found.contains("\"retained_hits\":\"1\""));
    assert!(found.contains("\"hex\":\"433a5c726177ff2f736f757263652e7273\""));
    assert!(found.contains("\"discovery_scope\":\"directory\""));
    fs::write(path.join(&native).join("source.rs"), b"new current bytes").unwrap();
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let captured = take(super::search::fcb_atlas_search_open_reader(included, reader, 1, 1));
    assert!(captured.contains("\"discovery_scope\":\"directory\""));
    assert!(captured.contains("\"source_reopened\":false"));
    let before = take(fcb_atlas_info(included));
    assert!(open(included, &root, b"another-folder", 1).is_null());
    assert_eq!(take(fcb_atlas_info(included)), before);
    assert_eq!(fcb_atlas_close(included), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 16))
        .contains("\"original_hex\":\"6f726967696e616c206e6565646c650a\""));
    assert!(take(fcb_atlas_info(rules)).contains("\"discovery_policy\":\"repository-rules-v1"));
    assert_eq!(fcb_reader_close(reader), 1); assert_eq!(fcb_atlas_close(rules), 1);
}

#[test]
fn malformed_missing_and_symlink_directory_opens_leave_the_handle_retryable() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let path = root();
    fs::create_dir(path.join("selected")).unwrap();
    fs::create_dir(path.join("empty")).unwrap();
    fs::write(path.join("selected/source.rs"), b"needle").unwrap();
    symlink(path.join("selected"), path.join("alias")).unwrap();
    let root = CString::new(path.to_str().unwrap()).unwrap();
    let handle = fcb_atlas_create(); assert_ne!(handle, 0);
    for directory in [b"".as_slice(), b"/selected", b"selected/", b".", b"..",
        b"selected/../escape", b"selected//child", b"selected/\0child", b"missing",
        b"alias", b"selected/source.rs"] {
        assert!(open(handle, &root, directory, 1).is_null(), "{directory:?}");
        assert!(take(fcb_atlas_info(handle)).contains("ATLAS_HANDLE_NOT_OPEN"));
    }
    let too_deep = std::iter::repeat_n("a", 32).collect::<Vec<_>>().join("/");
    assert!(open(handle, &root, too_deep.as_bytes(), 1).is_null());
    let empty = take(open(handle, &root, b"empty", 0));
    assert!(empty.contains("\"discovery_complete\":true"));
    assert!(empty.contains("\"catalogued_files\":\"0\""));
    assert!(empty.contains("\"discovery_scope\":\"directory\""));
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn directory_open_cancellation_preserves_early_retry_and_late_installed_ownership() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let path = root();
    fs::create_dir(path.join("selected")).unwrap();
    fs::write(path.join("selected/source.rs"), b"needle").unwrap();
    let root = CString::new(path.to_str().unwrap()).unwrap(); let directory = b"selected";
    for policy in [0, 1] {
        let call = |handle, state: &Poll| unsafe {
            fcb_atlas_open_directory_with_policy_cancelable(handle, root.as_ptr(),
                directory.as_ptr(), directory.len() as u64, 16, policy, Some(poll), context(state))
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
                assert!(take(open(handle, &root, directory, policy)).contains("\"status\":\"ok\""));
            } else {
                assert!(info.contains("\"discovery_scope\":\"directory\""),
                    "final-handoff cancellation does not undo installation: {info}");
            }
            assert_eq!(fcb_atlas_close(handle), 1);
        }
    }
}

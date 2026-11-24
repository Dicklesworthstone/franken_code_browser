//! Exact-file metadata ABI: admission, raw identity, policy and cancellation.
use super::*;
use std::{ffi::{CStr, CString, OsString}, fs,
    os::unix::{ffi::OsStringExt, fs::symlink}, path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering}};

struct Stop { polls: AtomicUsize, at: usize }
unsafe extern "C" fn poll(context: *mut c_void) -> i32 {
    let state = unsafe { &*context.cast::<Stop>() };
    i32::from(state.polls.fetch_add(1, Ordering::Relaxed) + 1 >= state.at)
}
fn context(state: &Stop) -> *mut c_void { std::ptr::from_ref(state).cast_mut().cast() }
fn root() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let path = std::env::temp_dir().join(format!("fcb-file-catalog-ffi-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn text(pointer: *mut c_char) -> Option<String> {
    if pointer.is_null() { return None; }
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; Some(text)
}
fn call(root: &CString, file: &[u8], policy: u64) -> Option<String> {
    text(unsafe { fcb_project_file_catalog_cancelable(root.as_ptr(), file.as_ptr(),
        file.len() as u64, 1, policy, None, std::ptr::null_mut()) })
}

#[test]
fn file_catalog_abi_preflight_refuses_before_callbacks_or_input_access() {
    let _: unsafe extern "C" fn(*const c_char, *const u8, u64, u64, u64,
        Option<SearchCancellationCallback>, *mut c_void) -> *mut c_char
        = fcb_project_file_catalog_cancelable;
    assert_eq!(include_str!("../../../include/fcb_project_catalog.h")
        .matches("fcb_project_file_catalog_cancelable(").count(), 1);
    let stop = Stop { polls: AtomicUsize::new(0), at: usize::MAX };
    let root = CString::new("/unused-file-catalog-preflight").unwrap();
    let file = [b'x'; 2049];
    for (length, files, policy) in [(1, 1, 2), (1, 1, u64::MAX), (0, 1, 0),
        (2049, 1, 0), (u64::MAX, 1, 1), (1, 0, 0), (1, 20_001, 1), (1, u64::MAX, 0)] {
        assert!(unsafe { fcb_project_file_catalog_cancelable(root.as_ptr(),
            file.as_ptr(), length, files, policy, Some(poll), context(&stop)) }.is_null());
    }
    assert!(unsafe { fcb_project_file_catalog_cancelable(std::ptr::null(),
        std::ptr::null(), 1, 1, 0, Some(poll), context(&stop)) }.is_null());
    assert_eq!(stop.polls.load(Ordering::Relaxed), 0);
}

#[test]
fn file_catalog_preserves_exact_raw_target_and_selected_policy_without_reading_source() {
    let root = root(); let file = b"C:\\raw\xff.rs";
    fs::write(root.join(OsString::from_vec(file.to_vec())), b"\xff\x00binary").unwrap();
    fs::write(root.join("sibling.rs"), b"sibling").unwrap();
    fs::write(root.join(".gitignore"), b"*.rs\n").unwrap();
    let root_name = CString::new(root.to_str().unwrap()).unwrap();
    let excluded = call(&root_name, file, 0).unwrap();
    assert!(excluded.contains("\"files\":[]"));
    assert!(excluded.contains("\"discovery_complete\":true"));
    let included = call(&root_name, file, 1).unwrap();
    assert!(included.contains("\"schema\":\"fcb.project-file-catalog/1\""));
    assert!(included.contains("\"command\":\"file-catalog\""));
    assert!(included.contains("\"path_basis\":\"project-root\""));
    assert_eq!(included.matches("\"hex\":\"433a5c726177ff2e7273\"").count(), 2);
    assert!(included.contains("\"catalogued_files\":\"1\""));
    assert!(!included.contains("7369626c696e672e7273"), "a sibling must not enter the selected file's map");
    assert!(included.contains("\"rule_files_enabled\":false"));
    assert!(included.contains("\"source_payload_read\":false"));
    assert!(included.contains("\"payload_bytes_read\":\"0\",\"read_calls\":\"0\""));
}

#[test]
fn malformed_missing_directory_and_symlink_targets_do_not_become_empty_successes() {
    let root = root(); fs::create_dir(root.join("selected")).unwrap();
    fs::write(root.join("selected/source.rs"), b"source").unwrap();
    symlink(root.join("selected/source.rs"), root.join("alias.rs")).unwrap();
    symlink(root.join("selected"), root.join("alias-dir")).unwrap();
    let root_name = CString::new(root.to_str().unwrap()).unwrap();
    for file in [b"".as_slice(), b"/selected/source.rs", b"selected/", b".", b"..",
        b"selected/../escape", b"selected//source.rs", b"selected/\0source.rs",
        b"missing.rs", b"selected", b"alias.rs", b"alias-dir/source.rs"] {
        assert!(call(&root_name, file, 1).is_none(), "{file:?}");
    }
    let too_deep = std::iter::repeat_n("a", 33).collect::<Vec<_>>().join("/");
    assert!(call(&root_name, too_deep.as_bytes(), 1).is_none());
    let accepted = call(&root_name, b"selected/source.rs", 0).unwrap();
    assert!(accepted.contains("\"catalogued_files\":\"1\""));
    assert!(accepted.contains("\"discovery_complete\":true"));
}

#[test]
fn file_catalog_cancellation_refuses_early_and_final_handoffs_and_keeps_retry_available() {
    let root = root(); fs::write(root.join("selected.rs"), b"source").unwrap();
    let root_name = CString::new(root.to_str().unwrap()).unwrap(); let file = b"selected.rs";
    for policy in [0, 1] {
        let invoke = |stop: &Stop| unsafe { fcb_project_file_catalog_cancelable(
            root_name.as_ptr(), file.as_ptr(), file.len() as u64, 1, policy,
            Some(poll), context(stop)) };
        let probe = Stop { polls: AtomicUsize::new(0), at: usize::MAX };
        assert!(text(invoke(&probe)).is_some());
        let total = probe.polls.load(Ordering::Relaxed); assert!(total > 2);
        for at in [1, 2, total] {
            assert!(text(invoke(&Stop { polls: AtomicUsize::new(0), at })).is_none());
            assert!(call(&root_name, file, policy).is_some());
        }
    }
}

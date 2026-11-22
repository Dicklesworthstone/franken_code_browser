//! Real directory-catalog ABI: raw-byte identity, scope, budgets and cancellation.
use super::*;
use std::{ffi::{CStr, CString, OsString}, fs, os::unix::ffi::OsStringExt,
    path::PathBuf, sync::atomic::{AtomicUsize, Ordering}};

struct Stop { polls: AtomicUsize, at: usize }
unsafe extern "C" fn poll(context: *mut c_void) -> i32 {
    let state = unsafe { &*context.cast::<Stop>() };
    i32::from(state.polls.fetch_add(1, Ordering::Relaxed) + 1 >= state.at)
}
fn context(state: &Stop) -> *mut c_void { std::ptr::from_ref(state).cast_mut().cast() }
fn root() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let path = std::env::temp_dir().join(format!("fcb-directory-ffi-{}-{}-{}",
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

#[test]
fn directory_abi_header_and_preflight_refuse_before_callbacks() {
    let _: unsafe extern "C" fn(*const c_char, *const u8, u64, u64, u64,
        Option<SearchCancellationCallback>, *mut c_void) -> *mut c_char
        = fcb_project_directory_catalog_cancelable;
    let header = include_str!("../../../include/fcb_project_catalog.h");
    assert_eq!(header.matches("fcb_project_directory_catalog_cancelable(").count(), 1);
    let stop = Stop { polls: AtomicUsize::new(0), at: usize::MAX };
    let root = CString::new("/unused-directory-catalog-preflight").unwrap();
    let directory = [b'x'; 2049];
    for (length, files, policy) in [(1, 32, 2), (0, 32, 0), (2049, 32, 0),
        (1, 0, 0), (1, 20_001, 0)] {
        assert!(unsafe { fcb_project_directory_catalog_cancelable(root.as_ptr(),
            directory.as_ptr(), length, files, policy, Some(poll), context(&stop)) }.is_null());
    }
    assert!(unsafe { fcb_project_directory_catalog_cancelable(std::ptr::null(),
        std::ptr::null(), u64::MAX, 32, 0, Some(poll), context(&stop)) }.is_null());
    assert_eq!(stop.polls.load(Ordering::Relaxed), 0);
}

#[test]
fn directory_ffi_preserves_raw_bytes_root_basis_and_selected_policy() {
    let root = root();
    let directory = b"raw\\dir\xff";
    let native = OsString::from_vec(directory.to_vec());
    fs::create_dir(root.join(&native)).unwrap();
    fs::write(root.join(&native).join("source.rs"), b"\xff\x00binary").unwrap();
    fs::write(root.join(".gitignore"), b"*.rs\n").unwrap();
    let root_name = CString::new(root.to_str().unwrap()).unwrap();
    let call = |policy| text(unsafe { fcb_project_directory_catalog_cancelable(
        root_name.as_ptr(), directory.as_ptr(), directory.len() as u64, 32, policy,
        None, std::ptr::null_mut()) }).unwrap();
    let rule_aware = call(0);
    assert!(rule_aware.contains("\"files\":[]"));
    let included = call(1);
    assert!(included.contains("\"schema\":\"fcb.project-directory-catalog/1\""));
    assert!(included.contains("\"path_basis\":\"project-root\""));
    assert!(included.contains("\"hex\":\"7261775c646972ff\""));
    assert!(included.contains("\"hex\":\"7261775c646972ff2f736f757263652e7273\""));
    assert!(included.contains("\"rule_files_enabled\":false"));
    assert!(included.contains("\"source_payload_read\":false"));
}

#[test]
fn directory_ffi_cancels_before_work_and_at_final_response_handoff() {
    let root = root(); fs::create_dir(root.join("selected")).unwrap();
    fs::write(root.join("selected/source.rs"), b"x").unwrap();
    let root_name = CString::new(root.to_str().unwrap()).unwrap();
    let directory = b"selected";
    let call = |stop: &Stop| unsafe { fcb_project_directory_catalog_cancelable(
        root_name.as_ptr(), directory.as_ptr(), directory.len() as u64, 32, 0,
        Some(poll), context(stop)) };
    let probe = Stop { polls: AtomicUsize::new(0), at: usize::MAX };
    assert!(text(call(&probe)).is_some());
    for at in [1, 2, probe.polls.load(Ordering::Relaxed)] {
        assert!(text(call(&Stop { polls: AtomicUsize::new(0), at })).is_none());
    }
}

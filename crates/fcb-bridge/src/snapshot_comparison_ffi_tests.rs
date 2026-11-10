#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::ffi::{CStr, CString};
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}

#[test]
fn header_and_export_types_agree_for_every_comparison_entrypoint() {
    let _: extern "C" fn() -> u64 = fcb_snapshot_comparison_create;
    let _: unsafe extern "C" fn(u64, *const c_char, *const c_char) -> *mut c_char = fcb_snapshot_comparison_open;
    let _: unsafe extern "C" fn(u64, *const c_char, *const c_char, Option<SearchCancellationCallback>, *mut c_void)
        -> *mut c_char = fcb_snapshot_comparison_open_cancelable;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_snapshot_comparison_info;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_snapshot_comparison_begin;
    let _: unsafe extern "C" fn(u64, u64, Option<SearchCancellationCallback>, *mut c_void)
        -> *mut c_char = fcb_snapshot_comparison_begin_cancelable;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_snapshot_comparison_step;
    let _: unsafe extern "C" fn(u64, u64, u64, Option<SearchCancellationCallback>, *mut c_void)
        -> *mut c_char = fcb_snapshot_comparison_step_cancelable;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_snapshot_comparison_page;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_snapshot_comparison_open_reader;
    let _: extern "C" fn(u64) -> u8 = fcb_snapshot_comparison_cancel;
    let _: extern "C" fn(u64) -> u8 = fcb_snapshot_comparison_close;
    let header = include_str!("../include/fcb_snapshot_comparison.h");
    for name in ["create", "open", "open_cancelable", "info", "begin", "begin_cancelable", "step",
        "step_cancelable", "page", "open_reader", "cancel", "close"] {
        assert_eq!(header.matches(&format!("fcb_snapshot_comparison_{name}(")).count(), 1, "{name}");
    }
}

#[test]
fn archive_paths_are_bounded_exact_utf8_and_scalar_enums_are_checked() {
    let empty = CString::new("").unwrap();
    let malformed = [0xff_u8, 0];
    let boundary = CString::new("a".repeat(MAX_ARCHIVE_PATH_BYTES)).unwrap();
    let beyond = CString::new("a".repeat(MAX_ARCHIVE_PATH_BYTES + 1)).unwrap();
    let decomposed = CString::new("project-e\u{301}/before.fcbs").unwrap();
    for pointer in [std::ptr::null(), empty.as_ptr(), malformed.as_ptr().cast(), beyond.as_ptr()] {
        assert!(matches!(unsafe { path(pointer) }, Err(AccessError::InvalidArgument)));
    }
    assert_eq!(unsafe { path(boundary.as_ptr()) }.unwrap().as_os_str().len(), MAX_ARCHIVE_PATH_BYTES);
    let decoded = unsafe { path(decomposed.as_ptr()) }.unwrap().to_str().unwrap();
    assert!(decoded.as_bytes().iter().copied().eq(decomposed.as_bytes().iter().copied()));
    assert!(!decoded.as_bytes().eq("project-é/before.fcbs".as_bytes()));
    for value in [9, u64::MAX] { assert!(matches!(filter(value), Err(AccessError::InvalidArgument))); }
    for value in [2, u64::MAX] { assert!(matches!(side(value), Err(AccessError::InvalidArgument))); }
}

#[test]
fn unknown_handle_errors_are_owned_bounded_and_do_not_echo_private_paths() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let first = fcb_snapshot_comparison_info(u64::MAX);
    let secret = CString::new("/private/user/source-archive.fcbs").unwrap();
    let second = unsafe { fcb_snapshot_comparison_open(u64::MAX, secret.as_ptr(), secret.as_ptr()) };
    let third = fcb_snapshot_comparison_page(u64::MAX, u64::MAX, 0, 0, 64);
    for pointer in [first, second, third] {
        let response = take(pointer);
        assert!(response.len() < 4096 && response.contains("\"status\":\"error\""));
        assert!(response.contains("\"owner\":\"18446744073709551615\""));
        assert!(!response.contains("/private/user"));
    }
    assert_eq!(fcb_snapshot_comparison_cancel(u64::MAX), 0);
    assert_eq!(fcb_snapshot_comparison_close(u64::MAX), 0);
}

unsafe extern "C" fn cancel_now(context: *mut c_void) -> i32 {
    if !context.is_null() { unsafe { *context.cast::<u64>() += 1; } }
    1
}
#[test]
fn borrowed_cancel_poll_refuses_open_before_path_dereference_and_leaves_handle_empty() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let handle = fcb_snapshot_comparison_create(); assert_ne!(handle, 0);
    let mut polls = 0_u64;
    let response = take(unsafe { fcb_snapshot_comparison_open_cancelable(handle, std::ptr::null(),
        std::ptr::null(), Some(cancel_now), (&mut polls as *mut u64).cast()) });
    assert_eq!(polls, 1);
    assert!(response.contains("\"exit_code\":\"130\"") && response.contains("HANDLE_CANCELED"));
    assert!(take(fcb_snapshot_comparison_info(handle)).contains("HANDLE_NOT_OPEN"));
    assert_eq!(fcb_snapshot_comparison_close(handle), 1);
}

#[cfg(unix)]
fn archives() -> (std::path::PathBuf, CString, CString) {
    use fcb::{ArenaOwnerId, ByteLength};
    use fcb::search::{ResourceAllocationId, ResourceBudget, snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData}};
    let root = std::env::temp_dir().join(format!("fcb-snapshot-comparison-c-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(9981).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let mut paths = Vec::new();
    for (index, name, source) in [(1, "before.fcbs", b"old captured text".as_slice()),
        (2, "after.fcbs", b"new captured text".as_slice())] {
        let bytes = SnapshotBytes::encode(owner, true, "comparison-c", &[SnapshotEntry {
            path: b"source.rs", observed_bytes: source.len() as u64, data: SnapshotData::Captured(source) }],
            SnapshotLimits::default(), &budget, ResourceAllocationId::new(index).unwrap(), || false).unwrap();
        let path = root.join(name); std::fs::write(&path, bytes.bytes()).unwrap();
        paths.push(CString::new(path.to_str().unwrap()).unwrap());
    }
    let after = paths.pop().unwrap(); let before = paths.pop().unwrap(); (root, before, after)
}

#[test]
#[cfg(unix)]
fn actual_c_workflow_retains_full_width_generation_and_independent_exact_readers() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (_root, before, after) = archives();
    let handle = fcb_snapshot_comparison_create(); assert_ne!(handle, 0);
    let opened = take(unsafe { fcb_snapshot_comparison_open_cancelable(handle, before.as_ptr(), after.as_ptr(),
        None, std::ptr::null_mut()) });
    assert!(opened.contains("\"status\":\"ok\"") && opened.contains("\"accepted_comparison_generation\":null"));
    let generation = u64::MAX - 1;
    let mut progress = take(unsafe { fcb_snapshot_comparison_begin_cancelable(handle, generation, None, std::ptr::null_mut()) });
    for _ in 0..4 {
        if progress.contains("\"comparison_in_progress\":false") { break; }
        progress = take(unsafe { fcb_snapshot_comparison_step_cancelable(handle, generation, 1, None, std::ptr::null_mut()) });
    }
    assert!(progress.contains("\"comparison_in_progress\":false"));
    let rows = take(fcb_snapshot_comparison_page(handle, generation, 3, 0, 64));
    assert_eq!(number(&rows, "comparison_generation"), generation);
    assert_eq!(number(&rows, "matching_entries"), 1);
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_snapshot_comparison_open_reader(handle, reader, generation, 1, 1));
    assert_eq!(number(&opened, "reader_owner"), reader);
    assert!(opened.contains("\"side\":\"after\""));
    assert!(take(fcb_snapshot_comparison_open_reader(handle, reader, generation, 1, 0)).contains("READER_HANDLE_ALREADY_OPEN"));
    assert!(take(fcb_snapshot_comparison_page(handle, generation, 9, 0, 64)).contains("INVALID_ARGUMENT"));
    assert!(take(fcb_snapshot_comparison_open_reader(handle, reader, generation, 1, 2)).contains("INVALID_ARGUMENT"));
    let retained = fcb_snapshot_comparison_page(handle, generation, 0, 0, 64);
    assert_eq!(fcb_snapshot_comparison_cancel(handle), 1);
    assert_eq!(number(&take(fcb_snapshot_comparison_page(handle, generation, 0, 0, 64)), "matching_entries"), 1);
    assert_eq!(fcb_snapshot_comparison_close(handle), 1);
    assert!(take(retained).contains("\"kind\":\"changed\""));
    let bytes = take(crate::reader_ffi::fcb_reader_copy_range(reader, 0, 17));
    assert!(bytes.contains("6e65772063617074757265642074657874"));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
    assert!(take(fcb_snapshot_comparison_info(handle)).contains("HANDLE_UNKNOWN"));
}


#[test]
#[cfg(unix)]
fn immediately_canceled_step_retires_pending_attempt_and_preserves_accepted_rows() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (_root, before, after) = archives();
    let handle = fcb_snapshot_comparison_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_snapshot_comparison_open(handle, before.as_ptr(), after.as_ptr()) })
        .contains("\"status\":\"ok\""));
    assert!(take(fcb_snapshot_comparison_begin(handle, 1)).contains("\"comparison_in_progress\":true"));
    assert!(take(fcb_snapshot_comparison_step(handle, 1, 8)).contains("\"comparison_in_progress\":false"));
    assert!(take(fcb_snapshot_comparison_begin(handle, 2)).contains("\"pending_comparison_generation\":\"2\""));
    let mut polls = 0_u64;
    let canceled = take(unsafe { fcb_snapshot_comparison_step_cancelable(handle, 2, 1,
        Some(cancel_now), (&mut polls as *mut u64).cast()) });
    assert_eq!(polls, 1);
    assert!(canceled.contains("\"exit_code\":\"130\"") && canceled.contains("CANCELED"));
    let info = take(fcb_snapshot_comparison_info(handle));
    assert!(info.contains("\"pending_comparison_generation\":null"));
    assert_eq!(number(&info, "accepted_comparison_generation"), 1);
    assert_eq!(number(&info, "last_comparison_attempt"), 2);
    assert!(take(fcb_snapshot_comparison_step(handle, 2, 1)).contains("SNAPSHOT_COMPARISON_STALE_GENERATION"));
    assert_eq!(number(&take(fcb_snapshot_comparison_page(handle, 1, 0, 0, 64)), "matching_entries"), 1);
    assert!(take(fcb_snapshot_comparison_begin(handle, 2)).contains("SNAPSHOT_COMPARISON_STALE_GENERATION"));
    assert_eq!(fcb_snapshot_comparison_close(handle), 1);
}

#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use crate::reader_ffi::{fcb_reader_create, fcb_reader_open, fcb_reader_info,
    fcb_reader_cancel, fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString}, fs, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "known export effects must have an owned receipt");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn directory() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-reader-export-ffi-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&root).unwrap(); root
}
fn open(bytes: &[u8]) -> (u64, PathBuf) {
    let root = directory(); let source = root.join("source.bin");
    fs::write(&source, bytes).unwrap();
    let path = CString::new(source.to_str().unwrap()).unwrap();
    let handle = fcb_reader_create(); assert_ne!(handle, 0);
    let response = take(unsafe { fcb_reader_open(handle, path.as_ptr(), bytes.len().max(1) as u64) });
    assert!(response.contains("\"status\":\"ok\""), "{response}");
    (handle, root)
}
fn export(handle: u64, start: u64, end: u64, path: &Path,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    let path = CString::new(path.to_str().unwrap()).unwrap();
    unsafe { fcb_reader_export_original_cancelable(handle, start, end, path.as_ptr(), poll, context) }
}

#[test]
fn c_export_keeps_original_capture_and_owns_receipt_after_reader_close() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let bytes = b"\xff\xfeA\0\r\0\n\0\x3e\xd8\x80\xdd\xff";
    let (handle, root) = open(bytes);
    let before = take(fcb_reader_info(handle));
    fs::write(root.join("source.bin"), b"changed live source").unwrap();
    let output = root.join("selected.bin");
    let pointer = export(handle, 1, bytes.len() as u64 - 1, &output, None, std::ptr::null_mut());
    assert_eq!(fcb_reader_close(handle), 1);
    let receipt = take(pointer);
    assert!(receipt.contains("\"schema\":\"fcb.reader-export/1\""), "{receipt}");
    assert!(receipt.contains("\"status\":\"ok\""));
    assert!(receipt.contains("\"export_domain\":\"original-bytes\""));
    assert!(receipt.contains(&format!("\"owner\":\"{handle}\"")));
    assert!(receipt.contains("\"effect\":\"complete-file-and-parent-sync-requested\""));
    assert!(receipt.contains("\"original_range\":{\"start\":\"1\",\"end\":\"12\"}"));
    assert!(receipt.contains("\"export_bytes\":\"11\""));
    assert!(receipt.contains("\"bytes_written\":\"11\""));
    let digest_start = before.find("\"capture_sha256\":\"").unwrap();
    let digest = &before[digest_start..digest_start + "\"capture_sha256\":\"".len() + 64 + 1];
    assert!(receipt.contains(digest));
    assert_eq!(fs::read(output).unwrap(), bytes[1..bytes.len() - 1]);
}

struct Cancel { handle: u64, destination: PathBuf, at: u64, canceled: bool }
unsafe extern "C" fn cancel_after_bytes(context: *mut c_void) -> i32 {
    let state = unsafe { &mut *context.cast::<Cancel>() };
    if !state.canceled && fs::metadata(&state.destination).is_ok_and(|m| m.len() >= state.at) {
        state.canceled = true;
        // This lock-free cancellation is allowed while the reader writer runs.
        let _ = fcb_reader_cancel(state.handle);
        return 1;
    }
    0
}

#[test]
fn c_callback_and_epoch_cancellation_preserve_partial_and_complete_write_effects() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let bytes = vec![b'z'; 2 * 64 * 1024 + 11];
    for at in [0, 64 * 1024, bytes.len() as u64] {
        let (handle, root) = open(&bytes); let destination = root.join("interrupted.bin");
        let mut state = Cancel { handle, destination: destination.clone(), at, canceled: false };
        let context = std::ptr::from_mut(&mut state).cast();
        let receipt = take(export(handle, 0, bytes.len() as u64, &destination,
            Some(cancel_after_bytes), context));
        assert!(state.canceled);
        assert!(receipt.contains("READER_EXPORT_CANCELED"), "{receipt}");
        assert!(receipt.contains(&format!("\"bytes_written\":\"{at}\"")), "{receipt}");
        let effect = if at == bytes.len() as u64 { "complete-file-sync-unconfirmed" }
            else { "destination-created-incomplete" };
        assert!(receipt.contains(&format!("\"effect\":\"{effect}\"")));
        assert_eq!(fs::read(&destination).unwrap(), bytes[..at as usize]);
        assert!(take(fcb_reader_info(handle)).contains("\"status\":\"ok\""));
        assert_eq!(fcb_reader_close(handle), 1);
    }
}

#[test]
fn c_export_refuses_bad_arguments_and_existing_destination_without_overwrite() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (handle, root) = open(b"abc"); let output = root.join("new.bin");
    for (start, end) in [(0, 4), (2, 1), (0, u64::MAX)] {
        let response = take(export(handle, start, end, &output, None, std::ptr::null_mut()));
        assert!(response.contains("\"schema\":\"fcb.reader-session/1\""));
        assert!(response.contains("READER_SESSION_INVALID_RANGE"));
        assert!(!output.exists());
    }
    let response = take(unsafe { fcb_reader_export_original_cancelable(handle, 0, 3,
        std::ptr::null(), None, std::ptr::null_mut()) });
    assert!(response.contains("READER_HANDLE_INVALID_ARGUMENT")); assert!(!output.exists());
    fs::write(&output, b"earlier").unwrap();
    let response = take(export(handle, 0, 3, &output, None, std::ptr::null_mut()));
    assert!(response.contains("READER_EXPORT_DESTINATION_EXISTS"));
    assert!(response.contains("\"effect\":\"none\""));
    assert!(response.contains("\"bytes_written\":\"0\""));
    assert_eq!(fs::read(&output).unwrap(), b"earlier");
    assert_eq!(fcb_reader_close(handle), 1);
    let response = take(export(handle, 0, 3, &root.join("closed.bin"), None, std::ptr::null_mut()));
    assert!(response.contains("READER_HANDLE_UNKNOWN"));
    assert!(!root.join("closed.bin").exists());
}

#[test]
fn c_export_signature_matches_the_declared_ownership_contract() {
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char,
        Option<SearchCancellationCallback>, *mut c_void) -> *mut c_char =
        fcb_reader_export_original_cancelable;
    let header = include_str!("../include/fcb_reader_sessions.h");
    assert_eq!(header.matches("char *fcb_reader_export_original_cancelable(").count(), 1);
    assert!(header.contains("ALWAYS deliver the terminal receipt"));
}

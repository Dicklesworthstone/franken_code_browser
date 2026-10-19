//! Real C ABI pointer/string ownership and exact-reader import coverage.
use super::*;
use std::{ffi::{CStr, CString}, fs};

fn consume(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let json = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    json
}
fn field(json: &str, key: &str) -> u64 {
    let key = format!("\"{key}\":\"");
    json.split_once(&key).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

#[test]
fn desk_abi_opens_books_recalls_and_restores_without_live_source_reopening() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let desk = fcb_desk_create(); assert_ne!(desk, 0);
    let started = consume(fcb_desk_start(desk)); assert_eq!(field(&started, "model_revision"), 0);
    let missing = consume(unsafe { fcb_desk_open(desk, 0, 1, std::ptr::null()) });
    assert!(missing.contains("DESK_HANDLE_INVALID_ARGUMENT"));
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let root = std::env::temp_dir().join(format!("fcb-desk-abi-{}-{desk}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let source = root.join("source.rs"); fs::write(&source, b"original captured evidence\n").unwrap();
    let source_c = CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
    let opened = consume(unsafe { crate::reader_ffi::fcb_reader_open(reader, source_c.as_ptr(), 4096) });
    assert!(opened.contains("\"status\":\"ok\""));
    fs::write(&source, b"replacement source").unwrap();
    let imported = consume(fcb_desk_import_reader(desk, 0, 1, reader, 9, 17));
    let pane = field(&imported, "pane");
    let label = CString::new("why the original matters").unwrap();
    let bookmarked = consume(unsafe { fcb_desk_apply(desk, 1, 2, 3, pane, 0, 0, label.as_ptr()) });
    assert!(bookmarked.contains("why the original matters"));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
    let window = consume(fcb_desk_window(desk, 2, pane, 0, 4096));
    assert!(window.contains("original captured evidence")); assert!(!window.contains("replacement source"));
    let destination = root.join("retained.fcbdesk"); let destination_c = CString::new(destination.as_os_str().as_encoded_bytes()).unwrap();
    let saved = consume(unsafe { fcb_desk_save(desk, 2, destination_c.as_ptr(), 4096) });
    assert!(saved.contains("complete-file-and-parent-sync-requested"));
    let empty_label = std::ptr::null();
    consume(unsafe { fcb_desk_apply(desk, 2, 3, 8, pane, 0, 0, empty_label) });
    let recall = consume(unsafe { fcb_desk_apply(desk, 3, 4, 4, 1, 0, 0, empty_label) });
    assert!(recall.contains("\"selection\":{\"start\":\"9\",\"end\":\"17\"}"));
    let restored = consume(unsafe { fcb_desk_restore(desk, 4, 5, destination_c.as_ptr()) });
    assert!(restored.contains("why the original matters"));
    assert_eq!(field(&restored, "model_revision"), 5);
    assert_eq!(fcb_desk_cancel(desk), 1);
    let state = consume(fcb_desk_state(desk)); assert_eq!(field(&state, "model_revision"), 5);
    assert_eq!(fcb_desk_close(desk), 1);
    assert!(consume(fcb_desk_state(desk)).contains("DESK_HANDLE_UNKNOWN"));
}

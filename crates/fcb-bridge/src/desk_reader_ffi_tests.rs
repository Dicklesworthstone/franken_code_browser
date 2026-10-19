//! Real exported C ABI handoff and returned-buffer ownership. The global guard
//! prevents unrelated FFI suites from consuming the same eight reader slots.
use super::*;
use std::{ffi::{CStr, CString}, fs};

fn consume(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn field(json: &str, key: &str) -> u64 {
    let key = format!("\"{key}\":\"");
    json.split_once(&key).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

#[test]
fn desk_reader_export_signature_and_header_require_an_explicit_destination() {
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_desk_open_reader;
    let header = include_str!("../include/fcb_desk.h");
    assert_eq!(header.matches("fcb_desk_open_reader(").count(), 1);
}

#[test]
fn c_handoff_preserves_utf16_nul_and_original_ranges_after_live_replacement_and_desk_close() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let desk = fcb_desk_create(); assert_ne!(desk, 0);
    consume(fcb_desk_start(desk));
    let root = std::env::temp_dir().join(format!("fcb-desk-reader-abi-{}-{desk}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let source = root.join("retained.txt");
    let raw = b"\xff\xfea\0\r\0\n\0g\0o\0a\0l\0";
    fs::write(&source, raw).unwrap();
    let label = CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
    let opened = consume(unsafe { fcb_desk_open(desk, 0, 1, label.as_ptr()) });
    let pane = field(&opened, "pane");
    consume(unsafe { fcb_desk_apply(desk, 1, 2, 2, pane, 8, 16, std::ptr::null()) });
    fs::write(&source, b"unrelated replacement bytes").unwrap();
    let destination = crate::reader_ffi::fcb_reader_create(); assert_ne!(destination, 0);
    let receipt = consume(fcb_desk_open_reader(desk, 2, pane, destination));
    assert!(receipt.contains("\"status\":\"ok\""));
    assert!(receipt.contains("\"command\":\"open-reader\""));
    assert_eq!(field(&receipt, "owner"), desk);
    assert_eq!(field(&receipt, "offset"), 8);
    assert!(receipt.contains("\"selection\":{\"start\":\"8\",\"end\":\"16\"}"));
    assert!(receipt.contains("\"reader\":{\"schema\":\"fcb.reader-session/1\""));
    assert!(receipt.contains("\"initial_source_bytes_read\":\"0\""));
    assert!(receipt.contains("\"initial_read_calls\":\"0\""));
    assert!(receipt.contains("\"capture_sha256\":\""));
    assert_eq!(fcb_desk_close(desk), 1);
    let info = consume(crate::reader_ffi::fcb_reader_info(destination));
    assert_eq!(field(&info, "owner"), destination);
    assert_eq!(field(&info, "captured_bytes"), raw.len() as u64);
    let copied = consume(crate::reader_ffi::fcb_reader_copy_range(destination, 0, raw.len() as u64));
    assert!(copied.contains("fffe61000d000a0067006f0061006c00"));
    let needle = CString::new("goal").unwrap();
    let found = consume(unsafe { crate::reader_ffi::fcb_reader_find(destination, 1, needle.as_ptr(), 8, 4096) });
    assert_eq!(field(&found, "matches_seen"), 1);
    let hit = consume(crate::reader_ffi::fcb_reader_copy_hit(destination, 1, 0));
    assert!(hit.contains("\"original_hex\":\"67006f0061006c00\""));
    assert_eq!(crate::reader_ffi::fcb_reader_close(destination), 1);
}

#[test]
fn c_errors_are_desk_owned_and_refused_destinations_remain_empty() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let desk = fcb_desk_create(); assert_ne!(desk, 0);
    consume(fcb_desk_start(desk));
    let destination = crate::reader_ffi::fcb_reader_create(); assert_ne!(destination, 0);
    for (source, pane, reader) in [(desk, 1, destination), (u64::MAX, 1, destination), (desk, u64::MAX, 0)] {
        let error = consume(fcb_desk_open_reader(source, 0, pane, reader));
        assert!(error.contains("\"schema\":\"fcb.reading-desk/1\""));
        assert!(error.contains("\"status\":\"error\""));
        assert_eq!(field(&error, "owner"), source);
        assert!(!error.contains("\"reader\":"));
    }
    assert!(consume(crate::reader_ffi::fcb_reader_info(destination)).contains("READER_HANDLE_NOT_OPEN"));
    assert_eq!(crate::reader_ffi::fcb_reader_close(destination), 1);
    assert_eq!(fcb_desk_close(desk), 1);
}

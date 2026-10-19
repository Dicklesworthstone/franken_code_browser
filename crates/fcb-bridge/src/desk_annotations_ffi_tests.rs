//! Exact length-prefixed UTF-8 notes, including NUL, through the public C ABI.
use super::*;
use std::{ffi::{CStr, CString}, fs};

fn consume(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn ok(pointer: *mut c_char) -> String {
    let text = consume(pointer); assert!(text.contains("\"status\":\"ok\""), "{text}"); text
}
fn number(text: &str, key: &str) -> u64 {
    let key = format!("\"{key}\":\"");
    text.split_once(&key).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

#[test]
fn note_abi_keeps_embedded_nuls_and_refuses_bad_lengths_or_utf8_before_mutation() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let desk = fcb_desk_create(); assert_ne!(desk, 0); ok(fcb_desk_start(desk));
    let root = std::env::temp_dir().join(format!("fcb-desk-note-abi-{}-{desk}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let source = root.join("saved.rs"); fs::write(&source, b"exact").unwrap();
    let path = CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
    let opened = ok(unsafe { fcb_desk_open(desk, 0, 1, path.as_ptr()) });
    let pane = number(&opened, "pane");
    let label = CString::new("Reason").unwrap(); let body = b"line one\nline two\0still present";
    let added = ok(unsafe { fcb_desk_annotate(desk, 1, 2, pane, label.as_ptr(), body.as_ptr(), body.len() as u64) });
    assert!(added.contains("\"body\":\"line one\\nline two\\u0000still present\""));
    let bookmark = number(&added, "bookmark");
    for (pointer, length) in [(std::ptr::null(), 1), (std::ptr::null(), u64::MAX), (b"\xff".as_ptr(), 1)] {
        let refused = consume(unsafe { fcb_desk_edit_bookmark(desk, 2, 3, bookmark, label.as_ptr(), pointer, length) });
        assert!(refused.contains("DESK_HANDLE_INVALID_ARGUMENT"));
        let state = ok(fcb_desk_state(desk));
        assert_eq!(number(&state, "model_revision"), 2);
        assert!(state.contains("still present"));
    }
    let renamed = CString::new("Plain landmark").unwrap();
    let emptied = ok(unsafe { fcb_desk_edit_bookmark(desk, 2, 4, bookmark, renamed.as_ptr(), std::ptr::null(), 0) });
    assert!(emptied.contains("\"body\":\"\""));
    assert!(emptied.contains("\"label\":\"Plain landmark\""));
    assert!(emptied.contains("\"update_sequence\":\"2\""));
    let stale = consume(unsafe { fcb_desk_annotate(desk, 4, 4, pane, label.as_ptr(), body.as_ptr(), body.len() as u64) });
    assert!(stale.contains("DESK_STALE_ATTEMPT"));
    assert_eq!(ok(fcb_desk_state(desk)).matches("\"bookmark\":").count(), 1);
    assert_eq!(fcb_desk_close(desk), 1);
}

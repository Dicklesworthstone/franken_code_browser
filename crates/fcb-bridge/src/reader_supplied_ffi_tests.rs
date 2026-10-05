use super::*;
use std::ffi::CString;
use crate::reader_sessions::Command;
use fcb_app::host::reader::ReaderDocumentOptions;

#[test]
fn supplied_capture_is_owned_and_its_nonexistent_label_never_opens_a_file() {
    let readers = ReaderSessions::new();
    let handle = readers.create().unwrap();
    let label = CString::new("not-a-real-root/supplied.md").unwrap();
    let mut bytes = b"# Supplied\n\nOriginal **capture**.\n".to_vec();
    let original = bytes.clone();
    let info = unsafe { open_in(&readers, handle, label.as_ptr(), bytes.as_ptr(),
        bytes.len() as u64, || false) }.unwrap();
    assert!(info.as_str().contains("\"capture_origin\":\"host-supplied\""));
    assert!(info.as_str().contains("\"initial_source_bytes_read\":\"0\""));
    assert!(info.as_str().contains("\"initial_read_calls\":\"0\""));
    bytes.fill(b'x');
    let copied = readers.execute(handle, Command::CopyRange { start: 0, end: original.len() as u64 }, || false).unwrap();
    let expected: String = original.iter().map(|byte| format!("{byte:02x}")).collect();
    assert!(copied.as_str().contains(&format!("\"original_hex\":\"{expected}\"")));
    let document = readers.execute(handle, Command::Document {
        generation: 1, options: ReaderDocumentOptions::default(),
    }, || false).unwrap();
    assert!(document.as_str().contains("\"layout_complete\":true"));
    assert!(document.as_str().contains("Supplied"));
    assert!(document.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    assert!(matches!(unsafe { open_in(&readers, handle, label.as_ptr(), bytes.as_ptr(),
        bytes.len() as u64, || false) }, Err(AccessError::AlreadyOpen)));
    readers.close(handle).unwrap();
    // Returned buffers are independent of the closed source handle.
    assert!(copied.as_str().contains(&expected));
}

#[test]
fn raw_supplied_bytes_preserve_bom_nul_and_malformed_sequences() {
    let readers = ReaderSessions::new();
    let handle = readers.create().unwrap();
    let label = CString::new("raw-source").unwrap();
    let bytes = [0xef, 0xbb, 0xbf, 0, 0xff, b'\n'];
    unsafe { open_in(&readers, handle, label.as_ptr(), bytes.as_ptr(), bytes.len() as u64, || false) }.unwrap();
    let copied = readers.execute(handle, Command::CopyRange { start: 0, end: 6 }, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"efbbbf00ff0a\""));
    readers.close(handle).unwrap();
}

#[test]
fn empty_null_buffer_is_a_real_capture_but_nonempty_null_is_refused() {
    let readers = ReaderSessions::new();
    let handle = readers.create().unwrap();
    let label = CString::new("empty.md").unwrap();
    assert!(matches!(unsafe { open_in(&readers, handle, label.as_ptr(), std::ptr::null(),
        1, || false) }, Err(AccessError::InvalidArgument)));
    let info = unsafe { open_in(&readers, handle, label.as_ptr(), std::ptr::null(), 0, || false) }.unwrap();
    assert!(info.as_str().contains("\"captured_bytes\":\"0\""));
    let copied = readers.execute(handle, Command::CopyRange { start: 0, end: 0 }, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"\""));
    readers.close(handle).unwrap();
}

#[test]
fn limits_are_checked_before_pointers_and_failed_initialization_is_retryable() {
    let readers = ReaderSessions::new();
    let handle = readers.create().unwrap();
    for length in [MAX_HOST_TEXT_BYTES as u64 + 1, u64::MAX] {
        assert!(matches!(unsafe { open_in(&readers, handle, std::ptr::null(), std::ptr::null(),
            length, || false) }, Err(AccessError::InvalidArgument)));
    }
    let label = CString::new("retry.md").unwrap();
    let bytes = b"# Retry";
    assert!(matches!(unsafe { open_in(&readers, handle, label.as_ptr(), bytes.as_ptr(),
        bytes.len() as u64, || true) }, Err(error) if error.canceled()));
    unsafe { open_in(&readers, handle, label.as_ptr(), bytes.as_ptr(), bytes.len() as u64, || false) }.unwrap();
    readers.close(handle).unwrap();
    assert!(matches!(unsafe { open_in(&readers, handle, label.as_ptr(), bytes.as_ptr(),
        bytes.len() as u64, || false) }, Err(AccessError::UnknownHandle)));
}

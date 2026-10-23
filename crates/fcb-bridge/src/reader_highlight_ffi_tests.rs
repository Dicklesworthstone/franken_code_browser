#![deny(unsafe_op_in_unsafe_fn)]

//! Exercise the actual C handoff used by paged native readers. Classification
//! must describe the retained capture, including lexical context before a page.

use super::*;
use super::supplied::{fcb_reader_fork, fcb_reader_open_bytes};
use std::ffi::{CStr, CString};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a complete JSON handoff");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    text
}

fn supplied(label: &str, bytes: &[u8]) -> u64 {
    let handle = fcb_reader_create();
    assert_ne!(handle, 0);
    let label = CString::new(label).unwrap();
    let info = take(unsafe {
        fcb_reader_open_bytes(handle, label.as_ptr(), bytes.as_ptr(), bytes.len() as u64)
    });
    assert!(info.contains("\"status\":\"ok\""), "{info}");
    assert!(info.contains("\"initial_read_calls\":\"0\""), "{info}");
    handle
}

fn highlighted(page: &str, owner: u64) {
    assert!(page.contains("\"status\":\"ok\""), "{page}");
    assert!(page.contains(&format!("\"owner\":\"{owner}\"")), "{page}");
    assert!(page.contains("\"state\":\"highlighted\""), "{page}");
    assert!(page.contains("\"language\":\"rust\""), "{page}");
    assert!(page.contains("\"coordinate_space\":\"window-utf16\""), "{page}");
    assert!(page.contains("\"source_context\":\"complete-capture\""), "{page}");
}

#[test]
fn late_page_inherits_comment_context_from_owned_complete_capture() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let mut original = b"/* start\ninside */\nfn main() {}\n".to_vec();
    let handle = supplied("retained.rs", &original);
    original.fill(b'x'); // Foreign backing no longer contains the captured code.
    let comment = take(fcb_reader_lines(handle, 2, 1, 1024));
    highlighted(&comment, handle);
    assert!(comment.contains("inside */\\n"), "{comment}");
    assert!(comment.contains("\"start\":\"0\",\"length\":\"9\",\"role\":\"comment\""), "{comment}");
    let code = take(fcb_reader_lines(handle, 3, 1, 1024));
    highlighted(&code, handle);
    assert!(code.contains("\"start\":\"0\",\"length\":\"2\",\"role\":\"keyword\""), "{code}");
    assert!(take(fcb_reader_copy_range(handle, 9, 18)).contains("696e73696465202a2f"));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn bom_unicode_and_crlf_keep_window_coordinates_and_exact_copy() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let bytes = "\u{feff}// 🦀\r\npub fn run() {}\r\n".as_bytes();
    let handle = supplied("unicode.rs", bytes);
    let page = take(fcb_reader_lines(handle, 2, 1, 1024));
    highlighted(&page, handle);
    assert!(page.contains("pub fn run() {}\\r\\n"), "{page}");
    assert!(page.contains("\"start\":\"0\",\"length\":\"3\",\"role\":\"keyword\""), "{page}");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert!(take(fcb_reader_copy_range(handle, 0, bytes.len() as u64))
        .contains(&format!("\"original_hex\":\"{hex}\"")));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn unsupported_capture_decoding_remains_readable_and_copyable() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let utf16: Vec<u8> = "\u{feff}fn main() {}\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    for bytes in [utf16.as_slice(), b"fn main() {}\n\xff"] {
        let handle = supplied("fallback.rs", bytes);
        // A valid UTF-8 prefix cannot establish that the complete capture is
        // lexically supported when malformed bytes occur after this page.
        let page = take(fcb_reader_window(handle, 0, 12));
        assert!(page.contains("\"status\":\"ok\""), "{page}");
        assert!(page.contains("\"state\":\"unsupported_encoding\""), "{page}");
        assert!(page.contains("\"runs\":[]"), "{page}");
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert!(take(fcb_reader_copy_range(handle, 0, bytes.len() as u64))
            .contains(&format!("\"original_hex\":\"{hex}\"")));
        assert_eq!(fcb_reader_close(handle), 1);
    }
}

#[test]
fn forks_keep_highlighting_and_query_authority_after_parent_close() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let parent = supplied("shared.rs", b"/* first\ninside */\nfn main() {}\n");
    highlighted(&take(fcb_reader_lines(parent, 2, 1, 1024)), parent);
    let child = fcb_reader_create();
    assert_ne!(child, 0);
    assert!(take(fcb_reader_fork(parent, child)).contains("\"status\":\"ok\""));
    assert_eq!(fcb_reader_close(parent), 1);
    let needle = CString::new("inside").unwrap();
    let found = take(unsafe { fcb_reader_find(child, 1, needle.as_ptr(), 10, 1024) });
    assert!(found.contains("\"retained_hits\":\"1\""), "{found}");
    let hit = take(fcb_reader_hit(child, 1, 0, 0));
    highlighted(&hit, child);
    assert!(hit.contains("\"role\":\"comment\""), "{hit}");
    assert!(take(fcb_reader_copy_hit(child, 1, 0)).contains("696e73696465"));
    assert_eq!(fcb_reader_cancel(child), 1);
    let delivered = fcb_reader_lines(child, 3, 1, 1024);
    assert_eq!(fcb_reader_close(child), 1);
    highlighted(&take(delivered), child); // The handoff owns its data after close.
}

#[test]
fn excessive_token_detail_does_not_refuse_or_truncate_source() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let source = "a+1;".repeat(6000);
    let handle = supplied("dense.rs", source.as_bytes());
    let page = take(fcb_reader_window(handle, 0, 256 * 1024));
    assert!(page.contains("\"status\":\"ok\""), "{page}");
    assert!(page.contains("\"state\":\"page_limit\""), "highlight must degrade within its own budget");
    assert!(page.contains("\"runs\":[]"));
    assert!(page.contains(&source), "plain fallback must keep the whole admitted page");
    assert_eq!(fcb_reader_close(handle), 1);
}

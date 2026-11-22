#![deny(unsafe_op_in_unsafe_fn)]
#![cfg(unix)]

use super::*;
use std::{ffi::{CStr, CString}, fs, sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a complete JSON or error handoff");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    text
}

fn source(bytes: &[u8]) -> (std::path::PathBuf, u64) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-delimiter-ffi-{}-{stamp}-{}.rs",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::write(&path, bytes).unwrap();
    let label = CString::new(path.to_str().unwrap()).unwrap();
    let handle = fcb_reader_create(); assert_ne!(handle, 0);
    let reply = take(unsafe { fcb_reader_open(handle, label.as_ptr(), 4096) });
    assert!(reply.contains("\"status\":\"ok\""), "{reply}");
    (path, handle)
}

fn field(name: &str, start: u64, end: u64) -> String {
    format!("\"{name}\":{{\"start\":\"{start}\",\"end\":\"{end}\"}}")
}

#[test]
fn c_matching_uses_captured_code_and_ignores_comment_and_string_brackets() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let text = "fn main() { let s = \"}\"; /* } */ [1, (2)] }\n";
    let (path, handle) = source(text.as_bytes());
    let start = text.find('{').unwrap() as u64;
    let mate = text.rfind('}').unwrap() as u64;
    fs::write(&path, b"the live file no longer has any brackets").unwrap();
    let reply = take(fcb_reader_matching_delimiter(handle, start, start + 1));
    assert!(reply.contains("\"status\":\"ok\""), "{reply}");
    assert!(reply.contains("\"command\":\"matching-delimiter\""));
    assert!(reply.contains("\"delimiter_state\":\"matched\""));
    assert!(reply.contains(&field("selected_range", start, start + 1)));
    assert!(reply.contains(&field("matching_range", mate, mate + 1)));
    assert!(reply.contains(&format!("\"owner\":\"{handle}\"")));
    assert!(reply.contains("\"evidence_level\":\"lexical-delimiter-candidate\""));
    assert!(reply.contains("\"semantic_resolution\":false"));
    assert!(reply.contains("\"source_context\":\"complete-capture\""));
    assert!(reply.contains("\"additional_source_bytes_read\":\"0\""));
    let inside = text.find("\"}\"").unwrap() as u64 + 1;
    let excluded = take(fcb_reader_matching_delimiter(handle, inside, inside + 1));
    assert!(excluded.contains("\"delimiter_state\":\"not-code\""), "{excluded}");
    assert!(excluded.contains("\"matching_range\":null"));
    let reverse = take(fcb_reader_matching_delimiter(handle, mate, mate + 1));
    assert!(reverse.contains(&field("matching_range", start, start + 1)), "{reverse}");
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_matching_utf16_preserves_original_offsets_after_bom_and_surrogate_pair() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let text = "\u{feff}// 🦀\r\nfn x() { [1] }\r\n";
    let start = text[..text.find('[').unwrap()].encode_utf16().count() as u64 * 2;
    let mate = text[..text.find(']').unwrap()].encode_utf16().count() as u64 * 2;
    for big_endian in [false, true] {
        let bytes: Vec<u8> = text.encode_utf16().flat_map(|u| {
            if big_endian { u.to_be_bytes() } else { u.to_le_bytes() }
        }).collect();
        let (_, handle) = source(&bytes);
        let reply = take(fcb_reader_matching_delimiter(handle, start, start + 2));
        assert!(reply.contains("\"delimiter_state\":\"matched\""), "{reply}");
        assert!(reply.contains(&field("matching_range", mate, mate + 2)));
        assert!(take(fcb_reader_matching_delimiter(handle, start + 1, start + 2))
            .contains("\"status\":\"error\""));
        assert_eq!(fcb_reader_close(handle), 1);
    }
}

#[test]
fn c_matching_refuses_invalid_ranges_and_keeps_unmatched_separate_from_mismatch() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let empty = fcb_reader_create(); assert_ne!(empty, 0);
    assert!(take(fcb_reader_matching_delimiter(empty, 0, 1)).contains("READER_HANDLE_NOT_OPEN"));
    assert_eq!(fcb_reader_close(empty), 1);
    assert!(take(fcb_reader_matching_delimiter(empty, 0, 1)).contains("READER_HANDLE_UNKNOWN"));
    for (bytes, state) in [(&b"([)]"[..], "mismatched"), (&b"([1]"[..], "unmatched")] {
        let (_, handle) = source(bytes);
        let reply = take(fcb_reader_matching_delimiter(handle, 0, 1));
        assert!(reply.contains(&format!("\"delimiter_state\":\"{state}\"")), "{reply}");
        assert!(reply.contains("\"matching_range\":null"));
        for (start, end) in [(0, 0), (1, 0), (0, u64::MAX), (u64::MAX, u64::MAX)] {
            assert!(take(fcb_reader_matching_delimiter(handle, start, end))
                .contains("\"status\":\"error\""));
        }
        assert!(take(fcb_reader_window(handle, 0, 64)).contains("\"status\":\"ok\""));
        assert_eq!(fcb_reader_close(handle), 1);
    }
}

#[test]
fn c_matching_response_remains_owned_after_close_and_header_matches_signature() {
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_reader_matching_delimiter;
    let header = include_str!("../include/fcb_reader_sessions.h");
    assert!(header.contains("char *fcb_reader_matching_delimiter(uint64_t handle, uint64_t start, uint64_t end);"));
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, handle) = source(b"[1]");
    assert_eq!(fcb_reader_cancel(handle), 1);
    let reply = fcb_reader_matching_delimiter(handle, 0, 1);
    assert_eq!(fcb_reader_close(handle), 1);
    assert!(take(reply).contains(&field("matching_range", 2, 3)));
}

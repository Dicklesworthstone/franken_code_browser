#![deny(unsafe_op_in_unsafe_fn)]

//! Real C-boundary line navigation; no fabricated successful reader receipts.

use super::*;
use std::{ffi::{CStr, CString}, fs::{self, OpenOptions}, os::unix::fs::FileExt,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use crate::reader_ffi::FFI_TEST_LOCK;

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a bounded JSON receipt");
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    result
}
fn number(json: &str, key: &str) -> u64 {
    let prefix = format!("\"{key}\":\"");
    json.split_once(prefix.as_str()).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn root() -> (std::path::PathBuf, CString) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fcb-observed-lines-ffi-{}-{}-{}",
        std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap();
    let text = CString::new(path.to_str().unwrap()).unwrap();
    (path, text)
}
fn open(root: &CString) -> u64 {
    let handle = fcb_observed_reader_create(); assert_ne!(handle, 0);
    let relative = CString::new("736f75726365").unwrap(); // source
    let result = take(unsafe { fcb_observed_reader_open(handle, root.as_ptr(), relative.as_ptr(),
        None, std::ptr::null_mut()) });
    assert!(result.contains("\"status\":\"ok\""), "{result}");
    assert_eq!(number(&result, "payload_bytes_read"), 0);
    handle
}
fn lines(handle: u64, first: u64, count: u64, bytes: u64, scan: u64, encoding: u64) -> String {
    take(unsafe { fcb_observed_reader_lines(handle, first, count, bytes, scan, encoding,
        None, std::ptr::null_mut()) })
}
fn original_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn c_line_navigation_beyond_capture_limit_keeps_same_scan_source_and_history() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let path = root.join("source");
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
    let target = 33 * 1024 * 1024u64;
    file.set_len(target + 17).unwrap();
    file.write_at(b"\nline forty\r\nnext\n", target - 1).unwrap();
    let handle = open(&croot);
    let page = lines(handle, 2, 1, 64, 64 * 1024 * 1024, 1);
    assert!(page.contains("\"status\":\"ok\""), "{page}");
    assert!(page.contains("\"command\":\"lines\""));
    assert!(page.contains("\"line_navigation\":{"));
    assert!(page.contains("\"first_physical_line\":\"2\""));
    assert!(page.contains("\"text\":\"line forty\\r\\n\""), "{page}");
    assert!(page.contains(&format!("\"visible_range\":{{\"start\":\"{target}\",\"end\":\"{}\"}}", target + 12)));
    assert!(page.contains("\"whole_file_immutable\":false"));
    assert!(page.len() < 16 * 1024, "the scanned prefix must not become retained output");
    let observation = number(&page, "observation");
    let spent = take(fcb_observed_reader_info(handle));
    assert!(number(&spent, "payload_bytes_read") > target);
    assert!(number(&spent, "retained_payload_bytes") <= 80);

    file.write_at(b"changed!!!\r\n", target).unwrap();
    fs::rename(&path, root.join("old-source")).unwrap();
    fs::write(&path, b"unrelated replacement\n").unwrap();
    let retained = take(fcb_observed_reader_retained(handle, observation));
    assert!(retained.contains("\"text\":\"line forty\\r\\n\""), "{retained}");
    assert!(retained.contains("\"first_physical_line\":\"2\""));
    assert!(retained.contains("\"line_navigation\":{"));
    assert!(retained.contains("\"additional_source_bytes_read\":\"0\""));
    let copy = take(fcb_observed_reader_copy(handle, observation, target, target + 12));
    assert!(copy.contains(&format!("\"original_hex\":\"{}\"", original_hex(b"line forty\r\n"))));
    assert_eq!(spent, take(fcb_observed_reader_info(handle)), "retained navigation cannot read live source");

    // New observations stay on the admitted descriptor after path replacement.
    let current = lines(handle, 2, 1, 64, 64 * 1024 * 1024, 1);
    assert!(current.contains("\"text\":\"changed!!!\\r\\n\""), "{current}");
    assert!(number(&current, "observation") > observation);
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

#[test]
fn c_line_navigation_preserves_utf16_crlf_and_original_copy_coordinates() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    for encoding in [2, 3] {
        let (root, croot) = root();
        let encode = |text: &str| text.encode_utf16().flat_map(|unit| if encoding == 2 {
            unit.to_le_bytes()
        } else { unit.to_be_bytes() }).collect::<Vec<_>>();
        let selected = encode("🦀 two\r");
        let prefix = encode("\u{feff}one\r\n");
        fs::write(root.join("source"), encode("\u{feff}one\r\n🦀 two\rlast\n")).unwrap();
        let handle = open(&croot);
        let page = lines(handle, 2, 1, 64, 1024, encoding);
        assert!(page.contains("\"status\":\"ok\""), "{page}");
        assert!(page.contains("\"text\":\"🦀 two\\r\""), "{page}");
        assert!(page.contains("\"first_physical_line\":\"2\""));
        assert!(page.contains(if encoding == 2 { "\"encoding\":\"utf16le\"" } else { "\"encoding\":\"utf16be\"" }));
        let observation = number(&page, "observation");
        let copied = take(fcb_observed_reader_copy(handle, observation,
            prefix.len() as u64, (prefix.len() + selected.len()) as u64));
        assert!(copied.contains(&format!("\"original_hex\":\"{}\"", original_hex(&selected))), "{copied}");
        let last = lines(handle, 3, 8, 64, 1024, encoding);
        assert!(last.contains("\"text\":\"last\\n\""), "{last}");
        assert!(last.contains("\"last_line\":\"3\""));
        let absent = lines(handle, 4, 1, 64, 1024, encoding);
        assert!(absent.contains("OBSERVED_READER_LINE_MISSING"), "{absent}");
        assert_eq!(fcb_observed_reader_close(handle), 1);
    }
}

unsafe extern "C" fn cancel_now(_: *mut c_void) -> i32 { 1 }

#[test]
fn c_line_limits_missing_and_cancellation_never_replace_accepted_pages() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    fs::write(root.join("source"), b"one\r\ntwo\nlast\n").unwrap();
    let handle = open(&croot);
    let accepted = lines(handle, 2, 1, 64, 1024, 1);
    assert!(accepted.contains("\"text\":\"two\\n\""), "{accepted}");
    let observation = number(&accepted, "observation");
    let before = take(fcb_observed_reader_info(handle));
    let partial = lines(handle, 3, 1, 64, 4, 1);
    assert!(partial.contains("OBSERVED_READER_LINE_SCAN_BYTE_LIMIT"), "{partial}");
    assert!(!partial.contains("LINE_MISSING"));
    let after = take(fcb_observed_reader_info(handle));
    assert!(number(&after, "payload_bytes_read") > number(&before, "payload_bytes_read"));
    assert!(lines(handle, 4, 1, 64, 1024, 1).contains("OBSERVED_READER_LINE_MISSING"));
    assert!(lines(handle, 1, 3, 4, 1024, 1).contains("OBSERVED_READER_LINE_OUTPUT_BYTE_LIMIT"));
    for (first, count, bytes, scan, encoding) in [
        (0, 1, 64, 1024, 1), (1, 0, 64, 1024, 1), (1, 4097, 64, 1024, 1),
        (u64::MAX, 2, 64, 1024, 1), (1, 1, 3, 1024, 1), (1, 1, 262_145, 1024, 1),
        (1, 1, u64::MAX, 1024, 1), (1, 1, 64, 0, 1), (1, 1, 64, (4u64 << 30) + 1, 1),
        (1, 1, 64, 1024, 0), (1, 1, 64, 1024, 4),
    ] {
        let refused = lines(handle, first, count, bytes, scan, encoding);
        assert!(refused.contains("\"status\":\"error\""), "{refused}");
    }
    let canceled = take(unsafe { fcb_observed_reader_lines(handle, 1, 1, 64, 1024, 1,
        Some(cancel_now), std::ptr::null_mut()) });
    assert!(canceled.contains("\"exit_code\":\"130\""), "{canceled}");
    let retained = take(fcb_observed_reader_retained(handle, observation));
    assert!(retained.contains("\"text\":\"two\\n\""), "{retained}");
    assert!(retained.contains("\"first_physical_line\":\"2\""));
    assert!(retained.contains("\"additional_source_bytes_read\":\"0\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
    assert!(lines(handle, 1, 1, 64, 1024, 1).contains("OBSERVED_READER_HANDLE_UNKNOWN"));
}

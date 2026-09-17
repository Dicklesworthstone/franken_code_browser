#![cfg(unix)]

//! Actual C ABI marshaling and safe production services. These tests do not
//! qualify a SwiftUI window, AppKit callback, native text layout or Metal frame.

use super::*;
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-abi-{}-{now}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn native(path: &Path) -> CString { CString::new(path.to_str().unwrap()).unwrap() }
fn take(pointer: *mut c_char) -> Option<String> {
    if pointer.is_null() { return None; }
    // Only pointers returned by the tested library enter this helper. Copy
    // before freeing, and release the exact allocation once through its owner.
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { fcb_free_string(pointer) };
    Some(text)
}

#[test]
fn null_inputs_and_invalid_utf8_are_refused_without_a_fictional_success() {
    let null = std::ptr::null();
    unsafe {
        assert!(fcb_read_file(null).is_null());
        assert!(fcb_read_file_window(null, 0, 64).is_null());
        assert!(fcb_read_file_lines(null, 1, 20).is_null());
        assert!(fcb_search_workspace(null, null).is_null());
        assert!(fcb_atlas_plan(null).is_null());
        assert!(fcb_atlas_layout(null).is_null());
        assert!(fcb_markdown_window(null, 1, 40, 100).is_null());
        assert!(fcb_markdown_heading(null, null, 40, 100).is_null());
        fcb_free_string(std::ptr::null_mut());
        let invalid = CString::new(vec![0xff]).unwrap();
        assert!(fcb_read_file(invalid.as_ptr()).is_null());
    }
}
#[test]
fn legacy_read_preserves_bytes_and_refuses_nul_invalid_text_and_oversize() {
    let file = root().join("source"); let path = native(&file);
    fs::write(&file, "\u{feff}é😀\r\n").unwrap();
    assert_eq!(take(unsafe { fcb_read_file(path.as_ptr()) }).unwrap(), "\u{feff}é😀\r\n");
    fs::write(&file, b"left\0right").unwrap();
    assert!(unsafe { fcb_read_file(path.as_ptr()) }.is_null());
    fs::write(&file, b"left\xffright").unwrap();
    assert!(unsafe { fcb_read_file(path.as_ptr()) }.is_null());
    fs::OpenOptions::new().write(true).open(&file).unwrap().set_len(host::MAX_HOST_TEXT_BYTES as u64 + 1).unwrap();
    assert!(unsafe { fcb_read_file(path.as_ptr()) }.is_null());
    fs::write(&file, b"").unwrap();
    assert_eq!(take(unsafe { fcb_read_file(path.as_ptr()) }).unwrap(), "");
}
#[test]
fn structured_readers_reuse_exact_source_decoding_and_keep_error_json() {
    let file = root().join("utf16"); let path = native(&file);
    let mut bytes = vec![0xff, 0xfe];
    for unit in "alpha\r\nbeta😀\r\nlast".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(&file, bytes).unwrap();
    let window = take(unsafe { fcb_read_file_window(path.as_ptr(), 0, 64) }).unwrap();
    assert_eq!(window, host::read_window(&file, 0, 64, || false).unwrap().as_str());
    assert!(window.contains("beta😀"));
    let lines = take(unsafe { fcb_read_file_lines(path.as_ptr(), 2, 1) }).unwrap();
    assert_eq!(lines, host::read_lines(&file, 2, 1, || false).unwrap().as_str());
    assert!(lines.contains("beta😀"));
    let invalid = take(unsafe { fcb_read_file_window(path.as_ptr(), 0, 0) }).unwrap();
    assert!(invalid.contains("\"status\":\"error\""));
    assert!(invalid.ends_with('\n'));
}
#[test]
fn legacy_and_versioned_atlas_routes_share_native_policy_and_expose_real_profiles() {
    let root = root(); let path = native(&root);
    fs::create_dir_all(root.join("src/deep")).unwrap(); fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("src/deep/main.rs"), b"one\r\ntwo\rthree\n").unwrap();
    fs::write(root.join("target/excluded.rs"), b"do not discover").unwrap();
    let legacy = take(unsafe { fcb_atlas_layout(path.as_ptr()) }).unwrap();
    assert!(legacy.contains("\"world\":{\"w\":4096,\"h\":4096}"));
    assert!(legacy.contains("\"path\":\"src/deep/main.rs\""));
    assert!(legacy.contains("\"source_lines\":\"3\""));
    assert!(legacy.contains("\"n\":3,"));
    assert!(!legacy.contains("excluded.rs"));
    let plan = take(unsafe { fcb_atlas_plan(path.as_ptr()) }).unwrap();
    assert!(plan.contains("fcb.atlas/1"));
    assert!(plan.contains("\"payload_bytes_read\":\"0\""));
    assert!(!plan.contains("excluded.rs"));
}
#[test]
fn search_and_document_exports_call_the_same_application_engines() {
    let root = root(); let path = native(&root); let needle = CString::new("beta").unwrap();
    let file = root.join("README.md"); let file_path = native(&file);
    fs::write(&file, "# Start\n\nalpha\n\n## Install\n\n**beta**\n").unwrap();
    let search = take(unsafe { fcb_search_workspace(path.as_ptr(), needle.as_ptr()) }).unwrap();
    assert_eq!(search, host::search_workspace(&root, "beta", || false).unwrap().as_str());
    assert!(search.contains("beta"));
    let markdown = take(unsafe { fcb_markdown_window(file_path.as_ptr(), 1, 40, 100) }).unwrap();
    assert_eq!(markdown, host::markdown_window(&file, 1, 40, 100, || false).unwrap().as_str());
    let slug = CString::new("install").unwrap();
    let heading = take(unsafe { fcb_markdown_heading(file_path.as_ptr(), slug.as_ptr(), 40, 100) }).unwrap();
    assert_eq!(heading, host::markdown_heading(&file, "install", 40, 100, || false).unwrap().as_str());
    assert!(heading.contains("beta"));
}
#[test]
fn abi_handoff_does_not_rewrite_nul_or_unwind_through_c() {
    assert!(string_out("left\0right").is_none());
    assert!(reply(|| panic!("injected service panic")).is_null());
    assert_eq!(take(reply(|| string_out("normal"))).as_deref(), Some("normal"));
}

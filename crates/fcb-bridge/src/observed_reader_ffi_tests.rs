#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::{ffi::{CStr, CString}, fs::{self, OpenOptions}, os::unix::{ffi::OsStringExt, fs::FileExt},
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use crate::reader_ffi::FFI_TEST_LOCK;

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a bounded JSON receipt");
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    result
}
fn root() -> (std::path::PathBuf, CString) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!("fcb-observed-ffi-{}-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    let text = CString::new(root.to_str().unwrap()).unwrap();
    (root, text)
}
fn hex(bytes: &[u8]) -> CString {
    CString::new(bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>()).unwrap()
}
fn open(root: &CString, relative: &[u8]) -> u64 {
    let handle = fcb_observed_reader_create(); assert_ne!(handle, 0);
    let relative = hex(relative);
    let info = take(unsafe { fcb_observed_reader_open(handle, root.as_ptr(), relative.as_ptr(), None, std::ptr::null_mut()) });
    assert!(info.contains("\"status\":\"ok\""), "{info}");
    assert!(info.contains("\"payload_bytes_read\":\"0\""));
    assert!(info.contains("\"whole_file_immutable\":false"));
    handle
}
fn window(handle: u64, offset: u64, bytes: u64, encoding: u64) -> String {
    take(unsafe { fcb_observed_reader_window(handle, offset, bytes, encoding, None, std::ptr::null_mut()) })
}

#[test]
fn c_sparse_file_past_four_gib_reads_only_requested_ranges_and_keeps_old_bytes() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(root.join("huge.rs")).unwrap();
    let offset = (1u64 << 32) + 1024;
    file.set_len(offset + 128).unwrap();
    file.write_at(b"before", offset).unwrap();
    let handle = open(&croot, b"huge.rs");
    let page = window(handle, offset, 6, 1);
    assert!(page.contains("\"text\":\"before\""), "{page}");
    assert!(page.contains(&format!("\"visible_range\":{{\"start\":\"{offset}\",\"end\":\"{}\"}}", offset + 6)));
    assert!(page.contains("\"observation_source_bytes_read\":\"22\""));
    assert!(page.contains("\"observation\":\"1\""));
    assert!(!page.contains("\"capture_sha256\""), "an extent digest must not become a whole-source digest");
    assert!(page.len() < 8192);
    file.write_at(b"after!", offset).unwrap();
    let newer = window(handle, offset, 6, 1);
    assert!(newer.contains("\"text\":\"after!\""));
    assert!(newer.contains("\"observation\":\"2\""));
    let retained = take(fcb_observed_reader_retained(handle, 1));
    assert!(retained.contains("\"text\":\"before\""));
    assert!(retained.contains("\"additional_source_bytes_read\":\"0\""));
    let copy = take(fcb_observed_reader_copy(handle, 1, offset, offset + 6));
    assert!(copy.contains("\"original_hex\":\"6265666f7265\""));
    assert!(copy.contains("\"additional_read_calls\":\"0\""));
    for _ in 0..7 { assert!(window(handle, offset, 6, 1).contains("\"status\":\"ok\"")); }
    assert!(take(fcb_observed_reader_copy(handle, 1, offset, offset + 6)).contains("\"status\":\"error\""));
    assert!(take(fcb_observed_reader_retained(handle, 1)).contains("\"status\":\"error\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
    assert!(window(handle, offset, 6, 1).contains("OBSERVED_READER_HANDLE_UNKNOWN"));
}

#[test]
fn c_root_relative_raw_names_and_explicit_utf16_preserve_original_coordinates() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let name = b"raw-\xff\\name.rs";
    let bytes: Vec<u8> = "\u{feff}A🦀B\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(root.join(std::ffi::OsString::from_vec(name.to_vec())), &bytes).unwrap();
    let handle = open(&croot, name);
    let page = window(handle, 2, 8, 2);
    assert!(page.contains("\"encoding\":\"utf16le\""));
    assert!(page.contains("A🦀B"), "{page}");
    assert!(page.contains("\"original_hex\":\"41003ed880dd4200\""), "{page}");
    assert!(take(fcb_observed_reader_copy(handle, 1, 2, 10)).contains("41003ed880dd4200"));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

unsafe extern "C" fn cancel_now(_: *mut c_void) -> i32 { 1 }

#[test]
fn c_invalid_inputs_and_cancellation_preserve_accepted_observations() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    fs::write(root.join("source.rs"), b"retained source\n").unwrap();
    let relative = hex(b"source.rs");
    let handle = fcb_observed_reader_create(); assert_ne!(handle, 0);
    for bad in [b"../source.rs".as_slice(), b"/source.rs", b"./source.rs", b"a//b", b"a\0b"] {
        let bad = hex(bad);
        assert!(take(unsafe { fcb_observed_reader_open(handle, croot.as_ptr(), bad.as_ptr(), None, std::ptr::null_mut()) })
            .contains("\"status\":\"error\""));
    }
    let malformed = CString::new("zz").unwrap();
    assert!(take(unsafe { fcb_observed_reader_open(handle, croot.as_ptr(), malformed.as_ptr(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    assert!(take(unsafe { fcb_observed_reader_open(handle, std::ptr::null(), relative.as_ptr(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    let invalid_utf8 = [255u8, 0];
    assert!(take(unsafe { fcb_observed_reader_open(handle, invalid_utf8.as_ptr().cast(), relative.as_ptr(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    let too_long = CString::new("x".repeat(MAX_PATH_BYTES + 1)).unwrap();
    assert!(take(unsafe { fcb_observed_reader_open(handle, too_long.as_ptr(), relative.as_ptr(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    assert!(take(unsafe { fcb_observed_reader_open(handle, croot.as_ptr(), relative.as_ptr(), Some(cancel_now), std::ptr::null_mut()) })
        .contains("\"exit_code\":\"130\""));
    assert!(take(fcb_observed_reader_info(handle)).contains("OBSERVED_READER_HANDLE_NOT_OPEN"));
    assert!(take(unsafe { fcb_observed_reader_open(handle, croot.as_ptr(), relative.as_ptr(), None, std::ptr::null_mut()) })
        .contains("\"payload_bytes_read\":\"0\""));
    assert!(window(handle, 0, 8, 1).contains("\"observation\":\"1\""));
    for (offset, bytes, encoding) in [(0, 0, 1), (0, 262_145, 1), (u64::MAX, 8, 1), (0, u64::MAX, 1), (0, 8, 0), (0, 8, 4)] {
        assert!(window(handle, offset, bytes, encoding).contains("\"status\":\"error\""));
    }
    assert!(take(unsafe { fcb_observed_reader_window(handle, 0, 8, 1, Some(cancel_now), std::ptr::null_mut()) })
        .contains("\"exit_code\":\"130\""));
    assert!(take(fcb_observed_reader_retained(handle, 1)).contains("\"text\":\"retained\""));
    assert_eq!(fcb_observed_reader_cancel(handle), 1);
    assert!(take(fcb_observed_reader_copy(handle, 1, 0, 8)).contains("72657461696e6564"));
    assert_eq!(fcb_observed_reader_close(handle), 1);
    assert_eq!(fcb_observed_reader_close(handle), 0);
}

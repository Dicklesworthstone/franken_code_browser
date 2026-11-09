#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close};
use crate::reader_ffi::FFI_TEST_LOCK;
use std::{ffi::{CStr, CString}, fs, os::unix::ffi::OsStrExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a complete response");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    text
}
fn field(json: &str, name: &str) -> u64 {
    let prefix = format!("\"{name}\":\"");
    json.split_once(prefix.as_str()).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("fcb-stream-ffi-{}-{}-{}",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn open(&self) -> u64 {
        // The retained grant may canonicalize an otherwise authorized spelling.
        let root = CString::new(self.0.join(".").to_str().unwrap()).unwrap();
        let handle = fcb_atlas_create(); assert_ne!(handle, 0);
        let opened = take(unsafe { fcb_atlas_open(handle, root.as_ptr(), 20_000) });
        assert!(opened.contains("\"status\":\"ok\""), "{opened}");
        let canonical = fs::canonicalize(&self.0).unwrap();
        let hex: String = canonical.as_os_str().as_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(opened.contains("\"root_path\":"));
        assert_eq!(field(&opened, "source_manifest"), 1);
        assert!(opened.contains(&format!("\"hex\":\"{hex}\"")), "{opened}");
        handle
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn search(handle: u64, needle: &str, matches: u64, bytes: u64, calls: u64) -> String {
    let needle = CString::new(needle).unwrap();
    take(unsafe { fcb_atlas_stream_search_cancelable(handle, needle.as_ptr(), matches, bytes,
        calls, None, std::ptr::null_mut()) })
}

#[test]
fn c_stream_finds_large_utf8_and_bom_utf16_files_and_keeps_exact_literal_after_replacement() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    let far = 1024 * 1024 + 17;
    let mut large = vec![b' '; far]; large.extend_from_slice(b"needle\n");
    fs::write(fixture.0.join("a.rs"), large).unwrap();
    let utf16: Vec<u8> = "\u{feff}x needle".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fs::write(fixture.0.join("b.rs"), utf16).unwrap();
    let handle = fixture.open();
    let result = search(handle, "needle", 256, 4 * 1024 * 1024, 4096);
    assert!(result.contains("\"schema\":\"fcb.atlas-stream-search/1\""), "{result}");
    assert!(result.contains("\"search_complete\":true"), "{result}");
    assert!(result.contains("\"capture_kind\":\"literal-witnesses-only\""));
    assert_eq!(field(&result, "retained_hits"), 2);
    let generation = field(&result, "query_generation");
    assert!(result.contains(&format!("\"original_range\":{{\"start\":\"{far}\",\"end\":\"{}\"}}", far + 6)));
    let second = take(fcb_atlas_stream_search_hit(handle, generation, 1));
    assert!(second.contains("\"original_hex\":\"006e006500650064006c0065\""), "{second}");
    fs::rename(fixture.0.join("a.rs"), fixture.0.join("retired.rs")).unwrap();
    fs::write(fixture.0.join("a.rs"), b"replacement object").unwrap();
    let retained = take(fcb_atlas_stream_search_hit(handle, generation, 0));
    assert!(retained.contains("\"original_hex\":\"6e6565646c65\""), "{retained}");
    assert!(retained.contains("\"additional_source_bytes_read\":\"0\""));
    assert!(retained.contains("\"additional_read_calls\":\"0\""));
    let page = take(fcb_atlas_stream_search_page(handle, generation, 1, 1));
    assert!(page.contains("b.rs"), "{page}");
    assert!(page.contains("\"next_offset\":null"));
    assert_eq!(fcb_atlas_close(handle), 1);
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 0)).contains("ATLAS_HANDLE_UNKNOWN"));
}

unsafe extern "C" fn canceled(_: *mut c_void) -> i32 { 1 }

#[test]
fn c_stream_rejects_foreign_input_and_caps_without_losing_previous_witness() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.rs"), "needle ".repeat(40)).unwrap();
    let handle = fixture.open();
    let accepted = search(handle, "needle", 256, 4096, 4096);
    let generation = field(&accepted, "query_generation");
    assert_eq!(field(&accepted, "retained_hits"), 40);
    assert!(accepted.contains("\"next_offset\":\"32\""));
    let needle = CString::new("needle").unwrap();
    let overlong = [b'x'; MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES + 1]; // No NUL, still bounded.
    let invalid_utf8 = [255u8, 0];
    for pointer in [std::ptr::null(), overlong.as_ptr().cast(), invalid_utf8.as_ptr().cast()] {
        let result = take(unsafe { fcb_atlas_stream_search_cancelable(handle, pointer,
            256, 4096, 4096, None, std::ptr::null_mut()) });
        assert!(result.contains("ATLAS_HANDLE_INVALID_ARGUMENT"), "{result}");
    }
    for (matches, bytes, calls) in [(0, 4096, 4096), (257, 4096, 4096),
        (1, 0, 1), (1, (4u64 << 30) + 1, 1), (1, 4096, 0), (1, 4096, 1_048_577)] {
        let rejected = search(handle, "needle", matches, bytes, calls);
        assert!(rejected.contains("\"status\":\"error\""), "{rejected}");
    }
    let stopped = take(unsafe { fcb_atlas_stream_search_cancelable(handle, needle.as_ptr(),
        256, 4096, 4096, Some(canceled), std::ptr::null_mut()) });
    assert!(stopped.contains("ATLAS_HANDLE_CANCELED"), "{stopped}");
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 0)).contains("6e6565646c65"));
    for (start, count) in [(0, 0), (0, 33), (u64::MAX, 1)] {
        assert!(take(fcb_atlas_stream_search_page(handle, generation, start, count))
            .contains("\"status\":\"error\""));
    }
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 40)).contains("\"status\":\"error\""));
    let limited = search(handle, "needle", 1, 4096, 4096);
    assert!(limited.contains("\"search_complete\":false"));
    assert_eq!(field(&limited, "retained_hits"), 1);
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 0)).contains("\"status\":\"error\""));
    let current = field(&limited, "query_generation");
    assert!(take(fcb_atlas_stream_search_clear(handle)).contains("\"status\":\"ok\""));
    assert!(take(fcb_atlas_stream_search_hit(handle, current, 0)).contains("\"status\":\"error\""));
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_stream_public_signatures_and_header_are_consistent() {
    let _: unsafe extern "C" fn(u64, *const c_char, u64, u64, u64,
        Option<SearchCancellationCallback>, *mut c_void) -> *mut c_char = fcb_atlas_stream_search_cancelable;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_stream_search_page;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_atlas_stream_search_hit;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_atlas_stream_search_clear;
    let header = include_str!("../include/fcb_atlas_stream_search.h");
    for name in ["fcb_atlas_stream_search_cancelable(", "fcb_atlas_stream_search_page(",
        "fcb_atlas_stream_search_hit(", "fcb_atlas_stream_search_clear("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}

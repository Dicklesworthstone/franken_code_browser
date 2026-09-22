#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use crate::reader_ffi::{fcb_reader_create, fcb_reader_copy_range, fcb_reader_close, fcb_reader_info};
use std::{ffi::{CStr, CString}, fs, path::PathBuf, sync::{Mutex, atomic::{AtomicU64, Ordering}}};
static TEST_LOCK: Mutex<()> = Mutex::new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!("fcb-atlas-ffi-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap(); fs::write(root.join("a.rs"), b"needle\n").unwrap(); Self(root)
    }
    fn open(&self) -> u64 {
        let a = fcb_atlas_create(); assert_ne!(a, 0);
        let root = CString::new(self.0.to_str().unwrap()).unwrap();
        let reply = take(unsafe { fcb_atlas_open(a, root.as_ptr(), 100) });
        assert!(reply.contains("\"status\":\"ok\""), "{reply}"); a
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}

#[test]
fn c_atlas_reader_journey_uses_retained_map_and_original_captured_bytes() {
    let _guard = TEST_LOCK.lock().unwrap(); let fixture = Fixture::new(); let a = fixture.open();
    assert!(take(fcb_atlas_view(a, 1)).contains("\"native_presented\":false"));
    assert!(take(fcb_atlas_pick(a, 1, 1, 512.0, 384.0)).contains("ATLAS_SESSION_NOT_PRESENTED"));
    assert!(take(fcb_atlas_present(a, 1, 1, 1)).contains("host-declared-not-observed"));
    assert!(take(fcb_atlas_pick(a, 1, 1, 512.0, 384.0)).contains("\"can_open_source\":true"));
    let r = fcb_reader_create(); assert_ne!(r, 0); assert_ne!(a, r);
    let receipt = take(fcb_atlas_open_reader(a, r, 1, 1, 512.0, 384.0, 1024));
    assert!(receipt.contains(&format!("\"reader_owner\":\"{r}\"")), "{receipt}");
    fs::write(fixture.0.join("a.rs"), b"changed").unwrap();
    let pending = take(fcb_atlas_pan(a, 2, 10000.0, 10000.0));
    assert!(pending.contains("\"parcels\":[]"));
    assert!(take(fcb_atlas_pick(a, 1, 1, 512.0, 384.0)).contains("\"can_open_source\":true"));
    take(fcb_atlas_present(a, 2, 2, 1));
    assert!(take(fcb_atlas_pick(a, 1, 1, 512.0, 384.0)).contains("ATLAS_SESSION_STALE_FRAME"));
    assert_eq!(fcb_atlas_close(a), 1);
    assert!(take(fcb_reader_copy_range(r, 0, 6)).contains("6e6565646c65"));
    assert_eq!(fcb_reader_close(r), 1);
}

#[test]
fn c_invalid_inputs_and_wrong_handle_kinds_are_explicit_errors() {
    let _guard = TEST_LOCK.lock().unwrap(); let fixture = Fixture::new();
    let a = fcb_atlas_create(); assert_ne!(a, 0);
    assert!(take(unsafe { fcb_atlas_open(a, std::ptr::null(), 100) }).contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_atlas_info(a)).contains("ATLAS_HANDLE_NOT_OPEN"));
    let root = CString::new(fixture.0.to_str().unwrap()).unwrap();
    take(unsafe { fcb_atlas_open(a, root.as_ptr(), 100) });
    assert!(take(fcb_atlas_pan(a, 1, f64::NAN, 0.0)).contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_atlas_focus(a, 2, u64::MAX)).contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_atlas_children(a, 0, 0, 0)).contains("ATLAS_SESSION_INVALID_LIMITS"));
    assert!(take(fcb_reader_info(a)).contains("READER_HANDLE_UNKNOWN"));
    let r = fcb_reader_create(); assert_ne!(r, 0);
    assert!(take(fcb_atlas_info(r)).contains("ATLAS_HANDLE_UNKNOWN"));
    assert_eq!(fcb_reader_close(r), 1); assert_eq!(fcb_atlas_close(a), 1);
}

#[test]
fn c_cancel_and_close_do_not_free_already_returned_strings() {
    let _guard = TEST_LOCK.lock().unwrap(); let fixture = Fixture::new(); let a = fixture.open();
    assert_eq!(fcb_atlas_cancel(a), 1);
    let pointer = fcb_atlas_view(a, 1);
    assert_eq!(fcb_atlas_close(a), 1);
    assert!(take(pointer).contains("\"plan_generation\":\"1\""));
    assert_eq!(fcb_atlas_close(a), 0); assert_eq!(fcb_atlas_cancel(a), 0);
    assert!(take(fcb_atlas_info(a)).contains("ATLAS_HANDLE_UNKNOWN"));
}

#[test]
fn c_atlas_scope_filters_display_and_labels_responses() {
    let _guard = TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-atlas-scope-ffi-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.rs"), b"rust\n").unwrap();
    fs::write(root.join("b.md"), b"markdown\n").unwrap();
    let fixture = Fixture(root);
    let a = {
        let handle = fcb_atlas_create(); assert_ne!(handle, 0);
        let croot = CString::new(fixture.0.to_str().unwrap()).unwrap();
        let reply = take(unsafe { fcb_atlas_open(handle, croot.as_ptr(), 100) });
        assert!(reply.contains("\"catalogued_files\":\"2\""), "{reply}"); handle
    };
    // Invalid scope names and malformed custom lists are explicit refusals.
    let bogus = CString::new("bogus").unwrap();
    assert!(take(unsafe { fcb_atlas_scope(a, 1, bogus.as_ptr(), std::ptr::null()) })
        .contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    let empty = CString::new("extensions").unwrap();
    let blank = CString::new("").unwrap();
    assert!(take(unsafe { fcb_atlas_scope(a, 1, empty.as_ptr(), blank.as_ptr()) })
        .contains("ATLAS_HANDLE_INVALID_ARGUMENT"));

    let rust = CString::new("rust").unwrap();
    let scoped = take(unsafe { fcb_atlas_scope(a, 2, rust.as_ptr(), std::ptr::null()) });
    assert!(scoped.contains("\"catalogued_files\":\"1\""), "{scoped}");
    assert!(scoped.contains("\"scope\":{\"kind\":\"extensions\",\"extensions\":[\"rs\"]}"));
    assert!(scoped.contains("\"workspace_files\":\"2\""));
    // Restoring All reuses the retained original layout revision, while the
    // scoped plan reported a fresh, distinct revision.
    let original = take(fcb_atlas_info(a));
    let all = CString::new("all").unwrap();
    let restored = take(unsafe { fcb_atlas_scope(a, 3, all.as_ptr(), std::ptr::null()) });
    assert!(restored.contains("\"catalogued_files\":\"2\""));
    let after = take(fcb_atlas_info(a));
    let revision = |text: &str| text[text.find("\"layout_revision\":").unwrap()
        + "\"layout_revision\":".len()..].split(',').next().unwrap().to_string();
    assert_eq!(revision(&after), revision(&original));
    assert_ne!(revision(&scoped), revision(&original));

    // Custom extension lists parse exactly; case folds are applied.
    let custom = CString::new("extensions").unwrap();
    let list = CString::new("MD,TXT").unwrap();
    let mixed = take(unsafe { fcb_atlas_scope(a, 4, custom.as_ptr(), list.as_ptr()) });
    assert!(mixed.contains("\"extensions\":[\"md\",\"txt\"]"), "{mixed}");
    assert_eq!(fcb_atlas_close(a), 1);
}

#[test]
fn c_atlas_function_types_match_header() {
    let _: extern "C" fn() -> u64 = fcb_atlas_create;
    let _: unsafe extern "C" fn(u64, *const c_char, u64) -> *mut c_char = fcb_atlas_open;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_atlas_info;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_view;
    let _: extern "C" fn(u64, u64, f64, f64) -> *mut c_char = fcb_atlas_pan;
    let _: extern "C" fn(u64, u64, f64, f64, f64) -> *mut c_char = fcb_atlas_zoom;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_atlas_focus;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_back;
    let _: extern "C" fn(u64, u64, f64, f64, f64) -> *mut c_char = fcb_atlas_resize;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_present;
    let _: extern "C" fn(u64, u64, u64, f64, f64) -> *mut c_char = fcb_atlas_pick;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, *const c_char) -> *mut c_char = fcb_atlas_scope;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_children;
    let _: extern "C" fn(u64, u64, u64, u64, f64, f64, u64) -> *mut c_char = fcb_atlas_open_reader;
    let _: extern "C" fn(u64) -> u8 = fcb_atlas_cancel;
    let _: extern "C" fn(u64) -> u8 = fcb_atlas_close;
}

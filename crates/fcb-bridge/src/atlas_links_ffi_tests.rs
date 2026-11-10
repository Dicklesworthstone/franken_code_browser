#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close};
use crate::atlas_ffi::search::index::{fcb_atlas_index_prepare, fcb_atlas_index_clear};
use crate::reader_ffi::{fcb_reader_create, fcb_reader_info, fcb_reader_copy_range,
    fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString}, fs, path::PathBuf};

const DOC: &[u8] = b"# Guide\n\n[engine](src/lib.rs#entry) and [remote](https://example.invalid/doc).\n";
const SOURCE: &[u8] = b"fn original_entry() {}\n";
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let value = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; value
}
fn workspace() -> (PathBuf, u64) {
    let root = std::env::temp_dir().join(format!("fcb-atlas-links-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("README.md"), DOC).unwrap(); fs::write(root.join("src/lib.rs"), SOURCE).unwrap();
    let handle = fcb_atlas_create(); assert_ne!(handle, 0);
    let text = CString::new(root.to_str().unwrap()).unwrap();
    assert!(take(unsafe { fcb_atlas_open(handle, text.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    assert!(take(fcb_atlas_index_prepare(handle, 1, 100, 8192, 65536, 65536)).contains("\"capture_complete\":true"));
    (root, handle)
}
fn begin(handle: u64, generation: u64) -> String {
    take(fcb_atlas_links_begin(handle, generation, 1, 100, 65536))
}
fn finish(handle: u64, generation: u64) -> String {
    for _ in 0..100 {
        let step = take(fcb_atlas_links_step(handle, generation));
        assert!(step.contains("\"status\":\"ok\""), "{step}");
        if step.contains("\"relationships_in_progress\":false") { return step; }
    }
    panic!("bounded link inventory did not finish");
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn c_backlinks_transfer_captured_evidence_and_target_without_live_source_fallback() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let (root, handle) = workspace();
    let admitted = begin(handle, 1);
    assert!(admitted.contains("\"schema\":\"fcb.atlas-links/1\""), "{admitted}");
    assert!(admitted.contains("\"command\":\"links-begin\""));
    let finished = finish(handle, 1);
    assert!(finished.contains("\"retained_links\":\"2\""), "{finished}");
    assert!(finished.contains("whole-captured-markdown-document"));
    let path = CString::new(hex(b"src/lib.rs")).unwrap();
    let incoming = take(unsafe { fcb_atlas_links_page(handle, 1, 2, path.as_ptr(), 0, 1) });
    assert_eq!(incoming.matches("\"link_id\":").count(), 1);
    assert!(incoming.contains("src/lib.rs#entry"));
    assert!(incoming.contains("\"source_bytes_read\":\"0\""));
    fs::write(root.join("README.md"), b"changed documentation").unwrap();
    fs::write(root.join("src/lib.rs"), b"changed source").unwrap();
    let evidence = fcb_reader_create(); let target = fcb_reader_create();
    assert_ne!(evidence, 0); assert_ne!(target, 0);
    for (reader, side) in [(evidence, 0), (target, 1)] {
        let receipt = take(fcb_atlas_links_open_reader(handle, reader, 1, 1, side));
        assert!(receipt.contains("\"command\":\"links-open-reader\""), "{receipt}");
        assert!(receipt.contains("\"source_reopened\":false"));
        assert!(receipt.contains(&format!("\"reader_owner\":\"{reader}\"")));
    }
    let retained_reply = unsafe { fcb_atlas_links_page(handle, 1, 0, std::ptr::null(), 0, 128) };
    assert!(take(fcb_atlas_index_clear(handle, 2)).contains("\"status\":\"ok\""));
    assert!(take(unsafe { fcb_atlas_links_page(handle, 1, 0, std::ptr::null(), 0, 128) }).contains("\"status\":\"error\""));
    assert_eq!(fcb_atlas_close(handle), 1);
    assert!(take(retained_reply).contains("src/lib.rs#entry"));
    for (reader, bytes) in [(evidence, DOC), (target, SOURCE)] {
        assert!(take(fcb_reader_copy_range(reader, 0, bytes.len() as u64)).contains(&hex(bytes)));
        assert_eq!(fcb_reader_close(reader), 1);
    }
}

#[test]
fn c_invalid_paths_directions_and_targets_leave_readers_empty_and_inventory_intact() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let (_, handle) = workspace();
    begin(handle, 1); finish(handle, 1);
    for path in ["", "0", "6A", "zz", "00", "2e2e2f524541444d452e6d64", "616273656e742e6d64"] {
        let path = CString::new(path).unwrap();
        let refused = take(unsafe { fcb_atlas_links_page(handle, 1, 1, path.as_ptr(), 0, 128) });
        assert!(refused.contains("\"status\":\"error\""), "{refused}");
    }
    let oversized = vec![b'a'; 2 * MAX_SEARCH_SCOPE_PATH_BYTES + 1];
    assert!(take(unsafe { fcb_atlas_links_page(handle, 1, 1, oversized.as_ptr().cast(), 0, 128) })
        .contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    for (direction, limit) in [(1, 128), (3, 128), (0, 0), (0, 129)] {
        assert!(take(unsafe { fcb_atlas_links_page(handle, 1, direction, std::ptr::null(), 0, limit) })
            .contains("\"status\":\"error\""));
    }
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    for (id, side) in [(0, 0), (999, 0), (1, 2), (2, 1)] {
        assert!(take(fcb_atlas_links_open_reader(handle, reader, 1, id, side)).contains("\"status\":\"error\""));
        assert!(take(fcb_reader_info(reader)).contains("READER_HANDLE_NOT_OPEN"));
    }
    assert!(take(unsafe { fcb_atlas_links_page(handle, 1, 0, std::ptr::null(), 0, 128) })
        .contains("\"retained_links\":\"2\""));
    assert_eq!(fcb_reader_close(reader), 1); assert_eq!(fcb_atlas_close(handle), 1);
}

struct PollCounter { calls: usize, stop_at: usize }
unsafe extern "C" fn poll(context: *mut c_void) -> i32 {
    let counter = unsafe { &mut *context.cast::<PollCounter>() };
    counter.calls += 1; i32::from(counter.calls >= counter.stop_at)
}
#[test]
fn c_cooperative_begin_and_step_cancellation_preserve_accepted_links() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let (_, handle) = workspace();
    begin(handle, 1); finish(handle, 1);
    let mut counter = PollCounter { calls: 0, stop_at: 1 };
    let canceled = take(unsafe { fcb_atlas_links_begin_cancelable(handle, 2, 1, 100, 65536,
        Some(poll), (&mut counter as *mut PollCounter).cast()) });
    assert!(canceled.contains("CANCELED"), "{canceled}");
    assert_eq!(counter.calls, 1);
    // A refusal before entering the host did not consume its generation.
    assert!(begin(handle, 2).contains("\"status\":\"ok\""));
    counter.calls = 0; counter.stop_at = 2;
    let canceled = take(unsafe { fcb_atlas_links_step_cancelable(handle, 2,
        Some(poll), (&mut counter as *mut PollCounter).cast()) });
    assert!(canceled.contains("CANCELED"), "{canceled}");
    assert!(counter.calls >= 2);
    let previous = take(unsafe { fcb_atlas_links_page(handle, 1, 0, std::ptr::null(), 0, 128) });
    assert!(previous.contains("\"retained_links\":\"2\""), "{previous}");
    assert!(begin(handle, 3).contains("\"status\":\"ok\""));
    assert!(finish(handle, 3).contains("\"retained_links\":\"2\""));
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_signatures_and_bounded_hex_marshaling_preserve_exact_native_path_bytes() {
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_links_begin;
    let _: unsafe extern "C" fn(u64, u64, u64, u64, u64, Option<SearchCancellationCallback>, *mut c_void)
        -> *mut c_char = fcb_atlas_links_begin_cancelable;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_links_step;
    let _: unsafe extern "C" fn(u64, u64, Option<SearchCancellationCallback>, *mut c_void)
        -> *mut c_char = fcb_atlas_links_step_cancelable;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, u64, u64) -> *mut c_char = fcb_atlas_links_page;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_links_open_reader;
    let header = include_str!("../include/fcb_atlas_links.h");
    for name in ["fcb_atlas_links_begin(", "fcb_atlas_links_begin_cancelable(", "fcb_atlas_links_step(",
        "fcb_atlas_links_step_cancelable(", "fcb_atlas_links_page(", "fcb_atlas_links_open_reader("] {
        assert_eq!(header.matches(name).count(), 1);
    }
    let raw = b"src/e\xcc\x81-\xff.rs";
    let encoded = CString::new(hex(raw)).unwrap();
    assert_eq!(unsafe { decode_path(encoded.as_ptr()) }.unwrap().as_deref(), Some(raw.as_slice()));
    assert_eq!(unsafe { decode_path(std::ptr::null()) }.unwrap(), None);
    let maximum = CString::new("61".repeat(MAX_SEARCH_SCOPE_PATH_BYTES)).unwrap();
    assert_eq!(unsafe { decode_path(maximum.as_ptr()) }.unwrap().unwrap().len(), MAX_SEARCH_SCOPE_PATH_BYTES);
    let unterminated = vec![b'0'; 2 * MAX_SEARCH_SCOPE_PATH_BYTES + 1];
    assert_eq!(unsafe { decode_path(unterminated.as_ptr().cast()) }.err(), Some(AccessError::InvalidArgument));
}

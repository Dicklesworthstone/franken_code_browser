#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::{fcb_saved_create, fcb_saved_open, fcb_saved_info, fcb_saved_close,
    fcb_saved_attach_index, fcb_saved_search, fcb_saved_results};
use std::{ffi::{CStr, CString}, path::PathBuf};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected an owned response");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn value<'a>(text: &'a str, key: &str) -> &'a str {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap()
}
fn count(text: &str, key: &str) -> u64 { value(text, key).parse().unwrap() }
fn path_text(path: &std::path::Path) -> CString { CString::new(path.to_str().unwrap()).unwrap() }
fn fixture() -> (PathBuf, CString) {
    let root = std::env::temp_dir().join(format!("fcb-saved-index-build-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(9015).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let source = b"old needle needle\n";
    let entries = [SnapshotEntry { path: b"src/main.rs", observed_bytes: source.len() as u64,
        data: SnapshotData::Captured(source) }];
    let bytes = SnapshotBytes::encode(owner, true, "saved-index-build-c", &entries,
        Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = root.join("saved.fcbs"); std::fs::write(&path, bytes.bytes()).unwrap();
    let path = path_text(&path); (root, path)
}
fn build(saved: u64, operation: u64, path: &std::path::Path) -> String {
    take(unsafe { fcb_saved_build_index(saved, operation, path_text(path).as_ptr(),
        262_144, 67_108_864, 2_097_152) })
}
fn search(saved: u64, generation: u64, needle: &str) -> String {
    take(unsafe { fcb_saved_search(saved, generation, CString::new(needle).unwrap().as_ptr(), 32) })
}

#[test]
fn actual_c_build_returns_trusted_pin_and_attaches_only_on_explicit_request() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64) -> *mut c_char = fcb_saved_build_index;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64,
        Option<crate::SearchCancellationCallback>, *mut c_void) -> *mut c_char = fcb_saved_build_index_cancelable;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_saved_index_build_receipt;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["build_index", "build_index_cancelable", "index_build_receipt"] {
        assert_eq!(header.matches(&format!("fcb_saved_{name}(")).count(), 1);
    }
    let (root, archive) = fixture(); let saved = fcb_saved_create(); assert_ne!(saved, 0);
    assert!(take(unsafe { fcb_saved_open(saved, archive.as_ptr()) }).contains("\"status\":\"ok\""));
    search(saved, 1, "old");
    // The operation must read the retained descriptor, never reopen its label.
    std::fs::rename(root.join("saved.fcbs"), root.join("retained.fcbs")).unwrap();
    let destination = root.join("built.fcbd");
    let built = build(saved, 1, &destination);
    assert!(built.contains("\"status\":\"ok\""));
    assert!(built.contains("\"coverage_complete\":true"));
    assert!(built.contains("\"attach_eligible\":true"));
    assert!(built.contains("\"index_attached\":false"));
    assert_eq!(count(&built, "bytes_written"), std::fs::metadata(&destination).unwrap().len());
    assert_eq!(count(&built, "bytes_written"), count(&built, "index_bytes"));
    let info = take(fcb_saved_info(saved));
    assert!(info.contains("\"accepted_index_generation\":null"));
    assert_eq!(count(&info, "accepted_query_generation"), 1);
    assert_eq!(count(&info, "last_index_build_operation"), 1);
    assert!(take(fcb_saved_results(saved, 1, 0, 32)).contains("\"needle\":\"old\""));
    let pin = CString::new(value(&built, "index_pin")).unwrap(); assert_eq!(pin.as_bytes().len(), 64);
    assert!(take(unsafe { fcb_saved_attach_index(saved, 1, path_text(&destination).as_ptr(), pin.as_ptr()) })
        .contains("\"status\":\"ok\""));
    let found = search(saved, 2, "needle");
    assert!(found.contains("\"index_used\":true")); assert_eq!(count(&found, "retained_hits"), 2);
    let before = take(fcb_saved_info(saved));
    let replay = fcb_saved_index_build_receipt(saved, 1);
    let after = take(fcb_saved_info(saved));
    assert_eq!(count(&before, "member_read_calls"), count(&after, "member_read_calls"));
    assert_eq!(count(&before, "member_bytes_read"), count(&after, "member_bytes_read"));
    let refused = root.join("stale.fcbd");
    assert!(build(saved, 1, &refused).contains("SAVED_SESSION_STALE_INDEX_BUILD"));
    assert!(!refused.exists());
    assert!(take(unsafe { fcb_saved_build_index(saved, 2, path_text(&refused).as_ptr(),
        262_145, 67_108_864, 2_097_152) }).contains("\"status\":\"error\""));
    assert!(!refused.exists());
    assert_eq!(take(fcb_saved_index_build_receipt(saved, 1)), built);
    assert_eq!(fcb_saved_close(saved), 1);
    assert_eq!(take(replay), built); // owned reply outlives its source handle
}

unsafe extern "C" fn cancel_when_created(context: *mut c_void) -> i32 {
    let path = unsafe { &*(context.cast::<PathBuf>()) };
    if path.exists() { 1 } else { 0 }
}

#[test]
fn actual_c_cancel_after_create_preserves_effect_and_recovers_without_rewriting() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (root, archive) = fixture(); let saved = fcb_saved_create();
    take(unsafe { fcb_saved_open(saved, archive.as_ptr()) }); search(saved, 1, "old");
    let mut destination = root.join("canceled.fcbd");
    let path = path_text(&destination);
    let response = take(unsafe { fcb_saved_build_index_cancelable(saved, 1, path.as_ptr(),
        262_144, 67_108_864, 2_097_152, Some(cancel_when_created),
        (&mut destination as *mut PathBuf).cast::<c_void>()) });
    assert!(response.contains("\"status\":\"error\""));
    assert!(response.contains("SAVED_INDEX_WRITE_CANCELED"));
    assert!(response.contains("\"effect\":\"destination-created-incomplete\""));
    assert!(response.contains("\"attach_eligible\":false"));
    assert_eq!(count(&response, "bytes_written"), 0);
    assert_eq!(std::fs::metadata(&destination).unwrap().len(), 0);
    // Receipt recovery does not consult or replace a destination changed later.
    std::fs::write(&destination, b"external change after canceled build").unwrap();
    assert_eq!(take(fcb_saved_index_build_receipt(saved, 1)), response);
    assert_eq!(std::fs::read(&destination).unwrap(), b"external change after canceled build");
    assert!(take(fcb_saved_results(saved, 1, 0, 32)).contains("\"needle\":\"old\""));
    assert_eq!(fcb_saved_close(saved), 1);
}

#[test]
fn actual_c_existing_output_is_preserved_and_zero_coverage_indexes_fall_back_to_source() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (root, archive) = fixture(); let saved = fcb_saved_create();
    take(unsafe { fcb_saved_open(saved, archive.as_ptr()) });
    let existing = root.join("existing.fcbd"); std::fs::write(&existing, b"user file").unwrap();
    let refused = build(saved, 1, &existing);
    assert!(refused.contains("SAVED_INDEX_DESTINATION_EXISTS"));
    assert!(refused.contains("\"effect\":\"none\""));
    assert!(refused.contains("\"attach_eligible\":false"));
    assert_eq!(std::fs::read(&existing).unwrap(), b"user file");
    let destination = root.join("uncovered.fcbd");
    let built = take(unsafe { fcb_saved_build_index(saved, 2, path_text(&destination).as_ptr(), 0, 0, 0) });
    assert!(built.contains("\"status\":\"ok\""));
    assert!(built.contains("\"coverage_complete\":false"));
    assert!(built.contains("\"attach_eligible\":true"));
    assert_eq!(count(&built, "index_uncovered_files"), 1);
    assert_eq!(count(&built, "index_indexed_files"), 0);
    assert!(take(fcb_saved_index_build_receipt(saved, 1)).contains("SAVED_SESSION_STALE_INDEX_BUILD"));
    let pin = CString::new(value(&built, "index_pin")).unwrap();
    take(unsafe { fcb_saved_attach_index(saved, 1, path_text(&destination).as_ptr(), pin.as_ptr()) });
    let found = search(saved, 1, "needle");
    assert!(found.contains("\"search_complete\":true")); assert_eq!(count(&found, "retained_hits"), 2);
    assert_eq!(fcb_saved_close(saved), 1);
}

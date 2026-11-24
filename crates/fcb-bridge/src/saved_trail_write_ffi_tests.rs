#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::{fcb_saved_create, fcb_saved_open, fcb_saved_info, fcb_saved_close};
use super::super::trail::{fcb_saved_trail_attach, fcb_saved_trail_page};
use std::{ffi::{CStr, CString}, path::PathBuf};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget, snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}, trail::TrailView};

const SOURCE: &[u8] = b"Exact saved source, never its live pathname.\n";
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected an owned response");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn value<'a>(text: &'a str, key: &str) -> &'a str {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap()
}
fn count(text: &str, key: &str) -> u64 { value(text, key).parse().unwrap() }
fn path_text(path: &Path) -> CString { CString::new(path.to_str().unwrap()).unwrap() }
fn fixture() -> (PathBuf, CString) {
    let root = std::env::temp_dir().join(format!("fcb-saved-trail-write-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(9022).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let bytes = SnapshotBytes::encode(owner, true, "trail-write-c", &[SnapshotEntry {
        path: b"raw-\xff.rs", observed_bytes: SOURCE.len() as u64, data: SnapshotData::Captured(SOURCE) }],
        Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let archive = root.join("saved.fcbs"); std::fs::write(&archive, bytes.bytes()).unwrap();
    (root, path_text(&archive))
}
fn write(saved: u64, operation: u64, destination: &Path) -> String {
    let title = b"A\0title"; let note = b"Rationale\0bytes\n";
    take(unsafe { fcb_saved_trail_write(saved, operation, 0, c"".as_ptr(), 0, 6, 11,
        title.as_ptr(), title.len() as u64, note.as_ptr(), note.len() as u64, path_text(destination).as_ptr()) })
}

#[test]
fn actual_c_authoring_preserves_embedded_nul_raw_path_and_retained_descriptor() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, u64, u64, u64,
        *const u8, u64, *const u8, u64, *const c_char) -> *mut c_char = fcb_saved_trail_write;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, u64, u64, u64,
        *const u8, u64, *const u8, u64, *const c_char, Option<crate::SearchCancellationCallback>,
        *mut c_void) -> *mut c_char = fcb_saved_trail_write_cancelable;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_saved_trail_write_receipt;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["trail_write", "trail_write_cancelable", "trail_write_receipt"] {
        assert_eq!(header.matches(&format!("fcb_saved_{name}(")).count(), 1);
    }
    let (root, archive) = fixture(); let saved = fcb_saved_create();
    assert_ne!(saved, 0); take(unsafe { fcb_saved_open(saved, archive.as_ptr()) });
    std::fs::rename(root.join("saved.fcbs"), root.join("held.fcbs")).unwrap();
    let destination = root.join("new.fcbt"); let response = write(saved, 41, &destination);
    assert!(response.contains("\"status\":\"ok\""));
    assert!(response.contains("\"title\":\"A\\u0000title\""));
    assert!(response.contains("\"rationale\":\"Rationale\\u0000bytes\\n\""));
    assert!(response.contains("\"hex\":\"7261772dff2e7273\""));
    assert_eq!(count(&response, "member_bytes_read"), SOURCE.len() as u64);
    assert_eq!(count(&response, "bytes_written"), count(&response, "trail_bytes"));
    let encoded = std::fs::read(&destination).unwrap(); let view = TrailView::open(&encoded, || false).unwrap();
    assert_eq!(view.title(), "A\0title"); assert_eq!(view.entry(0).unwrap().rationale, "Rationale\0bytes\n");
    assert_eq!(view.entry(0).unwrap().path, b"raw-\xff.rs");
    let before = take(fcb_saved_info(saved));
    assert!(before.contains("\"accepted_trail_generation\":null"));
    assert_eq!(count(&before, "last_trail_write_operation"), 41);
    let held_reply = fcb_saved_trail_write_receipt(saved, 41);
    assert!(take(unsafe { fcb_saved_trail_attach(saved, 7, path_text(&destination).as_ptr()) }).contains("\"status\":\"ok\""));
    let title = b"A\0title"; let note = "Append exact point"; let pin = CString::new(value(&response, "trail_digest")).unwrap();
    let appended_path = root.join("appended.fcbt");
    let appended = take(unsafe { fcb_saved_trail_write(saved, 42, 7, pin.as_ptr(), 0, 11, 11,
        title.as_ptr(), title.len() as u64, note.as_ptr(), note.len() as u64, path_text(&appended_path).as_ptr()) });
    assert!(appended.contains("\"status\":\"ok\"")); assert_eq!(count(&appended, "total_items"), 2);
    assert_eq!(count(&appended, "reference_bytes"), 5);
    assert_eq!(count(&take(fcb_saved_trail_page(saved, 7, 0, 32)), "total_items"), 1);
    let after = take(fcb_saved_info(saved));
    assert_eq!(count(&after, "member_bytes_read"), SOURCE.len() as u64 * 2);
    assert_eq!(take(fcb_saved_trail_write_receipt(saved, 42)), appended);
    assert_eq!(fcb_saved_close(saved), 1); assert_eq!(take(held_reply), response);
}

#[test]
fn actual_c_rejects_bad_slice_parent_and_range_inputs_before_source_or_destination_io() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (root, archive) = fixture(); let saved = fcb_saved_create(); take(unsafe { fcb_saved_open(saved, archive.as_ptr()) });
    let destination = root.join("must-not-exist.fcbt"); let target = path_text(&destination);
    let bad_utf8 = [0xff]; let title = b"T";
    let cases = [
        (std::ptr::null(), 1), (title.as_ptr(), 257), (title.as_ptr(), u64::MAX), (bad_utf8.as_ptr(), 1),
    ];
    for (pointer, length) in cases {
        let result = take(unsafe { fcb_saved_trail_write(saved, 1, 0, c"".as_ptr(), 0, 0, 1,
            pointer, length, std::ptr::null(), 0, target.as_ptr()) });
        assert!(result.contains("INVALID_ARGUMENT"));
    }
    for (generation, digest, start, end) in [(1, c"", 0, 1), (0, c"0123", 0, 1), (0, c"", 2, 1)] {
        let result = take(unsafe { fcb_saved_trail_write(saved, 1, generation, digest.as_ptr(), 0, start, end,
            std::ptr::null(), 0, std::ptr::null(), 0, target.as_ptr()) });
        assert!(result.contains("INVALID_ARGUMENT"));
    }
    let invalid_note = take(unsafe { fcb_saved_trail_write(saved, 1, 0, c"".as_ptr(), 0, 0, 1,
        std::ptr::null(), 0, title.as_ptr(), 4097, target.as_ptr()) });
    assert!(invalid_note.contains("INVALID_ARGUMENT"));
    assert!(!destination.exists());
    let info = take(fcb_saved_info(saved)); assert_eq!(count(&info, "member_bytes_read"), 0);
    assert_eq!(count(&info, "last_trail_write_attempt"), 0);
    // Zero-length null text slices and an exact empty range are valid.
    let result = take(unsafe { fcb_saved_trail_write(saved, 1, 0, c"".as_ptr(), 0, 0, 0,
        std::ptr::null(), 0, std::ptr::null(), 0, target.as_ptr()) });
    assert!(result.contains("\"status\":\"ok\"")); assert_eq!(count(&result, "reference_bytes"), 0);
    assert_eq!(fcb_saved_close(saved), 1);
}

unsafe extern "C" fn cancel_when_created(context: *mut c_void) -> i32 {
    let path = unsafe { &*(context.cast::<PathBuf>()) };
    if path.exists() { 1 } else { 0 }
}

#[test]
fn actual_c_canceled_effect_and_existing_destination_have_recoverable_owned_receipts() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (root, archive) = fixture(); let saved = fcb_saved_create(); take(unsafe { fcb_saved_open(saved, archive.as_ptr()) });
    let mut destination = root.join("created.fcbt"); let target = path_text(&destination);
    let response = take(unsafe { fcb_saved_trail_write_cancelable(saved, 1, 0, c"".as_ptr(), 0, 0, 3,
        std::ptr::null(), 0, std::ptr::null(), 0, target.as_ptr(), Some(cancel_when_created),
        (&mut destination as *mut PathBuf).cast::<c_void>()) });
    assert!(response.contains("SAVED_TRAIL_WRITE_CANCELED"));
    assert!(response.contains("\"effect\":\"destination-created-incomplete\""));
    assert_eq!(count(&response, "bytes_written"), 0);
    assert_eq!(std::fs::metadata(&destination).unwrap().len(), 0);
    std::fs::write(&destination, b"existing user's bytes").unwrap();
    assert_eq!(take(fcb_saved_trail_write_receipt(saved, 1)), response);
    let refused = write(saved, 2, &destination);
    assert!(refused.contains("SAVED_TRAIL_DESTINATION_EXISTS")); assert!(refused.contains("\"effect\":\"none\""));
    assert!(refused.contains("\"complete_artifact\":false"));
    assert_eq!(std::fs::read(&destination).unwrap(), b"existing user's bytes");
    assert_eq!(take(fcb_saved_trail_write_receipt(saved, 2)), refused);
    assert!(write(saved, 2, &root.join("stale.fcbt")).contains("SAVED_TRAIL_STALE_WRITE"));
    assert!(!root.join("stale.fcbt").exists()); assert_eq!(fcb_saved_close(saved), 1);
}

#[test]
fn actual_c_edits_all_four_metadata_actions_without_reopening_source_or_parent_paths() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, u32, u64, u64,
        *const u8, u64, *const c_char) -> *mut c_char = fcb_saved_trail_edit;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, u32, u64, u64,
        *const u8, u64, *const c_char, Option<crate::SearchCancellationCallback>,
        *mut c_void) -> *mut c_char = fcb_saved_trail_edit_cancelable;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["trail_edit", "trail_edit_cancelable"] {
        assert_eq!(header.matches(&format!("fcb_saved_{name}(")).count(), 1);
    }
    let (root, archive) = fixture(); let saved = fcb_saved_create();
    take(unsafe { fcb_saved_open(saved, archive.as_ptr()) });
    let parent = root.join("parent.fcbt"); let created = write(saved, 41, &parent);
    let before = std::fs::read(&parent).unwrap(); let view = TrailView::open(&before, || false).unwrap();
    let digest = CString::new(value(&created, "trail_digest")).unwrap();
    take(unsafe { fcb_saved_trail_attach(saved, 7, path_text(&parent).as_ptr()) });
    std::fs::rename(&parent, root.join("held-parent.fcbt")).unwrap();
    std::fs::rename(root.join("saved.fcbs"), root.join("held-archive.fcbs")).unwrap();
    let reads = count(&take(fcb_saved_info(saved)), "member_bytes_read");
    let text = b"Revised\0rationale\n";
    for (kind, operation) in [(0, 42), (1, 43), (2, 44), (3, 45)] {
        let destination = root.join(format!("edit-{kind}.fcbt"));
        let length = if kind < 2 { text.len() as u64 } else { 0 };
        let response = take(unsafe { fcb_saved_trail_edit(saved, operation, 7, digest.as_ptr(),
            kind, 0, 0, text.as_ptr(), length, path_text(&destination).as_ptr()) });
        assert!(response.contains("\"command\":\"trail-edit\""));
        assert!(response.contains("\"status\":\"ok\"")); assert!(response.contains("\"source_verified\":false"));
        assert!(response.contains("\"source_anchors_unchanged\":true"));
        assert_eq!(count(&response, "member_bytes_read"), 0); assert_eq!(count(&response, "member_read_calls"), 0);
        assert_eq!(count(&response, "parent_generation"), 7); assert_eq!(value(&response, "parent_digest"), view.digest().to_hex());
        let bytes = std::fs::read(destination).unwrap(); let result = TrailView::open(&bytes, || false).unwrap();
        assert_eq!(result.parent_digest(), Some(view.digest()));
        match kind {
            0 => { assert_eq!(result.title(), "Revised\0rationale\n"); assert_eq!(result.entry(0).unwrap(), view.entry(0).unwrap()); }
            1 => {
                assert_eq!(result.title(), view.title()); assert_eq!(result.entry(0).unwrap().rationale, "Revised\0rationale\n");
                assert_eq!(result.entry(0).unwrap().path, b"raw-\xff.rs");
                assert_eq!(result.entry(0).unwrap().archive, view.entry(0).unwrap().archive);
                assert_eq!(result.entry(0).unwrap().source, view.entry(0).unwrap().source);
                assert_eq!(result.entry(0).unwrap().range, view.entry(0).unwrap().range);
            }
            2 => assert_eq!(result.entry(0).unwrap(), view.entry(0).unwrap()),
            _ => { assert!(result.is_empty()); assert_eq!(result.reference_bytes(), 0); }
        }
        assert_eq!(take(fcb_saved_trail_write_receipt(saved, operation)), response);
        assert_eq!(count(&take(fcb_saved_trail_page(saved, 7, 0, 32)), "total_items"), 1);
        let info = take(fcb_saved_info(saved));
        assert_eq!(count(&info, "member_bytes_read"), reads);
        assert_eq!(count(&info, "last_trail_write_attempt"), operation);
    }
    assert_eq!(std::fs::read(root.join("held-parent.fcbt")).unwrap(), before);
    assert_eq!(fcb_saved_close(saved), 1);
}

#[test]
fn actual_c_edit_rejects_bad_text_parent_and_reserved_fields_before_host_entry() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (root, archive) = fixture(); let saved = fcb_saved_create();
    take(unsafe { fcb_saved_open(saved, archive.as_ptr()) });
    let parent = root.join("parent.fcbt"); let created = write(saved, 1, &parent);
    take(unsafe { fcb_saved_trail_attach(saved, 7, path_text(&parent).as_ptr()) });
    let digest = CString::new(value(&created, "trail_digest")).unwrap();
    let destination = root.join("not-created.fcbt"); let target = path_text(&destination);
    let text = b"T"; let invalid_utf8 = [255];
    for (kind, pointer, length) in [
        (0, std::ptr::null(), 1), (0, text.as_ptr(), 257), (0, text.as_ptr(), u64::MAX),
        (0, invalid_utf8.as_ptr(), 1), (1, text.as_ptr(), 4097), (1, invalid_utf8.as_ptr(), 1),
    ] {
        let result = take(unsafe { fcb_saved_trail_edit(saved, 2, 7, digest.as_ptr(), kind,
            0, 0, pointer, length, target.as_ptr()) });
        assert!(result.contains("INVALID_ARGUMENT"));
    }
    for (kind, item, position, length) in [
        (0, 1, 0, 0), (0, 0, 1, 0), (1, 0, 1, 0),
        (2, 0, 0, 1), (3, 0, 1, 0), (3, 0, 0, 1), (4, 0, 0, 0),
    ] {
        let result = take(unsafe { fcb_saved_trail_edit(saved, 2, 7, digest.as_ptr(), kind,
            item, position, text.as_ptr(), length, target.as_ptr()) });
        assert!(result.contains("INVALID_ARGUMENT"));
    }
    for (generation, pin) in [(0, digest.as_ptr()), (7, c"".as_ptr()), (7, std::ptr::null())] {
        let result = take(unsafe { fcb_saved_trail_edit(saved, 2, generation, pin, 0,
            0, 0, text.as_ptr(), 1, target.as_ptr()) });
        assert!(result.contains("INVALID_ARGUMENT"));
    }
    let info = take(fcb_saved_info(saved));
    assert_eq!(count(&info, "last_trail_write_attempt"), 1);
    assert_eq!(count(&info, "member_bytes_read"), SOURCE.len() as u64);
    assert_eq!(take(fcb_saved_trail_write_receipt(saved, 1)), created);
    assert!(!destination.exists());
    // A canonical but absent visit reaches the host and consumes its operation,
    // while preserving the prior receipt and original accepted attachment.
    let invalid = take(unsafe { fcb_saved_trail_edit(saved, 2, 7, digest.as_ptr(), 3,
        1, 0, std::ptr::null(), 0, target.as_ptr()) });
    assert!(invalid.contains("TRAIL_ITEM_NOT_FOUND")); assert!(!destination.exists());
    assert_eq!(take(fcb_saved_trail_write_receipt(saved, 1)), created);
    assert_eq!(count(&take(fcb_saved_info(saved)), "last_trail_write_attempt"), 2);
    assert_eq!(fcb_saved_close(saved), 1);
}

#[test]
fn actual_c_edit_create_effect_survives_cancellation_receipt_recovery_and_close() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (root, archive) = fixture(); let saved = fcb_saved_create();
    take(unsafe { fcb_saved_open(saved, archive.as_ptr()) });
    let parent = root.join("parent.fcbt"); let created = write(saved, 1, &parent);
    let before = std::fs::read(&parent).unwrap();
    take(unsafe { fcb_saved_trail_attach(saved, 7, path_text(&parent).as_ptr()) });
    let digest = CString::new(value(&created, "trail_digest")).unwrap();
    let refused = take(unsafe { fcb_saved_trail_edit(saved, 2, 7, digest.as_ptr(), 0,
        0, 0, std::ptr::null(), 0, path_text(&parent).as_ptr()) });
    assert!(refused.contains("SAVED_TRAIL_DESTINATION_EXISTS")); assert!(refused.contains("\"effect\":\"none\""));
    assert_eq!(std::fs::read(&parent).unwrap(), before);
    let mut destination = root.join("canceled.fcbt"); let target = path_text(&destination);
    let response = take(unsafe { fcb_saved_trail_edit_cancelable(saved, 3, 7, digest.as_ptr(), 0,
        0, 0, std::ptr::null(), 0, target.as_ptr(), Some(cancel_when_created),
        (&mut destination as *mut PathBuf).cast::<c_void>()) });
    assert!(response.contains("SAVED_TRAIL_WRITE_CANCELED"));
    assert!(response.contains("\"effect\":\"destination-created-incomplete\""));
    assert_eq!(count(&response, "bytes_written"), 0);
    assert_eq!(std::fs::metadata(&destination).unwrap().len(), 0);
    assert_eq!(take(fcb_saved_trail_write_receipt(saved, 3)), response);
    let held_reply = fcb_saved_trail_write_receipt(saved, 3);
    assert_eq!(fcb_saved_close(saved), 1); assert_eq!(take(held_reply), response);
    assert_eq!(std::fs::read(&parent).unwrap(), before);
}

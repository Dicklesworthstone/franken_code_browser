#![deny(unsafe_op_in_unsafe_fn)]

//! C ABI regression coverage; independent batch compilation/execution pending.
use super::*;
use super::super::{fcb_saved_create, fcb_saved_open, fcb_saved_close};
use super::super::trail::fcb_saved_trail_attach;
use std::{ffi::{CStr, CString}, fs};
use fcb::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}, trail::{TrailBytes, TrailEntry}};
use fcb::store::Sha256;

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
#[test]
fn supplemental_export_c_abi_binds_the_path_inventory_and_rejects_invalid_arrays() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, *const c_char, u64, u64, u64,
        u64, u64, *const *const c_char, u64, u64, Option<crate::SearchCancellationCallback>, *mut c_void)
        -> *mut c_char = fcb_saved_trail_export_with_archives_cancelable;
    let header = include_str!("../include/fcb_saved_repository.h");
    assert_eq!(header.matches("fcb_saved_trail_export_with_archives_cancelable(").count(), 1);
    let root = std::env::temp_dir().join(format!("fcb-mixed-trail-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(9022).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let first_source = b"primary";
    let second_source = b"\xffsupplemental\0";
    let first = SnapshotBytes::encode(owner, true, "mixed-c", &[SnapshotEntry {
        path: b"same.rs", observed_bytes: first_source.len() as u64, data: SnapshotData::Captured(first_source) }],
        Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let second = SnapshotBytes::encode(owner, true, "mixed-c", &[SnapshotEntry {
        path: b"same.rs", observed_bytes: second_source.len() as u64, data: SnapshotData::Captured(second_source) }],
        Default::default(), &budget, ResourceAllocationId::new(2).unwrap(), || false).unwrap();
    let first_path = root.join("primary.fcbs"); fs::write(&first_path, first.bytes()).unwrap();
    let second_path = root.join("supplemental.fcbs"); fs::write(&second_path, second.bytes()).unwrap();
    let item = TrailEntry { archive: first.digest(), source: Sha256::digest(first_source),
        source_length: first_source.len() as u64, path: b"same.rs",
        range: ByteRange::new(ByteOffset::new(0), ByteOffset::new(first_source.len() as u64)).unwrap(),
        rationale: "Primary source" };
    let trail = TrailBytes::encode(owner, "Explicit mixed archive", None, &[item,
        TrailEntry { archive: second.digest(), source: Sha256::digest(second_source),
            source_length: second_source.len() as u64,
            range: ByteRange::new(ByteOffset::new(0), ByteOffset::new(second_source.len() as u64)).unwrap(),
            rationale: "Supplemental source", ..item }], &budget, ResourceAllocationId::new(3).unwrap(), || false).unwrap();
    let trail_path = root.join("tour.fcbt"); fs::write(&trail_path, trail.bytes()).unwrap();
    let first_c = CString::new(first_path.to_str().unwrap()).unwrap();
    let second_c = CString::new(second_path.to_str().unwrap()).unwrap();
    let trail_c = CString::new(trail_path.to_str().unwrap()).unwrap();
    let digest_c = CString::new(trail.digest().to_hex()).unwrap();
    let destination = root.join("pack.json"); let destination_c = CString::new(destination.to_str().unwrap()).unwrap();
    let saved = fcb_saved_create(); assert_ne!(saved, 0);
    assert!(take(unsafe { fcb_saved_open(saved, first_c.as_ptr()) }).contains("\"status\":\"ok\""));
    assert!(take(unsafe { fcb_saved_trail_attach(saved, 1, trail_c.as_ptr()) }).contains("\"status\":\"ok\""));
    let paths = [second_c.as_ptr()];
    let relative = CString::new("not-an-absolute-grant.fcbs").unwrap(); let relative_paths = [relative.as_ptr()];
    let null_paths = [std::ptr::null()];
    for (array, count) in [(std::ptr::null(), 1), (std::ptr::null(), 8),
        (relative_paths.as_ptr(), 1), (null_paths.as_ptr(), 1)] {
        let refused = take(unsafe { fcb_saved_trail_export_with_archives_cancelable(saved, 1, 1,
            digest_c.as_ptr(), destination_c.as_ptr(), 1024, 64 * 1024 * 1024, 1024 * 1024,
            16 * 1024 * 1024, 16 * 1024 * 1024, array, count, 256 * 1024 * 1024,
            None, std::ptr::null_mut()) });
        assert!(refused.contains("\"status\":\"error\""));
        assert!(!destination.exists());
    }
    let response = take(unsafe { fcb_saved_trail_export_with_archives_cancelable(saved, 1, 1,
        digest_c.as_ptr(), destination_c.as_ptr(), 1024, 64 * 1024 * 1024, 1024 * 1024,
        16 * 1024 * 1024, 16 * 1024 * 1024, paths.as_ptr(), 1, 256 * 1024 * 1024,
        None, std::ptr::null_mut()) });
    assert!(response.contains("\"status\":\"ok\""));
    assert!(response.contains("\"coverage_complete\":true"));
    assert!(response.contains("\"validation_complete\":true"));
    assert_eq!(number(&response, "distinct_archives"), 2);
    assert_eq!(number(&response, "opened_archive_paths"), 1);
    assert_eq!(number(&response, "member_bytes_read"), (first_source.len() + second_source.len()) as u64);
    let pack = fs::read_to_string(&destination).unwrap();
    assert!(pack.contains("\"selected_original_hex\":\"7072696d617279\""));
    assert!(pack.contains("\"selected_original_hex\":\"ff737570706c656d656e74616c00\""));
    fs::rename(&second_path, root.join("moved-supplemental.fcbs")).unwrap();
    fs::write(&destination, b"external change after worker drain").unwrap();
    assert_eq!(take(fcb_saved_trail_export_receipt(saved, 1)), response);
    assert_eq!(fs::read(&destination).unwrap(), b"external change after worker drain");
    assert!(take(fcb_saved_close(saved)).contains("\"status\":\"ok\""));
}

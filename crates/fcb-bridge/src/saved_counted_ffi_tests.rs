#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::ffi::{CStr, CString};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{IndexLimits, ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}, paged_snapshot::PagedSnapshot,
    snapshot_index::SnapshotIndex};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn field(text: &str, name: &str) -> u64 {
    text.split(&format!("\"{name}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn counted(text: &str, total: u64, retained: u64) {
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    assert!(text.contains("\"count_all_matches\":true") && text.contains("\"matches_seen_exact\":true"), "{text}");
    assert_eq!(field(text, "matches_seen"), total); assert_eq!(field(text, "retained_hits"), retained);
    assert_eq!(field(text, "matching_files"), 2);
    assert_eq!(field(text, "retained_matching_files"), u64::from(retained > 0));
    assert!(text.contains("\"truncated\":true"));
}

#[test]
fn counted_saved_abi_declarations_and_real_indexed_reader_handoffs_agree() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64) -> *mut c_char = fcb_saved_search_counted;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64) -> *mut c_char = fcb_saved_expression_search_counted;
    let header = include_str!("../include/fcb_saved_repository.h");
    assert_eq!(header.matches("fcb_saved_search_counted(").count(), 1);
    assert_eq!(header.matches("fcb_saved_expression_search_counted(").count(), 1);
    let root = std::env::temp_dir().join(format!("fcb-counted-saved-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("saved.fcbs"); let index_path = root.join("saved.fcbd");
    let owner = ArenaOwnerId::new(88751).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(128 * 1024 * 1024)).unwrap();
    let id = |n| ResourceAllocationId::new(n).unwrap();
    let le: Vec<_> = "\u{feff}banana banana required".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let second = b"banana required";
    let encoded = SnapshotBytes::encode(owner, true, "counted-c", &[
        SnapshotEntry { path: b"a.rs", observed_bytes: le.len() as u64, data: SnapshotData::Captured(&le) },
        SnapshotEntry { path: b"b.rs", observed_bytes: second.len() as u64, data: SnapshotData::Captured(second) },
    ], Default::default(), &budget, id(1), || false).unwrap();
    std::fs::write(&path, encoded.bytes()).unwrap();
    let mut archive = PagedSnapshot::open(std::fs::File::open(&path).unwrap(), owner, Default::default(),
        &budget, id(2), || false).unwrap();
    let index = SnapshotIndex::build(&mut archive, IndexLimits::default(), &budget,
        [id(3), id(4), id(5), id(6)], || false).unwrap();
    let inverse = index.invert(&budget, id(7), || false).unwrap();
    let artifact = inverse.encode_paged(&budget, id(8), || false).unwrap();
    std::fs::write(&index_path, artifact.bytes()).unwrap();
    let path = CString::new(path.to_str().unwrap()).unwrap();
    let index_path = CString::new(index_path.to_str().unwrap()).unwrap();
    let pin = CString::new(artifact.digest().to_hex()).unwrap();
    let needle = CString::new("ana").unwrap(); let expression = CString::new("ana required").unwrap();
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));
    counted(&take(unsafe { fcb_saved_search_counted(handle, 1, needle.as_ptr(), 1) }), 6, 1);
    assert!(take(unsafe { fcb_saved_attach_index(handle, 1, index_path.as_ptr(), pin.as_ptr()) }).contains("\"status\":\"ok\""));
    let indexed = take(unsafe { fcb_saved_search_counted(handle, 2, needle.as_ptr(), 1) });
    counted(&indexed, 6, 1); assert!(indexed.contains("\"search_complete\":true"));
    assert_eq!(field(&indexed, "fallback_files"), 1);
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_saved_open_hit_reader(handle, reader, 2, 1));
    assert!(opened.contains("\"status\":\"ok\""));
    assert!(opened.contains("\"live_source_reopened\":false"));
    counted(&take(fcb_saved_results(handle, 2, 0, 1)), 6, 1);
    counted(&take(unsafe { fcb_saved_search_counted(handle, 3, needle.as_ptr(), 0) }), 6, 0);
    counted(&take(unsafe { fcb_saved_expression_search_counted(handle, 1, expression.as_ptr(), 1, 100_000) }), 6, 1);
    let expression_reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(expression_reader, 0);
    let opened = take(fcb_saved_open_expression_hit_reader(handle, expression_reader, 1, 1));
    assert!(opened.contains("\"selection_namespace\":\"saved-expression\""));
    assert!(opened.contains("\"original_hex\":\"61006e006100\""));
    counted(&take(unsafe { fcb_saved_expression_search_counted(handle, 2, expression.as_ptr(), 0, 100_000) }), 6, 0);
    let invalid = take(unsafe { fcb_saved_search_counted(handle, 4, needle.as_ptr(), 4097) });
    assert!(invalid.contains("\"status\":\"error\""));
    counted(&take(fcb_saved_results(handle, 3, 0, 64)), 6, 0);
    assert_eq!(fcb_saved_close(handle), 1);
    for reader in [reader, expression_reader] {
        assert!(take(crate::reader_ffi::fcb_reader_copy_range(reader, 4, 10))
            .contains("\"original_hex\":\"61006e006100\""));
        assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
    }
}

#[test]
fn counted_saved_errors_keep_foreign_inputs_private_and_responses_independent() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let needle = CString::new("private counted query").unwrap();
    let first = unsafe { fcb_saved_search_counted(u64::MAX, 1, needle.as_ptr(), 0) };
    let second = unsafe { fcb_saved_expression_search_counted(u64::MAX, 1, needle.as_ptr(), 0, 4096) };
    for pointer in [first, second, unsafe { fcb_saved_search_counted(u64::MAX, 2, std::ptr::null(), 1) }] {
        let error = take(pointer);
        assert!(error.len() < 4096 && error.contains("\"status\":\"error\""));
        assert!(!error.contains("private counted query"));
    }
}

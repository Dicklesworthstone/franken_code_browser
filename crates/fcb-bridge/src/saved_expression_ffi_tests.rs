#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::ffi::{CStr, CString};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn archive(source: &[u8]) -> CString {
    let root = std::env::temp_dir().join(format!("fcb-expression-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("saved.fcbs"); let owner = ArenaOwnerId::new(8992).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let bytes = SnapshotBytes::encode(owner, true, "expression-c", &[SnapshotEntry {
        path: b"src/source.rs", observed_bytes: source.len() as u64, data: SnapshotData::Captured(source)
    }], Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    std::fs::write(&path, bytes.bytes()).unwrap(); CString::new(path.to_str().unwrap()).unwrap()
}

#[test]
fn actual_expression_c_entrypoints_preserve_utf16_proof_namespace_and_reader_ownership() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let bytes: Vec<_> = "\u{feff}needle\r\nrequired\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let path = archive(&bytes); let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));
    let query = CString::new("needle required path:src/ lang:rust").unwrap();
    let searched = take(unsafe { fcb_saved_expression_search(handle, 1, query.as_ptr(), 10, 4096) });
    assert!(searched.contains("\"retained_hits\":\"1\""), "{searched}");
    let mut old_foreign_query = query.into_bytes_with_nul(); old_foreign_query.fill(b'x');
    let retained = fcb_saved_expression_results(handle, 1, 0, 10);
    let literal = CString::new("required").unwrap();
    assert!(take(unsafe { fcb_saved_search(handle, 1, literal.as_ptr(), 10) }).contains("\"retained_hits\":\"1\""));
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_saved_open_expression_hit_reader(handle, reader, 1, 1));
    assert!(opened.contains("\"command\":\"expression-open-reader\""), "{opened}");
    assert!(opened.contains("\"selection_namespace\":\"saved-expression\""));
    assert!(opened.contains("\"expression_generation\":\"1\""));
    assert!(opened.contains("\"primary_needle\":\"needle\""));
    assert!(opened.contains("\"original_range\":{\"start\":\"2\",\"end\":\"14\"}"));
    assert!(opened.contains("\"original_hex\":\"6e006500650064006c006500\""));
    assert!(opened.contains("\"live_source_reopened\":false"));
    assert!(take(fcb_saved_clear_expression(handle, 2)).contains("\"accepted_expression_generation\":null"));
    assert!(take(fcb_saved_results(handle, 1, 0, 10)).contains("\"needle\":\"required\""));
    assert_eq!(fcb_saved_close(handle), 1);
    let page = take(retained);
    assert!(page.contains("needle required path:src/ lang:rust"));
    let copy = take(crate::reader_ffi::fcb_reader_copy_range(reader, 2, 14));
    assert!(copy.contains("\"original_hex\":\"6e006500650064006c006500\""));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
}

#[test]
fn expression_c_errors_are_bounded_and_failed_replacement_is_recoverable() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let query = CString::new("needle required").unwrap();
    for pointer in [
        unsafe { fcb_saved_expression_search(u64::MAX, 1, query.as_ptr(), 10, 4096) },
        unsafe { fcb_saved_expression_search(u64::MAX, 1, std::ptr::null(), 10, 4096) },
        fcb_saved_expression_results(u64::MAX, 1, 0, 10),
        fcb_saved_clear_expression(u64::MAX, 1),
        fcb_saved_open_expression_hit_reader(u64::MAX, 0, 1, 1),
    ] {
        let error = take(pointer);
        assert!(error.len() < 4096 && error.contains("\"status\":\"error\""));
        assert!(!error.contains("needle required"));
    }
    let path = archive(b"needle\nrequired"); let handle = fcb_saved_create(); assert_ne!(handle, 0);
    take(unsafe { fcb_saved_open(handle, path.as_ptr()) });
    assert!(take(unsafe { fcb_saved_expression_search(handle, 1, query.as_ptr(), 10, 4096) })
        .contains("\"retained_hits\":\"1\""));
    let invalid = CString::new("needle regex:a.*").unwrap();
    let failure = take(unsafe { fcb_saved_expression_search(handle, 2, invalid.as_ptr(), 10, 4096) });
    assert!(failure.contains("\"status\":\"error\""));
    let info = take(fcb_saved_info(handle));
    assert!(info.contains("\"accepted_expression_generation\":\"1\""));
    assert!(info.contains("\"last_expression_attempt\":\"2\""));
    assert!(take(fcb_saved_expression_results(handle, 1, 0, 10)).contains("\"retained_hits\":\"1\""));
    assert!(take(fcb_saved_expression_results(handle, 1, 0, 129)).contains("\"status\":\"error\""));
    assert_eq!(fcb_saved_close(handle), 1);
}

#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::{fcb_saved_create, fcb_saved_open, fcb_saved_info, fcb_saved_close};
use std::{ffi::{CStr, CString}, path::PathBuf};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}};

const SOURCE: &[u8] = b"# saved\nfrom .b import Value\nfrom . import missing\nimport external_pkg\nfrom .unavailable import Missing\n";
const TARGET: &[u8] = b"Value = 'only in the saved capture'\n";

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected an owned response");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn fixture() -> (PathBuf, CString) {
    let root = std::env::temp_dir().join(format!("fcb-saved-import-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    std::fs::write(root.join("pkg/b.py"), b"different live contents").unwrap();
    let owner = ArenaOwnerId::new(9012).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let entries = [
        SnapshotEntry { path: b"pkg/a.py", observed_bytes: SOURCE.len() as u64, data: SnapshotData::Captured(SOURCE) },
        SnapshotEntry { path: b"pkg/b.py", observed_bytes: TARGET.len() as u64, data: SnapshotData::Captured(TARGET) },
        SnapshotEntry { path: b"pkg/unavailable.py", observed_bytes: 9, data: SnapshotData::Unavailable("READ_DENIED") },
    ];
    let bytes = SnapshotBytes::encode(owner, true, "saved-import-c", &entries, Default::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = root.join("saved.fcbs");
    std::fs::write(&path, bytes.bytes()).unwrap();
    (root, CString::new(path.to_str().unwrap()).unwrap())
}

#[test]
fn actual_c_imports_deliver_exact_evidence_and_independent_offline_target_readers() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_prepare_imports;
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_saved_imports;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_saved_clear_imports;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_saved_open_import_reader;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["prepare_imports", "imports", "clear_imports", "open_import_reader"] {
        assert_eq!(header.matches(&format!("fcb_saved_{name}(")).count(), 1);
    }
    let (root, path) = fixture();
    let saved = fcb_saved_create(); assert_ne!(saved, 0);
    assert!(take(unsafe { fcb_saved_open(saved, path.as_ptr()) }).contains("\"status\":\"ok\""));
    let prepared = take(fcb_saved_prepare_imports(saved, 1, 64, 4096));
    assert_eq!(number(&prepared, "retained_links"), 4);
    assert!(prepared.contains("\"evidence_precision\":\"exact-import-source-range\""));
    assert!(prepared.contains("\"resolution\":\"unavailable\""));
    assert!(prepared.contains("\"imports_complete\":false"));
    let retained_page = fcb_saved_imports(saved, 1, 2, 1, 0, 64);
    std::fs::rename(root.join("saved.fcbs"), root.join("moved.fcbs")).unwrap();
    let evidence_reader = crate::reader_ffi::fcb_reader_create();
    let target_reader = crate::reader_ffi::fcb_reader_create();
    assert_ne!(evidence_reader, 0); assert_ne!(target_reader, 0);
    let evidence = take(fcb_saved_open_import_reader(saved, evidence_reader, 1, 1, 0));
    assert!(evidence.contains("\"command\":\"imports-open-reader\""));
    assert!(evidence.contains("\"live_source_reopened\":false"));
    assert_eq!(number(&evidence, "offset"), 8);
    assert!(evidence.contains("from .b import Value"));
    let target = take(fcb_saved_open_import_reader(saved, target_reader, 1, 1, 1));
    assert_eq!(number(&target, "offset"), 0);
    assert!(target.contains("only in the saved capture"));
    assert!(!target.contains("different live contents"));
    assert!(take(fcb_saved_clear_imports(saved, 2)).contains("\"accepted_import_generation\":null"));
    assert_eq!(fcb_saved_close(saved), 1);
    let incoming = take(retained_page);
    assert_eq!(number(&incoming, "matching_links"), 1);
    assert!(incoming.contains("\"target_member\":\"1\""));
    assert!(take(crate::reader_ffi::fcb_reader_window(evidence_reader, 0, 256)).contains("from .b import Value"));
    assert!(take(crate::reader_ffi::fcb_reader_window(target_reader, 0, 256)).contains("only in the saved capture"));
    assert_eq!(crate::reader_ffi::fcb_reader_close(evidence_reader), 1);
    assert_eq!(crate::reader_ffi::fcb_reader_close(target_reader), 1);
}

#[test]
fn c_import_validation_and_unresolved_targets_leave_empty_readers_and_source_counters_unchanged() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (_, path) = fixture(); let saved = fcb_saved_create();
    take(unsafe { fcb_saved_open(saved, path.as_ptr()) });
    take(fcb_saved_prepare_imports(saved, 1, 64, 4096));
    for (direction, member) in [(3, 0), (0, 1), (2, u64::MAX)] {
        assert!(take(fcb_saved_imports(saved, 1, direction, member, 0, 64)).contains("\"status\":\"error\""));
    }
    let before = take(fcb_saved_info(saved));
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    for (generation, link, side) in [(1, 1, 2), (1, 0, 0), (2, 1, 0), (1, 2, 1), (1, 3, 1), (1, 4, 1)] {
        assert!(take(fcb_saved_open_import_reader(saved, reader, generation, link, side)).contains("\"status\":\"error\""));
        assert!(take(crate::reader_ffi::fcb_reader_info(reader)).contains("READER_HANDLE_NOT_OPEN"));
    }
    let after = take(fcb_saved_info(saved));
    assert_eq!(number(&before, "member_bytes_read"), number(&after, "member_bytes_read"));
    assert_eq!(number(&before, "member_read_calls"), number(&after, "member_read_calls"));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
    assert_eq!(fcb_saved_close(saved), 1);
    let unknown = take(fcb_saved_prepare_imports(u64::MAX, 1, 64, 4096));
    assert!(unknown.len() < 4096 && unknown.contains("\"status\":\"error\""));
}

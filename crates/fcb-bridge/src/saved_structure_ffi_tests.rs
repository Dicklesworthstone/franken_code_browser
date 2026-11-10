#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::{fcb_saved_create, fcb_saved_open, fcb_saved_info, fcb_saved_search,
    fcb_saved_results, fcb_saved_close};
use std::ffi::{CStr, CString};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected an owned response");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn archive(files: &[(&[u8], &[u8])]) -> CString {
    let root = std::env::temp_dir().join(format!("fcb-structure-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("saved.fcbs"); let owner = ArenaOwnerId::new(8994).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let entries: Vec<_> = files.iter().map(|(path, source)| SnapshotEntry {
        path, observed_bytes: source.len() as u64, data: SnapshotData::Captured(source),
    }).collect();
    let bytes = SnapshotBytes::encode(owner, true, "structure-c", &entries, Default::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    std::fs::write(&path, bytes.bytes()).unwrap(); CString::new(path.to_str().unwrap()).unwrap()
}
fn search(handle: u64, generation: u64, mode: u64, name_mode: u64, needle: &str) -> String {
    let needle = CString::new(needle).unwrap();
    take(unsafe { fcb_saved_structure_search(handle, generation, mode, name_mode,
        needle.as_ptr(), 100, 1024 * 1024) })
}

#[test]
fn saved_structure_c_signatures_match_the_public_header() {
    let _: unsafe extern "C" fn(u64, u64, u64, u64, *const c_char, u64, u64) -> *mut c_char =
        fcb_saved_structure_search;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_structure_results;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_saved_clear_structure;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_open_structure_hit_reader;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["fcb_saved_structure_search(", "fcb_saved_structure_results(",
        "fcb_saved_clear_structure(", "fcb_saved_open_structure_hit_reader("] {
        assert_eq!(header.matches(name).count(), 1, "missing or duplicate declaration for {name}");
    }
}

#[test]
fn c_occurrences_retain_utf16_proof_and_reader_bytes_after_saved_retirement() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let bytes: Vec<_> = "\u{feff}needle needle_suffix xneedle\r\n// needle\r\n"
        .encode_utf16().flat_map(u16::to_le_bytes).collect();
    let path = archive(&[(b"src/source.rs", &bytes)]);
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));
    let needle = CString::new("needle").unwrap();
    let searched = take(unsafe { fcb_saved_structure_search(handle, 1, 4, 0,
        needle.as_ptr(), 10, 4096) });
    assert!(searched.contains("\"retained_hits\":\"2\""), "{searched}");
    assert!(searched.contains("\"requested_mode\":\"occurrences\""));
    assert!(searched.contains("\"evidence_level\":\"whole-token-text-candidate\""));
    assert!(searched.contains("\"semantic_complete\":false"));
    let mut expired_foreign_input = needle.into_bytes_with_nul(); expired_foreign_input.fill(b'x');
    let retained = fcb_saved_structure_results(handle, 1, 0, 10);
    let literal = CString::new("needle_suffix").unwrap();
    assert!(take(unsafe { fcb_saved_search(handle, 1, literal.as_ptr(), 10) })
        .contains("\"retained_hits\":\"1\""));
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_saved_open_structure_hit_reader(handle, reader, 1, 1));
    assert!(opened.contains("\"command\":\"structure-open-reader\""), "{opened}");
    assert!(opened.contains("\"selection_namespace\":\"saved-structure\""));
    assert!(opened.contains("\"structure_generation\":\"1\""));
    assert!(opened.contains("\"original_range\":{\"start\":\"2\",\"end\":\"14\"}"));
    assert!(opened.contains("\"original_hex\":\"6e006500650064006c006500\""));
    assert!(opened.contains("\"live_source_reopened\":false"));
    assert!(take(fcb_saved_clear_structure(handle, 2)).contains("\"accepted_structure_generation\":null"));
    assert!(take(fcb_saved_results(handle, 1, 0, 10)).contains("\"needle\":\"needle_suffix\""));
    assert_eq!(fcb_saved_close(handle), 1);
    let page = take(retained);
    assert!(page.contains("\"needle\":\"needle\""));
    assert!(page.contains("\"retained_hits\":\"2\""));
    let copied = take(crate::reader_ffi::fcb_reader_copy_range(reader, 2, 14));
    assert!(copied.contains("\"original_hex\":\"6e006500650064006c006500\""));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
}

#[test]
fn c_modes_and_name_matching_preserve_live_search_numeric_contract() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let path = archive(&[
        (b"docs/guide.md", b"# Target\n\n## Target Extra\n"),
        (b"src/source.rs", b"fn target() {}\nfn target_extra() {}\n"),
    ]);
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));
    for (generation, mode, name_mode, needle, count, requested, matching) in [
        (1, 1, 0, "target", 1, "symbols", "exact"),
        (2, 1, 1, "target", 2, "symbols", "prefix"),
        (3, 1, 2, "arg", 2, "symbols", "contains"),
        (4, 2, 0, "Target", 1, "headings", "exact"),
        (5, 2, 1, "Target", 2, "headings", "prefix"),
        (6, 2, 2, "arg", 2, "headings", "contains"),
    ] {
        let page = search(handle, generation, mode, name_mode, needle);
        assert!(page.contains(&format!("\"retained_hits\":\"{count}\"")), "{page}");
        assert!(page.contains(&format!("\"requested_mode\":\"{requested}\"")), "{page}");
        assert!(page.contains(&format!("\"name_mode\":\"{matching}\"")), "{page}");
    }
    assert_eq!(fcb_saved_close(handle), 1);
}

#[test]
fn c_malformed_inputs_are_redacted_and_failed_replacement_can_be_reconciled() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let needle = CString::new("private_name").unwrap();
    for pointer in [
        unsafe { fcb_saved_structure_search(u64::MAX, 1, 4, 0, needle.as_ptr(), 10, 4096) },
        fcb_saved_structure_results(u64::MAX, 1, 0, 10),
        fcb_saved_clear_structure(u64::MAX, 1),
        fcb_saved_open_structure_hit_reader(u64::MAX, 0, 1, 1),
    ] {
        let error = take(pointer);
        assert!(error.len() < 4096 && error.contains("\"status\":\"error\""));
        assert!(!error.contains("private_name"));
    }
    let path = archive(&[(b"src/source.rs", b"fn private_name() {}\n")]);
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));
    assert!(search(handle, 1, 4, 0, "private_name").contains("\"retained_hits\":\"1\""));
    let invalid_utf8 = [255u8, 0];
    for pointer in [std::ptr::null(), invalid_utf8.as_ptr().cast()] {
        let error = take(unsafe { fcb_saved_structure_search(handle, 2, 4, 0, pointer, 10, 4096) });
        assert!(error.contains("SAVED_HANDLE_INVALID_ARGUMENT"), "{error}");
    }
    for (mode, name_mode) in [(0, 0), (3, 0), (4, 3)] {
        let error = take(unsafe { fcb_saved_structure_search(handle, 2, mode, name_mode,
            needle.as_ptr(), 10, 4096) });
        assert!(error.contains("SAVED_HANDLE_INVALID_ARGUMENT"), "{error}");
        assert!(!error.contains("private_name"));
    }
    let failure = search(handle, 2, 4, 0, "private_name::other");
    assert!(failure.contains("\"status\":\"error\""), "{failure}");
    assert!(!failure.contains("private_name::other"));
    let info = take(fcb_saved_info(handle));
    assert!(info.contains("\"accepted_structure_generation\":\"1\""));
    assert!(info.contains("\"last_structure_attempt\":\"2\""));
    assert!(take(fcb_saved_structure_results(handle, 1, 0, 10)).contains("\"retained_hits\":\"1\""));
    assert!(take(fcb_saved_structure_results(handle, 1, 0, 129)).contains("\"status\":\"error\""));
    assert!(search(handle, 3, 4, 1, "private_name").contains("\"status\":\"error\""));
    assert!(search(handle, 4, 4, 2, "private_name").contains("\"status\":\"error\""));
    assert!(search(handle, 5, 4, 0, "private_name").contains("\"retained_hits\":\"1\""));
    assert_eq!(fcb_saved_close(handle), 1);
}

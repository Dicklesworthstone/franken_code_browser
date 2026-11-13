#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::{fcb_saved_create, fcb_saved_open, fcb_saved_info, fcb_saved_close};
use std::ffi::{CStr, CString};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected an owned response");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn fixture() -> CString {
    let root = std::env::temp_dir().join(format!("fcb-saved-path-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(9014).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let source = b"fn saved_alpha() {}\n";
    let secondary = b"fn secondary() {}\n";
    let raw = b"captured under a raw native name\n";
    let entries = [
        SnapshotEntry { path: b"pkg/alpha.rs", observed_bytes: source.len() as u64, data: SnapshotData::Captured(source) },
        SnapshotEntry { path: b"pkg/alpha_copy.rs", observed_bytes: secondary.len() as u64, data: SnapshotData::Captured(secondary) },
        SnapshotEntry { path: b"pkg/alpha_missing.rs", observed_bytes: 8, data: SnapshotData::Unavailable("READ_DENIED") },
        SnapshotEntry { path: b"raw/\xff.rs", observed_bytes: raw.len() as u64, data: SnapshotData::Captured(raw) },
    ];
    // Historical discovery is incomplete, but every retained metadata record
    // must still participate in this directory's exact match count and top-k.
    let bytes = SnapshotBytes::encode(owner, false, "saved-path-c", &entries, Default::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = root.join("saved.fcbs"); std::fs::write(&path, bytes.bytes()).unwrap();
    CString::new(path.to_str().unwrap()).unwrap()
}
fn search(saved: u64, generation: u64, needle: &[u8], mode: u64, limit: u64, work: u64) -> String {
    take(unsafe { fcb_saved_path_search(saved, generation, needle.as_ptr(), needle.len() as u64,
        mode, 1, limit, work) })
}

#[test]
fn actual_c_saved_quick_open_counts_all_metadata_and_preserves_raw_named_reader_ownership() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: unsafe extern "C" fn(u64, u64, *const u8, u64, u64, u64, u64, u64) -> *mut c_char = fcb_saved_path_search;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_path_results;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_saved_clear_paths;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_open_path_hit_reader;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["path_search", "path_results", "clear_paths", "open_path_hit_reader"] {
        assert_eq!(header.matches(&format!("fcb_saved_{name}(")).count(), 1);
    }
    let path = fixture(); let saved = fcb_saved_create(); assert_ne!(saved, 0);
    take(unsafe { fcb_saved_open(saved, path.as_ptr()) });
    let before = take(fcb_saved_info(saved));
    let found = search(saved, 1, b"alpha", 1, 2, 64 * 1024 * 1024);
    assert!(found.contains("\"discovery_complete\":false"));
    assert!(found.contains("\"search_complete\":true"));
    assert!(found.contains("\"top_k_complete\":true"));
    assert!(found.contains("\"truncated\":true"));
    assert_eq!(number(&found, "matches_seen"), 3);
    assert_eq!(number(&found, "retained_hits"), 2);
    let retained_page = fcb_saved_path_results(saved, 1, 0, 64);
    let after = take(fcb_saved_info(saved));
    assert_eq!(number(&before, "member_bytes_read"), number(&after, "member_bytes_read"));
    assert_eq!(number(&before, "member_read_calls"), number(&after, "member_read_calls"));
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_saved_open_path_hit_reader(saved, reader, 1, 1));
    assert!(opened.contains("saved_alpha"));
    assert!(opened.contains("\"selection\":null"));
    let unavailable = search(saved, 2, b"alpha_missing.rs", 0, 256, 64 * 1024 * 1024);
    assert_eq!(number(&unavailable, "matches_seen"), 1);
    assert!(unavailable.contains("\"captured\":false"));
    assert!(unavailable.contains("READ_DENIED"));
    let empty = crate::reader_ffi::fcb_reader_create(); assert_ne!(empty, 0);
    let before = take(fcb_saved_info(saved));
    assert!(take(fcb_saved_open_path_hit_reader(saved, empty, 2, 1)).contains("\"status\":\"error\""));
    assert!(take(crate::reader_ffi::fcb_reader_info(empty)).contains("READER_HANDLE_NOT_OPEN"));
    let after = take(fcb_saved_info(saved));
    assert_eq!(number(&before, "member_bytes_read"), number(&after, "member_bytes_read"));
    let raw = search(saved, 3, b"\xff.rs", 0, 256, 64 * 1024 * 1024);
    assert_eq!(number(&raw, "matches_seen"), 1);
    assert!(raw.contains("7261772fff2e7273"));
    assert!(take(fcb_saved_open_path_hit_reader(saved, empty, 3, 1)).contains("captured under a raw native name"));
    assert!(take(fcb_saved_clear_paths(saved, 4)).contains("\"accepted_path_generation\":null"));
    assert!(take(fcb_saved_path_results(saved, 3, 0, 64)).contains("\"status\":\"error\""));
    assert_eq!(fcb_saved_close(saved), 1);
    assert_eq!(number(&take(retained_page), "matches_seen"), 3);
    assert!(take(crate::reader_ffi::fcb_reader_window(reader, 0, 128)).contains("saved_alpha"));
    assert!(take(crate::reader_ffi::fcb_reader_window(empty, 0, 128)).contains("captured under a raw native name"));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
    assert_eq!(crate::reader_ffi::fcb_reader_close(empty), 1);
}

#[test]
fn c_path_bounds_stale_selection_and_zero_work_are_explicit_before_member_reads() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let path = fixture(); let saved = fcb_saved_create();
    take(unsafe { fcb_saved_open(saved, path.as_ptr()) });
    let before = take(fcb_saved_info(saved));
    assert!(search(saved, 1, b"alpha", 2, 256, 0).contains("\"work_limited\":true"));
    let needle = b"alpha";
    for (pointer, length, mode, case) in [
        (std::ptr::null(), 5, 0, 1), (needle.as_ptr(), 0, 0, 1),
        (needle.as_ptr(), 257, 0, 1), (needle.as_ptr(), 5, 3, 1),
        (needle.as_ptr(), 5, 0, 2),
    ] {
        assert!(take(unsafe { fcb_saved_path_search(saved, 2, pointer, length, mode, case, 256, 4096) })
            .contains("\"status\":\"error\""));
    }
    assert!(search(saved, 2, b"a\0", 2, 256, 4096).contains("\"status\":\"error\""));
    assert!(search(saved, 3, needle, 2, 257, 4096).contains("\"status\":\"error\""));
    assert!(search(saved, 4, needle, 2, 256, (1u64 << 32) + 1).contains("\"status\":\"error\""));
    assert!(take(fcb_saved_path_results(saved, 1, 0, 64)).contains("\"work_limited\":true"));
    search(saved, 5, b"alpha.rs", 0, 256, 64 * 1024 * 1024);
    let reader = crate::reader_ffi::fcb_reader_create();
    for (generation, hit) in [(1, 1), (5, 0), (5, 2)] {
        assert!(take(fcb_saved_open_path_hit_reader(saved, reader, generation, hit)).contains("\"status\":\"error\""));
        assert!(take(crate::reader_ffi::fcb_reader_info(reader)).contains("READER_HANDLE_NOT_OPEN"));
    }
    let after = take(fcb_saved_info(saved));
    assert_eq!(number(&before, "member_bytes_read"), number(&after, "member_bytes_read"));
    assert_eq!(number(&before, "member_read_calls"), number(&after, "member_read_calls"));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
    assert_eq!(fcb_saved_close(saved), 1);
}

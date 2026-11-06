#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::index::{fcb_atlas_index_prepare, fcb_atlas_search_indexed_begin_matching};
use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close};
use crate::reader_ffi::{fcb_reader_create, fcb_reader_find, fcb_reader_hit,
    fcb_reader_copy_hit, fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString}, fs, path::PathBuf};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    text
}

fn workspace(label: &str, bytes: &[u8]) -> (PathBuf, u64) {
    let root = std::env::temp_dir().join(format!("fcb-case-ffi-{label}-{}-{}",
        std::process::id(), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("source.rs"), bytes).unwrap();
    let path = CString::new(root.to_str().unwrap()).unwrap();
    let handle = fcb_atlas_create();
    assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_atlas_open(handle, path.as_ptr(), 100) })
        .contains("\"status\":\"ok\""));
    (root, handle)
}

fn begin(handle: u64, generation: u64, needle: &CString, case_sensitive: u64) -> String {
    take(unsafe { fcb_atlas_search_begin_matching(handle, generation, needle.as_ptr(),
        std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), case_sensitive,
        100, 100, 8192, 65536) })
}

#[test]
fn matching_abi_keeps_legacy_case_and_rejects_invalid_flags_without_replacement() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_root, atlas) = workspace("choices", b"NEEDLE needle NeeDLe");
    let query = CString::new("needle").unwrap();
    let admitted = begin(atlas, 1, &query, 0);
    assert!(admitted.contains("\"case_sensitive\":false"), "{admitted}");
    assert!(admitted.contains("\"source_bytes_read\":\"0\""));
    for invalid in [2, u64::MAX] {
        assert!(begin(atlas, 2, &query, invalid).contains("INVALID_ARGUMENT"));
    }
    let insensitive = take(fcb_atlas_search_step(atlas, 1));
    assert!(insensitive.contains("\"search_complete\":true"), "{insensitive}");
    assert!(insensitive.contains("\"retained_hits\":\"3\""));
    assert!(insensitive.contains("\"mode\":\"unicode-lowercase-literal\""));
    for spelling in ["NEEDLE", "needle", "NeeDLe"] {
        assert!(insensitive.contains(&format!("\"matched_text\":\"{spelling}\"")));
    }
    // A rejected flag did not consume generation 2. The legacy ABI preserves
    // exact spelling and must not inherit the preceding request's case mode.
    let exact = take(unsafe { fcb_atlas_search(atlas, 2, query.as_ptr(), 100, 100, 8192, 65536) });
    assert!(exact.contains("\"case_sensitive\":true"), "{exact}");
    assert!(exact.contains("\"retained_hits\":\"1\""));
    assert!(exact.contains("\"original_range\":{\"start\":\"7\",\"end\":\"13\"}"));
    assert_eq!(fcb_atlas_close(atlas), 1);
}

#[test]
fn indexed_matching_retains_original_spelling_and_reader_bytes_after_disk_replacement() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    // Each Kelvin sign is three UTF-8 bytes but compares as the one-byte "k".
    // The source witness exceeds the former 1024-byte retained-reader needle
    // limit while the user's query stays inside the workspace allowance.
    let original = "K".repeat(1024);
    let (root, atlas) = workspace("wide-witness", original.as_bytes());
    let query = CString::new("k".repeat(1024)).unwrap();
    assert!(take(fcb_atlas_index_prepare(atlas, 1, 100, 8192, 65536, 65536))
        .contains("\"capture_complete\":true"));
    fs::write(root.join("source.rs"), b"changed live file").unwrap();
    let admitted = take(unsafe { fcb_atlas_search_indexed_begin_matching(atlas, 2, 1,
        query.as_ptr(), std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 0, 100, 65536) });
    assert!(admitted.contains("\"case_sensitive\":false"), "{admitted}");
    assert!(admitted.contains("\"verification_source_bytes\":\"0\""));
    let found = take(fcb_atlas_search_step(atlas, 2));
    assert!(found.contains("\"search_complete\":true"), "{found}");
    assert!(found.contains("\"retained_hits\":\"1\""));
    assert!(found.contains("\"skipped_by_index\":\"0\""));
    assert!(found.contains("\"fallback_files\":\"1\""));
    assert!(found.contains(&format!("\"matched_text\":\"{original}\"")));
    let reader = fcb_reader_create();
    assert_ne!(reader, 0);
    let imported = take(fcb_atlas_search_open_reader(atlas, reader, 2, 1));
    assert!(imported.contains("\"source_reopened\":false"), "{imported}");
    let witness = CString::new(original).unwrap();
    let exact = take(unsafe { fcb_reader_find(reader, 1, witness.as_ptr(), 4096, 3072) });
    assert!(exact.contains("\"search_complete\":true"), "{exact}");
    assert!(exact.contains("\"retained_hits\":\"1\""));
    assert!(take(fcb_reader_hit(reader, 1, 0, 2048)).contains("\"status\":\"ok\""));
    let copied = take(fcb_reader_copy_hit(reader, 1, 0));
    assert!(copied.contains(&format!("\"original_hex\":\"{}\"", "e284aa".repeat(1024))));
    // Original case-sensitive gram lookup remains available on this same index.
    let exact = take(unsafe { fcb_atlas_search_indexed_begin_matching(atlas, 3, 1,
        query.as_ptr(), std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), 1, 100, 65536) });
    assert!(exact.contains("\"case_sensitive\":true"));
    let empty = take(fcb_atlas_search_step(atlas, 3));
    assert!(empty.contains("\"search_complete\":true"), "{empty}");
    assert!(empty.contains("\"retained_hits\":\"0\""));
    assert_eq!(fcb_reader_close(reader), 1);
    assert_eq!(fcb_atlas_close(atlas), 1);
}

#[test]
fn utf16_lowercase_expansion_opens_and_copies_the_contributing_original_scalar() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    for (label, bytes, original_hex) in [
        ("utf16le", [0xff, 0xfe, 0x30, 0x01], "3001"),
        ("utf16be", [0xfe, 0xff, 0x01, 0x30], "0130"),
    ] {
        let (_root, atlas) = workspace(label, &bytes);
        let query = CString::new("i\u{307}").unwrap();
        assert!(begin(atlas, 1, &query, 0).contains("\"status\":\"ok\""));
        let found = take(fcb_atlas_search_step(atlas, 1));
        assert!(found.contains("\"search_complete\":true"), "{found}");
        assert!(found.contains("\"matched_text\":\"İ\""));
        assert!(found.contains("\"original_range\":{\"start\":\"2\",\"end\":\"4\"}"));
        let reader = fcb_reader_create();
        assert_ne!(reader, 0);
        assert!(take(fcb_atlas_search_open_reader(atlas, reader, 1, 1))
            .contains("\"source_reopened\":false"));
        let original = CString::new("İ").unwrap();
        let exact = take(unsafe { fcb_reader_find(reader, 1, original.as_ptr(), 4096, 4) });
        assert!(exact.contains("\"retained_hits\":\"1\""), "{exact}");
        assert!(take(fcb_reader_copy_hit(reader, 1, 0))
            .contains(&format!("\"original_hex\":\"{original_hex}\"")));
        assert_eq!(fcb_reader_close(reader), 1);
        assert_eq!(fcb_atlas_close(atlas), 1);
    }
}

#[test]
fn matching_signatures_match_public_c_headers() {
    let _: unsafe extern "C" fn(u64, u64, *const c_char, *const c_char, *const c_char,
        u64, *const c_char, u64, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_search_begin_matching;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, *const c_char, *const c_char,
        u64, *const c_char, u64, u64, u64) -> *mut c_char = fcb_atlas_search_indexed_begin_matching;
    assert_eq!(include_str!("../include/fcb_atlas_search.h")
        .matches("fcb_atlas_search_begin_matching(").count(), 1);
    assert_eq!(include_str!("../include/fcb_atlas_index.h")
        .matches("fcb_atlas_search_indexed_begin_matching(").count(), 1);
}

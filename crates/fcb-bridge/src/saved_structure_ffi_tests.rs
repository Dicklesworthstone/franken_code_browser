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
    let _: unsafe extern "C" fn(u64, u64, u64, u64, *const c_char, u64, u64, u64, u64) -> *mut c_char =
        fcb_saved_structure_search_matching;
    let _: unsafe extern "C" fn(u64, u64, u64, u64, *const c_char, u64, u64, u64, u64, u64) -> *mut c_char =
        fcb_saved_structure_search_matching_page;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_structure_results;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_saved_clear_structure;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_open_structure_hit_reader;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["fcb_saved_structure_search(", "fcb_saved_structure_search_matching(",
        "fcb_saved_structure_search_matching_page(", "fcb_saved_structure_results(",
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

#[test]
fn c_counted_name_search_keeps_counts_windows_and_reader_provenance_separate() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let path = archive(&[(b"docs/guide.md", b"# Target\n\n## target\n\n### TARGET\n")]);
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));
    let needle = CString::new("target").unwrap();
    let first = take(unsafe { fcb_saved_structure_search_matching(handle, 1, 2, 0,
        needle.as_ptr(), 0, 1, 1, 4096) });
    for expected in ["\"case_sensitive\":false", "\"name_case\":\"unicode-lowercase\"",
        "\"count_all_matches\":true", "\"matches_seen\":\"3\"", "\"retained_hits\":\"1\"",
        "\"matches_seen_exact\":true", "\"complete\":true", "\"truncated\":true",
        "\"has_more_matches\":true", "\"next_match_offset\":\"1\"", "\"name\":\"Target\""] {
        assert!(first.contains(expected), "missing {expected}: {first}");
    }
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_saved_open_structure_hit_reader(handle, reader, 1, 1));
    assert!(opened.contains("\"command\":\"structure-open-reader\""), "{opened}");
    assert!(opened.contains("\"case_sensitive\":false"), "{opened}");
    assert!(opened.contains("\"count_all_matches\":true"), "{opened}");
    assert!(opened.contains("\"name\":\"Target\""), "{opened}");
    let later = take(unsafe { fcb_saved_structure_search_matching_page(handle, 2, 2, 0,
        needle.as_ptr(), 0, 1, 1, 4096, 1) });
    for expected in ["\"matches_seen\":\"3\"", "\"retained_hits\":\"1\"",
        "\"skipped_matches\":\"1\"", "\"next_match_offset\":\"2\"", "\"name\":\"target\""] {
        assert!(later.contains(expected), "missing {expected}: {later}");
    }
    let last = take(unsafe { fcb_saved_structure_search_matching_page(handle, 3, 2, 0,
        needle.as_ptr(), 0, 1, 1, 4096, 2) });
    for expected in ["\"matches_seen\":\"3\"", "\"matches_seen_exact\":true",
        "\"truncated\":false", "\"has_more_matches\":false", "\"next_match_offset\":null",
        "\"name\":\"TARGET\""] {
        assert!(last.contains(expected), "missing {expected}: {last}");
    }
    let count = take(unsafe { fcb_saved_structure_search_matching(handle, 4, 2, 0,
        needle.as_ptr(), 0, 1, 0, 4096) });
    for expected in ["\"matches_seen\":\"3\"", "\"retained_hits\":\"0\"",
        "\"matches_seen_exact\":true", "\"truncated\":true",
        "\"has_more_matches\":false", "\"next_match_offset\":null"] {
        assert!(count.contains(expected), "missing {expected}: {count}");
    }
    let page = take(fcb_saved_structure_results(handle, 4, 0, 64));
    assert!(page.contains("\"matches_seen\":\"3\""), "{page}");
    assert_eq!(fcb_saved_close(handle), 1);
    let retained = take(crate::reader_ffi::fcb_reader_info(reader));
    assert!(retained.contains("\"status\":\"ok\""), "{retained}");
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
}

#[test]
fn c_structure_matching_rejects_invalid_flags_and_preserves_exact_occurrences() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let path = archive(&[(b"src/source.rs", b"needle Needle needle\n")]);
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));
    let needle = CString::new("needle").unwrap();
    let accepted = take(unsafe { fcb_saved_structure_search_matching(handle, 1, 4, 0,
        needle.as_ptr(), 1, 1, 1, 4096) });
    assert!(accepted.contains("\"matches_seen\":\"2\""), "{accepted}");
    assert!(accepted.contains("\"retained_hits\":\"1\""), "{accepted}");
    for (case_sensitive, count_all) in [(2, 1), (1, 2)] {
        let error = take(unsafe { fcb_saved_structure_search_matching(handle, 2, 4, 0,
            needle.as_ptr(), case_sensitive, count_all, 1, 4096) });
        assert!(error.contains("SAVED_HANDLE_INVALID_ARGUMENT"), "{error}");
        assert!(!error.contains("needle"), "{error}");
    }
    let retained = take(fcb_saved_structure_results(handle, 1, 0, 64));
    assert!(retained.contains("\"matches_seen\":\"2\""), "{retained}");
    assert!(retained.contains("\"case_sensitive\":true"), "{retained}");
    assert!(retained.contains("\"name_case\":\"sensitive\""), "{retained}");
    // The new false policy is valid and retains the original mixed-case token.
    let later = take(unsafe { fcb_saved_structure_search_matching_page(handle, 2, 4, 0,
        needle.as_ptr(), 0, 1, 1, 4096, 1) });
    for expected in ["\"matches_seen\":\"3\"", "\"retained_hits\":\"1\"",
        "\"case_sensitive\":false", "\"name_case\":\"unicode-lowercase\"",
        "\"name\":\"Needle\"", "\"next_match_offset\":\"2\""] {
        assert!(later.contains(expected), "missing {expected}: {later}");
    }
    // Lowercase changes equality only; prefixes and malformed query tokens
    // cannot replace the accepted complete-token result.
    for (generation, name_mode) in [(3, 1), (4, 2)] {
        let error = take(unsafe { fcb_saved_structure_search_matching(handle, generation, 4,
            name_mode, needle.as_ptr(), 0, 1, 1, 4096) });
        assert!(error.contains("\"status\":\"error\""), "{error}");
    }
    let invalid_token = CString::new("needle::other").unwrap();
    let error = take(unsafe { fcb_saved_structure_search_matching(handle, 5, 4, 0,
        invalid_token.as_ptr(), 0, 1, 1, 4096) });
    assert!(error.contains("\"status\":\"error\""), "{error}");
    assert!(!error.contains("needle::other"), "{error}");
    let retained = take(fcb_saved_structure_results(handle, 2, 0, 64));
    assert!(retained.contains("\"matches_seen\":\"3\"") && retained.contains("\"name\":\"Needle\""));
    assert_eq!(fcb_saved_close(handle), 1);
}

#[test]
fn c_lowercase_occurrence_windows_preserve_original_utf16_and_independent_reader_bytes() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let bytes: Vec<_> = "\u{feff}K K k Kx".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let path = archive(&[(b"a.txt", &bytes), (b"b.txt", "K K".as_bytes())]);
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));
    let needle = CString::new("K").unwrap();
    let first = take(unsafe { fcb_saved_structure_search_matching_page(handle, 1, 4, 0,
        needle.as_ptr(), 0, 1, 1, 4096, 1) });
    for expected in ["\"case_sensitive\":false", "\"name_case\":\"unicode-lowercase\"",
        "\"requested_mode\":\"occurrences\"", "\"name_mode\":\"exact\"", "\"needle\":\"K\"",
        "\"matches_seen\":\"5\"", "\"matches_seen_exact\":true", "\"complete\":true",
        "\"retained_hits\":\"1\"", "\"skipped_matches\":\"1\"", "\"next_match_offset\":\"2\"",
        "\"name\":\"K\"", "\"original_range\":{\"start\":\"6\",\"end\":\"8\"}"] {
        assert!(first.contains(expected), "missing {expected}: {first}");
    }
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_saved_open_structure_hit_reader(handle, reader, 1, 1));
    for expected in ["\"needle\":\"K\"", "\"name\":\"K\"", "\"original_hex\":\"2a21\"",
        "\"live_source_reopened\":false", "\"case_sensitive\":false"] {
        assert!(opened.contains(expected), "missing {expected}: {opened}");
    }
    let metadata = take(fcb_saved_structure_results(handle, 1, 0, 1));
    assert_eq!(metadata, first);
    let last = take(unsafe { fcb_saved_structure_search_matching_page(handle, 2, 4, 0,
        needle.as_ptr(), 0, 1, 1, 4096, 4) });
    for expected in ["\"matches_seen\":\"5\"", "\"name\":\"K\"", "\"member\":\"1\"",
        "\"skipped_matches\":\"4\"", "\"has_more_matches\":false", "\"next_match_offset\":null"] {
        assert!(last.contains(expected), "missing {expected}: {last}");
    }
    let count = take(unsafe { fcb_saved_structure_search_matching(handle, 3, 4, 0,
        needle.as_ptr(), 0, 1, 0, 4096) });
    for expected in ["\"matches_seen\":\"5\"", "\"retained_hits\":\"0\"",
        "\"complete\":true", "\"matches_seen_exact\":true", "\"has_more_matches\":false"] {
        assert!(count.contains(expected), "missing {expected}: {count}");
    }
    // The legacy ABI still applies exact spelling after a lowercase query.
    let exact = search(handle, 4, 4, 0, "K");
    assert!(exact.contains("\"case_sensitive\":true") && exact.contains("\"matches_seen\":\"2\""), "{exact}");
    assert_eq!(fcb_saved_close(handle), 1);
    assert!(take(crate::reader_ffi::fcb_reader_copy_range(reader, 6, 8))
        .contains("\"original_hex\":\"2a21\""));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
}

#[test]
fn c_scoped_structure_search_preserves_windows_and_reader_path_provenance() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let saved = archive(&[(b"b.rs", b"needle"), (b"src-extra.rs", b"needle"),
        (b"src/\xff.rs", b"needle needle"), (b"src2/b.rs", b"needle")]);
    let handle = fcb_saved_create();
    assert!(take(unsafe { fcb_saved_open(handle, saved.as_ptr()) }).contains("\"status\":\"ok\""));
    let needle = CString::new("needle").unwrap();
    let run = |generation, scope, label: &[u8]| take(unsafe {
        fcb_saved_structure_search_scoped_page(handle, generation, 4, 0, needle.as_ptr(), 1, 1,
            1, 13, 1, scope, label.as_ptr(), label.len() as u64)
    });
    let folder = run(1, 1, b"src");
    for field in ["\"scope\":\"saved-directory\"", "\"scope_path_hex\":\"737263\"",
        "\"scope_known_members\":\"1\"", "\"matches_seen\":\"2\"", "\"skipped_matches\":\"1\"",
        "\"members_visited\":\"1\"", "\"member\":\"2\""] { assert!(folder.contains(field), "{folder}"); }
    let file = run(2, 2, b"src/\xff.rs");
    assert!(file.contains("\"scope_path_hex\":\"7372632fff2e7273\""));
    assert!(run(3, 1, b"../src").contains("\"status\":\"error\""));
    let reader = crate::reader_ffi::fcb_reader_create();
    let opened = take(fcb_saved_open_structure_hit_reader(handle, reader, 2, 1));
    for field in ["\"scope\":\"saved-member\"", "\"scope_path_hex\":\"7372632fff2e7273\"",
        "\"scope_membership_complete\":true", "\"member\":\"2\""] { assert!(opened.contains(field), "{opened}"); }
    assert_eq!(fcb_saved_close(handle), 1);
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
}

#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use crate::reader_ffi::{fcb_reader_create, fcb_reader_open, fcb_reader_info,
    fcb_reader_find, fcb_reader_copy_hit, fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString}, fs, sync::atomic::{AtomicU64, Ordering}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; result
}
fn source(bytes: &[u8]) -> (std::path::PathBuf, u64) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fcb-reference-c-{}-{}-{}.rs", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::write(&path, bytes).unwrap(); let cpath = CString::new(path.to_str().unwrap()).unwrap();
    let handle = fcb_reader_create(); assert_ne!(handle, 0);
    let opened = take(unsafe { fcb_reader_open(handle, cpath.as_ptr(), 1024 * 1024) });
    assert!(opened.contains("\"status\":\"ok\""), "{opened}"); (path, handle)
}

#[test]
fn c_occurrence_navigation_copy_replacement_and_clear_keep_capture() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (path, handle) = source(b"foo foobar\n// foo\n\"foo\"\n");
    fs::write(&path, b"replacement").unwrap();
    let name = CString::new("foo").unwrap();
    let inventory = take(unsafe { fcb_reader_references(handle, 1, name.as_ptr(), 4096) });
    assert!(inventory.contains("\"retained_references\":\"3\""), "{inventory}");
    assert!(inventory.contains("\"evidence_level\":\"whole-token-text-candidate\""));
    assert!(inventory.contains("\"includes_comments_and_literals\":true"));
    let next = take(fcb_reader_reference_page(handle, 1, 1, 1));
    assert!(next.contains("\"reference_id\":\"2\""));
    assert!(next.contains("\"next_offset\":\"2\""));
    let saved = fcb_reader_reference(handle, 1, 2, 128);
    let copy = take(fcb_reader_copy_reference(handle, 1, 3));
    assert!(copy.contains("\"original_hex\":\"666f6f\""));
    assert!(take(fcb_reader_reference(handle, 1, 0, 0)).contains("REFERENCE_NOT_FOUND"));
    assert!(take(fcb_reader_reference_page(handle, 1, 0, 0)).contains("INVALID_LIMITS"));
    assert!(take(fcb_reader_reference(handle, 1, 1, 16385)).contains("INVALID_LIMITS"));
    let find = take(unsafe { fcb_reader_find(handle, 1, name.as_ptr(), 10, 1024) });
    assert!(find.contains("\"status\":\"ok\""));
    take(unsafe { fcb_reader_references(handle, 2, name.as_ptr(), 4096) });
    assert!(take(fcb_reader_reference(handle, 1, 2, 0)).contains("REFERENCE_STALE_QUERY"));
    assert!(take(fcb_reader_references_clear(handle, 3)).contains("\"retained_references\":\"0\""));
    assert!(take(fcb_reader_copy_hit(handle, 1, 0)).contains("666f6f"));
    assert!(take(fcb_reader_reference(handle, 2, 1, 0)).contains("NO_REFERENCES"));
    assert_eq!(fcb_reader_close(handle), 1);
    let retained = take(saved);
    assert!(retained.contains("\"selection_namespace\":\"reference\""));
    assert!(retained.contains("// foo")); assert!(!retained.contains("replacement"));
    assert!(take(fcb_reader_copy_reference(handle, 2, 1)).contains("READER_HANDLE_UNKNOWN"));
}

#[test]
fn c_utf16_occurrences_preserve_original_byte_and_decoded_ranges() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    for little in [true, false] {
        let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in "𐐀();\r\nfoo foo_ foo".encode_utf16() {
            bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
        }
        let (_, handle) = source(&bytes); let name = CString::new("foo").unwrap();
        let out = take(unsafe { fcb_reader_references(handle, 1, name.as_ptr(), 4096) });
        assert!(out.contains("\"retained_references\":\"2\""), "{out}");
        assert!(out.contains("\"original_range\":{\"start\":\"16\",\"end\":\"22\"}"));
        let window = take(fcb_reader_reference(handle, 1, 1, 0));
        // The four-byte boundary guard retains the two preceding UTF-16
        // CR/LF units; selection offsets address the decoded UTF-8 window.
        assert!(window.contains("\"window_utf8_range\":{\"start\":\"2\",\"end\":\"5\"}"), "{window}");
        assert!(window.contains("\"selection_namespace\":\"reference\""));
        assert!(!window.contains("\"query_generation\":"));
        let expected = if little { "66006f006f00" } else { "0066006f006f" };
        assert!(take(fcb_reader_copy_reference(handle, 1, 1)).contains(expected));
        assert_eq!(fcb_reader_close(handle), 1);
    }
}

#[test]
fn c_invalid_names_and_source_limits_are_errors_with_intact_reader() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, handle) = source(b"foo foo"); let name = CString::new("foo").unwrap();
    take(unsafe { fcb_reader_references(handle, 1, name.as_ptr(), 4096) });
    assert!(take(unsafe { fcb_reader_references(handle, 2, std::ptr::null(), 4096) }).contains("INVALID_ARGUMENT"));
    let invalid_utf8 = [0xff_u8, 0];
    assert!(take(unsafe { fcb_reader_references(handle, 2, invalid_utf8.as_ptr().cast(), 4096) }).contains("INVALID_ARGUMENT"));
    let composite = CString::new("foo.bar").unwrap();
    assert!(take(unsafe { fcb_reader_references(handle, 2, composite.as_ptr(), 4096) }).contains("REFERENCE_INVALID_NAME"));
    assert!(take(fcb_reader_info(handle)).contains("\"accepted_reference_generation\":\"1\""));
    assert!(take(fcb_reader_reference_page(handle, 1, 0, 128)).contains("\"retained_references\":\"2\""));
    assert_eq!(fcb_reader_close(handle), 1);
    let (_, malformed) = source(b"foo\xff");
    assert!(take(unsafe { fcb_reader_references(malformed, 1, name.as_ptr(), 4096) }).contains("REFERENCE_UNSUPPORTED_ENCODING"));
    assert_eq!(fcb_reader_close(malformed), 1);
    let (_, oversized) = source(&vec![b' '; 512 * 1024 + 1]);
    assert!(take(unsafe { fcb_reader_references(oversized, 1, name.as_ptr(), 4096) }).contains("REFERENCE_SOURCE_LIMIT"));
    assert_eq!(fcb_reader_close(oversized), 1);
}

#[test]
fn c_limited_and_zero_cap_results_do_not_claim_exhaustive_counts() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, handle) = source(b"foo foo foo foo"); let name = CString::new("foo").unwrap();
    let limited = take(unsafe { fcb_reader_references(handle, 1, name.as_ptr(), 2) });
    for field in ["\"retained_references\":\"2\"", "\"matches_counted\":\"3\"",
        "\"search_complete\":false", "\"count_complete\":false", "\"output_limited\":true"] {
        assert!(limited.contains(field), "{limited}");
    }
    let lookahead = take(unsafe { fcb_reader_references(handle, 2, name.as_ptr(), 0) });
    assert!(lookahead.contains("\"retained_references\":\"0\""));
    assert!(lookahead.contains("\"matches_counted\":\"1\""));
    assert!(take(fcb_reader_reference(handle, 2, 1, 0)).contains("REFERENCE_NOT_FOUND"));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_later_occurrence_windows_count_beyond_the_cap_and_activate_original_utf16() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let name = CString::new("foo").unwrap();
    for little in [true, false] {
        let text = format!("\u{feff}😀\r\n{}", "foo\r\n".repeat(5005));
        let bytes: Vec<_> = text.encode_utf16()
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
        let (path, handle) = source(&bytes);
        fs::write(path, b"replacement").unwrap();
        let inventory = take(unsafe { fcb_reader_references_window(handle, 1, name.as_ptr(), 2, 1, 4500) });
        for fragment in ["\"matches_counted\":\"5005\"", "\"retained_references\":\"2\"",
            "\"result_offset\":\"4500\"", "\"skipped_matches\":\"4500\"", "\"reference_id\":\"1\"",
            "\"occurrence_id\":\"4501\"", "\"next_match_offset\":\"4502\"", "\"count_all_matches\":true",
            "\"count_complete\":true", "\"search_complete\":true", "\"output_limited\":true",
            "\"has_more_matches\":true", "\"additional_source_bytes_read\":\"0\"",
            "\"original_range\":{\"start\":\"45010\",\"end\":\"45016\"}"] {
            assert!(inventory.contains(fragment), "{fragment}: {inventory}");
        }
        let page = take(fcb_reader_reference_page(handle, 1, 1, 1));
        assert!(page.contains("\"reference_id\":\"2\""));
        assert!(page.contains("\"occurrence_id\":\"4502\""));
        assert!(page.contains("\"next_offset\":null"));
        assert!(take(fcb_reader_reference(handle, 1, 4501, 0)).contains("REFERENCE_NOT_FOUND"));
        let window = take(fcb_reader_reference(handle, 1, 1, 128));
        assert!(window.contains("\"reference_line\":\"4502\""));
        assert!(window.contains("\"window_utf8_range\":"));
        let copy = fcb_reader_copy_reference(handle, 1, 2);
        let last = take(unsafe { fcb_reader_references_window(handle, 2, name.as_ptr(), 2, 0, 5003) });
        assert!(last.contains("\"occurrence_id\":\"5005\""));
        assert!(last.contains("\"count_complete\":true"));
        assert!(last.contains("\"has_more_matches\":false"));
        assert!(last.contains("\"next_match_offset\":null"));
        assert!(take(fcb_reader_reference(handle, 1, 1, 0)).contains("REFERENCE_STALE_QUERY"));
        assert_eq!(fcb_reader_close(handle), 1);
        let expected = if little { "66006f006f00" } else { "0066006f006f" };
        assert!(take(copy).contains(expected));
    }
}

#[test]
fn c_window_flags_refusals_full_width_offsets_and_zero_storage_keep_exact_contracts() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (_, handle) = source(b"foo foo foo foo foo"); let name = CString::new("foo").unwrap();
    take(unsafe { fcb_reader_references_window(handle, 1, name.as_ptr(), 2, 1, 0) });
    assert!(take(unsafe { fcb_reader_references_window(handle, 2, name.as_ptr(), 2, 2, 2) })
        .contains("INVALID_ARGUMENT"));
    assert!(take(unsafe { fcb_reader_references_window(handle, 2, std::ptr::null(), 2, 1, 2) })
        .contains("INVALID_ARGUMENT"));
    assert!(take(unsafe { fcb_reader_references_window(handle, 2, name.as_ptr(), 4097, 1, 2) })
        .contains("REFERENCE_INVALID_LIMITS"));
    assert!(take(fcb_reader_info(handle)).contains("\"accepted_reference_generation\":\"1\""));
    assert!(take(fcb_reader_reference_page(handle, 1, 0, 128)).contains("\"occurrence_id\":\"1\""));
    let beyond = take(unsafe { fcb_reader_references_window(handle, 3, name.as_ptr(), 2, 0, u64::MAX) });
    for fragment in ["\"result_offset\":\"18446744073709551615\"", "\"skipped_matches\":\"5\"",
        "\"matches_counted\":\"5\"", "\"references\":[]", "\"search_complete\":true",
        "\"has_more_matches\":false", "\"next_match_offset\":null"] {
        assert!(beyond.contains(fragment), "{fragment}: {beyond}");
    }
    let counted = take(unsafe { fcb_reader_references_window(handle, 4, name.as_ptr(), 0, 1, 0) });
    for fragment in ["\"matches_counted\":\"5\"", "\"retained_references\":\"0\"",
        "\"search_complete\":true", "\"has_more_matches\":true", "\"next_match_offset\":null"] {
        assert!(counted.contains(fragment), "{fragment}: {counted}");
    }
    let legacy = take(unsafe { fcb_reader_references(handle, 5, name.as_ptr(), 2) });
    assert!(!legacy.contains("\"result_offset\":")); assert!(!legacy.contains("\"occurrence_id\":"));
    assert!(legacy.contains("\"matches_counted\":\"3\""));
    assert!(legacy.contains("\"count_complete\":false"));
    assert_eq!(fcb_reader_close(handle), 1);
}

#[test]
fn c_reference_types_and_public_header_declarations_agree() {
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64) -> *mut c_char = fcb_reader_references;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64) -> *mut c_char = fcb_reader_references_window;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_reference_page;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_reference;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_reader_copy_reference;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_reader_references_clear;
    let header = include_str!("../include/fcb_reader_references.h");
    for name in ["fcb_reader_references(", "fcb_reader_references_window(", "fcb_reader_reference_page(", "fcb_reader_reference(",
        "fcb_reader_copy_reference(", "fcb_reader_references_clear("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}

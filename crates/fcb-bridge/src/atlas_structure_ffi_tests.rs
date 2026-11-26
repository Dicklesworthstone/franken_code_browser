#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use super::super::{fcb_atlas_search_step, fcb_atlas_search_page, fcb_atlas_search_open_reader};
use super::super::index::fcb_atlas_index_prepare;
use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close, fcb_atlas_search_step_cancelable};
use crate::reader_ffi::{fcb_reader_create, fcb_reader_copy_range, fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString, c_void}, fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};

#[test]
fn structural_query_signature_and_public_header_match() {
    let _: unsafe extern "C" fn(u64, u64, u64, u64, *const c_char, *const c_char,
        *const c_char, u64, *const c_char, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_structure_begin;
    let header = include_str!("../include/fcb_atlas_structure.h");
    assert_eq!(header.matches("fcb_atlas_structure_begin(").count(), 1);
    assert!(header.contains("fcb_atlas_search_step"));
    assert!(header.contains("semantic_complete"));
    let _: unsafe extern "C" fn(u64, u64, u64, u64, u64, *const c_char,
        *const c_char, *const c_char, u64, *const c_char, u64, u64) -> *mut c_char
        = fcb_atlas_structure_indexed_begin;
    assert_eq!(header.matches("fcb_atlas_structure_indexed_begin(").count(), 1);
    let _: unsafe extern "C" fn(u64, u64, u64, u64, u64, *const c_char,
        *const c_char, *const c_char, u64, *const c_char, u64, u64, u64, u64) -> *mut c_char
        = fcb_atlas_structure_indexed_begin_window;
    assert_eq!(header.matches("fcb_atlas_structure_indexed_begin_window(").count(), 1);
    assert!(header.contains("retained-capture-structure"));
    let _: unsafe extern "C" fn(u64, u64, u64, u64, *const c_char, *const c_char,
        *const c_char, u64, *const c_char, u64, u64, u64, u64, u64) -> *mut c_char
        = fcb_atlas_structure_begin_matching;
    let _: unsafe extern "C" fn(u64, u64, u64, u64, u64, *const c_char,
        *const c_char, *const c_char, u64, *const c_char, u64, u64, u64) -> *mut c_char
        = fcb_atlas_structure_indexed_begin_matching;
    let _: unsafe extern "C" fn(u64, u64, u64, u64, u64, *const c_char,
        *const c_char, *const c_char, u64, *const c_char, u64, u64, u64, u64, u64) -> *mut c_char
        = fcb_atlas_structure_indexed_begin_window_matching;
    for name in ["fcb_atlas_structure_begin_matching(", "fcb_atlas_structure_indexed_begin_matching(",
        "fcb_atlas_structure_indexed_begin_window_matching("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}


fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a complete owned response");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn integer(json: &str, field: &str) -> u64 {
    let prefix = format!("\"{field}\":\"");
    json.split_once(prefix.as_str()).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
struct Fixture { root: PathBuf, handle: u64 }
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!("fcb-atlas-occurrences-c-{}-{}-{}",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        for (name, source) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }
        let handle = fcb_atlas_create(); assert_ne!(handle, 0);
        let root_name = CString::new(root.to_str().unwrap()).unwrap();
        let opened = take(unsafe { fcb_atlas_open(handle, root_name.as_ptr(), 100) });
        assert!(opened.contains("\"status\":\"ok\""), "{opened}");
        Self { root, handle }
    }
    fn close(&mut self) {
        assert_eq!(fcb_atlas_close(self.handle), 1); self.handle = 0;
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.handle != 0 { fcb_atlas_close(self.handle); }
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn begin(handle: u64, generation: u64, name: &str, name_mode: u64, maximum: u64) -> String {
    let name = CString::new(name).unwrap();
    take(unsafe { fcb_atlas_structure_begin(handle, generation, 4, name_mode, name.as_ptr(),
        std::ptr::null(), std::ptr::null(), 0, std::ptr::null(),
        maximum, 100, 1024 * 1024, 32 * 1024 * 1024) })
}
fn finish(handle: u64, generation: u64) -> String {
    for _ in 0..100 {
        let step = take(fcb_atlas_search_step(handle, generation));
        assert!(step.contains("\"status\":\"ok\""), "{step}");
        if step.contains("\"search_in_progress\":false") { return step; }
    }
    panic!("bounded occurrence query did not terminate");
}

#[test]
fn c_workspace_occurrences_preserve_scope_tokens_and_exact_utf16_capture_after_replacement() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let utf16: Vec<_> = "\u{feff}needle\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut fixture = Fixture::new(&[
        ("src/a.rs", b"needle needle_suffix xneedle\n// needle\n\"needle\"\n"),
        ("src/b.txt", &utf16), ("docs/excluded.rs", b"needle"),
    ]);
    let needle = CString::new("needle").unwrap(); let directory = CString::new("src").unwrap();
    let admitted = take(unsafe { fcb_atlas_structure_begin(fixture.handle, 1, 4, 0,
        needle.as_ptr(), std::ptr::null(), std::ptr::null(), 1, directory.as_ptr(),
        100, 100, 1024 * 1024, 32 * 1024 * 1024) });
    assert!(admitted.contains("\"command\":\"begin-structure\""), "{admitted}");
    assert_eq!(integer(&admitted, "source_bytes_read"), 0);
    let mut expired = needle.into_bytes_with_nul(); expired.fill(b'x');
    let found = finish(fixture.handle, 1);
    assert_eq!(integer(&found, "retained_hits"), 4);
    assert!(found.contains("\"search_complete\":true"), "{found}");
    for fragment in ["\"mode\":\"identifier-occurrence\"", "\"requested_mode\":\"occurrences\"",
        "\"realized_mode\":\"whole-token-text-candidate\"", "\"name_mode\":\"exact\"",
        "\"includes_comments_and_literals\":true", "\"compiler_resolved\":false",
        "\"semantic_complete\":false", "\"unicode_normalization\":\"none\"",
        "\"name_syntax\":\"single-token-no-leading-ascii-digit\"",
        "\"structure_file_byte_limit\":\"1048576\"", "\"kind\":\"identifier-occurrence\"",
        "\"language\":\"text\""] {
        assert!(found.contains(fragment), "missing {fragment}: {found}");
    }
    assert!(!found.contains("docs/excluded.rs"));
    fs::write(fixture.root.join("src/b.txt"), b"changed working file").unwrap();
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let imported = take(fcb_atlas_search_open_reader(fixture.handle, reader, 1, 4));
    assert!(imported.contains("\"source_reopened\":false"), "{imported}");
    let owned_page = fcb_atlas_search_page(fixture.handle, 1, 0, 100);
    fixture.close();
    assert_eq!(integer(&take(owned_page), "retained_hits"), 4);
    let copied = take(fcb_reader_copy_range(reader, 2, 14));
    assert!(copied.contains("\"original_hex\":\"6e006500650064006c006500\""), "{copied}");
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn c_occurrence_query_rejects_non_tokens_nonexact_modes_and_foreign_input_without_replacing_results() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(&[("source.rs", b"needle")]);
    assert!(begin(fixture.handle, 1, "needle", 0, 100).contains("\"status\":\"ok\""));
    finish(fixture.handle, 1);
    for (i, name) in ["needle suffix", "needle::suffix", "9needle", "needle.", "needle\n"].into_iter().enumerate() {
        let rejected = begin(fixture.handle, i as u64 + 2, name, 0, 100);
        assert!(rejected.contains("REFERENCE_INVALID_NAME"), "{rejected}");
        assert_eq!(integer(&take(fcb_atlas_search_page(fixture.handle, 1, 0, 100)), "retained_hits"), 1);
    }
    for name_mode in [1, 2] {
        assert!(begin(fixture.handle, 10 + name_mode, "needle", name_mode, 100)
            .contains("\"status\":\"error\""));
    }
    let invalid_utf8 = [255u8, 0];
    for pointer in [std::ptr::null(), invalid_utf8.as_ptr().cast()] {
        let rejected = take(unsafe { fcb_atlas_structure_begin(fixture.handle, 20, 4, 0,
            pointer, std::ptr::null(), std::ptr::null(), 0, std::ptr::null(),
            100, 100, 1024 * 1024, 32 * 1024 * 1024) });
        assert!(rejected.contains("INVALID_ARGUMENT"), "{rejected}");
    }
    let needle = CString::new("needle").unwrap();
    let reserved = take(unsafe { fcb_atlas_structure_begin(fixture.handle, 20, 3, 0,
        needle.as_ptr(), std::ptr::null(), std::ptr::null(), 0, std::ptr::null(),
        100, 100, 1024 * 1024, 32 * 1024 * 1024) });
    assert!(reserved.contains("INVALID_ARGUMENT"), "{reserved}");
    assert_eq!(integer(&take(fcb_atlas_search_page(fixture.handle, 1, 0, 100)), "retained_hits"), 1);
    assert!(begin(fixture.handle, 21, "needle", 0, 100).contains("\"status\":\"ok\""));
    assert_eq!(integer(&finish(fixture.handle, 21), "retained_hits"), 1);
}

#[test]
fn c_occurrence_caps_and_bad_encoding_report_partial_coverage_without_fabricated_misses() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let oversized = vec![b' '; 1024 * 1024 + 1];
    let bad = [b'n', b'e', b'e', b'd', b'l', b'e', 255];
    let fixture = Fixture::new(&[("a.txt", b"needle needle"), ("b.txt", &oversized), ("c.txt", &bad)]);
    assert!(begin(fixture.handle, 1, "needle", 0, 100).contains("\"status\":\"ok\""));
    let all = finish(fixture.handle, 1);
    assert_eq!(integer(&all, "retained_hits"), 2);
    assert!(all.contains("\"search_complete\":false"), "{all}");
    assert!(all.contains("STRUCTURE_FILE_BYTE_LIMIT"), "{all}");
    assert!(all.contains("REFERENCE_UNSUPPORTED_ENCODING"), "{all}");
    assert!(begin(fixture.handle, 2, "needle", 0, 1).contains("\"status\":\"ok\""));
    let capped = finish(fixture.handle, 2);
    assert_eq!(integer(&capped, "retained_hits"), 1);
    assert!(capped.contains("\"truncated\":true") && capped.contains("\"search_complete\":false"), "{capped}");
    assert_eq!(integer(&capped, "matches_seen"), 2);
}

unsafe extern "C" fn cancel_after_entry(context: *mut c_void) -> i32 {
    let polls = unsafe { &mut *context.cast::<u64>() };
    *polls += 1;
    i32::from(*polls > 1)
}

#[test]
fn c_canceling_occurrence_extraction_preserves_previous_query_and_reader_ownership() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(&[("source.rs", b"needle other")]);
    begin(fixture.handle, 1, "needle", 0, 100); finish(fixture.handle, 1);
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    assert!(take(fcb_atlas_search_open_reader(fixture.handle, reader, 1, 1))
        .contains("\"source_reopened\":false"));
    begin(fixture.handle, 2, "other", 0, 100);
    let mut polls = 0u64;
    let canceled = unsafe { fcb_atlas_search_step_cancelable(fixture.handle, 2,
        Some(cancel_after_entry), std::ptr::from_mut(&mut polls).cast()) };
    assert!(canceled.is_null());
    assert!(polls > 1);
    assert_eq!(integer(&take(fcb_atlas_search_page(fixture.handle, 1, 0, 100)), "retained_hits"), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 6)).contains("6e6565646c65"));
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn c_indexed_structure_uses_scoped_captures_and_zero_io_after_live_file_disappears() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let mut fixture = Fixture::new(&[("src/a.rs", b"fn needle() {}\n"),
        ("src/noise.txt", b"needle"), ("src-old/a.rs", b"fn needle() {}\n")]);
    let prepared = take(fcb_atlas_index_prepare(fixture.handle, 1, 100,
        1024 * 1024, 32 * 1024 * 1024, 0));
    assert!(prepared.contains("\"status\":\"ok\""), "{prepared}");
    fs::remove_file(fixture.root.join("src/a.rs")).unwrap();
    let needle = CString::new("needle").unwrap(); let types = CString::new("rs").unwrap();
    let directory = CString::new("src").unwrap();
    let admitted = take(unsafe { fcb_atlas_structure_indexed_begin(fixture.handle, 2, 1,
        1, 0, needle.as_ptr(), types.as_ptr(), std::ptr::null(), 1,
        directory.as_ptr(), 100, 32 * 1024 * 1024) });
    assert!(admitted.contains("\"command\":\"begin-indexed-structure\""), "{admitted}");
    assert_eq!(integer(&admitted, "source_bytes_read"), 0);
    let page = finish(fixture.handle, 2);
    for fragment in ["\"search_strategy\":\"retained-capture-structure\"",
        "\"search_complete\":true", "\"semantic_complete\":false",
        "\"mode\":\"symbol-name\"", "\"realized_mode\":\"heuristic-outline-candidate\""] {
        assert!(page.contains(fragment), "{fragment}: {page}");
    }
    assert_eq!(integer(&page, "retained_hits"), 1);
    assert_eq!(integer(&page, "eligible_captured_files"), 1);
    assert_eq!(integer(&page, "verification_source_bytes"), b"fn needle() {}\n".len() as u64);
    assert_eq!(integer(&page, "source_bytes_read"), 0); assert_eq!(integer(&page, "read_calls"), 0);
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    assert!(take(fcb_atlas_search_open_reader(fixture.handle, reader, 2, 1))
        .contains("\"source_reopened\":false"));
    // Extraction budget admission is distinct from capture/source I/O limits.
    let empty_budget = take(unsafe { fcb_atlas_structure_indexed_begin(fixture.handle, 3, 1,
        1, 0, needle.as_ptr(), types.as_ptr(), std::ptr::null(), 1, directory.as_ptr(), 100, 0) });
    assert!(empty_budget.contains("\"status\":\"ok\""), "{empty_budget}");
    let limited = finish(fixture.handle, 3);
    assert!(limited.contains("\"stop_reason\":\"verification-byte-limit\""), "{limited}");
    assert_eq!(integer(&limited, "pending_files"), 1);
    assert_eq!(integer(&limited, "retained_hits"), 0);
    assert_eq!(integer(&limited, "verification_source_bytes"), 0);
    fixture.close();
    assert!(take(fcb_reader_copy_range(reader, 3, 9)).contains("6e6565646c65"));
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn c_indexed_structure_validates_modes_limits_and_cancels_without_losing_accepted_rows() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(&[("source.txt", b"needle other")]);
    take(fcb_atlas_index_prepare(fixture.handle, 1, 100, 1024, 1024, 0));
    let needle = CString::new("needle").unwrap();
    let begin_indexed = |generation, index_generation, mode, name_mode, maximum, bytes| {
        take(unsafe { fcb_atlas_structure_indexed_begin(fixture.handle, generation, index_generation,
            mode, name_mode, needle.as_ptr(), std::ptr::null(), std::ptr::null(), 0,
            std::ptr::null(), maximum, bytes) })
    };
    assert!(begin_indexed(2, 1, 4, 0, 100, 1024).contains("\"status\":\"ok\""));
    assert_eq!(integer(&finish(fixture.handle, 2), "retained_hits"), 1);
    assert!(begin_indexed(3, 1, 3, 0, 100, 1024).contains("INVALID_ARGUMENT"));
    assert!(begin_indexed(3, 1, 4, 1, 100, 1024).contains("ATLAS_SEARCH_INVALID_LIMITS"));
    assert!(begin_indexed(4, 1, 4, 0, 0, 1024).contains("ATLAS_SEARCH_INVALID_LIMITS"));
    assert!(begin_indexed(5, 1, 4, 0, 100, 32 * 1024 * 1024 + 1).contains("ATLAS_SEARCH_INVALID_LIMITS"));
    assert!(begin_indexed(6, 99, 4, 0, 100, 1024).contains("ATLAS_SEARCH_STALE_INDEX"));
    assert_eq!(integer(&take(fcb_atlas_search_page(fixture.handle, 2, 0, 100)), "retained_hits"), 1);
    assert!(begin_indexed(7, 1, 4, 0, 100, 1024).contains("\"status\":\"ok\""));
    let mut polls = 0u64;
    let canceled = unsafe { fcb_atlas_search_step_cancelable(fixture.handle, 7,
        Some(cancel_after_entry), std::ptr::from_mut(&mut polls).cast()) };
    assert!(canceled.is_null()); assert!(polls > 1);
    assert_eq!(integer(&take(fcb_atlas_search_page(fixture.handle, 2, 0, 100)), "retained_hits"), 1);
}

#[test]
fn c_structural_windows_reach_late_utf16_occurrences_from_the_original_capture() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let utf16: Vec<_> = "\u{feff}".encode_utf16().chain("needle\n".repeat(5005).encode_utf16())
        .flat_map(u16::to_le_bytes).collect();
    let mut fixture = Fixture::new(&[("src/repeated.txt", &utf16), ("other.txt", b"needle")]);
    let prepared = take(fcb_atlas_index_prepare(fixture.handle, 1, 100,
        1024 * 1024, 32 * 1024 * 1024, 0));
    assert!(prepared.contains("\"capture_complete\":true"), "{prepared}");
    fs::write(fixture.root.join("src/repeated.txt"), b"replacement").unwrap();
    let needle = CString::new("needle").unwrap();
    let directory = CString::new("src").unwrap();
    let null = std::ptr::null();
    let begin = take(unsafe { fcb_atlas_structure_indexed_begin_window(fixture.handle, 2, 1,
        4, 0, needle.as_ptr(), null, null, 1, directory.as_ptr(), 1, 4500, 3, 32 * 1024 * 1024) });
    assert!(begin.contains("\"status\":\"ok\""), "{begin}");
    assert_eq!(integer(&begin, "result_offset"), 4500);
    assert_eq!(integer(&begin, "source_bytes_read"), 0);
    let page = finish(fixture.handle, 2);
    for (field, expected) in [("matches_seen", 5005), ("retained_hits", 3),
        ("skipped_matches", 4500), ("window_matches", 3), ("next_match_offset", 4503),
        ("occurrence_id", 4501), ("eligible_files", 1), ("source_bytes_read", 0), ("read_calls", 0)] {
        assert_eq!(integer(&page, field), expected, "{field}: {page}");
    }
    for expected in ["\"has_more_matches\":true", "\"search_complete\":true",
        "\"match_count_complete\":true", "\"semantic_complete\":false",
        "\"original_range\":{\"start\":\"63002\",\"end\":\"63014\"}"] {
        assert!(page.contains(expected), "missing {expected}: {page}");
    }
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    assert!(take(fcb_atlas_search_open_reader(fixture.handle, reader, 2, 1))
        .contains("\"source_reopened\":false"));
    // A canceled later window leaves the accepted page and reader intact.
    let next = take(unsafe { fcb_atlas_structure_indexed_begin_window(fixture.handle, 3, 1,
        4, 0, needle.as_ptr(), null, null, 1, directory.as_ptr(), 1, 4503, 3, 32 * 1024 * 1024) });
    assert!(next.contains("\"status\":\"ok\""), "{next}");
    let mut polls = 0u64;
    let canceled = unsafe { fcb_atlas_search_step_cancelable(fixture.handle, 3,
        Some(cancel_after_entry), std::ptr::from_mut(&mut polls).cast()) };
    assert!(canceled.is_null()); assert!(polls > 1);
    assert_eq!(integer(&take(fcb_atlas_search_page(fixture.handle, 2, 0, 100)), "result_offset"), 4500);
    fixture.close();
    assert!(take(fcb_reader_copy_range(reader, 63002, 63014))
        .contains("\"original_hex\":\"6e006500650064006c006500\""));
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn c_structural_windows_validate_inputs_and_full_width_offsets_without_losing_results() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(&[("source.txt", b"needle needle needle")]);
    take(fcb_atlas_index_prepare(fixture.handle, 1, 100, 1024, 1024, 0));
    let needle = CString::new("needle").unwrap();
    let null = std::ptr::null();
    let begin = |generation, mode, name_mode, text, counted, offset, maximum, bytes| {
        take(unsafe { fcb_atlas_structure_indexed_begin_window(fixture.handle, generation, 1,
            mode, name_mode, text, null, null, 0, null, counted, offset, maximum, bytes) })
    };
    assert!(begin(2, 4, 0, needle.as_ptr(), 0, 1, 1, 1024).contains("\"status\":\"ok\""));
    assert_eq!(integer(&finish(fixture.handle, 2), "occurrence_id"), 2);
    for (mode, name_mode, counted) in [(3, 0, 0), (4, 3, 0), (4, 0, 2)] {
        assert!(begin(3, mode, name_mode, needle.as_ptr(), counted, 1, 1, 1024)
            .contains("INVALID_ARGUMENT"));
    }
    let unterminated = [b'x' as c_char; 1025];
    let malformed = [0xffu8, 0];
    for text in [null, unterminated.as_ptr(), malformed.as_ptr().cast()] {
        assert!(begin(3, 4, 0, text, 0, 1, 1, 1024).contains("INVALID_ARGUMENT"));
    }
    assert_eq!(integer(&take(fcb_atlas_search_page(fixture.handle, 2, 0, 100)), "result_offset"), 1);
    // A full-width offset is accepted as an ordinal, never narrowed or added
    // unchecked to a row cap. It yields an empty, fully counted window.
    assert!(begin(3, 4, 0, needle.as_ptr(), 1, u64::MAX, 1, 1024).contains("\"status\":\"ok\""));
    let past_end = finish(fixture.handle, 3);
    assert_eq!(integer(&past_end, "result_offset"), u64::MAX);
    assert_eq!(integer(&past_end, "matches_seen"), 3);
    assert_eq!(integer(&past_end, "skipped_matches"), 3);
    assert_eq!(integer(&past_end, "retained_hits"), 0);
    assert!(past_end.contains("\"search_complete\":true"), "{past_end}");
    assert!(past_end.contains("\"has_more_matches\":false"), "{past_end}");
    assert!(past_end.contains("\"next_match_offset\":null"), "{past_end}");
    assert!(begin(4, 4, 0, needle.as_ptr(), 1, 1, 1, 0).contains("\"status\":\"ok\""));
    let limited = finish(fixture.handle, 4);
    assert!(limited.contains("\"stop_reason\":\"verification-byte-limit\""), "{limited}");
    assert!(limited.contains("\"search_complete\":false"), "{limited}");
    assert_eq!(integer(&limited, "pending_files"), 1);
    assert_eq!(integer(&limited, "source_bytes_read"), 0);
}

#[test]
fn c_matching_occurrences_share_policy_across_live_indexed_windows_and_exact_reader_copies() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let mut fixture = Fixture::new(&[("source.txt", "k K K kx _K".as_bytes())]);
    let name = CString::new("K").unwrap(); let null = std::ptr::null();
    let started = take(unsafe { fcb_atlas_structure_begin_matching(fixture.handle, 1,
        4, 0, name.as_ptr(), null, null, 0, null, 0, 100, 100, 1024, 1024) });
    assert!(started.contains("\"status\":\"ok\""), "{started}");
    let live = finish(fixture.handle, 1);
    assert_eq!(integer(&live, "retained_hits"), 3);
    assert!(live.contains("\"case_sensitive\":false"));
    assert!(live.contains("\"name_case\":\"unicode-lowercase\""));
    assert!(live.contains("\"name\":\"K\""));
    let prepared = take(fcb_atlas_index_prepare(fixture.handle, 2, 100, 1024, 1024, 0));
    assert!(prepared.contains("\"capture_complete\":true"), "{prepared}");
    fs::write(fixture.root.join("source.txt"), b"replacement").unwrap();
    let later = take(unsafe { fcb_atlas_structure_indexed_begin_window_matching(fixture.handle, 3, 2,
        4, 0, name.as_ptr(), null, null, 0, null, 0, 1, 2, 1, 1024) });
    assert!(later.contains("\"status\":\"ok\""), "{later}");
    let page = finish(fixture.handle, 3);
    for (field, expected) in [("matches_seen", 3), ("retained_hits", 1), ("occurrence_id", 3),
        ("source_bytes_read", 0), ("read_calls", 0)] { assert_eq!(integer(&page, field), expected); }
    assert!(page.contains("\"name_case\":\"unicode-lowercase\""));
    assert!(page.contains("\"name\":\"K\""));
    assert!(page.contains("\"next_match_offset\":null"));
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    assert!(take(fcb_atlas_search_open_reader(fixture.handle, reader, 3, 1))
        .contains("\"source_reopened\":false"));
    let direct = take(unsafe { fcb_atlas_structure_indexed_begin_matching(fixture.handle, 4, 2,
        4, 0, name.as_ptr(), null, null, 0, null, 0, 10, 1024) });
    assert!(direct.contains("\"status\":\"ok\""), "{direct}");
    assert_eq!(integer(&finish(fixture.handle, 4), "retained_hits"), 3);
    let exact = take(unsafe { fcb_atlas_structure_indexed_begin(fixture.handle, 5, 2,
        4, 0, name.as_ptr(), null, null, 0, null, 10, 1024) });
    assert!(exact.contains("\"status\":\"ok\""), "{exact}");
    let legacy = finish(fixture.handle, 5);
    assert_eq!(integer(&legacy, "retained_hits"), 1);
    assert!(legacy.contains("\"name_case\":\"sensitive\""));
    for (case_sensitive, counted) in [(2, 0), (0, 2), (u64::MAX, 1)] {
        let refused = take(unsafe { fcb_atlas_structure_indexed_begin_window_matching(fixture.handle, 6, 2,
            4, 0, null, null, null, 0, null, case_sensitive, counted, 0, 1, 1024) });
        assert!(refused.contains("INVALID_ARGUMENT"));
    }
    assert_eq!(integer(&take(fcb_atlas_search_page(fixture.handle, 5, 0, 100)), "retained_hits"), 1);
    fixture.close();
    assert!(take(fcb_reader_copy_range(reader, 4, 7)).contains("\"original_hex\":\"e284aa\""));
    assert_eq!(fcb_reader_close(reader), 1);
}

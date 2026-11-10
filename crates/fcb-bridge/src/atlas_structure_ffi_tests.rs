#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use super::super::{fcb_atlas_search_step, fcb_atlas_search_page, fcb_atlas_search_open_reader};
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
        "\"structure_file_byte_limit\":\"524288\"", "\"kind\":\"identifier-occurrence\"",
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
    let oversized = vec![b' '; 512 * 1024 + 1];
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

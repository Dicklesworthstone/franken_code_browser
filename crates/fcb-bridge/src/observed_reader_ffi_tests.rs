#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::{ffi::{CStr, CString}, fs::{self, OpenOptions}, os::unix::{ffi::{OsStrExt, OsStringExt}, fs::FileExt},
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use crate::reader_ffi::FFI_TEST_LOCK;

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a bounded JSON receipt");
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    result
}
fn root() -> (std::path::PathBuf, CString) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!("fcb-observed-ffi-{}-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    let text = CString::new(root.to_str().unwrap()).unwrap();
    (root, text)
}
fn hex(bytes: &[u8]) -> CString {
    CString::new(bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>()).unwrap()
}
fn open(root: &CString, relative: &[u8]) -> u64 {
    let handle = fcb_observed_reader_create(); assert_ne!(handle, 0);
    let relative = hex(relative);
    let info = take(unsafe { fcb_observed_reader_open(handle, root.as_ptr(), relative.as_ptr(), None, std::ptr::null_mut()) });
    assert!(info.contains("\"status\":\"ok\""), "{info}");
    assert!(info.contains("\"payload_bytes_read\":\"0\""));
    assert!(info.contains("\"whole_file_immutable\":false"));
    handle
}
fn window(handle: u64, offset: u64, bytes: u64, encoding: u64) -> String {
    take(unsafe { fcb_observed_reader_window(handle, offset, bytes, encoding, None, std::ptr::null_mut()) })
}

#[test]
fn c_sparse_file_past_four_gib_reads_only_requested_ranges_and_keeps_old_bytes() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(root.join("huge.rs")).unwrap();
    let offset = (1u64 << 32) + 1024;
    file.set_len(offset + 128).unwrap();
    file.write_at(b"before", offset).unwrap();
    let handle = open(&croot, b"huge.rs");
    let page = window(handle, offset, 6, 1);
    assert!(page.contains("\"text\":\"before\""), "{page}");
    assert!(page.contains(&format!("\"visible_range\":{{\"start\":\"{offset}\",\"end\":\"{}\"}}", offset + 6)));
    assert!(page.contains("\"observation_source_bytes_read\":\"22\""));
    assert!(page.contains("\"observation\":\"1\""));
    assert!(!page.contains("\"capture_sha256\""), "an extent digest must not become a whole-source digest");
    assert!(page.len() < 8192);
    file.write_at(b"after!", offset).unwrap();
    let newer = window(handle, offset, 6, 1);
    assert!(newer.contains("\"text\":\"after!\""));
    assert!(newer.contains("\"observation\":\"2\""));
    let retained = take(fcb_observed_reader_retained(handle, 1));
    assert!(retained.contains("\"text\":\"before\""));
    assert!(retained.contains("\"additional_source_bytes_read\":\"0\""));
    let copy = take(fcb_observed_reader_copy(handle, 1, offset, offset + 6));
    assert!(copy.contains("\"original_hex\":\"6265666f7265\""));
    assert!(copy.contains("\"additional_read_calls\":\"0\""));
    for _ in 0..7 { assert!(window(handle, offset, 6, 1).contains("\"status\":\"ok\"")); }
    assert!(take(fcb_observed_reader_copy(handle, 1, offset, offset + 6)).contains("\"status\":\"error\""));
    assert!(take(fcb_observed_reader_retained(handle, 1)).contains("\"status\":\"error\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
    assert!(window(handle, offset, 6, 1).contains("OBSERVED_READER_HANDLE_UNKNOWN"));
}

#[test]
fn c_standalone_raw_path_opens_a_sparse_file_without_reading_the_source() {
    let _: unsafe extern "C" fn(u64, *const c_char, Option<SearchCancellationCallback>, *mut c_void)
        -> *mut c_char = fcb_observed_reader_open_file;
    assert!(include_str!("../include/fcb_observed_reader.h").contains("fcb_observed_reader_open_file("));
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, _) = root();
    let path = root.join(std::ffi::OsString::from_vec(b"selected-\xff.rs".to_vec()));
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
    let offset = (1u64 << 32) + 512;
    file.set_len(offset + 128).unwrap();
    assert_eq!(file.write_at(b"before", offset).unwrap(), 6);
    let encoded = hex(path.as_os_str().as_bytes());
    let handle = fcb_observed_reader_create(); assert_ne!(handle, 0);
    let info = take(unsafe { fcb_observed_reader_open_file(handle, encoded.as_ptr(), None, std::ptr::null_mut()) });
    assert!(info.contains("\"status\":\"ok\""), "{info}");
    assert!(info.contains(&format!("\"hex\":\"{}\"", encoded.to_str().unwrap())));
    assert!(info.contains(&format!("\"initial_observed_length\":\"{}\"", offset + 128)));
    assert!(info.contains("\"payload_bytes_read\":\"0\""));
    assert!(info.contains("\"read_calls\":\"0\""));
    assert!(info.contains("\"retained_observations\":[]"));
    let page = window(handle, offset, 6, 1);
    assert!(page.contains("\"text\":\"before\""), "{page}");
    assert!(page.contains("\"observation_source_bytes_read\":\"22\""));
    fs::rename(&path, root.join("moved.rs")).unwrap();
    fs::write(&path, b"replacement").unwrap();
    assert!(window(handle, offset, 6, 1).contains("\"text\":\"before\""));
    assert!(take(fcb_observed_reader_copy(handle, 1, offset, offset + 6)).contains("\"original_hex\":\"6265666f7265\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
    assert!(take(fcb_observed_reader_info(handle)).contains("OBSERVED_READER_HANDLE_UNKNOWN"));
}

#[test]
fn c_standalone_open_rejects_invalid_paths_without_loading_or_retargeting_a_handle() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let path = root.join("source.rs"); fs::write(&path, b"selected source\n").unwrap();
    let encoded = hex(path.as_os_str().as_bytes());
    let handle = fcb_observed_reader_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_observed_reader_open_file(handle, std::ptr::null(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    for malformed in [String::new(), "f".into(), "zz".into(), "2F".into(), "61".repeat(MAX_PATH_BYTES + 1)] {
        let malformed = CString::new(malformed).unwrap();
        assert!(take(unsafe { fcb_observed_reader_open_file(handle, malformed.as_ptr(), None, std::ptr::null_mut()) })
            .contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    }
    for invalid in [b"source.rs".as_slice(), b"/", b"/tmp/../source.rs", b"/tmp/./source.rs", b"/tmp//source.rs", b"/tmp/source.rs\0"] {
        let invalid = hex(invalid);
        assert!(take(unsafe { fcb_observed_reader_open_file(handle, invalid.as_ptr(), None, std::ptr::null_mut()) })
            .contains("OBSERVED_READER_INVALID_PATH"));
    }
    let link = root.join("link.rs"); std::os::unix::fs::symlink(&path, &link).unwrap();
    let link = hex(link.as_os_str().as_bytes());
    assert!(take(unsafe { fcb_observed_reader_open_file(handle, link.as_ptr(), None, std::ptr::null_mut()) })
        .contains("\"status\":\"error\""));
    assert!(take(unsafe { fcb_observed_reader_open_file(handle, encoded.as_ptr(), Some(cancel_now), std::ptr::null_mut()) })
        .contains("\"exit_code\":\"130\""));
    assert!(take(fcb_observed_reader_info(handle)).contains("OBSERVED_READER_HANDLE_NOT_OPEN"));
    assert!(take(unsafe { fcb_observed_reader_open_file(handle, encoded.as_ptr(), None, std::ptr::null_mut()) })
        .contains("\"payload_bytes_read\":\"0\""));
    assert!(take(unsafe { fcb_observed_reader_open_file(handle, encoded.as_ptr(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_ALREADY_OPEN"));
    let relative = hex(b"source.rs");
    assert!(take(unsafe { fcb_observed_reader_open(handle, croot.as_ptr(), relative.as_ptr(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_ALREADY_OPEN"));
    assert!(window(handle, 0, 8, 1).contains("\"text\":\"selected\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

#[test]
fn c_root_relative_raw_names_and_explicit_utf16_preserve_original_coordinates() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let name = b"raw-\xff\\name.rs";
    let bytes: Vec<u8> = "\u{feff}A🦀B\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(root.join(std::ffi::OsString::from_vec(name.to_vec())), &bytes).unwrap();
    let handle = open(&croot, name);
    let page = window(handle, 2, 8, 2);
    assert!(page.contains("\"encoding\":\"utf16le\""));
    assert!(page.contains("A🦀B"), "{page}");
    assert!(page.contains("\"original_hex\":\"41003ed880dd4200\""), "{page}");
    assert!(take(fcb_observed_reader_copy(handle, 1, 2, 10)).contains("41003ed880dd4200"));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

unsafe extern "C" fn cancel_now(_: *mut c_void) -> i32 { 1 }

#[test]
fn c_invalid_inputs_and_cancellation_preserve_accepted_observations() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    fs::write(root.join("source.rs"), b"retained source\n").unwrap();
    let relative = hex(b"source.rs");
    let handle = fcb_observed_reader_create(); assert_ne!(handle, 0);
    for bad in [b"../source.rs".as_slice(), b"/source.rs", b"./source.rs", b"a//b", b"a\0b"] {
        let bad = hex(bad);
        assert!(take(unsafe { fcb_observed_reader_open(handle, croot.as_ptr(), bad.as_ptr(), None, std::ptr::null_mut()) })
            .contains("\"status\":\"error\""));
    }
    let malformed = CString::new("zz").unwrap();
    assert!(take(unsafe { fcb_observed_reader_open(handle, croot.as_ptr(), malformed.as_ptr(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    assert!(take(unsafe { fcb_observed_reader_open(handle, std::ptr::null(), relative.as_ptr(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    let invalid_utf8 = [255u8, 0];
    assert!(take(unsafe { fcb_observed_reader_open(handle, invalid_utf8.as_ptr().cast(), relative.as_ptr(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    let too_long = CString::new("x".repeat(MAX_PATH_BYTES + 1)).unwrap();
    assert!(take(unsafe { fcb_observed_reader_open(handle, too_long.as_ptr(), relative.as_ptr(), None, std::ptr::null_mut()) })
        .contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    assert!(take(unsafe { fcb_observed_reader_open(handle, croot.as_ptr(), relative.as_ptr(), Some(cancel_now), std::ptr::null_mut()) })
        .contains("\"exit_code\":\"130\""));
    assert!(take(fcb_observed_reader_info(handle)).contains("OBSERVED_READER_HANDLE_NOT_OPEN"));
    assert!(take(unsafe { fcb_observed_reader_open(handle, croot.as_ptr(), relative.as_ptr(), None, std::ptr::null_mut()) })
        .contains("\"payload_bytes_read\":\"0\""));
    assert!(window(handle, 0, 8, 1).contains("\"observation\":\"1\""));
    for (offset, bytes, encoding) in [(0, 0, 1), (0, 262_145, 1), (u64::MAX, 8, 1), (0, u64::MAX, 1), (0, 8, 0), (0, 8, 4)] {
        assert!(window(handle, offset, bytes, encoding).contains("\"status\":\"error\""));
    }
    assert!(take(unsafe { fcb_observed_reader_window(handle, 0, 8, 1, Some(cancel_now), std::ptr::null_mut()) })
        .contains("\"exit_code\":\"130\""));
    assert!(take(fcb_observed_reader_retained(handle, 1)).contains("\"text\":\"retained\""));
    assert_eq!(fcb_observed_reader_cancel(handle), 1);
    assert!(take(fcb_observed_reader_copy(handle, 1, 0, 8)).contains("72657461696e6564"));
    assert_eq!(fcb_observed_reader_close(handle), 1);
    assert_eq!(fcb_observed_reader_close(handle), 0);
}

fn find(handle: u64, generation: u64, text: &str, encoding: u64, hits: u64, bytes: u64) -> String {
    let text = CString::new(text).unwrap();
    take(unsafe { fcb_observed_reader_find(handle, generation, text.as_ptr(), encoding,
        hits, bytes, None, std::ptr::null_mut()) })
}

#[test]
fn c_search_retains_only_exact_witnesses_from_the_opened_file_object() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let path = root.join("source.rs");
    fs::write(&path, b"before needle after needle\n").unwrap();
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    let handle = open(&croot, b"source.rs");
    assert!(window(handle, 0, 6, 1).contains("\"observation\":\"1\""));

    let found = find(handle, 7, "needle", 1, 10, 1024);
    assert!(found.contains("\"status\":\"ok\""), "{found}");
    assert!(found.contains("\"search_observation\":\"2\""), "search and window IDs must not alias: {found}");
    assert!(found.contains("\"query_generation\":\"7\""));
    assert!(found.contains("\"retained_hits\":\"2\""));
    assert!(found.contains("\"search_complete\":true"));
    assert!(found.contains("\"whole_file_immutable\":false"));
    assert!(found.contains("\"literal_original_hex\":\"6e6565646c65\""));
    assert!(found.contains("\"retention\":\"literal-witnesses-only\""));

    // Change the opened object, then replace its pathname with unrelated bytes.
    // The accepted witness must remain exact and cannot gain invented context.
    file.write_at(b"change", 7).unwrap();
    fs::rename(&path, root.join("original.rs")).unwrap();
    fs::write(&path, b"replacement only\n").unwrap();
    let before = take(fcb_observed_reader_info(handle));
    let witness = take(fcb_observed_reader_search_hit(handle, 7, 0));
    assert!(witness.contains("\"command\":\"search-hit\""), "{witness}");
    assert!(witness.contains("\"original_hex\":\"6e6565646c65\""));
    assert!(witness.contains("\"original_range\":{\"start\":\"7\",\"end\":\"13\"}"));
    assert!(witness.contains("\"additional_source_bytes_read\":\"0\""));
    assert!(!witness.contains("before needle"), "unretained context must not be fabricated");
    assert_eq!(before, take(fcb_observed_reader_info(handle)), "witness resolution must perform no I/O");
    assert!(take(fcb_observed_reader_retained(handle, 1)).contains("\"text\":\"before\""));

    let newer = find(handle, 8, "needle", 1, 10, 1024);
    assert!(newer.contains("\"retained_hits\":\"1\""), "new scans must stay on the opened object: {newer}");
    assert!(take(fcb_observed_reader_search_hit(handle, 7, 0)).contains("\"status\":\"error\""));
    assert!(take(fcb_observed_reader_search_hit(handle, 8, 1)).contains("\"status\":\"error\""));
    assert!(take(fcb_observed_reader_search_hit(handle, 8, u64::MAX)).contains("\"status\":\"error\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
    assert!(take(fcb_observed_reader_search_hit(handle, 8, 0)).contains("OBSERVED_READER_HANDLE_UNKNOWN"));
}

#[test]
fn c_streaming_utf16_matches_cross_buffer_boundaries_in_original_byte_coordinates() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    for encoding in [2, 3] {
        let (root, croot) = root();
        let text = format!("\u{feff}{}🦀x tail", "a".repeat(8190));
        let bytes: Vec<_> = text.encode_utf16().flat_map(|unit| if encoding == 2 {
            unit.to_le_bytes()
        } else { unit.to_be_bytes() }).collect();
        fs::write(root.join("source.rs"), &bytes).unwrap();
        let handle = open(&croot, b"source.rs");
        let found = find(handle, 1, "🦀x", encoding, 10, 64 * 1024);
        assert!(found.contains("\"search_complete\":true"), "{found}");
        assert!(found.contains("\"retained_hits\":\"1\""));
        assert!(found.contains("\"original_range\":{\"start\":\"16382\",\"end\":\"16388\"}"));
        let witness = take(fcb_observed_reader_search_hit(handle, 1, 0));
        assert!(witness.contains(if encoding == 2 { "3ed880dd7800" } else { "d83edd800078" }), "{witness}");
        assert!(witness.contains("🦀x"));
        assert_eq!(fcb_observed_reader_close(handle), 1);
    }
}

#[test]
fn c_search_limits_and_invalid_foreign_inputs_cannot_relabel_old_results() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    fs::write(root.join("source.rs"), b"aaaaa\n").unwrap();
    let handle = open(&croot, b"source.rs");
    let found = find(handle, 1, "aa", 1, 10, 1024);
    assert!(found.contains("\"retained_hits\":\"4\""), "overlapping matches must remain distinct: {found}");
    let text = CString::new("aa").unwrap();
    let invalid_utf8 = [255u8, 0];
    for pointer in [std::ptr::null(), invalid_utf8.as_ptr().cast::<c_char>()] {
        assert!(take(unsafe { fcb_observed_reader_find(handle, 2, pointer, 1, 10, 1024,
            None, std::ptr::null_mut()) }).contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    }
    let oversized = CString::new("a".repeat(MAX_OBSERVED_READER_NEEDLE_BYTES + 1)).unwrap();
    assert!(take(unsafe { fcb_observed_reader_find(handle, 2, oversized.as_ptr(), 1, 10, 1024,
        None, std::ptr::null_mut()) }).contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"));
    for (generation, needle, encoding, hits, bytes) in [
        (0, "aa", 1, 10, 1024), (1, "aa", 1, 10, 1024), (2, "", 1, 10, 1024),
        (3, "aa", 0, 10, 1024), (4, "aa", 4, 10, 1024),
        (5, "aa", 1, 257, 1024), (6, "aa", 1, u64::MAX, 1024),
        (7, "aa", 1, 10, (4u64 << 30) + 1),
    ] {
        assert!(find(handle, generation, needle, encoding, hits, bytes).contains("\"status\":\"error\""));
    }
    assert!(take(unsafe { fcb_observed_reader_find(handle, 20, text.as_ptr(), 1, 10, 1024,
        Some(cancel_now), std::ptr::null_mut()) }).contains("\"exit_code\":\"130\""));
    assert!(take(fcb_observed_reader_search_hit(handle, 1, 0)).contains("\"original_hex\":\"6161\""));
    assert!(find(handle, 20, "aa", 1, 10, 1024).contains("\"status\":\"error\""),
        "a canceled attempt must not reuse its generation");

    let limited = find(handle, 21, "aa", 1, 0, 1024);
    assert!(limited.contains("\"state\":\"match-limit\""), "{limited}");
    assert!(limited.contains("\"matches_seen\":\"1\""));
    assert!(limited.contains("\"retained_hits\":\"0\""));
    assert!(limited.contains("\"search_complete\":false"));
    let absent = find(handle, 22, "absent", 1, 0, 1024);
    assert!(absent.contains("\"search_complete\":true"), "zero displayed rows must still distinguish no match: {absent}");
    assert!(absent.contains("\"matches_seen\":\"0\""));
    let partial = find(handle, 23, "aa", 1, 10, 0);
    assert!(partial.contains("\"state\":\"byte-limit\""), "{partial}");
    assert!(partial.contains("\"search_complete\":false"));
    assert!(partial.contains("\"payload_bytes_read\":\"0\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

#[test]
fn c_matching_find_preserves_lowercase_original_witnesses_and_count_only_policy() {
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64, u64, u64,
        Option<SearchCancellationCallback>, *mut c_void) -> *mut c_char = fcb_observed_reader_find_matching;
    assert!(include_str!("../include/fcb_observed_reader.h").contains("fcb_observed_reader_find_matching("));
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    for encoding in [1, 2, 3] {
        let (root, croot) = root();
        let text = "\u{feff}K K k";
        let bytes: Vec<u8> = match encoding {
            2 => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            3 => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            _ => text.as_bytes().to_vec(),
        };
        fs::write(root.join("source.rs"), &bytes).unwrap();
        let handle = open(&croot, b"source.rs");
        let needle = CString::new("k").unwrap();
        let found = take(unsafe { fcb_observed_reader_find_matching(handle, 1, needle.as_ptr(),
            encoding, 0, 1, 3, 1024, None, std::ptr::null_mut()) });
        assert!(found.contains("\"mode\":\"unicode-lowercase-text\""), "{found}");
        assert!(found.contains("\"case_sensitive\":false"));
        assert!(found.contains("\"count_all_matches\":true"));
        assert!(found.contains("\"match_count_complete\":true"));
        assert!(found.contains("\"matches_seen\":\"3\""));
        assert!(found.contains("\"literal_original_hex\":null"));
        assert!(found.contains("\"literal_sha256\":null"));
        assert_eq!(found.matches("\"witness_sha256\":").count(), 3);
        let witness = take(fcb_observed_reader_search_hit(handle, 1, 0));
        assert!(witness.contains("\"text\":\"K\""), "{witness}");
        let original = match encoding { 2 => "2a21", 3 => "212a", _ => "e284aa" };
        assert!(witness.contains(&format!("\"original_hex\":\"{original}\"")), "{witness}");
        assert!(witness.contains("\"mode\":\"unicode-lowercase-text\""));
        assert!(witness.contains("\"case_sensitive\":false"));
        let before = take(fcb_observed_reader_info(handle));
        for (case, count) in [(2, 1), (0, 2)] {
            let refused = take(unsafe { fcb_observed_reader_find_matching(handle, 2, needle.as_ptr(),
                encoding, case, count, 3, 1024, None, std::ptr::null_mut()) });
            assert!(refused.contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"), "{refused}");
        }
        assert_eq!(before, take(fcb_observed_reader_info(handle)));
        assert_eq!(witness, take(fcb_observed_reader_search_hit(handle, 1, 0)));
        let count_only = take(unsafe { fcb_observed_reader_find_matching(handle, 2, needle.as_ptr(),
            encoding, 0, 1, 0, 1024, None, std::ptr::null_mut()) });
        assert!(count_only.contains("\"retained_hits\":\"0\""), "{count_only}");
        assert!(count_only.contains("\"retained_witness_bytes\":\"0\""));
        assert!(count_only.contains("\"matches_seen\":\"3\""));
        assert!(count_only.contains("\"match_count_complete\":true"));
        assert!(count_only.contains("\"truncated\":true"));
        assert_eq!(fcb_observed_reader_close(handle), 1);
    }
}

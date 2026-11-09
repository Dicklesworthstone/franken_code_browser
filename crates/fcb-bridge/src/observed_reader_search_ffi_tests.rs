#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::{ffi::{CStr, CString}, fs::{self, OpenOptions}, os::unix::fs::FileExt,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use crate::reader_ffi::FFI_TEST_LOCK;

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a bounded response");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    text
}
fn number(json: &str, key: &str) -> u64 {
    let prefix = format!("\"{key}\":\"");
    json.split_once(prefix.as_str()).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn root() -> (std::path::PathBuf, CString) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fcb-observed-find-{}-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap();
    let cpath = CString::new(path.to_str().unwrap()).unwrap();
    (path, cpath)
}
fn open(root: &CString) -> u64 {
    let handle = fcb_observed_reader_create(); assert_ne!(handle, 0);
    let relative = CString::new("736f75726365").unwrap(); // source
    let result = take(unsafe { fcb_observed_reader_open(handle, root.as_ptr(), relative.as_ptr(), None, std::ptr::null_mut()) });
    assert!(result.contains("\"status\":\"ok\""), "{result}");
    assert!(result.contains("\"payload_bytes_read\":\"0\""));
    handle
}
fn find(handle: u64, generation: u64, needle: &str, encoding: u64, hits: u64, bytes: u64) -> String {
    let needle = CString::new(needle).unwrap();
    take(unsafe { fcb_observed_reader_find(handle, generation, needle.as_ptr(), encoding, hits, bytes,
        None, std::ptr::null_mut()) })
}
fn hit(handle: u64, generation: u64, index: u64) -> String {
    take(unsafe { fcb_observed_reader_observe_hit(handle, generation, index, 4096, 65_536,
        None, std::ptr::null_mut()) })
}

#[test]
fn c_find_beyond_complete_capture_limit_verifies_fresh_context_and_preserves_history() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let source = OpenOptions::new().create_new(true).read(true).write(true).open(root.join("source")).unwrap();
    let far = 33 * 1024 * 1024u64;
    source.set_len(far + 64).unwrap();
    source.write_at(b"needle", 65_534).unwrap(); // Crosses a streaming read boundary.
    source.write_at(b"needle", far).unwrap();
    let handle = open(&croot);
    let report = find(handle, 1, "needle", 1, 256, 64 * 1024 * 1024);
    assert!(report.contains("\"search_complete\":true"), "{report}");
    assert!(report.contains("\"whole_file_immutable\":false"));
    assert_eq!(number(&report, "retained_hits"), 2);
    assert_eq!(number(&report, "matches_seen"), 2);
    assert!(report.contains(&format!("\"original_range\":{{\"start\":\"{far}\",\"end\":\"{}\"}}", far + 6)));
    let search_observation = number(&report, "search_observation");
    let selected = hit(handle, 1, 1);
    assert!(selected.contains("\"witness_verified\":true"), "{selected}");
    assert!(selected.contains("\"verification\":\"equal-original-bytes-in-new-observation\""));
    let observation = number(&selected, "observation");
    assert!(observation > search_observation, "fresh context must have a new identity");
    source.write_at(b"change", far).unwrap();
    let refused = hit(handle, 1, 1);
    assert!(refused.contains("OBSERVED_READER_SEARCH_HIT_CHANGED"), "{refused}");
    let literal = take(fcb_observed_reader_search_hit(handle, 1, 1));
    assert!(literal.contains("\"original_hex\":\"6e6565646c65\""), "{literal}");
    assert!(literal.contains("\"additional_source_bytes_read\":\"0\""));
    let retained = take(fcb_observed_reader_retained(handle, observation));
    assert!(retained.contains("\"additional_source_bytes_read\":\"0\""));
    assert!(retained.contains("\"witness_verified\":true"));
    let copy = take(fcb_observed_reader_copy(handle, observation, far, far + 6));
    assert!(copy.contains("\"original_hex\":\"6e6565646c65\""), "{copy}");
    // A later search does not revoke an already retained observation's evidence.
    assert!(find(handle, 2, "change", 1, 256, 64 * 1024 * 1024).contains("\"retained_hits\":\"1\""));
    assert!(take(fcb_observed_reader_retained(handle, observation)).contains("\"query_generation\":\"1\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

#[test]
fn c_find_reports_partial_limits_decoding_failure_and_original_utf16_offsets() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let bytes: Vec<u8> = "\u{feff}A🦀B🦀C".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(root.join("source"), &bytes).unwrap();
    let handle = open(&croot);
    let report = find(handle, 1, "🦀", 2, 256, 1024);
    assert!(report.contains("\"search_complete\":true"), "{report}");
    assert!(report.contains("\"literal_original_hex\":\"3ed880dd\""));
    assert!(report.contains("\"original_range\":{\"start\":\"4\",\"end\":\"8\"}"));
    let selected = hit(handle, 1, 0);
    assert!(selected.contains("\"witness_verified\":true"), "{selected}");
    let limited = find(handle, 2, "🦀", 2, 1, 1024);
    assert!(limited.contains("\"state\":\"match-limit\""), "{limited}");
    assert!(limited.contains("\"search_complete\":false"));
    assert_eq!(number(&limited, "matches_seen"), 2);
    assert_eq!(number(&limited, "retained_hits"), 1);
    let partial = find(handle, 3, "C", 2, 256, 8);
    assert!(partial.contains("\"state\":\"byte-limit\""), "{partial}");
    assert!(partial.contains("\"search_complete\":false"));
    let wrong_encoding = find(handle, 4, "C", 1, 256, 1024);
    assert!(wrong_encoding.contains("\"state\":\"unsupported-text\""), "{wrong_encoding}");
    assert!(wrong_encoding.contains("\"search_complete\":false"));
    assert_eq!(number(&wrong_encoding, "retained_hits"), 0);
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

unsafe extern "C" fn cancel_now(_: *mut c_void) -> i32 { 1 }

#[test]
fn c_find_rejects_invalid_inputs_stale_generations_and_canceled_delivery() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    fs::write(root.join("source"), b"exact source\n").unwrap();
    let handle = open(&croot);
    assert!(find(handle, 1, "source", 1, 256, 1024).contains("\"search_complete\":true"));
    let needle = CString::new("source").unwrap();
    let canceled = take(unsafe { fcb_observed_reader_find(handle, 2, needle.as_ptr(), 1, 256, 1024,
        Some(cancel_now), std::ptr::null_mut()) });
    assert!(canceled.contains("\"exit_code\":\"130\""), "{canceled}");
    assert!(hit(handle, 1, 0).contains("\"witness_verified\":true"));
    assert!(hit(handle, 99, 0).contains("\"status\":\"error\""));
    assert!(hit(handle, 1, 1).contains("\"status\":\"error\""));
    for (generation, encoding, hits, bytes) in [(0, 1, 1, 1), (1, 1, 1, 1024),
        (3, 0, 1, 1024), (3, 1, 257, 1024), (4, 1, 1, u64::MAX)] {
        assert!(find(handle, generation, "source", encoding, hits, bytes).contains("\"status\":\"error\""));
    }
    assert!(find(handle, 20, "", 1, 1, 1024).contains("\"status\":\"error\""));
    let overlong = CString::new("x".repeat(MAX_OBSERVED_READER_NEEDLE_BYTES + 1)).unwrap();
    let invalid_utf8 = [255u8, 0];
    for pointer in [std::ptr::null(), overlong.as_ptr(), invalid_utf8.as_ptr().cast()] {
        let result = take(unsafe { fcb_observed_reader_find(handle, 30, pointer, 1, 1, 1024,
            None, std::ptr::null_mut()) });
        assert!(result.contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"), "{result}");
    }
    assert_eq!(fcb_observed_reader_close(handle), 1);
    assert!(hit(handle, 1, 0).contains("OBSERVED_READER_HANDLE_UNKNOWN"));
}

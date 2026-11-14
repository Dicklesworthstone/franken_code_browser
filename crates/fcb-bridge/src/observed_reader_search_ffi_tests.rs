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
fn find_counted(handle: u64, generation: u64, needle: &str, encoding: u64, hits: u64, bytes: u64) -> String {
    let needle = CString::new(needle).unwrap();
    take(unsafe { fcb_observed_reader_find_counted(handle, generation, needle.as_ptr(), encoding, hits, bytes,
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


#[test]
fn c_counted_find_continues_after_retention_fills_and_legacy_find_does_not() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    fs::write(root.join("source"), b"banana banana").unwrap();
    let handle = open(&croot);
    let legacy = find(handle, 1, "ana", 1, 1, 64);
    assert!(legacy.contains("\"state\":\"match-limit\""), "{legacy}");
    assert_eq!(number(&legacy, "matches_seen"), 2);
    assert!(legacy.contains("\"count_all_matches\":false"));

    let counted = find_counted(handle, 2, "ana", 1, 1, 64);
    assert!(counted.contains("\"state\":\"complete-observed-input\""), "{counted}");
    assert!(counted.contains("\"search_complete\":true"));
    assert!(counted.contains("\"count_all_matches\":true"));
    assert!(counted.contains("\"match_count_complete\":true"));
    assert!(counted.contains("\"truncated\":true"));
    assert_eq!(number(&counted, "matches_seen"), 4);
    assert_eq!(number(&counted, "retained_hits"), 1);
    let literal = take(fcb_observed_reader_search_hit(handle, 2, 0));
    assert!(literal.contains("\"count_all_matches\":true"), "{literal}");
    assert!(literal.contains("\"match_count_complete\":true"), "{literal}");
    assert!(literal.contains("\"original_hex\":\"616e61\""), "{literal}");
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

#[test]
fn c_counted_find_keeps_budget_and_cancellation_semantics() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    fs::write(root.join("source"), b"banana banana").unwrap();
    let handle = open(&croot);
    let partial = find_counted(handle, 1, "ana", 1, 1, 6);
    assert!(partial.contains("\"state\":\"byte-limit\""), "{partial}");
    assert!(partial.contains("\"match_count_complete\":false"));
    assert_eq!(number(&partial, "matches_seen"), 2);
    let needle = CString::new("ana").unwrap();
    let canceled = take(unsafe { fcb_observed_reader_find_counted(handle, 2, needle.as_ptr(), 1, 1, 64,
        Some(cancel_now), std::ptr::null_mut()) });
    assert!(canceled.contains("\"exit_code\":\"130\""), "{canceled}");
    assert!(take(fcb_observed_reader_search_hit(handle, 1, 0)).contains("\"original_hex\":\"616e61\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

fn find_mode(handle: u64, generation: u64, needle: &str, encoding: u64, mode: u64,
    case_sensitive: u64, count_all_matches: u64, hits: u64, bytes: u64) -> String {
    let needle = CString::new(needle).unwrap();
    take(unsafe { fcb_observed_reader_find_mode(handle, generation, needle.as_ptr(), encoding,
        mode, case_sensitive, count_all_matches, hits, bytes, None, std::ptr::null_mut()) })
}

#[test]
fn c_occurrence_mode_distinguishes_tokens_from_literal_hits_and_counts_without_rows() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    fs::write(root.join("source"), b"cat bobcat cat2 _cat $cat cat$ // cat\n\"cat\" Cat\n").unwrap();
    let handle = open(&croot);
    let literal = find(handle, 1, "cat", 1, 16, 1024);
    assert_eq!(number(&literal, "matches_seen"), 8);
    assert!(literal.contains("\"mode\":\"exact-decoded-text\""));
    let tokens = find_mode(handle, 2, "cat", 1, 1, 1, 0, 16, 1024);
    for expected in ["\"mode\":\"whole-token-occurrences\"", "\"requested_mode\":\"occurrences\"",
        "\"realized_mode\":\"whole-token-occurrences\"", "\"case_sensitive\":true",
        "\"evidence_level\":\"whole-token-text-candidate\"", "\"includes_comments_and_literals\":true",
        "\"compiler_resolved\":false", "\"semantic_complete\":false", "\"search_complete\":true"] {
        assert!(tokens.contains(expected), "missing {expected}: {tokens}");
    }
    assert_eq!(number(&tokens, "matches_seen"), 3);
    assert_eq!(number(&tokens, "retained_hits"), 3);
    let witness = take(fcb_observed_reader_search_hit(handle, 2, 0));
    assert!(witness.contains("\"original_hex\":\"636174\""));
    assert!(witness.contains("\"mode\":\"whole-token-occurrences\""));
    let current = hit(handle, 2, 0);
    assert!(current.contains("\"token_boundaries_verified\":true"), "{current}");
    let counted = find_mode(handle, 3, "cat", 1, 1, 1, 1, 0, 1024);
    assert_eq!(number(&counted, "matches_seen"), 3);
    assert_eq!(number(&counted, "retained_hits"), 0);
    assert!(counted.contains("\"match_count_complete\":true"));
    assert!(counted.contains("\"truncated\":true"));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

#[test]
fn c_occurrence_mode_keeps_utf16_witnesses_but_refuses_changed_neighbor_boundaries() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let bytes: Vec<u8> = "\u{feff}cat catx cat".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(root.join("source"), &bytes).unwrap();
    let handle = open(&croot);
    let tokens = find_mode(handle, 1, "cat", 2, 1, 1, 0, 16, 1024);
    assert_eq!(number(&tokens, "matches_seen"), 2);
    assert!(tokens.contains("\"original_range\":{\"start\":\"2\",\"end\":\"8\"}"));
    assert!(tokens.contains("\"original_range\":{\"start\":\"20\",\"end\":\"26\"}"));
    let current = hit(handle, 1, 0);
    assert!(current.contains("\"token_boundaries_verified\":true"), "{current}");
    let observation = number(&current, "observation");
    let file = OpenOptions::new().write(true).open(root.join("source")).unwrap();
    file.write_at(&[b'x', 0], 8).unwrap();
    assert!(hit(handle, 1, 0).contains("OBSERVED_READER_SEARCH_HIT_CHANGED"));
    let old = take(fcb_observed_reader_search_hit(handle, 1, 0));
    assert!(old.contains("\"original_hex\":\"630061007400\""));
    assert!(old.contains("\"additional_source_bytes_read\":\"0\""));
    // A later literal query does not rewrite the mode frozen in history.
    assert_eq!(number(&find(handle, 2, "cat", 2, 16, 1024), "matches_seen"), 3);
    let replay = take(fcb_observed_reader_retained(handle, observation));
    assert!(replay.contains("\"mode\":\"whole-token-occurrences\""));
    assert!(replay.contains("\"token_boundaries_verified\":true"));
    assert!(replay.contains("\"query_generation\":\"1\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

#[test]
fn c_occurrence_mode_rejects_unknown_flags_invalid_names_and_canceled_replacements() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    fs::write(root.join("source"), b"foo foobar Foo").unwrap();
    let handle = open(&croot);
    assert_eq!(number(&find_mode(handle, 1, "foo", 1, 1, 1, 0, 16, 1024), "matches_seen"), 1);
    for (mode, case_sensitive, count_all) in [(2, 1, 0), (u64::MAX, 1, 0), (1, 2, 0), (1, 1, 2)] {
        let result = find_mode(handle, 2, "foo", 1, mode, case_sensitive, count_all, 16, 1024);
        assert!(result.contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"), "{result}");
    }
    let incompatible = find_mode(handle, 2, "foo", 1, 1, 0, 0, 16, 1024);
    assert!(incompatible.contains("OBSERVED_READER_INVALID_LIMITS"), "{incompatible}");
    for (generation, name) in [(3, "foo bar"), (4, "1foo"), (5, "foo-bar")] {
        let result = find_mode(handle, generation, name, 1, 1, 1, 0, 16, 1024);
        assert!(result.contains("STREAM_READ_INVALID_IDENTIFIER"), "{result}");
    }
    let needle = CString::new("foo").unwrap();
    let canceled = take(unsafe { fcb_observed_reader_find_mode(handle, 6, needle.as_ptr(),
        1, 1, 1, 0, 16, 1024, Some(cancel_now), std::ptr::null_mut()) });
    assert!(canceled.contains("\"exit_code\":\"130\""), "{canceled}");
    assert!(take(fcb_observed_reader_search_hit(handle, 1, 0)).contains("\"text\":\"foo\""));
    let lower = find_mode(handle, 7, "foo", 1, 0, 0, 1, 16, 1024);
    assert_eq!(number(&lower, "matches_seen"), 3);
    assert!(lower.contains("\"mode\":\"unicode-lowercase-text\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

fn find_page(handle: u64, generation: u64, needle: &str, encoding: u64, mode: u64,
    case_sensitive: u64, count_all_matches: u64, start_after: u64, hits: u64, bytes: u64) -> String {
    let needle = CString::new(needle).unwrap();
    take(unsafe { fcb_observed_reader_find_page(handle, generation, needle.as_ptr(), encoding,
        mode, case_sensitive, count_all_matches, start_after, hits, bytes, None, std::ptr::null_mut()) })
}

#[test]
fn c_find_page_reaches_later_occurrences_and_retains_page_provenance_in_history() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    fs::write(root.join("source"), b"cat ".repeat(300)).unwrap();
    let handle = open(&croot);
    let first = find_mode(handle, 1, "cat", 1, 1, 1, 0, 256, 4096);
    assert_eq!(number(&first, "page_start_after"), 0);
    assert_eq!(number(&first, "skipped_matches"), 0);
    assert!(first.contains("\"page_has_more_matches\":true"));
    let later = find_page(handle, 2, "cat", 1, 1, 1, 1, 256, 256, 4096);
    assert!(later.contains("\"search_complete\":true"), "{later}");
    assert!(later.contains("\"mode\":\"whole-token-occurrences\""));
    assert!(later.contains("\"count_all_matches\":true"));
    assert!(later.contains("\"truncated\":true"));
    assert!(later.contains("\"page_has_more_matches\":false"));
    assert_eq!(number(&later, "page_start_after"), 256);
    assert_eq!(number(&later, "skipped_matches"), 256);
    assert_eq!(number(&later, "matches_seen"), 300);
    assert_eq!(number(&later, "retained_hits"), 44);
    assert_eq!(number(&later, "occurrence_id"), 257);
    assert!(number(&later, "search_observation") > number(&first, "search_observation"));

    let literal = take(fcb_observed_reader_search_hit(handle, 2, 43));
    assert_eq!(number(&literal, "page_start_after"), 256);
    assert_eq!(number(&literal, "occurrence_id"), 300);
    assert!(literal.contains("\"original_hex\":\"636174\""));
    assert!(literal.contains("\"additional_source_bytes_read\":\"0\""));
    let selected = hit(handle, 2, 43);
    let observation = number(&selected, "observation");
    assert_eq!(number(&selected, "page_start_after"), 256);
    assert_eq!(number(&selected, "hit_index"), 43);
    assert_eq!(number(&selected, "occurrence_id"), 300);
    assert!(selected.contains("\"token_boundaries_verified\":true"));
    assert_eq!(number(&find_page(handle, 3, "cat", 1, 0, 1, 0, 0, 1, 4096), "page_start_after"), 0);
    let replay = take(fcb_observed_reader_retained(handle, observation));
    assert_eq!(number(&replay, "page_start_after"), 256);
    assert_eq!(number(&replay, "query_generation"), 2);
    assert_eq!(number(&replay, "occurrence_id"), 300);
    assert!(replay.contains("\"additional_source_bytes_read\":\"0\""));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

#[test]
fn c_find_page_preserves_limits_lowercase_utf16_and_canceled_accepted_results() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let (root, croot) = root();
    let bytes: Vec<u8> = "\u{feff}K K k".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(root.join("source"), &bytes).unwrap();
    let handle = open(&croot);
    let lower = find_page(handle, 1, "k", 2, 0, 0, 1, 1, 1, 1024);
    assert!(lower.contains("\"mode\":\"unicode-lowercase-text\""), "{lower}");
    assert_eq!(number(&lower, "page_start_after"), 1);
    assert_eq!(number(&lower, "matches_seen"), 3);
    assert_eq!(number(&lower, "retained_hits"), 1);
    assert!(lower.contains("\"page_has_more_matches\":true"));
    let witness = take(fcb_observed_reader_search_hit(handle, 1, 0));
    assert!(witness.contains("\"original_hex\":\"4b00\""));
    assert_eq!(number(&witness, "occurrence_id"), 2);
    for (mode, case_sensitive, count_all) in [(2, 1, 0), (0, 2, 0), (0, 1, 2)] {
        let invalid = find_page(handle, 2, "k", 2, mode, case_sensitive, count_all, 2, 1, 1024);
        assert!(invalid.contains("OBSERVED_READER_HANDLE_INVALID_ARGUMENT"), "{invalid}");
    }
    let needle = CString::new("k").unwrap();
    let canceled = take(unsafe { fcb_observed_reader_find_page(handle, 2, needle.as_ptr(),
        2, 0, 0, 1, 2, 1, 1024, Some(cancel_now), std::ptr::null_mut()) });
    assert!(canceled.contains("\"exit_code\":\"130\""), "{canceled}");
    assert_eq!(take(fcb_observed_reader_search_hit(handle, 1, 0)), witness);

    let limited = find_page(handle, 3, "k", 2, 0, 0, 1, 2, 1, 4);
    assert!(limited.contains("\"state\":\"byte-limit\""), "{limited}");
    assert_eq!(number(&limited, "payload_bytes_read"), 4);
    assert_eq!(number(&limited, "skipped_matches"), 1);
    assert_eq!(number(&limited, "retained_hits"), 0);
    assert!(limited.contains("\"page_has_more_matches\":false"));
    assert!(limited.contains("\"match_count_complete\":false"));
    let beyond = find_page(handle, 4, "k", 2, 0, 0, 1, u64::MAX, 256, 1024);
    assert_eq!(number(&beyond, "page_start_after"), u64::MAX);
    assert_eq!(number(&beyond, "skipped_matches"), 3);
    assert_eq!(number(&beyond, "retained_hits"), 0);
    assert!(beyond.contains("\"search_complete\":true"));
    assert!(beyond.contains("\"page_has_more_matches\":false"));
    assert_eq!(fcb_observed_reader_close(handle), 1);
}

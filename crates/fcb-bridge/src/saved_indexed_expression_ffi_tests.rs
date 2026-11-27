#![deny(unsafe_op_in_unsafe_fn)]

// Exercise the actual C -> retained saved host -> FCBD -> expression verifier.
use super::*;
use super::index_build::fcb_saved_build_index;
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}};
use std::{ffi::{CStr, CString}, io::{Seek, SeekFrom, Write}, path::{Path, PathBuf}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected an owned response");
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; result
}
fn value<'a>(text: &'a str, key: &str) -> &'a str {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap()
}
fn number(text: &str, key: &str) -> u64 { value(text, key).parse().unwrap() }
fn foreign(path: &Path) -> CString { CString::new(path.to_str().unwrap()).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn fixture(complete: bool, entries: &[SnapshotEntry<'_>]) -> (PathBuf, u64) {
    let root = std::env::temp_dir().join(format!("fcb-indexed-expression-c-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(88791).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let mut ordered = entries.to_vec(); ordered.sort_by(|a, b| a.path.cmp(b.path));
    let encoded = SnapshotBytes::encode(owner, complete, "indexed-expression-c", &ordered,
        Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = root.join("source.fcbs"); std::fs::write(&path, encoded.bytes()).unwrap();
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    let opened = take(unsafe { fcb_saved_open(handle, foreign(&path).as_ptr()) });
    assert!(!opened.contains("\"status\":\"error\""), "{opened}");
    (root, handle)
}
fn attach(handle: u64, root: &Path, generation: u64, covered: bool) -> (PathBuf, String) {
    let path = root.join(format!("index-{generation}.fcbd"));
    let (per_file, total, postings) = if covered { (262_144, 67_108_864, 2_097_152) } else { (0, 0, 0) };
    let built = take(unsafe { fcb_saved_build_index(handle, generation, foreign(&path).as_ptr(),
        per_file, total, postings) });
    assert!(built.contains("\"attach_eligible\":true"), "{built}");
    let pin = value(&built, "index_pin").to_owned();
    let response = take(unsafe { fcb_saved_attach_index(handle, generation, foreign(&path).as_ptr(),
        CString::new(pin.clone()).unwrap().as_ptr()) });
    assert!(response.contains("\"status\":\"ok\""), "{response}");
    (path, pin)
}
#[allow(clippy::too_many_arguments)]
fn query(handle: u64, generation: u64, expression: &str, case: bool,
    maximum: u64, bytes: u64, skipped: u64, kind: u64, path: &[u8]) -> String {
    let expression = CString::new(expression).unwrap();
    take(unsafe { fcb_saved_expression_search_scoped_page(handle, generation,
        expression.as_ptr(), u64::from(case), 1, maximum, bytes, skipped, kind,
        if path.is_empty() { std::ptr::null() } else { path.as_ptr() }, path.len() as u64) })
}
fn indexed(text: &str, generation: u64, pin: &str) {
    assert!(text.contains("\"index_used\":true"), "{text}");
    assert_eq!(number(text, "index_generation"), generation);
    assert_eq!(value(text, "index_pin"), pin);
}
fn closed(text: &str) {
    assert!(text.contains("\"complete\":true"), "{text}");
    assert!(text.contains("\"matches_seen_exact\":true"), "{text}");
}

#[test]
fn index_prunes_source_work_but_preserves_scope_and_whole_member_predicates() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let good = b"needle required needle";
    let forbidden = b"needle required forbidden";
    let missing_required = b"needle lacking";
    let (root, handle) = fixture(true, &[
        entry(b"elsewhere/a.rs", b"needle required"),
        entry(b"src/aa.rs", b"absent required"),
        entry(b"src/ab.rs", b"unrelated required"),
        entry(b"src/b.rs", good), entry(b"src/c.rs", forbidden),
        entry(b"src/d.rs", missing_required), entry(b"src/e.py", b"needle required"),
        SnapshotEntry { path: b"src/lost.py", observed_bytes: 50, data: SnapshotData::Unavailable("not captured") },
        entry(b"src-other/a.rs", b"needle required"),
    ]);
    let text = "needle required -forbidden lang:rust";
    let direct = query(handle, 1, text, true, 8, 4096, 0, 1, b"src");
    closed(&direct); assert_eq!(number(&direct, "matches_seen"), 2);
    let (_, pin) = attach(handle, &root, 1, true);
    let result = query(handle, 2, text, true, 8, 4096, 0, 1, b"src");
    indexed(&result, 1, &pin); closed(&result);
    assert_eq!(number(&result, "scope_known_members"), 7);
    assert_eq!(number(&result, "metadata_examined"), 7);
    assert_eq!(number(&result, "metadata_excluded"), 2);
    assert_eq!(number(&result, "scope_files"), 5);
    assert_eq!(number(&result, "unavailable_files"), 0);
    assert_eq!(number(&result, "index_eliminated_files"), 2);
    assert_eq!(number(&result, "members_loaded"), 3);
    assert_eq!(number(&result, "predicate_rejections"), 2);
    assert_eq!(number(&result, "matches_seen"), 2);
    assert_eq!(number(&result, "retained_hits"), 2);
    assert_eq!(number(&result, "member_bytes_read"),
        (good.len() + forbidden.len() + missing_required.len()) as u64);
    assert!(number(&result, "member_bytes_read") < number(&direct, "member_bytes_read"));
    assert!(result.contains("\"posting_cursor_complete\":true"));
    assert_eq!(value(&result, "scope_path_hex"), "737263");
    assert_eq!(fcb_saved_close(handle), 1);
}

#[test]
fn indexed_absence_needs_no_source_budget_but_still_honors_membership_coverage() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (root, handle) = fixture(false, &[
        entry(b"src/a.rs", b"absent required"),
        SnapshotEntry { path: b"unavailable.rs", observed_bytes: 8, data: SnapshotData::Unavailable("denied") },
    ]);
    let (_, pin) = attach(handle, &root, 1, true);
    let selected = query(handle, 1, "needle required", true, 8, 0, 0, 2, b"src/a.rs");
    indexed(&selected, 1, &pin); closed(&selected);
    assert!(selected.contains("\"discovery_complete\":false"));
    assert_eq!(number(&selected, "matches_seen"), 0);
    assert_eq!(number(&selected, "member_bytes_read"), 0);
    assert_eq!(number(&selected, "index_eliminated_files"), 1);
    let directory = query(handle, 2, "needle required", true, 8, 0, 0, 1, b"src");
    assert!(directory.contains("\"complete\":false"), "{directory}");
    assert!(directory.contains("\"scope_membership_complete\":false"));
    let missing = query(handle, 3, "needle required", true, 8, 0, 0, 2, b"src/unknown.rs");
    assert!(missing.contains("\"complete\":false"), "{missing}");
    assert_eq!(number(&missing, "index_eliminated_files"), 0);
    assert_eq!(number(&missing, "scope_known_members"), 0);
    assert_eq!(fcb_saved_close(handle), 1);
}

#[test]
fn fallback_keeps_short_unicode_and_uncovered_expression_matches() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let utf16: Vec<_> = "\u{feff}K REQUIRED".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let (root, handle) = fixture(true, &[
        entry(b"src/\xff.rs", &utf16), entry(b"src/a.rs", b"K required"),
        entry(b"src/b.rs", b"k required forbidden"), entry(b"src/c.rs", b"NEEDLE REQUIRED"),
    ]);
    let (_, pin) = attach(handle, &root, 1, true);
    let short = query(handle, 1, "k required -forbidden", false, 8, 4096, 0, 1, b"src");
    indexed(&short, 1, &pin); closed(&short);
    assert_eq!(number(&short, "matches_seen"), 2);
    assert_eq!(number(&short, "fallback_files"), 4);
    assert_eq!(number(&short, "index_eliminated_files"), 0);
    assert_eq!(value(&short, "index_policy"), "ascii-case-variants-with-source-fallback");
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_saved_open_expression_hit_reader(handle, reader, 1, 2));
    indexed(&opened, 1, &pin);
    assert!(opened.contains("\"text\":\"K\""), "{opened}");
    assert!(opened.contains("\"original_hex\":\"212a\""), "{opened}");
    assert!(opened.contains("\"path_hex\":\"7372632fff2e7273\""), "{opened}");
    let (_, uncovered_pin) = attach(handle, &root, 2, false);
    let uncovered = query(handle, 2, "needle required -forbidden", false, 8, 4096, 0, 1, b"src");
    indexed(&uncovered, 2, &uncovered_pin); closed(&uncovered);
    assert_eq!(number(&uncovered, "matches_seen"), 1);
    assert_eq!(number(&uncovered, "fallback_files"), 4);
    assert_eq!(number(&uncovered, "index_eliminated_files"), 0);
    assert_eq!(fcb_saved_close(handle), 1);
    assert!(take(crate::reader_ffi::fcb_reader_copy_range(reader, 2, 4))
        .contains("\"original_hex\":\"212a\""));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
}

#[test]
fn indexed_windows_and_readers_keep_query_time_index_after_detachment() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (root, handle) = fixture(true, &[
        entry(b"src/a.rs", b"needle required needle"), entry(b"src/negative.rs", b"absent required"),
    ]);
    let (_, pin) = attach(handle, &root, 1, true);
    let result = query(handle, 1, "needle required", true, 1, 4096, 1, 1, b"src");
    indexed(&result, 1, &pin); closed(&result);
    assert_eq!(number(&result, "matches_seen"), 2);
    assert_eq!(number(&result, "retained_hits"), 1);
    assert_eq!(number(&result, "skipped_matches"), 1);
    assert!(result.contains("\"start\":\"16\",\"end\":\"22\""), "{result}");
    let retained_pointer = fcb_saved_expression_results(handle, 1, 0, 8);
    let before = take(fcb_saved_info(handle));
    take(fcb_saved_detach_index(handle, 2));
    let page = take(fcb_saved_expression_results(handle, 1, 0, 8));
    indexed(&page, 1, &pin);
    let after = take(fcb_saved_info(handle));
    assert_eq!(number(&before, "member_bytes_read"), number(&after, "member_bytes_read"));
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let opened = take(fcb_saved_open_expression_hit_reader(handle, reader, 1, 1));
    indexed(&opened, 1, &pin);
    assert!(opened.contains("\"original_hex\":\"6e6565646c65\""));
    let fresh = query(handle, 2, "needle required", true, 1, 4096, 0, 1, b"src");
    closed(&fresh); assert!(fresh.contains("\"index_used\":false"));
    assert!(fresh.contains("\"index_generation\":null"));
    assert_eq!(number(&fresh, "matches_seen"), 2);
    assert_eq!(number(&fresh, "index_eliminated_files"), 0);
    assert_eq!(fcb_saved_close(handle), 1);
    indexed(&take(retained_pointer), 1, &pin);
    assert!(take(crate::reader_ffi::fcb_reader_copy_range(reader, 16, 22))
        .contains("\"original_hex\":\"6e6565646c65\""));
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
}

#[test]
fn corrupt_posting_pages_fail_without_replacing_an_accepted_expression() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (root, handle) = fixture(true, &[
        entry(b"src/a.rs", b"needle required"), entry(b"src/b.rs", b"absent required"),
    ]);
    let direct = query(handle, 1, "needle required", true, 8, 4096, 0, 0, b"");
    closed(&direct);
    let (path, _) = attach(handle, &root, 1, true);
    // Attachment admits the pinned metadata, not unread posting payload pages.
    let encoded = std::fs::read(&path).unwrap();
    let manifest = u64::from_le_bytes(encoded[8..16].try_into().unwrap());
    let payload = 32 + manifest;
    let at = usize::try_from(payload).unwrap(); assert!(at < encoded.len());
    let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    writer.seek(SeekFrom::Start(payload)).unwrap();
    writer.write_all(&[encoded[at] ^ 0x80]).unwrap(); writer.flush().unwrap();
    let failed = query(handle, 2, "needle required", true, 8, 4096, 0, 0, b"");
    assert!(failed.contains("\"status\":\"error\""), "{failed}");
    assert!(failed.contains("PAGED_INDEX_CHANGED_PAGE"), "{failed}");
    let retained = take(fcb_saved_expression_results(handle, 1, 0, 8));
    assert!(retained.contains("\"index_used\":false"));
    assert_eq!(number(&retained, "retained_hits"), 1);
    let info = take(fcb_saved_info(handle));
    assert_eq!(number(&info, "last_expression_attempt"), 2);
    assert_eq!(number(&info, "accepted_expression_generation"), 1);
    take(fcb_saved_detach_index(handle, 2));
    closed(&query(handle, 3, "needle required", true, 8, 4096, 0, 0, b""));
    assert_eq!(fcb_saved_close(handle), 1);
}

#[test]
fn canceled_indexed_replacement_preserves_the_accepted_receipt() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (root, handle) = fixture(true, &[entry(b"src/a.rs", b"needle required needle")]);
    let (_, pin) = attach(handle, &root, 1, true);
    closed(&query(handle, 1, "needle required", true, 8, 4096, 0, 1, b"src"));
    let sessions = SAVED.get().unwrap(); let mut signaled = false;
    let failed = sessions.execute(handle, Command::ExpressionSearchScopedPage {
        generation: 2, query: "needle required", max_hits: 1, max_scan_bytes: 4096,
        case_sensitive: true, count_all_matches: true, skip_matches: 1,
        scope: SnapshotScope::Directory(b"src"),
    }, || { if !signaled { sessions.cancel(handle).unwrap(); signaled = true; } false });
    assert!(failed.is_err()); assert!(signaled);
    let retained = take(fcb_saved_expression_results(handle, 1, 0, 8));
    indexed(&retained, 1, &pin); closed(&retained);
    assert_eq!(number(&retained, "matches_seen"), 2);
    assert_eq!(value(&retained, "scope_path_hex"), "737263");
    assert_eq!(fcb_saved_close(handle), 1);
}

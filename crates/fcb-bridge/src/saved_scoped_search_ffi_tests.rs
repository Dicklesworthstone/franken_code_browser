#![deny(unsafe_op_in_unsafe_fn)]

// These exercise the real C -> retained host -> archive engine route.
use super::*;
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}};
use std::{ffi::{CStr, CString}, io::{Seek, SeekFrom, Write}, path::PathBuf};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn field(text: &str, name: &str) -> u64 {
    text.split(&format!("\"{name}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn fixture(complete: bool, entries: &[SnapshotEntry<'_>]) -> (PathBuf, CString) {
    let root = std::env::temp_dir().join(format!("fcb-saved-scope-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("saved.fcbs");
    let owner = ArenaOwnerId::new(88762).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let encoded = SnapshotBytes::encode(owner, complete, "scope-c", entries, Default::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    std::fs::write(&path, encoded.bytes()).unwrap();
    let foreign = CString::new(path.to_str().unwrap()).unwrap();
    (path, foreign)
}
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn scope(text: &str, name: &str, path_hex: &str, known: u64, complete: bool) {
    assert!(text.contains(&format!("\"scope\":\"{name}\"")), "{text}");
    assert!(text.contains(&format!("\"scope_path_hex\":\"{path_hex}\"")), "{text}");
    assert_eq!(field(text, "scope_known_members"), known);
    assert!(text.contains(&format!("\"scope_membership_complete\":{complete}")), "{text}");
}

#[test]
fn scoped_search_signatures_match_the_public_c_contract() {
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64, u64, u64,
        *const u8, u64) -> *mut c_char = fcb_saved_search_scoped_page;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64, u64, u64, u64, u64, u64,
        *const u8, u64) -> *mut c_char = fcb_saved_expression_search_scoped_page;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["fcb_saved_search_scoped_page(", "fcb_saved_expression_search_scoped_page("] {
        assert_eq!(header.matches(name).count(), 1, "missing or duplicate {name}");
    }
}

#[test]
fn scoped_windows_count_only_members_and_keep_raw_paths_and_utf16_reader_proof() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let utf16: Vec<_> = "\u{feff}needle required".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let raw_path = b"src/\xff.rs";
    let (_, path) = fixture(true, &[
        entry(b"outside.rs", b"needle required"),
        entry(b"src-old/near.rs", b"needle required"),
        entry(b"src/a.rs", b"NEEDLE required needle"),
        entry(raw_path, &utf16),
    ]);
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    assert!(take(unsafe { fcb_saved_open(handle, path.as_ptr()) }).contains("\"status\":\"ok\""));
    let needle = CString::new("needle").unwrap();
    let expression = CString::new("needle required").unwrap();
    let mut directory = b"src".to_vec();
    let first = take(unsafe { fcb_saved_search_scoped_page(handle, 1, needle.as_ptr(), 0, 1,
        1, 1, 1, directory.as_ptr(), directory.len() as u64) });
    assert!(first.contains("\"status\":\"ok\""), "{first}");
    scope(&first, "saved-directory", "737263", 2, true);
    assert_eq!(field(&first, "matches_seen"), 3);
    assert_eq!(field(&first, "retained_hits"), 1);
    assert_eq!(field(&first, "skipped_matches"), 1);
    assert_eq!(field(&first, "next_match_offset"), 2);
    // Foreign path bytes may be released/changed as soon as the call returns.
    directory.fill(b'x');
    let retained = take(fcb_saved_results(handle, 1, 0, 10));
    scope(&retained, "saved-directory", "737263", 2, true);
    let last = take(unsafe { fcb_saved_search_scoped_page(handle, 2, needle.as_ptr(), 0, 1,
        1, 2, 1, b"src".as_ptr(), 3) });
    assert_eq!(field(&last, "matches_seen"), 3);
    assert!(last.contains("\"has_more_matches\":false"), "{last}");
    assert!(last.contains("\"path_hex\":\"7372632fff2e7273\""), "{last}");
    assert!(take(fcb_saved_results(handle, 1, 0, 10)).contains("\"status\":\"error\""));
    let literal_reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(literal_reader, 0);
    let opened = take(fcb_saved_open_hit_reader(handle, literal_reader, 2, 1));
    scope(&opened, "saved-directory", "737263", 2, true);
    assert!(opened.contains("\"start\":\"2\",\"end\":\"14\""), "{opened}");
    assert!(opened.contains("\"original_hex\":\"6e006500650064006c006500\""), "{opened}");

    let searched = take(unsafe { fcb_saved_expression_search_scoped_page(handle, 1,
        expression.as_ptr(), 0, 1, 1, 4096, 0, 2, raw_path.as_ptr(), raw_path.len() as u64) });
    scope(&searched, "saved-member", "7372632fff2e7273", 1, true);
    assert_eq!(field(&searched, "matches_seen"), 1);
    let page = take(fcb_saved_expression_results(handle, 1, 0, 10));
    scope(&page, "saved-member", "7372632fff2e7273", 1, true);
    let expression_reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(expression_reader, 0);
    let opened = take(fcb_saved_open_expression_hit_reader(handle, expression_reader, 1, 1));
    scope(&opened, "saved-member", "7372632fff2e7273", 1, true);
    assert!(opened.contains("\"original_hex\":\"6e006500650064006c006500\""), "{opened}");
    assert!(opened.contains("\"live_source_reopened\":false"));

    let legacy = take(unsafe { fcb_saved_search_matching(handle, 3, needle.as_ptr(), 0, 1, 20) });
    assert_eq!(field(&legacy, "matches_seen"), 5, "legacy call must still use the whole archive");
    assert!(legacy.contains("\"scope\":\"whole-saved-archive\""));
    assert_eq!(fcb_saved_close(handle), 1);
    for reader in [literal_reader, expression_reader] {
        assert!(take(crate::reader_ffi::fcb_reader_copy_range(reader, 2, 14))
            .contains("\"original_hex\":\"6e006500650064006c006500\""));
        assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
    }
}

#[test]
fn scopes_close_only_known_exact_members_and_reject_malformed_foreign_frames() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let (_, path) = fixture(false, &[
        entry(b"src/a.rs", b"needle required"),
        SnapshotEntry { path: b"unavailable.rs", observed_bytes: 50, data: SnapshotData::Unavailable("denied") },
    ]);
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    take(unsafe { fcb_saved_open(handle, path.as_ptr()) });
    let needle = CString::new("needle").unwrap();
    let expression = CString::new("needle required").unwrap();
    let exact = take(unsafe { fcb_saved_search_scoped_page(handle, 1, needle.as_ptr(), 1, 1,
        1, 0, 2, b"src/a.rs".as_ptr(), 8) });
    scope(&exact, "saved-member", "7372632f612e7273", 1, true);
    assert!(exact.contains("\"search_complete\":true"), "{exact}");
    let directory = take(unsafe { fcb_saved_search_scoped_page(handle, 2, needle.as_ptr(), 1, 1,
        1, 0, 1, b"src".as_ptr(), 3) });
    scope(&directory, "saved-directory", "737263", 1, false);
    assert!(directory.contains("\"search_complete\":false"), "{directory}");
    let missing = take(unsafe { fcb_saved_expression_search_scoped_page(handle, 1,
        expression.as_ptr(), 1, 1, 1, 4096, 0, 2, b"missing.rs".as_ptr(), 10) });
    scope(&missing, "saved-member", "6d697373696e672e7273", 0, false);
    assert!(missing.contains("\"complete\":false"), "{missing}");
    assert_eq!(field(&missing, "matches_seen"), 0);

    for (kind, bytes, length) in [
        (0, b"src".as_ptr(), 3), (1, std::ptr::null(), 3),
        (2, b"src".as_ptr(), 0), (3, std::ptr::null(), 0),
        (1, b"src".as_ptr(), u64::MAX),
    ] {
        for answer in [
            take(unsafe { fcb_saved_search_scoped_page(handle, 3, needle.as_ptr(), 1, 1,
                1, 0, kind, bytes, length) }),
            take(unsafe { fcb_saved_expression_search_scoped_page(handle, 2, expression.as_ptr(), 1, 1,
                1, 4096, 0, kind, bytes, length) }),
        ] {
            assert!(answer.contains("SAVED_HANDLE_INVALID_ARGUMENT"), "{answer}");
            assert!(!answer.contains("needle"), "malformed requests must not echo source/query payloads");
        }
    }
    for (ordinal, path) in [b"../src".as_slice(), b"src/", b"src//a.rs", b"/src", b"src/./a.rs", b"src/\0a"].iter().enumerate() {
        let error = take(unsafe { fcb_saved_search_scoped_page(handle, 10 + ordinal as u64,
            needle.as_ptr(), 1, 1, 1, 0, 1, path.as_ptr(), path.len() as u64) });
        assert!(error.contains("\"status\":\"error\""), "{error}");
    }
    scope(&take(fcb_saved_results(handle, 2, 0, 10)), "saved-directory", "737263", 1, false);
    scope(&take(fcb_saved_expression_results(handle, 1, 0, 10)), "saved-member", "6d697373696e672e7273", 0, false);
    assert_eq!(fcb_saved_close(handle), 1);
}

#[test]
fn scoped_source_loading_skips_corrupted_unselected_members() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let outside = b"outside unique payload needle required";
    let (path, foreign) = fixture(true, &[
        entry(b"a-outside.rs", outside), entry(b"src/main.rs", b"needle required"),
    ]);
    let archive = std::fs::read(&path).unwrap();
    let offset = archive.windows(outside.len()).position(|bytes| bytes == outside).unwrap() as u64;
    let handle = fcb_saved_create(); assert_ne!(handle, 0);
    take(unsafe { fcb_saved_open(handle, foreign.as_ptr()) });
    // The initial envelope was verified. Now corrupt only an excluded member:
    // a post-filter implementation would still try to load it and fail.
    let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    writer.seek(SeekFrom::Start(offset)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    let needle = CString::new("needle").unwrap();
    let expression = CString::new("needle required").unwrap();
    let literal = take(unsafe { fcb_saved_search_scoped_page(handle, 1, needle.as_ptr(), 1, 1,
        10, 0, 1, b"src".as_ptr(), 3) });
    assert!(literal.contains("\"search_complete\":true"), "{literal}");
    assert_eq!(field(&literal, "matches_seen"), 1);
    let expression_page = take(unsafe { fcb_saved_expression_search_scoped_page(handle, 1,
        expression.as_ptr(), 1, 1, 10, 4096, 0, 1, b"src".as_ptr(), 3) });
    assert!(expression_page.contains("\"complete\":true"), "{expression_page}");
    assert_eq!(field(&expression_page, "matches_seen"), 1);
    let whole = take(unsafe { fcb_saved_search_counted(handle, 2, needle.as_ptr(), 10) });
    assert!(whole.contains("\"status\":\"error\""), "negative control must detect corrupted source: {whole}");
    scope(&take(fcb_saved_results(handle, 1, 0, 10)), "saved-directory", "737263", 1, true);
    assert_eq!(fcb_saved_close(handle), 1);
}

#[test]
fn scoped_query_cancellation_keeps_previously_accepted_rows_and_scope() {
    let (path, _) = fixture(true, &[entry(b"src/a.rs", b"needle required needle")]);
    let sessions = SavedSessions::new();
    let handle = sessions.create().unwrap();
    sessions.open(handle, &path, Default::default(), || false).unwrap();
    sessions.execute(handle, Command::SearchScopedPage { scope: SnapshotScope::Member(b"src/a.rs"),
        generation: 1, needle: "needle", limit: 10, case_sensitive: true,
        count_all_matches: true, skip_matches: 0 }, || false).unwrap();
    sessions.execute(handle, Command::ExpressionSearchScopedPage { scope: SnapshotScope::Directory(b"src"),
        generation: 1, query: "needle required", max_hits: 10, max_scan_bytes: 4096,
        case_sensitive: true, count_all_matches: true, skip_matches: 0 }, || false).unwrap();
    for command in [
        Command::SearchScopedPage { scope: SnapshotScope::Archive, generation: 2, needle: "needle",
            limit: 10, case_sensitive: true, count_all_matches: true, skip_matches: 1 },
        Command::ExpressionSearchScopedPage { scope: SnapshotScope::Member(b"src/a.rs"), generation: 2,
            query: "needle required", max_hits: 10, max_scan_bytes: 4096,
            case_sensitive: true, count_all_matches: true, skip_matches: 1 },
    ] {
        let mut signaled = false;
        let result = sessions.execute(handle, command, || {
            if !signaled { sessions.cancel(handle).unwrap(); signaled = true; } false
        });
        assert!(result.is_err()); assert!(signaled);
    }
    let literal = sessions.execute(handle, Command::Results { generation: 1, start: 0, limit: 10 }, || false).unwrap();
    scope(literal.as_str(), "saved-member", "7372632f612e7273", 1, true);
    assert_eq!(field(literal.as_str(), "retained_hits"), 2);
    let expression = sessions.execute(handle, Command::ExpressionResults { generation: 1, start: 0, limit: 10 }, || false).unwrap();
    scope(expression.as_str(), "saved-directory", "737263", 1, true);
    assert_eq!(field(expression.as_str(), "retained_hits"), 2);
    sessions.close(handle).unwrap();
}

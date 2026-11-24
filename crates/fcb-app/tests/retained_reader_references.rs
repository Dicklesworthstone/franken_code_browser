#![forbid(unsafe_code)]

//! Production shared-engine → retained-reader occurrence workflows. These are
//! portable semantics, not native rendering or compiler-reference evidence.
use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteRange};
use fcb::search::symbols::ReferenceError;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::reader::{ReaderSession, ReaderSessionError, MAX_READER_REFERENCE_PAGE,
    MAX_READER_REFERENCE_NAME_BYTES, MAX_READER_REFERENCES, MAX_READER_REFERENCE_SOURCE_BYTES,
    MAX_READER_SOURCE_BYTES};

fn reader(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(9511).unwrap(), Path::new("retained.rs"), bytes, || false).unwrap()
}
fn prepare(reader: &mut ReaderSession, generation: u64, name: &str) {
    reader.prepare_references(generation, name, MAX_READER_REFERENCES, || false).unwrap();
}
fn selected(reader: &ReaderSession, range: ByteRange) -> &[u8] {
    &reader.capture().bytes()[range.start().get() as usize..range.end().get() as usize]
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn count(json: &str, field: &str, expected: u64) {
    assert!(json.contains(&format!("\"{field}\":\"{expected}\"")), "{field}={expected}: {json}");
}
fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fcb-reference-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}

#[test]
fn token_occurrences_include_comments_and_literals_without_substring_false_positives() {
    let source = b"fn target() {}\n// target\nlet s = \"target\";\nother_target target_extra TARGET $target target";
    let mut r = reader(source);
    let first = r.prepare_references(1, "target", 100, || false).unwrap();
    assert_eq!(first.exit_code(), EXIT_OK);
    assert!(first.as_str().contains("\"retained_references\":\"4\""));
    assert!(first.as_str().contains("\"matches_counted\":\"4\""));
    assert!(first.as_str().contains("\"evidence_level\":\"whole-token-text-candidate\""));
    assert!(first.as_str().contains("\"includes_comments_and_literals\":true"));
    assert!(first.as_str().contains("\"compiler_resolved\":false"));
    assert!(first.as_str().contains("\"semantic_complete\":false"));
    assert!(first.as_str().contains("\"name_case\":\"sensitive\""));
    count(first.as_str(), "max_source_bytes", 32 * 1024 * 1024);
    for id in 1..=4 {
        assert_eq!(selected(&r, r.reference_range(1, id).unwrap()), b"target");
    }
    assert_eq!(r.reference_range(1, 4).unwrap().end().get(), source.len() as u64);
    assert_eq!(r.reference_range(1, 5).err(), Some(ReaderSessionError::Reference(ReferenceError::NotFound)));
}

#[test]
fn occurrence_window_copy_and_line_use_original_capture_without_query_namespace() {
    let mut r = reader(b"alpha\r\nalpha\ralpha\n"); prepare(&mut r, 1, "alpha");
    let address = r.capture().bytes().as_ptr();
    let window = r.reference_window(1, 2, 100, || false).unwrap();
    assert!(window.as_str().contains("\"command\":\"reference\""));
    assert!(window.as_str().contains("\"reference_generation\":\"1\""));
    assert!(window.as_str().contains("\"reference_id\":\"2\""));
    assert!(window.as_str().contains("\"reference_line\":\"2\""));
    assert!(window.as_str().contains("\"original_range\":{\"start\":\"7\",\"end\":\"12\"}"));
    assert!(window.as_str().contains("\"window_utf8_range\":{\"start\":\"7\",\"end\":\"12\"}"));
    assert!(!window.as_str().contains("\"query_generation\":"));
    assert!(!window.as_str().contains("\"outline_generation\":"));
    let copy = r.copy_reference(1, 2, || false).unwrap();
    assert!(copy.as_str().contains("\"original_hex\":\"616c706861\""));
    assert!(copy.as_str().contains("\"selection_namespace\":\"reference\""));
    assert!(copy.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    // Line metadata follows the engine's declared LF-counting policy, not an
    // invented promise about physical CR-only lines or native shaped rows.
    let final_row = r.reference_page(1, 2, 1, || false).unwrap();
    assert!(final_row.as_str().contains("\"line\":\"2\""));
    assert!(final_row.as_str().contains("\"line_semantics\":\"LF-counted-source-lines\""));
    assert_eq!(r.capture().bytes().as_ptr(), address);
}

#[test]
fn utf16_both_endiannesses_keep_original_ranges_after_surrogate_pairs() {
    for little in [true, false] {
        let encode = |text: &str| -> Vec<u8> {
            text.encode_utf16().flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
        };
        let source = encode("\u{feff}// 🦀\r\nlet target = target;\n");
        let expected = encode("target");
        let mut r = reader(&source); prepare(&mut r, 1, "target");
        for id in [1, 2] {
            let range = r.reference_range(1, id).unwrap();
            assert_eq!(selected(&r, range), expected);
            assert_eq!(range.len().get(), 12);
            let window = r.reference_window(1, id, 128, || false).unwrap();
            assert!(window.as_str().contains("\"window_utf8_range\":"));
            assert!(window.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&expected))));
            let copy = r.copy_reference(1, id, || false).unwrap();
            assert!(copy.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&expected))));
        }
    }
}

#[test]
fn exact_name_spelling_is_not_unicode_normalization_or_case_folding() {
    let composed = "café"; let decomposed = "cafe\u{301}";
    let source = format!("{composed} {decomposed} CAFÉ {composed}_suffix");
    let mut r = reader(source.as_bytes());
    let first = r.prepare_references(1, composed, 100, || false).unwrap();
    assert!(first.as_str().contains("\"retained_references\":\"1\""));
    assert_eq!(selected(&r, r.reference_range(1, 1).unwrap()), composed.as_bytes());
    prepare(&mut r, 2, decomposed);
    assert_eq!(selected(&r, r.reference_range(2, 1).unwrap()), decomposed.as_bytes());
}

#[test]
fn page_offsets_are_not_ids_and_paging_reuses_the_inventory() {
    let mut r = reader("token\n".repeat(150).as_bytes());
    let initial = r.prepare_references(1, "token", 200, || false).unwrap();
    assert_eq!(initial.as_str().matches("\"reference_id\":").count(), 64);
    assert!(initial.as_str().contains("\"next_offset\":\"64\""));
    let next = r.reference_page(1, 64, 64, || false).unwrap();
    assert!(next.as_str().contains("\"reference_id\":\"65\""));
    assert!(next.as_str().contains("\"next_offset\":\"128\""));
    let last = r.reference_page(1, 128, MAX_READER_REFERENCE_PAGE, || false).unwrap();
    assert_eq!(last.as_str().matches("\"reference_id\":").count(), 22);
    assert!(last.as_str().contains("\"reference_id\":\"150\""));
    assert!(last.as_str().contains("\"next_offset\":null"));
    let empty = r.reference_page(1, 150, 1, || false).unwrap();
    assert!(empty.as_str().contains("\"references\":[]"));
    assert!(r.reference_page(1, 151, 1, || false).is_err());
    assert_eq!(selected(&r, r.reference_range(1, 150).unwrap()), b"token");
}

#[test]
fn caps_distinguish_exact_total_from_one_extra_match_and_zero_cap_probe() {
    for (source, cap, retained, counted, limited) in [
        ("one one", 2, 2, 2, false), ("one one one", 2, 2, 3, true),
        ("one", 0, 0, 1, true), ("none", 0, 0, 0, false), ("", 0, 0, 0, false),
    ] {
        let mut r = reader(source.as_bytes());
        let out = r.prepare_references(1, "one", cap, || false).unwrap();
        assert_eq!(out.exit_code(), if limited { EXIT_PARTIAL } else { EXIT_OK });
        assert!(out.as_str().contains(&format!("\"retained_references\":\"{retained}\"")));
        assert!(out.as_str().contains(&format!("\"matches_counted\":\"{counted}\"")));
        assert!(out.as_str().contains(&format!("\"output_limited\":{limited}")));
        assert!(out.as_str().contains(&format!("\"count_complete\":{}", !limited)));
        assert!(out.as_str().contains(&format!("\"search_complete\":{}", !limited)));
        assert!(out.as_str().contains("\"semantic_complete\":false"));
    }
}

#[test]
fn failed_attempts_and_canceled_clear_preserve_old_published_inventory() {
    let mut r = reader(b"retained other"); prepare(&mut r, 1, "retained");
    assert_eq!(r.prepare_references(2, "two words", 10, || false).err(),
        Some(ReaderSessionError::Reference(ReferenceError::InvalidName)));
    assert_eq!(r.reference_generation(), Some(1));
    assert_eq!(r.prepare_references(2, "other", 10, || false).err(),
        Some(ReaderSessionError::Reference(ReferenceError::StaleQuery)));
    assert!(r.clear_references(3, || true).is_err());
    assert!(r.copy_reference(1, 1, || false).is_ok());
    let info = r.info(|| false).unwrap();
    assert!(info.as_str().contains("\"accepted_reference_generation\":\"1\""));
    assert!(info.as_str().contains("\"last_reference_attempt\":\"3\""));
    prepare(&mut r, 4, "other");
    assert_eq!(r.reference_range(1, 1).err(), Some(ReaderSessionError::Reference(ReferenceError::StaleQuery)));
    assert_eq!(selected(&r, r.reference_range(4, 1).unwrap()), b"other");
}

#[test]
fn every_observed_cancellation_boundary_preserves_published_occurrences() {
    let source = b"fn first() { first(); second(); } // second\n";
    let mut probe = reader(source); prepare(&mut probe, 1, "first");
    let mut checks = 0;
    probe.prepare_references(2, "second", 20, || { checks += 1; false }).unwrap();
    assert!(checks > 0);
    for cancel_at in 1..=checks {
        let mut r = reader(source); prepare(&mut r, 1, "first");
        let mut visited = 0;
        assert!(r.prepare_references(2, "second", 20, || {
            visited += 1; visited == cancel_at
        }).is_err(), "cancellation checkpoint {cancel_at}");
        assert_eq!(r.reference_generation(), Some(1));
        assert_eq!(selected(&r, r.reference_range(1, 1).unwrap()), b"first");
        assert!(r.prepare_references(2, "second", 20, || false).is_err());
    }
}

#[test]
fn source_find_outline_and_occurrence_namespaces_are_independent() {
    let mut r = reader(b"fn target() {}\n");
    r.search(1, "target", 10, 1024, || false).unwrap();
    r.prepare_outline(1, Default::default(), || false).unwrap();
    prepare(&mut r, 1, "target");
    let range = r.reference_range(1, 1).unwrap();
    assert_eq!(range, r.hit_range(1, 0).unwrap());
    assert_eq!(range, r.symbol_range(1, 1, false).unwrap());
    let hit = r.hit_window(1, 0, 0, || false).unwrap();
    let symbol = r.symbol_window(1, 1, 0, || false).unwrap();
    assert!(!hit.as_str().contains("reference_generation"));
    assert!(!symbol.as_str().contains("reference_generation"));
    r.clear_references(2, || false).unwrap();
    assert_eq!(r.reference_generation(), None);
    assert!(r.copy_hit(1, 0, || false).is_ok());
    assert!(r.copy_symbol(1, 1, false, || false).is_ok());
    prepare(&mut r, 3, "target");
    r.clear_outline(2, || false).unwrap();
    r.search(2, "fn", 10, 1024, || false).unwrap();
    assert!(r.copy_reference(3, 1, || false).is_ok());
}

#[test]
fn source_replacement_and_rename_cannot_change_retained_occurrences() {
    let root = fixture(); let path = root.join("source.rs");
    fs::write(&path, b"fn original() { original(); }\n").unwrap();
    let mut r = ReaderSession::open(ArenaOwnerId::new(9512).unwrap(), &path, 4096, || false).unwrap();
    fs::write(&path, b"fn replacement() {}\n").unwrap();
    fs::rename(&path, root.join("replacement.rs")).unwrap();
    prepare(&mut r, 1, "original");
    assert_eq!(selected(&r, r.reference_range(1, 2).unwrap()), b"original");
    let window = r.reference_window(1, 2, 1024, || false).unwrap();
    assert!(window.as_str().contains("fn original() { original(); }"));
    assert!(window.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    let old = r.copy_reference(1, 2, || false).unwrap();
    prepare(&mut r, 2, "replacement");
    assert!(r.reference_page(2, 0, 10, || false).unwrap().as_str().contains("\"retained_references\":\"0\""));
    drop(r);
    assert!(old.as_str().contains("6f726967696e616c"));
}

#[test]
fn occurrence_limit_is_independent_of_outline_limit_and_reader_refusals_do_not_lose_source() {
    let mut source = b"fn target() {}\n".to_vec(); source.resize(70 * 1024, b' ');
    let mut r = reader(&source);
    assert!(r.prepare_outline(1, Default::default(), || false).is_err());
    prepare(&mut r, 1, "target");
    assert_eq!(selected(&r, r.reference_range(1, 1).unwrap()), b"target");
    source.resize(1024 * 1024, b' ');
    let mut large = reader(&source);
    prepare(&mut large, 1, "target");
    assert_eq!(selected(&large, large.reference_range(1, 1).unwrap()), b"target");
    assert!(large.prepare_outline(1, Default::default(), || false).is_err());
    assert!(large.read_window(0, 128, || false).is_ok());
    assert_eq!(MAX_READER_REFERENCE_SOURCE_BYTES, fcb_app::host::reader::MAX_READER_SOURCE_BYTES);
    source.resize(MAX_READER_REFERENCE_SOURCE_BYTES + 1, b' ');
    assert!(matches!(ReaderSession::from_bytes(ArenaOwnerId::new(9511).unwrap(),
        Path::new("oversized.rs"), &source, || false), Err(ReaderSessionError::InvalidLimits)));
    let mut malformed = reader(b"target \xff");
    assert_eq!(malformed.prepare_references(1, "target", 10, || false).err(),
        Some(ReaderSessionError::Reference(ReferenceError::UnsupportedEncoding)));
    assert!(malformed.copy_range(0, 8, || false).is_ok());
}

#[test]
fn occurrence_windows_reach_the_reader_capture_ceiling_with_exact_copy_and_no_reread() {
    assert_eq!(MAX_READER_REFERENCE_SOURCE_BYTES, MAX_READER_SOURCE_BYTES);
    let mut source = vec![b' '; MAX_READER_SOURCE_BYTES];
    source[..4].copy_from_slice(b"foo\n");
    let start = source.len() - 3;
    source[start..].copy_from_slice(b"foo");
    let mut r = reader(&source);
    let address = r.capture().bytes().as_ptr();
    let result = r.prepare_references_window(1, "foo", 1, true, 1, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);
    count(result.as_str(), "max_source_bytes", 32 * 1024 * 1024);
    count(result.as_str(), "matches_counted", 2);
    count(result.as_str(), "retained_references", 1);
    count(result.as_str(), "occurrence_id", 2);
    assert!(result.as_str().contains("\"count_complete\":true"));
    assert!(result.as_str().contains("\"has_more_matches\":false"));
    let range = r.reference_range(1, 1).unwrap();
    assert_eq!((range.start().get(), range.end().get()), (start as u64, source.len() as u64));
    assert_eq!(selected(&r, range), b"foo");
    let window = r.reference_window(1, 1, 64, || false).unwrap();
    count(window.as_str(), "reference_line", 2);
    count(window.as_str(), "additional_source_bytes_read", 0);
    assert!(window.as_str().contains("\"original_hex\":\"666f6f\""));
    let copy = r.copy_reference(1, 1, || false).unwrap();
    assert!(copy.as_str().contains("\"original_hex\":\"666f6f\""));
    assert_eq!(r.capture().bytes().as_ptr(), address);
    assert_eq!(r.capture().bytes(), source);
    drop(r);
    source.push(b' ');
    assert_eq!(ReaderSession::from_bytes(ArenaOwnerId::new(9515).unwrap(),
        Path::new("oversized.rs"), &source, || false).err(), Some(ReaderSessionError::InvalidLimits));
}

#[test]
fn invalid_names_ranges_pages_and_exhausted_generations_do_not_alias() {
    for name in ["", "two words", "1token", "foo.bar", "nul\0", "line\n"] {
        let mut r = reader(b"token");
        assert_eq!(r.prepare_references(1, name, 10, || false).err(),
            Some(ReaderSessionError::Reference(ReferenceError::InvalidName)));
    }
    let mut r = reader(b"token");
    assert_eq!(r.prepare_references(1, &"x".repeat(MAX_READER_REFERENCE_NAME_BYTES + 1), 10, || false).err(),
        Some(ReaderSessionError::Reference(ReferenceError::InvalidName)));
    assert_eq!(r.prepare_references(2, "token", MAX_READER_REFERENCES + 1, || false).err(),
        Some(ReaderSessionError::Reference(ReferenceError::InvalidLimits)));
    prepare(&mut r, u64::MAX, "token");
    assert!(r.prepare_references(0, "token", 1, || false).is_err());
    assert!(r.clear_references(u64::MAX, || false).is_err());
    for invalid in [0, 2, u64::MAX] { assert!(r.reference_range(u64::MAX, invalid).is_err()); }
    for invalid in [0, MAX_READER_REFERENCE_PAGE + 1] {
        assert!(r.reference_page(u64::MAX, 0, invalid, || false).is_err());
    }
    assert!(r.reference_window(u64::MAX, 1, 16 * 1024 + 1, || false).is_err());
    assert!(r.copy_reference(u64::MAX, 1, || false).is_ok());
}

#[test]
fn fork_shares_exact_source_but_does_not_inherit_occurrence_authority() {
    let mut r = reader(b"token token"); prepare(&mut r, 1, "token");
    let mut fork = r.fork(ArenaOwnerId::new(9513).unwrap(), || false).unwrap();
    assert_eq!(fork.capture().bytes().as_ptr(), r.capture().bytes().as_ptr());
    assert_eq!(fork.reference_generation(), None);
    assert_eq!(fork.reference_range(1, 1).err(), Some(ReaderSessionError::MissingReferences));
    let info = fork.info(|| false).unwrap();
    assert!(info.as_str().contains("\"accepted_reference_generation\":null"));
    assert!(info.as_str().contains("\"last_reference_attempt\":\"0\""));
    prepare(&mut fork, 1, "token");
    r.clear_references(2, || false).unwrap(); drop(r);
    assert_eq!(selected(&fork, fork.reference_range(1, 2).unwrap()), b"token");
    assert!(fork.copy_reference(1, 2, || false).is_ok());
}

#[test]
fn later_counted_windows_keep_utf16_source_and_distinct_local_and_global_ids() {
    for little in [true, false] {
        let encode = |text: &str| -> Vec<u8> {
            text.encode_utf16().flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
        };
        // Remains below this route's 1 MiB capture admission while exercising
        // occurrence windows well beyond the former 512 KiB analysis limit.
        let source = encode(&format!("\u{feff}{}😀\r\n{}", " ".repeat(300_000), "foo\r\n".repeat(5005)));
        assert!(source.len() > 512 * 1024 && source.len() < 1024 * 1024);
        let expected = encode("foo");
        let path = fixture().join("source.rs"); fs::write(&path, &source).unwrap();
        let mut r = ReaderSession::open(ArenaOwnerId::new(9514).unwrap(), &path, 1024 * 1024, || false).unwrap();
        let address = r.capture().bytes().as_ptr();
        fs::write(&path, b"replacement").unwrap();
        let first = r.prepare_references_window(1, "foo", 4096, true, 0, || false).unwrap();
        assert_eq!(first.exit_code(), EXIT_OK);
        count(first.as_str(), "matches_counted", 5005); count(first.as_str(), "retained_references", 4096);
        count(first.as_str(), "next_match_offset", 4096); count(first.as_str(), "next_offset", 64);
        for fragment in ["\"count_all_matches\":true", "\"count_complete\":true", "\"search_complete\":true",
            "\"output_limited\":true", "\"rows_truncated\":true", "\"has_more_matches\":true"] {
            assert!(first.as_str().contains(fragment), "{fragment}: {}", first.as_str());
        }
        let later = r.prepare_references_window(2, "foo", 128, true, 4096, || false).unwrap();
        count(later.as_str(), "result_offset", 4096); count(later.as_str(), "skipped_matches", 4096);
        count(later.as_str(), "retained_references", 128); count(later.as_str(), "next_match_offset", 4224);
        count(later.as_str(), "reference_id", 1); count(later.as_str(), "occurrence_id", 4097);
        let second_page = r.reference_page(2, 64, 64, || false).unwrap();
        count(second_page.as_str(), "reference_id", 65); count(second_page.as_str(), "occurrence_id", 4161);
        count(second_page.as_str(), "occurrence_id", 4224);
        assert!(second_page.as_str().contains("\"next_offset\":null"));
        count(second_page.as_str(), "next_match_offset", 4224);
        let range = r.reference_range(2, 65).unwrap();
        assert_eq!((range.start().get(), range.end().get()), (641_610, 641_616));
        assert_eq!(selected(&r, range), expected);
        // Global ordinals are display/navigation metadata, never local IDs.
        assert!(r.reference_range(2, 4161).is_err());
        let selection = r.reference_window(2, 65, 128, || false).unwrap();
        assert!(selection.as_str().contains("\"window_utf8_range\":"));
        count(selection.as_str(), "reference_id", 65); count(selection.as_str(), "reference_line", 4162);
        count(selection.as_str(), "additional_source_bytes_read", 0);
        let copied = r.copy_reference(2, 65, || false).unwrap();
        assert!(copied.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&expected))));
        let last = r.prepare_references_window(3, "foo", 5, false, 5000, || false).unwrap();
        assert_eq!(last.exit_code(), EXIT_OK);
        count(last.as_str(), "matches_counted", 5005); count(last.as_str(), "retained_references", 5);
        count(last.as_str(), "occurrence_id", 5005);
        assert!(last.as_str().contains("\"search_complete\":true"));
        assert!(last.as_str().contains("\"has_more_matches\":false"));
        assert!(last.as_str().contains("\"next_match_offset\":null"));
        assert_eq!(r.reference_range(2, 65).err(), Some(ReaderSessionError::Reference(ReferenceError::StaleQuery)));
        assert_eq!(r.capture().bytes().as_ptr(), address);
        assert_eq!(r.capture().bytes(), source);
        drop(r);
        assert!(copied.as_str().contains(&hex(&expected)));
    }
}

#[test]
fn window_counting_zero_storage_full_width_offsets_and_lookahead_are_independent() {
    for (offset, max_items, all, retained, counted, complete, more, next) in [
        (1, 2, false, 2, 4, false, true, Some(3)),
        (3, 2, false, 2, 5, true, false, None),
        (0, 0, true, 0, 5, true, true, None),
        (2, 0, false, 0, 3, false, true, None),
        (u64::MAX, 2, false, 0, 5, true, false, None),
        (u64::MAX, 0, true, 0, 5, true, false, None),
    ] {
        let mut r = reader(b"foo foo foo foo foo");
        let response = r.prepare_references_window(1, "foo", max_items, all, offset, || false).unwrap();
        let json = response.as_str();
        assert_eq!(response.exit_code(), if complete { EXIT_OK } else { EXIT_PARTIAL });
        count(json, "result_offset", offset); count(json, "skipped_matches", offset.min(counted));
        count(json, "matches_counted", counted); count(json, "retained_references", retained);
        assert!(json.contains(&format!("\"count_complete\":{complete}")), "{json}");
        assert!(json.contains(&format!("\"search_complete\":{complete}")), "{json}");
        assert!(json.contains(&format!("\"has_more_matches\":{more}")), "{json}");
        assert!(json.contains(&format!("\"output_limited\":{more}")), "{json}");
        if let Some(next) = next { count(json, "next_match_offset", next); }
        else { assert!(json.contains("\"next_match_offset\":null")); }
        if retained == 0 { assert!(r.reference_range(1, 1).is_err()); }
        let page = r.reference_page(1, retained as usize, 1, || false).unwrap();
        assert!(page.as_str().contains("\"references\":[]"));
    }
    let mut empty = reader(b"");
    let response = empty.prepare_references_window(1, "foo", 0, true, u64::MAX, || false).unwrap();
    count(response.as_str(), "matches_counted", 0);
    assert!(response.as_str().contains("\"rows_truncated\":false"));
    assert!(response.as_str().contains("\"search_complete\":true"));
}

#[test]
fn counted_tail_cancellation_and_refused_window_preserve_accepted_rows_and_source() {
    let source = format!("old {}{}", " ".repeat(600_000), "foo ".repeat(5005));
    let mut r = reader(source.as_bytes()); prepare(&mut r, 1, "old");
    let before = r.reference_range(1, 1).unwrap();
    let mut polls = 0;
    // Decoder polls reach beyond 512 KiB before this replacement is canceled.
    let result = r.prepare_references_window(2, "foo", 1, true, 0, || { polls += 1; polls == 288 });
    assert_eq!(result.err(), Some(ReaderSessionError::Reference(ReferenceError::Canceled)));
    assert_eq!(polls, 288);
    assert_eq!(r.reference_generation(), Some(1));
    assert_eq!(r.reference_range(1, 1).unwrap(), before);
    assert!(r.prepare_references_window(2, "foo", 1, true, 4096, || false).is_err());
    assert_eq!(r.prepare_references_window(3, "foo", MAX_READER_REFERENCES + 1, true, 4096, || false).err(),
        Some(ReaderSessionError::Reference(ReferenceError::InvalidLimits)));
    assert_eq!(r.reference_generation(), Some(1));
    assert!(r.copy_reference(1, 1, || false).unwrap().as_str().contains("\"original_hex\":\"6f6c64\""));
    let next = r.prepare_references_window(4, "foo", 2, true, 5000, || false).unwrap();
    count(next.as_str(), "matches_counted", 5005); count(next.as_str(), "occurrence_id", 5001);
    assert_eq!(selected(&r, r.reference_range(4, 1).unwrap()), b"foo");
    assert_eq!(r.capture().bytes(), source.as_bytes());
    let mut malformed = reader(b"foo foo\xff");
    assert_eq!(malformed.prepare_references_window(1, "foo", 0, true, u64::MAX, || false).err(),
        Some(ReaderSessionError::Reference(ReferenceError::UnsupportedEncoding)));
}

#[test]
fn switching_back_to_legacy_references_keeps_the_legacy_wire_contract() {
    let mut r = reader(b"foo foo foo");
    let window = r.prepare_references_window(1, "foo", 1, true, 1, || false).unwrap();
    count(window.as_str(), "occurrence_id", 2);
    let legacy = r.prepare_references(2, "foo", 1, || false).unwrap();
    count(legacy.as_str(), "matches_counted", 2); count(legacy.as_str(), "reference_id", 1);
    assert!(legacy.as_str().contains("\"count_complete\":false"));
    for field in ["result_offset", "count_all_matches", "skipped_matches", "rows_truncated",
        "has_more_matches", "next_match_offset", "occurrence_id"] {
        assert!(!legacy.as_str().contains(&format!("\"{field}\":")), "{field}: {}", legacy.as_str());
    }
    assert_eq!(r.reference_range(2, 1).unwrap().start().get(), 0);
}

#[test]
fn large_retained_occurrence_windows_copy_exact_utf8_and_utf16_after_live_replacement() {
    let text = format!("{}😀\r\n{}", " ".repeat(600 * 1024), "foo\r\n".repeat(4101));
    for little in [None, Some(true), Some(false)] {
        let source: Vec<u8> = match little {
            None => text.as_bytes().to_vec(),
            Some(little) => format!("\u{feff}{text}").encode_utf16()
                .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect(),
        };
        let expected: Vec<u8> = match little {
            None => b"foo".to_vec(),
            Some(little) => "foo".encode_utf16()
                .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect(),
        };
        assert!(source.len() > 512 * 1024);
        let path = fixture().join("large.txt"); fs::write(&path, &source).unwrap();
        let mut r = ReaderSession::open(ArenaOwnerId::new(9518).unwrap(), &path,
            MAX_READER_REFERENCE_SOURCE_BYTES, || false).unwrap();
        let retained = r.capture().bytes().as_ptr();
        fs::write(&path, b"the live source has changed").unwrap();
        let result = r.prepare_references_window(1, "foo", 2, true, 4096, || false).unwrap();
        assert_eq!(result.exit_code(), EXIT_OK);
        count(result.as_str(), "matches_counted", 4101);
        count(result.as_str(), "retained_references", 2);
        count(result.as_str(), "occurrence_id", 4097);
        count(result.as_str(), "next_match_offset", 4098);
        count(result.as_str(), "max_source_bytes", MAX_READER_REFERENCE_SOURCE_BYTES as u64);
        let range = r.reference_range(1, 1).unwrap();
        assert!(range.start().get() > 512 * 1024);
        assert_eq!(selected(&r, range), expected);
        let window = r.reference_window(1, 1, 32, || false).unwrap();
        count(window.as_str(), "reference_line", 4098);
        count(window.as_str(), "additional_source_bytes_read", 0);
        assert!(window.as_str().contains("\"window_utf8_range\":"));
        assert!(r.copy_reference(1, 1, || false).unwrap().as_str()
            .contains(&format!("\"original_hex\":\"{}\"", hex(&expected))));
        assert!(r.prepare_references_window(2, "foo", 2, true, 4098, || true).is_err());
        assert_eq!(r.reference_generation(), Some(1));
        assert_eq!(r.capture().bytes().as_ptr(), retained);
        assert_eq!(selected(&r, r.reference_range(1, 1).unwrap()), expected);
    }
}

#![forbid(unsafe_code)]

//! Production shared-engine → retained-reader occurrence workflows. These are
//! portable semantics, not native rendering or compiler-reference evidence.
use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteRange};
use fcb::search::symbols::ReferenceError;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::reader::{ReaderSession, ReaderSessionError, MAX_READER_REFERENCE_PAGE,
    MAX_READER_REFERENCE_NAME_BYTES, MAX_READER_REFERENCES, MAX_READER_REFERENCE_SOURCE_BYTES};

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
    assert!(first.as_str().contains("\"max_source_bytes\":\"524288\""));
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
    source.resize(MAX_READER_REFERENCE_SOURCE_BYTES + 1, b' ');
    let mut large = reader(&source);
    assert_eq!(large.prepare_references(1, "target", 10, || false).err(),
        Some(ReaderSessionError::Reference(ReferenceError::SourceLimit)));
    assert!(large.read_window(0, 128, || false).is_ok());
    assert!(large.search(1, "target", 10, source.len() as u64, || false).is_ok());
    let mut malformed = reader(b"target \xff");
    assert_eq!(malformed.prepare_references(1, "target", 10, || false).err(),
        Some(ReaderSessionError::Reference(ReferenceError::UnsupportedEncoding)));
    assert!(malformed.copy_range(0, 8, || false).is_ok());
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

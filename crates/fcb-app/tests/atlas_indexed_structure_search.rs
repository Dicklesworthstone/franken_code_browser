#![forbid(unsafe_code)]
#![cfg(unix)]

//! Captured structural-search regressions for fcb-qtb.4/fcb-jvb.2. These are
//! code-first cases for independent remote verification, not execution proof.
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::search::SymbolNameMode;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasIndexedStructureOptions,
    AtlasSearchError, AtlasSearchPathKind, AtlasSearchScope, AtlasSearchStop,
    AtlasStructureMode, RetainedAtlasSearch};

struct Fixture(PathBuf);
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-indexed-structure-{}-{}-{}",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        Self(root)
    }
    fn open(&self) -> AtlasSession {
        AtlasSession::open(ArenaOwnerId::new(31620).unwrap(), &self.0,
            AtlasSessionOptions::default(), || false).unwrap()
    }
}
fn index_options() -> AtlasIndexOptions {
    // Structural extraction must include gram-quota fallback captures too.
    AtlasIndexOptions { max_files: 100, max_file_bytes: 1024 * 1024,
        max_source_bytes: 2 * 1024 * 1024, max_index_grams: 0 }
}
fn options(mode: AtlasStructureMode) -> AtlasIndexedStructureOptions {
    AtlasIndexedStructureOptions { mode, name_mode: SymbolNameMode::Exact,
        max_matches: 100, max_scan_bytes: 2 * 1024 * 1024 }
}
fn finish(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) -> String {
    for _ in 0..102 {
        if !search.progress(atlas, generation).unwrap().is_running() {
            return search.page(atlas, generation, 0, 128, || false).unwrap().as_str().to_owned();
        }
        search.step(atlas, generation, || false).unwrap();
        let progress = search.progress(atlas, generation).unwrap();
        assert!(progress.last_step_files <= 1);
        assert_eq!((progress.source_bytes_read, progress.read_calls,
            progress.last_step_source_bytes, progress.last_step_read_calls), (0, 0, 0, 0));
    }
    panic!("bounded captured structural query did not terminate");
}
fn count(json: &str, field: &str, expected: u64) {
    assert!(json.contains(&format!("\"{field}\":\"{expected}\"")), "{field}={expected}: {json}");
}

#[test]
fn all_structural_modes_reuse_original_captures_and_revision_after_live_paths_change() {
    let utf16: Vec<_> = "\u{feff}needle needle_suffix\n// needle\n".encode_utf16()
        .flat_map(u16::to_le_bytes).collect();
    for (mode, name, source, needle, expected) in [
        (AtlasStructureMode::Symbols, "source.rs", b"fn needle() {}\n// needle\n".as_slice(), "needle", 1),
        (AtlasStructureMode::Headings, "README.md", b"# Deploy!\n\nordinary Deploy!\n".as_slice(), "Deploy!", 1),
        (AtlasStructureMode::Occurrences, "source.txt", utf16.as_slice(), "needle", 2),
    ] {
        let fixture = Fixture::new(&[(name, source)]);
        let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
        search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
        fs::remove_file(fixture.0.join(name)).unwrap();
        let begin = search.begin_indexed_structure(&atlas, 2, 1, needle,
            AtlasSearchScope::all(), options(mode), || false).unwrap();
        assert!(begin.as_str().contains("\"command\":\"begin-indexed-structure\""));
        count(begin.as_str(), "source_bytes_read", 0);
        count(begin.as_str(), "verification_source_bytes", 0);
        let page = finish(&mut search, &atlas, 2);
        for fragment in ["\"search_strategy\":\"retained-capture-structure\"",
            "\"search_complete\":true", "\"semantic_complete\":false",
            "\"count_all_matches\":false", "\"match_count_complete\":true"] {
            assert!(page.contains(fragment), "{fragment}: {page}");
        }
        count(&page, "retained_hits", expected); count(&page, "matching_files", 1);
        count(&page, "index_generation", 1); count(&page, "verified_files", 1);
        count(&page, "verification_source_bytes", source.len() as u64);
        count(&page, "skipped_by_index", 0); count(&page, "fallback_files", 0);
        let hit = search.hit(&atlas, 2, 1).unwrap();
        assert_eq!(hit.revision.get(), 1);
        if mode == AtlasStructureMode::Symbols {
            assert_eq!((hit.original_range.start().get(), hit.original_range.end().get()), (3, 9));
        } else if mode == AtlasStructureMode::Occurrences {
            assert_eq!((hit.original_range.start().get(), hit.original_range.end().get()), (2, 14));
        } else {
            assert!(page.contains("\"heading_slug\":\"deploy\""));
        }
        search.clear_index(&atlas, 3, || false).unwrap();
        let (reader, receipt) = search.open_reader(&atlas, ArenaOwnerId::new(31621).unwrap(),
            2, 1, || false).unwrap();
        assert_eq!(reader.capture().bytes(), source);
        assert!(receipt.as_str().contains("\"source_reopened\":false"));
        assert_eq!(search.hit(&atlas, 2, 1).unwrap(), hit);
    }
}

#[test]
fn capture_scope_widening_stays_pending_while_narrowing_and_empty_scopes_can_finish() {
    let source = b"fn needle() {}\n";
    let fixture = Fixture::new(&[("a.rs", source), ("src/a.rs", source),
        ("src/nested/b.rs", source), ("src-old/a.rs", source)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let directory = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    search.prepare_index_scoped(&atlas, 1, directory, index_options(), || false).unwrap();
    search.begin_indexed_structure(&atlas, 2, 1, "needle", AtlasSearchScope::all(),
        options(AtlasStructureMode::Symbols), || false).unwrap();
    let broad = finish(&mut search, &atlas, 2);
    count(&broad, "eligible_files", 4); count(&broad, "eligible_captured_files", 2);
    count(&broad, "pending_files", 2); count(&broad, "retained_hits", 2);
    assert!(broad.contains("\"stop_reason\":\"capture-scope\""));
    assert!(broad.contains("\"search_complete\":false"));
    let narrowed = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Directory, "src/nested").unwrap();
    search.begin_indexed_structure(&atlas, 3, 1, "needle", narrowed,
        options(AtlasStructureMode::Symbols), || false).unwrap();
    let narrow = finish(&mut search, &atlas, 3);
    count(&narrow, "eligible_files", 1); count(&narrow, "excluded_files", 3);
    count(&narrow, "pending_files", 0); count(&narrow, "retained_hits", 1);
    assert!(narrow.contains("\"search_complete\":true"));
    let empty = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, "").unwrap();
    let mut settings = options(AtlasStructureMode::Symbols); settings.max_scan_bytes = 0;
    search.begin_indexed_structure(&atlas, 4, 1, "needle", empty, settings, || false).unwrap();
    let empty = finish(&mut search, &atlas, 4);
    count(&empty, "eligible_files", 0); count(&empty, "retained_hits", 0);
    assert!(empty.contains("\"search_complete\":true"));
    // A declared visible member absent from the original catalog is never a
    // complete negative, even when there is no captured member to extract.
    let missing = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible,
        "6d697373696e672e7273").unwrap();
    search.begin_indexed_structure(&atlas, 5, 1, "needle", missing, settings, || false).unwrap();
    let missing = finish(&mut search, &atlas, 5);
    count(&missing, "scope_region_missing_files", 1);
    assert!(missing.contains("\"search_complete\":false"));
}

#[test]
fn failed_captures_and_build_omissions_remain_separate_from_structural_negatives() {
    let fixture = Fixture::new(&[("a.txt", b"needle"), ("b.txt", b"needle!!!"), ("c.txt", b"absent")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut capture = index_options(); capture.max_files = 2; capture.max_file_bytes = 8;
    search.prepare_index(&atlas, 1, capture, || false).unwrap();
    let begin = search.begin_indexed_structure(&atlas, 2, 1, "needle", AtlasSearchScope::all(),
        options(AtlasStructureMode::Occurrences), || false).unwrap();
    count(begin.as_str(), "examined_files", 1); count(begin.as_str(), "scoped_capture_unavailable_files", 1);
    let page = finish(&mut search, &atlas, 2);
    count(&page, "examined_files", 2); count(&page, "scanned_files", 1);
    count(&page, "unavailable_files", 1); count(&page, "pending_files", 1);
    assert!(page.contains("\"stop_reason\":\"file-limit\""));
    assert!(page.contains("\"search_complete\":false"));
    let selected = AtlasSearchScope::all().with_path(AtlasSearchPathKind::File, "a.txt").unwrap();
    search.begin_indexed_structure(&atlas, 3, 1, "needle", selected,
        options(AtlasStructureMode::Occurrences), || false).unwrap();
    assert!(finish(&mut search, &atlas, 3).contains("\"search_complete\":true"));
}

#[test]
fn extractor_caps_bad_encoding_and_work_budget_produce_truthful_partial_coverage() {
    let oversized = vec![b' '; 65_537];
    let bad = b"fn needle() {}\xff";
    let good = b"fn needle() {}\n";
    let fixture = Fixture::new(&[("a.swift", b"func needle() {}\n"), ("b.rs", &oversized),
        ("c.rs", bad), ("d.rs", good)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
    let mut settings = options(AtlasStructureMode::Symbols); settings.max_scan_bytes = 0;
    search.begin_indexed_structure(&atlas, 2, 1, "needle", AtlasSearchScope::all(), settings, || false).unwrap();
    let zero = finish(&mut search, &atlas, 2);
    count(&zero, "examined_files", 2); count(&zero, "unavailable_files", 2);
    count(&zero, "pending_files", 2); count(&zero, "verification_source_bytes", 0);
    assert!(zero.contains("\"stop_reason\":\"verification-byte-limit\""));
    search.begin_indexed_structure(&atlas, 3, 1, "needle", AtlasSearchScope::all(),
        options(AtlasStructureMode::Symbols), || false).unwrap();
    let page = finish(&mut search, &atlas, 3);
    count(&page, "examined_files", 4); count(&page, "scanned_files", 1);
    count(&page, "unavailable_files", 3); count(&page, "unsupported_structure_files", 1);
    count(&page, "retained_hits", 1); count(&page, "pending_files", 0);
    count(&page, "verification_source_bytes", (bad.len() + good.len()) as u64);
    for refusal in ["STRUCTURE_UNSUPPORTED_LANGUAGE", "STRUCTURE_FILE_BYTE_LIMIT", "SYMBOL_UNSUPPORTED_ENCODING"] {
        assert!(page.contains(refusal), "{refusal}: {page}");
    }
    assert!(page.contains("\"search_complete\":false"));
}

#[test]
fn exact_row_capacity_continues_through_negative_captures_and_counts_real_lookahead() {
    for (mode, suffix, positive, negative) in [
        (AtlasStructureMode::Symbols, "rs", b"fn needle() {}\n".as_slice(), b"fn other() {}\n".as_slice()),
        (AtlasStructureMode::Headings, "md", b"# needle\n".as_slice(), b"# other\n".as_slice()),
        (AtlasStructureMode::Occurrences, "txt", b"needle".as_slice(), b"needle_suffix".as_slice()),
    ] {
        for extra_match in [false, true] {
            let first = format!("a.{suffix}"); let later = format!("b.{suffix}"); let last = format!("c.{suffix}");
            let mut parts = vec![(first.as_str(), positive), (later.as_str(), negative)];
            if extra_match { parts.push((last.as_str(), positive)); }
            let fixture = Fixture::new(&parts);
            let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
            search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
            let mut settings = options(mode); settings.max_matches = 1;
            search.begin_indexed_structure(&atlas, 2, 1, "needle", AtlasSearchScope::all(), settings, || false).unwrap();
            let page = finish(&mut search, &atlas, 2);
            count(&page, "retained_hits", 1); count(&page, "retained_matching_files", 1);
            count(&page, "matches_seen", if extra_match { 2 } else { 1 });
            count(&page, "matching_files", if extra_match { 2 } else { 1 });
            count(&page, "pending_files", 0);
            assert_eq!(search.progress(&atlas, 2).unwrap().complete, !extra_match);
            assert_eq!(search.progress(&atlas, 2).unwrap().truncated, extra_match);
        }
    }
}

#[test]
fn canceled_or_replaced_index_queries_cannot_resume_but_accepted_sources_stay_pinned() {
    let source = b"fn needle() {}\n";
    let fixture = Fixture::new(&[("a.rs", source), ("b.rs", source)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
    search.begin_indexed_structure(&atlas, 2, 1, "needle", AtlasSearchScope::all(),
        options(AtlasStructureMode::Symbols), || false).unwrap();
    finish(&mut search, &atlas, 2);
    let accepted = search.hit(&atlas, 2, 1).unwrap();
    search.begin_indexed_structure(&atlas, 3, 1, "needle", AtlasSearchScope::all(),
        options(AtlasStructureMode::Symbols), || false).unwrap();
    assert_eq!(search.step(&atlas, 3, || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(search.hit(&atlas, 2, 1).unwrap(), accepted);
    fs::write(fixture.0.join("a.rs"), b"fn other() {}\n").unwrap();
    search.begin_index(&atlas, 4, index_options(), || false).unwrap();
    search.begin_indexed_structure(&atlas, 5, 1, "needle", AtlasSearchScope::all(),
        options(AtlasStructureMode::Symbols), || false).unwrap();
    search.step(&atlas, 5, || false).unwrap();
    let (reader, _) = search.open_reader(&atlas, ArenaOwnerId::new(31622).unwrap(), 5, 1, || false).unwrap();
    while search.pending_index_generation().is_some() { search.step_index(&atlas, 4, || false).unwrap(); }
    assert_eq!(search.index_generation(), Some(4)); assert_eq!(search.pending_generation(), None);
    assert_eq!(search.step(&atlas, 5, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(search.hit(&atlas, 2, 1).unwrap(), accepted);
    assert_eq!(reader.capture().bytes(), source);
    assert_eq!(search.begin_indexed_structure(&atlas, 6, 1, "needle", AtlasSearchScope::all(),
        options(AtlasStructureMode::Symbols), || false).err(), Some(AtlasSearchError::StaleIndex));
    assert_eq!(search.hit(&atlas, 2, 1).unwrap(), accepted);
    assert_eq!(search.progress(&atlas, 2).unwrap().stop_reason, Some(AtlasSearchStop::AllFilesExamined));
}

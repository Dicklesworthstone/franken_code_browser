#![forbid(unsafe_code)]
#![cfg(unix)]

//! Captured structural-search regressions for fcb-qtb.4/fcb-jvb.2. These are
//! code-first cases for independent remote verification, not execution proof.
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::search::SymbolNameMode;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasIndexedStructureOptions,
    AtlasResultWindow, AtlasSearchError, AtlasSearchPathKind, AtlasSearchScope, AtlasSearchStop,
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
fn later_windows_reach_every_structural_mode_in_both_utf16_encodings_without_live_reads() {
    for (mode, name, prelude, line) in [
        (AtlasStructureMode::Symbols, "source.rs", "// 😀\n", "fn needle() {}\n"),
        (AtlasStructureMode::Headings, "README.md", "😀\n\n", "# needle\n\n"),
        (AtlasStructureMode::Occurrences, "source.txt", "😀\n", "needle\n"),
    ] {
        for little in [true, false] {
            let text = format!("\u{feff}{prelude}{}", line.repeat(1003));
            let source: Vec<_> = text.encode_utf16()
                .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
            let fixture = Fixture::new(&[(name, &source)]);
            let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
            search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
            fs::write(fixture.0.join(name), b"the live path has changed").unwrap();
            let mut settings = options(mode); settings.max_matches = 1000;
            search.begin_indexed_structure_window(&atlas, 2, 1, "needle", AtlasSearchScope::all(),
                settings, AtlasResultWindow { result_offset: 0, count_all_matches: true }, || false).unwrap();
            let first = finish(&mut search, &atlas, 2);
            count(&first, "matches_seen", 1003); count(&first, "retained_hits", 1000);
            count(&first, "window_matches", 1000); count(&first, "next_match_offset", 1000);
            count(&first, "occurrence_id", 1);
            for fragment in ["\"search_complete\":true", "\"match_count_complete\":true",
                "\"count_all_matches\":true", "\"truncated\":true", "\"has_more_matches\":true"] {
                assert!(first.contains(fragment), "{fragment}: {first}");
            }
            let original = search.hit(&atlas, 2, 1).unwrap();
            search.begin_indexed_structure_window(&atlas, 3, 1, "needle", AtlasSearchScope::all(),
                settings, AtlasResultWindow { result_offset: 1000, count_all_matches: true }, || false).unwrap();
            let later = finish(&mut search, &atlas, 3);
            count(&later, "matches_seen", 1003); count(&later, "retained_hits", 3);
            count(&later, "skipped_matches", 1000); count(&later, "window_matches", 3);
            for id in [1001, 1002, 1003] { count(&later, "occurrence_id", id); }
            assert!(later.contains("\"has_more_matches\":false"));
            assert!(later.contains("\"next_match_offset\":null"));
            assert!(later.contains("\"search_complete\":true"));
            let hit = search.hit(&atlas, 3, 1).unwrap();
            let skipped_bytes = 1000 * line.encode_utf16().count() as u64 * 2;
            assert_eq!(hit.original_range.start().get(), original.original_range.start().get() + skipped_bytes);
            assert_eq!(hit.original_range.end().get(), original.original_range.end().get() + skipped_bytes);
            assert_eq!(hit.revision.get(), 1);
            search.clear_index(&atlas, 4, || false).unwrap();
            let (reader, receipt) = search.open_reader(&atlas, ArenaOwnerId::new(31623).unwrap(),
                3, 1, || false).unwrap();
            assert_eq!(reader.capture().bytes(), source);
            assert!(receipt.as_str().contains("\"source_reopened\":false"));
        }
    }
}

#[test]
fn scoped_windows_count_skipped_files_and_continue_after_exact_capacity() {
    let fixture = Fixture::new(&[("excluded.txt", b"needle needle needle needle"),
        ("src/a.txt", b"needle needle"), ("src/b.txt", b"prefix needle needle"),
        ("src/c.txt", b"needle_suffix")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
    let scope = || AtlasSearchScope::all().with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let mut settings = options(AtlasStructureMode::Occurrences); settings.max_matches = 1;
    search.begin_indexed_structure_window(&atlas, 2, 1, "needle", scope(), settings,
        AtlasResultWindow { result_offset: 2, count_all_matches: true }, || false).unwrap();
    let counted = finish(&mut search, &atlas, 2);
    count(&counted, "eligible_files", 3); count(&counted, "excluded_files", 1);
    count(&counted, "matches_seen", 4); count(&counted, "matching_files", 2);
    count(&counted, "retained_matching_files", 1); count(&counted, "retained_hits", 1);
    count(&counted, "next_match_offset", 3); count(&counted, "occurrence_id", 3);
    assert_eq!(search.hit(&atlas, 2, 1).unwrap().original_range.start().get(), 7);
    assert!(counted.contains("\"search_complete\":true"));
    search.begin_indexed_structure_window(&atlas, 3, 1, "needle", scope(), settings,
        AtlasResultWindow { result_offset: 2, count_all_matches: false }, || false).unwrap();
    let bounded = finish(&mut search, &atlas, 3);
    count(&bounded, "matches_seen", 4); count(&bounded, "pending_files", 1);
    assert!(bounded.contains("\"stop_reason\":\"match-limit\""));
    assert!(bounded.contains("\"search_complete\":false"));
    search.begin_indexed_structure_window(&atlas, 4, 1, "needle", scope(), settings,
        AtlasResultWindow { result_offset: 3, count_all_matches: false }, || false).unwrap();
    let final_window = finish(&mut search, &atlas, 4);
    count(&final_window, "matches_seen", 4); count(&final_window, "pending_files", 0);
    count(&final_window, "occurrence_id", 4); count(&final_window, "retained_hits", 1);
    assert!(final_window.contains("\"search_complete\":true"));
    assert!(final_window.contains("\"truncated\":true"));
    assert!(final_window.contains("\"has_more_matches\":false"));
    search.begin_indexed_structure_window(&atlas, 5, 1, "needle", scope(), settings,
        AtlasResultWindow { result_offset: u64::MAX, count_all_matches: false }, || false).unwrap();
    let beyond = finish(&mut search, &atlas, 5);
    count(&beyond, "result_offset", u64::MAX); count(&beyond, "skipped_matches", 4);
    count(&beyond, "matches_seen", 4); count(&beyond, "window_matches", 0);
    count(&beyond, "retained_hits", 0); count(&beyond, "retained_matching_files", 0);
    assert!(beyond.contains("\"search_complete\":true"));
    assert!(beyond.contains("\"has_more_matches\":false"));
    settings.max_matches = 0;
    search.begin_indexed_structure_window(&atlas, 6, 1, "needle", scope(), settings,
        AtlasResultWindow { result_offset: 0, count_all_matches: true }, || false).unwrap();
    let count_only = finish(&mut search, &atlas, 6);
    count(&count_only, "matches_seen", 4); count(&count_only, "matching_files", 2);
    count(&count_only, "retained_hits", 0); count(&count_only, "retained_source_bytes", 0);
    assert!(count_only.contains("\"search_complete\":true"));
    assert!(count_only.contains("\"has_more_matches\":true"));
    assert!(count_only.contains("\"next_match_offset\":null"));
}

#[test]
fn omitted_metadata_advances_the_structural_window_without_repeating_later_rows() {
    let source = format!("# {}\n\n# A\n\n# Another\n", "A".repeat(4097));
    let fixture = Fixture::new(&[("README.md", source.as_bytes())]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
    let mut settings = options(AtlasStructureMode::Headings);
    settings.name_mode = SymbolNameMode::Prefix; settings.max_matches = 1;
    search.begin_indexed_structure_window(&atlas, 2, 1, "A", AtlasSearchScope::all(), settings,
        AtlasResultWindow { result_offset: 0, count_all_matches: true }, || false).unwrap();
    let omitted = finish(&mut search, &atlas, 2);
    count(&omitted, "matches_seen", 3); count(&omitted, "retained_hits", 0);
    count(&omitted, "window_matches", 1); count(&omitted, "structure_metadata_omitted", 1);
    count(&omitted, "next_match_offset", 1); count(&omitted, "retained_source_bytes", 0);
    assert!(omitted.contains("\"search_complete\":false"));
    search.begin_indexed_structure_window(&atlas, 3, 1, "A", AtlasSearchScope::all(), settings,
        AtlasResultWindow { result_offset: 1, count_all_matches: true }, || false).unwrap();
    let later = finish(&mut search, &atlas, 3);
    count(&later, "matches_seen", 3); count(&later, "retained_hits", 1);
    count(&later, "structure_metadata_omitted", 0); count(&later, "next_match_offset", 2);
    count(&later, "occurrence_id", 2);
    assert!(later.contains("\"name\":\"A\""));
    assert!(later.contains("\"search_complete\":true"));
}

#[test]
fn counting_a_later_window_keeps_extractor_item_limits_explicitly_partial() {
    // Stay inside the independent source-line guard to reach the item guard.
    let source = "fn a() {} ".repeat(4100);
    let fixture = Fixture::new(&[("source.rs", source.as_bytes())]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
    let mut settings = options(AtlasStructureMode::Symbols); settings.max_matches = 2;
    search.begin_indexed_structure_window(&atlas, 2, 1, "a", AtlasSearchScope::all(), settings,
        AtlasResultWindow { result_offset: 4095, count_all_matches: true }, || false).unwrap();
    let page = finish(&mut search, &atlas, 2);
    count(&page, "matches_seen", 4096); count(&page, "retained_hits", 1);
    count(&page, "occurrence_id", 4096); count(&page, "extraction_limited_files", 1);
    count(&page, "unavailable_files", 1); count(&page, "pending_files", 0);
    assert!(page.contains("STRUCTURE_EXTRACTION_ITEM_LIMIT"));
    assert!(page.contains("\"count_all_matches\":true"));
    assert!(page.contains("\"search_complete\":false"));
    assert!(page.contains("\"match_count_complete\":false"));
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
fn indexed_occurrence_queries_and_windows_admit_the_full_capture_bound_without_widening_it() {
    let mut source = vec![b' '; 1024 * 1024];
    source[..6].copy_from_slice(b"needle");
    let tail_start = source.len() - 6;
    source[tail_start..].copy_from_slice(b"needle");
    let fixture = Fixture::new(&[("large.txt", &source)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
    fs::write(fixture.0.join("large.txt"), b"changed working file").unwrap();
    let settings = options(AtlasStructureMode::Occurrences);
    search.begin_indexed_structure(&atlas, 2, 1, "needle", AtlasSearchScope::all(), settings, || false).unwrap();
    let all = finish(&mut search, &atlas, 2);
    count(&all, "structure_file_byte_limit", 1024 * 1024);
    count(&all, "retained_hits", 2); count(&all, "verification_source_bytes", source.len() as u64);
    assert!(all.contains("\"search_complete\":true"));
    search.begin_indexed_structure_window(&atlas, 3, 1, "needle", AtlasSearchScope::all(), settings,
        AtlasResultWindow { result_offset: 1, count_all_matches: true }, || false).unwrap();
    let tail = finish(&mut search, &atlas, 3);
    count(&tail, "structure_file_byte_limit", 1024 * 1024);
    count(&tail, "matches_seen", 2); count(&tail, "retained_hits", 1);
    let hit = search.hit(&atlas, 3, 1).unwrap();
    assert_eq!(hit.original_range.start().get(), tail_start as u64);
    let (reader, receipt) = search.open_reader(&atlas, ArenaOwnerId::new(31621).unwrap(),
        3, 1, || false).unwrap();
    assert!(receipt.as_str().contains("\"source_reopened\":false"));
    assert_eq!(reader.capture().bytes(), source);
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
    let oversized = vec![b' '; fcb::search::MAX_SYMBOL_SOURCE_BYTES + 1];
    let bad = b"fn needle() {}\xff";
    let good = b"fn needle() {}\n";
    let fixture = Fixture::new(&[("a.kt", b"fun needle() {}\n"), ("b.rs", &oversized),
        ("c.rs", bad), ("d.rs", good)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    // Capture the oversized source so this exercises the extractor's ceiling,
    // independently of the archive/index capture admission limit.
    let mut capture = index_options(); capture.max_file_bytes = 2 * 1024 * 1024;
    search.prepare_index(&atlas, 1, capture, || false).unwrap();
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
    search.begin_indexed_structure_window(&atlas, 4, 1, "needle", AtlasSearchScope::all(),
        options(AtlasStructureMode::Symbols), AtlasResultWindow {
            result_offset: 0, count_all_matches: true }, || false).unwrap();
    let counted = finish(&mut search, &atlas, 4);
    count(&counted, "matches_seen", 1); count(&counted, "unavailable_files", 3);
    assert!(counted.contains("\"count_all_matches\":true"));
    assert!(counted.contains("\"match_count_complete\":false"));
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


#[test]
fn indexed_java_and_csharp_symbols_reuse_utf16_sources_after_paths_disappear() {
    for (path, language) in [("Browser.java", "java"), ("Browser.cs", "csharp")] {
        let source: Vec<_> = "\u{feff}// 😀\nclass Browser { public void open() {} }\n".encode_utf16()
            .flat_map(u16::to_be_bytes).collect();
        let fixture = Fixture::new(&[(path, &source)]);
        let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
        search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
        fs::remove_file(fixture.0.join(path)).unwrap();
        search.begin_indexed_structure(&atlas, 2, 1, "open", AtlasSearchScope::all(),
            options(AtlasStructureMode::Symbols), || false).unwrap();
        let page = finish(&mut search, &atlas, 2);
        count(&page, "retained_hits", 1); count(&page, "source_bytes_read", 0);
        assert!(page.contains(&format!("\"language\":\"{language}\"")));
        assert!(page.contains("\"search_complete\":true") && page.contains("\"semantic_complete\":false"));
        let hit = search.hit(&atlas, 2, 1).unwrap();
        let (start, end) = hit.original_range.as_usize_bounds().unwrap();
        let expected: Vec<_> = "open".encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(&source[start..end], expected);
        let (reader, receipt) = search.open_reader(&atlas, ArenaOwnerId::new(31622).unwrap(), 2, 1, || false).unwrap();
        assert_eq!(reader.capture().bytes(), source);
        assert!(receipt.as_str().contains("\"source_reopened\":false"));
    }
}

#[test]
fn large_captured_occurrences_count_and_activate_after_index_and_live_source_change() {
    let source = [vec![b' '; 768 * 1024], b"needle needle_suffix needle\n".to_vec()].concat();
    let fixture = Fixture::new(&[("large.txt", &source)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
    fs::write(fixture.0.join("large.txt"), b"replacement").unwrap();
    let mut settings = options(AtlasStructureMode::Occurrences); settings.max_matches = 1;
    search.begin_indexed_structure_window(&atlas, 2, 1, "needle", AtlasSearchScope::all(),
        settings, AtlasResultWindow { result_offset: 1, count_all_matches: true }, || false).unwrap();
    let receipt = finish(&mut search, &atlas, 2);
    count(&receipt, "matches_seen", 2);
    count(&receipt, "retained_hits", 1);
    count(&receipt, "occurrence_id", 2);
    count(&receipt, "structure_file_byte_limit", 1024 * 1024);
    assert!(receipt.contains("\"search_complete\":true"));
    assert!(receipt.contains("\"has_more_matches\":false"));
    let hit = search.hit(&atlas, 2, 1).unwrap();
    assert_eq!(hit.original_range.start().get(), 768 * 1024 + 21);
    search.clear_index(&atlas, 3, || false).unwrap();
    let (mut reader, opened) = search.open_reader(&atlas, ArenaOwnerId::new(31628).unwrap(),
        2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), source);
    assert!(opened.as_str().contains("\"source_reopened\":false"));
    let refs = reader.prepare_references_window(1, "needle", 1, true, 1, || false).unwrap();
    count(refs.as_str(), "matches_counted", 2);
    assert_eq!(reader.reference_range(1, 1).unwrap(), hit.original_range);
}

#[test]
fn large_indexed_symbols_reach_late_lines_and_keep_captures_after_index_release() {
    let prefix = "// 😀 captured context\r\n".repeat(5_000);
    let text = format!("\u{feff}{prefix}fn late_target() {{}}\r\n");
    for little in [true, false] {
        let source: Vec<_> = text.encode_utf16()
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
        let fixture = Fixture::new(&[("large.rs", &source)]);
        let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
        search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
        fs::write(fixture.0.join("large.rs"), b"fn replacement() {}\n").unwrap();
        search.begin_indexed_structure(&atlas, 2, 1, "late_target", AtlasSearchScope::all(),
            options(AtlasStructureMode::Symbols), || false).unwrap();
        let page = finish(&mut search, &atlas, 2);
        count(&page, "retained_hits", 1); count(&page, "source_bytes_read", 0);
        count(&page, "verification_source_bytes", source.len() as u64);
        assert!(page.contains("\"search_complete\":true"), "{page}");
        let hit = search.hit(&atlas, 2, 1).unwrap();
        let expected: Vec<_> = "late_target".encode_utf16()
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
        assert_eq!(&source[hit.original_range.start().get() as usize..hit.original_range.end().get() as usize], expected);
        search.clear_index(&atlas, 3, || false).unwrap();
        let (mut reader, receipt) = search.open_reader(&atlas, ArenaOwnerId::new(31639).unwrap(),
            2, 1, || false).unwrap();
        assert_eq!(reader.capture().bytes(), source);
        assert!(receipt.as_str().contains("\"source_reopened\":false"));
        let outline = reader.prepare_outline(1, Default::default(), || false).unwrap();
        assert!(outline.as_str().contains("\"declaration_line\":\"5001\""));
        assert!(outline.as_str().contains("\"name\":\"late_target\""));
    }
}

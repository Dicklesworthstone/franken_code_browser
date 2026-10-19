#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real catalog/capture/extractor/reader regressions for fcb-qtb.4. These are
//! code-first cases for the independent verifier, not an execution receipt.
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::search::SymbolNameMode;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasSearchError, AtlasSearchOptions, AtlasSearchPathKind,
    AtlasSearchScope, AtlasStructureMode, AtlasStructureOptions, RetainedAtlasSearch};

struct Fixture(PathBuf);
impl Fixture {
    fn new(parts: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-structure-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).unwrap();
        for (name, source) in parts {
            let file = path.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, source).unwrap();
        }
        Self(path)
    }
    fn open(&self) -> AtlasSession {
        AtlasSession::open(ArenaOwnerId::new(31601).unwrap(), &self.0,
            AtlasSessionOptions::default(), || false).unwrap()
    }
}
fn options(mode: AtlasStructureMode) -> AtlasStructureOptions {
    AtlasStructureOptions { mode, name_mode: SymbolNameMode::Contains,
        limits: AtlasSearchOptions { max_matches: 100, max_files: 100,
            max_file_bytes: 1024 * 1024, max_source_bytes: 1024 * 1024 } }
}
fn finish(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) -> String {
    for _ in 0..102 {
        if !search.progress(atlas, generation).unwrap().is_running() {
            return search.page(atlas, generation, 0, 128, || false).unwrap().as_str().to_owned();
        }
        search.step(atlas, generation, || false).unwrap();
        assert!(search.progress(atlas, generation).unwrap().last_step_files <= 1);
    }
    panic!("bounded structural query failed to finish");
}
fn count(json: &str, name: &str, expected: u64) {
    assert!(json.contains(&format!("\"{name}\":\"{expected}\"")), "{name}={expected}: {json}");
}

#[test]
fn symbol_mode_extracts_names_and_exact_activation_survives_live_replacement() {
    let source = b"fn run() {}\nfn runner() {}\n// run is also ordinary text\n";
    let fixture = Fixture::new(&[("src/main.rs", source)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut settings = options(AtlasStructureMode::Symbols); settings.name_mode = SymbolNameMode::Exact;
    let begin = search.begin_structure(&atlas, 1, "run", AtlasSearchScope::all(), settings, || false).unwrap();
    count(begin.as_str(), "source_bytes_read", 0);
    assert!(begin.as_str().contains("\"requested_mode\":\"symbols\""));
    let json = finish(&mut search, &atlas, 1);
    assert!(json.contains("\"mode\":\"symbol-name\""));
    assert!(json.contains("\"evidence_level\":\"heuristic-outline-candidate\""));
    assert!(json.contains("\"semantic_complete\":false"));
    assert!(json.contains("\"search_complete\":true"));
    assert!(json.contains("\"name\":\"run\""));
    count(&json, "retained_hits", 1); count(&json, "matches_seen", 1);
    let hit = search.hit(&atlas, 1, 1).unwrap();
    assert_eq!((hit.original_range.start().get(), hit.original_range.end().get()), (3, 6));
    fs::write(fixture.0.join("src/main.rs"), b"fn other() {}\n").unwrap();
    let (mut reader, receipt) = search.open_reader(&atlas, ArenaOwnerId::new(31602).unwrap(), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), source);
    assert!(receipt.as_str().contains("\"source_reopened\":false"));
    assert!(receipt.as_str().contains("\"heading_slug\":null"));
    search.clear(&atlas, 2, || false).unwrap();
    assert!(reader.copy_range(3, 6, || false).unwrap().as_str().contains("72756e"));
}

#[test]
fn fmd_headings_preserve_bom_punctuation_duplicate_slugs_and_nonliteral_ranges() {
    let source = b"\xef\xbb\xbf# Deploy!\r\n\r\nordinary Deploy! text\r\n\r\n# Deploy!\r\n";
    let fixture = Fixture::new(&[("README.md", source)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut settings = options(AtlasStructureMode::Headings); settings.name_mode = SymbolNameMode::Exact;
    search.begin_structure(&atlas, 1, "Deploy!", AtlasSearchScope::all(), settings, || false).unwrap();
    let json = finish(&mut search, &atlas, 1);
    assert!(json.contains("\"mode\":\"markdown-heading\""));
    assert!(json.contains("\"realized_mode\":\"fmd-heading\""));
    assert!(json.contains("\"heading_slug\":\"deploy\""));
    assert!(json.contains("\"heading_slug\":\"deploy-2\""));
    assert!(json.contains("\"search_complete\":true"));
    count(&json, "retained_hits", 2);
    let first = search.hit(&atlas, 1, 1).unwrap(); let second = search.hit(&atlas, 1, 2).unwrap();
    assert_eq!(first.original_range.start().get(), 3);
    assert!(second.original_range.start() > first.original_range.end());
    let selected = &source[first.original_range.start().get() as usize..first.original_range.end().get() as usize];
    assert!(selected.starts_with(b"# Deploy!"));
    assert_ne!(selected, b"Deploy!"); // Source evidence includes Markdown syntax.
    let (reader, receipt) = search.open_reader(&atlas, ArenaOwnerId::new(31603).unwrap(), 1, 2, || false).unwrap();
    assert_eq!(reader.capture().bytes(), source);
    assert!(receipt.as_str().contains("\"heading_slug\":\"deploy-2\""));
}

#[test]
fn directory_and_type_scope_exclude_sources_before_file_and_match_quotas() {
    let fixture = Fixture::new(&[("a.rs", b"fn wanted() {}\n"),
        ("src-old/first.rs", b"fn wanted() {}\n"), ("src/noise.txt", b"wanted"),
        ("src/target.rs", b"fn wanted() {}\n")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let selected = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let mut settings = options(AtlasStructureMode::Symbols);
    settings.limits.max_files = 1; settings.limits.max_matches = 1;
    settings.limits.max_source_bytes = b"fn wanted() {}\n".len();
    search.begin_structure(&atlas, 1, "wanted", selected, settings, || false).unwrap();
    let json = finish(&mut search, &atlas, 1);
    assert!(json.contains("\"search_complete\":true"));
    assert!(json.contains("src/target.rs")); assert!(!json.contains("src-old"));
    count(&json, "eligible_files", 1); count(&json, "excluded_files", 3);
    count(&json, "source_bytes_read", b"fn wanted() {}\n".len() as u64);
    count(&json, "retained_hits", 1); count(&json, "unavailable_files", 0);
}

#[test]
fn unsupported_invalid_encoding_and_oversized_sources_are_explicit_partial_coverage() {
    let oversized = vec![b'a'; 65_537];
    let fixture = Fixture::new(&[("a.swift", b"func wanted() {}\n"), ("b.rs", &oversized),
        ("c.rs", b"fn wanted() {}\xff"), ("d.rs", b"fn wanted() {}\n")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin_structure(&atlas, 1, "wanted", AtlasSearchScope::all(), options(AtlasStructureMode::Symbols), || false).unwrap();
    let json = finish(&mut search, &atlas, 1);
    assert!(json.contains("\"search_complete\":false"));
    count(&json, "retained_hits", 1); count(&json, "unavailable_files", 3);
    count(&json, "unsupported_structure_files", 1); count(&json, "structure_file_byte_limit", 65_536);
    assert!(json.contains("STRUCTURE_UNSUPPORTED_LANGUAGE"));
    assert!(json.contains("STRUCTURE_FILE_BYTE_LIMIT"));
    assert!(json.contains("SYMBOL_UNSUPPORTED_ENCODING"));
    count(&json, "source_bytes_read", (b"fn wanted() {}\xff".len() + b"fn wanted() {}\n".len()) as u64);
}

#[test]
fn canceled_or_superseded_structural_work_preserves_the_previous_literal_query() {
    let fixture = Fixture::new(&[("a.rs", b"fn wanted() {}\n"), ("b.rs", b"fn wanted() {}\n")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, 1, "wanted", options(AtlasStructureMode::Symbols).limits, || false).unwrap();
    let old = search.hit(&atlas, 1, 1).unwrap();
    search.begin_structure(&atlas, 2, "wanted", AtlasSearchScope::all(), options(AtlasStructureMode::Symbols), || false).unwrap();
    assert_eq!(search.step(&atlas, 2, || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(search.hit(&atlas, 1, 1).unwrap(), old);
    let text = search.page(&atlas, 1, 0, 128, || false).unwrap();
    assert!(text.as_str().contains("\"mode\":\"exact-decoded-literal\""));
    assert!(!text.as_str().contains("\"requested_mode\""));
    search.begin_structure(&atlas, 3, "wanted", AtlasSearchScope::all(), options(AtlasStructureMode::Symbols), || false).unwrap();
    search.step(&atlas, 3, || false).unwrap();
    let (reader, _) = search.open_reader(&atlas, ArenaOwnerId::new(31604).unwrap(), 3, 1, || false).unwrap();
    assert!(search.cancel_pending());
    assert_eq!(search.step(&atlas, 3, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(reader.capture().bytes(), b"fn wanted() {}\n");
    assert_eq!(search.hit(&atlas, 1, 1).unwrap(), old);
}

#[test]
fn symbol_name_matching_preserves_decoding_and_prefix_case_semantics() {
    let bytes: Vec<u8> = [0xff, 0xfe].into_iter().chain("fn run() {}\nfn runner() {}\n".encode_utf16()
        .flat_map(u16::to_le_bytes)).collect();
    let fixture = Fixture::new(&[("utf16.rs", &bytes)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut settings = options(AtlasStructureMode::Symbols); settings.name_mode = SymbolNameMode::Prefix;
    search.begin_structure(&atlas, 1, "run", AtlasSearchScope::all(), settings, || false).unwrap();
    count(&finish(&mut search, &atlas, 1), "retained_hits", 2);
    let first = search.hit(&atlas, 1, 1).unwrap();
    assert_eq!((first.original_range.start().get(), first.original_range.end().get()), (8, 14));
    settings.name_mode = SymbolNameMode::Exact;
    search.begin_structure(&atlas, 2, "RUN", AtlasSearchScope::all(), settings, || false).unwrap();
    let json = finish(&mut search, &atlas, 2);
    count(&json, "retained_hits", 0); assert!(json.contains("\"search_complete\":true"));
}

#[test]
fn bounded_metadata_omission_does_not_drop_smaller_later_headings_or_claim_complete() {
    let mut source = format!("# {}\n\n", "A".repeat(4097)); source.push_str("# A\n");
    let fixture = Fixture::new(&[("README.md", source.as_bytes())]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin_structure(&atlas, 1, "A", AtlasSearchScope::all(), options(AtlasStructureMode::Headings), || false).unwrap();
    let json = finish(&mut search, &atlas, 1);
    count(&json, "structure_metadata_omitted", 1); count(&json, "retained_hits", 1);
    count(&json, "unavailable_files", 1); count(&json, "matches_seen", 2);
    assert!(json.contains("STRUCTURE_METADATA_LIMIT"));
    assert!(json.contains("\"name\":\"A\"")); assert!(json.contains("\"search_complete\":false"));
}

#[test]
fn result_limit_reports_lookahead_and_later_file_coverage_without_reopening_on_page() {
    let fixture = Fixture::new(&[("a.rs", b"fn wanted() {}\nfn wanted_more() {}\n"),
        ("b.rs", b"fn wanted_later() {}\n")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut settings = options(AtlasStructureMode::Symbols); settings.limits.max_matches = 1;
    search.begin_structure(&atlas, 1, "wanted", AtlasSearchScope::all(), settings, || false).unwrap();
    let json = finish(&mut search, &atlas, 1);
    count(&json, "matches_seen", 2); count(&json, "retained_hits", 1); count(&json, "pending_files", 1);
    assert!(json.contains("\"truncated\":true")); assert!(json.contains("\"search_complete\":false"));
    let before = search.progress(&atlas, 1).unwrap();
    search.page(&atlas, 1, 0, 128, || false).unwrap(); search.overlay(&atlas, 1, || false).unwrap();
    let after = search.progress(&atlas, 1).unwrap();
    assert_eq!(before.source_bytes_read, after.source_bytes_read); assert_eq!(before.read_calls, after.read_calls);
}

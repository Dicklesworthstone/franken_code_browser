#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Public Desk composition regressions for fcb-qtb.4. Code-first fixtures;
//! executable evidence belongs to the independent verification lane.
mod support;
use support::{parse, Json};
use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb::search::SymbolNameMode;
use fcb_app::host::{HostResponse, atlas_session::AtlasSessionOptions,
    atlas_search::{AtlasIndexOptions, AtlasSearchOptions, AtlasSearchScope, AtlasSearchPathKind,
        AtlasStructureMode, AtlasStructureOptions, AtlasIndexedStructureOptions, AtlasSearchError},
    desk::{DeskSession, DeskLimits, repository::{DeskRepository, DeskRepositoryError}}};

fn fixture(files: &[(&str, &[u8])]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-structure-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    for (name, bytes) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    root
}
fn repository(root: &Path) -> DeskRepository {
    DeskRepository::open(ArenaOwnerId::new(951).unwrap(), root,
        AtlasSessionOptions::default(), || false).unwrap()
}
fn desk() -> DeskSession {
    DeskSession::new(ArenaOwnerId::new(952).unwrap(), DeskLimits::default()).unwrap()
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn options(mode: AtlasStructureMode) -> AtlasStructureOptions {
    AtlasStructureOptions { mode, name_mode: SymbolNameMode::Exact, limits: AtlasSearchOptions::default() }
}
fn finish(repo: &mut DeskRepository, generation: u64) -> Json {
    for _ in 0..32 {
        let p = repo.query_progress(generation).unwrap();
        if !p.is_running() { return json(&repo.page(generation, 0, 128, || false).unwrap()); }
        repo.step_query(generation, p.step_count, || false).unwrap();
        assert!(repo.query_progress(generation).unwrap().last_step_files <= 1);
    }
    panic!("small structural fixture did not terminate");
}
fn build(repo: &mut DeskRepository, generation: u64) {
    repo.begin_index(generation, AtlasIndexOptions { max_index_grams: 0, ..Default::default() }, || false).unwrap();
    for _ in 0..32 {
        let p = repo.index_progress(generation).unwrap();
        if !p.in_progress { return; }
        repo.step_index(generation, p.steps, || false).unwrap();
    }
    panic!("small capture fixture did not terminate");
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

#[test]
fn symbols_admit_without_io_and_open_exact_original_names_after_live_replacement() {
    let source = b"fn target() {}\nfn targets() {}\n// target\n";
    let root = fixture(&[("main.rs", source)]);
    let mut repo = repository(&root);
    let begin = repo.begin_structure(1, "target", AtlasSearchScope::all(),
        options(AtlasStructureMode::Symbols), || false).unwrap();
    assert_eq!(json(&begin).get("source_bytes_read").number(), 0);
    let p = repo.query_progress(1).unwrap();
    assert_eq!(p.examined_files, 0);
    repo.page(1, 0, 16, || false).unwrap();
    assert_eq!(repo.query_progress(1).unwrap(), p);
    let page = finish(&mut repo, 1);
    assert_eq!(page.get("requested_mode").text(), "symbols");
    assert_eq!(page.get("realized_mode").text(), "heuristic-outline-candidate");
    assert!(!page.get("semantic_complete").flag());
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("retained_hits").number(), 1);
    fs::write(root.join("main.rs"), b"fn changed() {}\n").unwrap();
    let mut d = desk();
    let opened = repo.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    let pane = opened.change.active.unwrap();
    assert_eq!(d.model().source(pane, 1).unwrap().bytes(), source);
    assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), b"target");
    repo.clear(2, || false).unwrap(); drop(repo);
    assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), b"target");
}

#[test]
fn heading_names_keep_duplicate_identity_and_markdown_source_evidence() {
    let source = b"\xef\xbb\xbf# Deploy!\r\n\r\nordinary Deploy!\r\n\r\n# Deploy!\r\n";
    let root = fixture(&[("README.md", source)]);
    let mut repo = repository(&root);
    repo.begin_structure(1, "Deploy!", AtlasSearchScope::all(),
        options(AtlasStructureMode::Headings), || false).unwrap();
    let page = finish(&mut repo, 1);
    assert!(page.get("search_complete").flag());
    let rows = page.get("hits").array();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("structure").get("heading_slug").text(), "deploy");
    assert_eq!(rows[1].get("structure").get("heading_slug").text(), "deploy-2");
    let mut d = desk();
    let first = repo.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    let second = repo.open_hit(&mut d, 1, 2, 1, 2, || false).unwrap();
    assert_ne!(first.original_range, second.original_range);
    assert!(second.reused_source);
    assert_eq!(d.model().retained_source_count(), 1);
    assert!(d.model().selected_bytes(second.change.active.unwrap(), 2).unwrap().starts_with(b"# Deploy!"));
}

#[test]
fn occurrences_use_original_utf16_offsets_and_include_comments_and_literals() {
    let source: Vec<u8> = [0xff, 0xfe].into_iter().chain(
        "needle needleSuffix\n// needle\n\"needle\"\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
    let root = fixture(&[("source.txt", &source)]);
    let mut repo = repository(&root);
    repo.begin_structure(1, "needle", AtlasSearchScope::all(),
        options(AtlasStructureMode::Occurrences), || false).unwrap();
    let page = finish(&mut repo, 1);
    assert_eq!(page.get("retained_hits").number(), 3);
    assert!(page.get("includes_comments_and_literals").flag());
    assert!(!page.get("compiler_resolved").flag());
    let mut d = desk();
    let hit = repo.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    assert_eq!((hit.original_range.start().get(), hit.original_range.end().get()), (2, 14));
    assert_eq!(d.model().selected_bytes(hit.change.active.unwrap(), 1).unwrap(), &source[2..14]);
}

#[test]
fn directory_and_file_scopes_apply_before_quotas_and_preserve_component_boundaries() {
    let source = b"fn target() {}\n";
    let root = fixture(&[("a.rs", source), ("src/main.rs", source), ("src-old/main.rs", source)]);
    let mut repo = repository(&root);
    let scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let mut o = options(AtlasStructureMode::Symbols);
    o.limits.max_files = 1; o.limits.max_matches = 1; o.limits.max_source_bytes = source.len();
    repo.begin_structure(1, "target", scope, o, || false).unwrap();
    let page = finish(&mut repo, 1);
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("eligible_files").number(), 1);
    assert_eq!(page.get("excluded_files").number(), 2);
    assert_eq!(page.get("source_bytes_read").number(), source.len() as u64);
    let file = AtlasSearchScope::all().with_path(AtlasSearchPathKind::File, "src-old/main.rs").unwrap();
    repo.begin_structure(2, "target", file, o, || false).unwrap();
    let page = finish(&mut repo, 2);
    assert_eq!(page.get("scope_path").text(), "src-old/main.rs");
    assert_eq!(page.get("retained_hits").number(), 1);
}

#[test]
fn visible_membership_is_frozen_and_empty_or_missing_members_never_become_workspace() {
    let root = fixture(&[("a.rs", b"fn target() {}\n"), ("b.rs", b"fn target() {}\n")]);
    let mut repo = repository(&root);
    let scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, &hex(b"a.rs")).unwrap();
    repo.begin_structure(1, "target", scope, options(AtlasStructureMode::Symbols), || false).unwrap();
    let page = finish(&mut repo, 1);
    assert_eq!(page.get("eligible_files").number(), 1);
    assert_eq!(page.get("scope_region_catalogued_files").number(), 1);
    let empty = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, "").unwrap();
    repo.begin_structure(2, "target", empty, options(AtlasStructureMode::Symbols), || false).unwrap();
    let page = finish(&mut repo, 2);
    assert_eq!(page.get("eligible_files").number(), 0);
    assert_eq!(page.get("source_bytes_read").number(), 0);
    assert_eq!(page.get("retained_hits").number(), 0);
    let missing = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, &hex(b"missing.rs")).unwrap();
    repo.begin_structure(3, "target", missing, options(AtlasStructureMode::Symbols), || false).unwrap();
    let page = finish(&mut repo, 3);
    assert_eq!(page.get("scope_region_missing_files").number(), 1);
    assert!(!page.get("search_complete").flag());
}

#[test]
fn invalid_canceled_and_stale_replacements_preserve_accepted_results_and_desk_state() {
    let root = fixture(&[("a.rs", b"fn target() {}\n"), ("b.rs", b"fn target() {}\n")]);
    let mut repo = repository(&root);
    repo.begin_structure(1, "target", AtlasSearchScope::all(), options(AtlasStructureMode::Symbols), || false).unwrap();
    finish(&mut repo, 1);
    let mut d = desk();
    repo.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    repo.begin_structure(2, "target", AtlasSearchScope::all(), options(AtlasStructureMode::Symbols), || false).unwrap();
    repo.step_query(2, 0, || false).unwrap();
    let progress = repo.query_progress(2).unwrap();
    assert_eq!(repo.step_query(2, 0, || false).err(), Some(DeskRepositoryError::StaleStep));
    assert_eq!(repo.query_progress(2).unwrap(), progress);
    let mut invalid = options(AtlasStructureMode::Occurrences); invalid.name_mode = SymbolNameMode::Prefix;
    assert_eq!(repo.begin_structure(3, "target", AtlasSearchScope::all(), invalid, || false).err(),
        Some(DeskRepositoryError::Search(AtlasSearchError::InvalidLimits)));
    // A new attempt supersedes pending work before its name-mode validation.
    // The accepted result set remains available even when that attempt fails.
    assert_eq!(repo.work_state().pending_query, None);
    assert_eq!(repo.cancel_query(2, || false).err(),
        Some(DeskRepositoryError::Search(AtlasSearchError::StaleQuery)));
    repo.begin_structure(4, "target", AtlasSearchScope::all(),
        options(AtlasStructureMode::Symbols), || false).unwrap();
    repo.cancel_query(4, || false).unwrap();
    assert!(matches!(repo.begin_structure(5, "target", AtlasSearchScope::all(),
        options(AtlasStructureMode::Symbols), || true), Err(e) if e.is_canceled()));
    assert_eq!(repo.work_state().accepted_query, Some(1));
    assert_eq!(json(&repo.page(1, 0, 16, || false).unwrap()).get("retained_hits").number(), 2);
    assert_eq!(d.model().revision(), 1);
}

#[test]
fn indexed_structure_ignores_gram_coverage_and_keeps_captured_sources_after_clear() {
    let source = b"fn target() {}\n// target\n";
    let root = fixture(&[("a.rs", source)]);
    let mut repo = repository(&root); build(&mut repo, 1);
    fs::write(root.join("a.rs"), b"fn replacement() {}\n").unwrap();
    let options = AtlasIndexedStructureOptions { mode: AtlasStructureMode::Symbols,
        name_mode: SymbolNameMode::Exact, max_matches: 10, max_scan_bytes: 1000 };
    repo.begin_indexed_structure(2, 1, "target", AtlasSearchScope::all(), options, || false).unwrap();
    let page = finish(&mut repo, 2);
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("search_strategy").text(), "retained-capture-structure");
    assert_eq!(page.get("source_bytes_read").number(), 0);
    assert_eq!(page.get("read_calls").number(), 0);
    assert_eq!(page.get("retained_hits").number(), 1);
    let mut d = desk();
    let opened = repo.open_hit(&mut d, 0, 1, 2, 1, || false).unwrap();
    assert_eq!(opened.source_revision.get(), 1);
    repo.clear_index(3, || false).unwrap(); drop(repo);
    assert_eq!(d.model().source(opened.change.active.unwrap(), 1).unwrap().bytes(), source);
    assert_eq!(d.model().selected_bytes(opened.change.active.unwrap(), 1).unwrap(), b"target");
}

#[test]
fn missing_indexes_unsupported_sources_and_budget_stops_are_not_complete_absence() {
    let root = fixture(&[("a.kt", b"fun target() {}\n"), ("b.rs", b"fn target() {}\n")]);
    let mut repo = repository(&root);
    let options = AtlasIndexedStructureOptions { mode: AtlasStructureMode::Symbols,
        name_mode: SymbolNameMode::Exact, max_matches: 10, max_scan_bytes: 0 };
    assert_eq!(repo.begin_indexed_structure(1, 99, "target", AtlasSearchScope::all(), options, || false).err(),
        Some(DeskRepositoryError::Search(AtlasSearchError::MissingIndex)));
    repo.begin_structure(2, "target", AtlasSearchScope::all(),
        self::options(AtlasStructureMode::Symbols), || false).unwrap();
    let page = finish(&mut repo, 2);
    assert!(!page.get("search_complete").flag());
    assert_eq!(page.get("unsupported_structure_files").number(), 1);
    assert_eq!(page.get("retained_hits").number(), 1);
    build(&mut repo, 3);
    repo.begin_indexed_structure(4, 3, "target", AtlasSearchScope::all(), options, || false).unwrap();
    let page = finish(&mut repo, 4);
    assert!(!page.get("search_complete").flag());
    assert_eq!(page.get("retained_hits").number(), 0);
    assert_eq!(page.get("source_bytes_read").number(), 0);
}

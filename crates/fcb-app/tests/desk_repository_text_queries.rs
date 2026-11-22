#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Public Desk search composition for fcb-qtb.1 / fcb-n8sn.2.
//! Code-first regression fixtures; executable evidence belongs to the
//! independent verification lane.
mod support;

use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, atlas_session::AtlasSessionOptions,
    atlas_search::{AtlasIndexOptions, AtlasSearchError, AtlasSearchOptions,
        AtlasSearchPathKind as Kind, AtlasSearchScope},
    desk::{DeskLimits, DeskSession, repository::{DeskRepository, DeskRepositoryError}}};
use support::{parse, Json};

const SCAN_BYTES: u64 = 1024 * 1024;

fn owner() -> ArenaOwnerId {
    static NEXT: AtomicU64 = AtomicU64::new(61901);
    ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap()
}
fn fixture(files: &[(&str, &[u8])]) -> PathBuf {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-text-{}-{stamp}-{}",
        std::process::id(), owner().get()));
    fs::create_dir_all(&root).unwrap();
    for (name, bytes) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    root
}
fn repository(root: &Path) -> DeskRepository {
    DeskRepository::open(owner(), root, AtlasSessionOptions::default(), || false).unwrap()
}
fn desk() -> DeskSession { DeskSession::new(owner(), DeskLimits::default()).unwrap() }
fn options(max_matches: usize) -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches, max_files: 16, max_file_bytes: 65536, max_source_bytes: 131072 }
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn finish(repo: &mut DeskRepository, generation: u64) -> HostResponse {
    for _ in 0..32 {
        let progress = repo.query_progress(generation).unwrap();
        if !progress.is_running() { return repo.page(generation, 0, 64, || false).unwrap(); }
        repo.step_query(generation, progress.step_count, || false).unwrap();
        assert!(repo.query_progress(generation).unwrap().last_step_files <= 1);
    }
    panic!("small Desk query did not finish within its fixture membership");
}
fn build(repo: &mut DeskRepository, generation: u64) {
    repo.begin_index(generation, AtlasIndexOptions { max_files: 16, max_file_bytes: 65536,
        max_source_bytes: 131072, max_index_grams: 0 }, || false).unwrap();
    for _ in 0..32 {
        let progress = repo.index_progress(generation).unwrap();
        if !progress.in_progress { return; }
        repo.step_index(generation, progress.steps, || false).unwrap();
    }
    panic!("small Desk capture set did not finish");
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    "\u{feff}".encode_utf16().chain(text.encode_utf16())
        .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn counts(page: &Json, seen: u64, rows: u64, files: u64) {
    assert!(page.get("count_all_matches").flag());
    assert_eq!(page.get("matches_seen").number(), seen);
    assert_eq!(page.get("retained_hits").number(), rows);
    assert_eq!(page.get("matching_files").number(), files);
    assert_eq!(page.get("truncated").flag(), seen > rows);
    assert_eq!(page.get("match_count_complete").flag(), page.get("search_complete").flag());
}

#[test]
fn scoped_literals_keep_literal_syntax_and_case_policy_does_not_widen_path_membership() {
    let source = b"Needle path:fake -skip\n";
    let root = fixture(&[("src/main.rs", source), ("src-old/main.rs", source), ("other.rs", source)]);
    let mut repo = repository(&root);
    let scope = AtlasSearchScope::all().with_path(Kind::Directory, "src").unwrap()
        .with_case_sensitive(false);
    let mut limits = options(8); limits.max_files = 1; limits.max_source_bytes = source.len();
    let begin = repo.begin_query_scoped(1, "needle path:fake -skip", scope, limits, || false).unwrap();
    assert_eq!(json(&begin).get("source_bytes_read").number(), 0);
    let before = repo.query_progress(1).unwrap();
    repo.page(1, 0, 8, || false).unwrap();
    assert_eq!(repo.query_progress(1).unwrap(), before);
    let page = json(&finish(&mut repo, 1));
    assert_eq!(page.get("mode").text(), "unicode-lowercase-literal");
    assert!(!page.get("case_sensitive").flag());
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("eligible_files").number(), 1);
    assert_eq!(page.get("excluded_files").number(), 2);
    assert_eq!(page.get("retained_hits").number(), 1);
    assert_eq!(page.get("source_bytes_read").number(), source.len() as u64);
    let mut d = desk();
    let opened = repo.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    assert_eq!(d.model().selected_bytes(opened.change.active.unwrap(), 1).unwrap(),
        b"Needle path:fake -skip");
    let wrong_case_path = AtlasSearchScope::all().with_path(Kind::Directory, "SRC").unwrap()
        .with_case_sensitive(false);
    repo.begin_query_scoped(2, "needle", wrong_case_path, options(8), || false).unwrap();
    let empty = json(&finish(&mut repo, 2));
    assert!(empty.get("search_complete").flag());
    assert_eq!(empty.get("eligible_files").number(), 0);
    assert_eq!(empty.get("source_bytes_read").number(), 0);
}

#[test]
fn live_counting_finishes_overlapping_totals_after_zero_or_one_retained_row() {
    let root = fixture(&[("a.txt", b"banana banana"), ("b.txt", b"banana"), ("c.txt", b"quiet")]);
    let mut repo = repository(&root);
    for (generation, cap) in [(1, 0), (2, 1), (3, 6)] {
        repo.begin_counted_query_scoped(generation, "ana", AtlasSearchScope::all(),
            options(cap), || false).unwrap();
        let response = finish(&mut repo, generation); let page = json(&response);
        counts(&page, 6, cap as u64, 2);
        assert!(page.get("match_count_complete").flag());
        assert_eq!(response.exit_code(), if cap < 6 { EXIT_PARTIAL } else { EXIT_OK });
        if cap == 0 { assert_eq!(repo.query_progress(generation).unwrap().retained_source_bytes, 0); }
    }
    repo.begin_query(4, "ana", options(1), || false).unwrap();
    let legacy = json(&finish(&mut repo, 4));
    assert!(!legacy.get("count_all_matches").flag());
    assert!(!legacy.get("match_count_complete").flag());
    assert!(legacy.get("truncated").flag());
}

#[test]
fn live_expression_predicates_use_one_capture_and_only_primary_terms_create_hits() {
    let kept = b"needle needle required required\n";
    let root = fixture(&[("src/keep.rs", kept), ("src/blocked.rs", b"needle required forbidden\n"),
        ("src/missing.rs", b"needle\n"), ("src/other.py", b"needle required\n"),
        ("src-old/other.rs", b"needle required\n")]);
    let mut repo = repository(&root);
    let scope = AtlasSearchScope::all().with_path(Kind::Directory, "src").unwrap();
    repo.begin_expression_scoped(1, "\"needle\" required -forbidden lang:rust path:src/",
        scope, options(8), SCAN_BYTES, || false).unwrap();
    let page = json(&finish(&mut repo, 1));
    assert_eq!(page.get("mode").text(), "decoded-text-expression");
    assert_eq!(page.get("predicate_scope").text(), "same-complete-capture");
    assert_eq!(page.get("expression_scope_files").number(), 3);
    assert_eq!(page.get("expression_excluded_files").number(), 1);
    assert_eq!(page.get("retained_hits").number(), 2);
    assert_eq!(page.get("matching_files").number(), 1);
    assert!(page.get("search_complete").flag());
    fs::write(root.join("src/keep.rs"), b"needle forbidden").unwrap();
    let mut d = desk();
    let hit = repo.open_hit(&mut d, 0, 1, 1, 2, || false).unwrap();
    assert_eq!((hit.original_range.start().get(), hit.original_range.end().get()), (7, 13));
    assert_eq!(d.model().source(hit.change.active.unwrap(), 1).unwrap().bytes(), kept);
    assert_eq!(d.model().selected_bytes(hit.change.active.unwrap(), 1).unwrap(), b"needle");
}

#[test]
fn expression_metadata_filters_precede_the_live_file_and_source_byte_limits() {
    let wanted = b"needle required\n";
    let root = fixture(&[("a.py", b"needle required\n"), ("src/main.rs", wanted),
        ("src-old/main.rs", b"needle required\n")]);
    let mut repo = repository(&root);
    let mut limits = options(1); limits.max_files = 1; limits.max_source_bytes = wanted.len();
    repo.begin_expression_scoped(1, "needle required lang:rust path:src/", AtlasSearchScope::all(),
        limits, wanted.len() as u64, || false).unwrap();
    let page = json(&finish(&mut repo, 1));
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("expression_excluded_files").number(), 2);
    assert_eq!(page.get("expression_scope_files").number(), 1);
    assert_eq!(page.get("retained_hits").number(), 1);
    assert_eq!(page.get("source_bytes_read").number(), wanted.len() as u64);
    assert_eq!(page.get("scanned_input_bytes").number(), wanted.len() as u64);
}

#[test]
fn case_insensitive_expressions_preserve_utf16_ranges_and_apply_case_to_exclusions() {
    for little in [true, false] {
        let source = utf16("😀 NEEDLE required\n", little);
        let blocked = utf16("NEEDLE REQUIRED forbidden\n", little);
        let root = fixture(&[("a.txt", &source), ("b.txt", &blocked)]);
        let mut repo = repository(&root); build(&mut repo, 1);
        let mut d = desk();
        for (generation, indexed) in [(2, false), (3, true)] {
            let scope = AtlasSearchScope::all().with_case_sensitive(false);
            if indexed {
                repo.begin_indexed_expression_scoped(generation, 1, "needle REQUIRED -FORBIDDEN",
                    scope, 8, SCAN_BYTES, || false).unwrap();
            } else {
                repo.begin_expression_scoped(generation, "needle REQUIRED -FORBIDDEN",
                    scope, options(8), SCAN_BYTES, || false).unwrap();
            }
            let page = json(&finish(&mut repo, generation));
            assert!(page.get("search_complete").flag());
            assert_eq!(page.get("retained_hits").number(), 1);
            if indexed { assert_eq!(page.get("source_bytes_read").number(), 0); }
            let before = d.model().revision();
            let hit = repo.open_hit(&mut d, before, before + 1, generation, 1, || false).unwrap();
            assert_eq!(hit.source_revision.get(), if indexed { 1 } else { generation });
            assert_eq!((hit.original_range.start().get(), hit.original_range.end().get()), (8, 20));
            assert_eq!(d.model().selected_bytes(hit.change.active.unwrap(), before + 1).unwrap(), &source[8..20]);
        }
        repo.begin_expression_scoped(4, "needle REQUIRED -FORBIDDEN",
            AtlasSearchScope::all(), options(8), SCAN_BYTES, || false).unwrap();
        let sensitive = json(&finish(&mut repo, 4));
        assert!(sensitive.get("search_complete").flag());
        assert_eq!(sensitive.get("retained_hits").number(), 0);
    }
}

#[test]
fn indexed_scoped_text_opens_captured_bytes_after_live_replacement_and_index_clear() {
    let original = b"NEEDLE needle\n";
    let root = fixture(&[("src/main.rs", original), ("src-old/main.rs", b"needle\n")]);
    let mut repo = repository(&root); build(&mut repo, 1);
    fs::write(root.join("src/main.rs"), b"replacement").unwrap();
    let scope = AtlasSearchScope::all().with_path(Kind::Directory, "src").unwrap()
        .with_case_sensitive(false);
    repo.begin_indexed_query_scoped(2, 1, "needle", scope, 8, SCAN_BYTES, || false).unwrap();
    let page = json(&finish(&mut repo, 2));
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("retained_hits").number(), 2);
    assert_eq!(page.get("eligible_files").number(), 1);
    assert_eq!(page.get("source_bytes_read").number(), 0);
    assert_eq!(page.get("read_calls").number(), 0);
    let mut d = desk();
    let hit = repo.open_hit(&mut d, 0, 1, 2, 1, || false).unwrap();
    assert_eq!(hit.source_revision.get(), 1);
    repo.clear_index(3, || false).unwrap(); drop(repo);
    assert_eq!(d.model().source(hit.change.active.unwrap(), 1).unwrap().bytes(), original);
    assert_eq!(d.model().selected_bytes(hit.change.active.unwrap(), 1).unwrap(), b"NEEDLE");
}

#[test]
fn indexed_counts_use_all_captures_without_grams_or_live_reads() {
    let root = fixture(&[("a.txt", b"BANANA banana"), ("b.txt", b"banana"), ("c.txt", b"quiet")]);
    let mut repo = repository(&root); build(&mut repo, 1);
    for (generation, cap) in [(2, 0), (3, 1), (4, 6)] {
        repo.begin_indexed_query_counted_scoped(generation, 1, "ana",
            AtlasSearchScope::all().with_case_sensitive(false), cap, SCAN_BYTES, || false).unwrap();
        let response = finish(&mut repo, generation); let page = json(&response);
        counts(&page, 6, cap as u64, 2);
        assert!(page.get("match_count_complete").flag());
        assert_eq!(page.get("source_bytes_read").number(), 0);
        assert_eq!(page.get("read_calls").number(), 0);
        assert_eq!(response.exit_code(), if cap < 6 { EXIT_PARTIAL } else { EXIT_OK });
        if cap == 0 { assert_eq!(repo.query_progress(generation).unwrap().retained_source_bytes, 0); }
    }
}

#[test]
fn indexed_expressions_count_primary_hits_and_preserve_activated_captures_after_replacement() {
    let original = b"needle needle required\n";
    let root = fixture(&[("a.rs", original), ("b.rs", b"needle required forbidden\n"),
        ("c.rs", b"needle\n")]);
    let mut repo = repository(&root); build(&mut repo, 1);
    fs::write(root.join("a.rs"), b"replacement forbidden").unwrap();
    let expression = "needle required -forbidden";
    repo.begin_indexed_expression_scoped(2, 1, expression, AtlasSearchScope::all(),
        8, SCAN_BYTES, || false).unwrap();
    let uncounted = json(&finish(&mut repo, 2));
    assert!(uncounted.get("search_complete").flag());
    assert_eq!(uncounted.get("retained_hits").number(), 2);
    assert!(!uncounted.get("count_all_matches").flag());
    assert_eq!(uncounted.get("source_bytes_read").number(), 0);
    repo.begin_indexed_expression_counted_scoped(3, 1, expression, AtlasSearchScope::all(),
        1, SCAN_BYTES, || false).unwrap();
    let counted = json(&finish(&mut repo, 3));
    counts(&counted, 2, 1, 1);
    assert!(counted.get("match_count_complete").flag());
    let mut d = desk();
    let hit = repo.open_hit(&mut d, 0, 1, 3, 1, || false).unwrap();
    assert_eq!(hit.source_revision.get(), 1);
    repo.begin_indexed_expression_counted_scoped(4, 1, expression, AtlasSearchScope::all(),
        0, SCAN_BYTES, || false).unwrap();
    let count_only = json(&finish(&mut repo, 4));
    counts(&count_only, 2, 0, 1);
    assert!(count_only.get("match_count_complete").flag());
    assert_eq!(repo.query_progress(4).unwrap().retained_source_bytes, 0);
    repo.clear_index(5, || false).unwrap(); drop(repo);
    assert_eq!(d.model().source(hit.change.active.unwrap(), 1).unwrap().bytes(), original);
    assert_eq!(d.model().selected_bytes(hit.change.active.unwrap(), 1).unwrap(), b"needle");
}

#[test]
fn empty_and_missing_visible_memberships_keep_distinct_count_coverage() {
    let root = fixture(&[("a.rs", b"needle\n")]);
    for indexed in [false, true] {
        let mut repo = repository(&root); build(&mut repo, 1);
        for (generation, wire, complete) in [(2, String::new(), true),
            (3, hex(b"missing.rs"), false)] {
            let scope = AtlasSearchScope::all().with_path(Kind::Visible, &wire).unwrap();
            if indexed {
                repo.begin_indexed_query_counted_scoped(generation, 1, "needle", scope,
                    0, SCAN_BYTES, || false).unwrap();
            } else {
                repo.begin_counted_query_scoped(generation, "needle", scope, options(0), || false).unwrap();
            }
            let page = json(&finish(&mut repo, generation));
            counts(&page, 0, 0, 0);
            assert_eq!(page.get("match_count_complete").flag(), complete);
            assert_eq!(page.get("scope_region_missing_files").number(), u64::from(!complete));
            assert_eq!(page.get("source_bytes_read").number(), 0);
        }
    }
}

#[test]
fn incomplete_expression_verification_never_publishes_unqualified_hits_or_complete_counts() {
    let root = fixture(&[("a.rs", b"needle required trailing marker\n")]);
    let mut repo = repository(&root); build(&mut repo, 1);
    for (generation, indexed) in [(2, false), (3, true)] {
        if indexed {
            repo.begin_indexed_expression_counted_scoped(generation, 1, "needle required -forbidden",
                AtlasSearchScope::all(), 1, 8, || false).unwrap();
        } else {
            repo.begin_expression_counted_scoped(generation, "needle required -forbidden",
                AtlasSearchScope::all(), options(1), 8, || false).unwrap();
        }
        let page = json(&finish(&mut repo, generation));
        counts(&page, 0, 0, 0);
        assert!(!page.get("search_complete").flag());
        assert!(!page.get("match_count_complete").flag());
        assert!(page.get("scanned_input_bytes").number() <= 8);
        if indexed { assert_eq!(page.get("source_bytes_read").number(), 0); }
    }
    repo.begin_expression_counted_scoped(4, "needle required -forbidden", AtlasSearchScope::all(),
        options(0), SCAN_BYTES, || false).unwrap();
    let complete = json(&finish(&mut repo, 4));
    counts(&complete, 1, 0, 1);
    assert!(complete.get("match_count_complete").flag());
    let empty = AtlasSearchScope::all().with_path(Kind::Directory, "missing").unwrap();
    repo.begin_expression_counted_scoped(5, "needle required -forbidden", empty,
        options(0), 0, || false).unwrap();
    let page = json(&finish(&mut repo, 5));
    counts(&page, 0, 0, 0);
    assert!(page.get("match_count_complete").flag());
    assert_eq!(page.get("source_bytes_read").number(), 0);
    assert_eq!(page.get("scanned_input_bytes").number(), 0);
}

#[test]
fn invalid_canceled_and_superseded_modes_keep_accepted_results_and_desk_state() {
    let root = fixture(&[("a.rs", b"needle\n"), ("b.rs", b"needle\n")]);
    let mut repo = repository(&root); build(&mut repo, 1);
    repo.begin_query_scoped(2, "needle", AtlasSearchScope::all(), options(8), || false).unwrap();
    finish(&mut repo, 2);
    let mut d = desk();
    let opened = repo.open_hit(&mut d, 0, 1, 2, 1, || false).unwrap();
    let error = repo.begin_expression_scoped(3, "\"needle", AtlasSearchScope::all(),
        options(8), SCAN_BYTES, || false).err().unwrap();
    assert!(error.to_string().contains("QUERY_SYNTAX_ERROR"));
    assert_eq!(repo.work_state().accepted_query, Some(2));
    assert_eq!(repo.work_state().pending_query, None);
    assert_eq!(repo.begin_indexed_expression_scoped(4, 99, "needle", AtlasSearchScope::all(),
        8, SCAN_BYTES, || false).err(),
        Some(DeskRepositoryError::Search(AtlasSearchError::StaleIndex)));
    assert!(matches!(repo.begin_counted_query_scoped(5, "needle", AtlasSearchScope::all(),
        options(0), || true), Err(error) if error.is_canceled()));
    repo.begin_indexed_query_scoped(6, 1, "needle", AtlasSearchScope::all(),
        8, SCAN_BYTES, || false).unwrap();
    let before = repo.query_progress(6).unwrap();
    assert_eq!(repo.step_query(6, 1, || false).err(), Some(DeskRepositoryError::StaleStep));
    assert_eq!(repo.query_progress(6).unwrap(), before);
    repo.begin_indexed_expression_counted_scoped(7, 1, "needle", AtlasSearchScope::all(),
        0, SCAN_BYTES, || false).unwrap();
    assert_eq!(repo.cancel_query(6, || false).err(),
        Some(DeskRepositoryError::Search(AtlasSearchError::StaleQuery)));
    assert_eq!(repo.work_state().pending_query, Some(7));
    repo.cancel_query(7, || false).unwrap();
    assert_eq!(repo.begin_query_scoped(8, "needle", AtlasSearchScope::all(), options(0), || false).err(),
        Some(DeskRepositoryError::Search(AtlasSearchError::InvalidLimits)));
    assert_eq!(repo.work_state().accepted_query, Some(2));
    assert_eq!(json(&repo.page(2, 0, 8, || false).unwrap()).get("retained_hits").number(), 2);
    assert_eq!(d.model().revision(), 1);
    assert_eq!(d.model().selected_bytes(opened.change.active.unwrap(), 1).unwrap(), b"needle");
}

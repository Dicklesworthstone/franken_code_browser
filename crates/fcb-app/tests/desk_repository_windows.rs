#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Captured-index windows through the public reading-desk repository host.
//! FCB-028 / fcb-qtb.2; code-first, independent batch verification pending.
mod support;
use support::{parse, Json};
use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::host::{HostResponse, atlas_session::AtlasSessionOptions,
    atlas_search::{AtlasIndexOptions, AtlasSearchError, AtlasSearchPathKind, AtlasSearchScope},
    desk::{DeskSession, DeskLimits, DeskCommand, repository::{DeskRepository, DeskRepositoryError}}};

fn fixture(files: &[(&str, &[u8])]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-window-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    for (name, source) in files {
        let path = root.join(name); fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    root
}
fn repository(root: &Path, options: AtlasIndexOptions) -> DeskRepository {
    let mut repo = DeskRepository::open(ArenaOwnerId::new(920).unwrap(), root,
        AtlasSessionOptions::default(), || false).unwrap();
    repo.begin_index(1, options, || false).unwrap();
    for _ in 0..16 {
        let progress = repo.index_progress(1).unwrap();
        if !progress.in_progress { return repo; }
        repo.step_index(1, progress.steps, || false).unwrap();
    }
    panic!("bounded fixture index did not finish");
}
fn finish(repo: &mut DeskRepository, generation: u64) {
    for _ in 0..16 {
        let progress = repo.query_progress(generation).unwrap();
        if !progress.is_running() { return; }
        repo.step_query(generation, progress.step_count, || false).unwrap();
    }
    panic!("bounded fixture query did not finish");
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn page(repo: &mut DeskRepository, generation: u64) -> Json {
    json(&repo.page(generation, 0, 10, || false).unwrap())
}
fn desk(owner: u64) -> DeskSession {
    DeskSession::new(ArenaOwnerId::new(owner).unwrap(), DeskLimits::default()).unwrap()
}

#[test]
fn later_than_4096_imports_exact_indexed_source_and_survives_detachment_and_checkpoint() {
    let source = b"needle ".repeat(5000);
    let root = fixture(&[("main.rs", &source)]);
    let mut repo = repository(&root, AtlasIndexOptions::default());
    repo.begin_indexed_window_scoped(2, 1, "needle", AtlasSearchScope::all(), false,
        true, 0, 2, 100_000, || false).unwrap();
    finish(&mut repo, 2);
    let first = page(&mut repo, 2);
    assert_eq!(first.get("matches_seen").number(), 5000);
    assert_eq!(first.get("next_match_offset").number(), 2);
    let mut desk = desk(921);
    let opened = repo.open_hit(&mut desk, 0, 1, 2, 1, || false).unwrap();
    let pane = opened.change.active.unwrap();
    desk.apply(1, 2, DeskCommand::Bookmark { pane, label: "first occurrence".into() }, || false).unwrap();

    fs::write(root.join("main.rs"), b"replacement without the searched text").unwrap();
    repo.begin_indexed_window_scoped(3, 1, "needle", AtlasSearchScope::all(), false,
        true, 4096, 2, 100_000, || false).unwrap();
    // A candidate does not relabel or withdraw the accepted generation.
    assert_eq!(page(&mut repo, 2), first);
    assert_eq!(repo.query_progress(3).unwrap().source_bytes_read, 0);
    finish(&mut repo, 3);
    let later = page(&mut repo, 3);
    assert_eq!(later.get("result_offset").number(), 4096);
    assert_eq!(later.get("skipped_matches").number(), 4096);
    assert_eq!(later.get("matches_seen").number(), 5000);
    assert_eq!(later.get("retained_hits").number(), 2);
    assert!(later.get("match_count_complete").flag());
    assert!(later.get("truncated").flag());
    assert_eq!(later.get("next_match_offset").number(), 4098);
    assert_eq!(later.get("source_bytes_read").number(), 0);
    assert_eq!(later.get("read_calls").number(), 0);
    assert!(repo.page(2, 0, 1, || false).is_err());
    let late_hit = &later.get("hits").array()[0];
    assert_eq!(late_hit.get("original_range").get("start").number(), 4096 * 7);
    let imported = repo.open_hit(&mut desk, 2, 3, 3, late_hit.get("hit_id").number(), || false).unwrap();
    assert!(imported.reused_source);
    assert_eq!(imported.desk_revision, opened.desk_revision);
    assert_eq!(desk.model().retained_source_count(), 1);
    assert_eq!(desk.model().selected_bytes(imported.change.active.unwrap(), 3).unwrap(), b"needle");

    repo.clear_index(4, || false).unwrap(); drop(repo);
    assert_eq!(desk.model().source(imported.change.active.unwrap(), 3).unwrap().bytes(), source);
    let checkpoint = root.join("late-match.fcbk");
    assert_eq!(desk.save_checkpoint(3, &checkpoint, 100_000, || false).error(), None);
    let mut restored = DeskSession::new(ArenaOwnerId::new(922).unwrap(), DeskLimits::default()).unwrap();
    let pane = restored.restore_checkpoint_file(0, 1, &checkpoint, || false).unwrap().active.unwrap();
    assert_eq!(restored.model().selected_bytes(pane, 1).unwrap(), b"needle");
    assert_eq!(restored.model().source(pane, 1).unwrap().bytes(), source);
    assert_eq!(restored.model().bookmarks()[0].label(), "first occurrence");
}

#[test]
fn expression_windows_count_only_qualified_overlaps_across_utf8_and_utf16_captures() {
    for little in [true, false] {
        let utf16: Vec<u8> = "\u{feff}BANANA required".encode_utf16()
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
        let root = fixture(&[("src/a.rs", b"banana required"),
            ("src/b.rs", b"banana required forbidden"), ("src/c.rs", &utf16),
            ("src/ignored.py", b"banana required"), ("src-old/other.rs", b"banana required")]);
        // No grams: exact fallback must preserve both query eligibility and offsets.
        let mut repo = repository(&root, AtlasIndexOptions { max_index_grams: 0, ..Default::default() });
        let scope = AtlasSearchScope::all().with_case_sensitive(false)
            .with_path(AtlasSearchPathKind::Directory, "src").unwrap();
        repo.begin_indexed_window_scoped(2, 1, "ana required -forbidden lang:rust", scope,
            true, true, 1, 2, 100_000, || false).unwrap();
        finish(&mut repo, 2);
        let page = page(&mut repo, 2);
        assert_eq!(page.get("mode").text(), "decoded-text-expression");
        assert_eq!(page.get("matches_seen").number(), 4);
        assert_eq!(page.get("skipped_matches").number(), 1);
        assert_eq!(page.get("next_match_offset").number(), 3);
        assert_eq!(page.get("retained_hits").number(), 2);
        assert!(page.get("match_count_complete").flag());
        assert!(page.get("fallback_files").number() > 0);
        assert_eq!(page.get("source_bytes_read").number(), 0);
        let hits = page.get("hits").array();
        assert_eq!(hits[0].get("original_range").get("start").number(), 3);
        assert_eq!(hits[0].get("original_range").get("end").number(), 6);
        assert_eq!(hits[1].get("original_range").get("start").number(), 4);
        assert_eq!(hits[1].get("original_range").get("end").number(), 10);
        let mut desk = desk(923);
        let imported = repo.open_hit(&mut desk, 0, 1, 2, hits[1].get("hit_id").number(), || false).unwrap();
        assert_eq!(desk.model().selected_bytes(imported.change.active.unwrap(), 1).unwrap(), &utf16[4..10]);
    }
}

#[test]
fn failed_or_canceled_windows_preserve_accepted_rows_and_generation_identity() {
    let root = fixture(&[("a.rs", b"needle required"), ("b.rs", b"needle required")]);
    let mut repo = repository(&root, AtlasIndexOptions::default());
    repo.begin_indexed_window_scoped(2, 1, "needle", AtlasSearchScope::all(), false,
        true, 0, 1, 1000, || false).unwrap(); finish(&mut repo, 2);
    let accepted = page(&mut repo, 2);
    assert!(repo.begin_indexed_window_scoped(3, 1, "\"needle", AtlasSearchScope::all(), true,
        true, 1, 1, 1000, || false).is_err());
    assert!(matches!(repo.begin_indexed_window_scoped(4, 1, "needle", AtlasSearchScope::all(), false,
        true, 1, 1, 1000, || true), Err(e) if e.is_canceled()));
    assert_eq!(page(&mut repo, 2), accepted);
    repo.begin_indexed_window_scoped(5, 1, "needle", AtlasSearchScope::all(), false,
        true, 1, 1, 1000, || false).unwrap();
    repo.step_query(5, 0, || false).unwrap();
    assert!(repo.query_progress(5).unwrap().is_running());
    let before_stale_step = repo.query_progress(5).unwrap();
    assert!(matches!(repo.step_query(5, 0, || false), Err(DeskRepositoryError::StaleStep)));
    assert_eq!(repo.query_progress(5).unwrap(), before_stale_step);
    assert_eq!(repo.cancel_query(4, || false).err(), Some(DeskRepositoryError::Search(AtlasSearchError::StaleQuery)));
    repo.cancel_query(5, || false).unwrap();
    assert_eq!(page(&mut repo, 2), accepted);
    assert_eq!(repo.work_state().accepted_query, Some(2));
    assert!(!repo.work_state().has_pending());
}

#[test]
fn full_width_offsets_and_count_only_windows_do_not_allocate_skipped_results() {
    let root = fixture(&[("main.rs", b"needle needle needle")]);
    let mut repo = repository(&root, AtlasIndexOptions::default());
    repo.begin_indexed_window_scoped(2, 1, "needle", AtlasSearchScope::all(), false,
        true, u64::MAX, 1, 1000, || false).unwrap(); finish(&mut repo, 2);
    let beyond = page(&mut repo, 2);
    assert_eq!(beyond.get("result_offset").number(), u64::MAX);
    assert_eq!(beyond.get("matches_seen").number(), 3);
    assert_eq!(beyond.get("skipped_matches").number(), 3);
    assert_eq!(beyond.get("retained_hits").number(), 0);
    assert_eq!(beyond.get("retained_source_bytes").number(), 0);
    assert!(beyond.get("match_count_complete").flag());
    assert!(!beyond.get("has_more_matches").flag());
    assert_eq!(beyond.get("next_match_offset"), &Json::Null);
    repo.begin_indexed_window_scoped(3, 1, "needle", AtlasSearchScope::all(), false,
        true, 1, 0, 1000, || false).unwrap(); finish(&mut repo, 3);
    let counted = page(&mut repo, 3);
    assert_eq!(counted.get("matches_seen").number(), 3);
    assert_eq!(counted.get("retained_hits").number(), 0);
    assert_eq!(counted.get("retained_source_bytes").number(), 0);
    assert!(counted.get("has_more_matches").flag());
    assert_eq!(counted.get("next_match_offset"), &Json::Null);
}

#[test]
fn exhausted_verification_and_unavailable_captures_do_not_prove_an_empty_later_window() {
    let root = fixture(&[("a.rs", b"needle"), ("b.rs", b"not captured")]);
    let mut repo = repository(&root, AtlasIndexOptions { max_file_bytes: 6, ..Default::default() });
    repo.begin_indexed_window_scoped(2, 1, "needle", AtlasSearchScope::all(), false,
        true, 1, 1, 1000, || false).unwrap(); finish(&mut repo, 2);
    let unavailable = page(&mut repo, 2);
    assert_eq!(unavailable.get("matches_seen").number(), 1);
    assert_eq!(unavailable.get("retained_hits").number(), 0);
    assert!(!unavailable.get("match_count_complete").flag());
    assert!(unavailable.get("unavailable_files").number() > 0);
    repo.begin_indexed_window_scoped(3, 1, "needle", AtlasSearchScope::all(), false,
        true, 1, 1, 0, || false).unwrap(); finish(&mut repo, 3);
    let limited = page(&mut repo, 3);
    assert_eq!(limited.get("matches_seen").number(), 0);
    assert!(!limited.get("match_count_complete").flag());
    assert_eq!(limited.get("next_match_offset"), &Json::Null);
    assert_eq!(limited.get("stop_reason").text(), "verification-byte-limit");
}

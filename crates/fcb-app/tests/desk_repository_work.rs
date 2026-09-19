#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{parse, Json};
use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, atlas_session::AtlasSessionOptions,
    atlas_search::{AtlasSearchOptions, AtlasIndexOptions, AtlasSearchError},
    desk::{DeskSession, DeskLimits, DeskCommand, repository::{DeskRepository, DeskRepositoryError}}};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-work-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn repository(root: &Path) -> DeskRepository {
    DeskRepository::open(ArenaOwnerId::new(900).unwrap(), root, AtlasSessionOptions::default(), || false).unwrap()
}
fn desk() -> DeskSession { DeskSession::new(ArenaOwnerId::new(901).unwrap(), DeskLimits::default()).unwrap() }
fn json(r: &HostResponse) -> Json { parse(r.as_str().as_bytes()).unwrap() }
fn finish_query(r: &mut DeskRepository, generation: u64) {
    for _ in 0..10 {
        let progress = r.query_progress(generation).unwrap();
        if !progress.is_running() { return; }
        r.step_query(generation, progress.step_count, || false).unwrap();
    }
    panic!("small fixture did not finish");
}
fn build(r: &mut DeskRepository, generation: u64, options: AtlasIndexOptions) {
    r.begin_index(generation, options, || false).unwrap();
    for _ in 0..10 {
        let p = r.index_progress(generation).unwrap();
        if !p.in_progress { return; }
        r.step_index(generation, p.steps, || false).unwrap();
    }
    panic!("small fixture did not finish");
}

#[test]
fn admission_inspection_and_paging_do_not_scan_while_steps_visit_one_file() {
    let root = fixture();
    for name in ["a.rs", "b.rs", "c.rs"] { fs::write(root.join(name), b"needle").unwrap(); }
    let mut r = repository(&root);
    let response = r.begin_query(1, "needle", AtlasSearchOptions::default(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let initial = r.query_progress(1).unwrap();
    assert!(initial.is_running()); assert_eq!(initial.source_bytes_read, 0); assert_eq!(initial.examined_files, 0);
    assert_eq!(json(&r.page(1, 0, 10, || false).unwrap()).get("hits").array().len(), 0);
    assert_eq!(r.query_progress(1).unwrap(), initial);
    for step in 0..3 {
        r.step_query(1, step, || false).unwrap();
        let p = r.query_progress(1).unwrap();
        assert_eq!(p.last_step_files, 1); assert_eq!(p.examined_files as u64, step + 1);
        assert_eq!(p.retained_hits as u64, step + 1);
        let page = r.page(1, 0, 10, || false).unwrap();
        assert_eq!(json(&page).get("hits").array().len() as u64, step + 1);
        assert_eq!(r.query_progress(1).unwrap(), p);
    }
    assert_eq!(r.work_state().accepted_query, Some(1)); assert_eq!(r.work_state().pending_query, None);
    assert!(r.query_progress(1).unwrap().complete);
}

#[test]
fn stale_step_and_stale_cancel_do_not_advance_or_retire_new_work() {
    let root = fixture(); for name in ["a.rs", "b.rs"] { fs::write(root.join(name), b"needle").unwrap(); }
    let mut r = repository(&root); r.begin_query(1, "needle", AtlasSearchOptions::default(), || false).unwrap();
    r.step_query(1, 0, || false).unwrap(); let before = r.query_progress(1).unwrap();
    assert!(matches!(r.step_query(1, 0, || false), Err(DeskRepositoryError::StaleStep)));
    assert!(matches!(r.step_query(1, 9, || false), Err(DeskRepositoryError::StaleStep)));
    assert_eq!(r.query_progress(1).unwrap(), before);
    r.begin_query(2, "needle", AtlasSearchOptions::default(), || false).unwrap();
    assert_eq!(r.cancel_query(1, || false), Err(DeskRepositoryError::Search(AtlasSearchError::StaleQuery)));
    assert_eq!(r.work_state().pending_query, Some(2));
    assert_eq!(r.query_progress(2).unwrap().step_count, 0);
    r.cancel_query(2, || false).unwrap(); assert!(!r.work_state().has_pending());
}

#[test]
fn early_hit_survives_cancel_and_checkpoint_even_after_live_source_replacement() {
    let root = fixture(); let path = root.join("a.rs");
    fs::write(&path, b"old needle").unwrap(); fs::write(root.join("b.rs"), b"later needle").unwrap();
    let mut r = repository(&root); let mut d = desk();
    r.begin_query(1, "needle", AtlasSearchOptions::default(), || false).unwrap();
    r.step_query(1, 0, || false).unwrap(); assert!(r.query_progress(1).unwrap().is_running());
    fs::write(&path, b"new bytes").unwrap();
    let pane = r.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap().change.active.unwrap();
    d.apply(1, 2, DeskCommand::Pin { pane, pinned: true }, || false).unwrap();
    r.cancel_query(1, || false).unwrap();
    assert_eq!(d.model().source(pane, 2).unwrap().bytes(), b"old needle");
    assert_eq!(d.model().selected_bytes(pane, 2).unwrap(), b"needle");
    assert!(r.open_hit(&mut d, 2, 3, 1, 1, || false).is_err());
    let checkpoint = root.join("reading.fcbk"); assert_eq!(d.save_checkpoint(2, &checkpoint, 100, || false).error(), None);
    drop(d); drop(r);
    let mut restored = desk(); let pane = restored.restore_checkpoint_file(0, 1, &checkpoint, || false).unwrap().active.unwrap();
    assert_eq!(restored.model().selected_bytes(pane, 1).unwrap(), b"needle");
    assert!(restored.model().panes().active_pane().unwrap().is_pinned);
}

#[test]
fn accepted_query_stays_available_during_replacement_and_after_cancel() {
    let root = fixture(); fs::write(root.join("a.rs"), b"old new").unwrap(); let mut r = repository(&root);
    r.search(1, "old", AtlasSearchOptions::default(), || false).unwrap();
    r.begin_query(2, "new", AtlasSearchOptions::default(), || false).unwrap();
    let accepted = r.query_progress(1).unwrap();
    assert_eq!(r.step_query(1, accepted.step_count, || false).unwrap().exit_code(), EXIT_OK);
    assert_eq!(r.work_state().pending_query, Some(2));
    assert_eq!(r.cancel_query(2, || false).unwrap().accepted_query, Some(1));
    assert_eq!(json(&r.page(1, 0, 10, || false).unwrap()).get("hits").array().len(), 1);
}

#[test]
fn pre_canceled_step_and_cancellation_request_leave_work_reconcilable() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap(); let mut r = repository(&root);
    r.begin_query(1, "needle", AtlasSearchOptions::default(), || false).unwrap();
    assert!(matches!(r.step_query(1, 0, || true), Err(e) if e.is_canceled()));
    assert!(matches!(r.cancel_query(1, || true), Err(e) if e.is_canceled()));
    assert_eq!(r.query_progress(1).unwrap().step_count, 0);
    finish_query(&mut r, 1); assert!(r.query_progress(1).unwrap().complete);
}

#[test]
fn a_terminal_quota_stop_remains_partial_and_terminal_retries_do_no_work() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle needle").unwrap(); let mut r = repository(&root);
    r.begin_query(1, "needle", AtlasSearchOptions { max_matches: 1, ..Default::default() }, || false).unwrap();
    assert_eq!(r.step_query(1, 0, || false).unwrap().exit_code(), EXIT_PARTIAL);
    let p = r.query_progress(1).unwrap(); assert!(!p.is_running()); assert!(!p.complete); assert!(p.truncated);
    assert_eq!(r.step_query(1, p.step_count, || false).unwrap().exit_code(), EXIT_PARTIAL);
    assert_eq!(r.query_progress(1).unwrap(), p);
}

#[test]
fn index_build_is_incremental_and_indexed_queries_do_not_reopen_live_files() {
    let root = fixture(); let a = root.join("a.rs"); let b = root.join("b.rs");
    fs::write(&a, b"old needle").unwrap(); fs::write(&b, b"irrelevant").unwrap(); let mut r = repository(&root);
    r.begin_index(1, AtlasIndexOptions::default(), || false).unwrap();
    assert_eq!(r.index_progress(1).unwrap().source_bytes_read, 0);
    r.step_index(1, 0, || false).unwrap(); let p = r.index_progress(1).unwrap();
    assert_eq!(p.last_step_files, 1); assert!(p.in_progress);
    r.index_info(1, || false).unwrap(); assert_eq!(r.index_progress(1).unwrap(), p);
    assert!(matches!(r.step_index(1, 0, || false), Err(DeskRepositoryError::StaleStep)));
    r.step_index(1, 1, || false).unwrap(); assert_eq!(r.work_state().accepted_index, Some(1));
    fs::rename(&a, root.join("moved.rs")).unwrap(); fs::write(&b, b"needle was not here").unwrap();
    r.begin_indexed_query(2, 1, "needle", 10, 1000, || false).unwrap(); finish_query(&mut r, 2);
    let p = r.query_progress(2).unwrap(); assert_eq!(p.source_bytes_read, 0); assert_eq!(p.read_calls, 0);
    assert!(p.complete); assert_eq!(p.retained_hits, 1);
    let mut d = desk(); let opened = r.open_hit(&mut d, 0, 1, 2, 1, || false).unwrap();
    assert_eq!(d.model().source(opened.change.active.unwrap(), 1).unwrap().bytes(), b"old needle");
    r.search_indexed(3, 1, "old", 10, 1000, || false).unwrap();
    let again = r.open_hit(&mut d, 1, 2, 3, 1, || false).unwrap(); assert!(again.reused_source);
    assert_eq!(d.model().retained_source_count(), 1);
}

#[test]
fn zero_gram_budget_keeps_exact_fallback_instead_of_suppressing_results() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap(); let mut r = repository(&root);
    build(&mut r, 1, AtlasIndexOptions { max_index_grams: 0, ..Default::default() });
    assert!(r.index_progress(1).unwrap().uncovered_files > 0);
    let response = r.search_indexed(2, 1, "needle", 10, 100, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK); assert_eq!(r.query_progress(2).unwrap().retained_hits, 1);
    assert!(json(&response).get("fallback_files").number() > 0);
    assert_eq!(r.query_progress(2).unwrap().source_bytes_read, 0);
}

#[test]
fn canceling_replacement_index_preserves_old_index_and_query_in_flight() {
    let root = fixture(); for name in ["a.rs", "b.rs"] { fs::write(root.join(name), b"needle").unwrap(); }
    let mut r = repository(&root); build(&mut r, 1, AtlasIndexOptions::default());
    r.begin_index(2, AtlasIndexOptions::default(), || false).unwrap();
    r.begin_indexed_query(3, 1, "needle", 10, 1000, || false).unwrap(); r.step_query(3, 0, || false).unwrap();
    r.step_index(2, 0, || false).unwrap();
    assert_eq!(r.cancel_index(1, || false), Err(DeskRepositoryError::Search(AtlasSearchError::StaleIndex)));
    let state = r.cancel_index(2, || false).unwrap();
    assert_eq!(state.accepted_index, Some(1)); assert_eq!(state.pending_query, Some(3)); assert_eq!(state.pending_index, None);
    finish_query(&mut r, 3); assert!(r.query_progress(3).unwrap().complete);
}

#[test]
fn index_publication_retires_old_index_work_but_never_a_pinned_reader() {
    let root = fixture(); for name in ["a.rs", "b.rs"] { fs::write(root.join(name), b"needle").unwrap(); }
    let mut r = repository(&root); build(&mut r, 1, AtlasIndexOptions::default());
    r.begin_index(2, AtlasIndexOptions::default(), || false).unwrap();
    r.begin_indexed_query(3, 1, "needle", 10, 1000, || false).unwrap(); r.step_query(3, 0, || false).unwrap();
    let mut d = desk(); let pane = r.open_hit(&mut d, 0, 1, 3, 1, || false).unwrap().change.active.unwrap();
    d.apply(1, 2, DeskCommand::Pin { pane, pinned: true }, || false).unwrap();
    r.step_index(2, 0, || false).unwrap(); r.step_index(2, 1, || false).unwrap();
    assert_eq!(r.work_state().pending_query, None); assert_eq!(r.work_state().accepted_index, Some(2));
    assert!(r.step_query(3, 1, || false).is_err());
    assert_eq!(d.model().selected_bytes(pane, 2).unwrap(), b"needle");
    r.clear_index(4, || false).unwrap(); assert_eq!(r.work_state().accepted_index, None);
    assert_eq!(d.model().selected_bytes(pane, 2).unwrap(), b"needle");
}

#[test]
fn index_publication_preserves_a_pending_live_query_and_missing_index_is_explicit() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap(); let mut r = repository(&root);
    assert!(matches!(r.begin_indexed_query(1, 99, "needle", 10, 1000, || false),
        Err(DeskRepositoryError::Search(AtlasSearchError::MissingIndex))));
    r.begin_index(2, AtlasIndexOptions::default(), || false).unwrap();
    r.begin_query(3, "needle", AtlasSearchOptions::default(), || false).unwrap();
    r.step_index(2, 0, || false).unwrap(); assert_eq!(r.work_state().pending_query, Some(3));
    finish_query(&mut r, 3); assert!(r.query_progress(3).unwrap().complete);
}

#[test]
fn unavailable_index_members_and_verification_limits_never_claim_complete_absence() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap(); fs::write(root.join("b.rs"), b"too long").unwrap();
    let mut r = repository(&root); build(&mut r, 1, AtlasIndexOptions { max_file_bytes: 6, max_index_grams: 0, ..Default::default() });
    let result = r.search_indexed(2, 1, "needle", 10, 100, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL); assert!(!r.query_progress(2).unwrap().complete);
    assert_eq!(r.query_progress(2).unwrap().retained_hits, 1);
    let result = r.search_indexed(3, 1, "needle", 10, 0, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL); assert!(!r.query_progress(3).unwrap().complete);
    let mut d = desk(); let response = json(&d.repository_work_response(&r, || false).unwrap());
    assert_eq!(response.get("repository_index_generation").number(), 1);
    assert!(!response.get("work_pending").flag()); assert_eq!(d.model().revision(), 0);
}

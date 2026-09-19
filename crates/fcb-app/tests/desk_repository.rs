#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{parse, Json};
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::host::{HostResponse, atlas_session::AtlasSessionOptions, atlas_search::AtlasSearchOptions,
    desk::{DeskSession, DeskLimits, DeskCommand, DeskError, DeskSessionError,
        repository::{DeskRepository, DeskRepositoryError}}};
use fcb_app::{EXIT_PARTIAL, EXIT_OK};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-repo-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn desk(n: u64) -> DeskSession { DeskSession::new(ArenaOwnerId::new(n).unwrap(), DeskLimits::default()).unwrap() }
fn repo(n: u64, root: &std::path::Path) -> DeskRepository {
    DeskRepository::open(ArenaOwnerId::new(n).unwrap(), root, AtlasSessionOptions::default(), || false).unwrap()
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn find(repo: &mut DeskRepository, generation: u64, needle: &str) {
    assert_eq!(repo.search(generation, needle, AtlasSearchOptions::default(), || false).unwrap().exit_code(), EXIT_OK);
}
fn apply(d: &mut DeskSession, command: DeskCommand) {
    d.apply(d.model().revision(), d.model().last_attempt() + 1, command, || false).unwrap();
}

#[test]
fn search_to_pinned_bookmark_to_checkpoint_survives_repository_and_live_source_loss() {
    let root = fixture(); let path = root.join("a.rs"); fs::write(&path, b"old needle text").unwrap();
    let mut repository = repo(101, &root); let mut d = desk(102); find(&mut repository, 1, "needle");
    fs::write(&path, b"changed live bytes").unwrap();
    let opened = repository.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    let pane = opened.change.active.unwrap();
    assert_ne!(opened.source_file.owner(), opened.desk_file.owner());
    assert_eq!(d.model().source(pane, 1).unwrap().bytes(), b"old needle text");
    assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), b"needle");
    let receipt = json(&d.repository_open_response(&opened).unwrap());
    assert!(!receipt.get("source_reopened").flag());
    assert_eq!(receipt.get("repository_owner").number(), 101);
    apply(&mut d, DeskCommand::Pin { pane, pinned: true });
    apply(&mut d, DeskCommand::Bookmark { pane, label: "exact evidence".into() });
    drop(repository);
    fs::rename(&path, root.join("moved.rs")).unwrap();
    let saved = root.join("reading.fcbk");
    assert_eq!(d.save_checkpoint(3, &saved, 1024, || false).error(), None); drop(d);
    let mut restored = desk(103);
    let pane = restored.restore_checkpoint_file(0, 1, &saved, || false).unwrap().active.unwrap();
    assert_eq!(restored.model().selected_bytes(pane, 1).unwrap(), b"needle");
    assert_eq!(restored.model().source(pane, 1).unwrap().bytes(), b"old needle text");
    assert_eq!(restored.model().bookmarks()[0].label(), "exact evidence");
    assert!(restored.model().panes().active_pane().unwrap().is_pinned);
}

#[test]
fn repeated_occurrences_reuse_one_source_instead_of_exhausting_source_limit() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle x needle").unwrap();
    let mut repository = repo(110, &root); let mut d = desk(111); find(&mut repository, 1, "needle");
    let first = repository.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    for n in 2..=80 {
        let next = repository.open_hit(&mut d, n - 1, n, 1, n % 2 + 1, || false).unwrap();
        assert!(next.reused_source); assert_eq!(next.desk_file, first.desk_file);
        assert_eq!(next.desk_revision, first.desk_revision);
        assert_eq!(d.model().retained_source_count(), 1);
        assert_eq!(d.model().selected_bytes(next.change.active.unwrap(), n).unwrap(), b"needle");
    }
}

#[test]
fn new_query_opens_new_bytes_without_rebinding_a_pinned_old_reader() {
    let root = fixture(); let path = root.join("a.rs"); fs::write(&path, b"old needle").unwrap();
    let mut repository = repo(120, &root); let mut d = desk(121); find(&mut repository, 1, "needle");
    let first = repository.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    let old = first.change.active.unwrap(); apply(&mut d, DeskCommand::Pin { pane: old, pinned: true });
    fs::write(&path, b"new needle").unwrap(); find(&mut repository, 2, "needle");
    let next = repository.open_hit(&mut d, 2, 3, 2, 1, || false).unwrap();
    assert_ne!(next.change.active.unwrap(), old); assert_ne!(next.desk_revision, first.desk_revision);
    assert_eq!(d.model().source(old, 3).unwrap().bytes(), b"old needle");
    assert_eq!(d.model().source(next.change.active.unwrap(), 3).unwrap().bytes(), b"new needle");
    assert!(repository.open_hit(&mut d, 3, 4, 1, 1, || false).is_err());
    assert_eq!(d.model().revision(), 3);
}

#[test]
fn canceled_replacement_preserves_accepted_repository_query_and_local_search() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let mut repository = repo(130, &root); let mut d = desk(131); find(&mut repository, 1, "needle");
    let pane = repository.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap().change.active.unwrap();
    d.search(1, pane, 1, "needle", 10, 100, || false).unwrap();
    assert!(matches!(repository.search(2, "other", AtlasSearchOptions::default(), || true), Err(e) if e.is_canceled()));
    assert_eq!(repository.accepted_generation(), Some(1)); assert_eq!(d.accepted_query(), Some(1));
    assert!(matches!(repository.open_hit(&mut d, 1, 2, 1, 1, || true), Err(e) if e.is_canceled()));
    assert_eq!(d.model().revision(), 1); assert_eq!(d.accepted_query(), Some(1));
    d.activate_hit(1, 3, pane, 1, 0, || false).unwrap();
    assert_eq!(d.model().selected_bytes(pane, 3).unwrap(), b"needle");
}

#[test]
fn foreign_desks_stale_requests_and_missing_hits_do_not_mutate_readers() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let mut repository = repo(140, &root); let mut d = desk(141); find(&mut repository, 1, "needle");
    assert_eq!(repository.open_hit(&mut d, 1, 1, 1, 1, || false),
        Err(DeskRepositoryError::Desk(DeskSessionError::Desk(DeskError::StaleRevision))));
    assert!(repository.open_hit(&mut d, 0, 1, 1, 0, || false).is_err());
    assert_eq!(d.model().revision(), 0);
    repository.open_hit(&mut d, 0, 2, 1, 1, || false).unwrap();
    assert_eq!(repository.open_hit(&mut d, 2, 2, 1, 1, || false),
        Err(DeskRepositoryError::Desk(DeskSessionError::Desk(DeskError::StaleAttempt))));
    let mut foreign = desk(142);
    assert_eq!(repository.open_hit(&mut foreign, 0, 1, 1, 1, || false), Err(DeskRepositoryError::WrongDesk));
    assert_eq!(foreign.model().revision(), 0); assert_eq!(d.model().revision(), 2);
}

#[test]
fn source_budget_refusal_keeps_existing_state_and_repository_hit_available() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let mut repository = repo(150, &root); find(&mut repository, 1, "needle");
    let mut limited = DeskSession::new(ArenaOwnerId::new(151).unwrap(), DeskLimits { source_bytes: 3, ..DeskLimits::default() }).unwrap();
    assert_eq!(repository.open_hit(&mut limited, 0, 1, 1, 1, || false),
        Err(DeskRepositoryError::Desk(DeskSessionError::Desk(DeskError::SourceLimit))));
    assert_eq!(limited.model().revision(), 0);
    assert_eq!(json(&repository.page(1, 0, 10, || false).unwrap()).get("hits").array().len(), 1);
    let mut admitted = desk(152); repository.open_hit(&mut admitted, 0, 1, 1, 1, || false).unwrap();
}

#[test]
fn closed_pane_bookmark_retains_import_and_complete_retirement_rebases_next_import() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let mut repository = repo(160, &root); let mut d = desk(161); find(&mut repository, 1, "needle");
    let first = repository.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap(); let pane = first.change.active.unwrap();
    apply(&mut d, DeskCommand::Bookmark { pane, label: "keep".into() });
    let bookmark = d.model().bookmarks()[0].id();
    apply(&mut d, DeskCommand::Close(pane)); apply(&mut d, DeskCommand::ClearHistory);
    let next = repository.open_hit(&mut d, 4, 5, 1, 1, || false).unwrap(); assert!(next.reused_source);
    apply(&mut d, DeskCommand::ForgetBookmark(bookmark));
    apply(&mut d, DeskCommand::Close(next.change.active.unwrap())); apply(&mut d, DeskCommand::ClearHistory);
    assert_eq!(d.model().retained_source_count(), 0);
    let final_open = repository.open_hit(&mut d, 8, 9, 1, 1, || false).unwrap();
    assert!(!final_open.reused_source); assert_ne!(first.desk_file, final_open.desk_file);
}

#[test]
fn utf16_result_selects_original_bytes_not_decoded_utf8_columns() {
    let root = fixture(); let mut bytes = vec![0xff, 0xfe];
    for unit in "x 😀 needle\r\n".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(root.join("a.rs"), &bytes).unwrap();
    let mut repository = repo(170, &root); let mut d = desk(171); find(&mut repository, 1, "😀");
    let opened = repository.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    assert_eq!(opened.original_range.start().get(), 6); assert_eq!(opened.original_range.end().get(), 10);
    assert_eq!(d.model().selected_bytes(opened.change.active.unwrap(), 1).unwrap(), &[0x3d, 0xd8, 0x00, 0xde]);
}

#[test]
fn partial_search_remains_partial_and_its_retained_hits_are_usable() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle needle needle").unwrap();
    let mut repository = repo(180, &root); let mut d = desk(181);
    let response = repository.search(1, "needle", AtlasSearchOptions { max_matches: 1, ..AtlasSearchOptions::default() }, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let page = repository.page(1, 0, 10, || false).unwrap();
    assert_eq!(page.exit_code(), EXIT_PARTIAL); assert_eq!(json(&page).get("hits").array().len(), 1);
    let opened = repository.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    assert_eq!(d.model().selected_bytes(opened.change.active.unwrap(), 1).unwrap(), b"needle");
}

#[test]
fn restore_does_not_let_an_import_binding_alias_a_restored_source_identity() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let mut repository = repo(190, &root); let mut d = desk(191); find(&mut repository, 1, "needle");
    let first = repository.open_hit(&mut d, 0, 1, 1, 1, || false).unwrap();
    let saved = root.join("state.fcbk"); assert_eq!(d.save_checkpoint(1, &saved, 100, || false).error(), None);
    let restored = d.restore_checkpoint_file(1, 2, &saved, || false).unwrap().active.unwrap();
    let restored_file = d.model().source(restored, 2).unwrap().file();
    let next = repository.open_hit(&mut d, 2, 3, 1, 1, || false).unwrap();
    assert!(!next.reused_source); assert_ne!(next.desk_file, first.desk_file); assert_ne!(next.desk_file, restored_file);
    assert_eq!(d.model().selected_bytes(next.change.active.unwrap(), 3).unwrap(), b"needle");
}

#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{parse, Json};
use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange};
use fcb::search::ReadingWindowOptions;
use fcb::ui::reading_panes::desk::checkpoint::{CheckpointError, MAX_CHECKPOINT_BYTES};
use fcb_app::{EXIT_OK, EXIT_CANCELED};
use fcb_app::host::{HostResponse, desk::{DeskSession, DeskSessionError, DeskLimits, DeskCommand,
    DeskPaneId, DeskError, persistence::{CheckpointSaveEffect, CheckpointIoError}}};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-desk-checkpoint-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn session(n: u64) -> DeskSession { DeskSession::new(ArenaOwnerId::new(n).unwrap(), DeskLimits::default()).unwrap() }
fn json(reply: &HostResponse) -> Json { parse(reply.as_str().as_bytes()).unwrap() }
fn apply(s: &mut DeskSession, command: DeskCommand) {
    s.apply(s.model().revision(), s.model().last_attempt() + 1, command, || false).unwrap();
}
fn open(s: &mut DeskSession, path: &Path) -> DeskPaneId {
    s.open_file(s.model().revision(), s.model().last_attempt() + 1, path, || false).unwrap().active.unwrap()
}
fn save(s: &mut DeskSession, path: &Path) {
    let rev = s.model().revision();
    let result = s.save_checkpoint(rev, path, 32 * 1024 * 1024, || false);
    assert_eq!(result.error(), None); assert_eq!(result.exit_code(), EXIT_OK);
    assert_eq!(result.effect(), CheckpointSaveEffect::DirectorySynced);
    assert_eq!(result.bytes_written(), fs::metadata(path).unwrap().len());
    assert_eq!(result.bytes_written(), result.document_bytes());
    let response = json(&s.checkpoint_save_response(&result).unwrap());
    assert_eq!(response.get("status").text(), "ok");
    assert!(response.get("contains_source_payloads").flag());
    assert!(!response.get("encrypted").flag());
    assert_eq!(s.model().revision(), rev);
}
fn text(s: &mut DeskSession, pane: DeskPaneId) -> String {
    json(&s.window(s.model().revision(), pane, None, ReadingWindowOptions::default(), || false).unwrap()).get("text").text().to_owned()
}

#[test]
fn fresh_session_restores_exact_sources_pins_selection_and_bookmarks_after_rename() {
    let root = fixture(); let source = root.join("source.rs"); let checkpoint = root.join("reading.fcbk");
    fs::write(&source, b"old selection\r\n").unwrap();
    let mut first = session(100); let pane = open(&mut first, &source);
    apply(&mut first, DeskCommand::Navigate { pane, offset: 4, selection: Some(ByteRange::new(ByteOffset::new(4), ByteOffset::new(13)).unwrap()) });
    apply(&mut first, DeskCommand::Pin { pane, pinned: true });
    apply(&mut first, DeskCommand::Bookmark { pane, label: "preserved rationale".into() });
    apply(&mut first, DeskCommand::Arrange { pane, position: (120.5, 30.0), size: (700.0, 800.0) });
    save(&mut first, &checkpoint); drop(first);
    fs::rename(&source, root.join("moved.rs")).unwrap();
    let mut second = session(101);
    let pane = second.restore_checkpoint_file(0, 1, &checkpoint, || false).unwrap().active.unwrap();
    assert_eq!(text(&mut second, pane), "selection\r\n");
    assert_eq!(json(&second.copy_selection(1, pane, || false).unwrap()).get("original_hex").text(), "73656c656374696f6e");
    assert!(second.model().panes().active_pane().unwrap().is_pinned);
    assert_eq!(second.model().panes().active_pane().unwrap().position, (120.5, 30.0));
    assert_eq!(second.model().bookmarks()[0].label(), "preserved rationale");
    assert_eq!(json(&second.state(|| false).unwrap()).get("initial_source_bytes_read").number(), 0);
    assert!(!source.exists());
}

#[test]
fn corrupt_restore_preserves_old_query_and_successful_restore_retires_it() {
    let root = fixture(); let path = root.join("a.rs"); let checkpoint = root.join("state.fcbk");
    fs::write(&path, b"needle").unwrap(); let mut s = session(102); let old = open(&mut s, &path);
    s.search(1, old, 1, "needle", 10, 100, || false).unwrap(); save(&mut s, &checkpoint);
    let mut bad = fs::read(&checkpoint).unwrap(); bad[70] ^= 1;
    assert_eq!(s.restore_checkpoint_bytes(1, 2, &bad, || false), Err(CheckpointIoError::Checkpoint(CheckpointError::Integrity)));
    assert_eq!(s.accepted_query(), Some(1)); assert_eq!(s.model().active(), Some(old));
    s.activate_hit(1, 3, old, 1, 0, || false).unwrap();
    let pane = s.restore_checkpoint_file(3, 4, &checkpoint, || false).unwrap().active.unwrap();
    assert_ne!(old, pane); assert_eq!(s.accepted_query(), None);
    assert!(matches!(s.search(4, pane, 1, "needle", 10, 100, || false), Err(DeskSessionError::StaleQuery)));
    s.search(4, pane, 2, "needle", 10, 100, || false).unwrap();
    s.activate_hit(4, 5, pane, 2, 0, || false).unwrap();
    assert_eq!(json(&s.copy_selection(5, pane, || false).unwrap()).get("original_hex").text(), "6e6565646c65");
}

#[test]
fn later_live_open_cannot_reuse_restored_source_identities() {
    let root = fixture(); let source = root.join("a.rs"); let checkpoint = root.join("state.fcbk");
    fs::write(&source, b"first").unwrap(); let mut s = session(103); open(&mut s, &source); save(&mut s, &checkpoint);
    let restored = s.restore_checkpoint_file(1, 2, &checkpoint, || false).unwrap().active.unwrap();
    let old_file = s.model().source(restored, 2).unwrap().file();
    let old_revision = s.model().source(restored, 2).unwrap().revision();
    fs::write(&source, b"second").unwrap(); let new = open(&mut s, &source);
    assert_ne!(s.model().source(new, 3).unwrap().file(), old_file);
    assert_ne!(s.model().source(new, 3).unwrap().revision(), old_revision);
    apply(&mut s, DeskCommand::Back);
    let active = s.model().active().unwrap();
    assert_eq!(text(&mut s, active), "first");
}

#[test]
fn existing_destination_and_symlink_are_never_overwritten() {
    let root = fixture(); let existing = root.join("existing"); fs::write(&existing, b"do not touch").unwrap();
    let mut s = session(104);
    let outcome = s.save_checkpoint(0, &existing, 0, || false);
    assert_eq!(outcome.effect(), CheckpointSaveEffect::None);
    assert_eq!(outcome.error(), Some(CheckpointIoError::DestinationExists));
    assert_eq!(fs::read(&existing).unwrap(), b"do not touch");
    #[cfg(unix)] {
        use std::os::unix::fs::symlink;
        let link = root.join("link"); symlink(&existing, &link).unwrap();
        let outcome = s.save_checkpoint(0, &link, 0, || false);
        assert!(outcome.error().is_some()); assert_eq!(outcome.effect(), CheckpointSaveEffect::None);
        assert_eq!(fs::read(&existing).unwrap(), b"do not touch");
        assert!(s.restore_checkpoint_file(0, 1, &link, || false).is_err());
    }
}

#[test]
fn source_disclosure_limit_is_checked_before_destination_creation() {
    let root = fixture(); let source = root.join("a.rs"); let checkpoint = root.join("state.fcbk");
    fs::write(&source, b"private source").unwrap(); let mut s = session(105); open(&mut s, &source);
    let outcome = s.save_checkpoint(1, &checkpoint, 1, || false);
    assert_eq!(outcome.error(), Some(CheckpointIoError::ExportLimit));
    assert_eq!(outcome.effect(), CheckpointSaveEffect::None); assert_eq!(outcome.bytes_written(), 0);
    assert!(!checkpoint.exists()); assert_eq!(s.model().revision(), 1);
}

#[test]
fn canceled_creation_reports_incomplete_file_and_does_not_remove_it() {
    let root = fixture(); let checkpoint = root.join("interrupted.fcbk"); let mut s = session(106);
    let outcome = s.save_checkpoint(0, &checkpoint, 0, || checkpoint.exists());
    assert_eq!(outcome.exit_code(), EXIT_CANCELED); assert_eq!(outcome.effect(), CheckpointSaveEffect::Created);
    assert_eq!(outcome.bytes_written(), 0); assert!(checkpoint.exists());
    let response = s.checkpoint_save_response(&outcome).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    assert_eq!(json(&response).get("effect").text(), "destination-created-incomplete");
    assert!(s.restore_checkpoint_file(0, 1, &checkpoint, || false).is_err());
    assert_eq!(s.model().revision(), 0);
}

#[test]
fn pre_canceled_and_stale_save_never_create_a_destination() {
    let root = fixture(); let checkpoint = root.join("not-created"); let mut s = session(107);
    let outcome = s.save_checkpoint(0, &checkpoint, 0, || true);
    assert_eq!(outcome.exit_code(), EXIT_CANCELED); assert!(!checkpoint.exists());
    let outcome = s.save_checkpoint(1, &checkpoint, 0, || false);
    assert_eq!(outcome.error(), Some(CheckpointIoError::Session(DeskSessionError::Desk(DeskError::StaleRevision))));
    assert!(!checkpoint.exists());
    assert!(matches!(s.restore_checkpoint_file(1, 1, &checkpoint, || false),
        Err(CheckpointIoError::Session(DeskSessionError::Desk(DeskError::StaleRevision)))));
}

#[test]
fn oversized_artifact_is_refused_without_losing_current_readers() {
    let root = fixture(); let oversized = root.join("oversized");
    fs::File::create(&oversized).unwrap().set_len(MAX_CHECKPOINT_BYTES as u64 + 1).unwrap();
    let mut s = session(108);
    assert_eq!(s.restore_checkpoint_file(0, 1, &oversized, || false), Err(CheckpointIoError::Checkpoint(CheckpointError::Limit)));
    assert_eq!(s.model().revision(), 0); assert_eq!(s.model().active(), None);
}

#[test]
fn checkpoints_use_private_permissions_and_support_non_utf8_destinations() {
    use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
    let root = fixture(); let path = root.join(std::ffi::OsString::from_vec(b"checkpoint\t\xff.fcbk".to_vec()));
    let mut s = session(109); save(&mut s, &path);
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
    let mut reopened = session(110); reopened.restore_checkpoint_file(0, 1, &path, || false).unwrap();
    assert_eq!(reopened.model().active(), None);
}

#[test]
fn canceled_restore_keeps_search_results_and_original_reader_usable() {
    let root = fixture(); let source = root.join("a.rs"); let checkpoint = root.join("state.fcbk");
    fs::write(&source, b"needle").unwrap(); let mut s = session(111); let pane = open(&mut s, &source);
    s.search(1, pane, 1, "needle", 10, 100, || false).unwrap(); save(&mut s, &checkpoint);
    let mut calls = 0;
    let result = s.restore_checkpoint_file(1, 2, &checkpoint, || { calls += 1; calls == 2 });
    assert!(matches!(result, Err(e) if e.is_canceled()));
    assert_eq!(s.accepted_query(), Some(1)); assert_eq!(s.model().active(), Some(pane));
    assert_eq!(s.model().revision(), 1);
    s.activate_hit(1, 3, pane, 1, 0, || false).unwrap();
    assert_eq!(json(&s.copy_selection(3, pane, || false).unwrap()).get("original_hex").text(), "6e6565646c65");
}

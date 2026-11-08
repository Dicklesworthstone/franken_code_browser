#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::search::snapshot::{SnapshotLimits, SnapshotView};
use fcb_app::{EXIT_OK, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};
use fcb_app::host::{HostError, saved_repository::SavedRepositorySession,
    snapshot_export::{save_repository, SnapshotExportOptions, MAX_EXPORT_FILES}};
use support::parse;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-host-export-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(path.join("root")).unwrap(); Self(path)
    }
    fn root(&self) -> PathBuf { self.0.join("root") }
    fn archive(&self, name: &str) -> PathBuf { self.0.join(name) }
}
fn options() -> SnapshotExportOptions { SnapshotExportOptions { respect_ignores: false, ..Default::default() } }

#[test]
fn host_export_opens_in_saved_repository_and_retains_source_after_live_edits() {
    use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
    let f = Fixture::new();
    fs::write(f.root().join("source.rs"), b"old needle").unwrap();
    let native = std::ffi::OsString::from_vec(b"z-\xff.rs".to_vec());
    fs::write(f.root().join(native), b"raw filename").unwrap();
    let output = f.archive("repository.fcbs");
    let response = save_repository(&f.root(), &output, options(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let receipt = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(receipt.get("command").text(), "snapshot-save");
    assert_eq!(receipt.get("effect").text(), "complete-file-sync-requested");
    assert_eq!(receipt.get("captured_files").number(), 2);
    assert!(receipt.get("source_contains_plaintext").flag());
    assert!(!receipt.get("power_loss_qualified").flag());
    assert_eq!(fs::metadata(&output).unwrap().permissions().mode() & 0o077, 0);
    let bytes = fs::read(&output).unwrap();
    let view = SnapshotView::open(&bytes, SnapshotLimits::default(), || false).unwrap();
    assert!(view.entries().any(|entry| entry.unwrap().path == b"z-\xff.rs"));
    fs::write(f.root().join("source.rs"), b"replacement").unwrap();
    let mut saved = SavedRepositorySession::open(ArenaOwnerId::new(7201).unwrap(), &output,
        SnapshotLimits::default(), || false).unwrap();
    let found = saved.search(1, "needle", 10, || false).unwrap();
    assert_eq!(found.exit_code(), EXIT_OK);
    assert!(found.as_str().contains("\"retained_hits\":\"1\""));
    let (reader, _) = saved.open_hit_reader(ArenaOwnerId::new(7202).unwrap(), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"old needle");
}

#[test]
fn explicit_ignore_policy_and_capture_omissions_survive_the_same_writer() {
    let f = Fixture::new();
    fs::write(f.root().join(".gitignore"), b"secret.rs\n").unwrap();
    fs::write(f.root().join("secret.rs"), b"private needle").unwrap();
    fs::write(f.root().join("source.rs"), b"needle").unwrap();
    fs::write(f.root().join("large.rs"), b"x".repeat(64)).unwrap();
    let output = f.archive("partial.fcbs");
    let response = save_repository(&f.root(), &output,
        SnapshotExportOptions { max_file_bytes: 16, respect_ignores: true, ..options() }, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let receipt = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(receipt.get("unavailable_files_count").number(), 1);
    assert_eq!(receipt.get("effect").text(), "complete-file-sync-requested");
    let bytes = fs::read(&output).unwrap();
    let view = SnapshotView::open(&bytes, SnapshotLimits::default(), || false).unwrap();
    assert!(!view.entries().any(|entry| entry.unwrap().path == b"secret.rs"));
    assert!(view.entries().any(|entry| entry.unwrap().path == b"large.rs"));
    assert_eq!(view.len() - view.captured_files(), 1);
}

#[test]
fn partial_source_budget_is_a_valid_archive_with_explicit_unavailable_members() {
    let f = Fixture::new();
    fs::write(f.root().join("a-empty.rs"), b"").unwrap();
    fs::write(f.root().join("b-source.rs"), b"needle").unwrap();
    let output = f.archive("limited.fcbs");
    let response = save_repository(&f.root(), &output,
        SnapshotExportOptions { max_source_bytes: 0, ..options() }, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let receipt = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(receipt.get("known_files").number(), 2);
    assert_eq!(receipt.get("captured_files").number(), 1);
    assert_eq!(receipt.get("unavailable_files_count").number(), 1);
    let mut saved = SavedRepositorySession::open(ArenaOwnerId::new(7203).unwrap(), &output,
        SnapshotLimits::default(), || false).unwrap();
    let found = saved.search(1, "needle", 10, || false).unwrap();
    assert_eq!(found.exit_code(), EXIT_PARTIAL);
    assert!(found.as_str().contains("\"retained_hits\":\"0\""));
    assert!(!found.as_str().contains("\"search_complete\":true"));
}

#[test]
fn exclusive_destination_and_invalid_limits_preserve_source_and_existing_files() {
    let f = Fixture::new(); let source = f.root().join("source.rs");
    fs::write(&source, b"source must survive").unwrap();
    let denied = save_repository(&f.root(), &source, options(), || false).unwrap();
    assert_eq!(denied.exit_code(), EXIT_ERROR);
    let receipt = parse(denied.as_str().as_bytes()).unwrap();
    assert_eq!(receipt.get("effect").text(), "none");
    assert_eq!(receipt.get("error").get("code").text(), "SNAPSHOT_DESTINATION_EXISTS");
    assert_eq!(fs::read(&source).unwrap(), b"source must survive");
    let output = f.archive("invalid.fcbs");
    assert!(matches!(save_repository(&f.root(), &output,
        SnapshotExportOptions { max_files: MAX_EXPORT_FILES + 1, ..options() }, || false), Err(HostError::App(_))));
    assert!(!output.exists());
}

#[test]
fn canceled_writes_deliver_the_actual_effect_and_never_retry_or_remove_output() {
    let f = Fixture::new(); fs::write(f.root().join("source.rs"), b"needle").unwrap();
    let before = f.archive("before.fcbs");
    let response = save_repository(&f.root(), &before, options(), || true).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    assert_eq!(parse(response.as_str().as_bytes()).unwrap().get("effect").text(), "none");
    assert!(!before.exists());
    let partial = f.archive("incomplete.fcbs");
    let response = save_repository(&f.root(), &partial, options(), || partial.exists()).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    assert_eq!(parse(response.as_str().as_bytes()).unwrap().get("effect").text(), "destination-created-incomplete");
    assert_eq!(fs::metadata(&partial).unwrap().len(), 0);
    let retry = save_repository(&f.root(), &partial, options(), || false).unwrap();
    assert_eq!(retry.exit_code(), EXIT_ERROR);
    assert!(retry.as_str().contains("SNAPSHOT_DESTINATION_EXISTS"));
    let written = f.archive("written.fcbs");
    let response = save_repository(&f.root(), &written, options(),
        || fs::metadata(&written).is_ok_and(|metadata| metadata.len() > 0)).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    assert_eq!(parse(response.as_str().as_bytes()).unwrap().get("effect").text(), "complete-file-sync-unconfirmed");
    let bytes = fs::read(&written).unwrap();
    assert_eq!(SnapshotView::open(&bytes, SnapshotLimits::default(), || false).unwrap().captured_files(), 1);
}

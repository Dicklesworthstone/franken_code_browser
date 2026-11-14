#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::search::snapshot::SnapshotLimits;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{saved_repository::SavedRepositorySession,
    snapshot_export::{save_repository, SnapshotExportOptions,
        MAX_EXPORT_FILE_BYTES, MAX_EXPORT_SOURCE_BYTES}};
use support::parse;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-host-export-capacity-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(path.join("root")).unwrap();
        Self(path)
    }
    fn root(&self) -> PathBuf { self.0.join("root") }
    fn archive(&self) -> PathBuf { self.0.join("saved.fcbs") }
    fn write_members(&self, count: usize) -> Vec<u8> {
        let mut bytes = vec![0; MAX_EXPORT_FILE_BYTES];
        for ordinal in 0..count {
            bytes.fill(b'a' + (ordinal % 26) as u8);
            let header = format!("// member {ordinal:02}\n");
            bytes[..header.len()].copy_from_slice(header.as_bytes());
            fs::write(self.root().join(format!("source-{ordinal:02}.rs")), &bytes).unwrap();
        }
        bytes
    }
}

#[test]
fn native_defaults_save_the_full_advertised_source_budget_and_reopen_exact_bytes() {
    let fixture = Fixture::new();
    let count = MAX_EXPORT_SOURCE_BYTES / MAX_EXPORT_FILE_BYTES;
    let expected_last = fixture.write_members(count);
    let response = save_repository(&fixture.root(), &fixture.archive(),
        SnapshotExportOptions::default(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK, "{}", response.as_str());
    let receipt = parse(response.as_str().as_bytes()).unwrap();
    assert!(receipt.get("discovery_complete").flag());
    assert_eq!(receipt.get("effect").text(), "complete-file-sync-requested");
    assert_eq!(receipt.get("known_files").number(), count as u64);
    assert_eq!(receipt.get("captured_files").number(), count as u64);
    assert_eq!(receipt.get("unavailable_files_count").number(), 0);
    assert_eq!(receipt.get("captured_bytes").number(), MAX_EXPORT_SOURCE_BYTES as u64);
    assert_eq!(receipt.get("payload_bytes_read").number(), MAX_EXPORT_SOURCE_BYTES as u64);
    assert!(receipt.get("archive_bytes").number() > MAX_EXPORT_SOURCE_BYTES as u64);

    // A valid receipt must also deliver a usable archive, including its last
    // full-sized member. The saved reader must not substitute a later live edit.
    fs::write(fixture.root().join(format!("source-{:02}.rs", count - 1)), b"changed live source").unwrap();
    let mut saved = SavedRepositorySession::open(ArenaOwnerId::new(7301).unwrap(),
        &fixture.archive(), SnapshotLimits::default(), || false).unwrap();
    let (reader, opened) = saved.open_member_reader(ArenaOwnerId::new(7302).unwrap(),
        count - 1, || false).unwrap();
    assert_eq!(opened.exit_code(), EXIT_OK);
    assert_eq!(reader.capture().bytes(), expected_last);
}

#[test]
fn releasing_capture_overlap_does_not_raise_the_requested_source_limit() {
    let fixture = Fixture::new();
    fixture.write_members(3);
    let source_limit = 2 * MAX_EXPORT_FILE_BYTES;
    let response = save_repository(&fixture.root(), &fixture.archive(), SnapshotExportOptions {
        max_source_bytes: source_limit, ..SnapshotExportOptions::default()
    }, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL, "{}", response.as_str());
    let receipt = parse(response.as_str().as_bytes()).unwrap();
    assert!(receipt.get("discovery_complete").flag());
    assert_eq!(receipt.get("effect").text(), "complete-file-sync-requested");
    assert_eq!(receipt.get("known_files").number(), 3);
    assert_eq!(receipt.get("captured_files").number(), 2);
    assert_eq!(receipt.get("unavailable_files_count").number(), 1);
    assert_eq!(receipt.get("captured_bytes").number(), source_limit as u64);
    let mut saved = SavedRepositorySession::open(ArenaOwnerId::new(7303).unwrap(),
        &fixture.archive(), SnapshotLimits::default(), || false).unwrap();
    assert!(saved.open_member_reader(ArenaOwnerId::new(7304).unwrap(), 2, || false).is_err());
}

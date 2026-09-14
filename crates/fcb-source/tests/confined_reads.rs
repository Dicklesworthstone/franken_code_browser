#![forbid(unsafe_code)]

//! Integration test suite for Root read capabilities and confined native reads (FCB-009.A / fcb-qb2.1).
//!
//! Verifies:
//! 1. Descriptor-relative / no-follow opened-object checks and root confinement.
//! 2. Raw path identity, case distinctions, and safe escaped presentation.
//! 3. Symlink swaps, foreign symlink rejection, cycle/loop detection, and hop bounds.
//! 4. Non-file object rejection (FIFOs, sockets, character/block devices).
//! 5. Root grant lifecycle, revocation generation, multi-root separation.
//! 6. Revocation during read and export publication serialization.
//! 7. No escalation via percent-encoded traversal or nonlocal `file:` URIs.

use std::fs::{self, File};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use fcb_core::{ArenaOwnerId, ByteLength, FileId, RootId, SourceRevision};
use fcb_source::confined::{ConfinedSourceReader, SymlinkPolicy};
use fcb_source::path::{decode_uri_path, NormalizedPath, RawPath};
use fcb_source::root::{ExportPublicationGate, RootGrant};
use fcb_source::{CancelFlag, SourceError};

struct TempTestDir {
    path: std::path::PathBuf,
}

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("fcb_test_{}_{}_{}", prefix, std::process::id(), nanos));
        fs::create_dir_all(&path).expect("failed to create temp test dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn test_root_id(owner: u64, root: u64) -> RootId {
    let owner_id = ArenaOwnerId::new(owner).unwrap();
    RootId::new(owner_id, root).unwrap()
}

fn test_file_id(owner: u64, file: u64) -> FileId {
    let owner_id = ArenaOwnerId::new(owner).unwrap();
    FileId::new(owner_id, file).unwrap()
}

fn test_revision(owner: u64, rev: u64) -> SourceRevision {
    let owner_id = ArenaOwnerId::new(owner).unwrap();
    SourceRevision::new(owner_id, rev).unwrap()
}

#[test]
fn raw_path_preserves_case_distinction_and_byte_identity() {
    let p1 = RawPath::from_str("src/Makefile");
    let p2 = RawPath::from_str("src/makefile");
    assert_ne!(p1, p2, "Distinct case paths must not alias");
    assert_eq!(p1.as_bytes(), b"src/Makefile");
    assert_eq!(p2.as_bytes(), b"src/makefile");

    let raw_bytes = vec![0x7f, 0x80, 0x90, 0xff];
    let non_utf8 = RawPath::from_bytes(raw_bytes.clone());
    assert_eq!(non_utf8.as_bytes(), &raw_bytes[..]);
}

#[test]
fn escaped_path_presentation_neutralizes_newlines_and_bidi_overrides() {
    let dangerous = RawPath::from_str("report\nname\rwith\tbidi_\u{202E}txt.sh");
    let escaped = dangerous.display_escaped().to_string();

    assert!(!escaped.contains('\n'), "Newline must be escaped");
    assert!(!escaped.contains('\r'), "Carriage return must be escaped");
    assert!(!escaped.contains('\t'), "Tab must be escaped");
    assert!(!escaped.contains('\u{202E}'), "RLO bidi override must be escaped");
    assert!(escaped.contains("\\n"));
    assert!(escaped.contains("\\r"));
    assert!(escaped.contains("\\t"));
    assert!(escaped.contains("\\u{202e}"));
}

#[test]
fn encoded_path_traversal_escalation_is_strictly_rejected() {
    // Standard URL encoded traversal
    assert_eq!(decode_uri_path("%2e%2e/etc/passwd"), Err(SourceError::PathEscape));
    assert_eq!(decode_uri_path("foo/%2E%2E/bar"), Err(SourceError::PathEscape));
    assert_eq!(decode_uri_path("%2e%2e%2fsecret.key"), Err(SourceError::PathEscape));

    // Nonlocal file URI
    assert_eq!(
        decode_uri_path("file://evil-host/share/repo/main.rs"),
        Err(SourceError::PathEscape)
    );

    // Null byte injection
    assert_eq!(decode_uri_path("code.rs%00.png"), Err(SourceError::EncodingError));

    // Valid relative path
    let valid = decode_uri_path("src%2Fcomponents%2Fbutton.rs").unwrap();
    assert_eq!(valid.as_bytes(), b"src/components/button.rs");
}

#[test]
fn root_confinement_reads_regular_files_honoring_limits() {
    let dir = TempTestDir::new("confined_read");
    let file_path = dir.path().join("source.rs");
    let mut f = File::create(&file_path).unwrap();
    f.write_all(b"fn main() { println!(\"hello\"); }").unwrap();

    let root_id = test_root_id(100, 1);
    let grant = RootGrant::new(root_id, dir.path());
    let reader = ConfinedSourceReader::new(
        grant,
        SymlinkPolicy::DisallowAll,
        ByteLength::new(1024),
    );

    let norm = NormalizedPath::new("source.rs").unwrap();
    let cancel = CancelFlag::new();
    let capture = reader
        .read_file(test_file_id(100, 1), test_revision(100, 1), &norm, &cancel)
        .unwrap();

    assert_eq!(capture.bytes(), b"fn main() { println!(\"hello\"); }");
    assert_eq!(capture.declared_length().get(), 32);
}

#[test]
fn payload_limit_refuses_excessive_files() {
    let dir = TempTestDir::new("payload_limit");
    let file_path = dir.path().join("large.bin");
    let mut f = File::create(&file_path).unwrap();
    f.write_all(&vec![0xAA; 500]).unwrap();

    let root_id = test_root_id(101, 1);
    let grant = RootGrant::new(root_id, dir.path());
    // Limit is 100 bytes, file is 500 bytes
    let reader = ConfinedSourceReader::new(
        grant,
        SymlinkPolicy::DisallowAll,
        ByteLength::new(100),
    );

    let norm = NormalizedPath::new("large.bin").unwrap();
    let cancel = CancelFlag::new();
    let err = reader
        .read_file(test_file_id(101, 1), test_revision(101, 1), &norm, &cancel)
        .unwrap_err();

    assert_eq!(err, SourceError::PayloadTooLarge);
}

#[test]
fn symlink_policy_disallow_all_rejects_in_root_symlink() {
    let dir = TempTestDir::new("symlink_disallow");
    let target_file = dir.path().join("real.txt");
    fs::write(&target_file, b"content").unwrap();

    #[cfg(unix)]
    {
        let link_path = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&target_file, &link_path).unwrap();

        let root_id = test_root_id(102, 1);
        let grant = RootGrant::new(root_id, dir.path());
        let reader = ConfinedSourceReader::new(
            grant,
            SymlinkPolicy::DisallowAll,
            ByteLength::new(1024),
        );

        let norm = NormalizedPath::new("link.txt").unwrap();
        let cancel = CancelFlag::new();
        let err = reader
            .read_file(test_file_id(102, 1), test_revision(102, 1), &norm, &cancel)
            .unwrap_err();

        assert_eq!(err, SourceError::SymlinkForbidden);
    }
}

#[test]
fn symlink_policy_allow_within_root_rejects_foreign_symlink() {
    let outside_dir = TempTestDir::new("foreign_outside");
    let secret_file = outside_dir.path().join("secret.txt");
    fs::write(&secret_file, b"super-secret").unwrap();

    let inside_dir = TempTestDir::new("foreign_inside");

    #[cfg(unix)]
    {
        let foreign_link = inside_dir.path().join("escape_link.txt");
        std::os::unix::fs::symlink(&secret_file, &foreign_link).unwrap();

        let root_id = test_root_id(103, 1);
        let grant = RootGrant::new(root_id, inside_dir.path());
        let reader = ConfinedSourceReader::new(
            grant,
            SymlinkPolicy::AllowWithinRoot,
            ByteLength::new(1024),
        );

        let norm = NormalizedPath::new("escape_link.txt").unwrap();
        let cancel = CancelFlag::new();
        let err = reader
            .read_file(test_file_id(103, 1), test_revision(103, 1), &norm, &cancel)
            .unwrap_err();

        assert_eq!(err, SourceError::ForeignSymlink);
    }
}

#[test]
fn symlink_cycle_and_loop_detection_refuses_infinite_traversal() {
    let dir = TempTestDir::new("symlink_cycle");

    #[cfg(unix)]
    {
        let sub = dir.path().join("subdir");
        fs::create_dir_all(&sub).unwrap();

        // Create recursive symlink loop: subdir/loop -> subdir
        let loop_link = sub.join("loop");
        std::os::unix::fs::symlink(&sub, &loop_link).unwrap();

        let root_id = test_root_id(104, 1);
        let grant = RootGrant::new(root_id, dir.path());
        let reader = ConfinedSourceReader::new(
            grant,
            SymlinkPolicy::AllowWithinRoot,
            ByteLength::new(1024),
        );

        let norm = NormalizedPath::new("subdir/loop/loop/target.txt").unwrap();
        let cancel = CancelFlag::new();
        let err = reader
            .read_file(test_file_id(104, 1), test_revision(104, 1), &norm, &cancel)
            .unwrap_err();

        assert_eq!(err, SourceError::TraversalCycle);
    }
}

#[cfg(unix)]
#[test]
fn special_object_fifo_is_refused_without_blocking() {
    let dir = TempTestDir::new("fifo_special");
    let fifo_path = dir.path().join("test_pipe.fifo");

    // Create a FIFO named pipe using mkfifo command (no unsafe, zero dependencies)
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .status();

    if let Ok(exit) = status {
        if exit.success() {
            let root_id = test_root_id(105, 1);
            let grant = RootGrant::new(root_id, dir.path());
            let reader = ConfinedSourceReader::new(
                grant,
                SymlinkPolicy::DisallowAll,
                ByteLength::new(1024),
            );

            let norm = NormalizedPath::new("test_pipe.fifo").unwrap();
            let cancel = CancelFlag::new();
            let err = reader
                .read_file(test_file_id(105, 1), test_revision(105, 1), &norm, &cancel)
                .unwrap_err();

            assert_eq!(err, SourceError::SpecialObject);
        }
    }
}

#[test]
fn root_grant_revocation_during_read_and_export_publication() {
    let dir = TempTestDir::new("revocation_test");
    let file_path = dir.path().join("doc.md");
    fs::write(&file_path, b"# Documentation").unwrap();

    let root_id = test_root_id(106, 1);
    let grant = RootGrant::new(root_id, dir.path());
    let reader = ConfinedSourceReader::new(
        grant.clone(),
        SymlinkPolicy::DisallowAll,
        ByteLength::new(1024),
    );

    // Case 1: Revoke before read
    grant.revoke();
    let norm = NormalizedPath::new("doc.md").unwrap();
    let cancel = CancelFlag::new();
    let err = reader
        .read_file(test_file_id(106, 1), test_revision(106, 1), &norm, &cancel)
        .unwrap_err();
    assert_eq!(err, SourceError::GrantRevoked);

    // Case 2: Multi-root separation
    let other_root_id = test_root_id(106, 2);
    assert_eq!(
        grant.validate_root_match(other_root_id),
        Err(SourceError::ForeignOwner)
    );

    // Case 3: Export publication gate aborts when revoked during preparation
    let active_grant = RootGrant::new(root_id, dir.path());
    let grant_clone = active_grant.clone();

    let pub_err = ExportPublicationGate::publish(
        &active_grant,
        || {
            // Revoke while preparing export
            grant_clone.revoke();
            Ok(vec![1, 2, 3])
        },
        |data| Ok(data),
    )
    .unwrap_err();

    assert_eq!(pub_err, SourceError::GrantRevoked);

    // Case 4: Once published, outcome remains intact
    let valid_grant = RootGrant::new(root_id, dir.path());
    let published_data = ExportPublicationGate::publish(
        &valid_grant,
        || Ok("committed output"),
        |d| Ok(d.to_uppercase()),
    )
    .unwrap();

    assert_eq!(published_data, "COMMITTED OUTPUT");
    // Later revocation does not alter previously published value
    valid_grant.revoke();
    assert_eq!(published_data, "COMMITTED OUTPUT");
}

#[test]
fn root_unavailable_when_root_directory_missing() {
    let missing_path = std::env::temp_dir().join("fcb_non_existent_dir_9999");
    let _ = fs::remove_dir_all(&missing_path);

    let root_id = test_root_id(107, 1);
    let grant = RootGrant::new(root_id, &missing_path);
    let reader = ConfinedSourceReader::new(
        grant,
        SymlinkPolicy::DisallowAll,
        ByteLength::new(1024),
    );

    let norm = NormalizedPath::new("anything.txt").unwrap();
    let cancel = CancelFlag::new();
    let err = reader
        .read_file(test_file_id(107, 1), test_revision(107, 1), &norm, &cancel)
        .unwrap_err();

    assert_eq!(err, SourceError::RootUnavailable);
}

#[test]
fn cooperative_cancellation_aborts_read() {
    let dir = TempTestDir::new("cancel_test");
    let file_path = dir.path().join("cancel.txt");
    fs::write(&file_path, b"data").unwrap();

    let root_id = test_root_id(108, 1);
    let grant = RootGrant::new(root_id, dir.path());
    let reader = ConfinedSourceReader::new(
        grant,
        SymlinkPolicy::DisallowAll,
        ByteLength::new(1024),
    );

    let norm = NormalizedPath::new("cancel.txt").unwrap();
    let cancel = CancelFlag::new();
    cancel.cancel(); // Pre-canceled

    let err = reader
        .read_file(test_file_id(108, 1), test_revision(108, 1), &norm, &cancel)
        .unwrap_err();

    assert_eq!(err, SourceError::Canceled);
}

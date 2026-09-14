#![forbid(unsafe_code)]

//! Integration test suite for Root restoration, native permission lifecycle,
//! balanced access leases, and multi-root isolation (FCB-009.B / fcb-qb2.2).
//!
//! Verifies:
//! 1. Revalidation of native permissions and bookmarks upon reopen.
//! 2. Stale and unavailable states leaving explicit visible UI state (not empty success).
//! 3. Balanced native access leases with safe draining on revocation.
//! 4. Generation-serialized export publication gates.
//! 5. Multi-root session registry and selective revocation isolation.
//! 6. Negative control oracle detecting unauthorized restoration attempts.

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use fcb_core::{ArenaOwnerId, ByteLength, FileId, RootId, SourceRevision};
use fcb_source::confined::{ConfinedSourceReader, SymlinkPolicy};
use fcb_source::path::NormalizedPath;
use fcb_source::restoration::{
    RootAccessStatus, RootSessionRegistry, SandboxModel, SecurityScopedBookmark, StaleReason,
    UnavailableReason,
};
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
        let path = std::env::temp_dir().join(format!("fcb_restore_{}_{}_{}", prefix, std::process::id(), nanos));
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
fn restoration_revalidates_native_paths_and_presents_unavailable_state() {
    let mut registry = RootSessionRegistry::new();
    let valid_dir = TempTestDir::new("valid_restore");
    let missing_path = std::env::temp_dir().join("fcb_definitely_missing_dir_12345");
    let _ = fs::remove_dir_all(&missing_path);

    let r_valid = test_root_id(201, 1);
    let r_missing = test_root_id(201, 2);

    let valid_descriptor = SecurityScopedBookmark::new(
        valid_dir.path(),
        vec![0xAA, 0xBB],
        SandboxModel::AppSandbox,
    );
    let missing_descriptor = SecurityScopedBookmark::new(
        &missing_path,
        vec![0xCC, 0xDD],
        SandboxModel::AppSandbox,
    );

    // 1. Valid root restores to Active
    let restored_valid = registry.restore_saved_descriptor(r_valid, &valid_descriptor);
    assert!(restored_valid.status().is_active());
    assert!(restored_valid.grant().is_some());
    assert_eq!(restored_valid.status().to_string(), "active");

    // 2. Missing root leaves explicit visible Unavailable state (never empty success)
    let restored_missing = registry.restore_saved_descriptor(r_missing, &missing_descriptor);
    assert_eq!(
        restored_missing.status(),
        RootAccessStatus::Unavailable(UnavailableReason::NotFound)
    );
    assert!(restored_missing.grant().is_none());
    assert!(restored_missing.status().is_unavailable());

    // 3. Reauthorization transforms Unavailable into Active
    let mut restored_missing_mut = registry.get_root_mut(r_missing).unwrap().clone();
    let reauthorized_grant = RootGrant::new(r_missing, valid_dir.path());
    restored_missing_mut.reauthorize(reauthorized_grant).unwrap();
    assert!(restored_missing_mut.status().is_active());
    assert!(restored_missing_mut.grant().is_some());
}

#[test]
fn stale_security_scoped_bookmark_leaves_stale_state_requiring_refresh() {
    let mut registry = RootSessionRegistry::new();
    let valid_dir = TempTestDir::new("stale_restore");
    let root_id = test_root_id(202, 1);

    let stale_descriptor = SecurityScopedBookmark::new(
        valid_dir.path(),
        vec![0x11, 0x22],
        SandboxModel::AppSandbox,
    ).with_stale(true);

    let restored = registry.restore_saved_descriptor(root_id, &stale_descriptor);
    assert_eq!(
        restored.status(),
        RootAccessStatus::Stale(StaleReason::SecurityBookmarkNeedsRefresh)
    );
    assert!(restored.status().is_stale());

    // Stale root refuses leases until refreshed/reauthorized
    assert_eq!(restored.acquire_lease().unwrap_err(), SourceError::RootUnavailable);
}

#[test]
fn balanced_native_access_leases_and_safe_revocation_draining() {
    let dir = TempTestDir::new("lease_drain");
    fs::write(dir.path().join("main.rs"), b"fn main() {}").unwrap();

    let root_id = test_root_id(203, 1);
    let grant = RootGrant::new(root_id, dir.path());
    let mut restored = fcb_source::RestoredRoot::new(
        root_id,
        dir.path(),
        RootAccessStatus::Active,
        Some(grant.clone()),
    );

    // Acquire multiple active leases concurrently
    let lease1 = restored.acquire_lease().unwrap();
    let lease2 = restored.acquire_lease().unwrap();
    let lease3 = restored.acquire_lease().unwrap();
    assert_eq!(restored.controller().active_leases(), 3);
    assert!(!restored.controller().is_drained());

    // Reader can read using held lease
    let reader = ConfinedSourceReader::new(
        grant.clone(),
        SymlinkPolicy::DisallowAll,
        ByteLength::new(1024),
    );
    let norm = NormalizedPath::new("main.rs").unwrap();
    let cancel = CancelFlag::new();
    let capture = reader.read_file(test_file_id(203, 1), test_revision(203, 1), &norm, &cancel);
    assert!(capture.is_ok());

    // Revoke root while leases 1, 2, 3 are still held
    restored.revoke();
    assert!(restored.status().is_revoked());
    assert!(restored.controller().is_revoked());
    assert!(!restored.controller().is_drained(), "Must not be drained while leases are held");

    // New lease acquisition is immediately rejected
    assert_eq!(restored.acquire_lease().unwrap_err(), SourceError::GrantRevoked);

    // Drop leases one by one
    drop(lease1);
    assert_eq!(restored.controller().active_leases(), 2);
    assert!(!restored.controller().is_drained());

    drop(lease2);
    assert_eq!(restored.controller().active_leases(), 1);
    assert!(!restored.controller().is_drained());

    drop(lease3);
    // Last lease dropped -> drain completes immediately
    assert_eq!(restored.controller().active_leases(), 0);
    assert!(restored.controller().is_drained(), "Drain must be marked complete once all leases drop");
}

#[test]
fn generation_serialized_export_publication_prevents_revocation_race() {
    let dir = TempTestDir::new("export_gate");
    let root_id = test_root_id(204, 1);
    let grant = RootGrant::new(root_id, dir.path());

    // 1. Successful serialized publication
    let outcome = ExportPublicationGate::publish(
        &grant,
        || Ok(vec![b'#', b' ', b'E', b'x', b'p', b'o', b'r', b't']),
        |bytes| {
            let s = String::from_utf8(bytes).unwrap();
            Ok(s)
        },
    );
    assert_eq!(outcome.unwrap(), "# Export");

    // 2. Revoked while preparing export -> publication aborted and discarded
    let grant_clone = grant.clone();
    let aborted = ExportPublicationGate::publish(
        &grant,
        || {
            grant_clone.revoke();
            Ok(vec![1, 2, 3])
        },
        |bytes| Ok(bytes.len()),
    );
    assert_eq!(aborted.unwrap_err(), SourceError::GrantRevoked);

    // 3. Already published destination retains its completed outcome
    let grant_active = RootGrant::new(root_id, dir.path());
    let published_dest = ExportPublicationGate::publish(
        &grant_active,
        || Ok("final_archive.tar"),
        |archive| Ok(format!("persisted: {}", archive)),
    ).unwrap();

    assert_eq!(published_dest, "persisted: final_archive.tar");
    grant_active.revoke();
    // Outcome remains intact
    assert_eq!(published_dest, "persisted: final_archive.tar");
}

#[test]
fn multi_root_isolation_preserves_independent_roots() {
    let mut registry = RootSessionRegistry::new();
    let dir1 = TempTestDir::new("multi_root_1");
    let dir2 = TempTestDir::new("multi_root_2");

    let r1 = test_root_id(205, 1);
    let r2 = test_root_id(205, 2);

    let d1 = SecurityScopedBookmark::new(dir1.path(), vec![1], SandboxModel::NotarizedUnsandboxed);
    let d2 = SecurityScopedBookmark::new(dir2.path(), vec![2], SandboxModel::NotarizedUnsandboxed);

    registry.restore_saved_descriptor(r1, &d1);
    registry.restore_saved_descriptor(r2, &d2);

    assert_eq!(registry.len(), 2);
    assert!(registry.get_root(r1).unwrap().status().is_active());
    assert!(registry.get_root(r2).unwrap().status().is_active());

    // Revoking Root 1 only
    registry.revoke_root(r1).unwrap();
    assert!(registry.get_root(r1).unwrap().status().is_revoked());
    assert!(registry.get_root(r2).unwrap().status().is_active());

    // Root 2 can still acquire leases and read cleanly
    let lease2 = registry.get_root(r2).unwrap().acquire_lease();
    assert!(lease2.is_ok());

    // Root 1 cannot acquire lease
    assert_eq!(registry.get_root(r1).unwrap().acquire_lease().unwrap_err(), SourceError::GrantRevoked);
}

/// Negative control oracle: an unauthorized reauthorization with a mismatched RootId is refused.
#[test]
fn negative_control_mismatched_reauthorization_is_refused() {
    let root_id = test_root_id(206, 1);
    let foreign_root_id = test_root_id(206, 999);
    let mut root = fcb_source::RestoredRoot::new(
        root_id,
        "/workspace/project",
        RootAccessStatus::Unavailable(UnavailableReason::NotFound),
        None,
    );

    let foreign_grant = RootGrant::new(foreign_root_id, "/workspace/foreign");
    let res = root.reauthorize(foreign_grant);
    assert_eq!(res, Err(SourceError::ForeignOwner));
}

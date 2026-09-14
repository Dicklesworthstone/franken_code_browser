#![forbid(unsafe_code)]

//! Root restoration, security-scoped access leases, stale/unavailable states,
//! and native permission lifecycle (FCB-009.B / fcb-qb2.2).
//!
//! # Invariants
//!
//! 1. **Saved Paths Do Not Confer Current Access**: A persisted `RootId` or
//!    path must be revalidated against native permissions upon session reopen.
//! 2. **Explicit Non-Empty Unavailable UI State**: Failed restoration (missing
//!    volume, revoked permission, stale bookmark) leaves the root visible with
//!    status [`RootAccessStatus::Unavailable`] or [`RootAccessStatus::Stale`]
//!    and a deliberate reauthorization action, never an empty successful
//!    workspace.
//! 3. **Balanced Native Access Leases**: Acquisition of OS access
//!    (e.g., security-scoped bookmarks) produces an RAII [`NativeAccessLease`].
//!    Lease counts are balanced; revocation marks the grant as revoked while
//!    outstanding leases drain safely.
//! 4. **Selective Revocation Isolation**: Revoking Root A invalidates its grant,
//!    stops admission, and drains its reads without affecting independent Root B.
//! 5. **Separation of Concerns**: Root access revocation, source-cache clearing,
//!    and annotation deletion are distinct actions.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use fcb_core::RootId;

use crate::path::RawPath;
use crate::root::RootGrant;
use crate::SourceError;

/// Operating system entitlement and sandbox packaging model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SandboxModel {
    /// Sandboxed AppKit executable requiring security-scoped bookmarks across launches.
    AppSandbox,
    /// Unsandboxed notarized CLI or developer tool with direct filesystem capabilities.
    NotarizedUnsandboxed,
}

/// Reason why a restored root is stale but potentially recoverable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StaleReason {
    /// The directory was moved or renamed since the bookmark was created.
    PathMoved,
    /// The security-scoped bookmark data requires refresh with the OS.
    SecurityBookmarkNeedsRefresh,
}

/// Reason why a restored root is unavailable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UnavailableReason {
    /// The target root directory does not exist on disk.
    NotFound,
    /// The process lacks operating system permission to access the root.
    PermissionDenied,
    /// The volume hosting the root directory is unmounted or detached.
    VolumeUnmounted,
    /// The saved descriptor or bookmark bytes were corrupt or malformed.
    InvalidDescriptor,
}

/// Current native access status of a root in a session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RootAccessStatus {
    /// Root is validated, active, and available for confined reads.
    Active,
    /// Root bookmark is stale; requires refresh or re-resolution.
    Stale(StaleReason),
    /// Root is unavailable; displayed in UI with deliberate reauthorization action.
    Unavailable(UnavailableReason),
    /// Root access was revoked; pending reads are drained and discarded.
    Revoked,
}

impl RootAccessStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }

    pub fn is_stale(&self) -> bool {
        matches!(self, Self::Stale(_))
    }

    pub fn is_revoked(&self) -> bool {
        matches!(self, Self::Revoked)
    }
}

/// Persisted descriptor for a source root across application launches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityScopedBookmark {
    raw_path: RawPath,
    bookmark_bytes: Arc<[u8]>,
    sandbox_model: SandboxModel,
    is_stale: bool,
}

impl SecurityScopedBookmark {
    pub fn new(
        raw_path: impl Into<RawPath>,
        bookmark_bytes: impl Into<Arc<[u8]>>,
        sandbox_model: SandboxModel,
    ) -> Self {
        Self {
            raw_path: raw_path.into(),
            bookmark_bytes: bookmark_bytes.into(),
            sandbox_model,
            is_stale: false,
        }
    }

    pub fn with_stale(mut self, is_stale: bool) -> Self {
        self.is_stale = is_stale;
        self
    }

    pub fn raw_path(&self) -> &RawPath {
        &self.raw_path
    }

    pub fn bookmark_bytes(&self) -> &[u8] {
        &self.bookmark_bytes
    }

    pub fn sandbox_model(&self) -> SandboxModel {
        self.sandbox_model
    }

    pub fn is_stale(&self) -> bool {
        self.is_stale
    }
}

/// Coordinates balanced native access acquisition and safe draining on revocation.
#[derive(Debug)]
pub struct RootAccessController {
    root_id: RootId,
    active_leases: AtomicUsize,
    revoked: AtomicBool,
    drain_complete: AtomicBool,
    generation: AtomicU64,
}

impl RootAccessController {
    pub fn new(root_id: RootId) -> Arc<Self> {
        Arc::new(Self {
            root_id,
            active_leases: AtomicUsize::new(0),
            revoked: AtomicBool::new(false),
            drain_complete: AtomicBool::new(false),
            generation: AtomicU64::new(1),
        })
    }

    pub fn root_id(&self) -> RootId {
        self.root_id
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn active_leases(&self) -> usize {
        self.active_leases.load(Ordering::Acquire)
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }

    pub fn is_drained(&self) -> bool {
        self.drain_complete.load(Ordering::Acquire)
    }

    /// Acquires an RAII native access lease. Fails if the root is revoked or unavailable.
    pub fn acquire_lease(self: &Arc<Self>) -> Result<NativeAccessLease, SourceError> {
        if self.is_revoked() {
            return Err(SourceError::GrantRevoked);
        }
        self.active_leases.fetch_add(1, Ordering::Release);
        // Double-check revocation after incrementing lease count to close race
        if self.is_revoked() {
            self.release_lease();
            return Err(SourceError::GrantRevoked);
        }
        Ok(NativeAccessLease {
            controller: Arc::clone(self),
        })
    }

    /// Internal release called when a [`NativeAccessLease`] is dropped.
    fn release_lease(&self) {
        let prev = self.active_leases.fetch_sub(1, Ordering::Release);
        if prev == 1 && self.is_revoked() {
            // Last outstanding lease dropped while revoked: drain is now complete
            self.drain_complete.store(true, Ordering::Release);
        }
    }

    /// Marks this controller revoked, stops new admissions, and checks if drain is immediate.
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
        self.generation.fetch_add(1, Ordering::Release);
        if self.active_leases() == 0 {
            self.drain_complete.store(true, Ordering::Release);
        }
    }
}

/// An RAII lease for native access to an authorized root.
///
/// Ensures balanced native access acquisition and release. When dropped,
/// decrements the active lease count on the associated [`RootAccessController`].
#[derive(Debug)]
pub struct NativeAccessLease {
    controller: Arc<RootAccessController>,
}

impl NativeAccessLease {
    pub fn root_id(&self) -> RootId {
        self.controller.root_id()
    }

    pub fn is_valid(&self) -> bool {
        !self.controller.is_revoked()
    }
}

impl Drop for NativeAccessLease {
    fn drop(&mut self) {
        self.controller.release_lease();
    }
}

/// Represents a source root whose access state is tracked by a session.
#[derive(Clone, Debug)]
pub struct RestoredRoot {
    root_id: RootId,
    path: RawPath,
    status: RootAccessStatus,
    grant: Option<RootGrant>,
    controller: Arc<RootAccessController>,
}

impl RestoredRoot {
    /// Creates a restored root with an explicit initial access status.
    pub fn new(
        root_id: RootId,
        path: impl Into<RawPath>,
        status: RootAccessStatus,
        grant: Option<RootGrant>,
    ) -> Self {
        let controller = RootAccessController::new(root_id);
        Self {
            root_id,
            path: path.into(),
            status,
            grant,
            controller,
        }
    }

    pub fn root_id(&self) -> RootId {
        self.root_id
    }

    pub fn path(&self) -> &RawPath {
        &self.path
    }

    pub fn status(&self) -> RootAccessStatus {
        self.status
    }

    pub fn grant(&self) -> Option<&RootGrant> {
        self.grant.as_ref()
    }

    pub fn controller(&self) -> &Arc<RootAccessController> {
        &self.controller
    }

    /// Acquires a balanced native access lease for reading within this root.
    pub fn acquire_lease(&self) -> Result<NativeAccessLease, SourceError> {
        if !self.status.is_active() {
            return match self.status {
                RootAccessStatus::Revoked => Err(SourceError::GrantRevoked),
                _ => Err(SourceError::RootUnavailable),
            };
        }
        self.controller.acquire_lease()
    }

    /// Deliberate user or host reauthorization action restoring an Unavailable or Stale root.
    pub fn reauthorize(&mut self, new_grant: RootGrant) -> Result<(), SourceError> {
        if new_grant.root_id() != self.root_id {
            return Err(SourceError::ForeignOwner);
        }
        new_grant.validate_active()?;
        self.grant = Some(new_grant);
        self.status = RootAccessStatus::Active;
        self.controller = RootAccessController::new(self.root_id);
        Ok(())
    }

    /// Revokes native access to this root.
    ///
    /// Stops new admission, revokes the held grant, and initiates safe draining
    /// of outstanding native access leases. Does NOT delete user annotations
    /// or wipe cache artifacts (those are independent actions).
    pub fn revoke(&mut self) {
        self.status = RootAccessStatus::Revoked;
        if let Some(grant) = &self.grant {
            grant.revoke();
        }
        self.controller.revoke();
    }
}

/// Registry of source roots managed within one browser session.
///
/// Ensures independent multi-root isolation: operations on one root cannot
/// compromise or alias another authorized root.
#[derive(Debug, Default)]
pub struct RootSessionRegistry {
    roots: BTreeMap<RootId, RestoredRoot>,
}

impl RootSessionRegistry {
    pub fn new() -> Self {
        Self {
            roots: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.roots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub fn register_root(&mut self, root: RestoredRoot) {
        self.roots.insert(root.root_id(), root);
    }

    pub fn get_root(&self, id: RootId) -> Option<&RestoredRoot> {
        self.roots.get(&id)
    }

    pub fn get_root_mut(&mut self, id: RootId) -> Option<&mut RestoredRoot> {
        self.roots.get_mut(&id)
    }

    /// Restores a root from a saved bookmark descriptor.
    ///
    /// Performs physical check of the root path:
    /// - If path is valid directory -> [`RootAccessStatus::Active`] with newly minted [`RootGrant`].
    /// - If path is missing -> [`RootAccessStatus::Unavailable(NotFound)`].
    /// - If bookmark is marked stale -> [`RootAccessStatus::Stale(SecurityBookmarkNeedsRefresh)`].
    pub fn restore_saved_descriptor(
        &mut self,
        root_id: RootId,
        descriptor: &SecurityScopedBookmark,
    ) -> &RestoredRoot {
        let target_path = descriptor.raw_path().to_path_buf();

        let status = if descriptor.is_stale() {
            RootAccessStatus::Stale(StaleReason::SecurityBookmarkNeedsRefresh)
        } else if !target_path.exists() {
            RootAccessStatus::Unavailable(UnavailableReason::NotFound)
        } else if !target_path.is_dir() {
            RootAccessStatus::Unavailable(UnavailableReason::InvalidDescriptor)
        } else {
            RootAccessStatus::Active
        };

        let grant = if status.is_active() {
            Some(RootGrant::new(root_id, descriptor.raw_path().clone()))
        } else {
            None
        };

        let restored = RestoredRoot::new(root_id, descriptor.raw_path().clone(), status, grant);
        self.roots.insert(root_id, restored);
        self.roots.get(&root_id).unwrap()
    }

    /// Revokes one root by ID without affecting any other registered roots.
    pub fn revoke_root(&mut self, id: RootId) -> Result<(), SourceError> {
        let root = self.roots.get_mut(&id).ok_or(SourceError::RootUnavailable)?;
        root.revoke();
        Ok(())
    }
}

impl fmt::Display for RootAccessStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Stale(reason) => write!(f, "stale({:?})", reason),
            Self::Unavailable(reason) => write!(f, "unavailable({:?})", reason),
            Self::Revoked => write!(f, "revoked"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::ArenaOwnerId;

    fn test_root_id(owner: u64, root: u64) -> RootId {
        let owner_id = ArenaOwnerId::new(owner).unwrap();
        RootId::new(owner_id, root).unwrap()
    }

    #[test]
    fn restored_root_status_and_lease_lifecycle() {
        let root_id = test_root_id(30, 1);
        let grant = RootGrant::new(root_id, "/workspace/test");
        let mut restored = RestoredRoot::new(
            root_id,
            "/workspace/test",
            RootAccessStatus::Active,
            Some(grant),
        );

        assert!(restored.status().is_active());

        // Acquire lease
        let lease1 = restored.acquire_lease().unwrap();
        assert_eq!(restored.controller().active_leases(), 1);
        assert!(lease1.is_valid());

        // Acquire second lease
        let lease2 = restored.acquire_lease().unwrap();
        assert_eq!(restored.controller().active_leases(), 2);

        // Drop first lease
        drop(lease1);
        assert_eq!(restored.controller().active_leases(), 1);

        // Revoke while lease2 is still held
        restored.revoke();
        assert!(restored.status().is_revoked());
        assert!(restored.controller().is_revoked());
        assert!(!restored.controller().is_drained()); // lease2 still held

        // Attempting to acquire lease while revoked fails
        assert_eq!(restored.acquire_lease().unwrap_err(), SourceError::GrantRevoked);

        // Drop second lease -> drain completes
        drop(lease2);
        assert_eq!(restored.controller().active_leases(), 0);
        assert!(restored.controller().is_drained());
    }

    #[test]
    fn failed_restoration_leaves_visible_unavailable_state() {
        let mut registry = RootSessionRegistry::new();
        let root_id = test_root_id(31, 1);

        // Non-existent path descriptor
        let missing_descriptor = SecurityScopedBookmark::new(
            "/non_existent_mount/missing_repo",
            vec![1, 2, 3],
            SandboxModel::AppSandbox,
        );

        let restored = registry.restore_saved_descriptor(root_id, &missing_descriptor);
        assert_eq!(
            restored.status(),
            RootAccessStatus::Unavailable(UnavailableReason::NotFound)
        );
        assert!(restored.grant().is_none());

        // Attempting to acquire lease on unavailable root fails with RootUnavailable
        assert_eq!(restored.acquire_lease().unwrap_err(), SourceError::RootUnavailable);

        // Reauthorization with active grant restores to Active
        let mut restored_mut = registry.get_root_mut(root_id).unwrap().clone();
        let valid_grant = RootGrant::new(root_id, "/reauthorized/path");
        restored_mut.reauthorize(valid_grant).unwrap();
        assert!(restored_mut.status().is_active());
        assert!(restored_mut.grant().is_some());
    }

    #[test]
    fn selective_revocation_preserves_independent_roots() {
        let mut registry = RootSessionRegistry::new();
        let r1 = test_root_id(32, 1);
        let r2 = test_root_id(32, 2);

        let root1 = RestoredRoot::new(
            r1,
            "/workspace/repo1",
            RootAccessStatus::Active,
            Some(RootGrant::new(r1, "/workspace/repo1")),
        );
        let root2 = RestoredRoot::new(
            r2,
            "/workspace/repo2",
            RootAccessStatus::Active,
            Some(RootGrant::new(r2, "/workspace/repo2")),
        );

        registry.register_root(root1);
        registry.register_root(root2);

        // Revoke root1 only
        registry.revoke_root(r1).unwrap();

        assert!(registry.get_root(r1).unwrap().status().is_revoked());
        assert!(registry.get_root(r2).unwrap().status().is_active());

        // Root2 can still acquire leases cleanly
        let lease2 = registry.get_root(r2).unwrap().acquire_lease();
        assert!(lease2.is_ok());
    }
}

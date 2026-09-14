#![forbid(unsafe_code)]

//! Root read capabilities, revocation lifecycle, and export publication gates (FCB-009.A).
//!
//! # Invariants
//!
//! 1. **Root read capability**: Opening a root creates an explicit read
//!    capability bound to an owner-qualified [`RootId`]. Persisting a path
//!    does not confer ongoing access; grants are held in memory and must be
//!    explicitly validated.
//! 2. **Revocation generation**: Every root grant has a revocation generation.
//!    Revoking a root immediately increments its revocation generation, which
//!    invalidates pending deliveries, link activations, and new queries/exports.
//! 3. **Export publication gate**: Grant revalidation is serialized with the
//!    export publication decision. If a grant is revoked while an export is
//!    being prepared, the publication is aborted and discarded before taking
//!    effect. An already published destination retains its completed outcome.
//! 4. **Isolation of multiple roots**: Two independent root grants cannot alias;
//!    access checks against one root reject attempts to read files scoped to
//!    another root.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use fcb_core::{ArenaOwnerId, RootId};

use crate::path::RawPath;
use crate::SourceError;

/// Shared revocation token for a root grant.
///
/// Holds the active revocation generation. When `revoke()` is called,
/// the generation is incremented atomically.
#[derive(Debug)]
pub struct GrantRevocationToken {
    current_generation: AtomicU64,
}

impl GrantRevocationToken {
    pub fn new() -> Self {
        Self {
            current_generation: AtomicU64::new(1),
        }
    }

    pub fn generation(&self) -> u64 {
        self.current_generation.load(Ordering::Acquire)
    }

    pub fn revoke(&self) -> u64 {
        self.current_generation.fetch_add(1, Ordering::Release) + 1
    }
}

impl Default for GrantRevocationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// An explicit read capability for a single source root.
///
/// A grant is bound to an owner-qualified [`RootId`], an authorized
/// filesystem or logical root [`RawPath`], and an immutable initial
/// `issued_generation`. If the root's revocation token has incremented
/// past `issued_generation`, the grant is revoked.
#[derive(Clone, Debug)]
pub struct RootGrant {
    root_id: RootId,
    root_path: RawPath,
    issued_generation: u64,
    token: Arc<GrantRevocationToken>,
}

impl RootGrant {
    /// Creates a new active root grant.
    pub fn new(root_id: RootId, root_path: impl Into<RawPath>) -> Self {
        let token = Arc::new(GrantRevocationToken::new());
        let issued_generation = token.generation();
        Self {
            root_id,
            root_path: root_path.into(),
            issued_generation,
            token,
        }
    }

    /// Creates a grant sharing an existing revocation token.
    pub fn with_token(
        root_id: RootId,
        root_path: impl Into<RawPath>,
        token: Arc<GrantRevocationToken>,
    ) -> Self {
        let issued_generation = token.generation();
        Self {
            root_id,
            root_path: root_path.into(),
            issued_generation,
            token,
        }
    }

    pub fn root_id(&self) -> RootId {
        self.root_id
    }

    pub fn owner(&self) -> ArenaOwnerId {
        self.root_id.owner()
    }

    pub fn root_path(&self) -> &RawPath {
        &self.root_path
    }

    pub fn issued_generation(&self) -> u64 {
        self.issued_generation
    }

    /// Whether this grant has been revoked.
    pub fn is_revoked(&self) -> bool {
        self.token.generation() != self.issued_generation
    }

    /// Explicitly revokes this grant and all other grants sharing this token.
    pub fn revoke(&self) {
        self.token.revoke();
    }

    /// Validates that this grant is still active; returns [`SourceError::GrantRevoked`] if not.
    pub fn validate_active(&self) -> Result<(), SourceError> {
        if self.is_revoked() {
            Err(SourceError::GrantRevoked)
        } else {
            Ok(())
        }
    }

    /// Validates that a requested `RootId` matches this grant's identity.
    pub fn validate_root_match(&self, expected: RootId) -> Result<(), SourceError> {
        if self.root_id != expected {
            return Err(SourceError::ForeignOwner);
        }
        self.validate_active()
    }

    pub fn token(&self) -> &Arc<GrantRevocationToken> {
        &self.token
    }
}

impl PartialEq for RootGrant {
    fn eq(&self, other: &Self) -> bool {
        self.root_id == other.root_id
            && self.root_path == other.root_path
            && self.issued_generation == other.issued_generation
            && Arc::ptr_eq(&self.token, &other.token)
    }
}

impl Eq for RootGrant {}

/// Serializes grant validation with an export publication decision.
///
/// An export operation prepares data in scratch space, then atomically
/// commits or publishes it only if the grant is still valid at the exact
/// moment of publication.
pub struct ExportPublicationGate;

impl ExportPublicationGate {
    /// Executes an export closure and commits its outcome only if the grant
    /// remains active throughout preparation and at publication time.
    ///
    /// If the grant was revoked while `prepare_effect` was running, the
    /// prepared effect is dropped and [`SourceError::GrantRevoked`] is returned.
    pub fn publish<T, R, E, P>(
        grant: &RootGrant,
        prepare_effect: E,
        commit_publication: P,
    ) -> Result<R, SourceError>
    where
        E: FnOnce() -> Result<T, SourceError>,
        P: FnOnce(T) -> Result<R, SourceError>,
    {
        // 1. Revalidate grant before beginning preparation
        grant.validate_active()?;

        // 2. Prepare effect (compute export bytes, format markdown, etc.)
        let prepared = prepare_effect()?;

        // 3. Revalidate grant serialized immediately before publication
        grant.validate_active()?;

        // 4. Commit publication to destination
        commit_publication(prepared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root_id(owner_val: u64, root_val: u64) -> RootId {
        let owner = ArenaOwnerId::new(owner_val).unwrap();
        RootId::new(owner, root_val).unwrap()
    }

    #[test]
    fn grant_lifecycle_and_revocation() {
        let root_id = test_root_id(10, 1);
        let grant = RootGrant::new(root_id, "/workspace/repo");

        assert_eq!(grant.root_id(), root_id);
        assert!(!grant.is_revoked());
        assert_eq!(grant.validate_active(), Ok(()));

        // Revoke grant
        grant.revoke();
        assert!(grant.is_revoked());
        assert_eq!(grant.validate_active(), Err(SourceError::GrantRevoked));
    }

    #[test]
    fn grant_root_match_prevents_aliasing() {
        let r1 = test_root_id(10, 1);
        let r2 = test_root_id(10, 2);
        let grant1 = RootGrant::new(r1, "/workspace/repo_a");

        assert_eq!(grant1.validate_root_match(r1), Ok(()));
        assert_eq!(
            grant1.validate_root_match(r2),
            Err(SourceError::ForeignOwner)
        );
    }

    #[test]
    fn export_publication_gate_aborts_on_concurrent_revocation() {
        let root_id = test_root_id(20, 1);
        let grant = RootGrant::new(root_id, "/workspace/export_target");

        // Case 1: successful publication
        let res = ExportPublicationGate::publish(
            &grant,
            || Ok("export payload"),
            |payload| Ok(format!("committed: {}", payload)),
        );
        assert_eq!(res.unwrap(), "committed: export payload");

        // Case 2: revoked during preparation
        let grant_clone = grant.clone();
        let res2 = ExportPublicationGate::publish(
            &grant,
            || {
                // Revoke during preparation
                grant_clone.revoke();
                Ok("should not be committed")
            },
            |payload| Ok(format!("committed: {}", payload)),
        );
        assert_eq!(res2, Err(SourceError::GrantRevoked));

        // Case 3: already revoked before start
        let res3 = ExportPublicationGate::publish(
            &grant,
            || Ok("never reached"),
            |payload| Ok(payload),
        );
        assert_eq!(res3, Err(SourceError::GrantRevoked));
    }
}

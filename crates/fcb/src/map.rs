#![forbid(unsafe_code)]

//! Headless atlas layout, camera, visible plans, and exact file activation.
//!
//! The `map` feature is independent of search, source-provider services, native
//! rendering and persistence. A host supplies hierarchy metadata, stable file
//! bindings and already authorized captures. No pathname in this module grants
//! filesystem access; display labels never substitute for a FileId.

pub mod navigation;
#[cfg(feature = "search")]
pub mod workspace;
pub use navigation::{AtlasLocation, AtlasNavigation};
pub use fcb_map::*;
pub use fcb_core::{DisplayColorConfig, QueryGeneration, ResourceAllocationId,
    ResourceBudget, RootId};

use std::mem::size_of;
use fcb_core::{ByteLength, ResourceLease};
use crate::{BrowserSession, BrowserView, FcbError, FileId, SourceCapture};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AtlasNavigationError {
    Atlas(AtlasError),
    Source(FcbError),
    DuplicateBinding,
    UnboundSource,
    WrongCapture,
}
impl std::fmt::Display for AtlasNavigationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => write!(f, "{error}"),
            Self::Source(error) => write!(f, "{error}"),
            Self::DuplicateBinding => f.write_str("ATLAS_DUPLICATE_SOURCE_BINDING"),
            Self::UnboundSource => f.write_str("ATLAS_UNBOUND_SOURCE"),
            Self::WrongCapture => f.write_str("ATLAS_WRONG_SOURCE_CAPTURE"),
        }
    }
}
impl std::error::Error for AtlasNavigationError {}
impl From<AtlasError> for AtlasNavigationError {
    fn from(error: AtlasError) -> Self { Self::Atlas(error) }
}
impl From<FcbError> for AtlasNavigationError {
    fn from(error: FcbError) -> Self { Self::Source(error) }
}

/// Host-owned association between a layout parcel and a logical source file.
/// Both fields are validated against the prepared atlas before publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasSourceBinding {
    pub node: AtlasNodeId,
    pub file: FileId,
}

/// Snapshot-bound bidirectional file/parcel lookup. Unbound known parcels remain
/// visible but cannot silently turn their displayed names into provider requests.
/// Preparing the sorted table is worker work; lookups are binary searches.
pub struct AtlasSources<'index, 'layout> {
    index: &'index AtlasIndex<'layout>,
    bindings: Vec<AtlasSourceBinding>,
    by_file: Vec<usize>,
    _lease: ResourceLease,
}
impl<'index, 'layout> AtlasSources<'index, 'layout> {
    pub fn build(index: &'index AtlasIndex<'layout>, bindings: &[AtlasSourceBinding],
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, AtlasNavigationError> {
        if bindings.len() > index.len() { return Err(AtlasError::InvalidLimits.into()); }
        if canceled() { return Err(AtlasError::Canceled.into()); }
        for binding in bindings {
            if canceled() { return Err(AtlasError::Canceled.into()); }
            if binding.file.owner() != index.owner() { return Err(AtlasError::OwnerMismatch.into()); }
            if !matches!(index.node(binding.node)?.kind(), NodeKind::File | NodeKind::Placeholder) {
                return Err(AtlasError::NotFile.into());
            }
        }
        let charge = bindings.len().checked_mul(size_of::<AtlasSourceBinding>() + size_of::<usize>())
            .and_then(|n| n.checked_add(size_of::<Self>())).ok_or(AtlasError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(index.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AtlasError::ResourceDenied)?;
        let mut stored = Vec::new();
        let mut by_file = Vec::new();
        stored.try_reserve_exact(bindings.len()).map_err(|_| AtlasError::AllocationFailed)?;
        by_file.try_reserve_exact(bindings.len()).map_err(|_| AtlasError::AllocationFailed)?;
        if stored.capacity() > bindings.len() || by_file.capacity() > bindings.len() {
            return Err(AtlasError::ResourceDenied.into());
        }
        stored.extend_from_slice(bindings);
        stored.sort_unstable_by_key(|binding| binding.node);
        if stored.windows(2).any(|pair| pair[0].node == pair[1].node) {
            return Err(AtlasNavigationError::DuplicateBinding);
        }
        if canceled() { return Err(AtlasError::Canceled.into()); }
        by_file.extend(0..stored.len());
        by_file.sort_unstable_by_key(|&index| stored[index].file);
        if by_file.windows(2).any(|pair| stored[pair[0]].file == stored[pair[1]].file) {
            return Err(AtlasNavigationError::DuplicateBinding);
        }
        if canceled() { return Err(AtlasError::Canceled.into()); }
        Ok(Self { index, bindings: stored, by_file, _lease: lease })
    }
    pub fn index(&self) -> &'index AtlasIndex<'layout> { self.index }
    pub fn len(&self) -> usize { self.bindings.len() }
    pub fn is_empty(&self) -> bool { self.bindings.is_empty() }
    pub fn file(&self, node: AtlasNodeId) -> Result<FileId, AtlasNavigationError> {
        self.index.node(node)?;
        self.bindings.binary_search_by_key(&node, |binding| binding.node)
            .map(|position| self.bindings[position].file)
            .map_err(|_| AtlasNavigationError::UnboundSource)
    }
    pub fn node_for_file(&self, file: FileId) -> Result<AtlasNodeId, AtlasNavigationError> {
        if file.owner() != self.index.owner() { return Err(AtlasError::OwnerMismatch.into()); }
        self.by_file.binary_search_by_key(&file, |&index| self.bindings[index].file)
            .map(|position| self.bindings[self.by_file[position]].node)
            .map_err(|_| AtlasNavigationError::UnboundSource)
    }
    pub fn source_target(&self, presented: PresentedAtlas<'_>, hit: AtlasHit)
        -> Result<AtlasSourceTarget, AtlasNavigationError> {
        hit.validate(presented)?;
        if !matches!(hit.detail(), AtlasDetail::File | AtlasDetail::Placeholder) {
            return Err(AtlasError::NotFile.into());
        }
        Ok(AtlasSourceTarget { hit, file: self.file(hit.node())? })
    }
}

/// Exact file intent captured from one acknowledged presentation. It pins a
/// layout/file association, NOT unread bytes. A host captures this FileId under
/// its own grant, then revalidates this target before opening those bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasSourceTarget {
    hit: AtlasHit,
    file: FileId,
}
impl AtlasSourceTarget {
    pub const fn file(self) -> FileId { self.file }
    pub const fn node(self) -> AtlasNodeId { self.hit.node() }
    pub const fn frame(self) -> crate::PresentedFrameId { self.hit.frame() }
    pub fn validate(self, sources: &AtlasSources<'_, '_>, presented: PresentedAtlas<'_>)
        -> Result<(), AtlasNavigationError> {
        self.hit.validate(presented)?;
        if sources.file(self.hit.node())? != self.file { return Err(AtlasNavigationError::WrongCapture); }
        Ok(())
    }
}

impl BrowserSession {
    /// Activate a parcel using bytes supplied by the authorized host. No implicit
    /// provider call or path decoding occurs. Like metadata path navigation, this
    /// permits a current revision of the SAME file: the atlas never claimed to
    /// have searched or captured that file's content. Content hits instead use
    /// `open_search_hit` and its exact searched-revision contract.
    pub fn open_atlas_target(&self, sources: &AtlasSources<'_, '_>, presented: PresentedAtlas<'_>,
        target: AtlasSourceTarget, capture: SourceCapture) -> Result<BrowserView, AtlasNavigationError> {
        target.validate(sources, presented)?;
        if self.owner() != target.file.owner() || capture.owner() != self.owner() {
            return Err(FcbError::OwnerMismatch.into());
        }
        if capture.file() != target.file { return Err(AtlasNavigationError::WrongCapture); }
        Ok(self.open_capture(capture)?)
    }
}

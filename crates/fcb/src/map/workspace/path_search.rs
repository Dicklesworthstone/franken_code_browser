#![forbid(unsafe_code)]

//! Query overlays share a frozen workspace's file/parcel identities. Preparing
//! path keys is explicit worker work once per catalog, not work on camera input.
//! New queries replace only their bounded overlay; they never repack the atlas.
//! Counts describe retained matching FILES, never text occurrences/source lines.

use std::{cmp::Ordering, mem::size_of};
use fcb_core::{ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::search::{MembershipState, PathEntry, PathIndex, PathIndexLimits, PathRank,
    PathSearch, PathSearchError, PathSearchOptions};
use super::{AtlasNodeId, NodeKind, WorkspaceAtlas, WorkspaceAtlasError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WorkspacePathSearchError { Atlas(WorkspaceAtlasError), Path(PathSearchError) }
impl std::fmt::Display for WorkspacePathSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Atlas(e) => write!(f, "{e}"), Self::Path(e) => write!(f, "{e}") }
    }
}
impl std::error::Error for WorkspacePathSearchError {}
impl From<WorkspaceAtlasError> for WorkspacePathSearchError {
    fn from(e: WorkspaceAtlasError) -> Self { Self::Atlas(e) }
}
impl From<PathSearchError> for WorkspacePathSearchError {
    fn from(e: PathSearchError) -> Self { Self::Path(e) }
}

/// A path match names a file to explicitly capture, not an already-captured
/// source range. Rank semantics are those of the shared native-path engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasPathHit {
    pub node: AtlasNodeId,
    pub file: FileId,
    pub rank: PathRank,
}

pub struct WorkspacePathIndex<'atlas, 'catalog> {
    atlas: &'atlas WorkspaceAtlas<'catalog>,
    index: PathIndex,
}
impl<'atlas, 'catalog> WorkspacePathIndex<'atlas, 'catalog> {
    /// Allocations are [temporary entry descriptors, retained native path keys].
    /// Neither build nor query opens a source file or re-enumerates a directory.
    pub fn build(atlas: &'atlas WorkspaceAtlas<'catalog>, budget: &ResourceBudget,
        allocations: [ResourceAllocationId; 2], mut canceled: impl FnMut() -> bool)
        -> Result<Self, WorkspacePathSearchError> {
        atlas.validate_active()?;
        if allocations[0] == allocations[1] { return Err(PathSearchError::InvalidLimits.into()); }
        if canceled() { return Err(PathSearchError::Canceled.into()); }
        let catalog = atlas.catalog();
        let count = catalog.entries().len();
        let bytes = count.checked_mul(size_of::<PathEntry<'_>>())
            .and_then(|n| n.checked_add(size_of::<Vec<PathEntry<'_>>>()))
            .ok_or(PathSearchError::InvalidLimits)?;
        let _scratch = budget.try_reserve_managed(catalog.grant().owner(), allocations[0], ByteLength::new(bytes as u64))
            .map_err(|_| PathSearchError::ResourceDenied)?;
        let mut entries = reserve(count)?;
        for (i, entry) in catalog.entries().iter().enumerate() {
            if canceled() { return Err(PathSearchError::Canceled.into()); }
            entries.push(PathEntry::new(catalog.file_id(i).ok_or(PathSearchError::MissingFile)?,
                catalog.grant().root_id(), entry.path().raw()));
        }
        let membership = if catalog.discovery_complete() { MembershipState::Closed } else { MembershipState::Discovering };
        let index = PathIndex::build(catalog.id(), membership, &entries, PathIndexLimits::default(),
            budget, allocations[1], &mut canceled)?;
        atlas.validate_active()?;
        Ok(Self { atlas, index })
    }
    pub fn atlas(&self) -> &'atlas WorkspaceAtlas<'catalog> { self.atlas }
    pub fn index(&self) -> &PathIndex { &self.index }

    /// Worker convenience using the existing resumable engine. Retain this index
    /// for repeated queries. Allocations are [query work, retained overlay], with
    /// old/new/query overlap reserved separately. The overlay outlives this path
    /// index and borrows only the unchanged atlas, not temporary query strings.
    pub fn search(&self, needle: &[u8], options: PathSearchOptions, budget: &ResourceBudget,
        allocations: [ResourceAllocationId; 2], mut canceled: impl FnMut() -> bool)
        -> Result<AtlasPathOverlay<'atlas, 'catalog>, WorkspacePathSearchError> {
        self.atlas.validate_active()?;
        if allocations[0] == allocations[1] { return Err(PathSearchError::InvalidLimits.into()); }
        if canceled() { return Err(PathSearchError::Canceled.into()); }
        let mut stop = || canceled() || self.atlas.validate_active().is_err();
        let mut query = PathSearch::new(&self.index, needle, options, budget, allocations[0])?;
        query.run_to_completion(&mut stop)?;
        self.atlas.validate_active()?;
        if canceled() { return Err(PathSearchError::Canceled.into()); }
        let count = query.ranked_matches().len();
        let bytes = count.checked_mul(size_of::<AtlasPathHit>() + size_of::<usize>())
            .and_then(|n| n.checked_add(size_of::<AtlasPathOverlay<'_, '_>>()))
            .ok_or(PathSearchError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(self.index.id().owner(), allocations[1], ByteLength::new(bytes as u64))
            .map_err(|_| PathSearchError::ResourceDenied)?;
        let mut hits = reserve(count)?;
        for hit in query.ranked_matches() {
            if canceled() { return Err(PathSearchError::Canceled.into()); }
            let node = self.atlas.node_for_file(hit.file_id())?;
            hits.push(AtlasPathHit { node, file: hit.file_id(), rank: hit.rank() });
        }
        let mut by_path = reserve(count)?;
        by_path.extend(0..count);
        by_path.sort_unstable_by(|&a, &b| {
            self.atlas.layout().nodes()[hits[a].node.ordinal() as usize].path()
                .cmp(self.atlas.layout().nodes()[hits[b].node.ordinal() as usize].path())
        });
        self.atlas.validate_active()?;
        if canceled() { return Err(PathSearchError::Canceled.into()); }
        Ok(AtlasPathOverlay { atlas: self.atlas, hits, by_path, generation: query.generation(),
            matches_seen: query.matches_seen(), complete: query.is_complete(), truncated: query.truncated(),
            _lease: lease })
    }
}

pub struct AtlasPathOverlay<'atlas, 'catalog> {
    atlas: &'atlas WorkspaceAtlas<'catalog>,
    hits: Vec<AtlasPathHit>, // Existing engine's ranked order.
    by_path: Vec<usize>,    // For logarithmic subtree counts, not rank order.
    generation: QueryGeneration,
    matches_seen: usize,
    complete: bool,
    truncated: bool,
    _lease: ResourceLease,
}
impl<'atlas, 'catalog> AtlasPathOverlay<'atlas, 'catalog> {
    pub fn atlas(&self) -> &'atlas WorkspaceAtlas<'catalog> { self.atlas }
    pub fn hits(&self) -> &[AtlasPathHit] { &self.hits }
    pub fn generation(&self) -> QueryGeneration { self.generation }
    pub fn matches_seen(&self) -> usize { self.matches_seen }
    /// Completeness of catalog membership AND match count, separate from rows.
    pub fn is_complete(&self) -> bool { self.complete }
    pub fn truncated(&self) -> bool { self.truncated }
    pub fn validate_delivery(&self, atlas: &WorkspaceAtlas<'_>, generation: QueryGeneration)
        -> Result<(), WorkspacePathSearchError> {
        self.atlas.validate_active()?;
        if !std::ptr::eq(self.atlas, atlas) { return Err(PathSearchError::StaleIndex.into()); }
        if self.generation != generation { return Err(PathSearchError::StaleQuery.into()); }
        Ok(())
    }
    /// Count retained matched files in a real hierarchy node in O(log hits)
    /// comparisons. This is NOT a total count when top-k or membership is partial.
    /// A visible sibling-group box may cover only part of its parent: callers
    /// must not attach a parent's count to that group's smaller rectangle.
    pub fn retained_matches_in(&self, node: AtlasNodeId) -> Result<usize, WorkspacePathSearchError> {
        self.validate_delivery(self.atlas, self.generation)?;
        if node.root() != self.atlas.layout().root() || node.layout() != self.atlas.layout().revision() {
            return Err(PathSearchError::StaleIndex.into());
        }
        let node = self.atlas.layout().nodes().get(node.ordinal() as usize).ok_or(PathSearchError::MissingFile)?;
        if node.path().is_empty() { return Ok(self.hits.len()); }
        let compare = |&i: &usize| {
            let path = self.atlas.layout().nodes()[self.hits[i].node.ordinal() as usize].path();
            if node.kind() == NodeKind::Directory { subtree_compare(path, node.path()) }
            else { path.cmp(node.path()) }
        };
        let start = self.by_path.partition_point(|i| compare(i) == Ordering::Less);
        let end = self.by_path.partition_point(|i| compare(i) != Ordering::Greater);
        Ok(end - start)
    }
}

// Compare with the virtual prefix dir + '/' without allocating in a camera
// consumer. Equal covers ALL descendants, not adjacent names such as src-old.
fn subtree_compare(path: &[u8], dir: &[u8]) -> Ordering {
    let common = path.len().min(dir.len());
    let prefix = path[..common].cmp(&dir[..common]);
    if prefix != Ordering::Equal { return prefix; }
    if path.len() <= dir.len() { return Ordering::Less; }
    path[dir.len()].cmp(&b'/')
}
fn reserve<T>(capacity: usize) -> Result<Vec<T>, WorkspacePathSearchError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| PathSearchError::AllocationFailed)?;
    if values.capacity() > capacity { return Err(PathSearchError::ResourceDenied.into()); }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subtree_intervals_do_not_confuse_raw_prefixes_or_nested_directory_names() {
        for (candidate, expected) in [(b"src".as_slice(), Ordering::Less), (b"src-old/a", Ordering::Less),
            (b"src.rs", Ordering::Less), (b"src/a", Ordering::Equal), (b"src/deep/b", Ordering::Equal),
            (b"src0/a", Ordering::Greater), (b"srd/a", Ordering::Greater)] {
            assert_eq!(subtree_compare(candidate, b"src"), expected);
        }
        assert_eq!(subtree_compare(b"raw\xff/a", b"raw\xff"), Ordering::Equal);
    }
}

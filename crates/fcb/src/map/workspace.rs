#![forbid(unsafe_code)]

//! Metadata-only bridge from a frozen workspace catalog to the retained atlas.
//!
//! Preparation is explicit worker work. It does not open source files, build a
//! search index, start a runtime, or imply that the repository is atomic. The
//! catalog and layout remain borrowed/owned separately from `AtlasIndex`, so a
//! host can retain one spatial index across camera updates without self-reference.
//! Empty directories and undiscovered/unavailable entries are NOT manufactured:
//! this is an atlas of catalogued regular files and their ancestor directories.

pub mod path_search;
pub mod text_search;
pub mod text_preview;

use std::mem::size_of;
use fcb_core::{ByteLength, FileId, LayoutRevision, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::search::workspace::{WorkspaceCatalog, WorkspaceEntry, WorkspaceError, WorkspaceStage};
use super::{AtlasBuildLimits, AtlasError, AtlasIndex, AtlasNavigationError, AtlasNodeId,
    AtlasSourceBinding, AtlasSources, HierarchySpec, LayoutError, LayoutOptions, NodeKind,
    NodeSpec, PartitionLayout, Size2D, commit_layout};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WorkspaceAtlasError {
    Workspace(WorkspaceError), Layout(LayoutError), Atlas(AtlasError),
    Navigation(AtlasNavigationError), InvalidLimits, WrongIndex,
}
impl std::fmt::Display for WorkspaceAtlasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workspace(e) => write!(f, "{e}"), Self::Layout(e) => write!(f, "{e}"),
            Self::Atlas(e) => write!(f, "{e}"), Self::Navigation(e) => write!(f, "{e}"),
            Self::InvalidLimits => f.write_str("WORKSPACE_ATLAS_INVALID_LIMITS"),
            Self::WrongIndex => f.write_str("WORKSPACE_ATLAS_WRONG_INDEX"),
        }
    }
}
impl std::error::Error for WorkspaceAtlasError {}
impl From<WorkspaceError> for WorkspaceAtlasError {
    fn from(e: WorkspaceError) -> Self { Self::Workspace(e) }
}
impl From<LayoutError> for WorkspaceAtlasError {
    fn from(e: LayoutError) -> Self { Self::Layout(e) }
}
impl From<AtlasError> for WorkspaceAtlasError {
    fn from(e: AtlasError) -> Self { Self::Atlas(e) }
}
impl From<AtlasNavigationError> for WorkspaceAtlasError {
    fn from(e: AtlasNavigationError) -> Self { Self::Navigation(e) }
}

/// Preparation limits include inferred ancestor directories, not just files.
/// The depth cap also bounds recursion in the existing partition constructor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceAtlasLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_total_path_bytes: usize,
}
impl Default for WorkspaceAtlasLimits {
    fn default() -> Self {
        Self { max_nodes: 131_072, max_depth: 32, max_total_path_bytes: 8 * 1024 * 1024 }
    }
}

pub struct WorkspaceAtlas<'catalog> {
    catalog: &'catalog WorkspaceCatalog,
    layout: PartitionLayout,
    bindings: Vec<AtlasSourceBinding>, // Sorted by layout ordinal.
    by_file: Vec<usize>,              // Sorted by full owner-qualified FileId.
    limits: AtlasBuildLimits,
    _lease: ResourceLease,
}
impl<'catalog> WorkspaceAtlas<'catalog> {
    /// Build from a Ready catalog (which may have explicitly partial discovery).
    /// `revision` is a fresh host-owned layout identity, never a file identity.
    /// Reserve a conservative envelope for the existing layout builder's tree,
    /// path copies, maps and old/new vectors before constructing any of them.
    /// Sorting and packing are bounded whole preparation operations; they do not
    /// claim hard real-time cancellation within an individual allocation/sort.
    pub fn build(catalog: &'catalog WorkspaceCatalog, revision: LayoutRevision,
        world: Size2D, options: LayoutOptions, limits: WorkspaceAtlasLimits,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, WorkspaceAtlasError> {
        catalog.validate_active()?;
        if canceled() { return Err(AtlasError::Canceled.into()); }
        if catalog.stage() != WorkspaceStage::Ready { return Err(WorkspaceError::Pending.into()); }
        if revision.owner() != catalog.grant().owner() { return Err(AtlasError::OwnerMismatch.into()); }
        if !(1..=131_072).contains(&limits.max_nodes) || !(1..=32).contains(&limits.max_depth)
            || limits.max_total_path_bytes > 16 * 1024 * 1024 {
            return Err(WorkspaceAtlasError::InvalidLimits);
        }
        // Catalog entries are frozen in raw-path order. Each ancestor prefix is
        // counted once using the previous path, without allocating a second tree.
        let (nodes, path_bytes) = measure(catalog, limits, &mut canceled)?;
        let charge = nodes.checked_mul(1024)
            .and_then(|n| path_bytes.checked_mul(16).and_then(|p| n.checked_add(p)))
            .and_then(|n| n.checked_add(size_of::<Self>()))
            .ok_or(WorkspaceAtlasError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(catalog.grant().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AtlasError::ResourceDenied)?;
        let mut specs = reserve(catalog.entries().len())?;
        for entry in catalog.entries() {
            if canceled() { return Err(AtlasError::Canceled.into()); }
            specs.push(NodeSpec::new(copy_path(entry.path().as_bytes())?, NodeKind::File, Some(entry.observed_bytes())));
        }
        let spec = HierarchySpec::new(catalog.grant().owner(), catalog.grant().root_id(), specs)?;
        let layout = commit_layout(revision, world, &spec, options)?;
        if canceled() { return Err(AtlasError::Canceled.into()); }
        // A discrepancy is an invalid preparation, not an invitation to grow
        // beyond the capacity reserved before invoking the layout constructor.
        if layout.nodes().len() != nodes { return Err(AtlasError::InvalidHierarchy.into()); }
        let mut bindings = reserve(catalog.entries().len())?;
        for (ordinal, node) in layout.nodes().iter().enumerate() {
            if canceled() { return Err(AtlasError::Canceled.into()); }
            if node.kind() != NodeKind::File { continue; }
            let position = catalog.entries().binary_search_by(|entry| entry.path().as_bytes().cmp(node.path()))
                .map_err(|_| AtlasError::InvalidHierarchy)?;
            let file = catalog.file_id(position).ok_or(AtlasError::InvalidHierarchy)?;
            let ordinal = u32::try_from(ordinal).map_err(|_| WorkspaceAtlasError::InvalidLimits)?;
            bindings.push(AtlasSourceBinding { node: AtlasNodeId::new(layout.root(), revision, ordinal), file });
        }
        if bindings.len() != catalog.entries().len() { return Err(AtlasError::InvalidHierarchy.into()); }
        let mut by_file = reserve(bindings.len())?;
        by_file.extend(0..bindings.len());
        by_file.sort_unstable_by_key(|&i| bindings[i].file);
        catalog.validate_active()?;
        if canceled() { return Err(AtlasError::Canceled.into()); }
        Ok(Self { catalog, layout, bindings, by_file, _lease: lease,
            limits: AtlasBuildLimits { max_nodes: limits.max_nodes, max_depth: limits.max_depth,
                max_path_bytes: crate::search::workspace::MAX_WORKSPACE_PATH_BYTES,
                max_total_path_bytes: limits.max_total_path_bytes } })
    }
    pub fn catalog(&self) -> &'catalog WorkspaceCatalog { self.catalog }
    pub fn layout(&self) -> &PartitionLayout { &self.layout }
    pub fn file_count(&self) -> usize { self.bindings.len() }
    /// Closed catalog membership, not full directory inventory or captured bytes.
    pub fn discovery_complete(&self) -> bool { self.catalog.discovery_complete() }
    pub fn validate_active(&self) -> Result<(), WorkspaceAtlasError> { Ok(self.catalog.validate_active()?) }

    /// Build once on the worker, retain across pan/zoom. The separately charged
    /// index borrows this exact immutable layout and does not access source I/O.
    pub fn index(&self, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<AtlasIndex<'_>, WorkspaceAtlasError> {
        self.validate_active()?;
        let index = AtlasIndex::build(&self.layout, self.limits, budget, allocation, &mut canceled)?;
        self.validate_active()?;
        Ok(index)
    }
    pub fn file(&self, node: AtlasNodeId) -> Result<FileId, WorkspaceAtlasError> {
        self.validate_active()?;
        if node.root().owner() != self.layout.owner() || node.layout().owner() != self.layout.owner() {
            return Err(AtlasError::OwnerMismatch.into());
        }
        if node.root() != self.layout.root() || node.layout() != self.layout.revision() {
            return Err(AtlasError::StaleLayout.into());
        }
        self.bindings.binary_search_by_key(&node, |b| b.node)
            .map(|i| self.bindings[i].file).map_err(|_| AtlasNavigationError::UnboundSource.into())
    }
    pub fn node_for_file(&self, file: FileId) -> Result<AtlasNodeId, WorkspaceAtlasError> {
        self.validate_active()?;
        if file.owner() != self.layout.owner() { return Err(AtlasError::OwnerMismatch.into()); }
        self.by_file.binary_search_by_key(&file, |&i| self.bindings[i].file)
            .map(|i| self.bindings[self.by_file[i]].node)
            .map_err(|_| AtlasNavigationError::UnboundSource.into())
    }
    pub fn entry(&self, node: AtlasNodeId) -> Result<&'catalog WorkspaceEntry, WorkspaceAtlasError> {
        self.catalog.entry(self.file(node)?).ok_or_else(|| AtlasNavigationError::UnboundSource.into())
    }
    /// Attach the existing acknowledged-frame/source activation API. Matching
    /// numeric IDs alone cannot substitute a different layout allocation here.
    pub fn sources<'index, 'layout>(&'layout self, index: &'index AtlasIndex<'layout>,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<AtlasSources<'index, 'layout>, WorkspaceAtlasError> {
        self.validate_active()?;
        if !std::ptr::eq(index.layout(), &self.layout) { return Err(WorkspaceAtlasError::WrongIndex); }
        let sources = AtlasSources::build(index, &self.bindings, budget, allocation, &mut canceled)?;
        self.validate_active()?;
        Ok(sources)
    }
}

fn measure(catalog: &WorkspaceCatalog, limits: WorkspaceAtlasLimits,
    canceled: &mut impl FnMut() -> bool) -> Result<(usize, usize), WorkspaceAtlasError> {
    let (mut nodes, mut bytes) = (1usize, 0usize); // Root, including an empty scope.
    let mut previous: &[u8] = b"";
    for entry in catalog.entries() {
        if canceled() { return Err(AtlasError::Canceled.into()); }
        let path = entry.path().as_bytes();
        if path.is_empty() || path.starts_with(b"/") || path.contains(&0)
            || path.split(|&b| b == b'/').any(|part| part.is_empty() || part == b"." || part == b"..") {
            return Err(AtlasError::InvalidHierarchy.into());
        }
        let common = previous.iter().zip(path).take_while(|(a, b)| a == b).count();
        let mut depth = 1usize;
        nodes = nodes.checked_add(1).ok_or(WorkspaceAtlasError::InvalidLimits)?;
        bytes = bytes.checked_add(path.len()).ok_or(WorkspaceAtlasError::InvalidLimits)?;
        for (i, &byte) in path.iter().enumerate() {
            if byte != b'/' { continue; }
            depth += 1;
            if i >= common {
                nodes = nodes.checked_add(1).ok_or(WorkspaceAtlasError::InvalidLimits)?;
                bytes = bytes.checked_add(i).ok_or(WorkspaceAtlasError::InvalidLimits)?;
            }
        }
        if nodes > limits.max_nodes || bytes > limits.max_total_path_bytes || depth > limits.max_depth {
            return Err(WorkspaceAtlasError::InvalidLimits);
        }
        previous = path;
    }
    Ok((nodes, bytes))
}
fn reserve<T>(capacity: usize) -> Result<Vec<T>, WorkspaceAtlasError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| AtlasError::AllocationFailed)?;
    if values.capacity() > capacity { return Err(AtlasError::ResourceDenied.into()); }
    Ok(values)
}
fn copy_path(path: &[u8]) -> Result<Vec<u8>, WorkspaceAtlasError> {
    let mut bytes = reserve(path.len())?;
    bytes.extend_from_slice(path);
    Ok(bytes)
}

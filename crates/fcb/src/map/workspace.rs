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
pub mod stream_search;
pub mod retained;

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

/// A file-type display scope for the native atlas: catalogued metadata only,
/// never a source-content classification. Extension matching is ASCII
/// case-insensitive on the file's final path component; files without an
/// extension are displayed only in the All scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasScope {
    All,
    Extensions(AtlasExtensionScope),
}

impl AtlasScope {
    pub fn is_all(&self) -> bool { matches!(self, Self::All) }

    /// Metadata-only membership test over raw catalog path bytes. No file is
    /// read, parsed or classified beyond its extension bytes.
    pub fn matches_path(&self, path: &[u8]) -> bool {
        match self {
            Self::All => true,
            Self::Extensions(scope) => scope.matches_path(path),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AtlasScopeError {
    TooManyExtensions,
    InvalidExtension,
}

impl std::fmt::Display for AtlasScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooManyExtensions => "ATLAS_SCOPE_TOO_MANY_EXTENSIONS",
            Self::InvalidExtension => "ATLAS_SCOPE_INVALID_EXTENSION",
        })
    }
}
impl std::error::Error for AtlasScopeError {}

/// Bounded inline set of file extensions in canonical (sorted, deduplicated,
/// lowercase-folded) form. Fixed capacity keeps the scope Copy so camera
/// actions and host marshaling can move it by value without allocation.
#[derive(Clone, Copy, Debug)]
pub struct AtlasExtensionScope {
    count: u8,
    tokens: [[u8; MAX_EXTENSION_BYTES]; MAX_SCOPE_EXTENSIONS],
    lengths: [u8; MAX_SCOPE_EXTENSIONS],
}

pub const MAX_SCOPE_EXTENSIONS: usize = 16;
pub const MAX_EXTENSION_BYTES: usize = 16;

impl AtlasExtensionScope {
    /// Canonicalize: ASCII-lowercase fold, validate charset and UTF-8, sort,
    /// deduplicate. Two scopes constructed from different input orders with
    /// the same token set therefore compare equal.
    pub fn from_extensions<'tokens>(tokens: impl IntoIterator<Item = &'tokens [u8]>)
        -> Result<Self, AtlasScopeError> {
        let mut scope = Self { count: 0, tokens: [[0; MAX_EXTENSION_BYTES]; MAX_SCOPE_EXTENSIONS],
            lengths: [0; MAX_SCOPE_EXTENSIONS] };
        for token in tokens {
            if scope.count as usize == MAX_SCOPE_EXTENSIONS { return Err(AtlasScopeError::TooManyExtensions); }
            let folded = fold_extension(token)?;
            if scope.find(&folded).is_ok() { continue; } // Deduplicate silently.
            let slot = scope.count as usize;
            scope.tokens[slot][..folded.len()].copy_from_slice(&folded);
            scope.lengths[slot] = folded.len() as u8;
            scope.count += 1;
        }
        if scope.count == 0 { return Err(AtlasScopeError::InvalidExtension); }
        scope.sort_tokens();
        Ok(scope)
    }

    pub fn count(&self) -> usize { self.count as usize }

    /// Sorted token views in canonical order; every token is valid UTF-8.
    pub fn extensions(&self) -> impl Iterator<Item = &[u8]> {
        (0..self.count as usize).map(move |i| &self.tokens[i][..self.lengths[i] as usize])
    }

    fn matches_path(&self, path: &[u8]) -> bool {
        let component = match last_component(path) {
            Some(component) => component,
            None => return false,
        };
        let dot = match component.iter().rposition(|&b| b == b'.') {
            // A leading dot (".gitignore") is a name, not an extension.
            Some(0) | None => return false,
            Some(dot) => dot,
        };
        let mut folded = [0u8; MAX_EXTENSION_BYTES];
        let extension = &component[dot + 1..];
        if extension.len() > MAX_EXTENSION_BYTES { return false; }
        fold_into(extension, &mut folded) && self.find(&folded[..extension.len()]).is_ok()
    }

    fn find(&self, token: &[u8]) -> Result<usize, usize> {
        self.extensions().enumerate()
            .find(|(_, candidate)| *candidate == token)
            .map(|(i, _)| i)
            .ok_or(usize::MAX)
    }

    fn sort_tokens(&mut self) {
        let count = self.count as usize;
        for i in 1..count {
            let mut j = i;
            while j > 0 && self.tokens[j][..self.lengths[j] as usize] < self.tokens[j - 1][..self.lengths[j - 1] as usize] {
                self.tokens.swap(j, j - 1);
                self.lengths.swap(j, j - 1);
                j -= 1;
            }
        }
    }
}

impl PartialEq for AtlasExtensionScope {
    fn eq(&self, other: &Self) -> bool {
        self.count == other.count
            && self.extensions().zip(other.extensions()).all(|(a, b)| a == b)
    }
}
impl Eq for AtlasExtensionScope {}

fn fold_extension(token: &[u8]) -> Result<[u8; MAX_EXTENSION_BYTES], AtlasScopeError> {
    let mut folded = [0u8; MAX_EXTENSION_BYTES];
    if token.is_empty() || token.len() > MAX_EXTENSION_BYTES
        || std::str::from_utf8(token).is_err() {
        return Err(AtlasScopeError::InvalidExtension);
    }
    fold_into(token, &mut folded).ok_or(AtlasScopeError::InvalidExtension)?;
    Ok(folded)
}

/// ASCII-lowercase fold in place; rejects separators, NUL and control bytes.
fn fold_into(token: &[u8], folded: &mut [u8; MAX_EXTENSION_BYTES]) -> bool {
    for (out, &byte) in folded.iter_mut().zip(token) {
        *out = byte.to_ascii_lowercase();
        if matches!(byte, b'/' | b'.' | 0 | 1..=0x1f | 0x7f) { return false; }
    }
    true
}

fn last_component(path: &[u8]) -> Option<&[u8]> {
    if path.is_empty() { return None; }
    Some(match path.iter().rposition(|&b| b == b'/') {
        Some(slash) if slash + 1 == path.len() => return None,
        Some(slash) => &path[slash + 1..],
        None => path,
    })
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
    /// Build from a Ready catalog (which may have explicitly partial discovery),
    /// restricted to the supplied metadata display scope. `revision` is a fresh
    /// host-owned layout identity, never a file identity. Reserve a conservative
    /// envelope for the existing layout builder's tree, path copies, maps and
    /// old/new vectors before constructing any of them. Sorting and packing are
    /// bounded whole preparation operations; they do not claim hard real-time
    /// cancellation within an individual allocation/sort.
    pub fn build(catalog: &'catalog WorkspaceCatalog, scope: &AtlasScope, revision: LayoutRevision,
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
        // Catalog entries are frozen in raw-path order. Each ancestor prefix of
        // an in-scope file is counted once using the previous in-scope path,
        // without allocating a second tree.
        let (nodes, path_bytes, files) = measure(catalog, scope, limits, &mut canceled)?;
        let charge = nodes.checked_mul(1024)
            .and_then(|n| path_bytes.checked_mul(16).and_then(|p| n.checked_add(p)))
            .and_then(|n| n.checked_add(size_of::<Self>()))
            .ok_or(WorkspaceAtlasError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(catalog.grant().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AtlasError::ResourceDenied)?;
        let mut specs = reserve(files)?;
        for entry in catalog.entries() {
            if canceled() { return Err(AtlasError::Canceled.into()); }
            if !scope.matches_path(entry.path().as_bytes()) { continue; }
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
        if bindings.len() != files { return Err(AtlasError::InvalidHierarchy.into()); }
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

fn measure(catalog: &WorkspaceCatalog, scope: &AtlasScope, limits: WorkspaceAtlasLimits,
    canceled: &mut impl FnMut() -> bool) -> Result<(usize, usize, usize), WorkspaceAtlasError> {
    let (mut nodes, mut bytes) = (1usize, 0usize); // Root, including an empty scope.
    let mut files = 0usize;
    let mut previous: &[u8] = b"";
    for entry in catalog.entries() {
        if canceled() { return Err(AtlasError::Canceled.into()); }
        let path = entry.path().as_bytes();
        if path.is_empty() || path.starts_with(b"/") || path.contains(&0)
            || path.split(|&b| b == b'/').any(|part| part.is_empty() || part == b"." || part == b"..") {
            return Err(AtlasError::InvalidHierarchy.into());
        }
        // Path integrity is catalog-wide; counting is scope-restricted. The
        // ancestor-prefix bookkeeping advances only between in-scope files so
        // out-of-scope entries contribute no directories.
        if !scope.matches_path(path) { continue; }
        files = files.checked_add(1).ok_or(WorkspaceAtlasError::InvalidLimits)?;
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
    Ok((nodes, bytes, files))
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

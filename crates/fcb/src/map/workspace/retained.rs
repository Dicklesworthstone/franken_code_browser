#![forbid(unsafe_code)]

//! Owned catalog + layout + prepared spatial index for native hosts. Moving the
//! owner never rebuilds the map; borrowed AtlasIndex views share the existing
//! acceleration data. The original workspace file bindings and all three
//! catalog/layout/index leases survive. No source payload is captured here.
//! The frozen catalog is shared through an Arc so a host can hold one All
//! layout plus bounded alternative scope layouts over the same catalog without
//! re-discovering, re-reading or duplicating source identity.

use std::sync::Arc;
use fcb_map::atlas::retained::RetainedAtlasIndex;
use super::*;

pub struct RetainedWorkspaceAtlas {
    catalog: Arc<WorkspaceCatalog>,
    index: RetainedAtlasIndex,
    bindings: Vec<AtlasSourceBinding>,
    by_file: Vec<usize>,
    _layout_lease: ResourceLease,
}
impl RetainedWorkspaceAtlas {
    /// Consume one frozen Ready catalog, including explicitly partial scopes,
    /// into an unscoped (All) atlas. Allocations are [layout/bindings, spatial
    /// index]; catalog admission is already owned by the supplied value. This
    /// is worker preparation work.
    pub fn build(catalog: WorkspaceCatalog, revision: LayoutRevision,
        world: Size2D, options: LayoutOptions, limits: WorkspaceAtlasLimits,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
        mut canceled: impl FnMut() -> bool) -> Result<Self, WorkspaceAtlasError> {
        Self::build_shared(Arc::new(catalog), &AtlasScope::All, revision, world,
            options, limits, budget, allocations, canceled)
    }

    /// Build an alternative display scope over an already frozen shared
    /// catalog. Metadata only: no discovery, source I/O or catalog mutation.
    /// The revision must be fresh and distinct from every other live layout of
    /// the same owner; node identity is (root, revision, ordinal).
    pub fn build_scoped(catalog: Arc<WorkspaceCatalog>, scope: &AtlasScope, revision: LayoutRevision,
        world: Size2D, options: LayoutOptions, limits: WorkspaceAtlasLimits,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
        mut canceled: impl FnMut() -> bool) -> Result<Self, WorkspaceAtlasError> {
        if scope.is_all() { return Err(WorkspaceAtlasError::InvalidLimits); }
        Self::build_shared(catalog, scope, revision, world, options, limits,
            budget, allocations, canceled)
    }

    fn build_shared(catalog: Arc<WorkspaceCatalog>, scope: &AtlasScope, revision: LayoutRevision,
        world: Size2D, options: LayoutOptions, limits: WorkspaceAtlasLimits,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
        mut canceled: impl FnMut() -> bool) -> Result<Self, WorkspaceAtlasError> {
        if allocations[0] == allocations[1] { return Err(WorkspaceAtlasError::InvalidLimits); }
        let WorkspaceAtlas { layout, bindings, by_file, limits, _lease, .. } =
            WorkspaceAtlas::build(&catalog, scope, revision, world, options, limits, budget,
                allocations[0], &mut canceled)?;
        let index = RetainedAtlasIndex::build(layout, limits, budget, allocations[1], &mut canceled)?;
        catalog.validate_active()?;
        if canceled() { return Err(AtlasError::Canceled.into()); }
        Ok(Self { catalog, index, bindings, by_file, _layout_lease: _lease })
    }

    pub fn catalog(&self) -> &WorkspaceCatalog { &self.catalog }

    /// Shared frozen-catalog handle for alternative scope layouts. Cloning the
    /// Arc never re-discovers and never duplicates catalog storage.
    pub fn catalog_shared(&self) -> Arc<WorkspaceCatalog> { Arc::clone(&self.catalog) }
    pub fn layout(&self) -> &PartitionLayout { self.index.layout() }
    pub fn file_count(&self) -> usize { self.bindings.len() }
    pub fn discovery_complete(&self) -> bool { self.catalog.discovery_complete() }
    pub fn validate_active(&self) -> Result<(), WorkspaceAtlasError> { Ok(self.catalog.validate_active()?) }

    /// Constant-size view of the already-built spatial index. No allocation,
    /// path traversal, layout work or source I/O occurs here.
    pub fn index(&self) -> Result<AtlasIndex<'_>, WorkspaceAtlasError> {
        self.validate_active()?;
        Ok(self.index.index())
    }
    pub fn file(&self, node: AtlasNodeId) -> Result<FileId, WorkspaceAtlasError> {
        self.validate_active()?;
        self.index.index().node(node)?;
        self.bindings.binary_search_by_key(&node, |binding| binding.node)
            .map(|i| self.bindings[i].file).map_err(|_| AtlasNavigationError::UnboundSource.into())
    }
    pub fn node_for_file(&self, file: FileId) -> Result<AtlasNodeId, WorkspaceAtlasError> {
        self.validate_active()?;
        if file.owner() != self.layout().owner() { return Err(AtlasError::OwnerMismatch.into()); }
        self.by_file.binary_search_by_key(&file, |&i| self.bindings[i].file)
            .map(|i| self.bindings[self.by_file[i]].node)
            .map_err(|_| AtlasNavigationError::UnboundSource.into())
    }
    pub fn entry(&self, node: AtlasNodeId) -> Result<&WorkspaceEntry, WorkspaceAtlasError> {
        self.catalog.entry(self.file(node)?).ok_or_else(|| AtlasNavigationError::UnboundSource.into())
    }
    /// Existing conservative acknowledged-frame activation remains available.
    pub fn sources<'index, 'layout>(&'layout self, index: &'index AtlasIndex<'layout>,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        canceled: impl FnMut() -> bool) -> Result<AtlasSources<'index, 'layout>, WorkspaceAtlasError> {
        self.validate_active()?;
        if !std::ptr::eq(index.layout(), self.layout()) { return Err(WorkspaceAtlasError::WrongIndex); }
        let sources = AtlasSources::build(index, &self.bindings, budget, allocation, canceled)?;
        self.validate_active()?;
        Ok(sources)
    }
}

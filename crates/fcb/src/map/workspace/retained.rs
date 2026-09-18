#![forbid(unsafe_code)]

//! Owned catalog + layout + prepared spatial index for native hosts. Moving the
//! owner never rebuilds the map; borrowed AtlasIndex views share the existing
//! acceleration data. The original workspace file bindings and all three
//! catalog/layout/index leases survive. No source payload is captured here.

use fcb_map::atlas::retained::RetainedAtlasIndex;
use super::*;

pub struct RetainedWorkspaceAtlas {
    catalog: WorkspaceCatalog,
    index: RetainedAtlasIndex,
    bindings: Vec<AtlasSourceBinding>,
    by_file: Vec<usize>,
    _layout_lease: ResourceLease,
}
impl RetainedWorkspaceAtlas {
    /// Consume one frozen Ready catalog, including explicitly partial scopes.
    /// Allocations are [layout/bindings, spatial index]; catalog admission is
    /// already owned by the supplied value. This is worker preparation work.
    pub fn build(catalog: WorkspaceCatalog, revision: LayoutRevision,
        world: Size2D, options: LayoutOptions, limits: WorkspaceAtlasLimits,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
        mut canceled: impl FnMut() -> bool) -> Result<Self, WorkspaceAtlasError> {
        if allocations[0] == allocations[1] { return Err(WorkspaceAtlasError::InvalidLimits); }
        let WorkspaceAtlas { layout, bindings, by_file, limits, _lease, .. } =
            WorkspaceAtlas::build(&catalog, revision, world, options, limits, budget,
                allocations[0], &mut canceled)?;
        let index = RetainedAtlasIndex::build(layout, limits, budget, allocations[1], &mut canceled)?;
        catalog.validate_active()?;
        if canceled() { return Err(AtlasError::Canceled.into()); }
        Ok(Self { catalog, index, bindings, by_file, _layout_lease: _lease })
    }
    pub fn catalog(&self) -> &WorkspaceCatalog { &self.catalog }
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

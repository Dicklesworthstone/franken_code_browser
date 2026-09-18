#![forbid(unsafe_code)]

//! Movable ownership of the exact layout and its prepared spatial index.
//! Borrowed views share index storage without allocation, path sorting, or
//! rebuilding. The owner exposes no mutable layout and cannot rebind an index
//! to another layout with coincidentally equal IDs. Drop the owner on a worker.

use std::sync::Arc;
use super::{AtlasBuildLimits, AtlasError, AtlasIndex, Branch, IndexedNode};
use crate::PartitionLayout;
use fcb_core::{ResourceAllocationId, ResourceBudget, ResourceLease};

pub struct RetainedAtlasIndex {
    layout: PartitionLayout,
    nodes: Arc<Vec<IndexedNode>>,
    branches: Arc<Vec<Branch>>,
    children: Arc<Vec<usize>>,
    by_path: Arc<Vec<usize>>,
    root_index: usize,
    lease: ResourceLease,
}
impl RetainedAtlasIndex {
    /// Own an already prepared layout; build its spatial index exactly once.
    /// As with AtlasIndex::build, the caller retains the layout's admission.
    /// This allocation reserves the added index, not the supplied layout.
    pub fn build(layout: PartitionLayout, limits: AtlasBuildLimits,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        canceled: impl FnMut() -> bool) -> Result<Self, AtlasError> {
        let AtlasIndex { nodes, branches, children, by_path, root_index, _lease: lease, .. } =
            AtlasIndex::build(&layout, limits, budget, allocation, canceled)?;
        Ok(Self { layout, nodes, branches, children, by_path, root_index, lease })
    }
    pub fn layout(&self) -> &PartitionLayout { &self.layout }

    /// Constant-size borrowed view. No allocation, repository traversal, I/O,
    /// or geometry reconstruction. All existing visible-query, hit-test and
    /// source-binding consumers accept this SAME AtlasIndex type.
    pub fn index(&self) -> AtlasIndex<'_> {
        AtlasIndex { layout: &self.layout, nodes: Arc::clone(&self.nodes),
            branches: Arc::clone(&self.branches), children: Arc::clone(&self.children),
            by_path: Arc::clone(&self.by_path), root_index: self.root_index,
            _lease: self.lease.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{ArenaOwnerId, ByteLength, LayoutRevision, RootId};
    use crate::{HierarchySpec, LayoutOptions, NodeKind, NodeSpec, Size2D, commit_layout};
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2415).unwrap() }
    fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap() }
    fn layout(revision: u64) -> PartitionLayout {
        let spec = HierarchySpec::new(owner(), RootId::new(owner(), 1).unwrap(), vec![
            NodeSpec::new(b"src/a.rs".to_vec(), NodeKind::File, Some(20)),
            NodeSpec::new(b"src/b.rs".to_vec(), NodeKind::File, Some(40)),
            NodeSpec::new(b"README.md".to_vec(), NodeKind::File, Some(5)),
        ]).unwrap();
        commit_layout(LayoutRevision::new(owner(), revision).unwrap(), Size2D::new(1000.0, 800.0).unwrap(),
            &spec, LayoutOptions::modest()).unwrap()
    }

    #[test]
    fn moved_owner_reuses_identical_spatial_allocations_and_accounting() {
        let budget = budget();
        let retained = RetainedAtlasIndex::build(layout(1), AtlasBuildLimits::default(),
            &budget, allocation(1), || false).unwrap();
        let charge = budget.accounting().reserved();
        let address = retained.nodes.as_ptr();
        let moved = Box::new(retained);
        for _ in 0..100 {
            let index = moved.index();
            assert_eq!(index.nodes.as_ptr(), address);
            assert!(std::ptr::eq(index.layout(), moved.layout()));
            let source = index.find_path(b"src/a.rs").unwrap();
            assert_eq!(index.node(source).unwrap().path(), b"src/a.rs");
            assert_eq!(budget.accounting().reserved(), charge);
        }
        drop(moved);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }

    #[test]
    fn retained_and_borrowed_indexes_have_identical_queries() {
        let budget = budget();
        let a_layout = layout(1);
        let a = AtlasIndex::build(&a_layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
        let retained = RetainedAtlasIndex::build(layout(1), AtlasBuildLimits::default(), &budget, allocation(2), || false).unwrap();
        let b = retained.index();
        for node in a_layout.nodes() {
            let ak = a.find_path(node.path()).unwrap();
            let bk = b.find_path(node.path()).unwrap();
            assert_eq!(ak, bk);
            assert_eq!(a.bounds_in(ak, a.root_node()), b.bounds_in(bk, b.root_node()));
            assert_eq!(a.children(ak).unwrap().collect::<Vec<_>>(), b.children(bk).unwrap().collect::<Vec<_>>());
        }
    }

    #[test]
    fn denied_or_canceled_retention_does_not_leak_index_admission() {
        let budget = budget();
        assert!(matches!(RetainedAtlasIndex::build(layout(1), AtlasBuildLimits::default(),
            &budget, allocation(1), || true), Err(AtlasError::Canceled)));
        assert_eq!(budget.accounting().reserved().get(), 0);
        let limits = AtlasBuildLimits { max_nodes: 1, ..AtlasBuildLimits::default() };
        assert!(matches!(RetainedAtlasIndex::build(layout(1), limits,
            &budget, allocation(1), || false), Err(AtlasError::InvalidLimits)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

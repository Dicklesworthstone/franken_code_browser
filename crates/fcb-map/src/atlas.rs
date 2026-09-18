#![forbid(unsafe_code)]

//! Prepared spatial access to an existing retained partition layout.
//!
//! Preparation is explicit worker work, bounded by input/path/depth limits and
//! cancellable between nodes and sibling groups. The sort is a bounded, whole
//! preparation operation, not an interaction callback. Queries never sort paths,
//! copy filenames, rebuild layout or visit every hidden leaf.
//!
//! Every directory owns a balanced bounding hierarchy over its direct children.
//! This also avoids a linear sibling walk in a million-file flat directory.
//! Bounds stay parent-local; focusing a subtree starts a new local origin rather
//! than accumulating the absolute coordinates of all its ancestors.

pub mod retained;

use std::{mem::size_of, sync::Arc};
use fcb_core::{ArenaOwnerId, ByteLength, LayoutRevision, Rect2D,
    ResourceAllocationId, ResourceBudget, ResourceLease, RootId};
use crate::{LaidOutNode, NodeKind, PartitionLayout};
use crate::camera::{Camera2D, CameraError, checked_rect};

pub const MAX_ATLAS_NODES: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AtlasError {
    InvalidLimits, InvalidHierarchy, AllocationFailed, ResourceDenied,
    OwnerMismatch, StaleLayout, NodeNotFound, NotDescendant, Canceled,
    StaleQuery, NotComplete, DisplayMismatch, FrameMismatch, NotFile,
    Camera(CameraError),
}
impl std::fmt::Display for AtlasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Self::Camera(error) = self { return error.fmt(f); }
        f.write_str(match self {
            Self::InvalidLimits => "ATLAS_INVALID_LIMITS",
            Self::InvalidHierarchy => "ATLAS_INVALID_HIERARCHY",
            Self::AllocationFailed => "ATLAS_ALLOCATION_FAILED",
            Self::ResourceDenied => "ATLAS_RESOURCE_DENIED",
            Self::OwnerMismatch => "ATLAS_OWNER_MISMATCH",
            Self::StaleLayout => "ATLAS_STALE_LAYOUT",
            Self::NodeNotFound => "ATLAS_NODE_NOT_FOUND",
            Self::NotDescendant => "ATLAS_NOT_DESCENDANT",
            Self::Canceled => "ATLAS_CANCELED",
            Self::StaleQuery => "ATLAS_STALE_QUERY",
            Self::NotComplete => "ATLAS_NOT_COMPLETE",
            Self::DisplayMismatch => "ATLAS_DISPLAY_MISMATCH",
            Self::FrameMismatch => "ATLAS_FRAME_MISMATCH",
            Self::NotFile => "ATLAS_NOT_FILE",
            Self::Camera(_) => unreachable!(),
        })
    }
}
impl std::error::Error for AtlasError {}
impl From<CameraError> for AtlasError {
    fn from(error: CameraError) -> Self { Self::Camera(error) }
}

/// A layout-qualified location, NOT a persistent FileId or filesystem grant.
/// Ordinals are meaningful only in this root and immutable layout revision.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AtlasNodeId {
    root: RootId,
    layout: LayoutRevision,
    ordinal: u32,
}
impl AtlasNodeId {
    pub const fn new(root: RootId, layout: LayoutRevision, ordinal: u32) -> Self {
        Self { root, layout, ordinal }
    }
    pub const fn root(self) -> RootId { self.root }
    pub const fn layout(self) -> LayoutRevision { self.layout }
    pub const fn ordinal(self) -> u32 { self.ordinal }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasBuildLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_path_bytes: usize,
    pub max_total_path_bytes: usize,
}
impl Default for AtlasBuildLimits {
    fn default() -> Self {
        Self { max_nodes: 250_000, max_depth: 512, max_path_bytes: 16_384,
            max_total_path_bytes: 64 * 1024 * 1024 }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct IndexedNode {
    pub(crate) parent: Option<usize>,
    pub(crate) child_start: usize,
    pub(crate) child_end: usize,
    pub(crate) acceleration: Option<usize>,
    pub(crate) leaves: usize,
    pub(crate) depth: usize,
}
impl IndexedNode {
    fn empty() -> Self {
        Self { parent: None, child_start: 0, child_end: 0, acceleration: None, leaves: 0, depth: 0 }
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum BranchKind { Leaf(usize), Fork(usize, usize) }
#[derive(Clone, Copy, Debug)]
pub(crate) struct Branch {
    pub(crate) bounds: Rect2D,
    pub(crate) kind: BranchKind,
    pub(crate) parent: usize,
    pub(crate) leaves: usize,
}

pub struct AtlasIndex<'layout> {
    pub(crate) layout: &'layout PartitionLayout,
    pub(crate) nodes: Arc<Vec<IndexedNode>>,
    pub(crate) branches: Arc<Vec<Branch>>,
    children: Arc<Vec<usize>>,
    by_path: Arc<Vec<usize>>,
    root_index: usize,
    _lease: ResourceLease,
}
impl<'layout> AtlasIndex<'layout> {
    /// The caller separately owns/charges the retained layout. This lease covers
    /// all added index vectors, including construction scratch retained for lookup.
    pub fn build(layout: &'layout PartitionLayout, limits: AtlasBuildLimits,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, AtlasError> {
        let count = layout.nodes().len();
        if limits.max_nodes == 0 || limits.max_nodes > MAX_ATLAS_NODES
            || limits.max_depth == 0 || limits.max_depth > 4096
            || limits.max_path_bytes == 0 || limits.max_path_bytes > 65_536
            || count == 0 || count > limits.max_nodes { return Err(AtlasError::InvalidLimits); }
        if canceled() { return Err(AtlasError::Canceled); }
        let mut path_bytes = 0usize;
        for node in layout.nodes() {
            if canceled() { return Err(AtlasError::Canceled); }
            path_bytes = path_bytes.checked_add(node.path().len()).ok_or(AtlasError::InvalidLimits)?;
            if node.path().len() > limits.max_path_bytes || path_bytes > limits.max_total_path_bytes {
                return Err(AtlasError::InvalidLimits);
            }
            crate::validate_path(node.path()).map_err(|_| AtlasError::InvalidHierarchy)?;
            checked_rect(node.parent_local()).map_err(AtlasError::from)?;
        }
        let branch_capacity = count.checked_mul(2).ok_or(AtlasError::InvalidLimits)?;
        let charge = count.checked_mul(size_of::<IndexedNode>() + 2 * size_of::<usize>())
            .and_then(|n| branch_capacity.checked_mul(size_of::<Branch>()).and_then(|b| n.checked_add(b)))
            // Four shared vector headers/control blocks and the retained owner/view.
            .and_then(|n| n.checked_add(size_of::<Self>() + 256)).ok_or(AtlasError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(layout.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AtlasError::ResourceDenied)?;
        let mut nodes = reserved_vec::<IndexedNode>(count)?;
        let mut by_path = reserved_vec::<usize>(count)?;
        let mut children = reserved_vec::<usize>(count)?;
        let mut branches = reserved_vec::<Branch>(branch_capacity)?;
        nodes.resize(count, IndexedNode::empty());
        by_path.extend(0..count);
        by_path.sort_unstable_by(|&a, &b| layout.nodes()[a].path().cmp(layout.nodes()[b].path()));
        if canceled() { return Err(AtlasError::Canceled); }
        let root_index = by_path[0];
        if !layout.nodes()[root_index].path().is_empty() || layout.nodes()[root_index].kind() != NodeKind::Directory {
            return Err(AtlasError::InvalidHierarchy);
        }
        for pair in by_path.windows(2) {
            if layout.nodes()[pair[0]].path() == layout.nodes()[pair[1]].path() {
                return Err(AtlasError::InvalidHierarchy);
            }
        }
        // Prefix order guarantees each parent has already received its depth.
        for &index in &by_path {
            if canceled() { return Err(AtlasError::Canceled); }
            if index == root_index { continue; }
            let node = &layout.nodes()[index];
            let parent_path = crate::parent_path(node.path()).unwrap_or(b"");
            let position = by_path.binary_search_by(|&candidate| layout.nodes()[candidate].path().cmp(parent_path))
                .map_err(|_| AtlasError::InvalidHierarchy)?;
            let parent = by_path[position];
            if layout.nodes()[parent].kind() != NodeKind::Directory { return Err(AtlasError::InvalidHierarchy); }
            let depth = nodes[parent].depth.checked_add(1).ok_or(AtlasError::InvalidLimits)?;
            if depth > limits.max_depth { return Err(AtlasError::InvalidLimits); }
            let bounds = node.parent_local();
            let parent_size = layout.nodes()[parent].parent_local().size();
            // Layout's row arithmetic can leave a few ulps at a shared edge.
            let tolerance = parent_size.width().max(parent_size.height()).max(1.0) * f64::EPSILON * 32.0;
            if bounds.min_x() < -tolerance || bounds.min_y() < -tolerance
                || bounds.max_x() > parent_size.width() + tolerance
                || bounds.max_y() > parent_size.height() + tolerance { return Err(AtlasError::InvalidHierarchy); }
            nodes[index].parent = Some(parent);
            nodes[index].depth = depth;
            nodes[parent].child_end += 1;
        }
        // Turn per-parent child counts into contiguous stable child ranges.
        let mut total = 0usize;
        for node in &mut nodes {
            let count = node.child_end;
            node.child_start = total;
            node.child_end = total; // Used as the write cursor during scatter.
            total += count;
        }
        children.resize(total, 0);
        for &index in &by_path {
            if let Some(parent) = nodes[index].parent {
                children[nodes[parent].child_end] = index;
                nodes[parent].child_end += 1;
            }
        }
        for &index in by_path.iter().rev() {
            if canceled() { return Err(AtlasError::Canceled); }
            if nodes[index].child_start == nodes[index].child_end { nodes[index].leaves = 1; }
            if let Some(parent) = nodes[index].parent {
                nodes[parent].leaves = nodes[parent].leaves.checked_add(nodes[index].leaves)
                    .ok_or(AtlasError::InvalidLimits)?;
            }
        }
        for index in 0..count {
            if canceled() { return Err(AtlasError::Canceled); }
            let range = nodes[index].child_start..nodes[index].child_end;
            if !range.is_empty() {
                let branch = build_group(&children[range], index, layout, &nodes, &mut branches, &mut canceled)?;
                nodes[index].acceleration = Some(branch);
            }
        }
        if canceled() { return Err(AtlasError::Canceled); }
        Ok(Self { layout, nodes: Arc::new(nodes), branches: Arc::new(branches),
            children: Arc::new(children), by_path: Arc::new(by_path), root_index, _lease: lease })
    }

    pub fn owner(&self) -> ArenaOwnerId { self.layout.owner() }
    pub fn root(&self) -> RootId { self.layout.root() }
    pub fn revision(&self) -> LayoutRevision { self.layout.revision() }
    pub fn root_node(&self) -> AtlasNodeId { self.key(self.root_index) }
    pub fn len(&self) -> usize { self.nodes.len() }
    pub fn is_empty(&self) -> bool { self.nodes.is_empty() }
    pub fn acceleration_nodes(&self) -> usize { self.branches.len() }
    pub fn layout(&self) -> &'layout PartitionLayout { self.layout }
    pub fn find_path(&self, raw_path: &[u8]) -> Option<AtlasNodeId> {
        self.by_path.binary_search_by(|&index| self.layout.nodes()[index].path().cmp(raw_path))
            .ok().map(|position| self.key(self.by_path[position]))
    }
    pub fn node(&self, id: AtlasNodeId) -> Result<&'layout LaidOutNode, AtlasError> {
        Ok(&self.layout.nodes()[self.resolve(id)?])
    }
    pub fn parent(&self, id: AtlasNodeId) -> Result<Option<AtlasNodeId>, AtlasError> {
        Ok(self.nodes[self.resolve(id)?].parent.map(|index| self.key(index)))
    }
    pub fn children(&self, id: AtlasNodeId) -> Result<impl Iterator<Item = AtlasNodeId> + '_, AtlasError> {
        let node = self.nodes[self.resolve(id)?];
        Ok(self.children[node.child_start..node.child_end].iter().map(|&index| self.key(index)))
    }
    pub fn leaf_count(&self, id: AtlasNodeId) -> Result<usize, AtlasError> {
        Ok(self.nodes[self.resolve(id)?].leaves)
    }

    /// Bounds in a focus node's coordinate domain; no world-origin subtraction.
    /// Walking ancestors is limited by the build's admitted maximum depth.
    pub fn bounds_in(&self, id: AtlasNodeId, focus: AtlasNodeId) -> Result<Rect2D, AtlasError> {
        let index = self.resolve(id)?;
        let focus = self.resolve(focus)?;
        let size = self.layout.nodes()[index].parent_local().size();
        let mut current = index;
        let (mut x, mut y) = (0.0, 0.0);
        while current != focus {
            let local = self.layout.nodes()[current].parent_local();
            x += local.min_x(); y += local.min_y();
            current = self.nodes[current].parent.ok_or(AtlasError::NotDescendant)?;
        }
        let rect = Rect2D::from_xywh(x, y, size.width(), size.height())
            .map_err(|_| AtlasError::Camera(CameraError::InvalidGeometry))?;
        checked_rect(rect)?;
        Ok(rect)
    }

    /// Fit this subtree as a local focus island. Callers retain the parent key
    /// or semantic history, not a rounded absolute camera position, for return.
    pub fn focus_camera(&self, focus: AtlasNodeId, generation: fcb_core::CameraGeneration,
        display: fcb_core::DisplayMetrics, padding_points: f64) -> Result<Camera2D, AtlasError> {
        if generation.owner() != self.owner() { return Err(AtlasError::OwnerMismatch); }
        Ok(Camera2D::fit(generation, display, self.bounds_in(focus, focus)?, padding_points)?)
    }

    pub(crate) fn key(&self, index: usize) -> AtlasNodeId {
        AtlasNodeId { root: self.root(), layout: self.revision(), ordinal: index as u32 }
    }
    pub(crate) fn resolve(&self, id: AtlasNodeId) -> Result<usize, AtlasError> {
        if id.root.owner() != self.owner() || id.layout.owner() != self.owner() { return Err(AtlasError::OwnerMismatch); }
        if id.root != self.root() || id.layout != self.revision() { return Err(AtlasError::StaleLayout); }
        let index = id.ordinal as usize;
        if index >= self.nodes.len() { return Err(AtlasError::NodeNotFound); }
        Ok(index)
    }
}

pub(crate) fn reserved_vec<T>(capacity: usize) -> Result<Vec<T>, AtlasError> {
    let mut vector = Vec::new();
    vector.try_reserve_exact(capacity).map_err(|_| AtlasError::AllocationFailed)?;
    if vector.capacity() > capacity { return Err(AtlasError::ResourceDenied); }
    Ok(vector)
}

// Recursion is over balanced sibling GROUPS, never directory nesting. With the
// hard one-million-node input cap its maximum call depth is 21.
fn build_group(children: &[usize], parent: usize, layout: &PartitionLayout, nodes: &[IndexedNode],
    branches: &mut Vec<Branch>, canceled: &mut impl FnMut() -> bool) -> Result<usize, AtlasError> {
    if canceled() { return Err(AtlasError::Canceled); }
    let first = children[0];
    let index = branches.len();
    branches.push(Branch { bounds: layout.nodes()[first].parent_local(), kind: BranchKind::Leaf(first),
        parent, leaves: nodes[first].leaves });
    if children.len() > 1 {
        let middle = children.len() / 2;
        let left = build_group(&children[..middle], parent, layout, nodes, branches, canceled)?;
        let right = build_group(&children[middle..], parent, layout, nodes, branches, canceled)?;
        let bounds = branches[left].bounds.union(branches[right].bounds).map_err(|_| AtlasError::InvalidHierarchy)?;
        checked_rect(bounds)?;
        branches[index] = Branch { bounds, kind: BranchKind::Fork(left, right), parent,
            leaves: branches[left].leaves + branches[right].leaves };
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::Point2D;
    use crate::{HierarchySpec, LayoutOptions, NodeSpec, Size2D, commit_layout};
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(415).unwrap() }
    fn layout(revision: u64) -> PartitionLayout {
        let spec = HierarchySpec::new(owner(), RootId::new(owner(), 1).unwrap(), vec![
            NodeSpec::new(b"src/a.rs".to_vec(), NodeKind::File, Some(20)),
            NodeSpec::new(b"src/b.rs".to_vec(), NodeKind::File, Some(40)),
            NodeSpec::new(b"README.md".to_vec(), NodeKind::File, Some(5)),
            NodeSpec::new(vec![0xff], NodeKind::Placeholder, None),
        ]).unwrap();
        commit_layout(LayoutRevision::new(owner(), revision).unwrap(), Size2D::new(1000.0, 800.0).unwrap(),
            &spec, LayoutOptions::modest()).unwrap()
    }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap() }
    fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
    #[test]
    fn retains_raw_paths_parent_relations_and_exact_layout() {
        let layout = layout(1);
        let budget = budget();
        let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
        assert_eq!(index.len(), 6);
        assert_eq!(index.leaf_count(index.root_node()).unwrap(), 4);
        let a = index.find_path(b"src/a.rs").unwrap();
        let parent = index.parent(a).unwrap().unwrap();
        assert_eq!(index.node(parent).unwrap().path(), b"src");
        assert_eq!(index.children(parent).unwrap().count(), 2);
        assert_eq!(index.node(index.find_path(&[0xff]).unwrap()).unwrap().kind(), NodeKind::Placeholder);
        assert_eq!(index.layout() as *const _, &layout as *const _);
        let in_parent = index.bounds_in(a, parent).unwrap();
        assert_eq!(in_parent, index.node(a).unwrap().parent_local());
        assert_eq!(index.bounds_in(a, a).unwrap().origin(), Point2D::ORIGIN);
    }
    #[test]
    fn different_roots_revisions_and_nonancestors_are_not_interchangeable() {
        let old = layout(1); let new = layout(2); let budget = budget();
        let a = AtlasIndex::build(&old, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
        let b = AtlasIndex::build(&new, AtlasBuildLimits::default(), &budget, allocation(2), || false).unwrap();
        assert!(matches!(b.node(a.root_node()), Err(AtlasError::StaleLayout)));
        assert_eq!(a.bounds_in(a.find_path(b"README.md").unwrap(), a.find_path(b"src").unwrap()), Err(AtlasError::NotDescendant));
    }
    #[test]
    fn denied_canceled_and_overlimit_builds_release_all_index_bytes() {
        let layout = layout(1); let budget = budget();
        let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
        assert!(matches!(AtlasIndex::build(&layout, AtlasBuildLimits::default(), &tiny, allocation(1), || false), Err(AtlasError::ResourceDenied)));
        assert!(matches!(AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1),
            || budget.accounting().reserved().get() > 0), Err(AtlasError::Canceled)));
        let limits = AtlasBuildLimits { max_nodes: 2, ..AtlasBuildLimits::default() };
        assert!(matches!(AtlasIndex::build(&layout, limits, &budget, allocation(1), || false), Err(AtlasError::InvalidLimits)));
        assert_eq!(budget.accounting().reserved().get(), 0);
        let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
        assert!(budget.accounting().reserved().get() > 0);
        assert!(index.acceleration_nodes() <= 2 * index.len());
        drop(index); assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

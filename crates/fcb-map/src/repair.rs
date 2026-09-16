#![forbid(unsafe_code)]

//! Local slack repair and explicit restorable repack (FCB-013.B / fcb-d6x.2).
//!
//! Existing sibling parcels stay put. Newcomers consume retained slack. A
//! repair that would move an unaffected neighborhood is refused so interaction
//! can defer it. Global improvement is an explicit [`PartitionLayout::repack`]
//! that archives the previous generation.

use std::collections::BTreeMap;

use fcb_core::{LayoutRevision, Rect2D};

use crate::{
    build_tree, commit_layout, emit_tree, pack_ordered, HierarchySpec, LaidOutNode, LayoutError,
    PartitionLayout, TreeNode,
};

/// How far existing parcels may travel during a local repair.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplacementBudget {
    max_moved: u32,
    max_centroid_travel: f64,
}

impl DisplacementBudget {
    /// No existing parcel may change rectangle. Weight-only and slack inserts ok.
    pub fn freeze_neighborhoods() -> Self {
        Self {
            max_moved: 0,
            max_centroid_travel: 0.0,
        }
    }

    pub fn new(max_moved: u32, max_centroid_travel: f64) -> Result<Self, LayoutError> {
        if !max_centroid_travel.is_finite() || max_centroid_travel < 0.0 {
            return Err(LayoutError::SlackOutOfRange);
        }
        Ok(Self {
            max_moved,
            max_centroid_travel,
        })
    }

    pub fn max_moved(self) -> u32 {
        self.max_moved
    }

    pub fn max_centroid_travel(self) -> f64 {
        self.max_centroid_travel
    }
}

/// Counts for one local repair or explicit repack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepairReport {
    preserved: u32,
    inserted: u32,
    removed: u32,
    moved: u32,
}

impl RepairReport {
    pub fn preserved(self) -> u32 {
        self.preserved
    }

    pub fn inserted(self) -> u32 {
        self.inserted
    }

    pub fn removed(self) -> u32 {
        self.removed
    }

    pub fn moved(self) -> u32 {
        self.moved
    }
}

/// Restorable history of committed layout generations for one owner/root.
#[derive(Clone, Debug, Default)]
pub struct LayoutArchive {
    generations: BTreeMap<u64, PartitionLayout>,
}

impl LayoutArchive {
    pub fn new() -> Self {
        Self {
            generations: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, layout: PartitionLayout) {
        self.generations.insert(layout.revision.get(), layout);
    }

    pub fn restore(&self, revision: LayoutRevision) -> Result<&PartitionLayout, LayoutError> {
        self.generations
            .get(&revision.get())
            .ok_or_else(|| LayoutError::from(fcb_core::CoreError::StaleLayoutRevision))
    }

    pub fn len(&self) -> usize {
        self.generations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.generations.is_empty()
    }
}

impl PartitionLayout {
    /// Absorb hierarchy edits without moving existing sibling parcels.
    ///
    /// Insertions consume parent slack. If slack cannot hold them, the repair
    /// is refused and the original generation is unchanged. Existing-neighborhood
    /// motion beyond `budget` is [`LayoutError::MovementDeferred`].
    pub fn local_repair(
        &self,
        next_revision: LayoutRevision,
        spec: &HierarchySpec,
        budget: DisplacementBudget,
    ) -> Result<(PartitionLayout, RepairReport), LayoutError> {
        next_revision.validate_for(self.owner)?;
        spec.root.validate_for(self.owner)?;
        if spec.root != self.root {
            return Err(LayoutError::from(fcb_core::CoreError::OwnershipMismatch));
        }
        if next_revision == self.revision {
            return Err(LayoutError::from(fcb_core::CoreError::StaleLayoutRevision));
        }

        let new_tree = build_tree(spec)?;
        let old_by_path: BTreeMap<Vec<u8>, &LaidOutNode> =
            self.nodes.iter().map(|node| (node.path.clone(), node)).collect();

        let mut out = Vec::new();
        let mut inserted = 0u32;
        let mut preserved = 0u32;
        repair_existing(self, &new_tree, &old_by_path, &mut out, &mut inserted, &mut preserved)?;

        let new_paths: std::collections::BTreeSet<Vec<u8>> =
            out.iter().map(|node| node.path.clone()).collect();
        let removed = self
            .nodes
            .iter()
            .filter(|node| !new_paths.contains(&node.path))
            .count() as u32;

        let mut moved = 0u32;
        let mut max_travel = 0.0_f64;
        for node in &out {
            if let Some(old) = old_by_path.get(&node.path) {
                let travel = centroid_travel(old.parent_local, node.parent_local);
                if travel > 0.0 {
                    moved = moved.saturating_add(1);
                    max_travel = max_travel.max(travel);
                }
            }
        }
        if moved > budget.max_moved || max_travel > budget.max_centroid_travel {
            return Err(LayoutError::MovementDeferred);
        }

        let repaired = PartitionLayout {
            owner: self.owner,
            root: self.root,
            revision: next_revision,
            options: self.options,
            world: self.world,
            nodes: out,
        };
        Ok((
            repaired,
            RepairReport {
                preserved,
                inserted,
                removed,
                moved,
            },
        ))
    }

    /// Global recompute. The previous generation is not mutated; archive it to restore.
    pub fn repack(
        &self,
        next_revision: LayoutRevision,
        spec: &HierarchySpec,
    ) -> Result<PartitionLayout, LayoutError> {
        next_revision.validate_for(self.owner)?;
        if next_revision == self.revision {
            return Err(LayoutError::from(fcb_core::CoreError::StaleLayoutRevision));
        }
        commit_layout(next_revision, self.world.size(), spec, self.options)
    }
}

fn centroid_travel(left: Rect2D, right: Rect2D) -> f64 {
    let lx = left.min_x() + left.size().width() / 2.0;
    let ly = left.min_y() + left.size().height() / 2.0;
    let rx = right.min_x() + right.size().width() / 2.0;
    let ry = right.min_y() + right.size().height() / 2.0;
    let dx = lx - rx;
    let dy = ly - ry;
    (dx * dx + dy * dy).sqrt()
}

fn repair_existing(
    base: &PartitionLayout,
    tree: &TreeNode,
    old_by_path: &BTreeMap<Vec<u8>, &LaidOutNode>,
    out: &mut Vec<LaidOutNode>,
    inserted: &mut u32,
    preserved: &mut u32,
) -> Result<(), LayoutError> {
    let old = old_by_path
        .get(&tree.path)
        .ok_or(LayoutError::RepairExceedsBudget)?;
    let mut slack = old.slack;
    *preserved = preserved.saturating_add(1);

    let mut kept_children: Vec<&TreeNode> = Vec::new();
    let mut new_children: Vec<&TreeNode> = Vec::new();
    for child in tree.children.values() {
        if old_by_path.contains_key(&child.path) {
            kept_children.push(child);
        } else {
            new_children.push(child);
        }
    }

    if !new_children.is_empty() {
        let slack_rect = slack.ok_or(LayoutError::RepairExceedsBudget)?;
        if slack_rect.size().is_empty() {
            return Err(LayoutError::RepairExceedsBudget);
        }
        let items: Vec<(Vec<u8>, f64)> = new_children
            .iter()
            .map(|child| (child.path.clone(), child.weight(base.options.metric())))
            .collect();
        let packed = pack_ordered(slack_rect, &items)?;
        if packed.iter().any(|(_, rect)| rect.size().is_empty()) {
            return Err(LayoutError::RepairExceedsBudget);
        }
        let mut leftover = slack_rect;
        for (_, rect) in &packed {
            leftover = subtract_packed_prefix(leftover, *rect)?;
        }
        slack = if leftover.size().is_empty() {
            None
        } else {
            Some(leftover)
        };

        let packed_map: BTreeMap<Vec<u8>, Rect2D> = packed.into_iter().collect();
        out.push(LaidOutNode {
            path: tree.path.clone(),
            kind: tree.kind,
            parent_local: old.parent_local,
            weight: tree.weight(base.options.metric()),
            slack,
        });
        for child in kept_children {
            repair_existing(base, child, old_by_path, out, inserted, preserved)?;
        }
        for child in new_children {
            let cell = *packed_map.get(&child.path).expect("new child packed");
            emit_new_subtree(base, child, cell, out, inserted)?;
        }
        return Ok(());
    }

    out.push(LaidOutNode {
        path: tree.path.clone(),
        kind: tree.kind,
        parent_local: old.parent_local,
        weight: tree.weight(base.options.metric()),
        slack,
    });
    for child in kept_children {
        repair_existing(base, child, old_by_path, out, inserted, preserved)?;
    }
    Ok(())
}

fn subtract_packed_prefix(slack: Rect2D, taken: Rect2D) -> Result<Rect2D, LayoutError> {
    // Ordered packing carves a strip from the left or top of slack.
    if (taken.min_x() - slack.min_x()).abs() <= 1e-9
        && (taken.min_y() - slack.min_y()).abs() <= 1e-9
        && (taken.size().height() - slack.size().height()).abs() <= 1e-9
    {
        let width = (slack.size().width() - taken.size().width()).max(0.0);
        return Rect2D::from_xywh(taken.max_x(), slack.min_y(), width, slack.size().height())
            .map_err(LayoutError::from);
    }
    if (taken.min_x() - slack.min_x()).abs() <= 1e-9
        && (taken.min_y() - slack.min_y()).abs() <= 1e-9
        && (taken.size().width() - slack.size().width()).abs() <= 1e-9
    {
        let height = (slack.size().height() - taken.size().height()).max(0.0);
        return Rect2D::from_xywh(slack.min_x(), taken.max_y(), slack.size().width(), height)
            .map_err(LayoutError::from);
    }
    // Non-strip leftover: keep the remaining slack bounding box minus a conservative
    // empty result when the taken cell consumed the slack.
    if taken.size().area() + 1e-9 >= slack.size().area() {
        Rect2D::from_xywh(slack.max_x(), slack.max_y(), 0.0, 0.0).map_err(LayoutError::from)
    } else {
        Ok(slack)
    }
}

fn emit_new_subtree(
    base: &PartitionLayout,
    tree: &TreeNode,
    cell_in_parent: Rect2D,
    out: &mut Vec<LaidOutNode>,
    inserted: &mut u32,
) -> Result<(), LayoutError> {
    let local_world = Rect2D::from_xywh(
        0.0,
        0.0,
        cell_in_parent.size().width(),
        cell_in_parent.size().height(),
    )?;
    let mut tmp = Vec::new();
    emit_tree(tree, local_world, local_world, base.options, &mut tmp)?;
    if let Some(root_emitted) = tmp.first_mut() {
        root_emitted.parent_local = cell_in_parent;
    }
    *inserted = inserted.saturating_add(tmp.len() as u32);
    out.extend(tmp);
    Ok(())
}

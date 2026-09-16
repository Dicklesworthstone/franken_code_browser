#![forbid(unsafe_code)]

//! Bounded atlas display plans over the retained hierarchy and sibling index.
//!
//! Frontier admission reserves one output/work slot for every pending region.
//! Refinement is allowed only when all resulting regions still fit. Exhaustion
//! therefore produces labelled aggregates, not silently omitted visible files.
//! Work is measured in visited hierarchy/group nodes; each step is bounded.
//!
//! This implements rectangle/aggregate LOD, not source textures, glyphs, native
//! rendering or label shaping. An aggregate is never presented as an exact file.

use std::mem::size_of;
use fcb_core::{ByteLength, CameraGeneration, DisplayGeneration, DisplayMetrics,
    Point2D, PresentedFrameId, QueryGeneration, Rect2D, ResourceAllocationId,
    ResourceBudget, ResourceLease};
use crate::atlas::{AtlasError, AtlasIndex, AtlasNodeId, BranchKind, reserved_vec};
use crate::camera::{Camera2D, CameraError, checked_rect};
use crate::NodeKind;

pub const MAX_VISIBLE_ITEMS: usize = 65_536;
pub const MAX_VISIBLE_VISITS: usize = 1_000_000;
pub const MAX_VISIBLE_STEP_VISITS: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VisibleLimits {
    pub max_items: usize,
    pub max_visits: usize,
}
impl Default for VisibleLimits {
    fn default() -> Self { Self { max_items: 1024, max_visits: 32_768 } }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodThresholds {
    expand_pixels: f64,
    collapse_pixels: f64,
}
impl LodThresholds {
    pub fn new(expand_pixels: f64, collapse_pixels: f64) -> Result<Self, AtlasError> {
        if !expand_pixels.is_finite() || !collapse_pixels.is_finite()
            || collapse_pixels < 0.0 || expand_pixels <= collapse_pixels {
            return Err(AtlasError::InvalidLimits);
        }
        Ok(Self { expand_pixels, collapse_pixels })
    }
    pub const fn expand_pixels(self) -> f64 { self.expand_pixels }
    pub const fn collapse_pixels(self) -> f64 { self.collapse_pixels }
}
impl Default for LodThresholds {
    fn default() -> Self { Self { expand_pixels: 48.0, collapse_pixels: 32.0 } }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasDetail { File, Placeholder, Directory, SiblingGroup }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AggregateReason { Leaf, Distance, ItemLimit, WorkLimit, Precision }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisibleParcel {
    node: AtlasNodeId,
    detail: AtlasDetail,
    logical_rect: Rect2D,
    represented_leaves: usize,
    reason: AggregateReason,
}
impl VisibleParcel {
    pub const fn node(self) -> AtlasNodeId { self.node }
    pub const fn detail(self) -> AtlasDetail { self.detail }
    pub const fn logical_rect(self) -> Rect2D { self.logical_rect }
    /// Hierarchy leaves, not a claim of readable files, exact source lines or matches.
    pub const fn represented_leaves(self) -> usize { self.represented_leaves }
    pub const fn reason(self) -> AggregateReason { self.reason }
    pub const fn is_source_parcel(self) -> bool {
        matches!(self.detail, AtlasDetail::File | AtlasDetail::Placeholder)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VisibleStats {
    pub visited_nodes: usize,
    pub culled_regions: usize,
    pub refined_regions: usize,
    pub emitted_items: usize,
    pub distance_aggregates: usize,
    pub budget_aggregates: usize,
    pub precision_aggregates: usize,
    pub peak_frontier: usize,
    pub last_step_visits: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisibleState { Pending, Complete, Canceled }

#[derive(Clone, Copy, Debug)]
enum Target { Node(usize), Group(usize) }
#[derive(Clone, Copy, Debug)]
struct Visit {
    target: Target,
    /// Parent origin in focus-local coordinates, shared by sibling groups.
    parent_origin: Point2D,
    bounds: Rect2D,
}

pub struct VisibleQuery<'index, 'layout, 'previous> {
    index: &'index AtlasIndex<'layout>,
    focus: AtlasNodeId,
    camera: Camera2D,
    generation: QueryGeneration,
    thresholds: LodThresholds,
    limits: VisibleLimits,
    previous_expanded: &'previous [usize],
    pending: Vec<Visit>,
    items: Vec<VisibleParcel>,
    expanded: Vec<usize>,
    stats: VisibleStats,
    canceled: bool,
    lease: ResourceLease,
}
impl<'index, 'layout, 'previous> VisibleQuery<'index, 'layout, 'previous> {
    /// The previous plan is read only and supplies bounded hysteresis state.
    /// Changing layout, focus or thresholds discards it instead of reusing slots
    /// from a different hierarchy. New output holds a separate resource lease.
    pub fn new(index: &'index AtlasIndex<'layout>, focus: AtlasNodeId, camera: Camera2D,
        generation: QueryGeneration, thresholds: LodThresholds, limits: VisibleLimits,
        previous: Option<&'previous VisiblePlan>, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<Self, AtlasError> {
        let focus_index = index.resolve(focus)?;
        if camera.generation().owner() != index.owner() || generation.owner() != index.owner() {
            return Err(AtlasError::OwnerMismatch);
        }
        if !(1..=MAX_VISIBLE_ITEMS).contains(&limits.max_items)
            || !(1..=MAX_VISIBLE_VISITS).contains(&limits.max_visits) {
            return Err(AtlasError::InvalidLimits);
        }
        let previous_expanded = match previous {
            Some(plan) if plan.focus == focus && plan.thresholds == thresholds => plan.expanded.as_slice(),
            _ => &[],
        };
        let charge = limits.max_items.checked_mul(size_of::<Visit>() + size_of::<VisibleParcel>())
            .and_then(|n| limits.max_visits.checked_mul(size_of::<usize>()).and_then(|e| n.checked_add(e)))
            .and_then(|n| n.checked_add(size_of::<Self>() + size_of::<VisiblePlan>()))
            .ok_or(AtlasError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(index.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AtlasError::ResourceDenied)?;
        let mut pending = reserved_vec(limits.max_items)?;
        pending.push(Visit { target: Target::Node(focus_index), parent_origin: Point2D::ORIGIN,
            bounds: index.bounds_in(focus, focus)? });
        Ok(Self { index, focus, camera, generation, thresholds, limits, previous_expanded,
            pending, items: reserved_vec(limits.max_items)?, expanded: reserved_vec(limits.max_visits)?,
            stats: VisibleStats { peak_frontier: 1, ..VisibleStats::default() }, canceled: false, lease })
    }
    pub fn state(&self) -> VisibleState {
        if self.canceled { VisibleState::Canceled }
        else if self.pending.is_empty() { VisibleState::Complete }
        else { VisibleState::Pending }
    }
    pub const fn stats(&self) -> VisibleStats { self.stats }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub fn cancel(&mut self) { self.canceled = true; }

    /// Each inspected region spends one pre-admitted visit. No hidden leaf,
    /// filename or source scan occurs after a region has been culled/aggregated.
    pub fn step(&mut self, max_visits: usize, active_generation: QueryGeneration,
        mut canceled: impl FnMut() -> bool) -> Result<VisibleState, AtlasError> {
        self.stats.last_step_visits = 0;
        if active_generation != self.generation { self.cancel(); return Err(AtlasError::StaleQuery); }
        if canceled() { self.cancel(); return Err(AtlasError::Canceled); }
        if self.canceled { return Ok(VisibleState::Canceled); }
        let allowance = max_visits.min(MAX_VISIBLE_STEP_VISITS);
        while self.stats.last_step_visits < allowance {
            if canceled() { self.cancel(); return Err(AtlasError::Canceled); }
            let Some(mut visit) = self.pending.pop() else { break; };
            self.stats.visited_nodes += 1;
            self.stats.last_step_visits += 1;
            // A sibling-tree leaf is the actual child, not another display item.
            if let Target::Group(branch) = visit.target {
                if let BranchKind::Leaf(node) = self.index.branches[branch].kind { visit.target = Target::Node(node); }
            }
            let Some(screen) = self.camera.project_clipped(visit.bounds)? else {
                self.stats.culled_regions += 1;
                continue;
            };
            let (key, child_count) = match visit.target {
                Target::Node(node) => (node * 2, usize::from(self.index.nodes[node].acceleration.is_some())),
                Target::Group(branch) => (branch * 2 + 1, 2),
            };
            if child_count == 0 {
                self.emit(visit, screen, AggregateReason::Leaf);
                continue;
            }
            let was_expanded = self.previous_expanded.binary_search(&key).is_ok();
            let threshold = if was_expanded { self.thresholds.collapse_pixels } else { self.thresholds.expand_pixels };
            let extent_pixels = visit.bounds.size().width().min(visit.bounds.size().height())
                * self.camera.points_per_unit() * self.camera.display().scale_factor();
            if extent_pixels < threshold {
                self.emit(visit, screen, AggregateReason::Distance);
                continue;
            }
            // Invariant: every pending visit can still emit one coarse parcel.
            if self.items.len() + self.pending.len() + child_count > self.limits.max_items {
                self.emit(visit, screen, AggregateReason::ItemLimit);
                continue;
            }
            if self.stats.visited_nodes + self.pending.len() + child_count > self.limits.max_visits {
                self.emit(visit, screen, AggregateReason::WorkLimit);
                continue;
            }
            let children = self.refine(visit);
            let (left, right) = match children {
                Ok(children) => children,
                Err(AtlasError::Camera(CameraError::PrecisionLost | CameraError::InvalidGeometry)) => {
                    self.emit(visit, screen, AggregateReason::Precision);
                    continue;
                }
                Err(error) => { self.cancel(); return Err(error); }
            };
            if let Some(right) = right { self.pending.push(right); }
            self.pending.push(left);
            self.expanded.push(key);
            self.stats.refined_regions += 1;
            self.stats.peak_frontier = self.stats.peak_frontier.max(self.pending.len());
        }
        if canceled() { self.cancel(); return Err(AtlasError::Canceled); }
        Ok(self.state())
    }

    /// Consume completed worker output. A canceled or partial query cannot
    /// create an apparently complete/presentable plan. No source data is copied.
    pub fn finish(mut self) -> Result<VisiblePlan, AtlasError> {
        match self.state() {
            VisibleState::Canceled => return Err(AtlasError::Canceled),
            VisibleState::Pending => return Err(AtlasError::NotComplete),
            VisibleState::Complete => {}
        }
        self.expanded.sort_unstable();
        Ok(VisiblePlan { focus: self.focus, camera: self.camera, generation: self.generation,
            thresholds: self.thresholds, items: self.items, expanded: self.expanded,
            stats: self.stats, _lease: self.lease })
    }

    fn refine(&self, visit: Visit) -> Result<(Visit, Option<Visit>), AtlasError> {
        match visit.target {
            Target::Node(node) => {
                let branch = self.index.nodes[node].acceleration.ok_or(AtlasError::InvalidHierarchy)?;
                let origin = visit.bounds.origin();
                Ok((self.group_visit(branch, origin)?, None))
            }
            Target::Group(branch) => match self.index.branches[branch].kind {
                BranchKind::Fork(left, right) => Ok((self.group_visit(left, visit.parent_origin)?,
                    Some(self.group_visit(right, visit.parent_origin)?))),
                BranchKind::Leaf(_) => Err(AtlasError::InvalidHierarchy),
            },
        }
    }
    fn group_visit(&self, branch: usize, origin: Point2D) -> Result<Visit, AtlasError> {
        let local = self.index.branches[branch].bounds;
        let bounds = Rect2D::from_xywh(origin.x() + local.min_x(), origin.y() + local.min_y(),
            local.size().width(), local.size().height())
            .map_err(|_| AtlasError::Camera(CameraError::InvalidGeometry))?;
        checked_rect(bounds)?;
        Ok(Visit { target: Target::Group(branch), parent_origin: origin, bounds })
    }
    fn emit(&mut self, visit: Visit, screen: Rect2D, reason: AggregateReason) {
        let (node, detail, leaves) = match visit.target {
            Target::Node(index) => {
                let detail = match self.index.layout.nodes()[index].kind() {
                    NodeKind::File => AtlasDetail::File,
                    NodeKind::Placeholder => AtlasDetail::Placeholder,
                    NodeKind::Directory => AtlasDetail::Directory,
                };
                (index, detail, self.index.nodes[index].leaves)
            }
            Target::Group(index) => {
                let branch = self.index.branches[index];
                (branch.parent, AtlasDetail::SiblingGroup, branch.leaves)
            }
        };
        self.items.push(VisibleParcel { node: self.index.key(node), detail, logical_rect: screen,
            represented_leaves: leaves, reason });
        self.stats.emitted_items += 1;
        match reason {
            AggregateReason::Distance => self.stats.distance_aggregates += 1,
            AggregateReason::ItemLimit | AggregateReason::WorkLimit => self.stats.budget_aggregates += 1,
            AggregateReason::Precision => self.stats.precision_aggregates += 1,
            AggregateReason::Leaf => {}
        }
    }
}

pub struct VisiblePlan {
    focus: AtlasNodeId,
    camera: Camera2D,
    generation: QueryGeneration,
    thresholds: LodThresholds,
    items: Vec<VisibleParcel>,
    expanded: Vec<usize>,
    stats: VisibleStats,
    _lease: ResourceLease,
}
impl VisiblePlan {
    pub const fn focus(&self) -> AtlasNodeId { self.focus }
    pub const fn camera(&self) -> Camera2D { self.camera }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub fn parcels(&self) -> &[VisibleParcel] { &self.items }
    pub const fn stats(&self) -> VisibleStats { self.stats }
    pub fn validate_delivery(&self, focus: AtlasNodeId, camera: Camera2D, generation: QueryGeneration) -> Result<(), AtlasError> {
        if self.focus != focus { return Err(AtlasError::StaleLayout); }
        if self.camera != camera || self.generation != generation { return Err(AtlasError::StaleQuery); }
        Ok(())
    }

    /// Explicit host acknowledgment, NOT observation of an OS/GPU presentation.
    /// The host must choose the conservatively known presented plan using its
    /// timing policy. Merely finishing/submitting a newer plan does not replace
    /// this immutable interaction snapshot. The borrow retains its geometry.
    pub fn acknowledge_presented(&self, frame: PresentedFrameId, actual_display: DisplayMetrics)
        -> Result<PresentedAtlas<'_>, AtlasError> {
        if frame.owner() != self.focus.root().owner() { return Err(AtlasError::OwnerMismatch); }
        if actual_display != self.camera.display() { return Err(AtlasError::DisplayMismatch); }
        Ok(PresentedAtlas { plan: self, frame })
    }
}

#[derive(Clone, Copy)]
pub struct PresentedAtlas<'plan> {
    plan: &'plan VisiblePlan,
    frame: PresentedFrameId,
}
impl<'plan> PresentedAtlas<'plan> {
    pub const fn frame(self) -> PresentedFrameId { self.frame }
    pub const fn plan(self) -> &'plan VisiblePlan { self.plan }

    /// Tests the same clipped rectangles offered to drawing/accessibility.
    /// Shared edges use half-open rectangles. Exact source parcels outrank group
    /// boxes; remaining overlap chooses the smaller box, then retained order.
    /// A group hit targets its parent directory, NEVER an unseen child file.
    pub fn hit_test(self, point: Point2D, display: DisplayGeneration) -> Result<Option<AtlasHit>, AtlasError> {
        if display != self.plan.camera.display().generation() { return Err(AtlasError::DisplayMismatch); }
        let mut best: Option<usize> = None;
        for (index, parcel) in self.plan.items.iter().enumerate() {
            let rect = parcel.logical_rect;
            if point.x() < rect.min_x() || point.x() >= rect.max_x()
                || point.y() < rect.min_y() || point.y() >= rect.max_y() { continue; }
            let replace = best.is_none_or(|previous| {
                let previous = self.plan.items[previous];
                (parcel.is_source_parcel() && !previous.is_source_parcel())
                    || (parcel.is_source_parcel() == previous.is_source_parcel()
                        && rect.size().area() < previous.logical_rect.size().area())
            });
            if replace { best = Some(index); }
        }
        Ok(best.map(|parcel| AtlasHit { node: self.plan.items[parcel].node, detail: self.plan.items[parcel].detail,
            frame: self.frame, generation: self.plan.generation, camera: self.plan.camera.generation(),
            display, parcel }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasHit {
    node: AtlasNodeId,
    detail: AtlasDetail,
    frame: PresentedFrameId,
    generation: QueryGeneration,
    camera: CameraGeneration,
    display: DisplayGeneration,
    parcel: usize,
}
impl AtlasHit {
    pub const fn node(self) -> AtlasNodeId { self.node }
    pub const fn detail(self) -> AtlasDetail { self.detail }
    pub const fn frame(self) -> PresentedFrameId { self.frame }
    pub fn validate(self, presented: PresentedAtlas<'_>) -> Result<(), AtlasError> {
        let plan = presented.plan;
        if self.frame != presented.frame || self.generation != plan.generation
            || self.camera != plan.camera.generation() || self.display != plan.camera.display().generation() {
            return Err(AtlasError::FrameMismatch);
        }
        let parcel = plan.items.get(self.parcel).ok_or(AtlasError::FrameMismatch)?;
        if parcel.node != self.node || parcel.detail != self.detail { return Err(AtlasError::FrameMismatch); }
        Ok(())
    }
}

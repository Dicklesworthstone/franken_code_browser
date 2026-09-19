#![forbid(unsafe_code)]

//! Deterministic retained partition-tree layout (FCB-013.A / fcb-d6x.1).
//!
//! The repository hierarchy is stored separately from its layout. This crate
//! assigns parent-local rectangles from a frozen child order (raw path bytes,
//! never size-sorted squarify), bounded sublinear weights, unknown-but-present
//! placeholders, and reserved slack. A committed [`LayoutRevision`] restores
//! identically. Local neighborhood repair and explicit global repack are in
//! [`repair`].

mod repair;
pub mod atlas;
pub mod camera;
pub mod labels;
pub mod navigation;
pub mod text_columns;
pub mod text_parcels;
pub mod visible;

pub use atlas::{AtlasBuildLimits, AtlasError, AtlasIndex, AtlasNodeId};
pub use camera::{Camera2D, CameraError};
pub use labels::{
    candidates_from_visible_parcels, place_labels, CollisionGrid, LabelCandidate, LabelContext,
    LabelLimits, LabelMeasureCache, LabelPlan, LabelStats, PlacedLabel, RetainedLabelSet,
    DEFAULT_MAX_LABELS, MAX_LABELS,
};
pub use navigation::{
    HistoryEntry, MotionPreference, NavigationError, NavigationFlight, NavigationFlightConfig,
    NavigationHistory, NavigationReason, DEFAULT_FLIGHT_DURATION_NANOS,
    DEFAULT_MAX_HISTORY_CAPACITY, DEFAULT_MAX_STEP_DELTA_NANOS,
};
pub use visible::{AggregateReason, AtlasDetail, AtlasHit, LodThresholds, PresentedAtlas,
    VisibleLimits, VisibleParcel, VisiblePlan, VisibleQuery, VisibleState, VisibleStats};

use std::collections::BTreeMap;

use fcb_core::{ArenaOwnerId, CoreError, Point2D, Rect2D, RootId};

pub use fcb_core::{LayoutRevision, Size2D};

pub use repair::{DisplacementBudget, LayoutArchive, RepairReport};

/// Named size metric. The legend must name this; it is not raw byte area.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WeightMetric {
    /// `ln(1 + bytes)` with a floor for empty/unknown and a cap for huge files.
    CappedLogBytes,
}

impl WeightMetric {
    pub const fn name(self) -> &'static str {
        match self {
            Self::CappedLogBytes => "capped-log-bytes",
        }
    }
}

/// Kind of one hierarchy node supplied to layout.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NodeKind {
    Directory,
    File,
    /// Present but unreadable or otherwise unknown. Not zero weight.
    Placeholder,
}

/// One input node. Paths are root-relative `/`-separated raw bytes; empty is root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeSpec {
    path: Vec<u8>,
    kind: NodeKind,
    observed_bytes: Option<u64>,
}

impl NodeSpec {
    pub fn new(path: impl Into<Vec<u8>>, kind: NodeKind, observed_bytes: Option<u64>) -> Self {
        Self {
            path: path.into(),
            kind,
            observed_bytes,
        }
    }

    pub fn path(&self) -> &[u8] {
        &self.path
    }

    pub fn kind(&self) -> NodeKind {
        self.kind
    }

    pub fn observed_bytes(&self) -> Option<u64> {
        self.observed_bytes
    }
}

/// Explicit hierarchy snapshot. Layout never walks a filesystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HierarchySpec {
    owner: ArenaOwnerId,
    root: RootId,
    nodes: Vec<NodeSpec>,
}

impl HierarchySpec {
    pub fn new(owner: ArenaOwnerId, root: RootId, nodes: Vec<NodeSpec>) -> Result<Self, LayoutError> {
        root.validate_for(owner).map_err(LayoutError::from)?;
        Ok(Self {
            owner,
            root,
            nodes,
        })
    }

    pub fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn root(&self) -> RootId {
        self.root
    }

    pub fn nodes(&self) -> &[NodeSpec] {
        &self.nodes
    }
}

/// Packing options frozen into a committed generation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutOptions {
    metric: WeightMetric,
    slack_fraction: f64,
}

impl LayoutOptions {
    pub fn new(metric: WeightMetric, slack_fraction: f64) -> Result<Self, LayoutError> {
        if !slack_fraction.is_finite() || slack_fraction < 0.0 || slack_fraction >= 1.0 {
            return Err(LayoutError::SlackOutOfRange);
        }
        Ok(Self {
            metric,
            slack_fraction,
        })
    }

    pub fn modest() -> Self {
        Self {
            metric: WeightMetric::CappedLogBytes,
            slack_fraction: 0.125,
        }
    }

    pub fn metric(self) -> WeightMetric {
        self.metric
    }

    pub fn slack_fraction(self) -> f64 {
        self.slack_fraction
    }
}

/// Parent-local laid-out node.
#[derive(Clone, Debug, PartialEq)]
pub struct LaidOutNode {
    pub(crate) path: Vec<u8>,
    pub(crate) kind: NodeKind,
    pub(crate) parent_local: Rect2D,
    pub(crate) weight: f64,
    pub(crate) slack: Option<Rect2D>,
}

impl LaidOutNode {
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    pub fn kind(&self) -> NodeKind {
        self.kind
    }

    pub fn parent_local(&self) -> Rect2D {
        self.parent_local
    }

    pub fn weight(&self) -> f64 {
        self.weight
    }

    pub fn slack(&self) -> Option<Rect2D> {
        self.slack
    }
}

/// A committed partition tree. Restoring this revision yields the same rectangles.
#[derive(Clone, Debug, PartialEq)]
pub struct PartitionLayout {
    pub(crate) owner: ArenaOwnerId,
    pub(crate) root: RootId,
    pub(crate) revision: LayoutRevision,
    pub(crate) options: LayoutOptions,
    pub(crate) world: Rect2D,
    pub(crate) nodes: Vec<LaidOutNode>,
}

impl PartitionLayout {
    pub fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn root(&self) -> RootId {
        self.root
    }

    pub fn revision(&self) -> LayoutRevision {
        self.revision
    }

    pub fn options(&self) -> LayoutOptions {
        self.options
    }

    pub fn world(&self) -> Rect2D {
        self.world
    }

    pub fn nodes(&self) -> &[LaidOutNode] {
        &self.nodes
    }

    pub fn node(&self, path: &[u8]) -> Option<&LaidOutNode> {
        self.nodes.iter().find(|node| node.path == path)
    }

    /// Re-emit the committed rectangles. A different revision is refused.
    pub fn restore(&self, revision: LayoutRevision) -> Result<&Self, LayoutError> {
        if revision != self.revision {
            return Err(LayoutError::from(CoreError::StaleLayoutRevision));
        }
        Ok(self)
    }
}

/// Layout-specific refusals.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LayoutError {
    Core(CoreError),
    DuplicatePath,
    InvalidPath,
    SlackOutOfRange,
    /// Slack cannot absorb an insertion without moving neighbors.
    RepairExceedsBudget,
    /// Existing parcels would move; interaction must not commit that.
    MovementDeferred,
}

impl LayoutError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Core(err) => err.code(),
            Self::DuplicatePath => "LAYOUT_DUPLICATE_PATH",
            Self::InvalidPath => "LAYOUT_INVALID_PATH",
            Self::SlackOutOfRange => "LAYOUT_SLACK_OUT_OF_RANGE",
            Self::RepairExceedsBudget => "LAYOUT_REPAIR_EXCEEDS_BUDGET",
            Self::MovementDeferred => "LAYOUT_MOVEMENT_DEFERRED",
        }
    }
}

impl From<CoreError> for LayoutError {
    fn from(err: CoreError) -> Self {
        Self::Core(err)
    }
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for LayoutError {}

/// Floor applied to empty known files so they still occupy a parcel.
const MIN_KNOWN_BYTES: u64 = 1;
/// Floor applied when presence is known but size is not.
const PLACEHOLDER_BYTES: u64 = 4096;
/// Cap so a giant generated file cannot consume the atlas.
const MAX_WEIGHT_BYTES: u64 = 1_048_576;

pub fn bounded_weight(metric: WeightMetric, kind: NodeKind, observed_bytes: Option<u64>) -> f64 {
    match metric {
        WeightMetric::CappedLogBytes => {
            let bytes = match (kind, observed_bytes) {
                (NodeKind::Placeholder, _) | (_, None) => PLACEHOLDER_BYTES,
                (_, Some(0)) => MIN_KNOWN_BYTES,
                (_, Some(value)) => value.min(MAX_WEIGHT_BYTES),
            };
            (1.0 + bytes as f64).ln()
        }
    }
}

pub(crate) struct TreeNode {
    pub(crate) path: Vec<u8>,
    pub(crate) kind: NodeKind,
    pub(crate) observed_bytes: Option<u64>,
    pub(crate) children: BTreeMap<Vec<u8>, TreeNode>,
}

impl TreeNode {
    fn new(path: Vec<u8>, kind: NodeKind, observed_bytes: Option<u64>) -> Self {
        Self {
            path,
            kind,
            observed_bytes,
            children: BTreeMap::new(),
        }
    }

    pub(crate) fn weight(&self, metric: WeightMetric) -> f64 {
        if self.children.is_empty() {
            return bounded_weight(metric, self.kind, self.observed_bytes);
        }
        self.children
            .values()
            .map(|child| child.weight(metric))
            .sum()
    }
}

fn validate_path(path: &[u8]) -> Result<(), LayoutError> {
    if path.is_empty() {
        return Ok(());
    }
    if path.starts_with(b"/") || path.contains(&0) {
        return Err(LayoutError::InvalidPath);
    }
    for segment in path.split(|byte| *byte == b'/') {
        if segment.is_empty() || segment == b"." || segment == b".." {
            return Err(LayoutError::InvalidPath);
        }
    }
    Ok(())
}

fn parent_path(path: &[u8]) -> Option<&[u8]> {
    path.iter()
        .rposition(|byte| *byte == b'/')
        .map(|idx| &path[..idx])
}

fn ensure_dir<'a>(root: &'a mut TreeNode, path: &[u8]) -> Result<&'a mut TreeNode, LayoutError> {
    if path.is_empty() {
        return Ok(root);
    }
    let mut cursor = root;
    let mut assembled = Vec::new();
    for segment in path.split(|byte| *byte == b'/') {
        if !assembled.is_empty() {
            assembled.push(b'/');
        }
        assembled.extend_from_slice(segment);
        cursor = cursor
            .children
            .entry(segment.to_vec())
            .or_insert_with(|| TreeNode::new(assembled.clone(), NodeKind::Directory, None));
        if matches!(cursor.kind, NodeKind::File | NodeKind::Placeholder) {
            return Err(LayoutError::InvalidPath);
        }
        cursor.kind = NodeKind::Directory;
    }
    Ok(cursor)
}

pub(crate) fn build_tree(spec: &HierarchySpec) -> Result<TreeNode, LayoutError> {
    let mut root = TreeNode::new(Vec::new(), NodeKind::Directory, None);
    let mut seen: BTreeMap<Vec<u8>, NodeKind> = BTreeMap::new();
    seen.insert(Vec::new(), NodeKind::Directory);
    for node in &spec.nodes {
        validate_path(&node.path)?;
        if node.path.is_empty() {
            if seen.get(&node.path) != Some(&NodeKind::Directory) {
                return Err(LayoutError::DuplicatePath);
            }
            root.kind = node.kind;
            root.observed_bytes = node.observed_bytes;
            continue;
        }
        if seen.insert(node.path.clone(), node.kind).is_some() {
            return Err(LayoutError::DuplicatePath);
        }
        let parent = parent_path(&node.path).unwrap_or(b"");
        let parent_node = ensure_dir(&mut root, parent)?;
        let key = node.path.rsplit(|byte| *byte == b'/').next().unwrap().to_vec();
        match parent_node.children.get_mut(&key) {
            Some(existing) => {
                if node.kind == NodeKind::File && !existing.children.is_empty() {
                    return Err(LayoutError::InvalidPath);
                }
                existing.kind = node.kind;
                existing.observed_bytes = node.observed_bytes;
            }
            None => {
                parent_node.children.insert(
                    key,
                    TreeNode::new(node.path.clone(), node.kind, node.observed_bytes),
                );
            }
        }
    }
    Ok(root)
}

fn split_slack(rect: Rect2D, slack_fraction: f64) -> Result<(Rect2D, Option<Rect2D>), LayoutError> {
    if slack_fraction == 0.0 {
        return Ok((rect, None));
    }
    let width = rect.size().width();
    let height = rect.size().height();
    if width >= height {
        let slack_w = width * slack_fraction;
        let usable_w = width - slack_w;
        let usable = Rect2D::from_xywh(rect.min_x(), rect.min_y(), usable_w, height)?;
        let slack = Rect2D::from_xywh(rect.min_x() + usable_w, rect.min_y(), slack_w, height)?;
        Ok((usable, Some(slack)))
    } else {
        let slack_h = height * slack_fraction;
        let usable_h = height - slack_h;
        let usable = Rect2D::from_xywh(rect.min_x(), rect.min_y(), width, usable_h)?;
        let slack = Rect2D::from_xywh(rect.min_x(), rect.min_y() + usable_h, width, slack_h)?;
        Ok((usable, Some(slack)))
    }
}

fn worst_row_aspect(row: &[f64], remaining_weight: f64, remaining_area: f64, side: f64) -> f64 {
    let row_weight: f64 = row.iter().sum();
    if row_weight <= 0.0 || remaining_weight <= 0.0 || side <= 0.0 || remaining_area <= 0.0 {
        return f64::INFINITY;
    }
    let row_area = row_weight / remaining_weight * remaining_area;
    let thickness = row_area / side;
    if thickness <= 0.0 {
        return f64::INFINITY;
    }
    row.iter()
        .map(|weight| {
            let item_area = *weight / remaining_weight * remaining_area;
            let length = item_area / thickness;
            if length <= 0.0 {
                f64::INFINITY
            } else {
                (thickness / length).max(length / thickness)
            }
        })
        .fold(0.0_f64, f64::max)
}

fn take_row<'a>(items: &'a [(Vec<u8>, f64)], remaining_weight: f64, remaining_area: f64, side: f64) -> usize {
    let mut count = 1;
    let mut row = vec![items[0].1];
    let mut worst = worst_row_aspect(&row, remaining_weight, remaining_area, side);
    while count < items.len() {
        row.push(items[count].1);
        let next_worst = worst_row_aspect(&row, remaining_weight, remaining_area, side);
        if next_worst <= worst {
            worst = next_worst;
            count += 1;
        } else {
            break;
        }
    }
    count
}

fn pack_row(
    rect: Rect2D,
    row: &[(Vec<u8>, f64)],
    remaining_weight: f64,
) -> Result<(Vec<(Vec<u8>, Rect2D)>, Rect2D), LayoutError> {
    let row_weight: f64 = row.iter().map(|item| item.1).sum();
    let remaining_area = rect.size().area();
    let row_area = if remaining_weight <= 0.0 {
        remaining_area
    } else {
        row_weight / remaining_weight * remaining_area
    };
    let along_x = rect.size().width() >= rect.size().height();
    let mut placed = Vec::with_capacity(row.len());
    if along_x {
        let thickness = if rect.size().height() <= 0.0 {
            0.0
        } else {
            row_area / rect.size().height()
        };
        let mut y = rect.min_y();
        let mut leftover_h = rect.size().height();
        for (idx, (path, weight)) in row.iter().enumerate() {
            let is_last = idx + 1 == row.len();
            let h = if is_last {
                leftover_h
            } else if row_weight <= 0.0 {
                0.0
            } else {
                (*weight / row_weight) * rect.size().height()
            };
            let child = Rect2D::from_xywh(rect.min_x(), y, thickness, h)?;
            placed.push((path.clone(), child));
            y += h;
            leftover_h -= h;
        }
        let rest = Rect2D::from_xywh(
            rect.min_x() + thickness,
            rect.min_y(),
            (rect.size().width() - thickness).max(0.0),
            rect.size().height(),
        )?;
        Ok((placed, rest))
    } else {
        let thickness = if rect.size().width() <= 0.0 {
            0.0
        } else {
            row_area / rect.size().width()
        };
        let mut x = rect.min_x();
        let mut leftover_w = rect.size().width();
        for (idx, (path, weight)) in row.iter().enumerate() {
            let is_last = idx + 1 == row.len();
            let w = if is_last {
                leftover_w
            } else if row_weight <= 0.0 {
                0.0
            } else {
                (*weight / row_weight) * rect.size().width()
            };
            let child = Rect2D::from_xywh(x, rect.min_y(), w, thickness)?;
            placed.push((path.clone(), child));
            x += w;
            leftover_w -= w;
        }
        let rest = Rect2D::from_xywh(
            rect.min_x(),
            rect.min_y() + thickness,
            (rect.size().width()).max(0.0),
            (rect.size().height() - thickness).max(0.0),
        )?;
        Ok((placed, rest))
    }
}

pub(crate) fn pack_ordered(rect: Rect2D, items: &[(Vec<u8>, f64)]) -> Result<Vec<(Vec<u8>, Rect2D)>, LayoutError> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let mut remaining_rect = rect;
    let mut remaining = items;
    let mut remaining_weight: f64 = items.iter().map(|item| item.1).sum();
    let mut out = Vec::with_capacity(items.len());
    while !remaining.is_empty() {
        if remaining.len() == 1 {
            out.push((remaining[0].0.clone(), remaining_rect));
            break;
        }
        let side = remaining_rect.size().width().min(remaining_rect.size().height());
        let area = remaining_rect.size().area();
        let count = take_row(remaining, remaining_weight, area, side);
        let row = &remaining[..count];
        let (placed, rest) = pack_row(remaining_rect, row, remaining_weight)?;
        let used: f64 = row.iter().map(|item| item.1).sum();
        remaining_weight -= used;
        out.extend(placed);
        remaining = &remaining[count..];
        remaining_rect = rest;
    }
    Ok(out)
}

fn to_parent_local(parent_world: Rect2D, child_world: Rect2D) -> Result<Rect2D, LayoutError> {
    Rect2D::from_xywh(
        child_world.min_x() - parent_world.min_x(),
        child_world.min_y() - parent_world.min_y(),
        child_world.size().width(),
        child_world.size().height(),
    )
    .map_err(LayoutError::from)
}

pub(crate) fn emit_tree(
    tree: &TreeNode,
    world_rect: Rect2D,
    parent_world: Rect2D,
    options: LayoutOptions,
    out: &mut Vec<LaidOutNode>,
) -> Result<(), LayoutError> {
    let parent_local = to_parent_local(parent_world, world_rect)?;
    let (usable_world, slack_world) = if tree.children.is_empty() {
        (world_rect, None)
    } else {
        split_slack(world_rect, options.slack_fraction)?
    };
    let slack = match slack_world {
        Some(rect) => Some(to_parent_local(world_rect, rect)?),
        None => None,
    };
    out.push(LaidOutNode {
        path: tree.path.clone(),
        kind: tree.kind,
        parent_local,
        weight: tree.weight(options.metric),
        slack,
    });
    if tree.children.is_empty() {
        return Ok(());
    }
    let items: Vec<(Vec<u8>, f64)> = tree
        .children
        .values()
        .map(|child| (child.path.clone(), child.weight(options.metric)))
        .collect();
    let packed = pack_ordered(usable_world, &items)?;
    let mut by_path: BTreeMap<Vec<u8>, Rect2D> = BTreeMap::new();
    for (path, child_rect) in packed {
        by_path.insert(path, child_rect);
    }
    for child in tree.children.values() {
        let child_world = *by_path.get(&child.path).expect("every child was packed");
        emit_tree(child, child_world, world_rect, options, out)?;
    }
    Ok(())
}

/// Commit a retained partition tree. Child order is raw-path order, not size order.
pub fn commit_layout(
    revision: LayoutRevision,
    world: Size2D,
    spec: &HierarchySpec,
    options: LayoutOptions,
) -> Result<PartitionLayout, LayoutError> {
    revision.validate_for(spec.owner).map_err(LayoutError::from)?;
    if world.is_empty() {
        return Err(LayoutError::from(CoreError::InvalidGeometry));
    }
    let world_rect = Rect2D::new(Point2D::ORIGIN, world);
    let tree = build_tree(spec)?;
    let mut nodes = Vec::new();
    emit_tree(&tree, world_rect, world_rect, options, &mut nodes)?;
    Ok(PartitionLayout {
        owner: spec.owner,
        root: spec.root,
        revision,
        options,
        world: world_rect,
        nodes,
    })
}

/// True when two rectangles share interior area. Edge-adjacent packing is allowed.
pub fn interiors_overlap(left: Rect2D, right: Rect2D) -> bool {
    left.min_x() < right.max_x()
        && left.max_x() > right.min_x()
        && left.min_y() < right.max_y()
        && left.max_y() > right.min_y()
}

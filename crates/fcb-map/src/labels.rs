#![forbid(unsafe_code)]

//! Stable collision-limited label placement (FCB-014.B / fcb-e5o.2).
//!
//! Labels are ranked by selection, search relevance, navigation context,
//! hierarchy importance, and projected size. Accepted labels are packed into a
//! screen-space collision grid subject to a hard per-viewport budget.
//!
//! To prevent nondeterministic flicker and churn across frame updates, previously
//! accepted labels that remain visible and valid receive a retention boost.
//!
//! Invariant: The selected item always receives an accessible visible identity
//! even when ordinary labels are culled by collision or budget pressure.

use std::collections::BTreeMap;

use fcb_core::{Point2D, Rect2D, Size2D};

use crate::atlas::{AtlasError, AtlasNodeId};
use crate::camera::Camera2D;
use crate::visible::VisibleParcel;

pub const MAX_LABELS: usize = 1024;
pub const MAX_GRID_CELLS: usize = 16_384;
pub const DEFAULT_MAX_LABELS: usize = 64;

/// Configuration and hard budgets for label placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelLimits {
    pub max_labels: usize,
    pub min_parcel_extent_pixels: f64,
    pub label_padding_pixels: f64,
    pub cell_size_pixels: f64,
}

impl Default for LabelLimits {
    fn default() -> Self {
        Self {
            max_labels: DEFAULT_MAX_LABELS,
            min_parcel_extent_pixels: 16.0,
            label_padding_pixels: 4.0,
            cell_size_pixels: 32.0,
        }
    }
}

impl LabelLimits {
    pub fn new(
        max_labels: usize,
        min_parcel_extent_pixels: f64,
        label_padding_pixels: f64,
        cell_size_pixels: f64,
    ) -> Result<Self, AtlasError> {
        if !(1..=MAX_LABELS).contains(&max_labels)
            || !min_parcel_extent_pixels.is_finite()
            || min_parcel_extent_pixels < 0.0
            || !label_padding_pixels.is_finite()
            || label_padding_pixels < 0.0
            || !cell_size_pixels.is_finite()
            || cell_size_pixels <= 1.0
        {
            return Err(AtlasError::InvalidLimits);
        }
        Ok(Self {
            max_labels,
            min_parcel_extent_pixels,
            label_padding_pixels,
            cell_size_pixels,
        })
    }
}

/// Priority context for ranking label candidates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LabelContext {
    pub is_selected: bool,
    pub search_relevance: Option<u32>,
    pub is_navigation_context: bool,
}

/// Candidate for screen-space label placement.
#[derive(Clone, Debug, PartialEq)]
pub struct LabelCandidate {
    pub node: AtlasNodeId,
    pub text: String,
    pub logical_bounds: Rect2D,
    pub importance: u32,
    pub context: LabelContext,
    pub estimated_size: Size2D,
}

impl LabelCandidate {
    pub fn new(
        node: AtlasNodeId,
        text: String,
        logical_bounds: Rect2D,
        importance: u32,
        context: LabelContext,
        estimated_size: Size2D,
    ) -> Self {
        Self {
            node,
            text,
            logical_bounds,
            importance,
            context,
            estimated_size,
        }
    }

    /// Compute composite rank score. Higher score = higher placement priority.
    /// Selected items are guaranteed highest rank.
    pub fn rank_score(&self, was_previously_placed: bool) -> u64 {
        let mut score: u64 = 0;

        // 1. Selection: highest priority tier (bits 60..63)
        if self.context.is_selected {
            score |= 1 << 60;
        }

        // 2. Search relevance: next priority tier (bits 48..59)
        if let Some(rel) = self.context.search_relevance {
            let rel_score = (rel as u64).min(0x0FFF) << 48;
            score |= rel_score;
        }

        // 3. Navigation context: (bit 47)
        if self.context.is_navigation_context {
            score |= 1 << 47;
        }

        // 4. Stability / Hysteresis retention boost: (bit 46)
        if was_previously_placed {
            score |= 1 << 46;
        }

        // 5. Hierarchy importance: (bits 30..45)
        let imp = (self.importance as u64).min(0xFFFF) << 30;
        score |= imp;

        // 6. Projected area contribution (lower bits)
        let area = (self.logical_bounds.size().area().min(1_000_000.0) as u64) & 0x3FFF_FFFF;
        score |= area;

        score
    }
}

/// Placed label output with screen coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedLabel {
    pub node: AtlasNodeId,
    pub text: String,
    pub screen_rect: Rect2D,
    pub is_selected: bool,
    pub is_retained: bool,
}

/// Statistics emitted during label placement.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LabelStats {
    pub candidates_evaluated: usize,
    pub labels_placed: usize,
    pub rejected_extent_too_small: usize,
    pub rejected_collision: usize,
    pub rejected_budget: usize,
    pub retained_from_previous: usize,
    pub selected_preserved: bool,
}

/// Bounded screen-space 2D collision grid.
#[derive(Clone, Debug)]
pub struct CollisionGrid {
    viewport: Rect2D,
    cell_size: f64,
    cols: usize,
    rows: usize,
    cells: Vec<Vec<Rect2D>>,
}

impl CollisionGrid {
    pub fn new(viewport: Rect2D, cell_size: f64) -> Result<Self, AtlasError> {
        if !cell_size.is_finite() || cell_size <= 1.0 {
            return Err(AtlasError::InvalidLimits);
        }
        let w = viewport.size().width();
        let h = viewport.size().height();
        if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
            return Err(AtlasError::InvalidLimits);
        }

        let cols = ((w / cell_size).ceil() as usize).max(1);
        let rows = ((h / cell_size).ceil() as usize).max(1);
        let total_cells = cols.checked_mul(rows).ok_or(AtlasError::InvalidLimits)?;
        if total_cells > MAX_GRID_CELLS {
            return Err(AtlasError::InvalidLimits);
        }

        let mut cells = Vec::with_capacity(total_cells);
        for _ in 0..total_cells {
            cells.push(Vec::new());
        }

        Ok(Self {
            viewport,
            cell_size,
            cols,
            rows,
            cells,
        })
    }

    fn cell_range(&self, rect: Rect2D) -> (usize, usize, usize, usize) {
        let min_x = (rect.min_x() - self.viewport.min_x()).max(0.0);
        let min_y = (rect.min_y() - self.viewport.min_y()).max(0.0);
        let max_x = (rect.max_x() - self.viewport.min_x()).max(0.0);
        let max_y = (rect.max_y() - self.viewport.min_y()).max(0.0);

        let col_start = (min_x / self.cell_size).floor() as usize;
        let row_start = (min_y / self.cell_size).floor() as usize;
        let col_end = ((max_x / self.cell_size).floor() as usize).min(self.cols.saturating_sub(1));
        let row_end = ((max_y / self.cell_size).floor() as usize).min(self.rows.saturating_sub(1));

        (
            col_start.min(self.cols.saturating_sub(1)),
            col_end,
            row_start.min(self.rows.saturating_sub(1)),
            row_end,
        )
    }

    /// Check if `rect` collides with any previously inserted rectangles.
    pub fn collides(&self, rect: Rect2D) -> bool {
        let (c_start, c_end, r_start, r_end) = self.cell_range(rect);
        for r in r_start..=r_end {
            for c in c_start..=c_end {
                let cell_idx = r * self.cols + c;
                for existing in &self.cells[cell_idx] {
                    if rectangles_overlap(rect, *existing) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Insert `rect` into all overlapping grid cells.
    pub fn insert(&mut self, rect: Rect2D) {
        let (c_start, c_end, r_start, r_end) = self.cell_range(rect);
        for r in r_start..=r_end {
            for c in c_start..=c_end {
                let cell_idx = r * self.cols + c;
                self.cells[cell_idx].push(rect);
            }
        }
    }
}

fn rectangles_overlap(a: Rect2D, b: Rect2D) -> bool {
    !(a.max_x() <= b.min_x()
        || b.max_x() <= a.min_x()
        || a.max_y() <= b.min_y()
        || b.max_y() <= a.min_y())
}

/// Cache of measured label dimensions keyed by exact text string.
#[derive(Clone, Debug, Default)]
pub struct LabelMeasureCache {
    measurements: BTreeMap<String, Size2D>,
}

impl LabelMeasureCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Estimate label size using conservative heuristic or cached measurement.
    pub fn measure_or_estimate(
        &mut self,
        text: &str,
        char_width: f64,
        line_height: f64,
    ) -> Size2D {
        if let Some(&cached) = self.measurements.get(text) {
            return cached;
        }

        let width = (text.chars().count() as f64 * char_width).max(8.0);
        let height = line_height.max(10.0);
        let default_size = Size2D::new(8.0, 10.0).expect("valid default size");
        let size = Size2D::new(width, height).unwrap_or(default_size);
        if self.measurements.len() < 2048 {
            self.measurements.insert(text.to_string(), size);
        }
        size
    }

    pub fn insert_measurement(&mut self, text: String, size: Size2D) {
        if self.measurements.len() < 4096 {
            self.measurements.insert(text, size);
        }
    }
}

/// Retained set of placed labels across frames for stability / anti-flicker.
#[derive(Clone, Debug, Default)]
pub struct RetainedLabelSet {
    placed_nodes: BTreeMap<AtlasNodeId, Rect2D>,
}

impl RetainedLabelSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, node: AtlasNodeId) -> bool {
        self.placed_nodes.contains_key(&node)
    }

    pub fn get_screen_rect(&self, node: AtlasNodeId) -> Option<Rect2D> {
        self.placed_nodes.get(&node).copied()
    }

    pub fn update_from_plan(&mut self, plan: &LabelPlan) {
        self.placed_nodes.clear();
        for label in &plan.labels {
            self.placed_nodes.insert(label.node, label.screen_rect);
        }
    }
}

/// Immutable plan of placed labels for presentation.
#[derive(Clone, Debug, PartialEq)]
pub struct LabelPlan {
    pub labels: Vec<PlacedLabel>,
    pub stats: LabelStats,
}

impl LabelPlan {
    pub fn labels(&self) -> &[PlacedLabel] {
        &self.labels
    }

    pub fn stats(&self) -> LabelStats {
        self.stats
    }
}

/// Places labels into the viewport using rank sorting and collision detection.
pub fn place_labels(
    candidates: &[LabelCandidate],
    camera: &Camera2D,
    limits: LabelLimits,
    retained: Option<&RetainedLabelSet>,
) -> Result<LabelPlan, AtlasError> {
    let viewport = Rect2D::new(Point2D::ORIGIN, camera.display().logical_size());
    let mut grid = CollisionGrid::new(viewport, limits.cell_size_pixels)?;
    let mut stats = LabelStats {
        candidates_evaluated: candidates.len(),
        ..Default::default()
    };

    // Filter and score candidates
    let mut ranked: Vec<(u64, usize)> = Vec::with_capacity(candidates.len());
    let mut selected_idx: Option<usize> = None;

    for (idx, cand) in candidates.iter().enumerate() {
        if cand.context.is_selected {
            selected_idx = Some(idx);
        }

        // Check if candidate parcel is large enough to warrant a label
        let min_extent = cand.logical_bounds.size().width().min(cand.logical_bounds.size().height());
        let screen_extent = min_extent * camera.points_per_unit() * camera.display().scale_factor();

        // Selected candidate bypasses minimum extent check to guarantee visibility
        if !cand.context.is_selected && screen_extent < limits.min_parcel_extent_pixels {
            stats.rejected_extent_too_small += 1;
            continue;
        }

        let was_retained = retained.map(|r| r.contains(cand.node)).unwrap_or(false);
        let score = cand.rank_score(was_retained);
        ranked.push((score, idx));
    }

    // Sort descending by rank score (highest score first)
    ranked.sort_unstable_by(|a, b| b.0.cmp(&a.0));

    let mut placed_labels = Vec::with_capacity(limits.max_labels.min(candidates.len()));
    let padding = limits.label_padding_pixels;

    // 1. Mandatory Selected Item Placement:
    // If a selected item candidate exists within the viewport, place it first
    // to guarantee that label budget/collision pressure cannot cull the selected item.
    if let Some(sel_idx) = selected_idx {
        let sel_cand = &candidates[sel_idx];
        if let Some(screen_rect) = compute_label_rect(sel_cand, camera, padding) {
            let padded_rect = expand_rect(screen_rect, padding)?;
            grid.insert(padded_rect);
            placed_labels.push(PlacedLabel {
                node: sel_cand.node,
                text: sel_cand.text.clone(),
                screen_rect,
                is_selected: true,
                is_retained: retained.map(|r| r.contains(sel_cand.node)).unwrap_or(false),
            });
            stats.labels_placed += 1;
            stats.selected_preserved = true;
            if retained.map(|r| r.contains(sel_cand.node)).unwrap_or(false) {
                stats.retained_from_previous += 1;
            }
        }
    }

    // 2. Place remaining candidates in rank order up to max_labels
    for (_score, idx) in ranked {
        if placed_labels.len() >= limits.max_labels {
            stats.rejected_budget += 1;
            continue;
        }

        // Skip if already placed as selected item
        if selected_idx == Some(idx) {
            continue;
        }

        let cand = &candidates[idx];
        let Some(screen_rect) = compute_label_rect(cand, camera, padding) else {
            continue;
        };

        let padded_rect = match expand_rect(screen_rect, padding) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if grid.collides(padded_rect) {
            stats.rejected_collision += 1;
            continue;
        }

        grid.insert(padded_rect);
        let is_ret = retained.map(|r| r.contains(cand.node)).unwrap_or(false);
        if is_ret {
            stats.retained_from_previous += 1;
        }

        placed_labels.push(PlacedLabel {
            node: cand.node,
            text: cand.text.clone(),
            screen_rect,
            is_selected: cand.context.is_selected,
            is_retained: is_ret,
        });
        stats.labels_placed += 1;
    }

    Ok(LabelPlan {
        labels: placed_labels,
        stats,
    })
}

/// Compute screen-space label rectangle positioned near the center/top of the parcel.
fn compute_label_rect(
    candidate: &LabelCandidate,
    camera: &Camera2D,
    _padding: f64,
) -> Option<Rect2D> {
    // Project parcel center to screen coordinates
    let center = Point2D::new(
        candidate.logical_bounds.min_x() + candidate.logical_bounds.size().width() / 2.0,
        candidate.logical_bounds.min_y() + candidate.logical_bounds.size().height() / 2.0,
    )
    .ok()?;

    let screen_center = camera.local_to_logical(center).ok()?;

    let label_w = candidate.estimated_size.width();
    let label_h = candidate.estimated_size.height();

    let origin = Point2D::new(
        screen_center.x() - label_w / 2.0,
        screen_center.y() - label_h / 2.0,
    )
    .ok()?;

    let size = Size2D::new(label_w, label_h).ok()?;
    Some(Rect2D::new(origin, size))
}

fn expand_rect(rect: Rect2D, padding: f64) -> Result<Rect2D, AtlasError> {
    Rect2D::from_xywh(
        rect.min_x() - padding,
        rect.min_y() - padding,
        rect.size().width() + padding * 2.0,
        rect.size().height() + padding * 2.0,
    )
    .map_err(|_| AtlasError::Camera(crate::CameraError::InvalidGeometry))
}

/// Helper to generate candidate labels from a visible plan.
pub fn candidates_from_visible_parcels(
    parcels: &[VisibleParcel],
    path_resolver: impl Fn(AtlasNodeId) -> Option<String>,
    selected_node: Option<AtlasNodeId>,
    cache: &mut LabelMeasureCache,
) -> Vec<LabelCandidate> {
    let mut candidates = Vec::with_capacity(parcels.len());
    for parcel in parcels {
        let Some(path) = path_resolver(parcel.node()) else {
            continue;
        };
        // Use filename or last path component for label text
        let filename = path
            .rsplit('/')
            .next()
            .unwrap_or(&path)
            .to_string();

        let is_selected = selected_node == Some(parcel.node());
        let estimated_size = cache.measure_or_estimate(&filename, 8.0, 14.0);

        candidates.push(LabelCandidate::new(
            parcel.node(),
            filename,
            parcel.logical_rect(),
            parcel.represented_leaves() as u32,
            LabelContext {
                is_selected,
                search_relevance: None,
                is_navigation_context: false,
            },
            estimated_size,
        ));
    }
    candidates
}

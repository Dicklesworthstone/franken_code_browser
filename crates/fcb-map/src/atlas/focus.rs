//! Precision-safe gestures in a retained atlas. An explicit focus bounds the
//! navigation domain. Automatic islands may be left by panning or zooming out.
use fcb_core::{DisplayMetrics, Point2D, Rect2D, Size2D};
use super::{AtlasError, AtlasIndex, AtlasNodeId, BranchKind};
use crate::{Camera2D, CameraError};

/// An automatically chosen local origin and the explicit focus it belongs to.
/// Both identities refer to the same immutable layout. Keep this with the
/// camera in history so Back also restores the correct navigation boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasFocus {
    node: AtlasNodeId,
    boundary: AtlasNodeId,
}

impl AtlasFocus {
    pub const fn new(node: AtlasNodeId) -> Self { Self { node, boundary: node } }
    pub const fn node(self) -> AtlasNodeId { self.node }

    /// Pan, resize or restore an already computed camera. If the viewport
    /// crosses an automatic island edge, admit the neighboring layout again.
    pub fn accommodate(self, index: &AtlasIndex<'_>, camera: Camera2D)
        -> Result<(Self, Camera2D), AtlasError> {
        index.resolve(self.node)?;
        if camera.generation().owner() != index.owner() { return Err(AtlasError::OwnerMismatch); }
        if self.node == self.boundary { return Ok((self, camera)); }
        let rect = camera.local_viewport()?;
        let view = Viewport::new(rect.origin(), rect.size());
        self.place(index, camera, camera.points_per_unit(), view)
    }

    /// Magnify around a logical point. All ordinary root gestures use the
    /// original camera arithmetic. Rebase only when an origin's rounding
    /// interval approaches 1/32 of a physical pixel at the proposed scale.
    pub fn zoom(self, index: &AtlasIndex<'_>, camera: Camera2D, anchor: Point2D, factor: f64)
        -> Result<(Self, Camera2D), AtlasError> {
        index.resolve(self.node)?;
        if camera.generation().owner() != index.owner() { return Err(AtlasError::OwnerMismatch); }
        let scale = camera.points_per_unit() * factor;
        if !scale.is_finite() || scale <= 0.0 { return Err(CameraError::InvalidGeometry.into()); }
        if factor == 1.0 { return Ok((self, camera)); }
        let display = camera.display();
        let size = display.logical_size();
        let mut view = Viewport::new(camera.origin(), Size2D::new(size.width() / scale, size.height() / scale)
            .map_err(|_| CameraError::InvalidGeometry)?);
        view = view.shift(anchor.x() / camera.points_per_unit(), anchor.y() / camera.points_per_unit())
            .shift(-anchor.x() / scale, -anchor.y() / scale);
        if !view.needs_rebase(scale, display) {
            return self.accommodate(index, camera.zoom_at(anchor, factor)?);
        }
        // Do not materialize a camera in the old domain: its viewport might
        // already collapse there, although all its corners are distinct locally.
        let (focus, placed) = self.place(index, camera, scale, view)?;
        Ok((focus, Camera2D::new(camera.next_generation()?, display, placed.origin(), scale)?))
    }

    fn place(mut self, index: &AtlasIndex<'_>, camera: Camera2D, scale: f64, mut view: Viewport)
        -> Result<(Self, Camera2D), AtlasError> {
        let mut node = index.resolve(self.node)?;
        // Parent depth is capped and checked when the index is built.
        while self.node != self.boundary {
            let cell = index.layout.nodes()[node].parent_local();
            let parent_view = view.shift(cell.min_x(), cell.min_y());
            // Hysteresis allows zoom-out to restore the surrounding map before
            // the viewport crosses a parcel edge, without flickering at entry.
            if view.fits(cell.size()) && parent_view.pixel_spacing(scale, camera.display()) >= 1.0 / 128.0 {
                break;
            }
            view = parent_view;
            node = index.nodes[node].parent.ok_or(AtlasError::NotDescendant)?;
            self.node = index.key(node);
        }
        // A broad group bounding box can cover gaps. Limit total probes, not
        // just returned hits. Retain the best valid domain when the budget ends.
        let mut probes = 256;
        while view.needs_rebase(scale, camera.display()) {
            let Some(group) = index.nodes[node].acceleration else { break };
            let Some(child) = containing_child(index, group, view, &mut probes) else { break };
            let bounds = index.layout.nodes()[child].parent_local();
            view = view.shift(-bounds.min_x(), -bounds.min_y());
            node = child;
            self.node = index.key(child);
        }
        // No qualified child spans a sibling seam, and even a leaf has finite
        // resolution. Keep the previous usable frame instead of silently moving
        // its anchor by a visible fraction of a pixel.
        if view.pixel_spacing(scale, camera.display()) > 0.25 { return Err(CameraError::PrecisionLost.into()); }
        let origin = Point2D::new(view.x.hi, view.y.hi).map_err(|_| CameraError::InvalidGeometry)?;
        Ok((self, Camera2D::new(camera.generation(), camera.display(), origin, scale)?))
    }
}

fn containing_child(index: &AtlasIndex<'_>, group: usize, view: Viewport, remaining: &mut usize)
    -> Option<usize> {
    *remaining = remaining.checked_sub(1)?;
    let branch = index.branches[group];
    if !view.inside(branch.bounds) { return None; }
    match branch.kind {
        BranchKind::Leaf(node) => Some(node),
        BranchKind::Fork(left, right) => containing_child(index, left, view, remaining)
            .or_else(|| containing_child(index, right, view, remaining)),
    }
}

// Error-free two-sum retains the small residual when adding a pointer offset
// to a large origin. It can be recovered when the child origin is subtracted.
#[derive(Clone, Copy)]
struct Coordinate { hi: f64, lo: f64 }
impl Coordinate {
    fn sum(a: f64, b: f64) -> Self {
        let hi = a + b;
        let part = hi - a;
        Self { hi, lo: (a - (hi - part)) + (b - part) }
    }
    fn shift(self, delta: f64) -> Self {
        let sum = Self::sum(self.hi, delta);
        Self::sum(sum.hi, sum.lo + self.lo)
    }
}

#[derive(Clone, Copy)]
struct Viewport { x: Coordinate, y: Coordinate, size: Size2D }
impl Viewport {
    fn new(origin: Point2D, size: Size2D) -> Self {
        Self { x: Coordinate { hi: origin.x(), lo: 0.0 }, y: Coordinate { hi: origin.y(), lo: 0.0 }, size }
    }
    fn shift(self, x: f64, y: f64) -> Self {
        Self { x: self.x.shift(x), y: self.y.shift(y), ..self }
    }
    fn fits(self, size: Size2D) -> bool {
        self.x.hi >= 0.0 && self.y.hi >= 0.0
            && self.x.shift(-size.width()).hi <= -self.size.width()
            && self.y.shift(-size.height()).hi <= -self.size.height()
    }
    fn inside(self, bounds: Rect2D) -> bool {
        self.shift(-bounds.min_x(), -bounds.min_y()).fits(bounds.size())
    }
    fn pixel_spacing(self, scale: f64, display: DisplayMetrics) -> f64 {
        (self.x.hi.abs().max(self.y.hi.abs()) * f64::EPSILON) * scale * display.scale_factor()
    }
    fn needs_rebase(self, scale: f64, display: DisplayMetrics) -> bool {
        self.pixel_spacing(scale, display) > 1.0 / 32.0
    }
}

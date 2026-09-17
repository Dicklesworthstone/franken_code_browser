#![forbid(unsafe_code)]

//! Renderer-neutral, top-left-origin atlas camera in logical points.
//!
//! Local atlas units, logical points and physical pixels are separate domains.
//! Camera changes are pure, checked values: no clock, animation, allocation,
//! source access or layout rebuild. A viewport query additionally binds a camera
//! to its focus node; rebasing the focus never changes the retained layout.

use fcb_core::{CameraGeneration, DisplayMetrics, Point2D, Rect2D};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CameraError {
    InvalidGeometry,
    PrecisionLost,
    OwnerMismatch,
    GenerationExhausted,
}
impl std::fmt::Display for CameraError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidGeometry => "CAMERA_INVALID_GEOMETRY",
            Self::PrecisionLost => "CAMERA_PRECISION_LOST",
            Self::OwnerMismatch => "CAMERA_OWNER_MISMATCH",
            Self::GenerationExhausted => "CAMERA_GENERATION_EXHAUSTED",
        })
    }
}
impl std::error::Error for CameraError {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera2D {
    generation: CameraGeneration,
    display: DisplayMetrics,
    origin: Point2D,
    points_per_unit: f64,
}
impl Camera2D {
    pub fn new(generation: CameraGeneration, display: DisplayMetrics,
        origin: Point2D, points_per_unit: f64) -> Result<Self, CameraError> {
        if generation.owner() != display.generation().owner() {
            return Err(CameraError::OwnerMismatch);
        }
        if !points_per_unit.is_finite() || points_per_unit <= 0.0
            || display.logical_size().is_empty() || display.physical_pixels().is_err() {
            return Err(CameraError::InvalidGeometry);
        }
        let camera = Self { generation, display, origin, points_per_unit };
        let extent = camera.local_viewport()?;
        checked_rect(extent)?;
        Ok(camera)
    }

    pub fn fit(generation: CameraGeneration, display: DisplayMetrics,
        bounds: Rect2D, padding_points: f64) -> Result<Self, CameraError> {
        checked_rect(bounds)?;
        if !padding_points.is_finite() || padding_points < 0.0 {
            return Err(CameraError::InvalidGeometry);
        }
        let width = display.logical_size().width() - 2.0 * padding_points;
        let height = display.logical_size().height() - 2.0 * padding_points;
        if width <= 0.0 || height <= 0.0 { return Err(CameraError::InvalidGeometry); }
        let scale = (width / bounds.size().width()).min(height / bounds.size().height());
        if !scale.is_finite() || scale <= 0.0 { return Err(CameraError::InvalidGeometry); }
        // Avoid averaging two large endpoints, which could overflow.
        let x = bounds.min_x() - (display.logical_size().width() / scale - bounds.size().width()) * 0.5;
        let y = bounds.min_y() - (display.logical_size().height() / scale - bounds.size().height()) * 0.5;
        Self::new(generation, display, Point2D::new(x, y).map_err(|_| CameraError::InvalidGeometry)?, scale)
    }

    pub const fn generation(self) -> CameraGeneration { self.generation }
    pub const fn display(self) -> DisplayMetrics { self.display }
    pub const fn origin(self) -> Point2D { self.origin }
    pub const fn points_per_unit(self) -> f64 { self.points_per_unit }

    pub fn local_viewport(self) -> Result<Rect2D, CameraError> {
        Rect2D::from_xywh(self.origin.x(), self.origin.y(),
            self.display.logical_size().width() / self.points_per_unit,
            self.display.logical_size().height() / self.points_per_unit)
            .map_err(|_| CameraError::InvalidGeometry)
    }

    pub fn local_to_logical(self, point: Point2D) -> Result<Point2D, CameraError> {
        Point2D::new((point.x() - self.origin.x()) * self.points_per_unit,
            (point.y() - self.origin.y()) * self.points_per_unit)
            .map_err(|_| CameraError::InvalidGeometry)
    }

    pub fn logical_to_local(self, point: Point2D) -> Result<Point2D, CameraError> {
        Point2D::new(self.origin.x() + point.x() / self.points_per_unit,
            self.origin.y() + point.y() / self.points_per_unit)
            .map_err(|_| CameraError::InvalidGeometry)
    }

    /// Pan the content with the gesture. Positive x moves parcels right.
    pub fn pan(self, delta_points: Point2D) -> Result<Self, CameraError> {
        if delta_points == Point2D::ORIGIN { return Ok(self); }
        let origin = Point2D::new(self.origin.x() - delta_points.x() / self.points_per_unit,
            self.origin.y() - delta_points.y() / self.points_per_unit)
            .map_err(|_| CameraError::InvalidGeometry)?;
        Self::new(self.next_generation()?, self.display, origin, self.points_per_unit)
    }

    /// Retain the local point under the pointer/gesture centroid without rounding.
    /// Overflow or a numerically unrepresentable viewport leaves `self` intact.
    pub fn zoom_at(self, anchor_points: Point2D, factor: f64) -> Result<Self, CameraError> {
        if !factor.is_finite() || factor <= 0.0 { return Err(CameraError::InvalidGeometry); }
        if factor == 1.0 { return Ok(self); }
        let scale = self.points_per_unit * factor;
        if !scale.is_finite() || scale <= 0.0 { return Err(CameraError::InvalidGeometry); }
        let anchor = self.logical_to_local(anchor_points)?;
        let origin = Point2D::new(anchor.x() - anchor_points.x() / scale,
            anchor.y() - anchor_points.y() / scale).map_err(|_| CameraError::InvalidGeometry)?;
        Self::new(self.next_generation()?, self.display, origin, scale)
    }

    /// Preserve the local center and logical scale during a host display change.
    pub fn with_display(self, display: DisplayMetrics) -> Result<Self, CameraError> {
        if display == self.display { return Ok(self); }
        let old = self.display.logical_size();
        let new = display.logical_size();
        let origin = Point2D::new(self.origin.x() + (old.width() - new.width()) / (2.0 * self.points_per_unit),
            self.origin.y() + (old.height() - new.height()) / (2.0 * self.points_per_unit))
            .map_err(|_| CameraError::InvalidGeometry)?;
        Self::new(self.next_generation()?, display, origin, self.points_per_unit)
    }

    /// Project ONLY the visible intersection. Huge offscreen coordinates never
    /// become GPU-facing values. Touching edges have no visible interior.
    pub fn project_clipped(self, bounds: Rect2D) -> Result<Option<Rect2D>, CameraError> {
        checked_rect(bounds)?;
        let Some(clipped) = bounds.intersection(self.local_viewport()?) else { return Ok(None); };
        if clipped.size().is_empty() { return Ok(None); }
        let start = self.local_to_logical(clipped.origin())?;
        let end = self.local_to_logical(Point2D::new(clipped.max_x(), clipped.max_y())
            .map_err(|_| CameraError::InvalidGeometry)?)?;
        // Clamp floating-point edge noise to the actual view, not arbitrary world bounds.
        let x = start.x().max(0.0);
        let y = start.y().max(0.0);
        let right = end.x().min(self.display.logical_size().width());
        let bottom = end.y().min(self.display.logical_size().height());
        if right <= x || bottom <= y { return Ok(None); }
        Ok(Some(Rect2D::from_xywh(x, y, right - x, bottom - y)
            .map_err(|_| CameraError::InvalidGeometry)?))
    }

    pub(crate) fn next_generation(self) -> Result<CameraGeneration, CameraError> {
        let next = self.generation.get().checked_add(1).ok_or(CameraError::GenerationExhausted)?;
        CameraGeneration::new(self.generation.owner(), next).map_err(|_| CameraError::GenerationExhausted)
    }
}

pub(crate) fn checked_rect(rect: Rect2D) -> Result<(), CameraError> {
    if rect.size().is_empty() || !rect.max_x().is_finite() || !rect.max_y().is_finite() {
        return Err(CameraError::InvalidGeometry);
    }
    if rect.max_x() <= rect.min_x() || rect.max_y() <= rect.min_y() {
        return Err(CameraError::PrecisionLost);
    }
    Ok(())
}

/// A viewport-rebased point narrowed to `f32` for GPU submission.
///
/// Rebasing subtracts the camera origin before the `f64 -> f32`
/// narrowing, so coordinates near the focal point stay exactly
/// representable even after deep zoom. The narrowing is checked: a value
/// that would lose precision or is non-finite is refused rather than
/// silently drifting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RebasedPointF32 {
    pub x: f32,
    pub y: f32,
}

impl Camera2D {
    /// Convert a local-space point into a rebased, checked `f32` pair
    /// suitable for GPU submission.
    ///
    /// The point is expressed relative to the camera's viewport origin
    /// (rebased), so magnitudes stay small regardless of world position.
    /// Refuses non-finite input and any narrowing that would change the
    /// value (f32 precision loss at deep zoom is detected, not hidden).
    pub fn checked_screen_f32(&self, local: Point2D) -> Result<RebasedPointF32, CameraError> {
        let viewport = self.local_viewport()?;
        let x = local.x() - viewport.min_x();
        let y = local.y() - viewport.min_y();
        if !x.is_finite() || !y.is_finite() {
            return Err(CameraError::InvalidGeometry);
        }
        let fx = x as f32;
        let fy = y as f32;
        if f64::from(fx) != x || f64::from(fy) != y {
            return Err(CameraError::PrecisionLost);
        }
        Ok(RebasedPointF32 { x: fx, y: fy })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{ArenaOwnerId, DisplayColorConfig, DisplayGeneration, Size2D};
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(314).unwrap() }
    fn display(scale: f64, width: f64, height: f64, id: u64) -> DisplayMetrics {
        DisplayMetrics::new(scale, Size2D::new(width, height).unwrap(), DisplayColorConfig::Srgb,
            DisplayGeneration::new(owner(), id).unwrap()).unwrap()
    }
    fn camera() -> Camera2D {
        Camera2D::new(CameraGeneration::new(owner(), 1).unwrap(), display(2.0, 800.0, 600.0, 1),
            Point2D::new(-30.0, 75.0).unwrap(), 2.5).unwrap()
    }
    fn near(left: f64, right: f64) { assert!((left - right).abs() < 1e-8, "{left} != {right}"); }

    #[test]
    fn anchored_zoom_preserves_local_point_over_many_scales() {
        for (x, y) in [(0.0, 0.0), (400.0, 300.0), (799.0, 599.0), (-4.0, 90.0)] {
            let anchor = Point2D::new(x, y).unwrap();
            for factor in [0.125, 0.5, 1.0, 2.0, 32.0] {
                let old = camera();
                let local = old.logical_to_local(anchor).unwrap();
                let next = old.zoom_at(anchor, factor).unwrap();
                let projected = next.local_to_logical(local).unwrap();
                near(projected.x(), x); near(projected.y(), y);
                let round_trip = next.zoom_at(anchor, 1.0 / factor).unwrap();
                near(round_trip.origin().x(), old.origin().x());
                near(round_trip.origin().y(), old.origin().y());
            }
        }
    }
    #[test]
    fn pan_tracks_gesture_in_points_not_backing_pixels() {
        let old = camera();
        let local = Point2D::new(10.0, 100.0).unwrap();
        let before = old.local_to_logical(local).unwrap();
        let after = old.pan(Point2D::new(17.0, -9.0).unwrap()).unwrap().local_to_logical(local).unwrap();
        near(after.x() - before.x(), 17.0); near(after.y() - before.y(), -9.0);
    }
    #[test]
    fn fit_keeps_aspect_ratio_and_requested_padding() {
        let bounds = Rect2D::from_xywh(100.0, -80.0, 200.0, 100.0).unwrap();
        let camera = Camera2D::fit(CameraGeneration::new(owner(), 1).unwrap(), display(2.0, 800.0, 600.0, 1), bounds, 20.0).unwrap();
        let rect = camera.project_clipped(bounds).unwrap().unwrap();
        near(rect.min_x(), 20.0); near(rect.max_x(), 780.0);
        near(rect.size().width() / rect.size().height(), 2.0);
    }
    #[test]
    fn display_change_preserves_center_and_scale() {
        let old = camera();
        let center = old.logical_to_local(Point2D::new(400.0, 300.0).unwrap()).unwrap();
        let next = old.with_display(display(1.0, 1200.0, 400.0, 2)).unwrap();
        let new_center = next.logical_to_local(Point2D::new(600.0, 200.0).unwrap()).unwrap();
        near(center.x(), new_center.x()); near(center.y(), new_center.y());
        assert_eq!(next.points_per_unit(), old.points_per_unit());
    }
    #[test]
    fn overflow_precision_loss_and_invalid_zoom_are_refused() {
        for factor in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            assert!(camera().zoom_at(Point2D::ORIGIN, factor).is_err());
        }
        assert!(matches!(Camera2D::new(CameraGeneration::new(owner(), 1).unwrap(), display(1.0, 800.0, 600.0, 1),
            Point2D::new(1e30, 1e30).unwrap(), 1.0), Err(CameraError::PrecisionLost)));
        let far = Rect2D::from_xywh(f64::MAX, 0.0, f64::MAX, 1.0).unwrap();
        assert!(camera().project_clipped(far).is_err());
    }
    #[test]
    fn generations_do_not_wrap_and_noop_does_not_exhaust() {
        let max = Camera2D::new(CameraGeneration::new(owner(), u64::MAX).unwrap(), camera().display(), Point2D::ORIGIN, 1.0).unwrap();
        assert_eq!(max.pan(Point2D::ORIGIN).unwrap(), max);
        assert_eq!(max.zoom_at(Point2D::ORIGIN, 1.0).unwrap(), max);
        assert_eq!(max.pan(Point2D::new(1.0, 0.0).unwrap()), Err(CameraError::GenerationExhausted));
    }
    #[test]
    fn clip_is_view_local_and_touching_edge_is_not_visible() {
        let camera = Camera2D::new(CameraGeneration::new(owner(), 1).unwrap(), display(1.0, 100.0, 100.0, 1), Point2D::ORIGIN, 1.0).unwrap();
        assert_eq!(camera.project_clipped(Rect2D::from_xywh(-20.0, -30.0, 40.0, 50.0).unwrap()).unwrap().unwrap(),
            Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0).unwrap());
        assert!(camera.project_clipped(Rect2D::from_xywh(100.0, 0.0, 20.0, 20.0).unwrap()).unwrap().is_none());
    }
    #[test]
    fn rebased_f32_conversion_is_exact_within_viewport() {
        let camera = Camera2D::new(CameraGeneration::new(owner(), 1).unwrap(), display(1.0, 100.0, 100.0, 1), Point2D::ORIGIN, 1.0).unwrap();
        for (x, y) in [(0.0, 0.0), (12.5, 87.25), (99.0, 1.0)] {
            let local = Point2D::new(x, y).unwrap();
            let screen = camera.checked_screen_f32(local).unwrap();
            assert_eq!(f64::from(screen.x), x);
            assert_eq!(f64::from(screen.y), y);
        }
    }
    #[test]
    fn rebased_f32_conversion_refuses_precision_loss() {
        // The rebasing subtraction itself can produce an f64 value that
        // is NOT f32-exact: 2^24 (f32-exact) minus 0.5 (f32-exact) yields
        // 16777215.5, which needs 25 significant bits. The conversion must
        // refuse it rather than silently round.
        let camera = Camera2D::new(CameraGeneration::new(owner(), 1).unwrap(), display(1.0, 100.0, 100.0, 1), Point2D::new(0.5, 0.5).unwrap(), 1.0).unwrap();
        let local = Point2D::new(16777216.0, 0.0).unwrap();
        assert_eq!(
            camera.checked_screen_f32(local).unwrap_err(),
            CameraError::PrecisionLost
        );
    }
    #[test]
    fn nan_point2d_is_rejected_at_construction() {
        // Point2D structurally excludes NaN, so the InvalidGeometry arm of
        // checked_screen_f32 is only reachable through the viewport; the
        // constructor rejection is the typed boundary.
        assert!(Point2D::new(f64::NAN, 0.0).is_err());
    }
    #[test]
    fn rebased_f32_survives_deep_zoom_near_anchor() {
        // Deep zoom: world coordinates become huge, but the viewport-
        // rebased f32 of points near the anchor stays small and exact.
        let mut camera = Camera2D::new(CameraGeneration::new(owner(), 1).unwrap(), display(1.0, 100.0, 100.0, 1), Point2D::ORIGIN, 1.0).unwrap();
        let anchor = Point2D::new(50.0, 50.0).unwrap();
        for _ in 0..40 {
            camera = camera.zoom_at(anchor, 2.0).unwrap();
        }
        let near_anchor = camera.local_to_logical(anchor).unwrap();
        let screen = camera.checked_screen_f32(near_anchor).unwrap();
        assert!(screen.x.is_finite() && screen.y.is_finite());
    }
}

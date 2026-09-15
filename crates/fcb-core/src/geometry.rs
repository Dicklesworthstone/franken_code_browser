#![forbid(unsafe_code)]

use crate::{CoreError, DisplayGeneration};

/// A finite 2D point in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point2D {
    x: f64,
    y: f64,
}

impl Point2D {
    pub const ORIGIN: Self = Self { x: 0.0, y: 0.0 };

    pub fn new(x: f64, y: f64) -> Result<Self, CoreError> {
        if !x.is_finite() || !y.is_finite() {
            Err(CoreError::NonFiniteGeometry)
        } else {
            // Canonicalize -0.0 to +0.0
            let x = if x == 0.0 { 0.0 } else { x };
            let y = if y == 0.0 { 0.0 } else { y };
            Ok(Self { x, y })
        }
    }

    pub const fn x(self) -> f64 {
        self.x
    }

    pub const fn y(self) -> f64 {
        self.y
    }
}

/// A finite, non-negative 2D size in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size2D {
    width: f64,
    height: f64,
}

impl Size2D {
    pub const ZERO: Self = Self {
        width: 0.0,
        height: 0.0,
    };

    pub fn new(width: f64, height: f64) -> Result<Self, CoreError> {
        if !width.is_finite() || !height.is_finite() {
            return Err(CoreError::NonFiniteGeometry);
        }
        if width < 0.0 || height < 0.0 {
            return Err(CoreError::InvalidGeometry);
        }
        let width = if width == 0.0 { 0.0 } else { width };
        let height = if height == 0.0 { 0.0 } else { height };
        Ok(Self { width, height })
    }

    pub const fn width(self) -> f64 {
        self.width
    }

    pub const fn height(self) -> f64 {
        self.height
    }

    pub fn is_empty(self) -> bool {
        self.width == 0.0 || self.height == 0.0
    }

    pub fn area(self) -> f64 {
        self.width * self.height
    }
}

/// A finite 2D axis-aligned rectangle in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect2D {
    origin: Point2D,
    size: Size2D,
}

impl Rect2D {
    pub const ZERO: Self = Self {
        origin: Point2D::ORIGIN,
        size: Size2D::ZERO,
    };

    pub const fn new(origin: Point2D, size: Size2D) -> Self {
        Self { origin, size }
    }

    pub fn from_xywh(x: f64, y: f64, width: f64, height: f64) -> Result<Self, CoreError> {
        let origin = Point2D::new(x, y)?;
        let size = Size2D::new(width, height)?;
        Ok(Self { origin, size })
    }

    pub const fn origin(self) -> Point2D {
        self.origin
    }

    pub const fn size(self) -> Size2D {
        self.size
    }

    pub fn min_x(self) -> f64 {
        self.origin.x()
    }

    pub fn max_x(self) -> f64 {
        self.origin.x() + self.size.width()
    }

    pub fn min_y(self) -> f64 {
        self.origin.y()
    }

    pub fn max_y(self) -> f64 {
        self.origin.y() + self.size.height()
    }

    pub fn contains_point(self, p: Point2D) -> bool {
        p.x() >= self.min_x()
            && p.x() <= self.max_x()
            && p.y() >= self.min_y()
            && p.y() <= self.max_y()
    }

    pub fn contains_rect(self, other: Rect2D) -> bool {
        other.min_x() >= self.min_x()
            && other.max_x() <= self.max_x()
            && other.min_y() >= self.min_y()
            && other.max_y() <= self.max_y()
    }

    pub fn intersects(self, other: Rect2D) -> bool {
        self.min_x() <= other.max_x()
            && self.max_x() >= other.min_x()
            && self.min_y() <= other.max_y()
            && self.max_y() >= other.min_y()
    }

    pub fn intersection(self, other: Rect2D) -> Option<Rect2D> {
        let min_x = self.min_x().max(other.min_x());
        let max_x = self.max_x().min(other.max_x());
        let min_y = self.min_y().max(other.min_y());
        let max_y = self.max_y().min(other.max_y());

        if min_x <= max_x && min_y <= max_y {
            let width = max_x - min_x;
            let height = max_y - min_y;
            // Both min_x, min_y, width, height are finite derived from finite inputs
            let origin = Point2D::new(min_x, min_y).ok()?;
            let size = Size2D::new(width, height).ok()?;
            Some(Rect2D::new(origin, size))
        } else {
            None
        }
    }

    pub fn union(self, other: Rect2D) -> Result<Rect2D, CoreError> {
        let min_x = self.min_x().min(other.min_x());
        let max_x = self.max_x().max(other.max_x());
        let min_y = self.min_y().min(other.min_y());
        let max_y = self.max_y().max(other.max_y());

        let origin = Point2D::new(min_x, min_y)?;
        let size = Size2D::new(max_x - min_x, max_y - min_y)?;
        Ok(Rect2D::new(origin, size))
    }
}

/// Color configuration for the presentation surface.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum DisplayColorConfig {
    Srgb,
    DisplayP3,
    ExtendedLinearSrgb,
}

/// Display metrics carrying logical points, physical pixels, backing scale,
/// color configuration, and a display generation. Immutable, Send + Sync.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayMetrics {
    scale_factor: f64,
    logical_size: Size2D,
    color_config: DisplayColorConfig,
    generation: DisplayGeneration,
}

impl DisplayMetrics {
    pub fn new(
        scale_factor: f64,
        logical_size: Size2D,
        color_config: DisplayColorConfig,
        generation: DisplayGeneration,
    ) -> Result<Self, CoreError> {
        if !scale_factor.is_finite() {
            return Err(CoreError::NonFiniteGeometry);
        }
        if scale_factor <= 0.0 {
            return Err(CoreError::InvalidGeometry);
        }
        Ok(Self {
            scale_factor,
            logical_size,
            color_config,
            generation,
        })
    }

    pub const fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    pub const fn logical_size(&self) -> Size2D {
        self.logical_size
    }

    pub const fn color_config(&self) -> DisplayColorConfig {
        self.color_config
    }

    pub const fn generation(&self) -> DisplayGeneration {
        self.generation
    }

    /// Computes the physical pixel dimensions from logical size and backing scale factor.
    pub fn physical_pixels(&self) -> Result<(u32, u32), CoreError> {
        let width_px = (self.logical_size.width() * self.scale_factor).round();
        let height_px = (self.logical_size.height() * self.scale_factor).round();

        if !width_px.is_finite() || !height_px.is_finite() || width_px < 0.0 || height_px < 0.0 {
            return Err(CoreError::InvalidGeometry);
        }
        if width_px > u32::MAX as f64 || height_px > u32::MAX as f64 {
            return Err(CoreError::ArithmeticOverflow);
        }
        Ok((width_px as u32, height_px as u32))
    }

    /// Converts a logical point to physical pixel coordinates.
    pub fn logical_to_physical_point(&self, p: Point2D) -> Result<(f64, f64), CoreError> {
        let px = p.x() * self.scale_factor;
        let py = p.y() * self.scale_factor;
        if !px.is_finite() || !py.is_finite() {
            Err(CoreError::NonFiniteGeometry)
        } else {
            Ok((px, py))
        }
    }

    /// Converts physical pixel coordinates to a logical point.
    pub fn physical_to_logical_point(&self, px: f64, py: f64) -> Result<Point2D, CoreError> {
        let lx = px / self.scale_factor;
        let ly = py / self.scale_factor;
        Point2D::new(lx, ly)
    }
}

/// Bounded geometry for a semantic node, with outer bounds, content rect, and clipping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SemanticGeometry {
    bounds: Rect2D,
    content_rect: Rect2D,
    clip_rect: Option<Rect2D>,
}

impl SemanticGeometry {
    pub fn new(
        bounds: Rect2D,
        content_rect: Rect2D,
        clip_rect: Option<Rect2D>,
    ) -> Result<Self, CoreError> {
        // The content rect must be contained within bounds
        if !bounds.contains_rect(content_rect) {
            return Err(CoreError::InvalidGeometry);
        }
        Ok(Self {
            bounds,
            content_rect,
            clip_rect,
        })
    }

    pub const fn bounds(&self) -> Rect2D {
        self.bounds
    }

    pub const fn content_rect(&self) -> Rect2D {
        self.content_rect
    }

    pub const fn clip_rect(&self) -> Option<Rect2D> {
        self.clip_rect
    }

    /// The visible rectangle of this semantic node after clipping.
    pub fn visible_rect(&self) -> Option<Rect2D> {
        if let Some(clip) = self.clip_rect {
            self.bounds.intersection(clip)
        } else {
            Some(self.bounds)
        }
    }

    /// Tests whether a point in logical coordinates hits this node's visible area.
    pub fn contains_hit(&self, p: Point2D) -> bool {
        if !self.bounds.contains_point(p) {
            return false;
        }
        if let Some(clip) = self.clip_rect {
            clip.contains_point(p)
        } else {
            true
        }
    }
}

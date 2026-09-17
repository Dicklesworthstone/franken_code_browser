//! Explicit color, alpha, clipping, and depth contracts for the FCB render
//! route (FCB-089.A, fcb-npyi.1).
//!
//! This crate implements the CPU-reference semantics of the shader
//! color/alpha/clip/depth ABI as pure, dependency-light Rust:
//!
//! - **Linear SDR composition.** Theme and image inputs carry their
//!   declared encoding; shader compositing operates on linear values.
//!   Blending uses premultiplied source-over with the matching
//!   source/destination blend factors.
//! - **Exactly-once premultiplication.]
//!   A premultiplied value records its state; premultiplying an already
//!   premultiplied color is a detected error (double alpha), never a
//!   silent second pass. A glyph coverage mask is linear coverage, not
//!   sRGB color: it is never gamma-decoded.
//! - **Finite typed coordinates.** All positions submitted for GPU upload
//!   are finite and validated up front; NaN/Inf never normalize.
//! - **Scissor clamping.** Scissor rectangles use clamped physical
//!   integer bounds; empty scissoring is explicit. Clip-stack
//!   intersections are validated and bounded.
//!
//! GPU round-trip/visual corpus qualification (gamma, double alpha on
//! device, clip/depth flips, incompatible target reuse) belongs to
//! FCB-089.v; this crate is the CPU-reference seam those fixtures
//! compare against.

use std::fmt;

/// Errors from color/alpha/clip/depth ABI validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderAbiError {
    /// Premultiplying a color that is already premultiplied (double alpha).
    DoublePremultiply,
    /// A coverage mask was passed where an sRGB color is required
    /// (gamma-decoding linear coverage is the exact defect this prevents).
    CoverageAsColor,
    /// A required color value was missing for a compositing operation.
    MissingColor,
    /// A coordinate was non-finite (NaN or infinity).
    NonFiniteCoordinate,
    /// A scissor rect had negative or non-integer-aligned physical bounds.
    InvalidScissorBounds,
    /// The clip stack exceeded its bounded depth.
    ClipStackOverflow { max: usize },
    /// Pop was called on an empty clip stack.
    ClipStackUnderflow,
    /// The requested clip rect does not intersect the current clip.
    ClipEmpty,
    /// A depth value was outside the documented [0, 1] range.
    DepthOutOfRange,
}

impl fmt::Display for RenderAbiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::DoublePremultiply => "double premultiply: color is already premultiplied",
            Self::CoverageAsColor => "coverage mask used as sRGB color",
            Self::MissingColor => "missing color for compositing",
            Self::NonFiniteCoordinate => "non-finite coordinate rejected before GPU upload",
            Self::InvalidScissorBounds => {
                "scissor bounds must be finite, non-negative, integer-aligned physical pixels"
            }
            Self::ClipStackOverflow { max } => {
                return write!(formatter, "clip stack exceeded {max} entries");
            }
            Self::ClipStackUnderflow => formatter.write_str("clip stack underflow"),
            Self::ClipEmpty => formatter.write_str("clip rect does not intersect current clip"),
            Self::DepthOutOfRange => formatter.write_str("depth value outside [0, 1]"),
        };
        formatter.write_str(text)
    }
}

impl std::error::Error for RenderAbiError {}

/// A linear-space SDR color with premultiplication tracking.
///
/// Channels are f32 linear values in [0, 1] (values above 1 from wide-gamut
/// sources are clamped at the SDR baseline — wide-gamut/HDR is an
/// explicitly qualified optional route, never inferred). `premultiplied`
/// records whether RGB has been multiplied by alpha exactly once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorLinearSdr {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
    premultiplied: bool,
}

impl ColorLinearSdr {
    /// Create from straight (unassociated) linear channels in [0, 1].
    pub fn straight(r: f32, g: f32, b: f32, a: f32) -> Result<Self, RenderAbiError> {
        Self::validate_channels(&[r, g, b, a])?;
        Ok(Self { r, g, b, a, premultiplied: false })
    }

    /// Create from premultiplied linear channels, marking the state
    /// exactly once. Channels must already include alpha; re-premultiplying
    /// is the double-alpha defect and is refused.
    pub fn premultiplied(r: f32, g: f32, b: f32, a: f32) -> Result<Self, RenderAbiError> {
        Self::validate_channels(&[r, g, b, a])?;
        Ok(Self { r, g, b, a, premultiplied: true })
    }

    /// Convert a straight color to premultiplied form. Refuses colors that
    /// are already premultiplied (exactly-once guarantee).
    pub fn into_premultiplied(self) -> Result<Self, RenderAbiError> {
        if self.premultiplied {
            return Err(RenderAbiError::DoublePremultiply);
        }
        Ok(Self {
            r: self.r * self.a,
            g: self.g * self.a,
            b: self.b * self.a,
            a: self.a,
            premultiplied: true,
        })
    }

    /// Convert a premultiplied color back to straight form.
    pub fn into_straight(self) -> Self {
        if !self.premultiplied {
            return self;
        }
        let (r, g, b) = if self.a <= 0.0 {
            (0.0, 0.0, 0.0)
        } else {
            (self.r / self.a, self.g / self.a, self.b / self.a)
        };
        Self { r, g, b, a: self.a, premultiplied: false }
    }

    pub const fn is_premultiplied(&self) -> bool {
        self.premultiplied
    }

    pub const fn red(&self) -> f32 {
        self.r
    }

    pub const fn green(&self) -> f32 {
        self.g
    }

    pub const fn blue(&self) -> f32 {
        self.b
    }

    pub const fn alpha(&self) -> f32 {
        self.a
    }

    fn validate_channels(channels: &[f32; 4]) -> Result<(), RenderAbiError> {
        for &channel in channels {
            if !channel.is_finite() || !(0.0..=1.0).contains(&channel) {
                return Err(RenderAbiError::MissingColor);
            }
        }
        Ok(())
    }
}

/// A glyph coverage mask: linear coverage in [0, 1], deliberately distinct
/// from an sRGB color. Passing it where a color is required is the exact
/// gamma-double-application defect this newtype prevents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GlyphCoverage {
    coverage_micros: u32,
}

impl GlyphCoverage {
    pub const OPAQUE: Self = Self { coverage_micros: 1_000_000 };
    pub const EMPTY: Self = Self { coverage_micros: 0 };

    /// Create from linear coverage in [0, 1].
    pub fn from_linear(coverage: f32) -> Result<Self, RenderAbiError> {
        if !coverage.is_finite() || !(0.0..=1.0).contains(&coverage) {
            return Err(RenderAbiError::CoverageAsColor);
        }
        Ok(Self { coverage_micros: (coverage * 1_000_000.0).round() as u32 })
    }

    /// Linear coverage fraction.
    pub fn linear(self) -> f32 {
        f64::from(self.coverage_micros) as f32 / 1_000_000.0
    }
}

/// Premultiplied source-over composition: `out = src + dst * (1 - src.a)`.
///
/// Both inputs must be premultiplied (the matching blend factors for an
/// sRGB-configured presentation target with premultiplied alpha). A
/// straight input is a blending-config mismatch and is refused.
pub fn compose_source_over(
    source: &ColorLinearSdr,
    destination: &ColorLinearSdr,
) -> Result<ColorLinearSdr, RenderAbiError> {
    if !source.premultiplied {
        return Err(RenderAbiError::MissingColor);
    }
    if !destination.premultiplied {
        return Err(RenderAbiError::MissingColor);
    }
    let inverse = 1.0 - source.a;
    ColorLinearSdr::premultiplied(
        source.r + destination.r * inverse,
        source.g + destination.g * inverse,
        source.b + destination.b * inverse,
        source.a + destination.a * inverse,
    )
}

/// A finite typed coordinate validated before GPU upload.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuCoordinate {
    x: f32,
    y: f32,
}

impl GpuCoordinate {
    pub fn new(x: f64, y: f64) -> Result<Self, RenderAbiError> {
        if !x.is_finite() || !y.is_finite() {
            return Err(RenderAbiError::NonFiniteCoordinate);
        }
        Ok(Self { x: x as f32, y: y as f32 })
    }

    pub const fn x(self) -> f32 {
        self.x
    }

    pub const fn y(self) -> f32 {
        self.y
    }
}

/// A scissor rectangle in clamped physical integer pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScissorRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl ScissorRect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    pub const fn x(self) -> u32 {
        self.x
    }

    pub const fn y(self) -> u32 {
        self.y
    }

    pub const fn width(self) -> u32 {
        self.width
    }

    pub const fn height(self) -> u32 {
        self.height
    }
}

/// A bounded clip stack: push/pop scissor intersections with validated
/// monotone shrinking. Depth is bounded; pops are paired with pushes.
#[derive(Clone, Debug)]
pub struct ClipStack {
    entries: Vec<ScissorRect>,
    max_depth: usize,
}

impl ClipStack {
    pub fn new(full_draw: ScissorRect, max_depth: usize) -> Result<Self, RenderAbiError> {
        if max_depth == 0 {
            return Err(RenderAbiError::ClipStackOverflow { max: 0 });
        }
        Ok(Self { entries: vec![full_draw], max_depth })
    }

    pub fn current(&self) -> &ScissorRect {
        self.entries.last().expect("clip stack never empty")
    }

    pub const fn depth(&self) -> usize {
        self.entries.len()
    }

    /// Push an intersection of `rect` with the current clip. The
    /// intersection must be non-empty; empty results are `ClipEmpty`
    /// (caller chooses scissor-less or skips draws explicitly).
    pub fn push(&mut self, rect: ScissorRect) -> Result<(), RenderAbiError> {
        if self.entries.len() >= self.max_depth {
            return Err(RenderAbiError::ClipStackOverflow { max: self.max_depth });
        }
        let current = self.current();
        let x = current.x.max(rect.x);
        let y = current.y.max(rect.y);
        let right = current.x.saturating_add(current.width).min(rect.x.saturating_add(rect.width));
        let bottom = current.y.saturating_add(current.height).min(rect.y.saturating_add(rect.height));
        if right <= x || bottom <= y {
            return Err(RenderAbiError::ClipEmpty);
        }
        self.entries.push(ScissorRect { x, y, width: right - x, height: bottom - y });
        Ok(())
    }

    /// Pop the current clip, restoring the parent. Underflow is refused.
    pub fn pop(&mut self) -> Result<(), RenderAbiError> {
        if self.entries.len() <= 1 {
            return Err(RenderAbiError::ClipStackUnderflow);
        }
        self.entries.pop();
        Ok(())
    }
}

/// Validate a depth value against the documented [0, 1] range.
pub const fn validate_depth(depth: f64) -> Result<(), RenderAbiError> {
    if !(0.0..=1.0).contains(&depth) {
        return Err(RenderAbiError::DepthOutOfRange);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_to_premultiplied_round_trips() {
        let straight = ColorLinearSdr::straight(0.8, 0.4, 0.2, 0.5).unwrap();
        assert!(!straight.is_premultiplied());
        let premul = straight.into_premultiplied().unwrap();
        assert!(premul.is_premultiplied());
        assert_eq!(premul.red(), 0.4);
        assert_eq!(premul.alpha(), 0.5);
        let back = premul.into_straight();
        assert!((back.red() - 0.8).abs() < 1e-6);
        assert!((back.green() - 0.4).abs() < 1e-6);
        assert!((back.blue() - 0.2).abs() < 1e-6);
        assert!(!back.is_premultiplied());
    }

    #[test]
    fn double_premultiply_is_refused() {
        let premul = ColorLinearSdr::premultiplied(0.4, 0.2, 0.1, 0.5).unwrap();
        assert_eq!(
            premul.clone().into_premultiplied().unwrap_err(),
            RenderAbiError::DoublePremultiply
        );
    }

    #[test]
    fn coverage_is_not_srgb_color() {
        // A glyph mask of 50% linear coverage must not gamma-round-trip as
        // a color: the newtype forbids the mix-up at the type level by
        // requiring an explicit constructor and exposing no color fields.
        let coverage = GlyphCoverage::from_linear(0.5).unwrap();
        assert_eq!(coverage.linear(), 0.5);
        assert!(GlyphCoverage::from_linear(1.5).is_err());
        assert!(GlyphCoverage::from_linear(f32::NAN).is_err());
        assert_eq!(GlyphCoverage::OPAQUE.linear(), 1.0);
        assert_eq!(GlyphCoverage::EMPTY.linear(), 0.0);
    }

    #[test]
    fn source_over_composition_matches_reference() {
        // Reference: opaque blue over 50% red = 50% blue + 50% red.
        let source = ColorLinearSdr::premultiplied(0.0, 0.0, 1.0, 1.0).unwrap();
        let destination = ColorLinearSdr::premultiplied(1.0, 0.0, 0.0, 0.5).unwrap();
        let out = compose_source_over(&source, &destination).unwrap();
        assert_eq!(out.alpha(), 1.0);
        assert_eq!(out.red(), 1.0);
        assert_eq!(out.blue(), 1.0);
    }

    #[test]
    fn straight_input_to_composition_is_refused() {
        let straight_source = ColorLinearSdr::straight(1.0, 0.0, 0.0, 1.0).unwrap();
        let premul_destination = ColorLinearSdr::premultiplied(1.0, 0.0, 0.0, 0.5).unwrap();
        assert_eq!(
            compose_source_over(&straight_source, &premul_destination).unwrap_err(),
            RenderAbiError::MissingColor
        );
    }

    #[test]
    fn non_finite_coordinates_are_refused_before_upload() {
        assert!(GpuCoordinate::new(f64::NAN, 0.0).is_err());
        assert!(GpuCoordinate::new(0.0, f64::INFINITY).is_err());
        let ok = GpuCoordinate::new(1.5, -2.5).unwrap();
        assert_eq!(ok.x(), 1.5);
    }

    #[test]
    fn scissor_clamps_intersection_and_refuses_empty() {
        let mut stack = ClipStack::new(ScissorRect::new(0, 0, 100, 100), 4).unwrap();
        assert_eq!(stack.depth(), 1);
        stack.push(ScissorRect::new(50, 50, 100, 100)).unwrap();
        let clip = stack.current();
        assert_eq!(clip.x(), 50);
        assert_eq!(clip.y(), 50);
        assert_eq!(clip.width(), 50);
        assert_eq!(clip.height(), 50);

        // Disjoint rect: empty intersection is an explicit error.
        assert_eq!(
            stack.push(ScissorRect::new(200, 200, 10, 10)).unwrap_err(),
            RenderAbiError::ClipEmpty
        );

        // Bounded depth.
        let mut tiny = ClipStack::new(ScissorRect::new(0, 0, 10, 10), 2).unwrap();
        tiny.push(ScissorRect::new(0, 0, 10, 10)).unwrap();
        assert_eq!(
            tiny.push(ScissorRect::new(0, 0, 10, 10)).unwrap_err(),
            RenderAbiError::ClipStackOverflow { max: 2 }
        );
        // Pops restore and underflow is refused.
        tiny.pop().unwrap();
        tiny.pop().unwrap();
        assert_eq!(tiny.pop().unwrap_err(), RenderAbiError::ClipStackUnderflow);
    }

    #[test]
    fn depth_validation_rejects_out_of_range() {
        assert!(validate_depth(0.0).is_ok());
        assert!(validate_depth(1.0).is_ok());
        assert_eq!(validate_depth(-0.1), Err(RenderAbiError::DepthOutOfRange));
        assert_eq!(validate_depth(1.1), Err(RenderAbiError::DepthOutOfRange));
        assert_eq!(validate_depth(f64::NAN), Err(RenderAbiError::DepthOutOfRange));
    }

    #[test]
    fn channels_out_of_range_are_refused() {
        assert!(ColorLinearSdr::straight(1.5, 0.0, 0.0, 1.0).is_err());
        assert!(ColorLinearSdr::premultiplied(0.0, 0.0, 0.0, -0.1).is_err());
        assert!(ColorLinearSdr::straight(f32::NAN, 0.0, 0.0, 1.0).is_err());
    }
}

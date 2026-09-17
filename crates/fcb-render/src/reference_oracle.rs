//! CPU reference compositing engine and defect-detection oracles (FCB-089.B, fcb-npyi.2).
//!
//! Implements the package oracle required by the FCB-089 contract:
//! - Reference linear-space SDR composition and coverage application.
//! - Detects gamma-decoding of linear glyph coverage masks (gamma defect).
//! - Detects double-application of alpha / re-premultiplication (double alpha defect).
//! - Detects clip-stack violations and depth inversions (clip/depth flip defect).
//! - Detects incompatible target reuse and stale drawable lease access.

use crate::{
    host_target::{RenderTargetDescriptor, TargetLeaseState},
    ColorLinearSdr, GlyphCoverage, RenderAbiError, ScissorRect,
};
use std::fmt;

/// Specific rendering or ABI defects detected by the reference oracle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OracleDefect {
    /// A linear glyph coverage mask was gamma-decoded (e.g. sRGB curve applied to coverage).
    GammaDecodedLinearCoverage {
        expected_linear_bits: u32,
        actual_bits: u32,
    },
    /// A color had alpha multiplied a second time (double premultiply).
    DoublePremultiplyDetected {
        expected_r_bits: u32,
        actual_r_bits: u32,
    },
    /// Pixels were rendered outside the active scissor clip rectangle.
    ScissorClipViolation {
        pixel_x: u32,
        pixel_y: u32,
        clip_x: u32,
        clip_y: u32,
        clip_w: u32,
        clip_h: u32,
    },
    /// Depth ordering or layer hierarchy was inverted.
    DepthOrderFlipped {
        foreground_depth_bits: u32,
        background_depth_bits: u32,
    },
    /// Incompatible target reuse: attempted to render to an unacquired or already presented target.
    IncompatibleTargetReuse { state: TargetLeaseState },
}

impl fmt::Display for OracleDefect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GammaDecodedLinearCoverage {
                expected_linear_bits,
                actual_bits,
            } => {
                write!(
                    f,
                    "gamma defect detected: linear coverage (0x{expected_linear_bits:08x}) was altered by gamma decoding (0x{actual_bits:08x})"
                )
            }
            Self::DoublePremultiplyDetected {
                expected_r_bits,
                actual_r_bits,
            } => {
                write!(
                    f,
                    "double premultiply defect: expected color component 0x{expected_r_bits:08x}, found re-premultiplied 0x{actual_r_bits:08x}"
                )
            }
            Self::ScissorClipViolation {
                pixel_x,
                pixel_y,
                clip_x,
                clip_y,
                clip_w,
                clip_h,
            } => {
                write!(
                    f,
                    "clip violation: pixel ({pixel_x}, {pixel_y}) falls outside scissor [{clip_x}, {clip_y}, {clip_w}, {clip_h}]"
                )
            }
            Self::DepthOrderFlipped {
                foreground_depth_bits,
                background_depth_bits,
            } => {
                write!(
                    f,
                    "depth flip defect: foreground depth 0x{foreground_depth_bits:08x} inverted relative to background 0x{background_depth_bits:08x}"
                )
            }
            Self::IncompatibleTargetReuse { state } => {
                write!(
                    f,
                    "incompatible target reuse defect: target in lease state {state:?}"
                )
            }
        }
    }
}

impl std::error::Error for OracleDefect {}

/// A reference CPU pixel with 4 premultiplied linear f32 channels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReferencePixel {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl ReferencePixel {
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    pub fn from_color(c: ColorLinearSdr) -> Result<Self, RenderAbiError> {
        let premul = if c.is_premultiplied() {
            c
        } else {
            c.into_premultiplied()?
        };
        Ok(Self {
            r: premul.red(),
            g: premul.green(),
            b: premul.blue(),
            a: premul.alpha(),
        })
    }
}

/// Pure reference linear SDR compositor.
pub struct CpuReferenceCompositor;

impl CpuReferenceCompositor {
    /// Blend `source` over `destination` using premultiplied linear math:
    /// `out = src + dst * (1 - src.a)`.
    pub fn blend_source_over(src: ReferencePixel, dst: ReferencePixel) -> ReferencePixel {
        let inv_a = 1.0 - src.a;
        ReferencePixel {
            r: src.r + dst.r * inv_a,
            g: src.g + dst.g * inv_a,
            b: src.b + dst.b * inv_a,
            a: src.a + dst.a * inv_a,
        }
    }

    /// Apply linear grayscale glyph coverage to a text color:
    /// `out = text_color * coverage`.
    /// Note: Coverage is linear and must NEVER be gamma decoded.
    pub fn apply_glyph_coverage(
        text_color: ReferencePixel,
        coverage: GlyphCoverage,
    ) -> ReferencePixel {
        let cov = coverage.linear();
        ReferencePixel {
            r: text_color.r * cov,
            g: text_color.g * cov,
            b: text_color.b * cov,
            a: text_color.a * cov,
        }
    }
}

/// Verification oracle testing render pipelines against CPU reference truth.
pub struct ReferenceOracle;

impl ReferenceOracle {
    /// Verify that glyph coverage was treated as linear coverage, not sRGB gamma decoded.
    pub fn verify_glyph_coverage(
        linear_coverage: GlyphCoverage,
        actual_output: f32,
        tolerance: f32,
    ) -> Result<(), OracleDefect> {
        let expected = linear_coverage.linear();
        // If sRGB decoding (approx c^2.2) was erroneously applied, the difference will be large.
        if (expected - actual_output).abs() > tolerance {
            return Err(OracleDefect::GammaDecodedLinearCoverage {
                expected_linear_bits: expected.to_bits(),
                actual_bits: actual_output.to_bits(),
            });
        }
        Ok(())
    }

    /// Verify that color was premultiplied exactly once (no second multiplication by alpha).
    pub fn verify_no_double_premultiply(
        expected_premul: ColorLinearSdr,
        actual: ColorLinearSdr,
        tolerance: f32,
    ) -> Result<(), OracleDefect> {
        let exp_r = expected_premul.red();
        let act_r = actual.red();
        if (exp_r - act_r).abs() > tolerance {
            return Err(OracleDefect::DoublePremultiplyDetected {
                expected_r_bits: exp_r.to_bits(),
                actual_r_bits: act_r.to_bits(),
            });
        }
        Ok(())
    }

    /// Verify that drawing respected the active scissor rectangle.
    pub fn verify_scissor_clip(
        pixel_x: u32,
        pixel_y: u32,
        scissor: &ScissorRect,
    ) -> Result<(), OracleDefect> {
        let sx = scissor.x();
        let sy = scissor.y();
        let sw = scissor.width();
        let sh = scissor.height();
        if pixel_x < sx || pixel_y < sy || pixel_x >= (sx + sw) || pixel_y >= (sy + sh) {
            return Err(OracleDefect::ScissorClipViolation {
                pixel_x,
                pixel_y,
                clip_x: sx,
                clip_y: sy,
                clip_w: sw,
                clip_h: sh,
            });
        }
        Ok(())
    }

    /// Verify that depth values preserve the required depth test invariant.
    pub fn verify_depth_order(
        foreground_depth: f32,
        background_depth: f32,
    ) -> Result<(), OracleDefect> {
        // In LessEqual depth testing, foreground MUST be <= background.
        if foreground_depth > background_depth {
            return Err(OracleDefect::DepthOrderFlipped {
                foreground_depth_bits: foreground_depth.to_bits(),
                background_depth_bits: background_depth.to_bits(),
            });
        }
        Ok(())
    }

    /// Verify that a target descriptor is in a valid state for command encoding.
    pub fn verify_target_lease(
        target: &RenderTargetDescriptor,
    ) -> Result<(), OracleDefect> {
        if target.state() != TargetLeaseState::Acquired {
            return Err(OracleDefect::IncompatibleTargetReuse {
                state: target.state(),
            });
        }
        Ok(())
    }
}

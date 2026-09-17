//! Semantic transparent ordering and depth policy contracts (FCB-089.B, fcb-npyi.2).
//!
//! Enforces plan §14.10:
//! - Transparent City overlays retain semantic order and depth policy.
//! - Changing projection (2D orthographic to 3D perspective City) must not reverse
//!   front/back interpretation or make hidden surfaces clickable.
//! - Opaque occluders correctly occlude underlying layers and mask hit-testability.
//! - Detect depth inversions and projection flips with explicit typed errors.

use std::fmt;

/// Errors detected during layer sorting and semantic order validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticOrderError {
    /// Depth value outside [0.0, 1.0] or non-finite.
    InvalidDepth { id: u64, depth_bits: u32 },
    /// Depth values violate the active depth policy (e.g. depth inversion).
    DepthInversion {
        foreground_id: u64,
        foreground_depth_bits: u32,
        background_id: u64,
        background_depth_bits: u32,
    },
    /// A hidden/occluded surface is marked as interactive/clickable.
    HiddenSurfaceClickable {
        occluded_id: u64,
        occluder_id: u64,
    },
    /// Projection transition altered the relative front-to-back order of transparent overlays.
    ProjectionOrderFlip {
        layer_a: u64,
        layer_b: u64,
    },
}

impl fmt::Display for SemanticOrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDepth { id, depth_bits } => {
                write!(
                    f,
                    "layer {id} has invalid depth: 0x{depth_bits:08x}"
                )
            }
            Self::DepthInversion {
                foreground_id,
                foreground_depth_bits,
                background_id,
                background_depth_bits,
            } => {
                write!(
                    f,
                    "depth inversion detected: foreground {foreground_id} (depth 0x{foreground_depth_bits:08x}) is behind background {background_id} (depth 0x{background_depth_bits:08x})"
                )
            }
            Self::HiddenSurfaceClickable {
                occluded_id,
                occluder_id,
            } => {
                write!(
                    f,
                    "hidden surface violation: layer {occluded_id} is completely occluded by opaque layer {occluder_id} but remains interactive"
                )
            }
            Self::ProjectionOrderFlip { layer_a, layer_b } => {
                write!(
                    f,
                    "projection order flip: layers {layer_a} and {layer_b} changed relative front/back order across projection transition"
                )
            }
        }
    }
}

impl std::error::Error for SemanticOrderError {}

/// The projection mode used for rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionMode {
    /// Standard 2D orthographic rendering (document, source viewer, atlas).
    Orthographic2D,
    /// 3D perspective City mode with extruded building geometry.
    PerspectiveCity,
}

/// The depth testing policy governing layer composition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepthPolicy {
    /// 2D painter's order: subsequent layers paint over earlier layers.
    PainterBackToFront,
    /// Standard GPU depth test: smaller depth values in [0.0, 1.0] are closer to camera.
    DepthTestLessEqual,
}

/// A transparent or opaque layer participating in composite rendering and hit testing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderLayer {
    pub id: u64,
    /// Relative sorting index (lower values are logically behind higher values).
    pub z_index: i32,
    /// Normalized device coordinate depth in [0.0, 1.0].
    pub depth: f32,
    /// Bounding rectangle in logical coordinates: (x, y, width, height).
    pub bounds: (f32, f32, f32, f32),
    /// Alpha opacity in [0.0, 1.0].
    pub alpha: f32,
    /// True if alpha == 1.0 and fully covers bounds.
    pub is_opaque: bool,
    /// True if this layer accepts pointer / click events.
    pub is_interactive: bool,
}

impl RenderLayer {
    pub fn new(
        id: u64,
        z_index: i32,
        depth: f32,
        bounds: (f32, f32, f32, f32),
        alpha: f32,
        is_interactive: bool,
    ) -> Result<Self, SemanticOrderError> {
        if !depth.is_finite() || !(0.0..=1.0).contains(&depth) {
            return Err(SemanticOrderError::InvalidDepth {
                id,
                depth_bits: depth.to_bits(),
            });
        }
        let clamped_alpha = alpha.clamp(0.0, 1.0);
        let is_opaque = (clamped_alpha - 1.0).abs() < 1e-5;
        Ok(Self {
            id,
            z_index,
            depth,
            bounds,
            alpha: clamped_alpha,
            is_opaque,
            is_interactive,
        })
    }

    /// Returns true if this layer completely encloses and covers `other`.
    pub fn completely_occludes(&self, other: &RenderLayer) -> bool {
        if !self.is_opaque {
            return false;
        }
        let (sx, sy, sw, sh) = self.bounds;
        let (ox, oy, ow, oh) = other.bounds;
        sx <= ox && sy <= oy && (sx + sw) >= (ox + ow) && (sy + sh) >= (oy + oh)
    }
}

/// Validate that a set of layers satisfies semantic transparent ordering and depth policies.
pub fn validate_semantic_order(
    layers: &[RenderLayer],
    policy: DepthPolicy,
) -> Result<(), SemanticOrderError> {
    for i in 0..layers.len() {
        let current = &layers[i];
        if !current.depth.is_finite() || !(0.0..=1.0).contains(&current.depth) {
            return Err(SemanticOrderError::InvalidDepth {
                id: current.id,
                depth_bits: current.depth.to_bits(),
            });
        }

        for j in (i + 1)..layers.len() {
            let later = &layers[j];
            match policy {
                DepthPolicy::PainterBackToFront => {
                    // In back-to-front painter's order, later layers in the list
                    // are in front of earlier layers, so their z_index should be >= current.z_index.
                    if current.z_index > later.z_index {
                        return Err(SemanticOrderError::DepthInversion {
                            foreground_id: current.id,
                            foreground_depth_bits: current.depth.to_bits(),
                            background_id: later.id,
                            background_depth_bits: later.depth.to_bits(),
                        });
                    }
                }
                DepthPolicy::DepthTestLessEqual => {
                    // In LessEqual depth testing, closer layers have smaller depth.
                    // If current is supposed to be behind later, current.depth must be >= later.depth.
                    if current.z_index < later.z_index && current.depth < later.depth {
                        return Err(SemanticOrderError::DepthInversion {
                            foreground_id: later.id,
                            foreground_depth_bits: later.depth.to_bits(),
                            background_id: current.id,
                            background_depth_bits: current.depth.to_bits(),
                        });
                    }
                }
            }

            // Occlusion check: if `later` is in front of `current`, is opaque,
            // and completely encloses `current`, `current` must NOT be interactive.
            if later.z_index > current.z_index
                && later.completely_occludes(current)
                && current.is_interactive
            {
                return Err(SemanticOrderError::HiddenSurfaceClickable {
                    occluded_id: current.id,
                    occluder_id: later.id,
                });
            }
        }
    }
    Ok(())
}

/// Verify that switching projection mode (2D -> City or vice-versa) does not reverse
/// the relative front/back order of any pair of visible transparent layers.
pub fn verify_projection_stability(
    layers_2d: &[RenderLayer],
    layers_city: &[RenderLayer],
) -> Result<(), SemanticOrderError> {
    for a in layers_2d {
        for b in layers_2d {
            if a.id == b.id {
                continue;
            }
            let a_ahead_in_2d = a.z_index > b.z_index;

            // Find corresponding layers in city mode.
            let city_a = layers_city.iter().find(|l| l.id == a.id);
            let city_b = layers_city.iter().find(|l| l.id == b.id);

            if let (Some(ca), Some(cb)) = (city_a, city_b) {
                // In City mode with DepthTestLessEqual, closer layer has smaller depth.
                let a_ahead_in_city = ca.depth < cb.depth;
                if a_ahead_in_2d != a_ahead_in_city {
                    return Err(SemanticOrderError::ProjectionOrderFlip {
                        layer_a: a.id,
                        layer_b: b.id,
                    });
                }
            }
        }
    }
    Ok(())
}

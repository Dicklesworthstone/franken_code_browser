//! Explicit shader serialization and ABI layout contracts (FCB-089.B, fcb-npyi.2).
//!
//! Enforces plan §14.6:
//! - No `bytemuck`, unchecked struct casts, or unaligned reference creation.
//! - Serialize GPU records with explicit field encoders into preallocated byte buffers.
//! - The authoritative representation remains safe Rust.
//! - Record field offsets, padding, alignment, scalar widths, coordinate units, and endianness
//!   for every shader-visible record.
//! - Validate counts, bounds, finite coordinates, premultiplied colors, and depth values up front.

use crate::{ColorLinearSdr, GlyphCoverage};
use std::fmt;

#[inline]
fn read_f32_le(src: &[u8], offset: usize) -> f32 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&src[offset..offset + 4]);
    f32::from_le_bytes(buf)
}

#[inline]
fn read_u32_le(src: &[u8], offset: usize) -> u32 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&src[offset..offset + 4]);
    u32::from_le_bytes(buf)
}

/// Errors arising from shader ABI serialization or buffer layout validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShaderAbiError {
    /// Provided destination buffer is smaller than the required record size.
    BufferTooSmall { required: usize, provided: usize },
    /// Destination buffer is not aligned to the required boundary.
    UnalignedBuffer { offset: usize, required_alignment: usize },
    /// Record count exceeds shader-allocated uniform/storage buffer bounds.
    RecordCountExceedsCapacity { count: usize, max: usize },
    /// A coordinate or dimension was non-finite (NaN or Inf).
    NonFiniteCoordinate,
    /// Depth was outside [0.0, 1.0] or non-finite.
    InvalidDepth,
    /// Color was not premultiplied linear SDR.
    ColorNotPremultiplied,
    /// Linear coverage mask outside [0.0, 1.0].
    InvalidCoverage,
    /// Truncated byte slice during decoding.
    UnexpectedEndOfBuffer { expected: usize, found: usize },
}

impl fmt::Display for ShaderAbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooSmall { required, provided } => {
                write!(
                    f,
                    "buffer too small: required {required} bytes, provided {provided}"
                )
            }
            Self::UnalignedBuffer {
                offset,
                required_alignment,
            } => {
                write!(
                    f,
                    "buffer offset {offset} is not aligned to required {required_alignment} bytes"
                )
            }
            Self::RecordCountExceedsCapacity { count, max } => {
                write!(
                    f,
                    "record count {count} exceeds maximum buffer capacity {max}"
                )
            }
            Self::NonFiniteCoordinate => {
                f.write_str("non-finite coordinate rejected before GPU upload")
            }
            Self::InvalidDepth => f.write_str("depth value must be in [0.0, 1.0]"),
            Self::ColorNotPremultiplied => {
                f.write_str("color must be premultiplied linear SDR for shader upload")
            }
            Self::InvalidCoverage => {
                f.write_str("glyph coverage must be in [0.0, 1.0]")
            }
            Self::UnexpectedEndOfBuffer { expected, found } => {
                write!(
                    f,
                    "unexpected end of buffer: expected {expected} bytes, found {found}"
                )
            }
        }
    }
}

impl std::error::Error for ShaderAbiError {}

/// Metadata describing a single field within a shader-visible record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldMetadata {
    pub name: &'static str,
    pub offset: usize,
    pub size: usize,
    pub type_desc: &'static str,
}

// -------------------------------------------------------------------------
// SolidRect Record (48 bytes, 16-byte aligned)
// -------------------------------------------------------------------------

/// Explicit GPU solid rectangle record layout matching MSL shader struct:
/// ```metal
/// struct SolidRectInstance {
///     float4 bounds; // x, y, width, height (offset 0..16)
///     float4 color;  // premultiplied linear SDR (offset 16..32)
///     uint clip_index; // offset 32..36
///     float depth;     // offset 36..40
///     uint pad[2];     // offset 40..48 (16-byte alignment)
/// };
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuSolidRectRecord {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color_r: f32,
    pub color_g: f32,
    pub color_b: f32,
    pub color_a: f32,
    pub clip_index: u32,
    pub depth: f32,
}

impl GpuSolidRectRecord {
    pub const RECORD_SIZE: usize = 48;
    pub const RECORD_ALIGNMENT: usize = 16;

    pub const FIELDS: &'static [FieldMetadata] = &[
        FieldMetadata {
            name: "bounds.x",
            offset: 0,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "bounds.y",
            offset: 4,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "bounds.width",
            offset: 8,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "bounds.height",
            offset: 12,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "color.r",
            offset: 16,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "color.g",
            offset: 20,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "color.b",
            offset: 24,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "color.a",
            offset: 28,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "clip_index",
            offset: 32,
            size: 4,
            type_desc: "uint",
        },
        FieldMetadata {
            name: "depth",
            offset: 36,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "pad[0]",
            offset: 40,
            size: 4,
            type_desc: "uint (padding)",
        },
        FieldMetadata {
            name: "pad[1]",
            offset: 44,
            size: 4,
            type_desc: "uint (padding)",
        },
    ];

    pub fn new(
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: ColorLinearSdr,
        clip_index: u32,
        depth: f32,
    ) -> Result<Self, ShaderAbiError> {
        if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
            return Err(ShaderAbiError::NonFiniteCoordinate);
        }
        if !depth.is_finite() || !(0.0..=1.0).contains(&depth) {
            return Err(ShaderAbiError::InvalidDepth);
        }
        if !color.is_premultiplied() {
            return Err(ShaderAbiError::ColorNotPremultiplied);
        }
        Ok(Self {
            x,
            y,
            width,
            height,
            color_r: color.red(),
            color_g: color.green(),
            color_b: color.blue(),
            color_a: color.alpha(),
            clip_index,
            depth,
        })
    }

    /// Encode into a preallocated byte slice using safe explicit Little-Endian encoders.
    pub fn encode(&self, dst: &mut [u8]) -> Result<usize, ShaderAbiError> {
        if dst.len() < Self::RECORD_SIZE {
            return Err(ShaderAbiError::BufferTooSmall {
                required: Self::RECORD_SIZE,
                provided: dst.len(),
            });
        }
        dst[0..4].copy_from_slice(&self.x.to_le_bytes());
        dst[4..8].copy_from_slice(&self.y.to_le_bytes());
        dst[8..12].copy_from_slice(&self.width.to_le_bytes());
        dst[12..16].copy_from_slice(&self.height.to_le_bytes());
        dst[16..20].copy_from_slice(&self.color_r.to_le_bytes());
        dst[20..24].copy_from_slice(&self.color_g.to_le_bytes());
        dst[24..28].copy_from_slice(&self.color_b.to_le_bytes());
        dst[28..32].copy_from_slice(&self.color_a.to_le_bytes());
        dst[32..36].copy_from_slice(&self.clip_index.to_le_bytes());
        dst[36..40].copy_from_slice(&self.depth.to_le_bytes());
        dst[40..44].copy_from_slice(&0_u32.to_le_bytes());
        dst[44..48].copy_from_slice(&0_u32.to_le_bytes());
        Ok(Self::RECORD_SIZE)
    }

    /// Decode from byte slice with strict Little-Endian conversion and validation.
    pub fn decode(src: &[u8]) -> Result<Self, ShaderAbiError> {
        if src.len() < Self::RECORD_SIZE {
            return Err(ShaderAbiError::UnexpectedEndOfBuffer {
                expected: Self::RECORD_SIZE,
                found: src.len(),
            });
        }
        let x = read_f32_le(src, 0);
        let y = read_f32_le(src, 4);
        let width = read_f32_le(src, 8);
        let height = read_f32_le(src, 12);
        let color_r = read_f32_le(src, 16);
        let color_g = read_f32_le(src, 20);
        let color_b = read_f32_le(src, 24);
        let color_a = read_f32_le(src, 28);
        let clip_index = read_u32_le(src, 32);
        let depth = read_f32_le(src, 36);

        if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
            return Err(ShaderAbiError::NonFiniteCoordinate);
        }
        if !depth.is_finite() || !(0.0..=1.0).contains(&depth) {
            return Err(ShaderAbiError::InvalidDepth);
        }

        Ok(Self {
            x,
            y,
            width,
            height,
            color_r,
            color_g,
            color_b,
            color_a,
            clip_index,
            depth,
        })
    }
}

// -------------------------------------------------------------------------
// Glyph Record (64 bytes, 16-byte aligned)
// -------------------------------------------------------------------------

/// Explicit GPU glyph record layout matching MSL shader struct:
/// ```metal
/// struct GlyphInstance {
///     float4 dest_bounds; // dest_x, dest_y, dest_w, dest_h (offset 0..16)
///     float4 uv_bounds;   // uv_x, uv_y, uv_w, uv_h (offset 16..32)
///     float4 color;       // premultiplied linear SDR (offset 32..48)
///     float coverage;     // linear coverage [0..1] (offset 48..52)
///     uint clip_index;    // offset 52..56
///     float depth;        // offset 56..60
///     uint pad;           // offset 60..64 (16-byte alignment)
/// };
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuGlyphRecord {
    pub dest_x: f32,
    pub dest_y: f32,
    pub dest_w: f32,
    pub dest_h: f32,
    pub uv_x: f32,
    pub uv_y: f32,
    pub uv_w: f32,
    pub uv_h: f32,
    pub color_r: f32,
    pub color_g: f32,
    pub color_b: f32,
    pub color_a: f32,
    pub coverage_linear: f32,
    pub clip_index: u32,
    pub depth: f32,
}

impl GpuGlyphRecord {
    pub const RECORD_SIZE: usize = 64;
    pub const RECORD_ALIGNMENT: usize = 16;

    pub const FIELDS: &'static [FieldMetadata] = &[
        FieldMetadata {
            name: "dest_bounds.x",
            offset: 0,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "dest_bounds.y",
            offset: 4,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "dest_bounds.width",
            offset: 8,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "dest_bounds.height",
            offset: 12,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "uv_bounds.x",
            offset: 16,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "uv_bounds.y",
            offset: 20,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "uv_bounds.width",
            offset: 24,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "uv_bounds.height",
            offset: 28,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "color.r",
            offset: 32,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "color.g",
            offset: 36,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "color.b",
            offset: 40,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "color.a",
            offset: 44,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "coverage_linear",
            offset: 48,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "clip_index",
            offset: 52,
            size: 4,
            type_desc: "uint",
        },
        FieldMetadata {
            name: "depth",
            offset: 56,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "pad",
            offset: 60,
            size: 4,
            type_desc: "uint (padding)",
        },
    ];

    pub fn new(
        dest: (f32, f32, f32, f32),
        uv: (f32, f32, f32, f32),
        color: ColorLinearSdr,
        coverage: GlyphCoverage,
        clip_index: u32,
        depth: f32,
    ) -> Result<Self, ShaderAbiError> {
        let (dest_x, dest_y, dest_w, dest_h) = dest;
        let (uv_x, uv_y, uv_w, uv_h) = uv;
        if !dest_x.is_finite()
            || !dest_y.is_finite()
            || !dest_w.is_finite()
            || !dest_h.is_finite()
            || !uv_x.is_finite()
            || !uv_y.is_finite()
            || !uv_w.is_finite()
            || !uv_h.is_finite()
        {
            return Err(ShaderAbiError::NonFiniteCoordinate);
        }
        if !depth.is_finite() || !(0.0..=1.0).contains(&depth) {
            return Err(ShaderAbiError::InvalidDepth);
        }
        if !color.is_premultiplied() {
            return Err(ShaderAbiError::ColorNotPremultiplied);
        }
        let coverage_linear = coverage.linear();
        if !coverage_linear.is_finite() || !(0.0..=1.0).contains(&coverage_linear) {
            return Err(ShaderAbiError::InvalidCoverage);
        }
        Ok(Self {
            dest_x,
            dest_y,
            dest_w,
            dest_h,
            uv_x,
            uv_y,
            uv_w,
            uv_h,
            color_r: color.red(),
            color_g: color.green(),
            color_b: color.blue(),
            color_a: color.alpha(),
            coverage_linear,
            clip_index,
            depth,
        })
    }

    pub fn encode(&self, dst: &mut [u8]) -> Result<usize, ShaderAbiError> {
        if dst.len() < Self::RECORD_SIZE {
            return Err(ShaderAbiError::BufferTooSmall {
                required: Self::RECORD_SIZE,
                provided: dst.len(),
            });
        }
        dst[0..4].copy_from_slice(&self.dest_x.to_le_bytes());
        dst[4..8].copy_from_slice(&self.dest_y.to_le_bytes());
        dst[8..12].copy_from_slice(&self.dest_w.to_le_bytes());
        dst[12..16].copy_from_slice(&self.dest_h.to_le_bytes());
        dst[16..20].copy_from_slice(&self.uv_x.to_le_bytes());
        dst[20..24].copy_from_slice(&self.uv_y.to_le_bytes());
        dst[24..28].copy_from_slice(&self.uv_w.to_le_bytes());
        dst[28..32].copy_from_slice(&self.uv_h.to_le_bytes());
        dst[32..36].copy_from_slice(&self.color_r.to_le_bytes());
        dst[36..40].copy_from_slice(&self.color_g.to_le_bytes());
        dst[40..44].copy_from_slice(&self.color_b.to_le_bytes());
        dst[44..48].copy_from_slice(&self.color_a.to_le_bytes());
        dst[48..52].copy_from_slice(&self.coverage_linear.to_le_bytes());
        dst[52..56].copy_from_slice(&self.clip_index.to_le_bytes());
        dst[56..60].copy_from_slice(&self.depth.to_le_bytes());
        dst[60..64].copy_from_slice(&0_u32.to_le_bytes());
        Ok(Self::RECORD_SIZE)
    }

    pub fn decode(src: &[u8]) -> Result<Self, ShaderAbiError> {
        if src.len() < Self::RECORD_SIZE {
            return Err(ShaderAbiError::UnexpectedEndOfBuffer {
                expected: Self::RECORD_SIZE,
                found: src.len(),
            });
        }
        let dest_x = read_f32_le(src, 0);
        let dest_y = read_f32_le(src, 4);
        let dest_w = read_f32_le(src, 8);
        let dest_h = read_f32_le(src, 12);
        let uv_x = read_f32_le(src, 16);
        let uv_y = read_f32_le(src, 20);
        let uv_w = read_f32_le(src, 24);
        let uv_h = read_f32_le(src, 28);
        let color_r = read_f32_le(src, 32);
        let color_g = read_f32_le(src, 36);
        let color_b = read_f32_le(src, 40);
        let color_a = read_f32_le(src, 44);
        let coverage_linear = read_f32_le(src, 48);
        let clip_index = read_u32_le(src, 52);
        let depth = read_f32_le(src, 56);

        if !dest_x.is_finite()
            || !dest_y.is_finite()
            || !dest_w.is_finite()
            || !dest_h.is_finite()
            || !uv_x.is_finite()
            || !uv_y.is_finite()
            || !uv_w.is_finite()
            || !uv_h.is_finite()
        {
            return Err(ShaderAbiError::NonFiniteCoordinate);
        }
        if !depth.is_finite() || !(0.0..=1.0).contains(&depth) {
            return Err(ShaderAbiError::InvalidDepth);
        }
        if !coverage_linear.is_finite() || !(0.0..=1.0).contains(&coverage_linear) {
            return Err(ShaderAbiError::InvalidCoverage);
        }

        Ok(Self {
            dest_x,
            dest_y,
            dest_w,
            dest_h,
            uv_x,
            uv_y,
            uv_w,
            uv_h,
            color_r,
            color_g,
            color_b,
            color_a,
            coverage_linear,
            clip_index,
            depth,
        })
    }
}

// -------------------------------------------------------------------------
// Clip Scissor Record (16 bytes, 16-byte aligned)
// -------------------------------------------------------------------------

/// Explicit GPU scissor record layout:
/// ```metal
/// struct ScissorRect {
///     uint x;
///     uint y;
///     uint width;
///     uint height;
/// };
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GpuScissorRecord {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl GpuScissorRecord {
    pub const RECORD_SIZE: usize = 16;
    pub const RECORD_ALIGNMENT: usize = 16;

    pub const FIELDS: &'static [FieldMetadata] = &[
        FieldMetadata {
            name: "x",
            offset: 0,
            size: 4,
            type_desc: "uint",
        },
        FieldMetadata {
            name: "y",
            offset: 4,
            size: 4,
            type_desc: "uint",
        },
        FieldMetadata {
            name: "width",
            offset: 8,
            size: 4,
            type_desc: "uint",
        },
        FieldMetadata {
            name: "height",
            offset: 12,
            size: 4,
            type_desc: "uint",
        },
    ];

    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    pub fn encode(&self, dst: &mut [u8]) -> Result<usize, ShaderAbiError> {
        if dst.len() < Self::RECORD_SIZE {
            return Err(ShaderAbiError::BufferTooSmall {
                required: Self::RECORD_SIZE,
                provided: dst.len(),
            });
        }
        dst[0..4].copy_from_slice(&self.x.to_le_bytes());
        dst[4..8].copy_from_slice(&self.y.to_le_bytes());
        dst[8..12].copy_from_slice(&self.width.to_le_bytes());
        dst[12..16].copy_from_slice(&self.height.to_le_bytes());
        Ok(Self::RECORD_SIZE)
    }

    pub fn decode(src: &[u8]) -> Result<Self, ShaderAbiError> {
        if src.len() < Self::RECORD_SIZE {
            return Err(ShaderAbiError::UnexpectedEndOfBuffer {
                expected: Self::RECORD_SIZE,
                found: src.len(),
            });
        }
        let x = read_u32_le(src, 0);
        let y = read_u32_le(src, 4);
        let width = read_u32_le(src, 8);
        let height = read_u32_le(src, 12);
        Ok(Self { x, y, width, height })
    }
}

// -------------------------------------------------------------------------
// Frame Uniforms Record (32 bytes, 16-byte aligned)
// -------------------------------------------------------------------------

/// Frame uniform layout:
/// ```metal
/// struct FrameUniforms {
///     float2 viewport_size; // width, height (offset 0..8)
///     float scale_factor;   // offset 8..12
///     uint clip_count;      // offset 12..16
///     uint color_space_tag; // offset 16..20 (0=sRGB, 1=DisplayP3)
///     uint projection_mode; // offset 20..24 (0=2D, 1=City3D)
///     uint pad[2];          // offset 24..32
/// };
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuFrameUniforms {
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub scale_factor: f32,
    pub clip_count: u32,
    pub color_space_tag: u32,
    pub projection_mode: u32,
}

impl GpuFrameUniforms {
    pub const RECORD_SIZE: usize = 32;
    pub const RECORD_ALIGNMENT: usize = 16;

    pub const FIELDS: &'static [FieldMetadata] = &[
        FieldMetadata {
            name: "viewport_size.x",
            offset: 0,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "viewport_size.y",
            offset: 4,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "scale_factor",
            offset: 8,
            size: 4,
            type_desc: "float",
        },
        FieldMetadata {
            name: "clip_count",
            offset: 12,
            size: 4,
            type_desc: "uint",
        },
        FieldMetadata {
            name: "color_space_tag",
            offset: 16,
            size: 4,
            type_desc: "uint",
        },
        FieldMetadata {
            name: "projection_mode",
            offset: 20,
            size: 4,
            type_desc: "uint",
        },
        FieldMetadata {
            name: "pad[0]",
            offset: 24,
            size: 4,
            type_desc: "uint (padding)",
        },
        FieldMetadata {
            name: "pad[1]",
            offset: 28,
            size: 4,
            type_desc: "uint (padding)",
        },
    ];

    pub fn new(
        viewport_width: f32,
        viewport_height: f32,
        scale_factor: f32,
        clip_count: u32,
        color_space_tag: u32,
        projection_mode: u32,
    ) -> Result<Self, ShaderAbiError> {
        if !viewport_width.is_finite()
            || !viewport_height.is_finite()
            || !scale_factor.is_finite()
            || viewport_width < 0.0
            || viewport_height < 0.0
            || scale_factor <= 0.0
        {
            return Err(ShaderAbiError::NonFiniteCoordinate);
        }
        Ok(Self {
            viewport_width,
            viewport_height,
            scale_factor,
            clip_count,
            color_space_tag,
            projection_mode,
        })
    }

    pub fn encode(&self, dst: &mut [u8]) -> Result<usize, ShaderAbiError> {
        if dst.len() < Self::RECORD_SIZE {
            return Err(ShaderAbiError::BufferTooSmall {
                required: Self::RECORD_SIZE,
                provided: dst.len(),
            });
        }
        dst[0..4].copy_from_slice(&self.viewport_width.to_le_bytes());
        dst[4..8].copy_from_slice(&self.viewport_height.to_le_bytes());
        dst[8..12].copy_from_slice(&self.scale_factor.to_le_bytes());
        dst[12..16].copy_from_slice(&self.clip_count.to_le_bytes());
        dst[16..20].copy_from_slice(&self.color_space_tag.to_le_bytes());
        dst[20..24].copy_from_slice(&self.projection_mode.to_le_bytes());
        dst[24..28].copy_from_slice(&0_u32.to_le_bytes());
        dst[28..32].copy_from_slice(&0_u32.to_le_bytes());
        Ok(Self::RECORD_SIZE)
    }

    pub fn decode(src: &[u8]) -> Result<Self, ShaderAbiError> {
        if src.len() < Self::RECORD_SIZE {
            return Err(ShaderAbiError::UnexpectedEndOfBuffer {
                expected: Self::RECORD_SIZE,
                found: src.len(),
            });
        }
        let viewport_width = read_f32_le(src, 0);
        let viewport_height = read_f32_le(src, 4);
        let scale_factor = read_f32_le(src, 8);
        let clip_count = read_u32_le(src, 12);
        let color_space_tag = read_u32_le(src, 16);
        let projection_mode = read_u32_le(src, 20);

        if !viewport_width.is_finite()
            || !viewport_height.is_finite()
            || !scale_factor.is_finite()
        {
            return Err(ShaderAbiError::NonFiniteCoordinate);
        }

        Ok(Self {
            viewport_width,
            viewport_height,
            scale_factor,
            clip_count,
            color_space_tag,
            projection_mode,
        })
    }
}

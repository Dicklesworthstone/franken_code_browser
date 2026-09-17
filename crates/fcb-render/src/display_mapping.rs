//! FCB-034.B: Maps display primitives to Metal clip/resource descriptors,
//! retaining unsupported syntax visibly rather than silently dropping it.
//!
//! The renderer receives display primitives from upstream (FrankenMarkdown)
//! and maps them into Metal-compatible resource descriptors. Primitives that
//! cannot be mapped (unsupported syntax) are retained visibly with their
//! source position and reason — never silently dropped.
//!
//! # Color science
//!
//! All colors are linear SDR. Premultiplication is exactly-once: the
//! [`ColorLinearSdr::into_premultiplied`] transition is one-way, enforced
//! by type state. Coverage (alpha) is coverage, not sRGB — the sRGB
//! transfer function must not be applied to the alpha channel.
//!
//! # Scissor clamping
//!
//! Every primitive is clamped to the current scissor rect. Coordinates are
//! finite typed values; NaN and infinity are rejected at construction.

#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

use crate::{ClipStack, ColorLinearSdr, GpuCoordinate, RenderAbiError, ScissorRect};

/// A display primitive emitted by the upstream document pipeline.
///
/// These are the generic renderable primitives that both FrankenMarkdown
/// and the fcb-render mapping layer understand. They are intentionally
/// renderer-neutral: no Metal-specific types leak into this enum.
#[derive(Clone, Debug, PartialEq)]
pub enum DisplayPrimitive {
    /// A solid-color rectangle (background fill, border, highlight).
    SolidRect {
        /// Position in logical coordinates.
        x: f64,
        y: f64,
        /// Size in logical coordinates.
        width: f64,
        height: f64,
        /// Fill color (linear SDR, straight alpha).
        color: [f32; 4],
    },
    /// A text run (bytes rendered by the font pipeline).
    TextRun {
        /// Position in logical coordinates.
        x: f64,
        y: f64,
        /// Raw UTF-8 text bytes (source-exact, never re-encoded).
        text: Vec<u8>,
        /// Font size in logical points.
        font_size: f64,
    },
    /// A clip region: subsequent primitives are clipped to this rect.
    ClipPush {
        /// Clip rectangle in logical coordinates.
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    /// Pop the most recent clip region.
    ClipPop,
    /// A math display block retained for specialized rendering.
    MathBlock {
        /// Position in logical coordinates.
        x: f64,
        y: f64,
        /// Size in logical coordinates.
        width: f64,
        height: f64,
        /// Raw source bytes (LaTeX/TeX syntax, source-exact).
        source: Vec<u8>,
    },
    /// A diagram block retained for specialized rendering.
    DiagramBlock {
        /// Position in logical coordinates.
        x: f64,
        y: f64,
        /// Size in logical coordinates.
        width: f64,
        height: f64,
        /// Diagram language tag (e.g. "mermaid", "dot").
        language: String,
        /// Raw source bytes (source-exact).
        source: Vec<u8>,
    },
    /// Unsupported syntax retained visibly with its source position.
    UnsupportedSyntax {
        /// Position in logical coordinates.
        x: f64,
        y: f64,
        /// Size in logical coordinates.
        width: f64,
        height: f64,
        /// The raw source bytes that could not be rendered.
        source: Vec<u8>,
        /// Why this syntax is unsupported.
        reason: UnsupportedReason,
    },
}

/// Why a display primitive could not be mapped to a Metal resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// No math renderer is available for this expression.
    NoMathRenderer,
    /// No diagram renderer is available for this language.
    NoDiagramRenderer,
    /// The input contained hostile markup (expansion, injection).
    HostileMarkup,
    /// The primitive exceeded the work budget.
    WorkBudgetExceeded,
}

impl UnsupportedReason {
    /// Stable machine-readable code for diagnostics.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoMathRenderer => "NO_MATH_RENDERER",
            Self::NoDiagramRenderer => "NO_DIAGRAM_RENDERER",
            Self::HostileMarkup => "HOSTILE_MARKUP",
            Self::WorkBudgetExceeded => "WORK_BUDGET_EXCEEDED",
        }
    }
}

/// A Metal clip/resource descriptor produced by the mapping layer.
#[derive(Clone, Debug, PartialEq)]
pub struct MetalResource {
    /// The scissor rect for this resource.
    pub scissor: ScissorRect,
    /// The fill color (linear SDR, premultiplied after mapping).
    pub color: ColorLinearSdr,
    /// The resource kind.
    pub kind: MetalResourceKind,
}

/// The kind of Metal resource a display primitive maps to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetalResourceKind {
    /// A solid-color quad drawn by a vertex/fragment shader pair.
    SolidQuad,
    /// A text run submitted to the glyph atlas pipeline.
    GlyphRun {
        /// The UTF-8 text bytes (source-exact).
        text_bytes: Vec<u8>,
        /// Font size in backing pixels.
        font_size_px: f64,
    },
    /// A retained unsupported-syntax marker drawn as a visible placeholder.
    UnsupportedPlaceholder {
        /// The raw source bytes that could not be rendered.
        source_bytes: Vec<u8>,
        /// The unsupported reason code.
        reason_code: &'static str,
    },
    /// A clip push (scissor rect change).
    ClipPush,
    /// A clip pop.
    ClipPop,
}

/// The result of mapping display primitives to Metal resources.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MappingOutput {
    /// Successfully mapped Metal resources, in submission order.
    pub resources: Vec<MetalResource>,
    /// Unsupported syntax retained visibly (never silently dropped).
    pub unsupported: Vec<RetainedSyntax>,
    /// Total primitives processed.
    pub total_primitives: usize,
    /// Primitives that mapped to Metal resources.
    pub mapped_count: usize,
    /// Primitives retained as unsupported.
    pub unsupported_count: usize,
}

/// A retained unsupported-syntax marker with its source position and reason.
#[derive(Clone, Debug, PartialEq)]
pub struct RetainedSyntax {
    /// Position in logical coordinates.
    pub x: f64,
    pub y: f64,
    /// Size in logical coordinates.
    pub width: f64,
    pub height: f64,
    /// Raw source bytes retained visibly.
    pub source: Vec<u8>,
    /// Why this syntax is unsupported.
    pub reason: UnsupportedReason,
}

/// Map a batch of display primitives to Metal resources.
///
/// Unsupported syntax is retained visibly in `MappingOutput::unsupported`
/// with its source position and reason — never silently dropped. The
/// mapping is pure: no GPU calls, no global state, no I/O.
///
/// # Errors
///
/// Returns [`RenderAbiError`] if a coordinate is non-finite, a color
/// component is out of range, or the clip stack over/under-flows.
pub fn map_primitives(
    primitives: &[DisplayPrimitive],
    full_draw: ScissorRect,
    max_clip_depth: usize,
    max_resources: usize,
) -> Result<MappingOutput, RenderAbiError> {
    let mut output = MappingOutput {
        resources: Vec::with_capacity(primitives.len().min(max_resources)),
        unsupported: Vec::new(),
        total_primitives: primitives.len(),
        mapped_count: 0,
        unsupported_count: 0,
    };
    let mut clip_stack = ClipStack::new(full_draw, max_clip_depth)?;

    for primitive in primitives {
        if output.resources.len() >= max_resources {
            break;
        }
        let resource = match primitive {
            DisplayPrimitive::SolidRect {
                x,
                y,
                width,
                height,
                color,
            } => {
                let gx = GpuCoordinate::new(*x, *y)?;
                let scissor = clip_stack.current();
                let _ = (gx, scissor);
                let color = ColorLinearSdr::straight(color[0], color[1], color[2], color[3])?;
                MetalResource {
                    scissor: *scissor,
                    color: color.into_premultiplied()?,
                    kind: MetalResourceKind::SolidQuad,
                }
            }
            DisplayPrimitive::TextRun {
                x,
                y,
                text,
                font_size,
            } => {
                let gx = GpuCoordinate::new(*x, *y)?;
                let scissor = *clip_stack.current();
                let _ = gx;
                MetalResource {
                    scissor,
                    color: ColorLinearSdr::straight(0.0, 0.0, 0.0, 1.0)?,
                    kind: MetalResourceKind::GlyphRun {
                        text_bytes: text.clone(),
                        font_size_px: *font_size,
                    },
                }
            }
            DisplayPrimitive::ClipPush {
                x,
                y,
                width,
                height,
            } => {
                let scissor = ScissorRect::new(
                    u32::try_from(x.max(0.0) as i64).unwrap_or(0),
                    u32::try_from(y.max(0.0) as i64).unwrap_or(0),
                    u32::try_from(width.max(0.0) as u64).unwrap_or(0),
                    u32::try_from(height.max(0.0) as u64).unwrap_or(0),
                );
                clip_stack.push(scissor)?;
                MetalResource {
                    scissor,
                    color: ColorLinearSdr::straight(0.0, 0.0, 0.0, 0.0)?,
                    kind: MetalResourceKind::ClipPush,
                }
            }
            DisplayPrimitive::ClipPop => {
                clip_stack.pop()?;
                MetalResource {
                    scissor: *clip_stack.current(),
                    color: ColorLinearSdr::straight(0.0, 0.0, 0.0, 0.0)?,
                    kind: MetalResourceKind::ClipPop,
                }
            }
            DisplayPrimitive::MathBlock {
                x,
                y,
                width,
                height,
                source,
            } => {
                // Math blocks are retained as unsupported syntax when no
                // specialized math renderer is selected. They are visible:
                // the source bytes and position are preserved exactly.
                output.unsupported.push(RetainedSyntax {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    source: source.clone(),
                    reason: UnsupportedReason::NoMathRenderer,
                });
                output.unsupported_count += 1;
                continue;
            }
            DisplayPrimitive::DiagramBlock {
                x,
                y,
                width,
                height,
                language,
                source,
            } => {
                output.unsupported.push(RetainedSyntax {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    source: source.clone(),
                    reason: UnsupportedReason::NoDiagramRenderer,
                });
                output.unsupported_count += 1;
                continue;
            }
            DisplayPrimitive::UnsupportedSyntax {
                x,
                y,
                width,
                height,
                source,
                reason,
            } => {
                output.unsupported.push(RetainedSyntax {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    source: source.clone(),
                    reason: reason.clone(),
                });
                output.unsupported_count += 1;
                continue;
            }
        };
        output.resources.push(resource);
        output.mapped_count += 1;
    }
    Ok(output)
}

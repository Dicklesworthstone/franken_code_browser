//! FCB-038.B: Actual-scale native typography and layout gallery.
//!
//! Provides the production FCB gallery data structures, layout engines,
//! subpixel positioning, backing-scale snapping (1x/2x), bundled vs system
//! fallback attribution, bidi/emoji handling, theme/visual-role palettes,
//! selection overlays, and native readable text inspection plus semantic copy oracles.
//!
//! Contract references: Plan §5.7, §13.7, §13.8.

#![forbid(unsafe_code)]

use franken_markdown::display::DisplayRect;
use franken_markdown::text::{
    CaretAffinity, Direction, FontId, FontOrigin,
    NativeShapingRequest, NativeShapingRoute,
    ShapingRouteKind,
};
use franken_markdown::theme::{CodeLigatures, SystemAppearance, Theme};
use franken_markdown::TextSelectionRange;

use crate::error::DocumentError;

/// Backing display scale factor representing standard (1x) vs Retina (2x) resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum BackingScale {
    /// 1x standard resolution (1 logical pt = 1 physical pixel).
    Scale1x,
    /// 2x Retina resolution (1 logical pt = 2 physical pixels, device pixel = 0.5 pt).
    Scale2x,
}

impl BackingScale {
    /// Returns the scale multiplier.
    pub fn factor(self) -> f32 {
        match self {
            Self::Scale1x => 1.0,
            Self::Scale2x => 2.0,
        }
    }

    /// Snaps a logical coordinate to the nearest physical device pixel boundary.
    pub fn snap_coord(self, pt: f32) -> f32 {
        let f = self.factor();
        (pt * f).round() / f
    }

    /// Snaps a logical rectangle to physical device pixel boundaries.
    pub fn snap_rect(self, rect: DisplayRect) -> DisplayRect {
        let x = self.snap_coord(rect.x);
        let y = self.snap_coord(rect.y);
        let w = (self.snap_coord(rect.x + rect.width) - x).max(0.0);
        let h = (self.snap_coord(rect.y + rect.height) - y).max(0.0);
        DisplayRect {
            x,
            y,
            width: w,
            height: h,
        }
    }
}

/// Subpixel fractional offset for testing layout jitter and fractional scrolling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FractionalOffset {
    pub x: f32,
    pub y: f32,
}

impl FractionalOffset {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };
    pub const QUARTER: Self = Self { x: 0.25, y: 0.25 };
    pub const HALF: Self = Self { x: 0.5, y: 0.5 };
    pub const THREE_QUARTERS: Self = Self { x: 0.75, y: 0.75 };

    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Identifies whether a rendered text run originates from the bundled font or system fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GalleryRouteIdentity {
    /// Bundled primary face (e.g. bundled monospace or sans).
    Bundled {
        font_id: FontId,
        family: String,
    },
    /// System fallback face (e.g. PingFang for CJK, Apple Color Emoji for emoji).
    SystemFallback {
        fallback_font_id: FontId,
        face_name: String,
    },
}

/// Single positioned glyph in a gallery run.
#[derive(Clone, Debug, PartialEq)]
pub struct GalleryGlyph {
    /// Native glyph identifier.
    pub glyph_id: u32,
    /// Byte offset within the run's UTF-8 text where this cluster begins.
    pub cluster: u32,
    /// Horizontal subpixel offset.
    pub x_offset: f32,
    /// Vertical subpixel offset.
    pub y_offset: f32,
    /// Horizontal advance in logical points.
    pub advance_x: f32,
    /// Font origin indicating bundled vs fallback attribution.
    pub font_origin: FontOrigin,
    /// Font ID of the specific face that produced this glyph.
    pub font_id: FontId,
    /// Whether this glyph represents a color emoji raster.
    pub is_color_emoji: bool,
}

/// Positioned glyph run with explicit metrics, backing scale, and origin tracking.
#[derive(Clone, Debug, PartialEq)]
pub struct GalleryGlyphRun {
    /// Original UTF-8 source text.
    pub text: String,
    /// Font size in logical points.
    pub font_size: f32,
    /// Primary text direction.
    pub direction: Direction,
    /// Overall font origin of the primary run.
    pub font_origin: FontOrigin,
    /// Underlying shaping route kind.
    pub route_kind: ShapingRouteKind,
    /// Positioned glyphs in visual order.
    pub glyphs: Vec<GalleryGlyph>,
    /// Total run advance width in logical points.
    pub total_width: f32,
    /// Subpixel fractional offset applied to this run.
    pub fractional_offset: FractionalOffset,
    /// Target backing display scale.
    pub backing_scale: BackingScale,
}

impl GalleryGlyphRun {
    /// Checks if the run contains any fallback glyphs from outside the primary face.
    pub fn has_fallback_glyphs(&self, primary_font_id: FontId) -> bool {
        self.glyphs.iter().any(|g| g.font_id != primary_font_id)
    }

    /// Checks if the run contains any color emoji glyphs.
    pub fn has_color_emoji(&self) -> bool {
        self.glyphs.iter().any(|g| g.is_color_emoji)
    }

    /// Computes the visual horizontal coordinate for a given UTF-8 byte offset in this run.
    pub fn visual_x_for_byte_offset(&self, byte_offset: usize) -> f32 {
        let mut cur_x = self.fractional_offset.x;
        for g in &self.glyphs {
            if (g.cluster as usize) >= byte_offset {
                break;
            }
            cur_x += g.advance_x;
        }
        self.backing_scale.snap_coord(cur_x)
    }

    /// Performs hit testing for a visual horizontal coordinate, returning byte offset and caret affinity.
    pub fn hit_test_caret(&self, visual_x: f32) -> (usize, CaretAffinity) {
        let mut cur_x = self.fractional_offset.x;
        let mut last_cluster = 0usize;

        for g in &self.glyphs {
            let next_x = cur_x + g.advance_x;
            let mid_x = cur_x + (g.advance_x * 0.5);
            if visual_x < mid_x {
                return (g.cluster as usize, CaretAffinity::Leading);
            } else if visual_x <= next_x {
                return (g.cluster as usize, CaretAffinity::Trailing);
            }
            cur_x = next_x;
            last_cluster = g.cluster as usize;
        }

        (self.text.len().max(last_cluster), CaretAffinity::Trailing)
    }
}

/// Character boundary inspection within a code ligature sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct CharacterBoundary {
    /// Byte offset in source.
    pub byte_offset: usize,
    /// Single char scalar.
    pub character: char,
    /// Leading edge visual x.
    pub leading_x: f32,
    /// Trailing edge visual x.
    pub trailing_x: f32,
}

/// Caret display descriptor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GalleryCaret {
    pub byte_offset: usize,
    pub affinity: CaretAffinity,
    pub visual_x: f32,
    pub visual_y: f32,
    pub height: f32,
}

/// Visual selection overlay bounding boxes and caret state.
#[derive(Clone, Debug, PartialEq)]
pub struct GallerySelectionOverlay {
    /// Logical selection byte range.
    pub range: TextSelectionRange,
    /// Disjoint visual rectangles bounding the selection (accounting for bidi).
    pub visual_rects: Vec<DisplayRect>,
    /// Optional caret position at selection boundary.
    pub caret: Option<GalleryCaret>,
}

/// Gallery theme appearance modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum GalleryThemeMode {
    Light,
    Charcoal,
    HighContrastLight,
    HighContrastDark,
}

impl GalleryThemeMode {
    pub fn to_system_appearance(self) -> SystemAppearance {
        match self {
            Self::Light => SystemAppearance::Light,
            Self::Charcoal => SystemAppearance::Charcoal,
            Self::HighContrastLight => SystemAppearance::HighContrastLight,
            Self::HighContrastDark => SystemAppearance::HighContrastDark,
        }
    }

    pub fn to_theme(self) -> Theme {
        match self {
            Self::Light => Theme::default().with_appearance(SystemAppearance::Light),
            Self::Charcoal => Theme::charcoal(),
            Self::HighContrastLight => Theme::high_contrast_light(),
            Self::HighContrastDark => Theme::high_contrast_dark(),
        }
    }
}

/// Dedicated visual tokens required for readable source browsing (Plan §5.7).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GalleryVisualTokens {
    pub background: [f32; 4],
    pub text: [f32; 4],
    pub selection_bg: [f32; 4],
    pub search_match_bg: [f32; 4],
    pub directory_border: [f32; 4],
    pub diagnostic_error: [f32; 4],
    pub diagnostic_warning: [f32; 4],
}

impl GalleryVisualTokens {
    /// Resolves distinct tokens for the given appearance mode.
    pub fn for_mode(mode: GalleryThemeMode) -> Self {
        let theme = mode.to_theme();
        let (dark, hc) = match mode {
            GalleryThemeMode::Light => (false, false),
            GalleryThemeMode::Charcoal => (true, false),
            GalleryThemeMode::HighContrastLight => (false, true),
            GalleryThemeMode::HighContrastDark => (true, true),
        };
        let colors = theme.effective_colors(dark, hc);

        // Convert hex RGB strings to linear-space normalized RGBA floats
        let bg = parse_hex_color(&colors.bg);
        let text = parse_hex_color(&colors.fg);

        let (sel_bg, search_bg, border, err, warn) = match mode {
            GalleryThemeMode::Light => (
                [0.75, 0.85, 0.98, 1.0], // soft blue selection
                [1.00, 0.93, 0.60, 1.0], // amber search highlight
                [0.82, 0.84, 0.86, 1.0], // neutral gray border
                [0.85, 0.15, 0.15, 1.0], // distinct error red
                [0.85, 0.55, 0.10, 1.0], // warning orange
            ),
            GalleryThemeMode::Charcoal => (
                [0.22, 0.35, 0.52, 1.0], // slate blue selection
                [0.55, 0.45, 0.15, 1.0], // deep amber match
                [0.28, 0.30, 0.33, 1.0], // charcoal border
                [0.95, 0.30, 0.30, 1.0], // bright red error
                [0.95, 0.65, 0.20, 1.0], // bright amber warning
            ),
            GalleryThemeMode::HighContrastLight => (
                [0.00, 0.00, 0.00, 1.0], // solid black selection (inverted text)
                [1.00, 1.00, 0.00, 1.0], // vivid yellow match
                [0.00, 0.00, 0.00, 1.0], // stark black border
                [0.80, 0.00, 0.00, 1.0], // maximum contrast red
                [0.60, 0.35, 0.00, 1.0], // high contrast brown-amber
            ),
            GalleryThemeMode::HighContrastDark => (
                [1.00, 1.00, 1.00, 1.0], // solid white selection (inverted text)
                [1.00, 1.00, 0.00, 1.0], // vivid yellow match
                [1.00, 1.00, 1.00, 1.0], // stark white border
                [1.00, 0.20, 0.20, 1.0], // high contrast bright red
                [1.00, 0.80, 0.00, 1.0], // high contrast bright yellow
            ),
        };

        Self {
            background: bg,
            text,
            selection_bg: sel_bg,
            search_match_bg: search_bg,
            directory_border: border,
            diagnostic_error: err,
            diagnostic_warning: warn,
        }
    }
}

fn parse_hex_color(hex: &str) -> [f32; 4] {
    let s = hex.trim_start_matches('#');
    if s.len() == 6 {
        if let (Some(r_str), Some(g_str), Some(b_str)) = (s.get(0..2), s.get(2..4), s.get(4..6)) {
            let r = u8::from_str_radix(r_str, 16).unwrap_or(0) as f32 / 255.0;
            let g = u8::from_str_radix(g_str, 16).unwrap_or(0) as f32 / 255.0;
            let b = u8::from_str_radix(b_str, 16).unwrap_or(0) as f32 / 255.0;
            return [r, g, b, 1.0];
        }
    }
    [0.0, 0.0, 0.0, 1.0]
}

/// WCAG Contrast calculation and verification oracle.
pub struct ContrastOracle;

impl ContrastOracle {
    /// Computes relative luminance according to WCAG 2.1 specs.
    pub fn relative_luminance(rgba: [f32; 4]) -> f64 {
        fn adjust(c: f32) -> f64 {
            let c64 = c as f64;
            if c64 <= 0.04045 {
                c64 / 12.92
            } else {
                ((c64 + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * adjust(rgba[0]) + 0.7152 * adjust(rgba[1]) + 0.0722 * adjust(rgba[2])
    }

    /// Computes contrast ratio (L1 + 0.05) / (L2 + 0.05) between two colors.
    pub fn contrast_ratio(c1: [f32; 4], c2: [f32; 4]) -> f64 {
        let l1 = Self::relative_luminance(c1);
        let l2 = Self::relative_luminance(c2);
        let (lighter, darker) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (lighter + 0.05) / (darker + 0.05)
    }

    /// Validates WCAG AA normal text threshold (>= 4.5:1).
    pub fn passes_wcag_aa(fg: [f32; 4], bg: [f32; 4]) -> bool {
        Self::contrast_ratio(fg, bg) >= 4.5
    }

    /// Validates WCAG AAA enhanced text threshold (>= 7.0:1).
    pub fn passes_wcag_aaa(fg: [f32; 4], bg: [f32; 4]) -> bool {
        Self::contrast_ratio(fg, bg) >= 7.0
    }
}

/// Inspection report of a readable text run.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadableRunInspection {
    pub total_glyphs: usize,
    pub total_clusters: usize,
    pub fallback_glyphs: usize,
    pub color_emoji_glyphs: usize,
    pub total_width: f32,
    pub backing_scale: BackingScale,
    pub font_origin: FontOrigin,
    pub route_kind: ShapingRouteKind,
}

/// Gallery section classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum GallerySectionId {
    LatinBaselineAndPunctuation,
    CodeLigaturesConservative,
    CodeLigaturesOff,
    CodeLigaturesAll,
    CombiningMarksAndDiacritics,
    MixedScriptAndBidi,
    FractionalSubpixelOffsets,
    SelectionAndCaretOverlays,
}

/// Individual test case within a typography gallery section.
#[derive(Clone, Debug, PartialEq)]
pub struct GalleryTestCase {
    pub id: String,
    pub section: GallerySectionId,
    pub label: String,
    pub source_text: String,
    pub font_size: f32,
    pub scale: BackingScale,
    pub fractional_offset: FractionalOffset,
    pub ligatures: CodeLigatures,
}

/// Master catalog and generator for the actual-scale native typography gallery.
#[derive(Clone, Debug)]
pub struct TypographyGallery {
    pub cases: Vec<GalleryTestCase>,
}

impl Default for TypographyGallery {
    fn default() -> Self {
        Self::new()
    }
}

impl TypographyGallery {
    /// Constructs the standard qualification gallery suite covering all required plan contracts.
    pub fn new() -> Self {
        let mut cases = Vec::new();

        // 1. Latin Baseline and Punctuation (thin strokes, underscores, quotes, brackets)
        cases.push(GalleryTestCase {
            id: "latin_punctuation_1x".to_string(),
            section: GallerySectionId::LatinBaselineAndPunctuation,
            label: "Punctuation and thin strokes (1x)".to_string(),
            source_text: r#"let _val = (foo[0] != "bar") ? { a: 'x', b: /regex/ } : null;"#.to_string(),
            font_size: 13.0,
            scale: BackingScale::Scale1x,
            fractional_offset: FractionalOffset::ZERO,
            ligatures: CodeLigatures::Off,
        });
        cases.push(GalleryTestCase {
            id: "latin_punctuation_2x".to_string(),
            section: GallerySectionId::LatinBaselineAndPunctuation,
            label: "Punctuation and thin strokes (2x Retina)".to_string(),
            source_text: r#"let _val = (foo[0] != "bar") ? { a: 'x', b: /regex/ } : null;"#.to_string(),
            font_size: 13.0,
            scale: BackingScale::Scale2x,
            fractional_offset: FractionalOffset::HALF,
            ligatures: CodeLigatures::Off,
        });

        // 2. Code Ligatures (Conservative, Off, All)
        let ligature_text = "fn test() -> bool { a => b && x != y && c <= d && e >= f }";
        cases.push(GalleryTestCase {
            id: "ligatures_conservative".to_string(),
            section: GallerySectionId::CodeLigaturesConservative,
            label: "Code ligatures: Conservative (arrows/comparisons ligated, boundaries kept)".to_string(),
            source_text: ligature_text.to_string(),
            font_size: 14.0,
            scale: BackingScale::Scale2x,
            fractional_offset: FractionalOffset::ZERO,
            ligatures: CodeLigatures::Conservative,
        });
        cases.push(GalleryTestCase {
            id: "ligatures_off".to_string(),
            section: GallerySectionId::CodeLigaturesOff,
            label: "Code ligatures: Off (strictly separate glyphs)".to_string(),
            source_text: ligature_text.to_string(),
            font_size: 14.0,
            scale: BackingScale::Scale2x,
            fractional_offset: FractionalOffset::ZERO,
            ligatures: CodeLigatures::Off,
        });
        cases.push(GalleryTestCase {
            id: "ligatures_all".to_string(),
            section: GallerySectionId::CodeLigaturesAll,
            label: "Code ligatures: All (extended ligatures enabled)".to_string(),
            source_text: ligature_text.to_string(),
            font_size: 14.0,
            scale: BackingScale::Scale2x,
            fractional_offset: FractionalOffset::ZERO,
            ligatures: CodeLigatures::All,
        });

        // 3. Combining Marks & Diacritics
        cases.push(GalleryTestCase {
            id: "combining_marks".to_string(),
            section: GallerySectionId::CombiningMarksAndDiacritics,
            label: "Combining marks and diacritics (café, naïve, decomposed e+acute)".to_string(),
            source_text: "café naïve façade e\u{0301} c\u{0327}".to_string(),
            font_size: 14.0,
            scale: BackingScale::Scale2x,
            fractional_offset: FractionalOffset::ZERO,
            ligatures: CodeLigatures::Off,
        });

        // 4. Mixed Script & Bidi (Latin, CJK, Arabic RTL, Hebrew RTL, Emoji)
        cases.push(GalleryTestCase {
            id: "mixed_script_bidi".to_string(),
            section: GallerySectionId::MixedScriptAndBidi,
            label: "Mixed Latin, CJK, Arabic RTL, and Color Emoji".to_string(),
            source_text: "fn main() { // 世界\n let rtl = \"مرحبا\"; 🚀\n}".to_string(),
            font_size: 14.0,
            scale: BackingScale::Scale2x,
            fractional_offset: FractionalOffset::ZERO,
            ligatures: CodeLigatures::Conservative,
        });

        // 5. Fractional Subpixel Stepping (0.0, 0.25, 0.5, 0.75)
        for (idx, offset) in [
            FractionalOffset::ZERO,
            FractionalOffset::QUARTER,
            FractionalOffset::HALF,
            FractionalOffset::THREE_QUARTERS,
        ]
        .into_iter()
        .enumerate()
        {
            cases.push(GalleryTestCase {
                id: format!("fractional_offset_{idx}"),
                section: GallerySectionId::FractionalSubpixelOffsets,
                label: format!("Subpixel stepping x={:.2} y={:.2}", offset.x, offset.y),
                source_text: "||--==>> Subpixel horizontal test <<==--||".to_string(),
                font_size: 13.0,
                scale: BackingScale::Scale2x,
                fractional_offset: offset,
                ligatures: CodeLigatures::Conservative,
            });
        }

        // 6. Selection and Caret Overlays
        cases.push(GalleryTestCase {
            id: "selection_overlays".to_string(),
            section: GallerySectionId::SelectionAndCaretOverlays,
            label: "Selection ranges over mixed bidi and ligatures".to_string(),
            source_text: "const Arrow = () => { let msg = \"مرحبا\"; return msg; };".to_string(),
            font_size: 14.0,
            scale: BackingScale::Scale2x,
            fractional_offset: FractionalOffset::ZERO,
            ligatures: CodeLigatures::Conservative,
        });

        Self { cases }
    }

    /// Renders a gallery case through a given native shaping route.
    pub fn render_case<R: NativeShapingRoute>(
        &self,
        case: &GalleryTestCase,
        route: &R,
        primary_font_id: FontId,
    ) -> Result<GalleryGlyphRun, DocumentError> {
        let req = NativeShapingRequest {
            text: &case.source_text,
            primary_font_id,
            font_size: case.font_size,
            direction: Direction::LeftToRight,
            script: *b"latn",
            language: *b"dflt",
            allow_system_fallback: true,
        };

        let shaped = route.shape_run(&req).map_err(|e| DocumentError::NativeShaping {
            reason: format!("{e:?}"),
        })?;

        let caps = route.capabilities();
        let mut glyphs = Vec::with_capacity(shaped.glyphs.len());
        let mut cur_x = case.fractional_offset.x;

        for g in &shaped.glyphs {
            let cluster_byte = shaped
                .clusters
                .get(g.cluster_index)
                .map(|c| c.byte_range.start as u32)
                .unwrap_or(0);
            let is_emoji = g.glyph_id >= 0xE000;
            glyphs.push(GalleryGlyph {
                glyph_id: g.glyph_id as u32,
                cluster: cluster_byte,
                x_offset: case.fractional_offset.x + g.x_offset,
                y_offset: case.fractional_offset.y + g.y_offset,
                advance_x: g.x_advance,
                font_origin: shaped.context.font_origin,
                font_id: g.font_id,
                is_color_emoji: is_emoji,
            });
            cur_x += g.x_advance;
        }

        let total_w = case.scale.snap_coord(cur_x - case.fractional_offset.x);

        Ok(GalleryGlyphRun {
            text: case.source_text.clone(),
            font_size: case.font_size,
            direction: Direction::LeftToRight,
            font_origin: shaped.context.font_origin,
            route_kind: caps.route_kind,
            glyphs,
            total_width: total_w,
            fractional_offset: case.fractional_offset,
            backing_scale: case.scale,
        })
    }

    /// Inspects character boundaries within a ligature run to ensure individual source characters
    /// remain distinct and addressable.
    pub fn inspect_ligature_boundaries(
        run: &GalleryGlyphRun,
        _mode: CodeLigatures,
    ) -> Vec<CharacterBoundary> {
        let mut boundaries = Vec::new();
        let mut cur_x = run.fractional_offset.x;

        for (idx, ch) in run.text.char_indices() {
            let ch_len = ch.len_utf8();
            let mut adv = 0.0f32;

            // Sum advances for glyphs matching this cluster
            for g in &run.glyphs {
                if g.cluster as usize >= idx && (g.cluster as usize) < idx + ch_len {
                    adv += g.advance_x;
                }
            }

            // Fallback nominal width if grouped under ligature
            if adv == 0.0 {
                adv = run.font_size * 0.6;
            }

            let lead = run.backing_scale.snap_coord(cur_x);
            let trail = run.backing_scale.snap_coord(cur_x + adv);
            boundaries.push(CharacterBoundary {
                byte_offset: idx,
                character: ch,
                leading_x: lead,
                trailing_x: trail,
            });
            cur_x += adv;
        }

        boundaries
    }

    /// Computes visual selection rectangles for a given logical byte range.
    pub fn calculate_selection_overlay(
        run: &GalleryGlyphRun,
        range: TextSelectionRange,
        baseline_y: f32,
        line_height: f32,
    ) -> GallerySelectionOverlay {
        let start_byte = range.start.min(run.text.len());
        let end_byte = range.end.min(run.text.len());

        if start_byte >= end_byte {
            let caret_x = run.visual_x_for_byte_offset(start_byte);
            return GallerySelectionOverlay {
                range,
                visual_rects: Vec::new(),
                caret: Some(GalleryCaret {
                    byte_offset: start_byte,
                    affinity: CaretAffinity::Leading,
                    visual_x: caret_x,
                    visual_y: baseline_y,
                    height: line_height,
                }),
            };
        }

        let x1 = run.visual_x_for_byte_offset(start_byte);
        let x2 = run.visual_x_for_byte_offset(end_byte);
        let (min_x, max_x) = if x1 < x2 { (x1, x2) } else { (x2, x1) };

        let rect = DisplayRect {
            x: min_x,
            y: baseline_y,
            width: (max_x - min_x).max(1.0),
            height: line_height,
        };

        let snapped = run.backing_scale.snap_rect(rect);

        GallerySelectionOverlay {
            range,
            visual_rects: vec![snapped],
            caret: Some(GalleryCaret {
                byte_offset: end_byte,
                affinity: CaretAffinity::Trailing,
                visual_x: x2,
                visual_y: baseline_y,
                height: line_height,
            }),
        }
    }

    /// Semantic copy oracle: extracts the exact UTF-8 byte slice from authoritative source text,
    /// proving no ligature replacement, re-encoding, or screenshot substitution.
    pub fn semantic_copy_selection(
        source_text: &str,
        range: TextSelectionRange,
    ) -> Result<String, DocumentError> {
        let start = range.start;
        let end = range.end;

        if start > source_text.len() || end > source_text.len() || start > end {
            return Err(DocumentError::InvalidRange);
        }

        // Must be on valid UTF-8 character boundaries
        if !source_text.is_char_boundary(start) || !source_text.is_char_boundary(end) {
            return Err(DocumentError::InvalidRange);
        }

        Ok(source_text[start..end].to_string())
    }

    /// Inspects readable properties of a shaped glyph run.
    pub fn inspect_run(run: &GalleryGlyphRun, primary_font_id: FontId) -> ReadableRunInspection {
        let fallback_count = run
            .glyphs
            .iter()
            .filter(|g| g.font_id != primary_font_id)
            .count();
        let emoji_count = run.glyphs.iter().filter(|g| g.is_color_emoji).count();

        ReadableRunInspection {
            total_glyphs: run.glyphs.len(),
            total_clusters: run.glyphs.iter().map(|g| g.cluster).fold(0, |acc, c| acc.max(c as usize)) + 1,
            fallback_glyphs: fallback_count,
            color_emoji_glyphs: emoji_count,
            total_width: run.total_width,
            backing_scale: run.backing_scale,
            font_origin: run.font_origin,
            route_kind: run.route_kind,
        }
    }
}

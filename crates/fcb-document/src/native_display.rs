#![forbid(unsafe_code)]

//! A retained native presentation of the already captured Markdown document.
//! All parsing, inline styles, wrapping, table/list geometry and font shaping
//! are supplied by the committed FrankenMarkdown public API. This adapter only
//! admits resources, binds source/layout identities and validates publication.
//! It performs no I/O, asset loading, native calls or implicit runtime work.

use fcb_core::{ByteLength, ByteOffset, ByteRange, DocumentGeneration,
    ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_source::CompleteCapture;
use franken_markdown::{DisplayItem, DisplayList, DisplayRect, FontFamily, ResumableFlowDisplay, SourceSpan};
use franken_markdown::display::VectorShapeType;
use franken_markdown::flow_display::{FlowDisplayLimits, FlowLayoutError, FlowLayoutOptions};
use franken_markdown::fonts::BundledFlowFonts;
use franken_markdown::text::{Direction, FontId};
use crate::reader::{DocumentReadError, DocumentReader};

pub const MAX_DISPLAY_SOURCE_BYTES: usize = 64 * 1024;
pub const MAX_DISPLAY_ITEMS: usize = 8192;
pub const MAX_DISPLAY_GLYPHS: usize = 131_072;
pub const MAX_DISPLAY_RUN_GLYPHS: usize = 4096;
pub const MAX_DISPLAY_PAGE_ITEMS: usize = 128;
pub const MAX_DISPLAY_PAGE_GLYPHS: usize = 16_384;
pub const MAX_DISPLAY_FONT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_DISPLAY_FONT_CHUNK: usize = 64 * 1024;
const MAX_DISPLAY_TEXT_BYTES: usize = 1024 * 1024;
const MAX_DISPLAY_HEIGHT: f32 = 2_000_000.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeDisplayError {
    InvalidLimits, SourceLimit, ResourceDenied, Canceled, EngineLimit,
    ShapingUnsupported, LayoutUnavailable, InvalidOutput, InvalidRange,
    StaleCapture, StaleGeneration, MissingDisplay,
}
impl NativeDisplayError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "DOCUMENT_DISPLAY_INVALID_LIMITS",
            Self::SourceLimit => "DOCUMENT_DISPLAY_SOURCE_LIMIT",
            Self::ResourceDenied => "DOCUMENT_DISPLAY_RESOURCE_DENIED",
            Self::Canceled => "DOCUMENT_DISPLAY_CANCELED",
            Self::EngineLimit => "DOCUMENT_DISPLAY_LIMIT",
            Self::ShapingUnsupported => "DOCUMENT_DISPLAY_SHAPING_UNSUPPORTED",
            Self::LayoutUnavailable => "DOCUMENT_DISPLAY_LAYOUT_UNAVAILABLE",
            Self::InvalidOutput => "DOCUMENT_DISPLAY_INVALID_OUTPUT",
            Self::InvalidRange => "DOCUMENT_DISPLAY_INVALID_RANGE",
            Self::StaleCapture => "DOCUMENT_DISPLAY_STALE_CAPTURE",
            Self::StaleGeneration => "DOCUMENT_DISPLAY_STALE_GENERATION",
            Self::MissingDisplay => "DOCUMENT_DISPLAY_NOT_PREPARED",
        }
    }
}
impl std::fmt::Display for NativeDisplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for NativeDisplayError {}

/// Immutable upstream geometry and exact bundled font identities. Text glyph
/// offsets are relative to the run's baseline; bounds are upstream line boxes.
/// A native adapter aligns the supplied font within that box without reshaping.
/// Original source ranges are enclosing AST blocks, never glyph/inline ranges.
pub struct NativeDocumentDisplay {
    capture: CompleteCapture,
    generation: DocumentGeneration,
    width_points: u32,
    source_base: usize,
    source_ranges_qualified: bool,
    qualified_spans: Vec<SourceSpan>,
    display: DisplayList,
    fonts: BundledFlowFonts,
    font_ids: Vec<FontId>,
    unresolved_images: usize,
    glyph_count: usize,
    _lease: ResourceLease,
}

impl NativeDocumentDisplay {
    /// Prepare on the host worker. The shared parser is bounded-input but not
    /// preemptible. Emission and shaping poll cancellation between bounded units.
    /// Nothing is installed in a reader until its complete wire page also fits.
    pub fn prepare(document: &DocumentReader<'_>, width_points: u32,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
        mut canceled: impl FnMut() -> bool) -> Result<Self, NativeDisplayError> {
        check(&mut canceled)?;
        if !(160..=4096).contains(&width_points) { return Err(NativeDisplayError::InvalidLimits); }
        let capture = document.capture();
        if capture.bytes().len() > MAX_DISPLAY_SOURCE_BYTES { return Err(NativeDisplayError::SourceLimit); }
        let source_base = document.source_base();
        let source = std::str::from_utf8(capture.bytes().get(source_base..)
            .ok_or(NativeDisplayError::InvalidOutput)?)
            .map_err(|_| NativeDisplayError::InvalidOutput)?;
        let parse_charge = crate::links::parse_workspace(source.as_bytes())
            .map_err(|_| NativeDisplayError::EngineLimit)?;
        // Includes simultaneous AST/projection/layout candidates, retained runs,
        // source pin and the upstream immutable bundled-font registry envelope.
        // It is managed admission, not a process-footprint or cache-eviction claim.
        let charge = parse_charge.checked_add(MAX_DISPLAY_GLYPHS * 256)
            .and_then(|n| n.checked_add(MAX_DISPLAY_ITEMS * 512))
            .and_then(|n| n.checked_add(24 * 1024 * 1024 + source.len()))
            .ok_or(NativeDisplayError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(capture.request().file().owner(), allocations[0],
            ByteLength::new(charge as u64)).map_err(|_| NativeDisplayError::ResourceDenied)?;
        let qualified_spans = match document.qualified_source_spans(
            budget, allocations[1], &mut canceled) {
            Ok(spans) => Some(spans),
            // Known upstream span/catalog limitations must not make otherwise
            // readable Markdown disappear. Such output confers no source action.
            Err(DocumentReadError::InvalidOutput | DocumentReadError::InvalidRange) => None,
            Err(DocumentReadError::Canceled) => return Err(NativeDisplayError::Canceled),
            Err(DocumentReadError::ResourceDenied) => return Err(NativeDisplayError::ResourceDenied),
            Err(_) => return Err(NativeDisplayError::EngineLimit),
        };
        let limits = FlowDisplayLimits {
            max_source_bytes: MAX_DISPLAY_SOURCE_BYTES, max_source_lines: 32_768,
            max_ast_nodes: 32_768, max_nesting_depth: 64, max_output_blocks: 4096,
            max_output_bytes: MAX_DISPLAY_TEXT_BYTES, max_asset_requests: 128,
            max_asset_bytes: 0, max_retained_asset_bytes: 0, max_image_dimension: 4096,
        };
        let mut engine = ResumableFlowDisplay::try_with_limits(source, 64,
            document.generation().get(), limits).map_err(|_| NativeDisplayError::EngineLimit)?;
        while !engine.is_finished() {
            check(&mut canceled)?;
            engine.step().map_err(|_| NativeDisplayError::EngineLimit)?;
        }
        check(&mut canceled)?;
        let fonts = BundledFlowFonts::new(FontFamily::Sans).map_err(layout_error)?;
        let options = FlowLayoutOptions {
            viewport_width: width_points as f32, body_size: 14.0, code_size: 13.0,
            line_height: 20.0, max_lines: 8192, max_items: MAX_DISPLAY_ITEMS,
            max_shape_bytes: 16 * 1024, max_total_shape_bytes: 2 * 1024 * 1024,
        };
        let mut stopped = false;
        let display = engine.to_styled_display_list(options, |text, size, role, style| {
            if canceled() { stopped = true; return Err("canceled".to_owned()); }
            fonts.shape(text, size, role, style)
        });
        if stopped { return Err(NativeDisplayError::Canceled); }
        let display = display.map_err(layout_error)?;
        check(&mut canceled)?;
        let mut candidate = Self { capture: capture.clone(), generation: document.generation(),
            width_points, source_base, source_ranges_qualified: qualified_spans.is_some(),
            qualified_spans: qualified_spans.unwrap_or_default(), display, fonts,
            font_ids: Vec::with_capacity(6), unresolved_images: 0, glyph_count: 0, _lease: lease };
        candidate.validate(&mut canceled)?;
        Ok(candidate)
    }

    pub fn generation(&self) -> DocumentGeneration { self.generation }
    pub fn width_points(&self) -> u32 { self.width_points }
    pub fn source_ranges_qualified(&self) -> bool { self.source_ranges_qualified }
    pub fn display(&self) -> &DisplayList { &self.display }
    pub fn unresolved_images(&self) -> usize { self.unresolved_images }
    pub fn glyph_count(&self) -> usize { self.glyph_count }
    pub fn font_count(&self) -> usize { self.font_ids.len() }

    pub fn validate_delivery(&self, capture: &CompleteCapture, generation: u64)
        -> Result<(), NativeDisplayError> {
        if self.generation.get() != generation { return Err(NativeDisplayError::StaleGeneration); }
        if self.capture.request() != capture.request()
            || !std::ptr::eq(self.capture.bytes(), capture.bytes()) {
            return Err(NativeDisplayError::StaleCapture);
        }
        Ok(())
    }

    /// Native font indices are local to this exact retained layout. The numeric
    /// upstream font hash alone is never accepted as authority for arbitrary data.
    pub fn font_index(&self, id: FontId) -> Result<usize, NativeDisplayError> {
        self.font_ids.iter().position(|known| *known == id).ok_or(NativeDisplayError::InvalidOutput)
    }
    pub fn font_bytes(&self, index: usize) -> Result<&'static [u8], NativeDisplayError> {
        let id = self.font_ids.get(index).ok_or(NativeDisplayError::InvalidRange)?;
        self.fonts.font_bytes(*id).ok_or(NativeDisplayError::InvalidOutput)
    }
    pub fn font_chunk(&self, index: usize, offset: usize, count: usize)
        -> Result<&'static [u8], NativeDisplayError> {
        if !(1..=MAX_DISPLAY_FONT_CHUNK).contains(&count) { return Err(NativeDisplayError::InvalidLimits); }
        let bytes = self.font_bytes(index)?;
        if offset > bytes.len() { return Err(NativeDisplayError::InvalidRange); }
        Ok(&bytes[offset..offset.saturating_add(count).min(bytes.len())])
    }

    /// A page never splits a glyph run. The returned end can precede requested
    /// count because glyph payload has an independent bound. EOF is empty.
    pub fn page_end(&self, first: usize, count: usize) -> Result<usize, NativeDisplayError> {
        if !(1..=MAX_DISPLAY_PAGE_ITEMS).contains(&count) { return Err(NativeDisplayError::InvalidLimits); }
        let items = self.display.items();
        if first > items.len() { return Err(NativeDisplayError::InvalidRange); }
        let limit = first.saturating_add(count).min(items.len());
        let mut glyphs = 0;
        let mut end = first;
        for item in &items[first..limit] {
            let count = match item { DisplayItem::Text(text) => text.font_run.as_ref()
                .ok_or(NativeDisplayError::InvalidOutput)?.glyphs.len(), _ => 0 };
            if count > MAX_DISPLAY_PAGE_GLYPHS - glyphs { break; }
            glyphs += count; end += 1;
        }
        if end == first && first < items.len() { return Err(NativeDisplayError::EngineLimit); }
        Ok(end)
    }

    pub fn original_range(&self, index: usize) -> Result<Option<ByteRange>, NativeDisplayError> {
        let item = self.display.items().get(index).ok_or(NativeDisplayError::InvalidRange)?;
        if !self.source_ranges_qualified { return Ok(None); }
        let span = item.source_span();
        if span.start == span.end { return Ok(None); }
        let start = span.start.checked_add(self.source_base).ok_or(NativeDisplayError::InvalidOutput)?;
        let end = span.end.checked_add(self.source_base).ok_or(NativeDisplayError::InvalidOutput)?;
        ByteRange::new(ByteOffset::new(start as u64), ByteOffset::new(end as u64))
            .map(Some).map_err(|_| NativeDisplayError::InvalidOutput)
    }

    /// Restore a viewport through the original captured source, never by
    /// scaling logical rows into unrelated glyph geometry. The result names the
    /// first retained item in the verified enclosing block, not an exact glyph.
    /// Whitespace gaps, BOM bytes and interior UTF-8 positions are not anchors.
    pub fn first_item_at_source(&self, offset: u64, mut canceled: impl FnMut() -> bool)
        -> Result<usize, NativeDisplayError> {
        check(&mut canceled)?;
        if !self.source_ranges_qualified { return Err(NativeDisplayError::InvalidOutput); }
        let offset = usize::try_from(offset).map_err(|_| NativeDisplayError::InvalidRange)?;
        let source = std::str::from_utf8(self.capture.bytes()).map_err(|_| NativeDisplayError::InvalidOutput)?;
        if offset < self.source_base || offset > source.len() || !source.is_char_boundary(offset) {
            return Err(NativeDisplayError::InvalidRange);
        }
        if offset == source.len() { return Ok(self.display.items().len()); }
        for index in 0..self.display.items().len() {
            if index.is_multiple_of(256) { check(&mut canceled)?; }
            if let Some(range) = self.original_range(index)?
                && range.start().get() <= offset as u64 && (offset as u64) < range.end().get() {
                return Ok(index);
            }
        }
        Err(NativeDisplayError::InvalidRange)
    }

    fn validate(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), NativeDisplayError> {
        if self.display.items().len() > MAX_DISPLAY_ITEMS { return Err(NativeDisplayError::EngineLimit); }
        let total = self.display.total_bounds();
        valid_rect(total)?;
        let source = std::str::from_utf8(&self.capture.bytes()[self.source_base..])
            .map_err(|_| NativeDisplayError::InvalidOutput)?;
        let mut text_bytes = 0_usize;
        let mut font_bytes = 0_usize;
        for item in self.display.items() {
            check(canceled)?;
            let bounds = item.bounds();
            valid_rect(bounds)?;
            if bounds.x < total.x - 1.0 || bounds.y < total.y - 1.0
                || bounds.right() > total.right() + 1.0 || bounds.bottom() > total.bottom() + 1.0 {
                return Err(NativeDisplayError::InvalidOutput);
            }
            let span = item.source_span();
            if source.get(span.start..span.end).is_none() { return Err(NativeDisplayError::InvalidOutput); }
            // The pinned styled emitter propagates enclosing top-level AST
            // BlockMeta.span unchanged to every primitive. Audit the retained
            // packet as well: foreign spans disable all source actions instead
            // of promoting a plausible UTF-8 range to proven correspondence.
            if span.start != span.end && self.source_ranges_qualified
                && self.qualified_spans.binary_search_by_key(&(span.start, span.end),
                    |known| (known.start, known.end)).is_err() {
                self.source_ranges_qualified = false;
            }
            match item {
                DisplayItem::Text(text) => {
                    let run = text.font_run.as_ref().ok_or(NativeDisplayError::InvalidOutput)?;
                    if text.text != run.logical_text || run.context.direction != Direction::LeftToRight
                        || run.context.font_size != text.font_size || !text.font_size.is_finite()
                        || !(1.0..=128.0).contains(&text.font_size) || text.color_role.len() > 128
                        || text.text.len() > 16 * 1024 || run.glyphs.len() > MAX_DISPLAY_RUN_GLYPHS {
                        return Err(NativeDisplayError::InvalidOutput);
                    }
                    text_bytes = text_bytes.checked_add(text.text.len()).ok_or(NativeDisplayError::EngineLimit)?;
                    self.glyph_count = self.glyph_count.checked_add(run.glyphs.len())
                        .ok_or(NativeDisplayError::EngineLimit)?;
                    if text_bytes > MAX_DISPLAY_TEXT_BYTES || self.glyph_count > MAX_DISPLAY_GLYPHS {
                        return Err(NativeDisplayError::EngineLimit);
                    }
                    let mut x = 0.0_f32; let mut y = 0.0_f32;
                    for glyph in &run.glyphs {
                        if glyph.glyph_id == 0 || glyph.cluster_index >= run.clusters.len()
                            || [glyph.x_advance, glyph.y_advance, glyph.x_offset, glyph.y_offset]
                                .iter().any(|value| !value.is_finite()) {
                            return Err(NativeDisplayError::InvalidOutput);
                        }
                        if (x + glyph.x_offset).abs() > 8192.0 || (y + glyph.y_offset).abs() > 8192.0 {
                            return Err(NativeDisplayError::InvalidOutput);
                        }
                        x += glyph.x_advance; y += glyph.y_advance;
                        if !x.is_finite() || !y.is_finite() || x.abs() > 8192.0 || y.abs() > 8192.0 {
                            return Err(NativeDisplayError::InvalidOutput);
                        }
                        if !self.font_ids.contains(&glyph.font_id) {
                            let bytes = self.fonts.font_bytes(glyph.font_id).ok_or(NativeDisplayError::InvalidOutput)?;
                            font_bytes = font_bytes.checked_add(bytes.len()).ok_or(NativeDisplayError::EngineLimit)?;
                            if self.font_ids.len() >= 6 || bytes.is_empty() || font_bytes > MAX_DISPLAY_FONT_BYTES {
                                return Err(NativeDisplayError::EngineLimit);
                            }
                            self.font_ids.push(glyph.font_id);
                        }
                    }
                }
                DisplayItem::Vector(vector) => {
                    if !vector.stroke_width.is_finite() || !(0.0..=16.0).contains(&vector.stroke_width)
                        || vector.color_role.len() > 128 { return Err(NativeDisplayError::InvalidOutput); }
                    // The pinned flow adapter supplies rule/border/marker
                    // primitives only. Future diagram directions/paths need an
                    // explicit geometry contract; never infer them from bounds.
                    if matches!(vector.shape, VectorShapeType::DiagramBox
                        | VectorShapeType::DiagramArrow | VectorShapeType::DiagramConnector) {
                        return Err(NativeDisplayError::LayoutUnavailable);
                    }
                }
                DisplayItem::Image(image) => {
                    if image.is_resolved || image.destination.len() > 4096 || image.alt_text.len() > 16 * 1024 {
                        return Err(NativeDisplayError::InvalidOutput);
                    }
                    self.unresolved_images += 1;
                }
                DisplayItem::Anchor(anchor) => {
                    if anchor.anchor_id.len() > 4096 || anchor.level > 6
                        || anchor.is_heading != (anchor.level != 0) { return Err(NativeDisplayError::InvalidOutput); }
                }
                // Current shaped output has no clips. Refuse an incompatible
                // future display contract instead of silently drawing unclipped.
                DisplayItem::Clip(_) => return Err(NativeDisplayError::InvalidOutput),
            }
        }
        check(canceled)
    }
}

fn valid_rect(rect: DisplayRect) -> Result<(), NativeDisplayError> {
    if [rect.x, rect.y, rect.width, rect.height, rect.right(), rect.bottom()]
        .iter().any(|value| !value.is_finite()) || rect.x < -4096.0 || rect.y < 0.0
        || rect.width < 0.0 || rect.height < 0.0 || rect.right() > 8192.0 || rect.bottom() > MAX_DISPLAY_HEIGHT {
        return Err(NativeDisplayError::InvalidOutput);
    }
    Ok(())
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), NativeDisplayError> {
    if canceled() { Err(NativeDisplayError::Canceled) } else { Ok(()) }
}
fn layout_error(error: FlowLayoutError) -> NativeDisplayError {
    match error {
        FlowLayoutError::Shaping(_) => NativeDisplayError::ShapingUnsupported,
        FlowLayoutError::BudgetExceeded(_) | FlowLayoutError::Input(_) => NativeDisplayError::EngineLimit,
        FlowLayoutError::TableTooNarrow { .. } | FlowLayoutError::ClusterTooWide { .. } => NativeDisplayError::LayoutUnavailable,
        FlowLayoutError::InvalidOptions => NativeDisplayError::InvalidLimits,
        FlowLayoutError::InvalidShapedRun => NativeDisplayError::InvalidOutput,
    }
}

#![forbid(unsafe_code)]

//! Native drawing packets from FrankenMarkdown's shaped display list. These
//! packets are independent of logical flow rows and rendered UTF-8 offsets.
//! A whole accepted logical-document generation owns at most one rich width.
//! Successful logical reflow/clear retires it; failures preserve prior output.

use fcb::document::{DisplayItem, DisplayRect};
use fcb::document::native_display::{NativeDocumentDisplay, NativeDisplayError,
    MAX_DISPLAY_FONT_CHUNK, MAX_DISPLAY_PAGE_ITEMS};
use super::{check, HostResponse, Output, ReaderSession, ReaderSessionError, EXIT_OK};

impl From<NativeDisplayError> for ReaderSessionError {
    fn from(error: NativeDisplayError) -> Self { Self::DocumentDisplay(error) }
}

impl ReaderSession {
    /// Requires an already prepared logical document; does not replace it.
    /// Width changes require a new logical document generation, so delayed
    /// display/font pages cannot bind to another layout under coincident IDs.
    pub fn prepare_document_display(&mut self, generation: u64, width_points: u32,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?;
        self.document_snapshot(generation)?;
        if let Some(display) = &self.document_display {
            display.validate_delivery(&self.capture, generation)?;
            if display.width_points() != width_points { return Err(NativeDisplayError::StaleGeneration.into()); }
            return self.display_page("document-display-prepare", generation, 0, 64, None, &mut canceled);
        }
        let allocations = self.allocations()?;
        let candidate = NativeDocumentDisplay::prepare(self.document_snapshot(generation)?, width_points,
            &self.budget, allocations, &mut canceled)?;
        check(&mut canceled)?;
        let mut out = self.output("document-display-prepare")?;
        summary(&mut out, &candidate)?;
        encode_page(&mut out, &candidate, 0, 64, &mut canceled)?;
        out.literal("}\n")?;
        let response = self.finish_output(out, EXIT_OK, &mut canceled)?;
        check(&mut canceled)?;
        self.document_display = Some(candidate);
        Ok(response)
    }

    pub fn document_display_page(&mut self, generation: u64, first: usize, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        self.display_page("document-display-page", generation, first, count, None, &mut canceled)
    }

    pub fn document_display_from_source(&mut self, generation: u64, original_offset: u64, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?;
        if !(1..=MAX_DISPLAY_PAGE_ITEMS).contains(&count) { return Err(NativeDisplayError::InvalidLimits.into()); }
        let first = self.display_snapshot(generation)?.first_item_at_source(original_offset, &mut canceled)?;
        self.display_page("document-display-from-source", generation, first, count, Some(original_offset), &mut canceled)
    }
    fn display_page(&mut self, command: &str, generation: u64, first: usize, count: usize,
        original_anchor: Option<u64>, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(canceled)?;
        if !(1..=MAX_DISPLAY_PAGE_ITEMS).contains(&count) { return Err(NativeDisplayError::InvalidLimits.into()); }
        self.display_snapshot(generation)?;
        let mut out = self.output(command)?;
        let display = self.display_snapshot(generation)?;
        summary(&mut out, display)?;
        if let Some(offset) = original_anchor {
            out.literal(",\"original_anchor\":")?; out.integer(offset)?;
        }
        encode_page(&mut out, display, first, count, canceled)?;
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, canceled)
    }

    /// Exact, bounded chunks of the bundled face actually named by a glyph.
    /// No platform font lookup or caller-provided filesystem/font path occurs.
    pub fn document_display_font(&mut self, generation: u64, index: usize, offset: usize, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?;
        if !(1..=MAX_DISPLAY_FONT_CHUNK).contains(&count) { return Err(NativeDisplayError::InvalidLimits.into()); }
        self.display_snapshot(generation)?;
        let mut out = self.output("document-display-font")?;
        let display = self.display_snapshot(generation)?;
        let bytes = display.font_chunk(index, offset, count)?;
        let total = display.font_bytes(index)?.len();
        summary(&mut out, display)?;
        out.literal(",\"font_index\":")?; out.integer(index as u64)?;
        out.literal(",\"font_offset\":")?; out.integer(offset as u64)?;
        out.literal(",\"font_bytes\":")?; out.integer(bytes.len() as u64)?;
        out.literal(",\"total_font_bytes\":")?; out.integer(total as u64)?;
        out.literal(",\"font_hex\":")?; out.hex(bytes)?;
        out.literal(",\"next_font_offset\":")?;
        if offset + bytes.len() < total { out.integer((offset + bytes.len()) as u64)?; }
        else { out.literal("null")?; }
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    fn display_snapshot(&self, generation: u64) -> Result<&NativeDocumentDisplay, ReaderSessionError> {
        self.document_snapshot(generation)?;
        let display = self.document_display.as_ref().ok_or(NativeDisplayError::MissingDisplay)?;
        display.validate_delivery(&self.capture, generation)?;
        Ok(display)
    }
}

fn summary(out: &mut Output, display: &NativeDocumentDisplay) -> Result<(), ReaderSessionError> {
    out.literal(",\"document_generation\":")?; out.integer(display.generation().get())?;
    out.literal(",\"display_width_points\":")?; out.integer(display.width_points() as u64)?;
    out.literal(",\"rendering\":\"bundled-frankenmarkdown-display\",\"native_shaped\":false,\"glyph_shaped\":true,\"source_mapping\":\"enclosing-ast-blocks-not-glyph-exact\",\"source_ranges_qualified\":")?;
    out.boolean(display.source_ranges_qualified())?;
    out.literal(",\"total_display_items\":")?; out.integer(display.display().items().len() as u64)?;
    out.literal(",\"total_glyphs\":")?; out.integer(display.glyph_count() as u64)?;
    out.literal(",\"unresolved_images\":")?; out.integer(display.unresolved_images() as u64)?;
    out.literal(",\"display_bounds\":")?; rectangle(out, display.display().total_bounds())?;
    out.literal(",\"fonts\":[")?;
    for index in 0..display.font_count() {
        if index != 0 { out.literal(",")?; }
        out.literal("{\"font_index\":")?; out.integer(index as u64)?;
        out.literal(",\"byte_length\":")?; out.integer(display.font_bytes(index)?.len() as u64)?;
        out.literal("}")?;
    }
    out.literal("]")?;
    Ok(())
}

fn encode_page(out: &mut Output, display: &NativeDocumentDisplay, first: usize, count: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(), ReaderSessionError> {
    let end = display.page_end(first, count)?;
    out.literal(",\"first_display_item\":")?; out.integer(first as u64)?;
    out.literal(",\"display_items\":[")?;
    for (offset, item) in display.display().items()[first..end].iter().enumerate() {
        check(canceled)?;
        if offset != 0 { out.literal(",")?; }
        let index = first + offset;
        out.literal("{\"item_index\":")?; out.integer(index as u64)?;
        out.literal(",\"bounds\":")?; rectangle(out, item.bounds())?;
        out.literal(",\"enclosing_original_range\":")?;
        match display.original_range(index)? {
            Some(range) => out.range(range)?, None => out.literal("null")?,
        }
        match item {
            DisplayItem::Text(text) => {
                let run = text.font_run.as_ref().ok_or(NativeDisplayError::InvalidOutput)?;
                out.literal(",\"kind\":\"text\",\"text\":")?; out.quoted(&text.text)?;
                out.literal(",\"color_role\":")?; out.quoted(&text.color_role)?;
                out.literal(",\"font_size\":")?; real(out, text.font_size)?;
                out.literal(",\"glyphs\":[")?;
                let mut x = 0.0; let mut y = 0.0;
                for (number, glyph) in run.glyphs.iter().enumerate() {
                    if number.is_multiple_of(256) { check(canceled)?; }
                    if number != 0 { out.literal(",")?; }
                    out.literal("{\"glyph_id\":")?; out.integer(u64::from(glyph.glyph_id))?;
                    out.literal(",\"font_index\":")?; out.integer(display.font_index(glyph.font_id)? as u64)?;
                    out.literal(",\"x\":")?; real(out, x + glyph.x_offset)?;
                    out.literal(",\"y\":")?; real(out, y + glyph.y_offset)?;
                    out.literal("}")?;
                    x += glyph.x_advance; y += glyph.y_advance;
                }
                out.literal("]")?;
            }
            DisplayItem::Vector(vector) => {
                out.literal(",\"kind\":\"vector\",\"shape\":")?;
                out.quoted(vector_shape(vector.shape))?;
                out.literal(",\"stroke_width\":")?; real(out, vector.stroke_width)?;
                out.literal(",\"color_role\":")?; out.quoted(&vector.color_role)?;
            }
            DisplayItem::Image(image) => {
                out.literal(",\"kind\":\"image\",\"request_id\":")?; out.integer(image.request_id)?;
                out.literal(",\"alt_text\":")?; out.quoted(&image.alt_text)?;
                out.literal(",\"destination\":")?; out.quoted(&image.destination)?;
                out.literal(",\"is_resolved\":false")?;
            }
            DisplayItem::Anchor(anchor) => {
                out.literal(",\"kind\":\"anchor\",\"anchor_id\":")?; out.quoted(&anchor.anchor_id)?;
                out.literal(",\"is_heading\":")?; out.boolean(anchor.is_heading)?;
                out.literal(",\"level\":")?; out.integer(u64::from(anchor.level))?;
            }
            DisplayItem::Clip(_) => return Err(NativeDisplayError::InvalidOutput.into()),
        }
        out.literal("}")?;
    }
    out.literal("],\"next_display_item\":")?;
    if end < display.display().items().len() { out.integer(end as u64)?; }
    else { out.literal("null")?; }
    Ok(())
}

fn rectangle(out: &mut Output, rect: DisplayRect) -> Result<(), ReaderSessionError> {
    out.literal("{\"x\":")?; real(out, rect.x)?;
    out.literal(",\"y\":")?; real(out, rect.y)?;
    out.literal(",\"width\":")?; real(out, rect.width)?;
    out.literal(",\"height\":")?; real(out, rect.height)?;
    out.literal("}")?;
    Ok(())
}
fn real(out: &mut Output, value: f32) -> Result<(), ReaderSessionError> {
    if !value.is_finite() { return Err(NativeDisplayError::InvalidOutput.into()); }
    out.literal(&value.to_string())?;
    Ok(())
}
fn vector_shape(shape: fcb::document::NativeVectorShape) -> &'static str {
    use fcb::document::NativeVectorShape;
    match shape {
        NativeVectorShape::HorizontalRule => "horizontal-rule",
        NativeVectorShape::TableBorder => "table-border",
        NativeVectorShape::CalloutAccentBar => "quote-accent",
        NativeVectorShape::CheckboxOutline => "checkbox-outline",
        NativeVectorShape::CheckboxCheck => "checkbox-check",
        NativeVectorShape::DiagramBox => "diagram-box",
        NativeVectorShape::DiagramArrow => "diagram-arrow",
        NativeVectorShape::DiagramConnector => "diagram-connector",
    }
}

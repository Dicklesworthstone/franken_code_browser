#![forbid(unsafe_code)]

//! Markdown preview on the SAME immutable source as reader/search/outline.
//! FrankenMarkdown owns parsing, logical flow, headings and selection mapping.
//! This module owns publication, bounded responses and navigation composition.
//! It never opens paths, activates links, fetches assets or starts a runtime.

pub use fcb::document::reader::DocumentReadOptions as ReaderDocumentOptions;
use fcb::document::{TextSelectionRange, reader::{DocumentReader, DocumentReadError}};
use fcb_core::{DocumentGeneration, DocumentId};
use super::{check, range, ByteOffset, HostResponse, Output, ReaderSession,
    ReaderSessionError, SelectionIdentity, EXIT_OK, MAX_READER_CONTEXT_BYTES, MAX_READER_WINDOW_BYTES};

pub const MAX_READER_DOCUMENT_PAGE: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentCopyMode {
    /// The selected upstream rendered UTF-8 text, NOT the original Markdown.
    RenderedText,
    /// Exact bytes of the enclosing Markdown region, NOT per-glyph provenance.
    EnclosingMarkdown,
}
impl From<DocumentReadError> for ReaderSessionError {
    fn from(error: DocumentReadError) -> Self { Self::Document(error) }
}

impl ReaderSession {
    pub fn document_generation(&self) -> Option<u64> {
        self.document.as_ref().map(|document| document.generation().get())
    }

    /// Prepare or explicitly reflow a document on the host's worker. Parsing is
    /// synchronous and bounded by the existing document adapter's input/flow
    /// limits; cancellation brackets the upstream call, not a hard deadline.
    /// Private layout AND complete first-page response precede replacement. Old
    /// preview, source, search and outline remain usable on any preparation error.
    pub fn prepare_document(&mut self, generation: u64, options: ReaderDocumentOptions,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        self.document_attempt(generation)?;
        check(&mut canceled)?;
        let [layout_id, source_id] = self.allocations()?;
        let id = DocumentId::new(self.owner(), 1).map_err(|_| ReaderSessionError::IdentityExhausted)?;
        let generation = DocumentGeneration::new(self.owner(), generation)
            .map_err(|_| ReaderSessionError::IdentityExhausted)?;
        let candidate = DocumentReader::prepare(&self.capture, id, generation, options,
            &self.budget, layout_id, &mut canceled)?.into_owned(&self.budget, source_id)?;
        check(&mut canceled)?;
        let mut out = self.output("document-prepare")?;
        summary(&mut out, &candidate)?;
        encode_window(&mut out, &candidate, 0, 64, &mut canceled)?;
        encode_headings(&mut out, &candidate, 0, 64, &mut canceled)?;
        out.literal("}\n")?;
        let response = self.finish_output(out, EXIT_OK, &mut canceled)?;
        check(&mut canceled)?;
        self.document = Some(candidate); // Old layout destruction is worker work.
        Ok(response)
    }

    /// Zero-based logical flow lines. A page is not partial source coverage.
    /// Viewport requests do not parse, rebuild, read source or change generation.
    pub fn document_window(&mut self, generation: u64, first: usize, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        page_limit(count)?; check(&mut canceled)?;
        self.document_snapshot(generation)?;
        let mut out = self.output("document-window")?;
        let document = self.document_snapshot(generation)?;
        summary(&mut out, document)?;
        encode_window(&mut out, document, first, count, &mut canceled)?;
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    /// Canonical upstream slugs and original-source regions. Heading indexes are
    /// paging offsets only; activation uses the slug under its document generation.
    pub fn document_headings(&mut self, generation: u64, first: usize, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        page_limit(count)?; check(&mut canceled)?;
        self.document_snapshot(generation)?;
        let mut out = self.output("document-headings")?;
        let document = self.document_snapshot(generation)?;
        summary(&mut out, document)?;
        encode_headings(&mut out, document, first, count, &mut canceled)?;
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    pub fn document_at_heading(&mut self, generation: u64, slug: &str, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        page_limit(count)?; check(&mut canceled)?;
        self.document_snapshot(generation)?;
        let mut out = self.output("document-heading")?;
        let document = self.document_snapshot(generation)?;
        let first = document.window_at_heading(slug, count)?.first_index();
        let heading = document.heading(slug)?;
        summary(&mut out, document)?;
        out.literal(",\"heading_slug\":")?; out.quoted(&heading.slug)?;
        out.literal(",\"heading_original_range\":")?; out.range(document.original_span(heading.source_span)?)?;
        encode_window(&mut out, document, first, count, &mut canceled)?;
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    /// Source/search-to-preview and reflow restoration use original bytes, not
    /// stale virtual row numbers. Navigation lands at the first logical row of
    /// the enclosing mapped region; it does not manufacture a glyph position.
    pub fn document_at_source(&mut self, generation: u64, offset: u64, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        page_limit(count)?; check(&mut canceled)?;
        self.document_snapshot(generation)?;
        let mut out = self.output("document-from-source")?;
        let document = self.document_snapshot(generation)?;
        let first = document.window_at_source(ByteOffset::new(offset), count)?.first_index();
        check(&mut canceled)?;
        summary(&mut out, document)?;
        out.literal(",\"original_anchor\":")?; out.integer(offset)?;
        encode_window(&mut out, document, first, count, &mut canceled)?;
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    /// Preview-to-source: select the upstream enclosing Markdown region through
    /// the ordinary reader's exact original-byte/decoded-selection round trip.
    /// Rendered offsets are UTF-8 bytes in this document generation, NOT UTF-16,
    /// glyph indices, original bytes, or virtual row numbers.
    pub fn document_selection_source(&mut self, generation: u64, start: usize, end: usize,
        context: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        if context > MAX_READER_CONTEXT_BYTES { return Err(ReaderSessionError::InvalidLimits); }
        check(&mut canceled)?;
        rendered_limit(start, end)?;
        let selected = self.document_snapshot(generation)?.selection(TextSelectionRange::new(start, end))?
            .enclosing_original_range;
        if selected.is_empty() { return Err(DocumentReadError::InvalidRange.into()); }
        let requested = range(selected.start().get().saturating_sub(context as u64 + 4),
            selected.end().get().saturating_add(context as u64 + 4).min(self.length()))?;
        let identity = SelectionIdentity::Document { generation, rendered_start: start as u64, rendered_end: end as u64 };
        self.window("document-source", requested, MAX_READER_WINDOW_BYTES,
            Some((identity, selected)), None, &mut canceled)
    }

    /// Explicit export data only; no clipboard, file write or external action.
    /// The two copy domains intentionally have different payloads and semantics.
    /// Never shorten a selection silently to fit an output or byte budget.
    pub fn copy_document_selection(&mut self, generation: u64, start: usize, end: usize,
        mode: DocumentCopyMode, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?; rendered_limit(start, end)?;
        self.document_snapshot(generation)?;
        let mut out = self.output("document-copy")?;
        let document = self.document_snapshot(generation)?;
        let selection = document.selection(TextSelectionRange::new(start, end))?;
        if selection.enclosing_original_bytes.len() > MAX_READER_WINDOW_BYTES {
            return Err(ReaderSessionError::InvalidLimits);
        }
        out.literal(",")?;
        SelectionIdentity::Document { generation, rendered_start: start as u64, rendered_end: end as u64 }.encode(&mut out)?;
        out.literal(",\"enclosing_original_range\":")?; out.range(selection.enclosing_original_range)?;
        match mode {
            DocumentCopyMode::RenderedText => {
                out.literal(",\"copy_domain\":\"rendered-text-utf8\",\"text\":")?;
                out.quoted(selection.rendered_text)?;
            }
            DocumentCopyMode::EnclosingMarkdown => {
                out.literal(",\"copy_domain\":\"enclosing-original-markdown\",\"original_hex\":")?;
                out.hex(selection.enclosing_original_bytes)?;
            }
        }
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    pub fn clear_document(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        self.document_attempt(generation)?; check(&mut canceled)?;
        let mut out = self.output("document-clear")?;
        out.literal(",\"document_generation\":")?; out.integer(generation)?;
        out.literal(",\"document_ready\":false}\n")?;
        let response = self.finish_output(out, EXIT_OK, &mut canceled)?;
        check(&mut canceled)?;
        self.document = None;
        Ok(response)
    }
    fn document_attempt(&mut self, generation: u64) -> Result<(), ReaderSessionError> {
        if generation == 0 || generation <= self.last_document_attempt {
            return Err(DocumentReadError::StaleGeneration.into());
        }
        self.last_document_attempt = generation;
        Ok(())
    }
    fn document_snapshot(&self, generation: u64) -> Result<&DocumentReader<'static>, ReaderSessionError> {
        let document = self.document.as_ref().ok_or(ReaderSessionError::MissingDocument)?;
        if document.generation().get() != generation { return Err(DocumentReadError::StaleGeneration.into()); }
        // The owned layout pins the SAME immutable backing; it must never name
        // another file/capture merely because the reader's label looks equal.
        if document.capture().request() != self.capture.request()
            || !std::ptr::eq(document.capture().bytes(), self.capture.bytes()) {
            return Err(DocumentReadError::StaleCapture.into());
        }
        Ok(document)
    }
}

fn page_limit(count: usize) -> Result<(), ReaderSessionError> {
    if !(1..=MAX_READER_DOCUMENT_PAGE).contains(&count) { return Err(ReaderSessionError::InvalidLimits); }
    Ok(())
}
fn rendered_limit(start: usize, end: usize) -> Result<(), ReaderSessionError> {
    if end <= start || end - start > MAX_READER_WINDOW_BYTES { return Err(ReaderSessionError::InvalidRange); }
    Ok(())
}
// Shared by single-reader and multi-pane desk hosts; neither owns a second flow engine.
pub(crate) fn summary(out: &mut Output, document: &DocumentReader<'_>) -> Result<(), ReaderSessionError> {
    out.literal(",\"document_generation\":")?; out.integer(document.generation().get())?;
    out.literal(",\"document_ready\":true,\"layout_complete\":true,\"rendering\":\"logical-frankenmarkdown-flow\",\"native_shaped\":false,\"source_mapping\":\"enclosing-regions-not-glyph-exact\",\"width_columns\":")?;
    out.integer(document.options().width_columns as u64)?;
    out.literal(",\"total_flow_lines\":")?; out.integer(document.total_lines() as u64)?;
    out.literal(",\"total_headings\":")?; out.integer(document.headings().len() as u64)?;
    out.literal(",\"rendered_utf8_bytes\":")?; out.integer(document.rendered_text().len() as u64)?;
    out.literal(",\"parser_source_base\":")?; out.integer(document.source_base() as u64)?;
    Ok(())
}
pub(crate) fn encode_window(out: &mut Output, document: &DocumentReader<'_>, first: usize, count: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(), ReaderSessionError> {
    let window = document.window(first, count)?;
    out.literal(",\"first_flow_line\":")?; out.integer(window.first_index() as u64)?;
    out.literal(",\"flow_lines\":[")?;
    for (i, line) in window.lines().iter().enumerate() {
        check(canceled)?;
        if i != 0 { out.literal(",")?; }
        out.literal("{\"flow_line\":")?; out.integer(line.line_index as u64)?;
        out.literal(",\"text\":")?; out.quoted(&line.rendered_text)?;
        out.literal(",\"rendered_utf8_range\":{\"start\":")?; out.integer(line.rendered_range.start as u64)?;
        out.literal(",\"end\":")?; out.integer(line.rendered_range.end as u64)?;
        out.literal("},\"enclosing_original_range\":")?;
        let original = document.original_span(line.source_span)?;
        if original.is_empty() { out.literal("null")?; } else { out.range(original)?; }
        out.literal("}")?;
    }
    out.literal("],\"next_flow_line\":")?;
    if let Some(next) = window.next_index() { out.integer(next as u64)?; } else { out.literal("null")?; }
    out.literal(",\"whole_document_visible\":")?; out.boolean(window.whole_document_visible())?;
    Ok(())
}
pub(crate) fn encode_headings(out: &mut Output, document: &DocumentReader<'_>, first: usize, count: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(), ReaderSessionError> {
    let headings = document.headings();
    if first > headings.len() { return Err(ReaderSessionError::InvalidRange); }
    let end = first.saturating_add(count).min(headings.len());
    out.literal(",\"headings\":[")?;
    for (i, heading) in headings[first..end].iter().enumerate() {
        check(canceled)?;
        if i != 0 { out.literal(",")?; }
        out.literal("{\"slug\":")?; out.quoted(&heading.slug)?;
        out.literal(",\"title\":")?; out.quoted(&heading.title)?;
        out.literal(",\"rendered_utf8_offset\":")?; out.integer(heading.rendered_offset as u64)?;
        out.literal(",\"original_range\":")?; out.range(document.original_span(heading.source_span)?)?;
        out.literal("}")?;
    }
    out.literal("],\"next_heading\":")?;
    if end < headings.len() { out.integer(end as u64)?; } else { out.literal("null")?; }
    Ok(())
}

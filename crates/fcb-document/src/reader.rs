#![forbid(unsafe_code)]

//! Retained Markdown reading over an explicitly supplied complete capture.
//! Parsing, logical flow and source mapping stay in FrankenMarkdown. Preparing
//! a reader is bounded-input worker work, not an input/redraw callback. The
//! upstream synchronous call is not preemptible; cancellation brackets it.
//! No link, image, include, executable, filesystem or network access occurs.

use std::{mem::size_of, sync::Arc};
use fcb_core::{ByteLength, ByteOffset, ByteRange, DocumentGeneration, DocumentId,
    ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_source::CompleteCapture;
use crate::{DocumentBudgets, DocumentError, DocumentFlowLine, DocumentSession,
    DocumentViewConstraints, HeadlessDocumentOutput, HeadingSourceAnchor,
    SourceSpan, TextSelectionRange};

pub const MAX_DOCUMENT_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_DOCUMENT_WINDOW_LINES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentReadOptions {
    /// Logical character-cell width, NOT native font shaping or physical pixels.
    pub width_columns: u32,
    pub max_source_bytes: usize,
    pub max_flow_lines: usize,
    pub max_flow_items: usize,
    pub max_blocks: usize,
}
impl Default for DocumentReadOptions {
    fn default() -> Self {
        Self { width_columns: 100, max_source_bytes: 64 * 1024,
            max_flow_lines: 8192, max_flow_items: 8192, max_blocks: 4096 }
    }
}
impl DocumentReadOptions {
    pub fn validate(self) -> Result<(), DocumentReadError> {
        if !(4..=512).contains(&self.width_columns)
            || self.max_source_bytes > MAX_DOCUMENT_SOURCE_BYTES
            || !(1..=32768).contains(&self.max_flow_lines)
            || !(1..=32768).contains(&self.max_flow_items)
            || !(1..=8192).contains(&self.max_blocks) {
            return Err(DocumentReadError::InvalidLimits);
        }
        Ok(())
    }
}

/// Redacted stable errors: upstream diagnostics may contain source text and are
/// deliberately not copied into ordinary application logs or machine errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DocumentReadError {
    InvalidLimits, SourceLimit, InvalidUtf8, OwnerMismatch, ResourceDenied,
    Canceled, EngineBudget, EngineFailure, InvalidOutput, InvalidRange,
    HeadingNotFound, StaleCapture, StaleGeneration,
}
impl DocumentReadError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "DOCUMENT_READ_INVALID_LIMITS",
            Self::SourceLimit => "DOCUMENT_READ_SOURCE_LIMIT",
            Self::InvalidUtf8 => "DOCUMENT_READ_INVALID_UTF8",
            Self::OwnerMismatch => "DOCUMENT_READ_OWNER_MISMATCH",
            Self::ResourceDenied => "DOCUMENT_READ_RESOURCE_DENIED",
            Self::Canceled => "DOCUMENT_READ_CANCELED",
            Self::EngineBudget => "DOCUMENT_READ_FLOW_LIMIT",
            Self::EngineFailure => "DOCUMENT_READ_ENGINE_ERROR",
            Self::InvalidOutput => "DOCUMENT_READ_INVALID_PROVENANCE",
            Self::InvalidRange => "DOCUMENT_READ_INVALID_RANGE",
            Self::HeadingNotFound => "DOCUMENT_READ_HEADING_NOT_FOUND",
            Self::StaleCapture => "DOCUMENT_READ_STALE_CAPTURE",
            Self::StaleGeneration => "DOCUMENT_READ_STALE_GENERATION",
        }
    }
}
impl std::fmt::Display for DocumentReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for DocumentReadError {}
impl From<DocumentError> for DocumentReadError {
    fn from(error: DocumentError) -> Self {
        match error {
            DocumentError::InvalidUtf8 => Self::InvalidUtf8,
            DocumentError::OwnerMismatch => Self::OwnerMismatch,
            DocumentError::Canceled => Self::Canceled,
            DocumentError::LimitExceeded
                | DocumentError::Flow(franken_markdown::FlowError::BudgetExceeded { .. }) => Self::EngineBudget,
            _ => Self::EngineFailure,
        }
    }
}

/// One immutable source and a completed logical layout. Source ownership remains
/// with the host; this separate lease covers preparation and retained output.
/// A viewport never reparses, rereads, clones the document, or changes identity.
pub struct DocumentReader<'capture> {
    capture: &'capture CompleteCapture,
    session: DocumentSession,
    output: HeadlessDocumentOutput,
    source_base: usize,
    options: DocumentReadOptions,
    _lease: ResourceLease,
}
impl<'capture> DocumentReader<'capture> {
    pub fn prepare(capture: &'capture CompleteCapture, id: DocumentId,
        generation: DocumentGeneration, options: DocumentReadOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, DocumentReadError> {
        options.validate()?;
        if canceled() { return Err(DocumentReadError::Canceled); }
        let owner = capture.request().file().owner();
        if id.owner() != owner || generation.owner() != owner { return Err(DocumentReadError::OwnerMismatch); }
        let bytes = capture.bytes();
        if bytes.len() > options.max_source_bytes { return Err(DocumentReadError::SourceLimit); }
        std::str::from_utf8(bytes).map_err(|_| DocumentReadError::InvalidUtf8)?;
        // Reserve before the parser/session/semantic-fixture allocations. This
        // conservative managed envelope is not an OS process-footprint bound.
        let charge = bytes.len().checked_mul(512)
            .and_then(|n| n.checked_add(options.max_flow_lines * 256))
            .and_then(|n| n.checked_add(options.max_flow_items * 512))
            .and_then(|n| n.checked_add(1024 * 1024 + size_of::<Self>()))
            .ok_or(DocumentReadError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| DocumentReadError::ResourceDenied)?;
        // A UTF-8 BOM is a source encoding header, not Markdown syntax. Strip
        // ONLY that header for the parser, then translate every source span back
        // to the original capture. Internal parser-view bytes confer no new ID.
        let source_base = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) { 3 } else { 0 };
        let parser_capture = CompleteCapture::new(*capture.request(),
            ByteLength::new((bytes.len() - source_base) as u64), Arc::from(&bytes[source_base..]))
            .map_err(|_| DocumentReadError::InvalidRange)?;
        if canceled() { return Err(DocumentReadError::Canceled); }
        let session = DocumentSession::new(id, &parser_capture, generation)?;
        let output = session.consume_headless(generation,
            DocumentViewConstraints { viewport_width: options.width_columns,
                line_height: 1, char_width: 1, max_viewport_lines: None },
            DocumentBudgets { max_blocks: options.max_blocks, max_bytes: options.max_source_bytes,
                max_lines: options.max_flow_lines, max_items: options.max_flow_items })?;
        if canceled() { return Err(DocumentReadError::Canceled); }
        let reader = Self { capture, session, output, source_base, options, _lease: lease };
        reader.validate_output(&mut canceled)?;
        Ok(reader)
    }
    pub fn capture(&self) -> &'capture CompleteCapture { self.capture }
    pub fn generation(&self) -> DocumentGeneration { self.session.generation() }
    pub fn options(&self) -> DocumentReadOptions { self.options }
    pub fn source_base(&self) -> usize { self.source_base }
    pub fn total_lines(&self) -> usize { self.output.lines.len() }
    pub fn headings(&self) -> &[HeadingSourceAnchor] { self.output.source_map.headings() }
    pub fn rendered_text(&self) -> &str { self.output.source_map.rendered_text() }

    /// Equality of file/revision/digest values alone does not rebind a delayed
    /// result to another capture object. No content hash serves as authority.
    pub fn validate_delivery(&self, capture: &CompleteCapture, generation: DocumentGeneration)
        -> Result<(), DocumentReadError> {
        if !std::ptr::eq(self.capture, capture) { return Err(DocumentReadError::StaleCapture); }
        if generation != self.generation() { return Err(DocumentReadError::StaleGeneration); }
        Ok(())
    }
    /// Convert upstream parser-relative primary-source bytes, including the BOM
    /// translation. Empty/synthetic spans must not be called exact source text.
    pub fn original_span(&self, span: SourceSpan) -> Result<ByteRange, DocumentReadError> {
        let text = self.session.source_text();
        if span.start > span.end || text.get(span.start..span.end).is_none() {
            return Err(DocumentReadError::InvalidOutput);
        }
        let start = span.start.checked_add(self.source_base).ok_or(DocumentReadError::InvalidRange)?;
        let end = span.end.checked_add(self.source_base).ok_or(DocumentReadError::InvalidRange)?;
        ByteRange::new(ByteOffset::new(start as u64), ByteOffset::new(end as u64))
            .map_err(|_| DocumentReadError::InvalidRange)
    }
    /// Zero-based logical flow rows, not original source line numbers. EOF is a
    /// valid empty window. No prefix materialization is done by this operation.
    pub fn window(&self, first: usize, count: usize) -> Result<DocumentWindow<'_, 'capture>, DocumentReadError> {
        if count == 0 || count > MAX_DOCUMENT_WINDOW_LINES || first > self.total_lines() {
            return Err(DocumentReadError::InvalidRange);
        }
        let end = first.saturating_add(count).min(self.total_lines());
        Ok(DocumentWindow { reader: self, first, end })
    }
    /// Resolve an upstream canonical heading identity, never cached geometry.
    /// The caller supplies the literal slug without URL decoding or a leading #.
    pub fn heading(&self, slug: &str) -> Result<&HeadingSourceAnchor, DocumentReadError> {
        if slug.is_empty() || slug.len() > 4096 { return Err(DocumentReadError::HeadingNotFound); }
        self.headings().iter().find(|heading| heading.slug == slug).ok_or(DocumentReadError::HeadingNotFound)
    }
    pub fn window_at_heading(&self, slug: &str, count: usize)
        -> Result<DocumentWindow<'_, 'capture>, DocumentReadError> {
        let heading = self.heading(slug)?;
        let first = self.output.lines.partition_point(|line| line.rendered_range.end <= heading.rendered_offset);
        self.window(first, count)
    }
    /// Rendered text and enclosing original Markdown are deliberately different
    /// copy operations. The enclosing block can contain delimiters or unrelated
    /// text; it is NOT advertised as the literal source of every selected glyph.
    pub fn selection(&self, range: TextSelectionRange) -> Result<DocumentReadingSelection<'_>, DocumentReadError> {
        if range.start >= range.end { return Err(DocumentReadError::InvalidRange); }
        let rendered = self.rendered_text().get(range.start..range.end).ok_or(DocumentReadError::InvalidRange)?;
        let source = self.session.source_text();
        let enclosing = self.output.source_map.copy_enclosing_block(range, source)
            .map_err(|_| DocumentReadError::InvalidRange)?;
        let start = (enclosing.as_ptr() as usize).checked_sub(source.as_ptr() as usize)
            .ok_or(DocumentReadError::InvalidOutput)?;
        let end = start.checked_add(enclosing.len()).ok_or(DocumentReadError::InvalidOutput)?;
        if source.get(start..end) != Some(enclosing) { return Err(DocumentReadError::InvalidOutput); }
        let original = self.original_span(SourceSpan { start, end })?;
        let (start, end) = original.as_usize_bounds().map_err(|_| DocumentReadError::InvalidRange)?;
        let bytes = self.capture.bytes().get(start..end).ok_or(DocumentReadError::InvalidOutput)?;
        Ok(DocumentReadingSelection { rendered_range: range, rendered_text: rendered,
            enclosing_original_range: original, enclosing_original_bytes: bytes })
    }
    fn validate_output(&self, canceled: &mut impl FnMut() -> bool) -> Result<(), DocumentReadError> {
        let length = self.session.source_text().len();
        if self.output.consumed_bytes != length || self.output.source_map.source_len() != length
            || self.total_lines() > self.options.max_flow_lines
            || self.output.source_map.elements().len() > self.options.max_flow_items {
            return Err(DocumentReadError::InvalidOutput);
        }
        let mut previous_end = 0;
        for (i, line) in self.output.lines.iter().enumerate() {
            if canceled() { return Err(DocumentReadError::Canceled); }
            if line.line_index != i || line.rendered_range.start < previous_end
                || self.rendered_text().get(line.rendered_range.start..line.rendered_range.end)
                    .is_none_or(|text| text.trim_end() != line.rendered_text) {
                return Err(DocumentReadError::InvalidOutput);
            }
            self.original_span(line.source_span)?;
            previous_end = line.rendered_range.end;
        }
        for heading in self.headings() {
            if canceled() { return Err(DocumentReadError::Canceled); }
            self.original_span(heading.source_span)?;
            if !self.rendered_text().is_char_boundary(heading.rendered_offset) {
                return Err(DocumentReadError::InvalidOutput);
            }
        }
        Ok(())
    }
}

pub struct DocumentWindow<'reader, 'capture> {
    reader: &'reader DocumentReader<'capture>,
    first: usize,
    end: usize,
}
impl DocumentWindow<'_, '_> {
    pub fn first_index(&self) -> usize { self.first }
    pub fn lines(&self) -> &[DocumentFlowLine] { &self.reader.output.lines[self.first..self.end] }
    pub fn next_index(&self) -> Option<usize> { (self.end < self.reader.total_lines()).then_some(self.end) }
    pub fn whole_document_visible(&self) -> bool { self.first == 0 && self.end == self.reader.total_lines() }
}

pub struct DocumentReadingSelection<'a> {
    pub rendered_range: TextSelectionRange,
    pub rendered_text: &'a str,
    pub enclosing_original_range: ByteRange,
    pub enclosing_original_bytes: &'a [u8],
}

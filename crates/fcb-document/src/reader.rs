#![forbid(unsafe_code)]

//! Retained Markdown reading over an explicitly supplied complete capture.
//! Parsing, logical flow and source mapping stay in FrankenMarkdown. Preparing
//! a reader is bounded-input worker work, not an input/redraw callback. The
//! upstream synchronous call is not preemptible; cancellation brackets it.
//! No link, image, include, executable, filesystem or network access occurs.

use std::{borrow::Cow, mem::size_of, sync::Arc};
use fcb_core::{ByteLength, ByteOffset, ByteRange, DocumentGeneration, DocumentId,
    ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_source::CompleteCapture;
use crate::{DocumentBudgets, DocumentError, DocumentFlowLine, DocumentSession,
    DocumentViewConstraints, HeadlessDocumentOutput, HeadingSourceAnchor,
    SourceSpan, TextSelectionRange};

mod heading_provenance;
mod source_encoding;
pub(crate) use source_encoding::DocumentSourceEncoding;

/// Opt-in ceiling for both captured bytes and decoded parser input. The
/// ordinary read options retain their smaller 64 KiB default.
pub const MAX_DOCUMENT_SOURCE_BYTES: usize = 512 * 1024;
/// Automatic workspace heading extraction retains its separate source bound;
/// admitting a larger retained preview does not expand background search work.
pub const MAX_HEADING_SEARCH_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_DOCUMENT_WINDOW_LINES: usize = 1024;
const MAX_DOCUMENT_PARSE_WORKSPACE_BYTES: usize = 256 * 1024 * 1024;

pub(crate) fn document_parse_workspace(source: &[u8]) -> Result<usize, DocumentReadError> {
    crate::links::parse_workspace_with_limit(source, MAX_DOCUMENT_PARSE_WORKSPACE_BYTES)
        .map_err(|_| DocumentReadError::EngineBudget)
}

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

/// One immutable source and a completed logical layout. Borrowed construction
/// leaves source ownership with the host; into_owned admits an independent pin.
/// A viewport never reparses, rereads, clones the document, or changes identity.
pub struct DocumentReader<'capture> {
    capture: Cow<'capture, CompleteCapture>,
    session: DocumentSession,
    output: HeadlessDocumentOutput,
    source_encoding: DocumentSourceEncoding,
    options: DocumentReadOptions,
    _lease: ResourceLease,
    _source_lease: Option<ResourceLease>,
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
        // Reserve decoder/map storage before allocating it. The parser and
        // provenance audit always consume the decoded UTF-8 view, while source
        // selection and capture identity remain in the original byte domain.
        let decoder_charge = DocumentSourceEncoding::workspace(bytes)?;
        let fixed_charge = decoder_charge.checked_add(options.max_flow_lines * 256)
            .and_then(|n| n.checked_add(options.max_flow_items * 512))
            .and_then(|n| n.checked_add(1024 * 1024 + size_of::<Self>()))
            .ok_or(DocumentReadError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(fixed_charge as u64))
            .map_err(|_| DocumentReadError::ResourceDenied)?;
        let source_encoding = DocumentSourceEncoding::build(bytes, options.max_source_bytes)?;
        if canceled() { return Err(DocumentReadError::Canceled); }
        let text = source_encoding.text(bytes)?;
        // Reference amplification must be measured in parser input, not raw
        // UTF-16, whose interleaved zero bytes hide the upstream "]:" gate.
        let parse_charge = document_parse_workspace(text.as_bytes())?;
        let charge = fixed_charge.checked_add(parse_charge).ok_or(DocumentReadError::InvalidLimits)?;
        lease.reconcile(ByteLength::new(charge as u64)).map_err(|_| DocumentReadError::ResourceDenied)?;
        let parser_capture = CompleteCapture::new(*capture.request(),
            ByteLength::new(text.len() as u64), Arc::from(text.as_bytes()))
            .map_err(|_| DocumentReadError::InvalidRange)?;
        if canceled() { return Err(DocumentReadError::Canceled); }
        let session = DocumentSession::new(id, &parser_capture, generation)?;
        let output = session.consume_headless(generation,
            DocumentViewConstraints { viewport_width: options.width_columns,
                line_height: 1, char_width: 1, max_viewport_lines: None },
            DocumentBudgets { max_blocks: options.max_blocks, max_bytes: options.max_source_bytes,
                max_lines: options.max_flow_lines, max_items: options.max_flow_items })?;
        if canceled() { return Err(DocumentReadError::Canceled); }
        let reader = Self { capture: Cow::Borrowed(capture), session, output, source_encoding, options,
            _lease: lease, _source_lease: None };
        reader.validate_output(&mut canceled)?;
        Ok(reader)
    }

    /// Transfer a prepared layout into independent ownership without reparsing,
    /// rehashing, copying source bytes, or constructing a self-referential value.
    /// CompleteCapture::clone shares immutable backing. Charge that backing for
    /// the owned reader's whole lifetime even if the original host pin goes away.
    /// The returned capture object is new; validate_delivery still requires the
    /// exact object returned by capture(), not merely equal IDs or a digest.
    pub fn into_owned(self, budget: &ResourceBudget, allocation: ResourceAllocationId)
        -> Result<DocumentReader<'static>, DocumentReadError> {
        let charge = self.capture.bytes().len().checked_add(size_of::<CompleteCapture>() + 64)
            .ok_or(DocumentReadError::ResourceDenied)?;
        let source_lease = budget.try_reserve_managed(self.capture.request().file().owner(), allocation,
            ByteLength::new(charge as u64)).map_err(|_| DocumentReadError::ResourceDenied)?;
        Ok(DocumentReader { capture: Cow::Owned(self.capture.into_owned()), session: self.session,
            output: self.output, source_encoding: self.source_encoding, options: self.options,
            _lease: self._lease, _source_lease: Some(source_lease) })
    }

    pub fn capture(&self) -> &CompleteCapture { self.capture.as_ref() }
    pub fn generation(&self) -> DocumentGeneration { self.session.generation() }
    pub fn options(&self) -> DocumentReadOptions { self.options }
    /// Original encoding-header length; parser offsets need original_span(),
    /// since UTF-16 source coordinates are not a constant byte translation.
    pub fn source_base(&self) -> usize { self.source_encoding.source_base() }
    pub(crate) fn source_encoding(&self) -> &DocumentSourceEncoding { &self.source_encoding }
    pub(crate) fn parser_text(&self) -> &str { self.session.source_text() }
    pub fn total_lines(&self) -> usize { self.output.lines.len() }
    pub fn headings(&self) -> &[HeadingSourceAnchor] { self.output.source_map.headings() }
    pub fn rendered_text(&self) -> &str { self.output.source_map.rendered_text() }

    /// Equality of file/revision/digest values alone does not rebind a delayed
    /// result to another capture object. No content hash serves as authority.
    pub fn validate_delivery(&self, capture: &CompleteCapture, generation: DocumentGeneration)
        -> Result<(), DocumentReadError> {
        if !std::ptr::eq(self.capture.as_ref(), capture) { return Err(DocumentReadError::StaleCapture); }
        if generation != self.generation() { return Err(DocumentReadError::StaleGeneration); }
        Ok(())
    }
    /// Convert upstream parser-relative UTF-8 coordinates to the exact original
    /// capture, including UTF-16 and its BOM. Empty/synthetic spans must not be
    /// called exact source text.
    pub fn original_span(&self, span: SourceSpan) -> Result<ByteRange, DocumentReadError> {
        self.source_encoding.original_span(self.capture.bytes(), span)
    }
    /// Check an original captured-byte anchor independently of whether upstream
    /// Markdown provenance maps it to a rendered row. Invalid encoding boundaries
    /// cannot become fallback navigation, even when the layout is unqualified.
    pub fn validate_source_offset(&self, original: ByteOffset) -> Result<(), DocumentReadError> {
        let offset = usize::try_from(original.get()).map_err(|_| DocumentReadError::InvalidRange)?;
        self.source_encoding.validate_original_offset(self.capture.bytes(), offset)
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

    /// Navigate using a UTF-8 byte offset in this generation's retained logical
    /// rendered text, never original Markdown bytes or native glyph positions.
    /// Use the containing or next upstream flow row. Layout separators after
    /// the last row show that final row; a layout with no rows has only an empty
    /// window. This is navigation context, not a glyph/source correspondence.
    /// Interior scalar bytes are refused; rendered EOF is an empty window.
    pub fn window_at_rendered(&self, offset: usize, count: usize)
        -> Result<DocumentWindow<'_, 'capture>, DocumentReadError> {
        if !self.rendered_text().is_char_boundary(offset) {
            return Err(DocumentReadError::InvalidRange);
        }
        if offset == self.rendered_text().len() { return self.window(self.total_lines(), count); }
        let first = self.output.lines.partition_point(|line| line.rendered_range.end <= offset);
        // Upstream omits newline/blank-line separators from flow rows. Keep
        // their rendered matches usable without inventing a visible row.
        self.window(first.min(self.total_lines().saturating_sub(1)), count)
    }

    /// Navigate from original source to the FIRST logical row of its enclosing
    /// upstream-mapped source region. This is a block-level correspondence, not
    /// an exact glyph/column or a claim that all source bytes are rendered.
    /// Markdown delimiters belong to their upstream block even when no rendered
    /// inline names those bytes. Unmapped gaps, blocks without a rendered row,
    /// BOM bytes and interior encoded scalars are refused. Source EOF maps to
    /// the valid empty document window. Only retained map/flow metadata is read.
    pub fn window_at_source(&self, original: ByteOffset, count: usize)
        -> Result<DocumentWindow<'_, 'capture>, DocumentReadError> {
        if count == 0 || count > MAX_DOCUMENT_WINDOW_LINES { return Err(DocumentReadError::InvalidRange); }
        self.validate_source_offset(original)?;
        if original.get() == self.capture.bytes().len() as u64 {
            return self.window(self.total_lines(), count);
        }
        // Flow-row spans enclose the rendered inlines, not their full Markdown
        // block. In particular, heading markers, emphasis delimiters and link
        // destinations can lie outside every row span. The upstream provenance
        // graph retains the enclosing blocks in the same order as block_index.
        // Use that catalog without reconstructing Markdown syntax locally.
        for (index, block) in self.output.source_map.provenance_graph().root.children.iter().enumerate() {
            if block.kind != franken_markdown::span::ProvenanceKind::Block {
                return Err(DocumentReadError::InvalidOutput);
            }
            if !block.source_ranges.origin().is_primary() { continue; }
            for &span in block.source_ranges.spans() {
                let range = self.original_span(span)?;
                if range.start().get() <= original.get() && original.get() < range.end().get() {
                    return self.window_at_block(index, count);
                }
            }
        }
        Err(DocumentReadError::InvalidRange)
    }

    /// Select only an actual row associated with this retained upstream block.
    /// Empty headings and newline-only elements have no row; their rendered
    /// offset must not silently select the next block's text.
    fn window_at_block(&self, block_index: usize, count: usize)
        -> Result<DocumentWindow<'_, 'capture>, DocumentReadError> {
        let mut first = 0;
        for (index, element) in self.output.source_map.elements().iter().enumerate() {
            if element.block_index != block_index || element.rendered_range.is_empty() { continue; }
            while self.output.lines.get(first).is_some_and(|line|
                line.rendered_range.end <= element.rendered_range.start) { first += 1; }
            if let Some(line) = self.output.lines.get(first)
                && line.rendered_range.overlaps(element.rendered_range) {
                if !line.element_indices.contains(&index) { return Err(DocumentReadError::InvalidOutput); }
                return self.window(first, count);
            }
        }
        Err(DocumentReadError::InvalidRange)
    }

    /// Resolve an upstream canonical heading identity, never cached geometry.
    /// The caller supplies the literal slug without URL decoding or a leading #.
    pub fn heading(&self, slug: &str) -> Result<&HeadingSourceAnchor, DocumentReadError> {
        if slug.is_empty() || slug.len() > 4096 { return Err(DocumentReadError::HeadingNotFound); }
        let mut matches = self.headings().iter().filter(|heading| heading.slug == slug);
        let heading = matches.next().ok_or(DocumentReadError::HeadingNotFound)?;
        if matches.next().is_some() { return Err(DocumentReadError::InvalidOutput); }
        Ok(heading)
    }
    pub fn window_at_heading(&self, slug: &str, count: usize)
        -> Result<DocumentWindow<'_, 'capture>, DocumentReadError> {
        let heading = self.heading(slug)?;
        self.window_at_block(heading.block_index, count)
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
            enclosing_markdown_text: enclosing,
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
    /// Decoded UTF-8 Markdown for text presentation. The exact encoded capture
    /// remains separately available through enclosing_original_bytes.
    pub enclosing_markdown_text: &'a str,
    pub enclosing_original_range: ByteRange,
    pub enclosing_original_bytes: &'a [u8],
}

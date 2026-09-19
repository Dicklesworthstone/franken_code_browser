#![forbid(unsafe_code)]

//! Markdown reading on an exact desk pane, including repository imports and
//! restored checkpoints. FrankenMarkdown owns parsing, logical flow and source
//! maps through the existing DocumentReader. No paths or assets are opened.
//! Hosts own these optional, rebuildable layouts; every use checks the current
//! pane and source backing. Preparation, reflow and destruction are worker work.

use std::mem::size_of;
use fcb::BrowserSession;
use fcb::document::{TextSelectionRange, reader::{DocumentReader, DocumentReadError}};
use fcb_core::{DocumentId, DocumentGeneration};
use super::{DeskSession, DeskSessionError, DeskPaneId, DeskChange, DeskCommand,
    DeskError, HostResponse, Output, OutputError, ByteLength, ByteOffset,
    ReadingTarget, ReadingWindowOptions, ResourceLease, FileId, SourceRevision,
    MAX_DESK_COPY_BYTES, EXIT_OK, check};
use crate::host::reader::{ReaderSessionError, document as wire};
pub use fcb::document::reader::DocumentReadOptions as DeskDocumentOptions;
pub use crate::host::reader::document::DocumentCopyMode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeskDocumentError {
    Desk(DeskSessionError), Document(DocumentReadError), Reader(ReaderSessionError),
}
impl std::fmt::Display for DeskDocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Desk(e) => write!(f, "{e}"), Self::Document(e) => write!(f, "{e}"),
            Self::Reader(e) => write!(f, "{e}") }
    }
}
impl std::error::Error for DeskDocumentError {}
impl From<DeskSessionError> for DeskDocumentError { fn from(e: DeskSessionError) -> Self { Self::Desk(e) } }
impl From<DeskError> for DeskDocumentError { fn from(e: DeskError) -> Self { Self::Desk(e.into()) } }
impl From<DocumentReadError> for DeskDocumentError { fn from(e: DocumentReadError) -> Self { Self::Document(e) } }
impl From<ReaderSessionError> for DeskDocumentError { fn from(e: ReaderSessionError) -> Self { Self::Reader(e) } }
impl From<OutputError> for DeskDocumentError { fn from(e: OutputError) -> Self { Self::Desk(e.into()) } }
impl DeskDocumentError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Desk(e) => e.is_canceled(), Self::Document(DocumentReadError::Canceled)
                | Self::Reader(ReaderSessionError::Canceled)
                | Self::Reader(ReaderSessionError::Document(DocumentReadError::Canceled)) => true,
            Self::Reader(ReaderSessionError::App(e)) => e.is_canceled(), _ => false,
        }
    }
}

/// One independently reflowable pane. Its original capture shares the desk's
/// immutable backing; upstream parsing/flow has separate admitted storage.
/// The caller supplies fresh document generations across new objects. Reflow
/// additionally enforces a non-reusing attempt counter on this object. Layouts
/// are not authoritative user state and are not serialized into desk checkpoints.
pub struct DeskDocument {
    pane: DeskPaneId,
    reader: DocumentReader<'static>,
    last_attempt: u64,
    _metadata: ResourceLease,
}
impl DeskDocument {
    pub fn prepare(desk: &mut DeskSession, expected: u64, pane: DeskPaneId,
        generation: u64, options: DeskDocumentOptions, mut canceled: impl FnMut() -> bool)
        -> Result<Self, DeskDocumentError> {
        let source = desk.model().source(pane, expected)?;
        options.validate()?;
        if source.bytes().len() > options.max_source_bytes { return Err(DocumentReadError::SourceLimit.into()); }
        if generation == 0 { return Err(DocumentReadError::StaleGeneration.into()); }
        let label_bytes = source.logical_path().len();
        check(&mut canceled)?;
        let [view_id, metadata_id, layout_id, pin_id] = desk.ids()?;
        let view = desk.model().view(pane, expected, view_id)?;
        let owner = desk.model().owner();
        let metadata = desk.budget.try_reserve_managed(owner, metadata_id,
            ByteLength::new((2 * label_bytes + size_of::<Self>() + 4096) as u64))
            .map_err(|_| DocumentReadError::ResourceDenied)?;
        // The public capture adapter shares the Arc; it computes a digest on the
        // worker but never reopens a pathname or changes the source identity.
        let capture = BrowserSession::new(owner).prepare_search_capture(view.source().clone())
            .map_err(|_| DocumentReadError::StaleCapture)?;
        check(&mut canceled)?;
        let id = DocumentId::new(owner, pane.get()).map_err(|_| DeskError::IdentityExhausted)?;
        let generation_id = DocumentGeneration::new(owner, generation).map_err(|_| DeskError::IdentityExhausted)?;
        let reader = DocumentReader::prepare(capture.capture(), id, generation_id, options,
            &desk.budget, layout_id, &mut canceled)?.into_owned(&desk.budget, pin_id)?;
        check(&mut canceled)?;
        Ok(Self { pane, reader, last_attempt: generation, _metadata: metadata })
    }
    pub const fn pane(&self) -> DeskPaneId { self.pane }
    pub fn generation(&self) -> u64 { self.reader.generation().get() }
    pub const fn last_attempt(&self) -> u64 { self.last_attempt }
    pub fn source_file(&self) -> FileId { self.reader.capture().request().file() }
    pub fn source_revision(&self) -> SourceRevision { self.reader.capture().request().revision() }

    /// Validate membership as well as identity. Closing/replacing/restoring a
    /// pane invalidates this binding even though this object still pins old bytes.
    /// Returning to that exact source in a still-live pane can reuse its layout.
    pub fn validate_source(&self, desk: &DeskSession, expected: u64) -> Result<(), DeskDocumentError> {
        let source = desk.model().source(self.pane, expected)?;
        if source.file() != self.source_file() || source.revision() != self.source_revision()
            || !std::ptr::eq(source.bytes(), self.reader.capture().bytes()) {
            return Err(DocumentReadError::StaleCapture.into());
        }
        Ok(())
    }
    pub fn layout(&self, desk: &DeskSession, expected: u64, generation: u64)
        -> Result<&DocumentReader<'static>, DeskDocumentError> {
        self.validate_source(desk, expected)?;
        if generation != self.generation() { return Err(DocumentReadError::StaleGeneration.into()); }
        Ok(&self.reader)
    }

    /// Old layout and source navigation survive failed/canceled reflow. The
    /// complete initial response is prepared before swapping the accepted layout.
    pub fn reflow(&mut self, desk: &mut DeskSession, expected: u64, generation: u64,
        options: DeskDocumentOptions, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskDocumentError> {
        self.validate_source(desk, expected)?;
        if generation <= self.last_attempt { return Err(DocumentReadError::StaleGeneration.into()); }
        self.last_attempt = generation;
        let candidate = Self::prepare(desk, expected, self.pane, generation, options, &mut canceled)?;
        let response = candidate.overview(desk, expected, generation, &mut canceled)?;
        check(&mut canceled)?;
        *self = candidate;
        Ok(response)
    }
    pub fn overview(&self, desk: &mut DeskSession, expected: u64, generation: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskDocumentError> {
        let document = self.layout(desk, expected, generation)?;
        check(&mut canceled)?;
        let mut out = self.output(desk, "doc-prepare")?;
        wire::summary(&mut out, document)?;
        wire::encode_window(&mut out, document, 0, 64, &mut canceled)?;
        wire::encode_headings(&mut out, document, 0, 64, &mut canceled)?;
        out.literal("}\n")?;
        Ok(desk.finish(out, EXIT_OK, &mut canceled)?)
    }
    pub fn window(&self, desk: &mut DeskSession, expected: u64, generation: u64,
        first: usize, count: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskDocumentError> {
        page_limit(count)?;
        let document = self.layout(desk, expected, generation)?;
        check(&mut canceled)?;
        let mut out = self.output(desk, "doc-window")?;
        wire::summary(&mut out, document)?;
        wire::encode_window(&mut out, document, first, count, &mut canceled)?;
        out.literal("}\n")?;
        Ok(desk.finish(out, EXIT_OK, &mut canceled)?)
    }
    pub fn headings(&self, desk: &mut DeskSession, expected: u64, generation: u64,
        first: usize, count: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskDocumentError> {
        page_limit(count)?;
        let document = self.layout(desk, expected, generation)?;
        check(&mut canceled)?;
        let mut out = self.output(desk, "doc-headings")?;
        wire::summary(&mut out, document)?;
        wire::encode_headings(&mut out, document, first, count, &mut canceled)?;
        out.literal("}\n")?;
        Ok(desk.finish(out, EXIT_OK, &mut canceled)?)
    }
    pub fn at_source(&self, desk: &mut DeskSession, expected: u64, generation: u64,
        offset: u64, count: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskDocumentError> {
        page_limit(count)?;
        let document = self.layout(desk, expected, generation)?;
        check(&mut canceled)?;
        let first = document.window_at_source(ByteOffset::new(offset), count)?.first_index();
        let mut out = self.output(desk, "doc-sync")?;
        wire::summary(&mut out, document)?;
        out.literal(",\"original_anchor\":")?; out.integer(offset)?;
        wire::encode_window(&mut out, document, first, count, &mut canceled)?;
        out.literal("}\n")?;
        Ok(desk.finish(out, EXIT_OK, &mut canceled)?)
    }

    /// Source and preview use one original-byte anchor, never scroll percentages.
    /// This is a logical split payload, not native shaping or presented pixels.
    pub fn split(&self, desk: &mut DeskSession, expected: u64, generation: u64,
        offset: u64, count: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskDocumentError> {
        let preview = self.at_source(desk, expected, generation, offset, count, &mut canceled)?;
        let source = desk.window(expected, self.pane, Some(ReadingTarget::Byte(ByteOffset::new(offset))),
            ReadingWindowOptions { max_bytes: 64 * 1024, max_lines: count }, &mut canceled)?;
        self.layout(desk, expected, generation)?;
        let mut out = self.output(desk, "doc-split")?;
        out.literal(",\"document_generation\":")?; out.integer(generation)?;
        out.literal(",\"synchronization\":\"enclosing-source-region\",\"original_anchor\":")?; out.integer(offset)?;
        out.literal(",\"source\":")?; out.literal(source.as_str().trim_end())?;
        out.literal(",\"preview\":")?; out.literal(preview.as_str().trim_end())?;
        out.literal("}\n")?;
        Ok(desk.finish(out, EXIT_OK, &mut canceled)?)
    }

    /// Preview coordinates are rendered UTF-8 bytes in THIS layout generation.
    /// Navigation selects the enclosing Markdown region, not alleged glyph-exact
    /// source. It uses the ordinary desk transaction, including back/forward.
    pub fn select_source(&self, desk: &mut DeskSession, expected: u64, attempt: u64,
        generation: u64, start: usize, end: usize, mut canceled: impl FnMut() -> bool)
        -> Result<DeskChange, DeskDocumentError> {
        desk.validate_mutation(expected, attempt)?;
        rendered_limit(start, end)?; check(&mut canceled)?;
        let range = self.layout(desk, expected, generation)?.selection(TextSelectionRange::new(start, end))?
            .enclosing_original_range;
        if range.is_empty() { return Err(DocumentReadError::InvalidRange.into()); }
        Ok(desk.apply(expected, attempt, DeskCommand::Navigate { pane: self.pane,
            offset: range.start().get(), selection: Some(range) }, &mut canceled)?)
    }
    pub fn seek_heading(&self, desk: &mut DeskSession, expected: u64, attempt: u64,
        generation: u64, slug: &str, mut canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskDocumentError> {
        desk.validate_mutation(expected, attempt)?;
        check(&mut canceled)?;
        let document = self.layout(desk, expected, generation)?;
        let range = document.original_span(document.heading(slug)?.source_span)?;
        if range.is_empty() { return Err(DocumentReadError::InvalidRange.into()); }
        Ok(desk.apply(expected, attempt, DeskCommand::Navigate { pane: self.pane,
            offset: range.start().get(), selection: Some(range) }, &mut canceled)?)
    }
    pub fn copy(&self, desk: &mut DeskSession, expected: u64, generation: u64,
        start: usize, end: usize, mode: DocumentCopyMode, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskDocumentError> {
        rendered_limit(start, end)?; check(&mut canceled)?;
        let selection = self.layout(desk, expected, generation)?.selection(TextSelectionRange::new(start, end))?;
        if selection.enclosing_original_bytes.len() > MAX_DESK_COPY_BYTES {
            return Err(DocumentReadError::InvalidLimits.into());
        }
        let mut out = self.output(desk, "doc-copy")?;
        out.literal(",\"document_generation\":")?; out.integer(generation)?;
        out.literal(",\"rendered_utf8_range\":{\"start\":")?; out.integer(start as u64)?;
        out.literal(",\"end\":")?; out.integer(end as u64)?;
        out.literal("},\"enclosing_original_range\":")?; out.range(selection.enclosing_original_range)?;
        out.literal(",\"clipboard_written\":false")?;
        match mode {
            DocumentCopyMode::RenderedText => { out.literal(",\"copy_domain\":\"rendered-text-utf8\",\"text\":")?; out.quoted(selection.rendered_text)?; }
            DocumentCopyMode::EnclosingMarkdown => { out.literal(",\"copy_domain\":\"enclosing-original-markdown\",\"original_hex\":")?; out.hex(selection.enclosing_original_bytes)?; }
        }
        out.literal("}\n")?;
        Ok(desk.finish(out, EXIT_OK, &mut canceled)?)
    }
    fn output(&self, desk: &mut DeskSession, command: &str) -> Result<Output, DeskDocumentError> {
        let mut out = desk.output(command)?;
        out.literal(",\"pane\":")?; out.integer(self.pane.get())?;
        out.literal(",\"file_id\":")?; out.integer(self.source_file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(self.source_revision().get())?;
        Ok(out)
    }
}
fn page_limit(count: usize) -> Result<(), DeskDocumentError> {
    if !(1..=wire::MAX_READER_DOCUMENT_PAGE).contains(&count) { Err(DocumentReadError::InvalidLimits.into()) } else { Ok(()) }
}
fn rendered_limit(start: usize, end: usize) -> Result<(), DeskDocumentError> {
    if end <= start || end - start > MAX_DESK_COPY_BYTES { Err(DocumentReadError::InvalidRange.into()) } else { Ok(()) }
}

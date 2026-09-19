#![forbid(unsafe_code)]

//! A retained native-host reading session over ONE immutable source capture.
//! Only `open` performs filesystem I/O. Windows, physical-line navigation,
//! search, outlines, Markdown previews and exact copies use retained bytes.
//! No self-referential index, new matcher, decoder, runtime or native UI exists
//! here. Calls are bounded synchronous worker operations, not redraw callbacks.

mod outline;
mod document;
pub use outline::{ReaderOutlineOptions, MAX_READER_SYMBOL_PAGE, MAX_READER_SYMBOL_QUERY_BYTES};
pub use document::{DocumentCopyMode, ReaderDocumentOptions, MAX_READER_DOCUMENT_PAGE};
use outline::{AcceptedOutline, SelectionIdentity};

use std::{mem::size_of, path::{Path, PathBuf}, sync::Arc};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb::search::{CaptureRequest, CompleteCapture, DetectedEncoding, ExtentViewError,
    ObservedExtent, QueryGeneration, ReaderSearch, ResourceAllocationId, ResourceBudget,
    StreamReadError, StreamReadOptions, StreamReadState, StreamReadStep, StreamingHit, StreamingNeedle, SymbolError};
use fcb::source::{SourceError, detect_encoding};
use fcb::source::line_index::{LineNumber, LineWindowError, LineWindowScanner, LineWindowStatus};
use fcb_core::ResourceLease;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL, MANAGED_BYTES};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};
use super::{HostError, HostResponse, MAX_HOST_TEXT_BYTES};

pub const MAX_READER_WINDOW_BYTES: usize = 256 * 1024;
pub const MAX_READER_MATCHES: usize = 4096;
pub const MAX_READER_NEEDLE_BYTES: usize = 1024;
pub const MAX_READER_CONTEXT_BYTES: usize = 16 * 1024;
const LINE_SCAN_STEP: usize = 64 * 1024;
const LINE_CHECKPOINTS: usize = MAX_HOST_TEXT_BYTES / LINE_SCAN_STEP + 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReaderSessionError {
    Host(HostError), App(AppError), Source(SourceError), Stream(StreamReadError),
    View(ExtentViewError), Lines(LineWindowError), InvalidLimits, InvalidRange,
    StaleQuery, MissingHit, MissingLine, Canceled, IdentityExhausted,
    Symbol(SymbolError), MissingOutline, UnsupportedOutlineLanguage,
    Document(fcb::document::reader::DocumentReadError), MissingDocument,
}
impl std::fmt::Display for ReaderSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(e) => write!(f, "{e}"), Self::App(e) => write!(f, "{e}"),
            Self::Source(e) => write!(f, "{e}"), Self::Stream(e) => write!(f, "{e}"),
            Self::View(e) => write!(f, "{e}"), Self::Lines(e) => write!(f, "{e}"),
            Self::Symbol(e) => write!(f, "{e}"), Self::Document(e) => write!(f, "{e}"),
            Self::MissingDocument => f.write_str("READER_SESSION_NO_DOCUMENT"),
            Self::MissingOutline => f.write_str("READER_SESSION_NO_OUTLINE"),
            Self::UnsupportedOutlineLanguage => f.write_str("READER_SESSION_OUTLINE_LANGUAGE_UNSUPPORTED"),
            Self::InvalidLimits => f.write_str("READER_SESSION_INVALID_LIMITS"),
            Self::InvalidRange => f.write_str("READER_SESSION_INVALID_RANGE"),
            Self::StaleQuery => f.write_str("READER_SESSION_STALE_QUERY"),
            Self::MissingHit => f.write_str("READER_SESSION_MISSING_HIT"),
            Self::MissingLine => f.write_str("READER_SESSION_MISSING_LINE"),
            Self::Canceled => f.write_str("READER_SESSION_CANCELED"),
            Self::IdentityExhausted => f.write_str("READER_SESSION_IDENTITY_EXHAUSTED"),
        }
    }
}
impl std::error::Error for ReaderSessionError {}
impl From<HostError> for ReaderSessionError { fn from(e: HostError) -> Self { Self::Host(e) } }
impl From<AppError> for ReaderSessionError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<SourceError> for ReaderSessionError { fn from(e: SourceError) -> Self { Self::Source(e) } }
impl From<StreamReadError> for ReaderSessionError { fn from(e: StreamReadError) -> Self { Self::Stream(e) } }
impl From<ExtentViewError> for ReaderSessionError { fn from(e: ExtentViewError) -> Self { Self::View(e) } }
impl From<LineWindowError> for ReaderSessionError { fn from(e: LineWindowError) -> Self { Self::Lines(e) } }
impl From<SymbolError> for ReaderSessionError { fn from(e: SymbolError) -> Self { Self::Symbol(e) } }
impl From<OutputError> for ReaderSessionError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

struct AcceptedQuery {
    generation: u64,
    hits: Vec<StreamingHit>,
    _lease: ResourceLease,
}

/// The host supplies a fresh, non-reused owner for each session. No method swaps
/// this capture for a live file. Opening a changed file requires a NEW session.
/// Returned JSON buffers retain their own leases and may coexist; native copies
/// after handoff remain the native host's separately owned storage.
pub struct ReaderSession {
    capture: CompleteCapture,
    path: PathBuf,
    encoding: DetectedEncoding,
    // Fixed capacity is charged by size_of::<Self> before session allocation.
    // Checkpoints reference only this immutable capture, never a live path.
    line_checkpoints: [Option<LineWindowScanner>; LINE_CHECKPOINTS],
    source_bytes_read: u64,
    read_calls: u64,
    file_observation: bool,
    query: Option<AcceptedQuery>,
    last_query_attempt: u64,
    outline: Option<AcceptedOutline>,
    last_outline_attempt: u64,
    document: Option<fcb::document::reader::DocumentReader<'static>>,
    last_document_attempt: u64,
    next_allocation: u64,
    budget: ResourceBudget,
    _source_lease: ResourceLease,
}
impl ReaderSession {
    /// Admit a complete regular-file observation, including arbitrary bytes.
    /// The legacy read_text service uses the SAME bounded byte reader but has
    /// stricter UTF-8/NUL requirements for its text-only C return value.
    pub fn open(owner: ArenaOwnerId, path: &Path, max_source_bytes: usize,
        mut canceled: impl FnMut() -> bool) -> Result<Self, ReaderSessionError> {
        let raw = super::read_bytes(path, max_source_bytes, &mut canceled)?;
        let mut session = Self::from_bytes(owner, path, &raw.bytes, &mut canceled)?;
        session.source_bytes_read = raw.bytes.len() as u64;
        session.read_calls = raw.read_calls;
        session.file_observation = true;
        check(&mut canceled)?;
        Ok(session)
    }

    /// Explicit supplied-source route. Copies only the bounded supplied capture;
    /// does not inspect the path or acquire filesystem authority from its label.
    pub fn from_bytes(owner: ArenaOwnerId, path: &Path, bytes: &[u8],
        mut canceled: impl FnMut() -> bool) -> Result<Self, ReaderSessionError> {
        check(&mut canceled)?;
        if bytes.len() > MAX_HOST_TEXT_BYTES || path.as_os_str().is_empty()
            || path.as_os_str().len() > 16_384 { return Err(ReaderSessionError::InvalidLimits); }
        let budget = ResourceBudget::new(owner, ByteLength::new(MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
        let charge = bytes.len().checked_mul(2).and_then(|n| n.checked_add(256 * 1024 + size_of::<Self>()))
            .ok_or(AppError::Admission)?;
        let lease = budget.try_reserve_managed(owner, ResourceAllocationId::new(1).map_err(|_| AppError::Admission)?,
            ByteLength::new(charge as u64)).map_err(|_| AppError::Admission)?;
        let request = CaptureRequest::new(FileId::new(owner, 1).map_err(|_| ReaderSessionError::IdentityExhausted)?,
            SourceRevision::new(owner, 1).map_err(|_| ReaderSessionError::IdentityExhausted)?)?;
        let capture = CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes))?;
        let encoding = detect_encoding(&bytes[..bytes.len().min(3)]);
        check(&mut canceled)?;
        Ok(Self { capture, path: path.to_path_buf(), encoding,
            line_checkpoints: std::array::from_fn(|_| None), source_bytes_read: 0, read_calls: 0,
            file_observation: false, query: None, last_query_attempt: 0,
            outline: None, last_outline_attempt: 0, document: None, last_document_attempt: 0, next_allocation: 2,
            budget, _source_lease: lease })
    }
    pub fn capture(&self) -> &CompleteCapture { &self.capture }
    pub fn accepted_generation(&self) -> Option<u64> { self.query.as_ref().map(|q| q.generation) }
    pub fn hit_range(&self, generation: u64, position: usize) -> Result<ByteRange, ReaderSessionError> {
        let query = self.query.as_ref().ok_or(ReaderSessionError::MissingHit)?;
        if query.generation != generation { return Err(ReaderSessionError::StaleQuery); }
        query.hits.get(position).map(|h| h.original_range()).ok_or(ReaderSessionError::MissingHit)
    }
    pub fn info(&mut self, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?;
        let mut out = self.output("info")?;
        out.literal(",\"capture_origin\":")?;
        out.quoted(if self.file_observation { "regular-file-observation-not-atomic" } else { "host-supplied" })?;
        out.literal(",\"initial_source_bytes_read\":")?; out.integer(self.source_bytes_read)?;
        out.literal(",\"initial_read_calls\":")?; out.integer(self.read_calls)?;
        out.literal(",\"accepted_query_generation\":")?;
        if let Some(generation) = self.accepted_generation() { out.integer(generation)?; } else { out.literal("null")?; }
        out.literal(",\"accepted_outline_generation\":")?;
        if let Some(generation) = self.outline_generation() { out.integer(generation)?; } else { out.literal("null")?; }
        out.literal(",\"accepted_document_generation\":")?;
        if let Some(generation) = self.document_generation() { out.integer(generation)?; } else { out.literal("null")?; }
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    /// No file is reopened. Byte windows use the existing scalar/CRLF-aware
    /// extent decoder. They are logical text, not native shaping/bidi results.
    pub fn read_window(&mut self, offset: u64, bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        window_limit(bytes)?;
        if offset > self.length() { return Err(ReaderSessionError::InvalidRange); }
        let range = range(offset, offset.saturating_add(bytes as u64).min(self.length()))?;
        self.window("window", range, bytes, None, None, &mut canceled)
    }

    /// Physical CR/LF/CRLF source lines, not editor rows: empty source has no
    /// physical line and a final terminator adds no phantom row. This bounded
    /// worker scan retains bounded coarse checkpoints, not a per-line table,
    /// and does not imply a native layout.
    pub fn read_lines(&mut self, first: u64, count: u64, max_bytes: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        window_limit(max_bytes)?;
        if count == 0 || count > 1024 { return Err(ReaderSessionError::InvalidLimits); }
        let first_line = LineNumber::new(first)?;
        let mut scan = self.line_checkpoints.iter().flatten()
            .filter(|cursor| cursor.lines_seen() < first)
            .max_by_key(|cursor| cursor.bytes_scanned())
            .map(|cursor| cursor.retarget(first_line, count))
            .unwrap_or_else(|| LineWindowScanner::new(first_line, count, self.encoding))?;
        while !scan.is_finished() && scan.bytes_scanned() < self.length() {
            check(&mut canceled)?;
            let at = scan.bytes_scanned() as usize; // Bounded capture length.
            scan.step(ByteOffset::new(at as u64), &self.capture.bytes()[at..], LINE_SCAN_STEP)?;
            if !scan.is_finished() && scan.lines_seen() < first {
                let slot = scan.bytes_scanned() as usize / LINE_SCAN_STEP;
                self.line_checkpoints[slot] = Some(scan.clone());
            }
        }
        check(&mut canceled)?;
        if !scan.is_finished() { scan.finish()?; }
        match scan.status() {
            LineWindowStatus::Resolved { range, .. } =>
                self.window("lines", range, max_bytes, None, Some(first), &mut canceled),
            _ => Err(ReaderSessionError::MissingLine),
        }
    }

    /// A replacement query is accepted only after scanning and complete private
    /// response encoding. Failed/canceled replacement work leaves old rows usable
    /// under THEIR generation. Attempts consume strictly increasing generations.
    pub fn search(&mut self, generation: u64, needle: &str, max_matches: usize, max_scan_bytes: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        if generation == 0 || generation <= self.last_query_attempt { return Err(ReaderSessionError::StaleQuery); }
        if needle.is_empty() || needle.len() > MAX_READER_NEEDLE_BYTES || max_matches > MAX_READER_MATCHES {
            return Err(ReaderSessionError::InvalidLimits);
        }
        self.last_query_attempt = generation;
        check(&mut canceled)?;
        let [pattern_id, worker_id, result_id] = self.allocations()?;
        let charge = max_matches.checked_mul(size_of::<StreamingHit>())
            .and_then(|n| n.checked_add(size_of::<AcceptedQuery>())).ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.owner(), result_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut hits = reserve(max_matches)?;
        let pattern = StreamingNeedle::text(self.owner(), needle, &self.budget, pattern_id)?;
        let query_generation = QueryGeneration::new(self.owner(), generation).map_err(|_| ReaderSessionError::IdentityExhausted)?;
        let mut options = StreamReadOptions::new(query_generation);
        options.max_matches = max_matches; options.max_bytes = max_scan_bytes;
        let mut scan = ReaderSearch::new(self.capture.bytes(), *self.capture.request(),
            ByteLength::new(self.length()), &pattern, options, &self.budget, worker_id)?;
        while scan.state() == StreamReadState::Pending {
            scan.step(StreamReadStep::default(), query_generation, &mut canceled)?;
        }
        let (_, report) = scan.finish()?;
        check(&mut canceled)?;
        if report.state() == StreamReadState::Canceled { return Err(ReaderSessionError::Canceled); }
        if let StreamReadState::Failed(error) = report.state() { return Err(error.into()); }
        hits.extend_from_slice(report.hits());
        let mut out = self.output("find")?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"mode\":\"exact-decoded-literal\",\"needle\":")?; out.quoted(needle)?;
        out.literal(",\"state\":")?; out.quoted(report.state().code())?;
        out.literal(",\"search_complete\":")?; out.boolean(report.input_complete())?;
        out.literal(",\"scanned_bytes\":")?; out.integer(report.stats().scanned_bytes)?;
        out.literal(",\"matches_seen\":")?; out.integer(report.matches_seen())?;
        out.literal(",\"retained_hits\":")?; out.integer(hits.len() as u64)?;
        out.literal(",\"literal_original_hex\":")?;
        if hits.is_empty() { out.literal("null")?; } else { out.hex(report.witness_bytes(0)?)?; }
        out.literal(",\"unsupported_at\":")?;
        if let Some(offset) = report.unsupported_at() { out.integer(offset.get())?; } else { out.literal("null")?; }
        out.literal(",\"hits\":[")?;
        for (i, hit) in hits.iter().enumerate() {
            check(&mut canceled)?;
            if i > 0 { out.literal(",")?; }
            out.literal("{\"hit_index\":")?; out.integer(i as u64)?;
            out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?; out.literal("}")?;
        }
        out.literal("]}\n")?;
        let exit = if report.input_complete() { EXIT_OK } else { EXIT_PARTIAL };
        let response = self.finish_output(out, exit, &mut canceled)?;
        check(&mut canceled)?;
        self.query = Some(AcceptedQuery { generation, hits, _lease: lease });
        Ok(response)
    }

    /// Context and the entire hit come from this session's capture, including
    /// after a working-tree replacement. No source byte outside it is invented.
    pub fn hit_window(&mut self, generation: u64, position: usize, context: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        if context > MAX_READER_CONTEXT_BYTES { return Err(ReaderSessionError::InvalidLimits); }
        let selected = self.hit_range(generation, position)?;
        // Padding keeps the entire scalar/CRLF match inside the decoded window.
        let requested = range(selected.start().get().saturating_sub(context as u64 + 4),
            selected.end().get().saturating_add(context as u64 + 4).min(self.length()))?;
        self.window("hit", requested, MAX_READER_WINDOW_BYTES, Some((SelectionIdentity::Search(generation), selected)), None, &mut canceled)
    }
    pub fn copy_hit(&mut self, generation: u64, position: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        let selected = self.hit_range(generation, position)?;
        self.copy("copy-hit", selected, Some(SelectionIdentity::Search(generation)), &mut canceled)
    }
    /// Exact raw-byte export data, NOT a clipboard write. Non-scalar selections,
    /// NUL and malformed bytes remain representable as original hex. Oversized
    /// copies are refused, never silently shortened to fit the window allowance.
    pub fn copy_range(&mut self, start: u64, end: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        self.copy("copy-range", range(start, end)?, None, &mut canceled)
    }
    fn copy(&mut self, command: &str, selected: ByteRange, identity: Option<SelectionIdentity>,
        canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(canceled)?;
        if selected.end().get() > self.length() || selected.len().get() > MAX_READER_WINDOW_BYTES as u64 {
            return Err(ReaderSessionError::InvalidRange);
        }
        let mut out = self.output(command)?;
        out.literal(",")?;
        if let Some(identity) = identity { identity.encode(&mut out)?; } else { out.literal("\"query_generation\":null")?; }
        out.literal(",\"copy_domain\":\"original-bytes\",\"original_range\":")?; out.range(selected)?;
        out.literal(",\"original_hex\":")?;
        out.hex(&self.capture.bytes()[selected.start().get() as usize..selected.end().get() as usize])?;
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, canceled)
    }
    fn window(&mut self, command: &str, requested: ByteRange, max_bytes: usize,
        selected: Option<(SelectionIdentity, ByteRange)>, first_line: Option<u64>, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        check(canceled)?;
        let [extent_id, decode_id] = self.allocations()?;
        let end = requested.start().get().saturating_add(max_bytes as u64).min(requested.end().get());
        let visible = range(requested.start().get(), end)?;
        let captured = range(visible.start().get().saturating_sub(8), end.saturating_add(8).min(self.length()))?;
        let mut request = *self.capture.request();
        if !captured.is_empty() { request = request.with_range(captured)?; }
        let extent = ObservedExtent::from_bytes(request, ByteLength::new(self.length()),
            &self.capture.bytes()[captured.start().get() as usize..captured.end().get() as usize],
            &self.budget, extent_id).map_err(AppError::from)?;
        let view = BrowserSession::new(self.owner()).open_extent(extent)?;
        let text = view.decode(visible, self.encoding,
            QueryGeneration::new(self.owner(), 1).map_err(|_| ReaderSessionError::IdentityExhausted)?,
            &self.budget, decode_id, &mut *canceled)?;
        let limited = end < requested.end().get();
        let mut out = self.output(command)?;
        out.literal(",\"text_kind\":\"logical-captured-text-not-shaped\",\"requested_original_range\":")?; out.range(requested)?;
        out.literal(",\"visible_range\":")?; out.range(text.range())?;
        out.literal(",\"range_limited\":")?; out.boolean(limited)?;
        out.literal(",\"boundaries_adjusted\":")?; out.boolean(visible != text.range())?;
        out.literal(",\"first_physical_line\":")?;
        if let Some(line) = first_line { out.integer(line)?; } else { out.literal("null")?; }
        out.literal(",\"text\":")?; out.quoted(text.text())?;
        out.literal(",\"original_hex\":")?; out.hex(view.raw_selection(text.range())?)?;
        out.literal(",\"has_replacements\":")?; out.boolean(text.has_replacements())?;
        out.literal(",\"next_offset\":")?;
        if let Some(offset) = text.next_offset() { out.integer(offset.get())?; } else { out.literal("null")?; }
        out.literal(",\"selection\":")?;
        if let Some((identity, range)) = selected {
            let decoded = text.source_to_text(range)?;
            let resolved = text.text_selection(decoded)?;
            let original = &self.capture.bytes()[range.start().get() as usize..range.end().get() as usize];
            if resolved.original_range != range || resolved.original_bytes != original {
                return Err(ReaderSessionError::InvalidRange);
            }
            out.literal("{")?; identity.encode(&mut out)?;
            out.literal(",\"original_range\":")?; out.range(range)?;
            out.literal(",\"window_utf8_range\":{\"start\":")?; out.integer(decoded.start().get())?;
            out.literal(",\"end\":")?; out.integer(decoded.end().get())?;
            out.literal("},\"original_hex\":")?; out.hex(original)?; out.literal("}")?;
        } else { out.literal("null")?; }
        out.literal("}\n")?;
        self.finish_output(out, if limited { EXIT_PARTIAL } else { EXIT_OK }, canceled)
    }
    fn owner(&self) -> ArenaOwnerId { self.capture.request().file().owner() }
    fn length(&self) -> u64 { self.capture.bytes().len() as u64 }
    fn allocations<const N: usize>(&mut self) -> Result<[ResourceAllocationId; N], ReaderSessionError> {
        let next = self.next_allocation.checked_add(N as u64).ok_or(ReaderSessionError::IdentityExhausted)?;
        let first = self.next_allocation;
        self.next_allocation = next;
        Ok(std::array::from_fn(|i| ResourceAllocationId::new(first + i as u64).expect("nonzero checked allocation")))
    }
    fn output(&mut self, command: &str) -> Result<Output, ReaderSessionError> {
        let [id] = self.allocations()?;
        let mut out = Output::new(self.owner(), MAX_RESPONSE_BYTES, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.reader-session/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.owner().get())?;
        out.literal(",\"file_id\":")?; out.integer(self.capture.request().file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(self.capture.request().revision().get())?;
        out.literal(",\"path\":")?; out.path(&self.path)?;
        out.literal(",\"captured_bytes\":")?; out.integer(self.length())?;
        out.literal(",\"additional_source_bytes_read\":\"0\",\"native_presented\":false,\"encoding\":")?;
        out.quoted(match self.encoding { DetectedEncoding::Utf16Le => "utf16le", DetectedEncoding::Utf16Be => "utf16be",
            DetectedEncoding::Utf8 { .. } => "utf8", _ => "unsupported" })?;
        Ok(out)
    }
    fn finish_output(&mut self, out: Output, exit_code: u8, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        check(canceled)?;
        let [id] = self.allocations()?;
        let length = out.as_bytes().len();
        let lease = self.budget.try_reserve_managed(self.owner(), id,
            ByteLength::new((2 * length + size_of::<HostResponse>() + 16) as u64)).map_err(|_| AppError::Admission)?;
        let mut text = String::new();
        text.try_reserve_exact(length).map_err(|_| AppError::Admission)?;
        if text.capacity() > length { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        check(canceled)?;
        Ok(HostResponse { text, exit_code, _lease: lease })
    }
}
fn range(start: u64, end: u64) -> Result<ByteRange, ReaderSessionError> {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| ReaderSessionError::InvalidRange)
}
fn window_limit(bytes: usize) -> Result<(), ReaderSessionError> {
    if !(4..=MAX_READER_WINDOW_BYTES).contains(&bytes) { Err(ReaderSessionError::InvalidLimits) } else { Ok(()) }
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), ReaderSessionError> {
    if canceled() { Err(ReaderSessionError::Canceled) } else { Ok(()) }
}
fn reserve<T>(count: usize) -> Result<Vec<T>, ReaderSessionError> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| AppError::Admission)?;
    if values.capacity() > count { return Err(AppError::Admission.into()); }
    Ok(values)
}

#![forbid(unsafe_code)]

//! Persistent, source-backed reading workflow for native and headless hosts.
//! Only open_file reads a live source path. Explicit checkpoint operations read
//! or create a selected saved artifact. Adoption, reading, finding, copying,
//! pinning, bookmarks and history use the SAME retained source bytes.
//! No runtime, clipboard write, source write or native presentation is created.
//! All calls, including final destruction, belong on the host's worker.

use std::{mem::size_of, path::Path};
use fcb::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, QueryGeneration,
    SourceCapture, SourceRevision};
use fcb::search::{CaptureRequest, ReaderError, ReaderLimits, ReaderSearch, ReadingSeekState,
    ReadingTarget, ReadingWindowOptions, ResourceAllocationId, ResourceBudget, StreamReadError,
    StreamReadOptions, StreamReadState, StreamReadStep, StreamingHit, StreamingNeedle};
use fcb::source::RawPath;
use fcb_core::ResourceLease;
pub use fcb::ui::reading_panes::desk::{DeskChange, DeskCommand, DeskError, DeskLimits,
    DeskLocation, DeskPaneId, ReadingDesk};
use fcb::ui::reading_panes::desk::DeskView;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL, MANAGED_BYTES};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};
use super::{HostError, HostResponse, MAX_HOST_TEXT_BYTES};

pub const MAX_DESK_MATCHES: usize = 4096;
pub const MAX_DESK_NEEDLE_BYTES: usize = 1024;
pub const MAX_DESK_COPY_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeskSessionError {
    Desk(DeskError), Host(HostError), App(AppError), Reader(ReaderError),
    Stream(StreamReadError), Output(OutputError), MissingQuery, StaleQuery, MissingHit,
}
impl std::fmt::Display for DeskSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Desk(e) => write!(f, "{e}"), Self::Host(e) => write!(f, "{e}"),
            Self::App(e) => write!(f, "{e}"), Self::Reader(e) => write!(f, "{e}"),
            Self::Stream(e) => write!(f, "{e}"), Self::Output(e) => write!(f, "{e}"),
            Self::MissingQuery => f.write_str("DESK_NO_QUERY"), Self::StaleQuery => f.write_str("DESK_STALE_QUERY"),
            Self::MissingHit => f.write_str("DESK_MISSING_HIT"),
        }
    }
}
impl std::error::Error for DeskSessionError {}
impl From<DeskError> for DeskSessionError { fn from(e: DeskError) -> Self { Self::Desk(e) } }
impl From<HostError> for DeskSessionError { fn from(e: HostError) -> Self { Self::Host(e) } }
impl From<AppError> for DeskSessionError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<ReaderError> for DeskSessionError { fn from(e: ReaderError) -> Self { Self::Reader(e) } }
impl From<StreamReadError> for DeskSessionError { fn from(e: StreamReadError) -> Self { Self::Stream(e) } }
impl From<OutputError> for DeskSessionError { fn from(e: OutputError) -> Self { Self::Output(e) } }
impl DeskSessionError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Desk(DeskError::Canceled) | Self::Reader(ReaderError::Canceled) => true,
            Self::App(e) | Self::Host(HostError::App(e)) => e.is_canceled(),
            _ => false,
        }
    }
}

struct AcceptedQuery {
    generation: u64,
    source: DeskView,
    hits: Vec<StreamingHit>,
    _lease: ResourceLease,
}

/// Semantic mutation returns a typed acceptance receipt. Encoding/delivery is
/// separate: a failed response write cannot roll back an accepted operation.
/// Hosts recover current state with state(), not a retry with a reused attempt.
/// Checkpoint save/restore is explicit; no session is automatically persisted.
pub struct DeskSession {
    desk: ReadingDesk,
    next_source: u64,
    next_allocation: u64,
    query: Option<AcceptedQuery>,
    last_query_attempt: u64,
    initial_source_bytes_read: u64,
    imports: [Option<imports::ImportLink>; 64],
    budget: ResourceBudget,
    _lease: ResourceLease,
}
impl DeskSession {
    pub fn new(owner: ArenaOwnerId, limits: DeskLimits) -> Result<Self, DeskSessionError> {
        if limits.source_bytes == 0 || limits.source_bytes > MAX_HOST_TEXT_BYTES as u64 {
            return Err(DeskError::InvalidLimits.into());
        }
        let budget = ResourceBudget::new(owner, ByteLength::new(MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
        let lease = budget.try_reserve_managed(owner, allocation(1)?,
            ByteLength::new((size_of::<Self>() + 256 * 1024) as u64)).map_err(|_| AppError::Admission)?;
        let desk = ReadingDesk::new(owner, limits, &budget, allocation(2)?)?;
        Ok(Self { desk, next_source: 1, next_allocation: 3, query: None,
            last_query_attempt: 0, initial_source_bytes_read: 0, imports: [None; 64], budget, _lease: lease })
    }
    pub fn model(&self) -> &ReadingDesk { &self.desk }
    pub fn accepted_query(&self) -> Option<u64> { self.query.as_ref().map(|q| q.generation) }

    /// Opening a file explicitly makes a NEW observed capture, not an atomic
    /// snapshot and not a refresh of an older exact identity. Labels use escaped
    /// native bytes and are never used as path authority by later operations.
    pub fn open_file(&mut self, expected: u64, attempt: u64, path: &Path,
        mut canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskSessionError> {
        self.validate_mutation(expected, attempt)?;
        check(&mut canceled)?;
        if path.as_os_str().is_empty() || path.as_os_str().len() > 16_384 { return Err(DeskError::InvalidLocation.into()); }
        let id = self.next_source;
        self.next_source = id.checked_add(1).ok_or(DeskError::IdentityExhausted)?;
        let raw = super::read_bytes(path, self.desk.limits().source_bytes as usize, &mut canceled)?;
        let raw_len = raw.bytes.len() as u64;
        self.initial_source_bytes_read = self.initial_source_bytes_read.checked_add(raw_len).ok_or(DeskError::IdentityExhausted)?;
        // SourceCapture converts the Vec into shared immutable backing. Reserve
        // that overlap before conversion; read_bytes retains its own read lease.
        let transient_id = self.next_id()?;
        let _overlap = self.budget.try_reserve_managed(self.desk.owner(), transient_id,
            ByteLength::new(raw_len * 2 + 256 * 1024)).map_err(|_| AppError::Admission)?;
        let label = RawPath::from_path(path).display_escaped().to_string();
        let source = SourceCapture::from_bytes(self.desk.owner(),
            FileId::new(self.desk.owner(), id).map_err(|_| DeskError::IdentityExhausted)?,
            SourceRevision::new(self.desk.owner(), id).map_err(|_| DeskError::IdentityExhausted)?, label, raw.bytes)
            .map_err(|_| DeskError::InvalidLocation)?;
        let allocation = self.next_id()?;
        Ok(self.desk.open(expected, attempt, source, 0, None, allocation, &mut canceled)?)
    }
    /// Adopt an exact source/search/snapshot result from the public facade.
    /// No source copy, filesystem lookup or ordinal-to-live-path conversion.
    /// The caller transfers a capture in this desk's owner domain.
    pub fn adopt(&mut self, expected: u64, attempt: u64, source: SourceCapture,
        offset: u64, selection: Option<ByteRange>, canceled: impl FnMut() -> bool)
        -> Result<DeskChange, DeskSessionError> {
        self.validate_mutation(expected, attempt)?;
        if source.owner() != self.desk.owner() { return Err(DeskError::OwnerMismatch.into()); }
        let next = source.file().get().max(source.revision().get()).checked_add(1).ok_or(DeskError::IdentityExhausted)?;
        self.next_source = self.next_source.max(next);
        let allocation = self.next_id()?;
        Ok(self.desk.open(expected, attempt, source, offset, selection, allocation, canceled)?)
    }
    pub fn apply(&mut self, expected: u64, attempt: u64, command: DeskCommand,
        canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskSessionError> {
        Ok(self.desk.apply(expected, attempt, command, canceled)?)
    }
    pub fn state(&mut self, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskSessionError> {
        check(&mut canceled)?;
        let mut out = self.output("state")?;
        out.literal(",\"persistence\":\"explicit-checkpoint\",\"autosave\":false,\"initial_source_bytes_read\":")?;
        out.integer(self.initial_source_bytes_read)?;
        out.literal(",\"retained_sources\":")?; out.integer(self.desk.retained_source_count() as u64)?;
        out.literal(",\"retained_source_bytes\":")?; out.integer(self.desk.retained_source_bytes())?;
        out.literal(",\"query_retains_source\":")?; out.boolean(self.query.is_some())?;
        out.literal(",\"accepted_query_generation\":")?; optional(&mut out, self.accepted_query())?;
        out.literal(",\"last_query_attempt\":")?; out.integer(self.last_query_attempt)?;
        out.literal(",\"active_pane\":")?; optional(&mut out, self.desk.active().map(DeskPaneId::get))?;
        out.literal(",\"can_go_back\":")?; out.boolean(self.desk.can_go_back())?;
        out.literal(",\"can_go_forward\":")?; out.boolean(self.desk.can_go_forward())?;
        out.literal(",\"panes\":[")?;
        for (i, pane) in self.desk.panes().iter().enumerate() {
            if i > 0 { out.literal(",")?; }
            let id = self.desk.pane_id(pane.id)?;
            let at = self.desk.location(id, self.desk.revision())?;
            out.literal("{\"pane\":")?; out.integer(pane.id)?;
            out.literal(",\"label\":")?; out.quoted(&pane.path)?;
            out.literal(",\"pinned\":")?; out.boolean(pane.is_pinned)?;
            out.literal(",\"position\":{\"x\":")?; out.quoted(&pane.position.0.to_string())?;
            out.literal(",\"y\":")?; out.quoted(&pane.position.1.to_string())?;
            out.literal("},\"size\":{\"width\":")?; out.quoted(&pane.size.0.to_string())?;
            out.literal(",\"height\":")?; out.quoted(&pane.size.1.to_string())?; out.literal("}")?;
            location_fields(&mut out, at)?; out.literal("}")?;
        }
        out.literal("],\"history_cursor\":")?; optional(&mut out, self.desk.history_cursor().map(|i| i as u64))?;
        out.literal(",\"history\":[")?;
        for (i, at) in self.desk.history().iter().enumerate() {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"pane_hint\":")?; out.integer(at.preferred_pane.get())?;
            location_fields(&mut out, *at)?; out.literal("}")?;
        }
        out.literal("],\"bookmarks\":[")?;
        for (i, mark) in self.desk.bookmarks().iter().enumerate() {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"bookmark\":")?; out.integer(mark.id())?;
            out.literal(",\"label\":")?; out.quoted(mark.label())?;
            location_fields(&mut out, mark.location())?; out.literal("}")?;
        }
        out.literal("]}\n")?;
        self.finish(out, EXIT_OK, &mut canceled)
    }

    /// The shared SourceReader supplies exact newline/encoding maps and bounded
    /// continuation. It may scan up to the admitted capture to resolve a far
    /// line; this is synchronous WORKER work, never a cheap paint callback.
    pub fn window(&mut self, expected: u64, pane: DeskPaneId, target: Option<ReadingTarget>,
        options: ReadingWindowOptions, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskSessionError> {
        let at = self.desk.location(pane, expected)?;
        check(&mut canceled)?;
        let [view_id, reader_id, window_id] = self.ids()?;
        let view = self.desk.view(pane, expected, view_id)?;
        let generation = QueryGeneration::new(self.desk.owner(), expected.max(1)).map_err(|_| DeskError::IdentityExhausted)?;
        let reader = view.view().source_reader(ReaderLimits::default(), &self.budget, reader_id)?;
        let mut seek = reader.seek(target.unwrap_or(ReadingTarget::Byte(ByteOffset::new(at.offset))), generation)?;
        while seek.state() == ReadingSeekState::Pending { seek.step(64 * 1024, generation, &mut canceled)?; }
        let anchor = match seek.state() { ReadingSeekState::Ready(a) => a,
            ReadingSeekState::Canceled => return Err(DeskError::Canceled.into()),
            _ => return Err(ReaderError::LineOutOfBounds.into()) };
        let window = reader.window(anchor, generation, options, &self.budget, window_id, &mut canceled)?;
        let mut out = self.output("window")?;
        out.literal(",\"pane\":")?; out.integer(pane.get())?;
        location_fields(&mut out, at)?;
        out.literal(",\"text_kind\":\"logical-captured-text-not-shaped\",\"line_semantics\":\"editor-rows\",\"original_range\":")?;
        out.range(window.raw_range())?;
        out.literal(",\"text\":")?; out.quoted(window.text())?;
        out.literal(",\"has_replacements\":")?; out.boolean(window.has_replacements())?;
        out.literal(",\"reaches_eof\":")?; out.boolean(window.reaches_eof())?;
        out.literal(",\"next_offset\":")?; optional(&mut out, window.next_anchor().map(|a| a.offset().get()))?;
        out.literal(",\"selection_window_utf8_range\":")?;
        match at.selection.and_then(|r| window.source_to_text(r).ok()) {
            Some(r) => { out.literal("{\"start\":")?; out.integer(r.start().get())?;
                out.literal(",\"end\":")?; out.integer(r.end().get())?; out.literal("}")?; }
            None => out.literal("null")?,
        }
        out.literal(",\"rows\":[")?;
        for (i, row) in window.lines().iter().enumerate() {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"line\":")?; out.integer(row.number)?;
            out.literal(",\"original_range\":")?; out.range(row.raw_range)?;
            out.literal(",\"continued_before\":")?; out.boolean(row.continued_before)?;
            out.literal(",\"continued_after\":")?; out.boolean(row.continued_after)?; out.literal("}")?;
        }
        out.literal("]}\n")?;
        self.finish(out, EXIT_OK, &mut canceled)
    }
    pub fn copy_selection(&mut self, expected: u64, pane: DeskPaneId,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskSessionError> {
        let at = self.desk.location(pane, expected)?;
        check(&mut canceled)?;
        let mut out = self.output("copy")?;
        let bytes = self.desk.selected_bytes(pane, expected)?;
        if bytes.len() > MAX_DESK_COPY_BYTES { return Err(DeskError::InvalidLimits.into()); }
        out.literal(",\"pane\":")?; out.integer(pane.get())?;
        location_fields(&mut out, at)?;
        out.literal(",\"copy_domain\":\"original-bytes\",\"clipboard_written\":false,\"original_hex\":")?;
        out.hex(bytes)?; out.literal("}\n")?;
        self.finish(out, EXIT_OK, &mut canceled)
    }

    /// One bounded, exact literal query on one pane's immutable capture. A
    /// query replaces old rows only after its private response is complete.
    pub fn search(&mut self, expected: u64, pane: DeskPaneId, generation: u64, needle: &str,
        max_matches: usize, max_bytes: u64, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskSessionError> {
        self.desk.location(pane, expected)?;
        if generation == 0 || generation <= self.last_query_attempt { return Err(DeskSessionError::StaleQuery); }
        if needle.is_empty() || needle.len() > MAX_DESK_NEEDLE_BYTES || max_matches > MAX_DESK_MATCHES {
            return Err(DeskError::InvalidLimits.into());
        }
        self.last_query_attempt = generation;
        check(&mut canceled)?;
        let [view_id, pattern_id, worker_id, result_id] = self.ids()?;
        let view = self.desk.view(pane, expected, view_id)?;
        let source = view.source();
        let request = CaptureRequest::new(source.file(), source.revision()).map_err(|_| DeskError::OwnerMismatch)?;
        let pattern = StreamingNeedle::text(self.desk.owner(), needle, &self.budget, pattern_id)?;
        let query_generation = QueryGeneration::new(self.desk.owner(), generation).map_err(|_| DeskError::IdentityExhausted)?;
        let mut options = StreamReadOptions::new(query_generation);
        options.max_matches = max_matches; options.max_bytes = max_bytes;
        let mut scan = ReaderSearch::new(source.bytes(), request, ByteLength::new(source.bytes().len() as u64),
            &pattern, options, &self.budget, worker_id)?;
        while scan.state() == StreamReadState::Pending { scan.step(StreamReadStep::default(), query_generation, &mut canceled)?; }
        let (_, report) = scan.finish()?;
        if report.state() == StreamReadState::Canceled { return Err(DeskError::Canceled.into()); }
        if let StreamReadState::Failed(e) = report.state() { return Err(e.into()); }
        let lease = self.budget.try_reserve_managed(self.desk.owner(), result_id,
            ByteLength::new((size_of::<AcceptedQuery>() + max_matches * size_of::<StreamingHit>() + 128) as u64))
            .map_err(|_| AppError::Admission)?;
        let mut hits = Vec::new();
        hits.try_reserve_exact(max_matches).map_err(|_| AppError::Admission)?;
        if hits.capacity() > max_matches { return Err(AppError::Admission.into()); }
        hits.extend_from_slice(report.hits());
        let mut out = self.output("find")?;
        out.literal(",\"pane\":")?; out.integer(pane.get())?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"file_id\":")?; out.integer(source.file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(source.revision().get())?;
        out.literal(",\"state\":")?; out.quoted(report.state().code())?;
        out.literal(",\"search_complete\":")?; out.boolean(report.input_complete())?;
        out.literal(",\"scanned_bytes\":")?; out.integer(report.stats().scanned_bytes)?;
        out.literal(",\"matches_seen\":")?; out.integer(report.matches_seen())?;
        out.literal(",\"hits\":[")?;
        for (i, hit) in hits.iter().enumerate() {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"hit\":")?; out.integer(i as u64)?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?; out.literal("}")?;
        }
        out.literal("]}\n")?;
        let exit = if report.input_complete() { EXIT_OK } else { EXIT_PARTIAL };
        let response = self.finish(out, exit, &mut canceled)?;
        check(&mut canceled)?;
        drop(report);
        self.query = Some(AcceptedQuery { generation, source: view, hits, _lease: lease });
        Ok(response)
    }
    pub fn activate_hit(&mut self, expected: u64, attempt: u64, pane: DeskPaneId,
        generation: u64, hit: usize, canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskSessionError> {
        let at = self.desk.location(pane, expected)?;
        let query = self.query.as_ref().ok_or(DeskSessionError::MissingQuery)?;
        if generation != query.generation || at.file != query.source.source().file()
            || at.revision != query.source.source().revision() { return Err(DeskSessionError::StaleQuery); }
        let range = query.hits.get(hit).ok_or(DeskSessionError::MissingHit)?.original_range();
        self.apply(expected, attempt, DeskCommand::Navigate { pane, offset: range.start().get(), selection: Some(range) }, canceled)
    }
    pub fn clear_query(&mut self, generation: u64) -> Result<(), DeskSessionError> {
        if self.accepted_query() != Some(generation) { return Err(DeskSessionError::StaleQuery); }
        self.query = None; Ok(())
    }
    fn validate_mutation(&self, expected: u64, attempt: u64) -> Result<(), DeskSessionError> {
        if expected != self.desk.revision() { return Err(DeskError::StaleRevision.into()); }
        if attempt == 0 || attempt <= self.desk.last_attempt() { return Err(DeskError::StaleAttempt.into()); }
        Ok(())
    }
    fn next_id(&mut self) -> Result<ResourceAllocationId, DeskSessionError> {
        let n = self.next_allocation;
        self.next_allocation = n.checked_add(1).ok_or(DeskError::IdentityExhausted)?;
        allocation(n)
    }
    fn ids<const N: usize>(&mut self) -> Result<[ResourceAllocationId; N], DeskSessionError> {
        let first = self.next_allocation;
        self.next_allocation = first.checked_add(N as u64).ok_or(DeskError::IdentityExhausted)?;
        Ok(std::array::from_fn(|i| ResourceAllocationId::new(first + i as u64).expect("checked nonzero allocation")))
    }
    fn output(&mut self, command: &str) -> Result<Output, DeskSessionError> {
        let id = self.next_id()?;
        let mut out = Output::new(self.desk.owner(), MAX_RESPONSE_BYTES, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.reading-desk/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.desk.owner().get())?;
        out.literal(",\"model_revision\":")?; out.integer(self.desk.revision())?;
        out.literal(",\"last_attempt\":")?; out.integer(self.desk.last_attempt())?;
        out.literal(",\"additional_source_bytes_read\":\"0\",\"native_presented\":false")?;
        Ok(out)
    }
    fn finish(&mut self, out: Output, exit_code: u8, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, DeskSessionError> {
        check(canceled)?;
        let id = self.next_id()?;
        let length = out.as_bytes().len();
        let lease = self.budget.try_reserve_managed(self.desk.owner(), id,
            ByteLength::new((2 * length + size_of::<HostResponse>() + 16) as u64)).map_err(|_| AppError::Admission)?;
        let mut text = String::new(); text.try_reserve_exact(length).map_err(|_| AppError::Admission)?;
        if text.capacity() > length { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        check(canceled)?;
        Ok(HostResponse { text, exit_code, _lease: lease })
    }
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), DeskSessionError> {
    if canceled() { Err(DeskError::Canceled.into()) } else { Ok(()) }
}
fn allocation(n: u64) -> Result<ResourceAllocationId, DeskSessionError> {
    ResourceAllocationId::new(n).map_err(|_| DeskError::IdentityExhausted.into())
}
fn optional(out: &mut Output, value: Option<u64>) -> Result<(), OutputError> {
    match value { Some(n) => out.integer(n), None => out.literal("null") }
}
fn location_fields(out: &mut Output, at: DeskLocation) -> Result<(), OutputError> {
    out.literal(",\"file_id\":")?; out.integer(at.file.get())?;
    out.literal(",\"source_revision\":")?; out.integer(at.revision.get())?;
    out.literal(",\"offset\":")?; out.integer(at.offset)?;
    out.literal(",\"selection\":")?;
    match at.selection { Some(r) => out.range(r), None => out.literal("null") }
}

/// Explicit, create-only source-bearing checkpoint files and offline restore.
pub mod persistence;
/// Bounded cross-owner source transfer with exact identity receipts.
pub mod imports;

/// Explicit repository search and exact-result adoption into persistent readers.
pub mod repository;

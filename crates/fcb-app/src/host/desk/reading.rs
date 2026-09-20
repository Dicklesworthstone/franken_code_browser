#![forbid(unsafe_code)]

//! Resumable reading over an exact desk capture. Optional indexing and far-jump
//! work never moves a pane; explicit go accepts a resolved source anchor into
//! ordinary history. Last accepted windows remain available while a new jump is
//! pending or canceled. The engine's sparse checkpoints survive requests.

use super::*;
use fcb::search::{ReaderIndexProgress, ReadingAnchor};
use fcb::search::reader::{MAX_READER_STEP_BYTES, retained::{RetainedSourceReader, RetainedReadingSeek}};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeskReadingError { Desk(DeskSessionError), Reader(ReaderError), StaleGeneration,
    StaleStep, NoRequest, NotReady, NoContinuation }
impl std::fmt::Display for DeskReadingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Desk(e) => write!(f, "{e}"), Self::Reader(e) => write!(f, "{e}"),
            Self::StaleGeneration => f.write_str("DESK_READER_STALE_GENERATION"),
            Self::StaleStep => f.write_str("DESK_READER_STALE_STEP"), Self::NoRequest => f.write_str("DESK_READER_NO_REQUEST"),
            Self::NotReady => f.write_str("DESK_READER_NOT_READY"), Self::NoContinuation => f.write_str("DESK_READER_NO_CONTINUATION"),
        }
    }
}
impl std::error::Error for DeskReadingError {}
impl From<DeskSessionError> for DeskReadingError { fn from(e: DeskSessionError) -> Self { Self::Desk(e) } }
impl From<DeskError> for DeskReadingError { fn from(e: DeskError) -> Self { Self::Desk(e.into()) } }
impl From<ReaderError> for DeskReadingError { fn from(e: ReaderError) -> Self { Self::Reader(e) } }
impl From<OutputError> for DeskReadingError { fn from(e: OutputError) -> Self { Self::Desk(e.into()) } }
impl DeskReadingError {
    pub fn is_canceled(self) -> bool {
        matches!(self, Self::Reader(ReaderError::Canceled)) || matches!(self, Self::Desk(e) if e.is_canceled())
    }
}

pub struct DeskReading {
    pane: DeskPaneId,
    generation: u64,
    reader: RetainedSourceReader,
    request: Option<RetainedReadingSeek>,
    request_steps: u64,
    index_steps: u64,
    last_seek_attempt: u64,
    ready: Option<ReadingAnchor>,
    next: Option<ReadingAnchor>,
    _lease: ResourceLease,
}
impl DeskReading {
    /// Construct without scanning. Hosts supply a fresh reader generation;
    /// candidate construction does not replace another reader's accepted state.
    pub fn prepare(desk: &mut DeskSession, expected: u64, pane: DeskPaneId,
        generation: u64, limits: ReaderLimits, mut canceled: impl FnMut() -> bool) -> Result<Self, DeskReadingError> {
        if generation == 0 { return Err(DeskReadingError::StaleGeneration); }
        desk.model().source(pane, expected)?; check(&mut canceled)?;
        let [descriptor_id, source_id, index_id] = desk.ids()?;
        let lease = desk.budget.try_reserve_managed(desk.model().owner(), descriptor_id,
            ByteLength::new((size_of::<Self>() + 256) as u64)).map_err(|_| DeskError::ResourceDenied)?;
        let reader = RetainedSourceReader::new(desk.model().source(pane, expected)?, limits,
            &desk.budget, [source_id, index_id])?;
        check(&mut canceled)?;
        Ok(Self { pane, generation, reader, request: None, request_steps: 0, index_steps: 0,
            last_seek_attempt: 0, ready: None, next: None, _lease: lease })
    }
    pub fn pane(&self) -> DeskPaneId { self.pane }
    pub fn generation(&self) -> u64 { self.generation }
    pub fn request_generation(&self) -> Option<u64> { self.request.as_ref().map(|s| s.generation().get()) }
    pub fn ready_generation(&self) -> Option<u64> { self.ready.map(|a| a.generation().get()) }
    pub fn request_steps(&self) -> u64 { self.request_steps }
    pub fn index_steps(&self) -> u64 { self.index_steps }
    pub fn has_pending_work(&self) -> bool { self.request.as_ref().is_some_and(|s| s.state() == ReadingSeekState::Pending) }
    pub fn validate_source(&self, desk: &DeskSession, expected: u64) -> Result<(), DeskReadingError> {
        Ok(self.reader.validate_source(desk.model().source(self.pane, expected)?)?)
    }
    fn validate(&self, desk: &DeskSession, expected: u64, generation: u64) -> Result<(), DeskReadingError> {
        self.validate_source(desk, expected)?;
        if generation != self.generation { return Err(DeskReadingError::StaleGeneration); }
        Ok(())
    }
    pub fn info(&mut self, desk: &mut DeskSession, expected: u64, generation: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskReadingError> {
        self.validate(desk, expected, generation)?; check(&mut canceled)?;
        self.report(desk, "reader-info", snapshot(self.request.as_ref(), self.request_steps), self.ready, &mut canceled)
    }
    /// Index progress is a derived cache effect: on cancellation valid prefix
    /// checkpoints remain. Step counters expose actual admitted work even when
    /// response encoding/delivery fails. They must not be blindly replayed.
    pub fn index_step(&mut self, desk: &mut DeskSession, expected: u64, generation: u64,
        expected_step: u64, max_bytes: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskReadingError> {
        self.validate(desk, expected, generation)?;
        if expected_step != self.index_steps { return Err(DeskReadingError::StaleStep); }
        step_limit(max_bytes)?; check(&mut canceled)?;
        self.index_steps = self.index_steps.checked_add(1).ok_or(DeskError::IdentityExhausted)?;
        let progress = self.reader.index_step(max_bytes, &mut canceled)?;
        if progress.canceled { return Err(ReaderError::Canceled.into()); }
        self.report(desk, "reader-index", snapshot(self.request.as_ref(), self.request_steps), self.ready, &mut canceled)
    }
    /// A successfully prepared request replaces only the previous request. It
    /// never publishes a partly resolved target or moves the desk. The previous
    /// accepted anchor remains readable until a new ready anchor is published.
    pub fn begin(&mut self, desk: &mut DeskSession, expected: u64, generation: u64,
        seek_generation: u64, target: ReadingTarget, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskReadingError> {
        self.validate(desk, expected, generation)?;
        if seek_generation == 0 || seek_generation <= self.last_seek_attempt { return Err(DeskReadingError::StaleGeneration); }
        self.last_seek_attempt = seek_generation;
        check(&mut canceled)?;
        let qgen = QueryGeneration::new(desk.model().owner(), seek_generation).map_err(|_| DeskError::IdentityExhausted)?;
        let id = desk.next_id()?;
        let candidate = self.reader.seek(target, qgen, &desk.budget, id)?;
        let anchor = match candidate.state() {
            ReadingSeekState::Ready(a) => Some(a), ReadingSeekState::OutOfRange => return Err(ReaderError::LineOutOfBounds.into()),
            _ => self.ready,
        };
        let response = self.report(desk, "reader-begin", snapshot(Some(&candidate), 0), anchor, &mut canceled)?;
        check(&mut canceled)?;
        if matches!(candidate.state(), ReadingSeekState::Ready(_)) { self.next = None; }
        self.ready = anchor; self.request = Some(candidate); self.request_steps = 0;
        Ok(response)
    }
    pub fn step(&mut self, desk: &mut DeskSession, expected: u64, generation: u64,
        seek_generation: u64, expected_step: u64, max_bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskReadingError> {
        self.validate(desk, expected, generation)?;
        self.check_request(seek_generation)?;
        if expected_step != self.request_steps { return Err(DeskReadingError::StaleStep); }
        step_limit(max_bytes)?; check(&mut canceled)?;
        self.request_steps = self.request_steps.checked_add(1).ok_or(DeskError::IdentityExhausted)?;
        let request = self.request.as_mut().ok_or(DeskReadingError::NoRequest)?;
        let result = request.step(max_bytes, request.generation(), &mut canceled);
        self.reader.learn(request)?;
        let state = result?;
        let anchor = match state {
            ReadingSeekState::Ready(a) => Some(a), ReadingSeekState::OutOfRange => return Err(ReaderError::LineOutOfBounds.into()),
            ReadingSeekState::Canceled => return Err(ReaderError::Canceled.into()), ReadingSeekState::Pending => self.ready,
        };
        let response = self.report(desk, "reader-step", snapshot(self.request.as_ref(), self.request_steps), anchor, &mut canceled)?;
        check(&mut canceled)?;
        if matches!(state, ReadingSeekState::Ready(_)) { self.next = None; }
        self.ready = anchor;
        Ok(response)
    }
    pub fn cancel(&mut self, desk: &mut DeskSession, expected: u64, generation: u64,
        seek_generation: u64, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskReadingError> {
        self.validate(desk, expected, generation)?; self.check_request(seek_generation)?;
        if !self.has_pending_work() { return Err(DeskReadingError::NoRequest); }
        check(&mut canceled)?;
        let mut pending = snapshot(self.request.as_ref(), self.request_steps).ok_or(DeskReadingError::NoRequest)?;
        pending.state = ReadingSeekState::Canceled;
        let response = self.report(desk, "reader-cancel", Some(pending), self.ready, &mut canceled)?;
        check(&mut canceled)?;
        self.request.as_mut().ok_or(DeskReadingError::NoRequest)?.cancel();
        Ok(response)
    }
    /// Accept an exact resolved target through ordinary source navigation.
    /// A ready older target can be used while a newer explicit request is pending.
    pub fn go(&self, desk: &mut DeskSession, expected: u64, attempt: u64,
        generation: u64, seek_generation: u64, mut canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskReadingError> {
        self.validate(desk, expected, generation)?;
        let anchor = self.anchor(seek_generation)?; check(&mut canceled)?;
        Ok(desk.apply(expected, attempt, DeskCommand::Navigate { pane: self.pane,
            offset: anchor.offset().get(), selection: anchor.selection() }, &mut canceled)?)
    }
    /// next_offset=None starts at the accepted seek. Some(offset) consumes only
    /// the last successfully returned continuation, never a caller-forged anchor.
    /// Paging reuses reader anchors and performs no source-prefix scan.
    pub fn window(&mut self, desk: &mut DeskSession, expected: u64, generation: u64,
        seek_generation: u64, next_offset: Option<u64>, options: ReadingWindowOptions,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskReadingError> {
        self.validate(desk, expected, generation)?; check(&mut canceled)?;
        let accepted = self.anchor(seek_generation)?;
        let anchor = match next_offset {
            None => accepted,
            Some(offset) => self.next.filter(|a| a.offset().get() == offset && a.generation() == accepted.generation())
                .ok_or(DeskReadingError::NoContinuation)?,
        };
        let id = desk.next_id()?;
        let mut out = self.header(desk, "reader-window")?;
        let window = self.reader.window(anchor, anchor.generation(), options, &desk.budget, id, &mut canceled)?;
        out.literal(",\"seek_generation\":")?; out.integer(seek_generation)?;
        out.literal(",\"anchor_offset\":")?; out.integer(anchor.offset().get())?;
        out.literal(",\"line\":")?; out.integer(anchor.line_number())?;
        out.literal(",\"original_range\":")?; out.range(window.raw_range())?;
        out.literal(",\"text_kind\":\"logical-captured-text-not-shaped\",\"text\":")?; out.quoted(window.text())?;
        out.literal(",\"has_replacements\":")?; out.boolean(window.has_replacements())?;
        out.literal(",\"reaches_eof\":")?; out.boolean(window.reaches_eof())?;
        out.literal(",\"next_offset\":")?; optional(&mut out, window.next_anchor().map(|a| a.offset().get()))?;
        out.literal(",\"rows\":[")?;
        for (i, row) in window.lines().iter().enumerate() {
            check(&mut canceled)?;
            if i != 0 { out.literal(",")?; }
            out.literal("{\"line\":")?; out.integer(row.number)?;
            out.literal(",\"original_range\":")?; out.range(row.raw_range)?;
            out.literal(",\"continued_before\":")?; out.boolean(row.continued_before)?;
            out.literal(",\"continued_after\":")?; out.boolean(row.continued_after)?; out.literal("}")?;
        }
        out.literal("]}\n")?;
        let next = window.next_anchor(); drop(window);
        let response = desk.finish(out, EXIT_OK, &mut canceled)?; check(&mut canceled)?;
        self.next = next;
        Ok(response)
    }
    fn check_request(&self, generation: u64) -> Result<(), DeskReadingError> {
        if self.request_generation().ok_or(DeskReadingError::NoRequest)? != generation { return Err(DeskReadingError::StaleGeneration); }
        Ok(())
    }
    fn anchor(&self, generation: u64) -> Result<ReadingAnchor, DeskReadingError> {
        let anchor = self.ready.ok_or(DeskReadingError::NotReady)?;
        if anchor.generation().get() != generation { return Err(DeskReadingError::StaleGeneration); }
        Ok(anchor)
    }
    fn header(&self, desk: &mut DeskSession, command: &str) -> Result<Output, DeskReadingError> {
        let mut out = desk.output(command)?;
        out.literal(",\"pane\":")?; out.integer(self.pane.get())?;
        out.literal(",\"reader_generation\":")?; out.integer(self.generation)?;
        out.literal(",\"file_id\":")?; out.integer(self.reader.source().file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(self.reader.source().revision().get())?;
        out.literal(",\"line_semantics\":\"editor-rows\",\"source_reopened\":false")?;
        Ok(out)
    }
    fn report(&mut self, desk: &mut DeskSession, command: &str, request: Option<RequestInfo>,
        ready: Option<ReadingAnchor>, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, DeskReadingError> {
        let index = self.reader.progress();
        let mut out = self.header(desk, command)?;
        out.literal(",\"index_steps\":")?; out.integer(self.index_steps)?;
        out.literal(",\"last_seek_attempt\":")?; out.integer(self.last_seek_attempt)?;
        encode_index(&mut out, index, self.reader.checkpoint_capacity())?;
        out.literal(",\"ready_seek_generation\":")?; optional(&mut out, ready.map(|a| a.generation().get()))?;
        out.literal(",\"ready_offset\":")?; optional(&mut out, ready.map(|a| a.offset().get()))?;
        out.literal(",\"request\":")?;
        let pending = request.is_some_and(|r| r.state == ReadingSeekState::Pending);
        if let Some(r) = request {
            out.literal("{\"generation\":")?; out.integer(r.generation)?;
            out.literal(",\"steps\":")?; out.integer(r.steps)?;
            out.literal(",\"state\":")?; out.quoted(match r.state { ReadingSeekState::Pending => "pending",
                ReadingSeekState::Ready(_) => "ready", ReadingSeekState::OutOfRange => "out-of-range", ReadingSeekState::Canceled => "canceled" })?;
            out.literal(",\"scanned_bytes\":")?; out.integer(r.scanned)?;
            out.literal(",\"scanned_through\":")?; out.integer(r.through)?;
            out.literal(",\"last_step_bytes\":")?; out.integer(r.last_bytes as u64)?; out.literal("}")?;
        } else { out.literal("null")?; }
        out.literal("}\n")?;
        Ok(desk.finish(out, if pending { EXIT_PARTIAL } else { EXIT_OK }, canceled)?)
    }
}
#[derive(Clone, Copy)]
struct RequestInfo { generation: u64, state: ReadingSeekState, steps: u64, scanned: u64, through: u64, last_bytes: usize }
fn snapshot(request: Option<&RetainedReadingSeek>, steps: u64) -> Option<RequestInfo> {
    request.map(|s| RequestInfo { generation: s.generation().get(), state: s.state(), steps,
        scanned: s.scanned_bytes(), through: s.scanned_through().get(), last_bytes: s.last_step_bytes() })
}
fn step_limit(max: usize) -> Result<(), DeskReadingError> {
    if !(4..=MAX_READER_STEP_BYTES).contains(&max) { Err(ReaderError::InvalidLimits.into()) } else { Ok(()) }
}
fn encode_index(out: &mut Output, p: ReaderIndexProgress, capacity: usize) -> Result<(), OutputError> {
    out.literal(",\"indexed_through\":")?; out.integer(p.indexed_through.get())?;
    out.literal(",\"known_lines\":")?; out.integer(p.known_lines)?;
    out.literal(",\"total_lines\":")?; optional(out, p.total_lines)?;
    out.literal(",\"checkpoint_count\":")?; out.integer(p.checkpoint_count as u64)?;
    out.literal(",\"checkpoint_capacity\":")?; out.integer(capacity as u64)
}

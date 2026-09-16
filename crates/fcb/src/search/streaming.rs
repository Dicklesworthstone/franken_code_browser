#![forbid(unsafe_code)]

//! Whole-observation literal search without retaining the whole source.
//!
//! The host explicitly supplies one reader positioned at byte zero and its
//! observed length. Reads are one continuous observation, not concatenated
//! independently captured revisions. This does NOT promise an atomic snapshot.
//! Only successful literal witnesses are retained; every other source byte is
//! discarded. A report can resolve its hits, never arbitrary old source ranges.
//!
//! Matching is the existing overlapping KMP cursor. Text uses the existing
//! source decoder to validate each complete-scalar window, then searches the
//! uniquely encoded exact literal, with UTF-16 alignment and initial-BOM checks.
//! This equivalence is ONLY for exact UTF-8/UTF-16 text, never normalization,
//! case folding, replacement text, regex or a guessed character encoding.

use std::{io::{self, Read}, mem::size_of};
use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, QueryGeneration,
    ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_source::{CaptureEncodingMap, CaptureRequest, DetectedEncoding, MappingSpan, SourceError, SpanKind, detect_encoding};
use fcb_search::stream::{ByteSearchCursor, StreamError, MAX_STREAM_NEEDLE_BYTES};

pub const STREAM_BUFFER_BYTES: usize = 16 * 1024;
pub const MAX_STREAM_RESULTS: usize = 4096;
pub const MAX_STREAM_STEP_HITS: usize = 64;
pub const MAX_STREAM_STEP_CALLS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamReadError {
    InvalidLimits, InvalidRange, OwnerMismatch, ResourceDenied, AllocationFailed,
    Io, InvalidReadCount, Pending, StaleGeneration,
    Decoder(SourceError), Matcher(StreamError),
}
impl std::fmt::Display for StreamReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decoder(error) => write!(f, "{error}"),
            Self::Matcher(error) => write!(f, "{error}"),
            _ => f.write_str(match self {
                Self::InvalidLimits => "STREAM_READ_INVALID_LIMITS",
                Self::InvalidRange => "STREAM_READ_INVALID_RANGE",
                Self::OwnerMismatch => "STREAM_READ_OWNER_MISMATCH",
                Self::ResourceDenied => "STREAM_READ_RESOURCE_DENIED",
                Self::AllocationFailed => "STREAM_READ_ALLOCATION_FAILED",
                Self::Io => "STREAM_READ_IO", Self::InvalidReadCount => "STREAM_READ_INVALID_IO_COUNT",
                Self::Pending => "STREAM_READ_PENDING", Self::StaleGeneration => "STREAM_READ_STALE_GENERATION",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for StreamReadError {}
impl From<StreamError> for StreamReadError { fn from(error: StreamError) -> Self { Self::Matcher(error) } }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamingMode { OriginalBytes, ExactText }

/// Reusable immutable literal. Pre-encoded alternatives avoid a self-referential
/// cursor and allow reports to share ONE exact literal witness across all hits.
/// The pattern's lease remains alive while any query/report borrows it.
pub struct StreamingNeedle {
    owner: ArenaOwnerId,
    mode: StreamingMode,
    utf8: Vec<u8>,
    little: Vec<u8>,
    big: Vec<u8>,
    _lease: ResourceLease,
}
impl StreamingNeedle {
    pub fn raw(owner: ArenaOwnerId, bytes: &[u8], budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<Self, StreamReadError> {
        Self::build(owner, bytes, None, budget, allocation)
    }
    pub fn text(owner: ArenaOwnerId, text: &str, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<Self, StreamReadError> {
        Self::build(owner, text.as_bytes(), Some(text), budget, allocation)
    }
    fn build(owner: ArenaOwnerId, bytes: &[u8], text: Option<&str>, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<Self, StreamReadError> {
        if bytes.is_empty() { return Err(StreamError::EmptyNeedle.into()); }
        if bytes.len() > MAX_STREAM_NEEDLE_BYTES { return Err(StreamError::NeedleTooLong.into()); }
        let utf16 = text.map_or(0, |text| text.encode_utf16().count() * 2);
        if utf16 > MAX_STREAM_NEEDLE_BYTES { return Err(StreamError::NeedleTooLong.into()); }
        let size = size_of::<Self>() + bytes.len() + 2 * utf16;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(size as u64))
            .map_err(|_| StreamReadError::ResourceDenied)?;
        let mut utf8 = reserve(bytes.len())?;
        utf8.extend_from_slice(bytes);
        let mut little = reserve(utf16)?;
        let mut big = reserve(utf16)?;
        if let Some(text) = text {
            for unit in text.encode_utf16() {
                little.extend_from_slice(&unit.to_le_bytes());
                big.extend_from_slice(&unit.to_be_bytes());
            }
        }
        Ok(Self { owner, mode: if text.is_some() { StreamingMode::ExactText } else { StreamingMode::OriginalBytes },
            utf8, little, big, _lease: lease })
    }
    pub const fn mode(&self) -> StreamingMode { self.mode }
    pub fn text_value(&self) -> Option<&str> {
        (self.mode == StreamingMode::ExactText).then(|| std::str::from_utf8(&self.utf8).ok()).flatten()
    }
    fn encoded(&self, encoding: Option<DetectedEncoding>) -> &[u8] {
        if self.mode == StreamingMode::OriginalBytes { return &self.utf8; }
        match encoding { Some(DetectedEncoding::Utf16Le) => &self.little,
            Some(DetectedEncoding::Utf16Be) => &self.big, _ => &self.utf8 }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamReadOptions {
    pub generation: QueryGeneration,
    pub encoding: Option<DetectedEncoding>,
    pub max_matches: usize,
    pub max_bytes: u64,
    pub max_read_calls: u64,
}
impl StreamReadOptions {
    pub fn new(generation: QueryGeneration) -> Self {
        Self { generation, encoding: None, max_matches: 1000,
            max_bytes: 256 * 1024 * 1024, max_read_calls: 131_072 }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamReadStep { pub max_bytes: usize, pub max_calls: usize, pub max_hits: usize }
impl Default for StreamReadStep {
    fn default() -> Self { Self { max_bytes: STREAM_BUFFER_BYTES, max_calls: MAX_STREAM_STEP_CALLS, max_hits: MAX_STREAM_STEP_HITS } }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamReadState {
    Pending, Complete, Truncated, ByteLimit, CallLimit, ShortRead, UnsupportedText,
    Canceled, Failed(StreamReadError),
}
impl StreamReadState {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending", Self::Complete => "complete-observed-input",
            Self::Truncated => "match-limit", Self::ByteLimit => "byte-limit", Self::CallLimit => "read-call-limit",
            Self::ShortRead => "short-read", Self::UnsupportedText => "unsupported-text",
            Self::Canceled => "canceled", Self::Failed(_) => "failed",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StreamReadStats {
    pub bytes_read: u64,
    pub scanned_bytes: u64,
    pub read_calls: u64,
    pub interrupted_calls: u64,
    pub last_step_read: usize,
    pub last_step_scanned: usize,
    pub last_step_calls: usize,
    pub peak_buffer_bytes: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamingHit { occurrence: u64, range: ByteRange }
impl StreamingHit {
    pub const fn occurrence_id(self) -> u64 { self.occurrence }
    pub const fn original_range(self) -> ByteRange { self.range }
}

/// Explicit synchronous-resumable reader operation. A step admits at most one
/// successful read and one matcher batch. Interrupted calls spend call budget.
/// A foreign blocking Read implementation has no invented wall-clock deadline.
pub struct ReaderSearch<'needle, R: Read> {
    reader: R,
    needle: &'needle StreamingNeedle,
    request: CaptureRequest,
    length: ByteLength,
    options: StreamReadOptions,
    encoding: Option<DetectedEncoding>,
    header: u64,
    cursor: Option<ByteSearchCursor<'needle>>,
    buffer: Vec<u8>,
    base: u64,
    filled: usize,
    ready: usize,
    consumed: usize,
    eof: bool,
    hits: Vec<StreamingHit>,
    matches_seen: u64,
    unsupported_at: Option<ByteOffset>,
    state: StreamReadState,
    stats: StreamReadStats,
    lease: ResourceLease,
}
impl<'needle, R: Read> ReaderSearch<'needle, R> {
    pub fn new(reader: R, request: CaptureRequest, length: ByteLength,
        needle: &'needle StreamingNeedle, options: StreamReadOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, StreamReadError> {
        if request.file().owner() != needle.owner || options.generation.owner() != needle.owner {
            return Err(StreamReadError::OwnerMismatch);
        }
        if request.range().is_some() { return Err(StreamReadError::InvalidRange); }
        if options.max_matches > MAX_STREAM_RESULTS
            || options.encoding == Some(DetectedEncoding::Unsupported)
            || (needle.mode == StreamingMode::OriginalBytes && options.encoding.is_some()) {
            return Err(StreamReadError::InvalidLimits);
        }
        // Decoder vectors/strings can grow geometrically. Charge conservative
        // transient reallocation overlap, the KMP table, input, batches and hits.
        // The source length is deliberately absent from this allocation formula.
        let charge = size_of::<Self>() + size_of::<StreamReadReport<'needle>>()
            + STREAM_BUFFER_BYTES * (8 * size_of::<MappingSpan>() + 64)
            + MAX_STREAM_NEEDLE_BYTES * size_of::<usize>()
            + options.max_matches * size_of::<StreamingHit>()
            + MAX_STREAM_STEP_HITS * size_of::<fcb_search::stream::ByteMatch>();
        let lease = budget.try_reserve_managed(needle.owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| StreamReadError::ResourceDenied)?;
        let mut buffer = reserve(STREAM_BUFFER_BYTES)?;
        buffer.resize(STREAM_BUFFER_BYTES, 0);
        let cursor = if needle.mode == StreamingMode::OriginalBytes { Some(ByteSearchCursor::new(&needle.utf8)?) } else { None };
        let mut search = Self { reader, needle, request, length, options, encoding: options.encoding,
            header: 0, cursor, buffer, base: 0, filled: 0, ready: 0, consumed: 0, eof: false,
            hits: reserve(options.max_matches)?, matches_seen: 0, unsupported_at: None,
            state: StreamReadState::Pending, stats: StreamReadStats::default(), lease };
        if length.get() == 0 { search.prepare()?; search.set_terminal(); }
        Ok(search)
    }
    pub const fn state(&self) -> StreamReadState { self.state }
    pub const fn stats(&self) -> StreamReadStats { self.stats }
    pub fn hits(&self) -> &[StreamingHit] { &self.hits }
    pub const fn matches_seen(&self) -> u64 { self.matches_seen }
    pub const fn generation(&self) -> QueryGeneration { self.options.generation }
    pub fn cancel(&mut self) { self.state = StreamReadState::Canceled; }

    pub fn step(&mut self, step: StreamReadStep, active: QueryGeneration,
        mut canceled: impl FnMut() -> bool) -> Result<StreamReadState, StreamReadError> {
        self.stats.last_step_read = 0; self.stats.last_step_scanned = 0; self.stats.last_step_calls = 0;
        if active != self.options.generation { self.cancel(); return Err(StreamReadError::StaleGeneration); }
        if canceled() { self.cancel(); }
        if self.state != StreamReadState::Pending { return Ok(self.state); }
        if step.max_bytes == 0 || step.max_hits == 0 { return Ok(self.state); }
        let result = self.advance(step, &mut canceled);
        if let Err(error) = result { self.state = StreamReadState::Failed(error); return Err(error); }
        if canceled() { self.cancel(); }
        Ok(self.state)
    }
    fn advance(&mut self, step: StreamReadStep, canceled: &mut impl FnMut() -> bool) -> Result<(), StreamReadError> {
        if self.ready == 0 && !self.eof && self.stats.bytes_read < self.length.get()
            && self.stats.bytes_read < self.options.max_bytes && self.stats.read_calls < self.options.max_read_calls {
            let count = (STREAM_BUFFER_BYTES - self.filled).min(step.max_bytes)
                .min((self.length.get() - self.stats.bytes_read).min(STREAM_BUFFER_BYTES as u64) as usize)
                .min((self.options.max_bytes - self.stats.bytes_read).min(STREAM_BUFFER_BYTES as u64) as usize);
            let calls = step.max_calls.min(MAX_STREAM_STEP_CALLS);
            while self.stats.last_step_calls < calls && self.stats.read_calls < self.options.max_read_calls {
                if canceled() { self.cancel(); return Ok(()); }
                self.stats.last_step_calls += 1; self.stats.read_calls += 1;
                match self.reader.read(&mut self.buffer[self.filled..self.filled + count]) {
                    Ok(0) => { self.eof = true; break; }
                    Ok(read) if read <= count => {
                        self.filled += read; self.stats.bytes_read += read as u64;
                        self.stats.last_step_read = read;
                        self.stats.peak_buffer_bytes = self.stats.peak_buffer_bytes.max(self.filled);
                        break;
                    }
                    Ok(_) => return Err(StreamReadError::InvalidReadCount),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => { self.stats.interrupted_calls += 1; }
                    Err(_) => return Err(StreamReadError::Io),
                }
            }
        }
        if canceled() { self.cancel(); return Ok(()); }
        if self.ready == 0 { self.prepare()?; }
        if self.state != StreamReadState::Pending { return Ok(()); }
        if self.ready > self.consumed {
            let cursor = self.cursor.as_mut().ok_or(StreamReadError::InvalidRange)?;
            let batch = cursor.step(&self.buffer[self.consumed..self.ready], step.max_bytes.min(STREAM_BUFFER_BYTES),
                step.max_hits.min(MAX_STREAM_STEP_HITS))?;
            self.consumed += batch.consumed;
            self.stats.scanned_bytes = cursor.scanned_bytes();
            self.stats.last_step_scanned = batch.consumed;
            for hit in batch.hits {
                if self.needle.mode == StreamingMode::ExactText && (hit.start < self.header
                    || (self.encoding.is_some_and(|encoding| encoding.is_utf16()) && hit.start % 2 != 0)) { continue; }
                self.matches_seen += 1;
                if self.hits.len() == self.options.max_matches { self.state = StreamReadState::Truncated; break; }
                let range = ByteRange::new(ByteOffset::new(hit.start), ByteOffset::new(hit.end))
                    .map_err(|_| StreamReadError::InvalidRange)?;
                self.hits.push(StreamingHit { occurrence: self.matches_seen, range });
            }
            if self.consumed == self.ready {
                self.buffer.copy_within(self.ready..self.filled, 0);
                self.filled -= self.ready; self.base += self.ready as u64;
                self.ready = 0; self.consumed = 0;
            }
        }
        self.set_terminal();
        Ok(())
    }
    fn prepare(&mut self) -> Result<(), StreamReadError> {
        let final_input = self.eof || self.stats.bytes_read == self.length.get();
        if self.cursor.is_none() {
            // A possibly split BOM is not guessed from a one-byte read/budget.
            if self.filled < 3 && !final_input { return Ok(()); }
            let encoding = self.encoding.unwrap_or_else(|| detect_encoding(&self.buffer[..self.filled]));
            self.encoding = Some(encoding);
            self.header = match encoding {
                DetectedEncoding::Utf8 { has_bom: true } if self.buffer[..self.filled].starts_with(&[0xef, 0xbb, 0xbf]) => 3,
                DetectedEncoding::Utf16Le if self.buffer[..self.filled].starts_with(&[0xff, 0xfe]) => 2,
                DetectedEncoding::Utf16Be if self.buffer[..self.filled].starts_with(&[0xfe, 0xff]) => 2,
                _ => 0,
            };
            self.cursor = Some(ByteSearchCursor::new(self.needle.encoded(self.encoding))?);
        }
        self.ready = if self.needle.mode == StreamingMode::OriginalBytes || final_input { self.filled }
            else { complete_prefix(&self.buffer[..self.filled], self.encoding.ok_or(StreamReadError::InvalidRange)?) };
        if self.needle.mode == StreamingMode::ExactText && self.ready > 0 {
            // Only validity is consumed here; stripped in-content FEFF remains
            // in the original input searched below. No second decoder is built.
            let map = CaptureEncodingMap::build_with_base_offset(&self.buffer[..self.ready],
                self.encoding.ok_or(StreamReadError::InvalidRange)?, self.base, 0, 0, 0)
                .map_err(StreamReadError::Decoder)?;
            if let Some(bad) = map.spans().iter().find(|span| matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
                self.unsupported_at = Some(ByteOffset::new(bad.raw_start));
                self.hits.clear(); self.matches_seen = 0;
                self.state = StreamReadState::UnsupportedText;
            }
        }
        Ok(())
    }
    fn set_terminal(&mut self) {
        if self.state != StreamReadState::Pending || self.ready > 0 { return; }
        self.state = if self.filled == 0 && self.stats.bytes_read == self.length.get() { StreamReadState::Complete }
            else if self.eof && self.filled == 0 { StreamReadState::ShortRead }
            else if self.stats.bytes_read >= self.options.max_bytes { StreamReadState::ByteLimit }
            else if self.stats.read_calls >= self.options.max_read_calls { StreamReadState::CallLimit }
            else { StreamReadState::Pending };
    }

    /// Returns the reader to its host and a self-contained result table borrowing
    /// only the immutable compiled literal. It never retains the reader/buffers.
    /// The conservative lease remains attached to the report until it is dropped.
    pub fn finish(self) -> Result<(R, StreamReadReport<'needle>), StreamReadError> {
        if self.state == StreamReadState::Pending { return Err(StreamReadError::Pending); }
        let report = StreamReadReport { needle: self.needle, request: self.request, length: self.length,
            generation: self.options.generation, encoding: self.encoding, header: self.header,
            hits: self.hits, matches_seen: self.matches_seen, unsupported_at: self.unsupported_at,
            state: self.state, stats: self.stats, _lease: self.lease };
        Ok((self.reader, report))
    }
}

pub struct StreamReadReport<'needle> {
    needle: &'needle StreamingNeedle,
    request: CaptureRequest,
    length: ByteLength,
    generation: QueryGeneration,
    encoding: Option<DetectedEncoding>,
    header: u64,
    hits: Vec<StreamingHit>,
    matches_seen: u64,
    unsupported_at: Option<ByteOffset>,
    state: StreamReadState,
    stats: StreamReadStats,
    _lease: ResourceLease,
}
impl StreamReadReport<'_> {
    pub const fn request(&self) -> CaptureRequest { self.request }
    pub const fn observed_length(&self) -> ByteLength { self.length }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn encoding(&self) -> Option<DetectedEncoding> { self.encoding }
    pub const fn header_bytes(&self) -> u64 { self.header }
    pub fn mode(&self) -> StreamingMode { self.needle.mode }
    pub fn text_literal(&self) -> Option<&str> { self.needle.text_value() }
    pub const fn state(&self) -> StreamReadState { self.state }
    pub const fn stats(&self) -> StreamReadStats { self.stats }
    pub const fn matches_seen(&self) -> u64 { self.matches_seen }
    pub const fn unsupported_at(&self) -> Option<ByteOffset> { self.unsupported_at }
    pub fn hits(&self) -> &[StreamingHit] { &self.hits }
    /// Complete over the supplied sequence/length, NOT native snapshot stability.
    pub fn input_complete(&self) -> bool { self.state == StreamReadState::Complete }
    /// KMP equality plus validated exact encoding establishes these exact source
    /// bytes. Identical witnesses share the compiled literal instead of retaining
    /// one copy per occurrence. No surrounding bytes are claimed or recreated.
    pub fn witness_bytes(&self, ordinal: usize) -> Result<&[u8], StreamReadError> {
        let hit = self.hits.get(ordinal).ok_or(StreamReadError::InvalidRange)?;
        let literal = self.needle.encoded(self.encoding);
        if hit.range.len().get() != literal.len() as u64 { return Err(StreamReadError::InvalidRange); }
        Ok(literal)
    }
    pub fn validate_delivery(&self, file: FileId, revision: SourceRevision, generation: QueryGeneration) -> Result<(), StreamReadError> {
        if file != self.request.file() || revision != self.request.revision() { return Err(StreamReadError::InvalidRange); }
        if generation != self.generation { return Err(StreamReadError::StaleGeneration); }
        Ok(())
    }
}

fn reserve<T>(capacity: usize) -> Result<Vec<T>, StreamReadError> {
    let mut items = Vec::new();
    items.try_reserve_exact(capacity).map_err(|_| StreamReadError::AllocationFailed)?;
    if items.capacity() > capacity { return Err(StreamReadError::ResourceDenied); }
    Ok(items)
}
// Framing only: leave incomplete scalars for the NEXT observation chunk. The
// existing source decoder remains the authority for malformed input detection.
fn complete_prefix(bytes: &[u8], encoding: DetectedEncoding) -> usize {
    if encoding.is_utf16() {
        let mut end = bytes.len() - bytes.len() % 2;
        if end >= 2 {
            let pair = [bytes[end - 2], bytes[end - 1]];
            let unit = if encoding == DetectedEncoding::Utf16Le { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) };
            if (0xd800..=0xdbff).contains(&unit) { end -= 2; }
        }
        end
    } else {
        match std::str::from_utf8(bytes) { Err(error) if error.error_len().is_none() => error.valid_up_to(), _ => bytes.len() }
    }
}

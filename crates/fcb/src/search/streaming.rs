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
//! Unicode-insensitive text reuses the shared lowercase scalar cursor and keeps
//! each occurrence's original spelling and encoded bytes. It does not add full
//! case folding, canonical equivalence, replacement text, or guessed encodings.
//! Identifier occurrences share the captured-reference whole-token policy and
//! wait for a delimiter or actual input end, including across read boundaries.

use std::{io::{self, Read}, mem::size_of};
use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, DecodedUtf8Offset, FileId, QueryGeneration,
    ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_source::{CaptureEncodingMap, CaptureRequest, DetectedEncoding, MappingSpan, SourceError, SpanKind, detect_encoding};
use fcb_search::stream::{ByteSearchCursor, StreamError, MAX_STREAM_NEEDLE_BYTES};
use fcb_search::identifier::{IdentifierCursor, validate_identifier};
use fcb_search::{SearchMode, UnicodeNormalization};
use fcb_search::normalized::{NormalizedCursor, SourceScalar, Units, matched_text_capacity};

pub const STREAM_BUFFER_BYTES: usize = 16 * 1024;
pub const MAX_STREAM_RESULTS: usize = 4096;
pub const MAX_STREAM_STEP_HITS: usize = 64;
pub const MAX_STREAM_STEP_CALLS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamReadError {
    InvalidLimits, InvalidRange, InvalidIdentifier, OwnerMismatch, ResourceDenied, AllocationFailed,
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
                Self::InvalidIdentifier => "STREAM_READ_INVALID_IDENTIFIER",
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
pub enum StreamingMode { OriginalBytes, ExactText, LowercaseText, Identifier }

/// Reusable immutable literal. Pre-encoded alternatives avoid a self-referential
/// cursor and allow reports to share ONE exact literal witness across all hits.
/// The pattern's lease remains alive while any query/report borrows it.
pub struct StreamingNeedle {
    owner: ArenaOwnerId,
    mode: StreamingMode,
    utf8: Vec<u8>,
    little: Vec<u8>,
    big: Vec<u8>,
    max_witness_bytes: usize,
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
    /// Exact whole-token text occurrences, including comments and strings.
    /// Shares the captured-reference token policy; no compiler binding, lexer,
    /// case folding or Unicode identifier-completeness claim is introduced.
    pub fn identifier(owner: ArenaOwnerId, name: &str, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<Self, StreamReadError> {
        validate_identifier(name).map_err(|_| StreamReadError::InvalidIdentifier)?;
        let mut needle = Self::text(owner, name, budget, allocation)?;
        needle.mode = StreamingMode::Identifier;
        Ok(needle)
    }
    /// Match with the same Unicode lowercase semantics as retained text search.
    /// This is not full case folding or canonical normalization. Original byte
    /// ranges and spellings are retained separately for each accepted occurrence.
    pub fn text_matching(owner: ArenaOwnerId, text: &str, case_sensitive: bool,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, StreamReadError> {
        let mut needle = Self::text(owner, text, budget, allocation)?;
        if !case_sensitive {
            let bound = matched_text_capacity(text, lowercase_mode()).map_err(|_| StreamReadError::InvalidLimits)?;
            // Public host receipts admit at most twelve source bytes per UTF-8
            // query byte. Refuse rather than silently widen it if tables change.
            if bound > text.len().checked_mul(12).ok_or(StreamReadError::InvalidLimits)? {
                return Err(StreamReadError::InvalidLimits);
            }
            needle.mode = StreamingMode::LowercaseText;
            needle.max_witness_bytes = bound;
        }
        Ok(needle)
    }
    /// A source-length-independent upper bound for one retained original witness.
    pub const fn max_witness_bytes(&self) -> usize { self.max_witness_bytes }
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
            max_witness_bytes: bytes.len().max(utf16), utf8, little, big, _lease: lease })
    }
    pub const fn mode(&self) -> StreamingMode { self.mode }
    pub fn text_value(&self) -> Option<&str> {
        (self.mode != StreamingMode::OriginalBytes).then(|| std::str::from_utf8(&self.utf8).ok()).flatten()
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
    /// Maximum retained hits. `ReaderSearch::new` stops after the first omitted
    /// occurrence; `ReaderSearch::new_counting` keeps counting within its budgets.
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

struct RetainedWitness { text: String, bytes: Vec<u8> }

struct LowercaseWork {
    cursor: NormalizedCursor,
    window: Option<CaptureEncodingMap>,
    decoded_base: u64,
    position: usize,
    pending: Option<(SourceScalar, Units)>,
}

struct IdentifierWork<'needle> {
    cursor: IdentifierCursor<'needle>,
    window: Option<CaptureEncodingMap>,
    decoded_base: u64,
    position: usize,
    token_start: u64,
    token_end: u64,
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
    count_all_matches: bool,
    encoding: Option<DetectedEncoding>,
    header: u64,
    cursor: Option<ByteSearchCursor<'needle>>,
    initialized: bool,
    lowercase: Option<LowercaseWork>,
    identifier: Option<IdentifierWork<'needle>>,
    witnesses: Vec<RetainedWitness>,
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
    /// Search until completion, a work limit, or the first occurrence beyond the
    /// retained-hit limit. The latter leaves a lower-bound count and match-limit
    /// state; use `new_counting` to continue without retaining additional hits.
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
        let lowercase_charge = if needle.mode == StreamingMode::LowercaseText {
            options.max_matches.checked_mul(size_of::<RetainedWitness>() + 2 * needle.max_witness_bytes)
                .and_then(|n| n.checked_add(needle.max_witness_bytes
                    * (size_of::<char>() + size_of::<usize>() + size_of::<SourceScalar>())))
                .ok_or(StreamReadError::InvalidLimits)?
        } else { 0 };
        let charge = (size_of::<Self>() + size_of::<StreamReadReport<'needle>>()
            + STREAM_BUFFER_BYTES * (8 * size_of::<MappingSpan>() + 64)
            + MAX_STREAM_NEEDLE_BYTES * size_of::<usize>()
            + options.max_matches * size_of::<StreamingHit>()
            + MAX_STREAM_STEP_HITS * size_of::<fcb_search::stream::ByteMatch>())
            .checked_add(lowercase_charge).ok_or(StreamReadError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(needle.owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| StreamReadError::ResourceDenied)?;
        let mut buffer = reserve(STREAM_BUFFER_BYTES)?;
        buffer.resize(STREAM_BUFFER_BYTES, 0);
        let cursor = if needle.mode == StreamingMode::OriginalBytes { Some(ByteSearchCursor::new(&needle.utf8)?) } else { None };
        let lowercase = if needle.mode == StreamingMode::LowercaseText {
            Some(LowercaseWork { cursor: NormalizedCursor::new(needle.text_value().ok_or(StreamReadError::InvalidLimits)?,
                lowercase_mode()).map_err(|_| StreamReadError::AllocationFailed)?
                    .ok_or(StreamReadError::InvalidLimits)?, window: None, decoded_base: 0, position: 0, pending: None })
        } else { None };
        let identifier = if needle.mode == StreamingMode::Identifier {
            Some(IdentifierWork { cursor: IdentifierCursor::new(needle.text_value()
                .ok_or(StreamReadError::InvalidIdentifier)?).map_err(|_| StreamReadError::InvalidIdentifier)?,
                window: None, decoded_base: 0, position: 0, token_start: 0, token_end: 0 })
        } else { None };
        let witnesses = reserve(if lowercase.is_some() { options.max_matches } else { 0 })?;
        let mut search = Self { reader, needle, request, length, options, count_all_matches: false, encoding: options.encoding,
            header: 0, initialized: cursor.is_some(), cursor, lowercase, identifier, witnesses,
            buffer, base: 0, filled: 0, ready: 0, consumed: 0, eof: false,
            hits: reserve(options.max_matches)?, matches_seen: 0, unsupported_at: None,
            state: StreamReadState::Pending, stats: StreamReadStats::default(), lease };
        if length.get() == 0 { search.prepare()?; search.set_terminal(); }
        Ok(search)
    }
    /// Count every overlapping occurrence while retaining at most `max_matches`
    /// witnesses, including a count-only scan when that limit is zero. Byte,
    /// call, per-step and cancellation limits still apply; a count is exact only
    /// when the report completes its declared input under the requested semantics.
    pub fn new_counting(reader: R, request: CaptureRequest, length: ByteLength,
        needle: &'needle StreamingNeedle, options: StreamReadOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, StreamReadError> {
        let mut search = Self::new(reader, request, length, needle, options, budget, allocation)?;
        search.count_all_matches = true;
        Ok(search)
    }
    pub const fn state(&self) -> StreamReadState { self.state }
    pub const fn stats(&self) -> StreamReadStats { self.stats }
    pub fn hits(&self) -> &[StreamingHit] { &self.hits }
    pub const fn matches_seen(&self) -> u64 { self.matches_seen }
    pub const fn count_all_matches(&self) -> bool { self.count_all_matches }
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
        if self.identifier.is_some() && self.ready > 0 {
            self.advance_identifier(step)?;
        } else if self.lowercase.is_some() && self.ready > 0 {
            self.advance_lowercase(step)?;
        } else if self.ready > self.consumed {
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
                if self.hits.len() == self.options.max_matches {
                    if self.count_all_matches { continue; }
                    self.state = StreamReadState::Truncated; break;
                }
                let range = ByteRange::new(ByteOffset::new(hit.start), ByteOffset::new(hit.end))
                    .map_err(|_| StreamReadError::InvalidRange)?;
                self.hits.push(StreamingHit { occurrence: self.matches_seen, range });
            }
        }
        let lower_done = self.lowercase.as_ref().is_none_or(|work| work.pending.is_none()
            && work.window.as_ref().is_none_or(|map| work.position == map.decoded_text().len()));
        let identifier_done = self.identifier.as_ref().is_none_or(|work|
            work.window.as_ref().is_none_or(|map| work.position == map.decoded_text().len()));
        if self.ready > 0 && self.consumed == self.ready && lower_done && identifier_done {
            if let Some(work) = &mut self.lowercase {
                if let Some(map) = work.window.take() {
                    work.decoded_base = work.decoded_base.checked_add(map.total_decoded_bytes())
                        .ok_or(StreamReadError::InvalidRange)?;
                }
            }
            if let Some(work) = &mut self.identifier {
                if let Some(map) = work.window.take() {
                    work.decoded_base = work.decoded_base.checked_add(map.total_decoded_bytes())
                        .ok_or(StreamReadError::InvalidRange)?;
                }
            }
            self.buffer.copy_within(self.ready..self.filled, 0);
            self.filled -= self.ready; self.base += self.ready as u64;
            self.ready = 0; self.consumed = 0;
        }
        // Only an actual input end closes a pending whole token. A byte/call
        // allowance, read quantum, short buffer or UI pause is not a boundary.
        // A real early EOF may retain a final witness, but remains ShortRead.
        if self.state == StreamReadState::Pending && self.ready == 0 && self.filled == 0
            && (self.eof || self.stats.bytes_read == self.length.get()) {
            let final_hit = self.identifier.as_mut().and_then(|work|
                work.cursor.finish().then_some((work.token_start, work.token_end)));
            if let Some((start, end)) = final_hit { self.retain_identifier(start, end)?; }
        }
        self.set_terminal();
        Ok(())
    }

    fn advance_identifier(&mut self, step: StreamReadStep) -> Result<(), StreamReadError> {
        let before = self.consumed;
        let end = self.ready.min(self.consumed + step.max_bytes.min(STREAM_BUFFER_BYTES));
        let max_hits = step.max_hits.min(MAX_STREAM_STEP_HITS);
        let mut emitted = 0;
        while emitted < max_hits {
            let work = self.identifier.as_mut().ok_or(StreamReadError::InvalidRange)?;
            let map = work.window.as_ref().ok_or(StreamReadError::InvalidRange)?;
            let Some(ch) = map.decoded_text()[work.position..].chars().next() else {
                self.consumed = end;
                break;
            };
            let decoded_start = work.decoded_base + work.position as u64;
            let decoded_end = decoded_start + ch.len_utf8() as u64;
            let raw_start = map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(decoded_start))
                .map_err(StreamReadError::Decoder)?.get();
            let raw_end = map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(decoded_end))
                .map_err(StreamReadError::Decoder)?.get();
            let local_end = usize::try_from(raw_end.checked_sub(self.base).ok_or(StreamReadError::InvalidRange)?)
                .map_err(|_| StreamReadError::InvalidRange)?;
            if local_end > end {
                self.consumed = end;
                break;
            }
            self.consumed = local_end;
            self.stats.last_step_scanned = self.consumed - before;
            self.stats.scanned_bytes = self.base + self.consumed as u64;
            work.position += ch.len_utf8();
            if !work.cursor.in_token() { work.token_start = raw_start; }
            let matched = work.cursor.push(ch);
            if work.cursor.in_token() { work.token_end = raw_end; }
            let range = (work.token_start, work.token_end);
            if matched {
                emitted += 1;
                self.retain_identifier(range.0, range.1)?;
                if self.state != StreamReadState::Pending { break; }
            }
        }
        self.stats.last_step_scanned = self.consumed - before;
        self.stats.scanned_bytes = self.base + self.consumed as u64;
        Ok(())
    }

    fn retain_identifier(&mut self, start: u64, end: u64) -> Result<(), StreamReadError> {
        let range = ByteRange::new(ByteOffset::new(start), ByteOffset::new(end))
            .map_err(|_| StreamReadError::InvalidRange)?;
        // Equality was established scalar-by-scalar under the qualified decoder.
        // Exact encoding means every hit can share the immutable query witness.
        if range.len().get() != self.needle.encoded(self.encoding).len() as u64 {
            return Err(StreamReadError::InvalidRange);
        }
        self.matches_seen = self.matches_seen.checked_add(1).ok_or(StreamReadError::InvalidLimits)?;
        if self.hits.len() < self.options.max_matches {
            self.hits.push(StreamingHit { occurrence: self.matches_seen, range });
        } else if !self.count_all_matches {
            self.state = StreamReadState::Truncated;
        }
        Ok(())
    }

    fn advance_lowercase(&mut self, step: StreamReadStep) -> Result<(), StreamReadError> {
        let before = self.consumed;
        let end = self.ready.min(self.consumed + step.max_bytes.min(STREAM_BUFFER_BYTES));
        let max_hits = step.max_hits.min(MAX_STREAM_STEP_HITS);
        let mut emitted = 0;
        while emitted < max_hits {
            let work = self.lowercase.as_mut().ok_or(StreamReadError::InvalidRange)?;
            if let Some((scalar, units)) = &mut work.pending {
                if let Some(unit) = units.next() {
                    if let Some(hit) = work.cursor.push(unit, *scalar) {
                        emitted += 1;
                        let occurrence = self.matches_seen.checked_add(1).ok_or(StreamReadError::InvalidLimits)?;
                        if self.hits.len() == self.options.max_matches {
                            self.matches_seen = occurrence;
                            if self.count_all_matches { continue; }
                            self.state = StreamReadState::Truncated;
                            break;
                        }
                        let text = work.cursor.matched_text().map_err(|_| StreamReadError::AllocationFailed)?;
                        if text.capacity() > self.needle.max_witness_bytes { return Err(StreamReadError::ResourceDenied); }
                        let bytes = encode_witness(&text, self.encoding.ok_or(StreamReadError::InvalidRange)?,
                            self.needle.max_witness_bytes)?;
                        let range = ByteRange::new(ByteOffset::new(hit.raw_start), ByteOffset::new(hit.raw_end))
                            .map_err(|_| StreamReadError::InvalidRange)?;
                        if range.len().get() != bytes.len() as u64 { return Err(StreamReadError::InvalidRange); }
                        // Allocation failure must leave a valid retained prefix:
                        // omitted rows below the cap are not cap truncation.
                        self.witnesses.push(RetainedWitness { text, bytes });
                        self.hits.push(StreamingHit { occurrence, range });
                        self.matches_seen = occurrence;
                    }
                    continue;
                }
                work.pending = None;
            }
            let map = work.window.as_ref().ok_or(StreamReadError::InvalidRange)?;
            let Some(ch) = map.decoded_text()[work.position..].chars().next() else {
                self.consumed = end;
                break;
            };
            let decoded_start = work.decoded_base + work.position as u64;
            let decoded_end = decoded_start + ch.len_utf8() as u64;
            let raw_start = map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(decoded_start))
                .map_err(StreamReadError::Decoder)?.get();
            let raw_end = map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(decoded_end))
                .map_err(StreamReadError::Decoder)?.get();
            let local_end = usize::try_from(raw_end.checked_sub(self.base).ok_or(StreamReadError::InvalidRange)?)
                .map_err(|_| StreamReadError::InvalidRange)?;
            if local_end > end {
                // Tiny quanta may stop within a validated scalar. Charge these
                // original bytes now; feed the scalar only after its end arrives.
                self.consumed = end;
                break;
            }
            self.consumed = local_end;
            // Preserve spent matching work even if the next witness allocation
            // fails before this step can publish its ordinary trailing counters.
            self.stats.last_step_scanned = self.consumed - before;
            self.stats.scanned_bytes = self.base + self.consumed as u64;
            work.position += ch.len_utf8();
            let scalar = SourceScalar { ch, raw_start, raw_end, decoded_start, decoded_end };
            work.pending = Some((scalar, work.cursor.units(ch)));
        }
        self.stats.last_step_scanned = self.consumed - before;
        self.stats.scanned_bytes = self.base + self.consumed as u64;
        Ok(())
    }
    fn prepare(&mut self) -> Result<(), StreamReadError> {
        let final_input = self.eof || self.stats.bytes_read == self.length.get();
        if !self.initialized {
            // Only a possible partial header delays the encoding decision.
            // Ordinary one-byte ASCII input may already produce a useful hit
            // under a tiny budget; a byte budget is never treated as EOF.
            let prefix = &self.buffer[..self.filled];
            if !final_input && (prefix.is_empty()
                || matches!(prefix, [0xef] | [0xef, 0xbb] | [0xff] | [0xfe])) { return Ok(()); }
            let encoding = self.encoding.unwrap_or_else(|| detect_encoding(prefix));
            self.encoding = Some(encoding);
            self.header = match encoding {
                DetectedEncoding::Utf8 { has_bom: true } if prefix.starts_with(&[0xef, 0xbb, 0xbf]) => 3,
                DetectedEncoding::Utf16Le if prefix.starts_with(&[0xff, 0xfe]) => 2,
                DetectedEncoding::Utf16Be if prefix.starts_with(&[0xfe, 0xff]) => 2,
                _ => 0,
            };
            let needle: &'needle StreamingNeedle = self.needle;
            if self.lowercase.is_none() && self.identifier.is_none() {
                self.cursor = Some(ByteSearchCursor::new(needle.encoded(self.encoding))?);
            }
            self.initialized = true;
        }
        self.ready = if self.needle.mode == StreamingMode::OriginalBytes || final_input { self.filled }
            else { complete_prefix(&self.buffer[..self.filled], self.encoding.ok_or(StreamReadError::InvalidRange)?) };
        if self.needle.mode != StreamingMode::OriginalBytes && self.ready > 0 {
            // Only validity is consumed here; stripped in-content FEFF remains
            // in the original input searched below. No second decoder is built.
            let map = CaptureEncodingMap::build_with_base_offset(&self.buffer[..self.ready],
                self.encoding.ok_or(StreamReadError::InvalidRange)?, self.base,
                self.lowercase.as_ref().map(|work| work.decoded_base)
                    .or_else(|| self.identifier.as_ref().map(|work| work.decoded_base)).unwrap_or(0), 0, 0)
                .map_err(StreamReadError::Decoder)?;
            if let Some(bad) = map.spans().iter().find(|span| matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
                self.unsupported_at = Some(ByteOffset::new(bad.raw_start));
                self.hits.clear(); self.witnesses.clear(); self.matches_seen = 0;
                self.state = StreamReadState::UnsupportedText;
            } else if let Some(work) = &mut self.lowercase {
                work.position = 0;
                work.window = Some(map);
            } else if let Some(work) = &mut self.identifier {
                work.position = 0;
                work.window = Some(map);
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
            count_all_matches: self.count_all_matches,
            hits: self.hits, witnesses: self.witnesses, matches_seen: self.matches_seen, unsupported_at: self.unsupported_at,
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
    count_all_matches: bool,
    hits: Vec<StreamingHit>,
    witnesses: Vec<RetainedWitness>,
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
    /// Whether this scan was asked to continue counting after its hit table filled.
    /// Work limits, cancellation and decoding failures can still leave it partial.
    pub const fn count_all_matches(&self) -> bool { self.count_all_matches }
    /// Whether known occurrences were omitted from the retained result table.
    /// This is independent of input coverage and may be true for a complete scan.
    pub fn hits_truncated(&self) -> bool { self.matches_seen > self.hits.len() as u64 }
    pub const fn unsupported_at(&self) -> Option<ByteOffset> { self.unsupported_at }
    pub fn hits(&self) -> &[StreamingHit] { &self.hits }
    /// Complete over the supplied sequence/length, independent of retained-hit
    /// truncation. This does NOT promise native snapshot stability.
    pub fn input_complete(&self) -> bool { self.state == StreamReadState::Complete }
    /// KMP equality plus validated exact encoding establishes these exact source
    /// bytes. Exact matches share the compiled literal; lowercase matches retain
    /// each contributing source spelling. No surrounding bytes are claimed.
    pub fn witness_bytes(&self, ordinal: usize) -> Result<&[u8], StreamReadError> {
        let hit = self.hits.get(ordinal).ok_or(StreamReadError::InvalidRange)?;
        let bytes = if self.needle.mode == StreamingMode::LowercaseText {
            self.witnesses.get(ordinal).ok_or(StreamReadError::InvalidRange)?.bytes.as_slice()
        } else { self.needle.encoded(self.encoding) };
        if hit.range.len().get() != bytes.len() as u64 { return Err(StreamReadError::InvalidRange); }
        Ok(bytes)
    }
    /// The exact original spelling which contributed this occurrence. A partial
    /// lowercase expansion still retains its complete original source scalar.
    pub fn matched_text(&self, ordinal: usize) -> Result<&str, StreamReadError> {
        self.hits.get(ordinal).ok_or(StreamReadError::InvalidRange)?;
        if self.needle.mode == StreamingMode::LowercaseText {
            Ok(&self.witnesses.get(ordinal).ok_or(StreamReadError::InvalidRange)?.text)
        } else { self.needle.text_value().ok_or(StreamReadError::InvalidRange) }
    }
    pub fn retained_witness_bytes(&self) -> usize {
        if self.needle.mode == StreamingMode::LowercaseText {
            self.witnesses.iter().map(|witness| witness.bytes.len()).sum()
        } else if self.hits.is_empty() { 0 } else { self.needle.encoded(self.encoding).len() }
    }
    pub fn validate_delivery(&self, file: FileId, revision: SourceRevision, generation: QueryGeneration) -> Result<(), StreamReadError> {
        if file != self.request.file() || revision != self.request.revision() { return Err(StreamReadError::InvalidRange); }
        if generation != self.generation { return Err(StreamReadError::StaleGeneration); }
        Ok(())
    }
}

fn lowercase_mode() -> SearchMode {
    SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::Exact }
}
fn encode_witness(text: &str, encoding: DetectedEncoding, maximum: usize) -> Result<Vec<u8>, StreamReadError> {
    let length = if encoding.is_utf16() { text.encode_utf16().count() * 2 } else { text.len() };
    if length > maximum { return Err(StreamReadError::ResourceDenied); }
    let mut bytes = reserve(length)?;
    if encoding.is_utf16() {
        for unit in text.encode_utf16() {
            let pair = if encoding == DetectedEncoding::Utf16Le { unit.to_le_bytes() } else { unit.to_be_bytes() };
            bytes.extend_from_slice(&pair);
        }
    } else { bytes.extend_from_slice(text.as_bytes()); }
    Ok(bytes)
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

#[cfg(test)]
mod tiny_budget_tests {
    use super::*;
    #[test]
    fn one_byte_ascii_budget_emits_available_prefix_without_claiming_eof() {
        let owner = ArenaOwnerId::new(1515).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
        let needle = StreamingNeedle::text(owner, "a", &budget, ResourceAllocationId::new(1).unwrap()).unwrap();
        let request = CaptureRequest::new(FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap()).unwrap();
        let mut options = StreamReadOptions::new(QueryGeneration::new(owner, 1).unwrap());
        options.max_bytes = 1;
        let mut query = ReaderSearch::new(b"abc".as_slice(), request, ByteLength::new(3), &needle,
            options, &budget, ResourceAllocationId::new(2).unwrap()).unwrap();
        query.step(StreamReadStep::default(), options.generation, || false).unwrap();
        assert_eq!(query.state(), StreamReadState::ByteLimit);
        assert_eq!(query.hits().len(), 1);
        assert_eq!(query.stats().bytes_read, 1);
    }
    #[test]
    fn byte_and_hit_zero_steps_do_not_consume_even_a_header() {
        let owner = ArenaOwnerId::new(1516).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
        let needle = StreamingNeedle::text(owner, "a", &budget, ResourceAllocationId::new(1).unwrap()).unwrap();
        let request = CaptureRequest::new(FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap()).unwrap();
        let options = StreamReadOptions::new(QueryGeneration::new(owner, 1).unwrap());
        let mut query = ReaderSearch::new(b"abc".as_slice(), request, ByteLength::new(3), &needle,
            options, &budget, ResourceAllocationId::new(2).unwrap()).unwrap();
        for step in [StreamReadStep { max_bytes: 0, ..Default::default() }, StreamReadStep { max_hits: 0, ..Default::default() }] {
            assert_eq!(query.step(step, options.generation, || false).unwrap(), StreamReadState::Pending);
            assert_eq!(query.stats().bytes_read, 0); assert_eq!(query.stats().read_calls, 0);
        }
    }
}

#[cfg(test)]
mod lowercase_failure_tests {
    use super::*;

    #[test]
    fn refused_later_witness_keeps_scanned_bytes_and_accepted_counts_consistent() {
        let owner = ArenaOwnerId::new(1517).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
        let mut needle = StreamingNeedle::text_matching(owner, "k", false, &budget,
            ResourceAllocationId::new(1).unwrap()).unwrap();
        // Inject a retained-witness budget refusal after one valid occurrence,
        // without global allocator hooks. K fits; the original Kelvin spelling
        // is three UTF-8 bytes and exercises the checked refusal path.
        needle.max_witness_bytes = 2;
        let request = CaptureRequest::new(FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap()).unwrap();
        let mut options = StreamReadOptions::new(QueryGeneration::new(owner, 1).unwrap());
        options.max_matches = 4;
        for counting in [false, true] {
            let mut query = if counting {
                ReaderSearch::new_counting("KK".as_bytes(), request, ByteLength::new(4), &needle,
                    options, &budget, ResourceAllocationId::new(2).unwrap())
            } else {
                ReaderSearch::new("KK".as_bytes(), request, ByteLength::new(4), &needle,
                    options, &budget, ResourceAllocationId::new(2).unwrap())
            }.unwrap();
            assert_eq!(query.step(StreamReadStep::default(), options.generation, || false), Err(StreamReadError::ResourceDenied));
            assert_eq!(query.stats().scanned_bytes, 4);
            assert_eq!(query.stats().last_step_scanned, 4);
            assert_eq!(query.matches_seen(), 1);
            let (_, report) = query.finish().unwrap();
            assert_eq!(report.state(), StreamReadState::Failed(StreamReadError::ResourceDenied));
            assert!(!report.input_complete());
            assert!(!report.hits_truncated());
            assert_eq!(report.hits().len(), 1);
            assert_eq!(report.hits()[0].occurrence_id(), 1);
            assert_eq!(report.witness_bytes(0).unwrap(), b"K");
            assert_eq!(report.matched_text(0).unwrap(), "K");
        }
    }
}

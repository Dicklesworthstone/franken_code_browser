#![forbid(unsafe_code)]

//! Progressive literal search confined to one retained observed extent/window.
//! Uses fcb-search's production byte and Unicode-lowercase KMP cursors.
//! Completion is explicitly scoped; neither holes nor other observations are
//! searched. Decoded coordinates are local to the text window, never relabelled
//! as whole-file decoded offsets. Replacement text is not searchable as source.

use std::mem::size_of;
use fcb_core::{ByteLength, ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range,
    FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_search::{QueryError, SearchMode, UnicodeNormalization};
use fcb_search::normalized::{NormalizedCursor, SourceScalar, Units, matched_text_capacity};
use fcb_search::stream::{ByteMatch, ByteSearchCursor, StreamError,
    MAX_STREAM_NEEDLE_BYTES, MAX_STREAM_STEP_BYTES};
use super::extents::{ExtentText, ExtentView, ExtentViewError, ObservedExtent};

pub const MAX_EXTENT_MATCHES: usize = 4096;
const BATCH_MATCHES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExtentQueryError {
    View(ExtentViewError), Stream(StreamError), Query(QueryError), InvalidLimits, InvalidRange,
    ResourceDenied, AllocationFailed, OwnerMismatch, UnsupportedText,
    Canceled, StaleGeneration, StaleObservation,
}
impl std::fmt::Display for ExtentQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::View(error) => write!(f, "{error}"),
            Self::Stream(error) => write!(f, "{error}"),
            Self::Query(error) => write!(f, "{error}"),
            Self::InvalidLimits => f.write_str("EXTENT_QUERY_INVALID_LIMITS"),
            Self::InvalidRange => f.write_str("EXTENT_QUERY_INVALID_RANGE"),
            Self::ResourceDenied => f.write_str("EXTENT_QUERY_RESOURCE_DENIED"),
            Self::AllocationFailed => f.write_str("EXTENT_QUERY_ALLOCATION_FAILED"),
            Self::OwnerMismatch => f.write_str("EXTENT_QUERY_OWNER_MISMATCH"),
            Self::UnsupportedText => f.write_str("EXTENT_QUERY_UNSUPPORTED_TEXT"),
            Self::Canceled => f.write_str("EXTENT_QUERY_CANCELED"),
            Self::StaleGeneration => f.write_str("EXTENT_QUERY_STALE_GENERATION"),
            Self::StaleObservation => f.write_str("EXTENT_QUERY_STALE_OBSERVATION"),
        }
    }
}
impl std::error::Error for ExtentQueryError {}
impl From<ExtentViewError> for ExtentQueryError { fn from(error: ExtentViewError) -> Self { Self::View(error) } }
impl From<StreamError> for ExtentQueryError { fn from(error: StreamError) -> Self { Self::Stream(error) } }
impl From<QueryError> for ExtentQueryError { fn from(error: QueryError) -> Self { Self::Query(error) } }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtentQueryState { Pending, Finished, Truncated, Canceled, Failed(ExtentQueryError) }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtentQueryInput { OriginalBytes, WindowUtf8 }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtentQueryOptions { pub generation: QueryGeneration, pub max_matches: usize }
impl ExtentQueryOptions {
    pub fn new(generation: QueryGeneration) -> Self { Self { generation, max_matches: 1000 } }
}

/// Explicit decoded-text comparison and count policy. The original constructors
/// retain their case-sensitive, stop-after-lookahead behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtentTextOptions {
    /// False uses Unicode lowercase comparison, without canonical normalization
    /// or full case folding. Returned ranges still name original source scalars.
    pub case_sensitive: bool,
    /// Scan the entire retained scope after max_matches rows have been stored.
    /// This does not widen the scope or admit reads from another observation.
    pub count_all_matches: bool,
}
impl Default for ExtentTextOptions {
    fn default() -> Self { Self { case_sensitive: true, count_all_matches: false } }
}

/// A match borrows its ACTUAL retained observation; it cannot outlive the bytes
/// it names. No search token resolves against changed live source automatically.
#[derive(Clone, Copy, Debug)]
pub struct ExtentMatch<'source> {
    extent: &'source ObservedExtent,
    generation: QueryGeneration,
    occurrence: u64,
    original: ByteRange,
    window_text: Option<DecodedUtf8Range>,
}
impl ExtentMatch<'_> {
    pub fn file_id(self) -> FileId { self.extent.request().file() }
    pub fn revision(self) -> SourceRevision { self.extent.request().revision() }
    pub fn generation(self) -> QueryGeneration { self.generation }
    pub fn occurrence_id(self) -> u64 { self.occurrence }
    pub fn original_range(self) -> ByteRange { self.original }
    pub fn window_text_range(self) -> Option<DecodedUtf8Range> { self.window_text }
    pub fn original_bytes(&self) -> Result<&[u8], ExtentQueryError> {
        self.extent.range_bytes(self.original).map_err(|_| ExtentQueryError::InvalidRange)
    }
    pub fn validate_delivery(self, view: &ExtentView, generation: QueryGeneration) -> Result<(), ExtentQueryError> {
        if generation != self.generation { return Err(ExtentQueryError::StaleGeneration); }
        let other = view.extent();
        if self.extent.request().file() != other.request().file()
            || self.extent.request().revision() != other.request().revision()
            || self.extent.range() != other.range()
            || !std::ptr::eq(self.extent.bytes(), other.bytes()) {
            return Err(ExtentQueryError::StaleObservation);
        }
        Ok(())
    }
}

enum Input<'source> {
    Raw { extent: &'source ObservedExtent, bytes: &'source [u8], scope: ByteRange },
    Text(&'source ExtentText),
}
impl<'source> Input<'source> {
    fn extent(&self) -> &'source ObservedExtent {
        match self { Self::Raw { extent, .. } => extent, Self::Text(text) => text.extent() }
    }
    fn bytes(&self) -> &'source [u8] {
        match self { Self::Raw { bytes, .. } => bytes, Self::Text(text) => text.text().as_bytes() }
    }
    fn scope(&self) -> ByteRange {
        match self { Self::Raw { scope, .. } => *scope, Self::Text(text) => text.range() }
    }
    fn hit(&self, hit: ByteMatch, generation: QueryGeneration) -> Result<ExtentMatch<'source>, ExtentQueryError> {
        let (original, window_text) = match self {
            Self::Raw { scope, .. } => {
                let start = scope.start().get().checked_add(hit.start).ok_or(ExtentQueryError::InvalidRange)?;
                let end = scope.start().get().checked_add(hit.end).ok_or(ExtentQueryError::InvalidRange)?;
                let original = ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| ExtentQueryError::InvalidRange)?;
                (original, None)
            }
            Self::Text(text) => {
                let range = DecodedUtf8Range::new(DecodedUtf8Offset::new(hit.start), DecodedUtf8Offset::new(hit.end))
                    .map_err(|_| ExtentQueryError::InvalidRange)?;
                (text.text_selection(range)?.original_range, Some(range))
            }
        };
        Ok(ExtentMatch { extent: self.extent(), generation, occurrence: hit.occurrence_id, original, window_text })
    }
}

struct QueryBatch {
    hits: Vec<ByteMatch>,
    consumed: usize,
}

struct PendingScalar {
    source: SourceScalar,
    units: std::iter::Peekable<Units>,
}

/// The decoded window already exists. Only the shared normalizer's query-sized
/// ring is retained here; no lowercased copy of the window is constructed.
struct LowercaseCursor {
    cursor: NormalizedCursor,
    next_scalar: usize,
    scanned: usize,
    pending: Option<PendingScalar>,
    occurrences: u64,
}
impl LowercaseCursor {
    fn step(&mut self, text: &str, byte_budget: usize, hit_limit: usize) -> Result<QueryBatch, ExtentQueryError> {
        let work = byte_budget.min(MAX_STREAM_STEP_BYTES);
        let mut batch = QueryBatch { hits: Vec::new(), consumed: 0 };
        batch.hits.try_reserve_exact(hit_limit.min(work)).map_err(|_| ExtentQueryError::AllocationFailed)?;
        let mut transformed_units = 0;
        while transformed_units < work && batch.hits.len() < hit_limit {
            if let Some(pending) = &mut self.pending {
                let source = pending.source;
                let unit = pending.units.next().ok_or(ExtentQueryError::InvalidRange)?;
                let scalar_finished = pending.units.peek().is_none();
                transformed_units += 1;
                if let Some(hit) = self.cursor.push(unit, source) {
                    self.occurrences = self.occurrences.checked_add(1).ok_or(ExtentQueryError::InvalidRange)?;
                    batch.hits.push(ByteMatch { occurrence_id: self.occurrences,
                        start: hit.decoded_start, end: hit.decoded_end });
                }
                if scalar_finished { self.pending = None; }
                continue;
            }
            if self.next_scalar == text.len() || batch.consumed == work { break; }
            let start = self.next_scalar;
            let ch = text.get(start..).and_then(|tail| tail.chars().next()).ok_or(ExtentQueryError::InvalidRange)?;
            let end = start.checked_add(ch.len_utf8()).ok_or(ExtentQueryError::InvalidRange)?;
            // A one-byte quantum can stop inside a scalar. Its comparison
            // units are admitted only after all of its UTF-8 bytes are counted.
            let consumed = (end - self.scanned).min(work - batch.consumed);
            self.scanned += consumed;
            batch.consumed += consumed;
            if self.scanned != end { break; }
            self.next_scalar = end;
            // These are coordinates in the decoded window, not original-file
            // bytes. Input::hit performs the original-byte mapping once per
            // retained occurrence, including full scalar provenance on expansion.
            let source = SourceScalar { ch, raw_start: start as u64, raw_end: end as u64,
                decoded_start: start as u64, decoded_end: end as u64 };
            self.pending = Some(PendingScalar { source, units: self.cursor.units(ch).peekable() });
        }
        Ok(batch)
    }
}

enum Cursor<'needle> {
    Exact(ByteSearchCursor<'needle>),
    Lowercase(LowercaseCursor),
}
impl Cursor<'_> {
    fn scanned_bytes(&self) -> u64 {
        match self { Self::Exact(cursor) => cursor.scanned_bytes(), Self::Lowercase(cursor) => cursor.scanned as u64 }
    }
    fn matches_seen(&self) -> u64 {
        match self { Self::Exact(cursor) => cursor.matches_seen(), Self::Lowercase(cursor) => cursor.occurrences }
    }
    fn complete(&self, length: usize) -> bool {
        match self {
            Self::Exact(cursor) => cursor.scanned_bytes() == length as u64,
            Self::Lowercase(cursor) => cursor.next_scalar == length && cursor.pending.is_none(),
        }
    }
    fn cancel(&mut self) {
        match self {
            Self::Exact(cursor) => cursor.cancel(),
            Self::Lowercase(cursor) => cursor.pending = None,
        }
    }
    fn step(&mut self, input: &Input<'_>, byte_budget: usize, hit_limit: usize) -> Result<QueryBatch, ExtentQueryError> {
        match self {
            Self::Exact(cursor) => {
                let offset = usize::try_from(cursor.scanned_bytes()).map_err(|_| ExtentQueryError::InvalidRange)?;
                let remaining = input.bytes().get(offset..).ok_or(ExtentQueryError::InvalidRange)?;
                let batch = cursor.step(remaining, byte_budget, hit_limit)?;
                Ok(QueryBatch { hits: batch.hits, consumed: batch.consumed })
            }
            Self::Lowercase(cursor) => {
                let Input::Text(text) = input else { return Err(ExtentQueryError::UnsupportedText); };
                cursor.step(text.text(), byte_budget, hit_limit)
            }
        }
    }
}

pub struct ExtentQuery<'source, 'needle> {
    input: Input<'source>,
    cursor: Cursor<'needle>,
    options: ExtentQueryOptions,
    count_all_matches: bool,
    hits: Vec<ExtentMatch<'source>>,
    state: ExtentQueryState,
    last_step_bytes: usize,
    _lease: ResourceLease,
}
impl<'source, 'needle> ExtentQuery<'source, 'needle> {
    pub fn raw(view: &'source ExtentView, scope: ByteRange, needle: &'needle [u8], options: ExtentQueryOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, ExtentQueryError> {
        Self::raw_with_counting(view, scope, needle, options, false, budget, allocation)
    }
    /// Exact original-byte matching with an independent row-retention limit.
    /// Counting continues only within scope; it never fills unobserved holes.
    pub fn raw_with_counting(view: &'source ExtentView, scope: ByteRange, needle: &'needle [u8],
        options: ExtentQueryOptions, count_all_matches: bool,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, ExtentQueryError> {
        let bytes = view.raw_selection(scope)?;
        Self::new(Input::Raw { extent: view.extent(), bytes, scope }, needle, options,
            ExtentTextOptions { case_sensitive: true, count_all_matches }, budget, allocation)
    }
    /// Exact decoded-text mode. Malformed replacement windows can still be
    /// searched via the raw route; normalization is never silently approximated.
    pub fn text(text: &'source ExtentText, needle: &'needle str, options: ExtentQueryOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, ExtentQueryError> {
        Self::text_with_options(text, needle, options, ExtentTextOptions::default(), budget, allocation)
    }
    /// Decoded literal search using the same Unicode lowercase semantics as
    /// whole-file search. Every result maps back to its original source bytes.
    pub fn text_with_options(text: &'source ExtentText, needle: &'needle str, options: ExtentQueryOptions,
        matching: ExtentTextOptions, budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, ExtentQueryError> {
        if text.has_replacements() { return Err(ExtentQueryError::UnsupportedText); }
        Self::new(Input::Text(text), needle.as_bytes(), options, matching, budget, allocation)
    }
    fn new(input: Input<'source>, needle: &'needle [u8], options: ExtentQueryOptions,
        matching: ExtentTextOptions, budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, ExtentQueryError> {
        if options.generation.owner() != input.extent().request().file().owner() { return Err(ExtentQueryError::OwnerMismatch); }
        if options.max_matches > MAX_EXTENT_MATCHES || needle.len() > MAX_STREAM_NEEDLE_BYTES { return Err(ExtentQueryError::InvalidLimits); }
        if needle.is_empty() { return Err(StreamError::EmptyNeedle.into()); }
        let mode = SearchMode::DecodedText { case_sensitive: matching.case_sensitive, normalization: UnicodeNormalization::Exact };
        let normalized_needle = if matching.case_sensitive { None } else {
            Some(std::str::from_utf8(needle).map_err(|_| ExtentQueryError::UnsupportedText)?)
        };
        let cursor_charge = if let Some(text) = normalized_needle {
            // The shared cursor bounds transformed units independently of byte
            // length. Allow geometric query/ring growth and replacement overlap
            // before constructing any of those allocations.
            let units = matched_text_capacity(text, mode)? / 4;
            units.max(4).checked_mul(4)
                .and_then(|n| n.checked_mul(size_of::<char>() + size_of::<usize>() + size_of::<SourceScalar>()))
                .ok_or(ExtentQueryError::InvalidLimits)?
        } else { 2 * needle.len() * size_of::<usize>() };
        let charge = options.max_matches.checked_mul(size_of::<ExtentMatch<'source>>())
            .and_then(|n| n.checked_add(cursor_charge))
            .and_then(|n| n.checked_add(2 * BATCH_MATCHES * size_of::<ByteMatch>() + size_of::<Self>()))
            .ok_or(ExtentQueryError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(options.generation.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ExtentQueryError::ResourceDenied)?;
        let mut hits = Vec::new();
        hits.try_reserve_exact(options.max_matches).map_err(|_| ExtentQueryError::AllocationFailed)?;
        if hits.capacity() > options.max_matches { return Err(ExtentQueryError::ResourceDenied); }
        let cursor = if let Some(text) = normalized_needle {
            let cursor = NormalizedCursor::new(text, mode)?.ok_or(ExtentQueryError::UnsupportedText)?;
            Cursor::Lowercase(LowercaseCursor { cursor, next_scalar: 0, scanned: 0, pending: None, occurrences: 0 })
        } else { Cursor::Exact(ByteSearchCursor::new(needle)?) };
        let state = if input.bytes().is_empty() { ExtentQueryState::Finished } else { ExtentQueryState::Pending };
        Ok(Self { input, cursor, options, count_all_matches: matching.count_all_matches,
            hits, state, last_step_bytes: 0, _lease: lease })
    }
    pub fn scope(&self) -> ByteRange { self.input.scope() }
    pub fn input_kind(&self) -> ExtentQueryInput {
        match self.input { Input::Raw { .. } => ExtentQueryInput::OriginalBytes, Input::Text(_) => ExtentQueryInput::WindowUtf8 }
    }
    pub fn state(&self) -> ExtentQueryState { self.state }
    pub fn hits(&self) -> &[ExtentMatch<'source>] { &self.hits }
    pub fn matches_seen(&self) -> u64 { self.cursor.matches_seen() }
    /// Row truncation is independent of scope completion when counting all.
    pub fn results_truncated(&self) -> bool { self.matches_seen() > self.hits.len() as u64 }
    /// Counted in input_kind() units: raw bytes or local decoded UTF-8 bytes.
    pub fn scanned_input_bytes(&self) -> u64 { self.cursor.scanned_bytes() }
    pub fn last_step_bytes(&self) -> usize { self.last_step_bytes }
    /// Exhaustive ONLY within scope(), never within the unseen rest of a file.
    pub fn scope_complete(&self) -> bool { self.state == ExtentQueryState::Finished }
    pub fn has_unsearched_source(&self) -> bool {
        self.scope().start().get() != 0 || self.scope().end().get() != self.input.extent().observed_length().get()
    }
    pub fn cancel(&mut self) { self.cursor.cancel(); self.state = ExtentQueryState::Canceled; }
    /// Bound decoded/raw input bytes and transformed scalar units independently
    /// by the quantum (capped at MAX_STREAM_STEP_BYTES). A resumed expansion can
    /// progress with zero newly consumed bytes. Zero quantum never advances.
    pub fn step(&mut self, max_input_bytes: usize, generation: QueryGeneration,
        mut canceled: impl FnMut() -> bool) -> Result<ExtentQueryState, ExtentQueryError> {
        self.last_step_bytes = 0;
        if generation != self.options.generation { self.cancel(); return Err(ExtentQueryError::StaleGeneration); }
        if canceled() { self.cancel(); return Err(ExtentQueryError::Canceled); }
        if let ExtentQueryState::Failed(error) = self.state { return Err(error); }
        if self.state != ExtentQueryState::Pending || max_input_bytes == 0 { return Ok(self.state); }
        let hit_limit = if self.count_all_matches { BATCH_MATCHES }
            else { (self.options.max_matches - self.hits.len()).saturating_add(1).min(BATCH_MATCHES) };
        let batch = match self.cursor.step(&self.input, max_input_bytes, hit_limit) {
            Ok(batch) => batch,
            Err(error) => { self.state = ExtentQueryState::Failed(error); return Err(error); }
        };
        self.last_step_bytes = batch.consumed;
        for hit in batch.hits {
            if self.hits.len() == self.options.max_matches {
                if self.count_all_matches { continue; }
                self.state = ExtentQueryState::Truncated;
                break;
            }
            match self.input.hit(hit, generation) {
                Ok(hit) => self.hits.push(hit),
                Err(error) => { self.state = ExtentQueryState::Failed(error); return Err(error); }
            }
        }
        if canceled() { self.cancel(); return Err(ExtentQueryError::Canceled); }
        if self.state == ExtentQueryState::Pending && self.cursor.complete(self.input.bytes().len()) {
            self.state = ExtentQueryState::Finished;
        }
        Ok(self.state)
    }
}

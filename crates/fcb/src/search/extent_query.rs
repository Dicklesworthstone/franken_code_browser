#![forbid(unsafe_code)]

//! Progressive exact search confined to one retained observed extent/window.
//! Uses fcb-search's production overlapping KMP cursor, not a second matcher.
//! Completion is explicitly scoped; neither holes nor other observations are
//! searched. Decoded coordinates are local to the text window, never relabelled
//! as whole-file decoded offsets. Replacement text is not searchable as source.

use std::mem::size_of;
use fcb_core::{ByteLength, ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range,
    FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_search::stream::{ByteMatch, ByteSearchCursor, StreamError, MAX_STREAM_NEEDLE_BYTES};
use super::extents::{ExtentText, ExtentView, ExtentViewError, ObservedExtent};

pub const MAX_EXTENT_MATCHES: usize = 4096;
const BATCH_MATCHES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExtentQueryError {
    View(ExtentViewError), Stream(StreamError), InvalidLimits, InvalidRange,
    ResourceDenied, AllocationFailed, OwnerMismatch, UnsupportedText,
    Canceled, StaleGeneration, StaleObservation,
}
impl std::fmt::Display for ExtentQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::View(error) => write!(f, "{error}"),
            Self::Stream(error) => write!(f, "{error}"),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtentQueryState { Pending, Finished, Truncated, Canceled, Failed(ExtentQueryError) }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtentQueryInput { OriginalBytes, WindowUtf8 }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtentQueryOptions { pub generation: QueryGeneration, pub max_matches: usize }
impl ExtentQueryOptions {
    pub fn new(generation: QueryGeneration) -> Self { Self { generation, max_matches: 1000 } }
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

pub struct ExtentQuery<'source, 'needle> {
    input: Input<'source>,
    cursor: ByteSearchCursor<'needle>,
    options: ExtentQueryOptions,
    hits: Vec<ExtentMatch<'source>>,
    state: ExtentQueryState,
    last_step_bytes: usize,
    _lease: ResourceLease,
}
impl<'source, 'needle> ExtentQuery<'source, 'needle> {
    pub fn raw(view: &'source ExtentView, scope: ByteRange, needle: &'needle [u8], options: ExtentQueryOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, ExtentQueryError> {
        let bytes = view.raw_selection(scope)?;
        Self::new(Input::Raw { extent: view.extent(), bytes, scope }, needle, options, budget, allocation)
    }
    /// Exact decoded-text mode only. Normalization is not silently approximated.
    /// Malformed replacement windows can still be searched via the raw route.
    pub fn text(text: &'source ExtentText, needle: &'needle str, options: ExtentQueryOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, ExtentQueryError> {
        if text.has_replacements() { return Err(ExtentQueryError::UnsupportedText); }
        Self::new(Input::Text(text), needle.as_bytes(), options, budget, allocation)
    }
    fn new(input: Input<'source>, needle: &'needle [u8], options: ExtentQueryOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, ExtentQueryError> {
        if options.generation.owner() != input.extent().request().file().owner() { return Err(ExtentQueryError::OwnerMismatch); }
        if options.max_matches > MAX_EXTENT_MATCHES || needle.len() > MAX_STREAM_NEEDLE_BYTES { return Err(ExtentQueryError::InvalidLimits); }
        if needle.is_empty() { return Err(StreamError::EmptyNeedle.into()); }
        // Output, KMP failure table and the cursor's bounded temporary batch.
        let charge = options.max_matches.checked_mul(size_of::<ExtentMatch<'source>>())
            .and_then(|n| n.checked_add(2 * needle.len() * size_of::<usize>()))
            .and_then(|n| n.checked_add(2 * BATCH_MATCHES * size_of::<ByteMatch>() + size_of::<Self>()))
            .ok_or(ExtentQueryError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(options.generation.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ExtentQueryError::ResourceDenied)?;
        let mut hits = Vec::new();
        hits.try_reserve_exact(options.max_matches).map_err(|_| ExtentQueryError::AllocationFailed)?;
        if hits.capacity() > options.max_matches { return Err(ExtentQueryError::ResourceDenied); }
        let state = if input.bytes().is_empty() { ExtentQueryState::Finished } else { ExtentQueryState::Pending };
        Ok(Self { input, cursor: ByteSearchCursor::new(needle)?, options, hits, state, last_step_bytes: 0, _lease: lease })
    }
    pub fn scope(&self) -> ByteRange { self.input.scope() }
    pub fn input_kind(&self) -> ExtentQueryInput {
        match self.input { Input::Raw { .. } => ExtentQueryInput::OriginalBytes, Input::Text(_) => ExtentQueryInput::WindowUtf8 }
    }
    pub fn state(&self) -> ExtentQueryState { self.state }
    pub fn hits(&self) -> &[ExtentMatch<'source>] { &self.hits }
    pub fn matches_seen(&self) -> u64 { self.cursor.matches_seen() }
    /// Counted in input_kind() units: raw bytes or local decoded UTF-8 bytes.
    pub fn scanned_input_bytes(&self) -> u64 { self.cursor.scanned_bytes() }
    pub fn last_step_bytes(&self) -> usize { self.last_step_bytes }
    /// Exhaustive ONLY within scope(), never within the unseen rest of a file.
    pub fn scope_complete(&self) -> bool { self.state == ExtentQueryState::Finished }
    pub fn has_unsearched_source(&self) -> bool {
        self.scope().start().get() != 0 || self.scope().end().get() != self.input.extent().observed_length().get()
    }
    pub fn cancel(&mut self) { self.cursor.cancel(); self.state = ExtentQueryState::Canceled; }
    pub fn step(&mut self, max_input_bytes: usize, generation: QueryGeneration,
        mut canceled: impl FnMut() -> bool) -> Result<ExtentQueryState, ExtentQueryError> {
        self.last_step_bytes = 0;
        if generation != self.options.generation { self.cancel(); return Err(ExtentQueryError::StaleGeneration); }
        if canceled() { self.cancel(); return Err(ExtentQueryError::Canceled); }
        if let ExtentQueryState::Failed(error) = self.state { return Err(error); }
        if self.state != ExtentQueryState::Pending || max_input_bytes == 0 { return Ok(self.state); }
        let offset = usize::try_from(self.cursor.scanned_bytes()).map_err(|_| ExtentQueryError::InvalidRange)?;
        let input = &self.input.bytes()[offset..];
        let hit_limit = (self.options.max_matches - self.hits.len()).saturating_add(1).min(BATCH_MATCHES);
        let batch = match self.cursor.step(input, max_input_bytes, hit_limit) {
            Ok(batch) => batch,
            Err(error) => { self.state = ExtentQueryState::Failed(error.into()); return Err(error.into()); }
        };
        self.last_step_bytes = batch.consumed;
        for hit in batch.hits {
            if self.hits.len() == self.options.max_matches { self.state = ExtentQueryState::Truncated; break; }
            match self.input.hit(hit, generation) {
                Ok(hit) => self.hits.push(hit),
                Err(error) => { self.state = ExtentQueryState::Failed(error); return Err(error); }
            }
        }
        if canceled() { self.cancel(); return Err(ExtentQueryError::Canceled); }
        if self.state == ExtentQueryState::Pending && self.cursor.scanned_bytes() == self.input.bytes().len() as u64 {
            self.state = ExtentQueryState::Finished;
        }
        Ok(self.state)
    }
}

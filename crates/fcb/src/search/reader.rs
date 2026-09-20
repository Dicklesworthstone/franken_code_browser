#![forbid(unsafe_code)]

//! Renderer-neutral reading of a retained facade capture (FCB-019.A).
//!
//! Construction does not decode, hash, copy or scan the source. Sparse byte
//! checkpoints are capacity-admitted once; there is no per-line offset vector.
//! Indexing and far jumps are separate resumable worker operations. A query
//! borrows the immutable capture, not a mutable index, so later indexing cannot
//! invalidate it. No provider, filesystem, runtime or native service is invoked.
//!
//! Reading rows use editor semantics: an empty capture has line 1 and a final
//! newline creates a final empty row. CRLF is one terminator, including UTF-16.
//! This is deliberately distinct from the legacy physical-line counter. Byte
//! offsets inside a terminator belong to the preceding row; EOF belongs to the
//! last row. UTF-8 and BOM-marked UTF-16 use original-byte coordinates throughout.

use std::mem::size_of;
use fcb_core::{ByteLength, ByteOffset, ByteRange, FileId, QueryGeneration,
    ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
pub use fcb_source::LineNumber;
use fcb_source::{DetectedEncoding, detect_encoding};
use crate::{BrowserView, SourceCapture};
use super::SearchMatch;

pub const MAX_READER_STEP_BYTES: usize = 64 * 1024;
pub const MAX_READER_CHECKPOINTS: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReaderError {
    InvalidLimits, InvalidRange, AllocationFailed, ResourceDenied, OwnerMismatch,
    StaleSource, StaleQuery, LineOutOfBounds, Canceled, StepTooSmall, EncodingError,
}
impl ReaderError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "READER_INVALID_LIMITS",
            Self::InvalidRange => "READER_INVALID_RANGE",
            Self::AllocationFailed => "READER_ALLOCATION_FAILED",
            Self::ResourceDenied => "READER_RESOURCE_DENIED",
            Self::OwnerMismatch => "READER_OWNER_MISMATCH",
            Self::StaleSource => "READER_STALE_SOURCE",
            Self::StaleQuery => "READER_STALE_QUERY",
            Self::LineOutOfBounds => "READER_LINE_OUT_OF_BOUNDS",
            Self::Canceled => "READER_CANCELED",
            Self::StepTooSmall => "READER_STEP_TOO_SMALL",
            Self::EncodingError => "READER_ENCODING_ERROR",
        }
    }
}
impl std::fmt::Display for ReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for ReaderError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReaderLimits {
    pub max_checkpoints: usize,
    pub min_checkpoint_bytes: usize,
}
impl Default for ReaderLimits {
    fn default() -> Self { Self { max_checkpoints: 4096, min_checkpoint_bytes: 4096 } }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadingTarget {
    Line(LineNumber),
    Byte(ByteOffset),
    /// Preserve the exact selection separately from the containing line.
    Range(ByteRange),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadingAnchor {
    pub(super) file: FileId,
    pub(super) revision: SourceRevision,
    pub(super) generation: QueryGeneration,
    pub(super) line: u64,
    pub(super) line_start: usize,
    pub(super) offset: usize,
    pub(super) selection: Option<ByteRange>,
}
impl ReadingAnchor {
    pub const fn file(&self) -> FileId { self.file }
    pub const fn revision(&self) -> SourceRevision { self.revision }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn line_number(&self) -> u64 { self.line }
    pub const fn line_start(&self) -> ByteOffset { ByteOffset::new(self.line_start as u64) }
    pub const fn offset(&self) -> ByteOffset { ByteOffset::new(self.offset as u64) }
    pub const fn selection(&self) -> Option<ByteRange> { self.selection }
    pub fn validate_delivery(&self, source: &SourceCapture, generation: QueryGeneration) -> Result<(), ReaderError> {
        if self.file.owner() != source.owner() || generation.owner() != source.owner() {
            return Err(ReaderError::OwnerMismatch);
        }
        if self.file != source.file() || self.revision != source.revision() { return Err(ReaderError::StaleSource); }
        if self.generation != generation { return Err(ReaderError::StaleQuery); }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadingSeekState { Pending, Ready(ReadingAnchor), OutOfRange, Canceled }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReaderIndexProgress {
    pub indexed_through: ByteOffset,
    pub known_lines: u64,
    pub total_lines: Option<u64>,
    pub checkpoint_count: usize,
    pub step_bytes: usize,
    pub canceled: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Cursor {
    pub(super) offset: usize,
    pub(super) line: u64,
    pub(super) start: usize,
}
impl Cursor {
    fn advance(&mut self, width: usize, newline: bool) -> Result<(), ReaderError> {
        self.offset = self.offset.checked_add(width).ok_or(ReaderError::InvalidRange)?;
        if newline {
            self.line = self.line.checked_add(1).ok_or(ReaderError::InvalidRange)?;
            self.start = self.offset;
        }
        Ok(())
    }
}

pub struct SourceReader<'source> {
    pub(super) source: &'source SourceCapture,
    pub(super) encoding: DetectedEncoding,
    pub(super) content_start: usize,
    checkpoints: Vec<Cursor>,
    cursor: Cursor,
    stride: usize,
    next_checkpoint: usize,
    capacity: usize,
    _lease: ResourceLease,
}
impl<'source> SourceReader<'source> {
    pub fn new(source: &'source SourceCapture, limits: ReaderLimits,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, ReaderError> {
        if !(2..=MAX_READER_CHECKPOINTS).contains(&limits.max_checkpoints) || limits.min_checkpoint_bytes == 0 {
            return Err(ReaderError::InvalidLimits);
        }
        let length = source.bytes().len();
        let encoding = detect_encoding(&source.bytes()[..length.min(3)]);
        let content_start = (encoding.bom_bytes_len() as usize).min(length);
        // The capture length is known even before its line count is known.
        // Choose a stride that fits the entire file within a fixed allocation.
        let stride = length.div_ceil(limits.max_checkpoints - 1).max(limits.min_checkpoint_bytes);
        let capacity = (length / stride).saturating_add(1).min(limits.max_checkpoints);
        let charge = size_of::<Self>().checked_add(capacity.checked_mul(size_of::<Cursor>())
            .ok_or(ReaderError::InvalidLimits)?).ok_or(ReaderError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(source.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ReaderError::ResourceDenied)?;
        let mut checkpoints = Vec::new();
        checkpoints.try_reserve_exact(capacity).map_err(|_| ReaderError::AllocationFailed)?;
        if checkpoints.capacity() > capacity { return Err(ReaderError::ResourceDenied); }
        let cursor = Cursor { offset: content_start, line: 1, start: content_start };
        checkpoints.push(cursor);
        Ok(Self { source, encoding, content_start, checkpoints, cursor, stride,
            next_checkpoint: content_start.saturating_add(stride), capacity, _lease: lease })
    }
    pub fn source(&self) -> &'source SourceCapture { self.source }
    pub const fn encoding(&self) -> DetectedEncoding { self.encoding }
    pub fn checkpoint_count(&self) -> usize { self.checkpoints.len() }
    pub const fn checkpoint_capacity(&self) -> usize { self.capacity }
    pub const fn checkpoint_stride_bytes(&self) -> usize { self.stride }
    pub fn progress(&self) -> ReaderIndexProgress {
        ReaderIndexProgress { indexed_through: ByteOffset::new(self.cursor.offset as u64),
            known_lines: self.cursor.line,
            total_lines: (self.cursor.offset == self.source.bytes().len()).then_some(self.cursor.line),
            checkpoint_count: self.checkpoints.len(), step_bytes: 0, canceled: false }
    }

    /// Advance at most min(max_bytes, MAX_READER_STEP_BYTES) original bytes.
    /// Newline recognition uses at most two bytes of lookahead beyond that
    /// consumed prefix. Cancellation retains useful checkpoints for other
    /// queries; it does not poison an immutable source or shared index.
    pub fn index_step(&mut self, max_bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<ReaderIndexProgress, ReaderError> {
        let allowance = max_bytes.min(MAX_READER_STEP_BYTES);
        let mut used = 0;
        let mut was_canceled = false;
        while self.cursor.offset < self.source.bytes().len() {
            if canceled() { was_canceled = true; break; }
            if used == allowance { break; }
            let (width, newline) = newline_token(self.source.bytes(), self.encoding, self.cursor.offset);
            if width > allowance - used {
                if used == 0 { return Err(ReaderError::StepTooSmall); }
                break;
            }
            self.cursor.advance(width, newline)?;
            used += width;
            if self.cursor.offset >= self.next_checkpoint && self.checkpoints.len() < self.capacity {
                self.checkpoints.push(self.cursor);
                self.next_checkpoint = self.cursor.offset.saturating_add(self.stride);
            }
        }
        let mut progress = self.progress();
        progress.step_bytes = used;
        progress.canceled = was_canceled || canceled();
        Ok(progress)
    }

    /// Start an independent request from the nearest retained checkpoint.
    /// No far-line scan occurs in this call. The returned request can outlive
    /// this index while the immutable source is retained by its host.
    pub fn seek(&self, target: ReadingTarget, generation: QueryGeneration) -> Result<ReadingSeek<'source>, ReaderError> {
        if generation.owner() != self.source.owner() { return Err(ReaderError::OwnerMismatch); }
        let byte = match target {
            ReadingTarget::Line(_) => None,
            ReadingTarget::Byte(offset) => Some(offset.get()),
            ReadingTarget::Range(range) => {
                if range.end().get() > self.source.bytes().len() as u64 { return Err(ReaderError::InvalidRange); }
                Some(range.start().get())
            }
        };
        let offset = byte.map(|value| usize::try_from(value).map_err(|_| ReaderError::InvalidRange)).transpose()?;
        if offset.is_some_and(|value| value > self.source.bytes().len()) { return Err(ReaderError::InvalidRange); }
        let offset = offset.map(|value| value.max(self.content_start));
        let eligible = |cursor: &Cursor| match target {
            ReadingTarget::Line(line) => cursor.line <= line.get(),
            _ => cursor.offset <= offset.unwrap_or(self.content_start),
        };
        let count = self.checkpoints.partition_point(eligible);
        let mut cursor = self.checkpoints[count.saturating_sub(1)];
        if eligible(&self.cursor) { cursor = self.cursor; }
        let mut seek = ReadingSeek { source: self.source, encoding: self.encoding, target,
            target_byte: offset, generation, cursor, state: ReadingSeekState::Pending,
            scanned_bytes: 0, last_step_bytes: 0 };
        seek.resolve_here();
        Ok(seek)
    }

    /// Locate an exact content hit without decoding or reopening its source.
    pub fn seek_hit(&self, hit: &SearchMatch, generation: QueryGeneration) -> Result<ReadingSeek<'source>, ReaderError> {
        if hit.file_id.owner() != self.source.owner() || hit.revision.owner() != self.source.owner() {
            return Err(ReaderError::OwnerMismatch);
        }
        super::exact_hit_bytes(self.source, hit).map_err(|_| ReaderError::StaleSource)?;
        self.seek(ReadingTarget::Range(hit.original_byte_range), generation)
    }
}

impl BrowserView {
    /// Prepare worker-side reading on exactly this view's bytes, with no
    /// provider request, source copy, digest pass, runtime or native resource.
    pub fn source_reader(&self, limits: ReaderLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<SourceReader<'_>, ReaderError> {
        SourceReader::new(self.source(), limits, budget, allocation)
    }
}

pub struct ReadingSeek<'source> {
    source: &'source SourceCapture,
    encoding: DetectedEncoding,
    target: ReadingTarget,
    target_byte: Option<usize>,
    generation: QueryGeneration,
    cursor: Cursor,
    state: ReadingSeekState,
    scanned_bytes: u64,
    last_step_bytes: usize,
}
impl ReadingSeek<'_> {
    pub const fn state(&self) -> ReadingSeekState { self.state }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn scanned_bytes(&self) -> u64 { self.scanned_bytes }
    pub const fn last_step_bytes(&self) -> usize { self.last_step_bytes }
    pub const fn scanned_through(&self) -> ByteOffset { ByteOffset::new(self.cursor.offset as u64) }
    pub fn cancel(&mut self) {
        if self.state == ReadingSeekState::Pending { self.state = ReadingSeekState::Canceled; }
    }
    pub fn step(&mut self, max_bytes: usize, active_generation: QueryGeneration,
        mut canceled: impl FnMut() -> bool) -> Result<ReadingSeekState, ReaderError> {
        self.last_step_bytes = 0;
        if active_generation != self.generation { self.state = ReadingSeekState::Canceled; return Err(ReaderError::StaleQuery); }
        if canceled() { self.cancel(); return Err(ReaderError::Canceled); }
        let allowance = max_bytes.min(MAX_READER_STEP_BYTES);
        while self.state == ReadingSeekState::Pending && self.last_step_bytes < allowance {
            if canceled() { self.cancel(); return Err(ReaderError::Canceled); }
            let (width, newline) = newline_token(self.source.bytes(), self.encoding, self.cursor.offset);
            if width > allowance - self.last_step_bytes {
                if self.last_step_bytes == 0 { return Err(ReaderError::StepTooSmall); }
                break;
            }
            let before = self.cursor;
            self.cursor.advance(width, newline)?;
            self.last_step_bytes += width;
            self.scanned_bytes += width as u64;
            if let Some(target) = self.target_byte.filter(|&target| target < self.cursor.offset) {
                self.state = ReadingSeekState::Ready(self.anchor(before, target));
            } else { self.resolve_here(); }
        }
        if canceled() { self.state = ReadingSeekState::Canceled; return Err(ReaderError::Canceled); }
        Ok(self.state)
    }
    fn resolve_here(&mut self) {
        let resolved = match self.target {
            ReadingTarget::Line(line) if self.cursor.line == line.get() => Some(self.cursor.start),
            ReadingTarget::Line(_) => None,
            _ => self.target_byte.filter(|&target| target == self.cursor.offset),
        };
        if let Some(offset) = resolved {
            self.state = ReadingSeekState::Ready(self.anchor(self.cursor, offset));
        } else if self.cursor.offset == self.source.bytes().len() { self.state = ReadingSeekState::OutOfRange; }
    }
    fn anchor(&self, cursor: Cursor, offset: usize) -> ReadingAnchor {
        ReadingAnchor { file: self.source.file(), revision: self.source.revision(), generation: self.generation,
            line: cursor.line, line_start: cursor.start, offset,
            selection: match self.target { ReadingTarget::Range(range) => Some(range), _ => None } }
    }
}

/// Only recognizes source terminators, not text shaping or column semantics.
/// An incomplete final UTF-16 unit remains one ordinary raw byte for the
/// existing decoder to label malformed; it cannot invent a newline.
pub(super) fn newline_token(bytes: &[u8], encoding: DetectedEncoding, offset: usize) -> (usize, bool) {
    let width = if encoding.is_utf16() { 2 } else { 1 };
    if offset + width > bytes.len() { return (1, false); }
    let unit = read_unit(bytes, encoding, offset);
    if unit == 13 && offset + width * 2 <= bytes.len() && read_unit(bytes, encoding, offset + width) == 10 {
        (width * 2, true)
    } else { (width, unit == 13 || unit == 10) }
}
pub(super) fn read_unit(bytes: &[u8], encoding: DetectedEncoding, offset: usize) -> u16 {
    match encoding {
        DetectedEncoding::Utf16Le => u16::from_le_bytes([bytes[offset], bytes[offset + 1]]),
        DetectedEncoding::Utf16Be => u16::from_be_bytes([bytes[offset], bytes[offset + 1]]),
        _ => u16::from(bytes[offset]),
    }
}

/// Owned sparse indexing and far-jump continuations using this same reader.
pub mod retained;

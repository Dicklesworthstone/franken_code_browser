#![forbid(unsafe_code)]

//! Resumable sparse line index and exact byte/line translation (FCB-012.A / fcb-37r.1).
//!
//! The retained index keeps exact local line starts as well as sparse absolute
//! checkpoints. For streaming files without retaining a per-line table, use
//! [`LineWindowScanner`]. Neither a chunk boundary nor an exhausted step budget
//! establishes EOF. Line jumps never publish a provisional split-CRLF offset.

mod window;
pub use window::{LineWindowError, LineWindowScanner, LineWindowStatus};

use fcb_core::{ByteLength, ByteOffset};
use crate::chunk::{ChunkedCapture, ChunkSize, SourceChunk};
use crate::{CaptureRequest, ObservationDigest, SourceError};

/// A 1-indexed logical source line number.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineNumber(u64);

impl LineNumber {
    /// Construct a 1-indexed line number. Line 1 is the first line.
    pub const fn new(one_based: u64) -> Result<Self, SourceError> {
        if one_based == 0 { Err(SourceError::InvalidRange) } else { Ok(Self(one_based)) }
    }
    pub const fn get(self) -> u64 { self.0 }
    pub const fn zero_based(self) -> u64 { self.0 - 1 }
}

impl std::fmt::Display for LineNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

/// Sparse checkpoint pairing a logical line number with its absolute byte offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineCheckpoint {
    pub line: LineNumber,
    pub byte_offset: ByteOffset,
}

/// Inclusive logical lines and their half-open original byte range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineRangeOffsets {
    pub start_line: LineNumber,
    pub end_line: LineNumber,
    pub start_offset: ByteOffset,
    pub end_offset: ByteOffset,
}

/// The result of an exact jump to a requested line number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineJumpResult {
    Resolved { line: LineNumber, offset: ByteOffset },
    PendingFarJump { indexed_through_line: u64, requested_line: LineNumber },
}

/// Retained line index with exact local starts and sparse absolute checkpoints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SparseLineIndex {
    total_lines: u64,
    total_bytes: u64,
    stride: u64,
    checkpoints: Vec<LineCheckpoint>,
    line_starts: Vec<u64>,
    is_complete: bool,
}

impl SparseLineIndex {
    pub fn new(stride: u64) -> Self {
        Self { total_lines: 0, total_bytes: 0, stride: stride.max(1),
            checkpoints: Vec::new(), line_starts: Vec::new(), is_complete: false }
    }
    pub const fn total_lines(&self) -> u64 { self.total_lines }
    pub const fn total_bytes(&self) -> u64 { self.total_bytes }
    pub const fn stride(&self) -> u64 { self.stride }
    pub const fn is_complete(&self) -> bool { self.is_complete }
    pub fn checkpoints(&self) -> &[LineCheckpoint] { &self.checkpoints }

    /// Resolve a logical line number to its exact starting byte offset.
    /// A preceding sparse checkpoint is not an exact answer for another line.
    pub fn line_to_byte(&self, line: LineNumber) -> Result<ByteOffset, SourceError> {
        if line.get() > self.total_lines { return Err(SourceError::RangeOutOfBounds); }
        let idx = usize::try_from(line.zero_based()).map_err(|_| SourceError::RangeOutOfBounds)?;
        self.line_starts.get(idx).copied().map(ByteOffset::new)
            .ok_or(SourceError::RangeOutOfBounds)
    }

    /// Resolve an absolute byte offset to its containing logical line number.
    pub fn byte_to_line(&self, offset: ByteOffset) -> Result<LineNumber, SourceError> {
        if offset.get() > self.total_bytes || self.total_lines == 0 {
            return Err(SourceError::RangeOutOfBounds);
        }
        let idx = match self.line_starts.binary_search(&offset.get()) {
            Ok(exact) => exact,
            Err(next) => next.saturating_sub(1),
        };
        let line = u64::try_from(idx).ok().and_then(|value| value.checked_add(1))
            .ok_or(SourceError::RangeOutOfBounds)?;
        LineNumber::new(line)
    }
}

/// Resumable byte-oriented line indexing for a single immutable capture.
/// Use the encoding-aware [`LineWindowScanner`] for UTF-16 streams.
#[derive(Clone, Debug)]
pub struct ResumableLineScanner {
    stride: u64,
    scanned_chunks: usize,
    current_byte_offset: u64,
    current_line: u64,
    last_chunk_ended_with_cr: bool,
    pending_line_start: bool,
    checkpoints: Vec<LineCheckpoint>,
    line_starts: Vec<u64>,
    is_complete: bool,
    binding: Option<(CaptureRequest, ByteLength, ChunkSize, ObservationDigest)>,
}

impl ResumableLineScanner {
    pub fn new(stride: u64) -> Self {
        Self { stride: stride.max(1), scanned_chunks: 0, current_byte_offset: 0,
            current_line: 0, last_chunk_ended_with_cr: false, pending_line_start: true,
            checkpoints: Vec::new(), line_starts: Vec::new(), is_complete: false, binding: None }
    }
    pub const fn scanned_chunks(&self) -> usize { self.scanned_chunks }
    pub const fn current_line(&self) -> u64 { self.current_line }
    pub const fn is_complete(&self) -> bool { self.is_complete }

    fn validate_capture(&mut self, capture: &ChunkedCapture) -> Result<(), SourceError> {
        let key = (*capture.request(), capture.total_length(), capture.chunk_size(), capture.digest());
        if let Some(bound) = self.binding {
            if bound != key { return Err(SourceError::MetadataMismatch); }
        } else { self.binding = Some(key); }
        Ok(())
    }

    /// Scan at most `chunk_budget` chunks. Zero is a no-op for nonempty input.
    /// A scanner cannot be continued or finalized against a different capture.
    pub fn step(&mut self, capture: &ChunkedCapture, chunk_budget: usize) -> Result<bool, SourceError> {
        self.validate_capture(capture)?;
        if self.is_complete { return Ok(true); }
        let total_chunks = capture.chunk_count();
        let end_chunk = self.scanned_chunks.saturating_add(chunk_budget).min(total_chunks);
        while self.scanned_chunks < end_chunk {
            let chunk = capture.chunk(self.scanned_chunks).ok_or(SourceError::ChunkOutOfBounds)?;
            self.scan_chunk(chunk)?;
            self.scanned_chunks += 1;
        }
        self.is_complete = self.scanned_chunks == total_chunks;
        Ok(self.is_complete)
    }

    fn scan_chunk(&mut self, chunk: &SourceChunk) -> Result<(), SourceError> {
        if chunk.range().start().get() != self.current_byte_offset {
            return Err(SourceError::MetadataMismatch);
        }
        for &byte in chunk.bytes() {
            let offset = self.current_byte_offset;
            self.current_byte_offset = offset.checked_add(1).ok_or(SourceError::RangeOutOfBounds)?;
            if self.last_chunk_ended_with_cr {
                self.last_chunk_ended_with_cr = false;
                if byte == b'\n' { continue; }
            }
            // A terminator does not prove that another line exists. Wait for
            // its first byte, including across a CRLF split, before publishing.
            if self.pending_line_start {
                self.record_line(offset)?;
                self.pending_line_start = false;
            }
            match byte {
                b'\r' => { self.pending_line_start = true; self.last_chunk_ended_with_cr = true; }
                b'\n' => self.pending_line_start = true,
                _ => {}
            }
        }
        Ok(())
    }

    fn record_line(&mut self, start: u64) -> Result<(), SourceError> {
        self.current_line = self.current_line.checked_add(1).ok_or(SourceError::RangeOutOfBounds)?;
        self.line_starts.push(start);
        if self.current_line == 1 || self.current_line.is_multiple_of(self.stride) {
            self.checkpoints.push(LineCheckpoint {
                line: LineNumber::new(self.current_line)?, byte_offset: ByteOffset::new(start),
            });
        }
        Ok(())
    }

    /// Synchronous convenience for an already-retained capture. Includes the
    /// entire final requested line, not merely its starting offset. For bounded
    /// interaction work, drive `step` or `LineWindowScanner` instead.
    pub fn locate_first_screen(&mut self, capture: &ChunkedCapture, screen_lines: u32)
        -> Result<LineRangeOffsets, SourceError> {
        self.validate_capture(capture)?;
        if screen_lines == 0 { return Err(SourceError::InvalidRange); }
        let target = u64::from(screen_lines);
        while self.current_line <= target && !self.is_complete { self.step(capture, 1)?; }
        if self.current_line == 0 { return Err(SourceError::CaptureUnavailable); }
        let last = self.current_line.min(target);
        let next = usize::try_from(last).map_err(|_| SourceError::RangeOutOfBounds)?;
        let end = self.line_starts.get(next).copied().unwrap_or(self.current_byte_offset);
        Ok(LineRangeOffsets {
            start_line: LineNumber::new(1)?, end_line: LineNumber::new(last)?,
            start_offset: ByteOffset::new(0), end_offset: ByteOffset::new(end),
        })
    }

    /// Only an observed, immutable line start can resolve. To distinguish
    /// permanent absence at EOF, use `resume_until_line` or the finished index.
    pub fn jump_to_line(&self, target: LineNumber) -> LineJumpResult {
        if target.get() <= self.current_line {
            if let Ok(idx) = usize::try_from(target.zero_based()) {
                if let Some(&offset) = self.line_starts.get(idx) {
                    return LineJumpResult::Resolved { line: target, offset: ByteOffset::new(offset) };
                }
            }
        }
        LineJumpResult::PendingFarJump { indexed_through_line: self.current_line, requested_line: target }
    }

    /// Advance by at most the caller's TOTAL chunk budget, then yield. Repeated
    /// calls resume far jumps; zero never spins. EOF makes absence an error.
    pub fn resume_until_line(&mut self, capture: &ChunkedCapture, target: LineNumber, chunk_budget: usize)
        -> Result<LineJumpResult, SourceError> {
        self.validate_capture(capture)?;
        for _ in 0..chunk_budget {
            if self.current_line >= target.get() || self.is_complete { break; }
            self.step(capture, 1)?;
        }
        if self.is_complete && self.current_line < target.get() { return Err(SourceError::RangeOutOfBounds); }
        Ok(self.jump_to_line(target))
    }

    /// Scan all remaining chunks and produce an immutable retained index.
    pub fn finish(&mut self, capture: &ChunkedCapture) -> Result<SparseLineIndex, SourceError> {
        self.validate_capture(capture)?;
        while !self.is_complete { self.step(capture, 32)?; }
        Ok(SparseLineIndex {
            total_lines: self.current_line, total_bytes: capture.total_length().get(), stride: self.stride,
            checkpoints: self.checkpoints.clone(), line_starts: self.line_starts.clone(), is_complete: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use fcb_core::{ArenaOwnerId, ByteRange, FileId, SourceRevision};

    fn sample_capture(data: &[u8], chunk_size_bytes: usize) -> ChunkedCapture {
        let owner = ArenaOwnerId::new(1).unwrap();
        let request = CaptureRequest::new(FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap()).unwrap();
        let mut chunks = Vec::new();
        for (index, bytes) in data.chunks(chunk_size_bytes).enumerate() {
            let start = index * chunk_size_bytes;
            let range = ByteRange::new(ByteOffset::new(start as u64), ByteOffset::new((start + bytes.len()) as u64)).unwrap();
            chunks.push(SourceChunk::new(index as u32, range, Arc::from(bytes)).unwrap());
        }
        ChunkedCapture::new(request, ByteLength::new(data.len() as u64), ChunkSize::bounded(chunk_size_bytes).unwrap(), chunks).unwrap()
    }

    #[test]
    fn empty_file_line_indexing() {
        let cap = sample_capture(b"", 16);
        let index = ResumableLineScanner::new(4).finish(&cap).unwrap();
        assert_eq!(index.total_lines(), 0);
        assert_eq!(index.total_bytes(), 0);
        assert!(index.is_complete());
    }

    #[test]
    fn single_line_without_newline() {
        let cap = sample_capture(b"hello world", 16);
        let index = ResumableLineScanner::new(4).finish(&cap).unwrap();
        assert_eq!(index.total_lines(), 1);
        assert_eq!(index.line_to_byte(LineNumber::new(1).unwrap()).unwrap().get(), 0);
    }

    #[test]
    fn multiline_with_lf_and_crlf() {
        let cap = sample_capture(b"line 1\nline 2\r\nline 3\n", 16);
        let index = ResumableLineScanner::new(2).finish(&cap).unwrap();
        assert_eq!(index.total_lines(), 3);
        for (line, offset) in [(1, 0), (2, 7), (3, 15)] {
            assert_eq!(index.line_to_byte(LineNumber::new(line).unwrap()).unwrap().get(), offset);
            assert_eq!(index.byte_to_line(ByteOffset::new(offset)).unwrap().get(), line);
        }
        assert_eq!(index.byte_to_line(ByteOffset::new(5)).unwrap().get(), 1);
    }

    #[test]
    fn boundary_split_crlf_across_chunks() {
        let cap = sample_capture(b"alpha 1\r\nbeta 2\n", 8);
        assert_eq!(cap.chunk_count(), 2);
        let index = ResumableLineScanner::new(2).finish(&cap).unwrap();
        assert_eq!(index.total_lines(), 2);
        assert_eq!(index.line_to_byte(LineNumber::new(1).unwrap()).unwrap().get(), 0);
        assert_eq!(index.line_to_byte(LineNumber::new(2).unwrap()).unwrap().get(), 9);
    }

    #[test]
    fn early_first_screen_without_full_indexing() {
        let text: String = (1..=200).map(|i| format!("line {i}\n")).collect();
        let cap = sample_capture(text.as_bytes(), 64);
        let mut scanner = ResumableLineScanner::new(10);
        let screen = scanner.locate_first_screen(&cap, 20).unwrap();
        assert_eq!(screen.start_line.get(), 1);
        assert_eq!(screen.end_line.get(), 20);
        assert_eq!(screen.start_offset.get(), 0);
        let expected: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        assert_eq!(&text.as_bytes()[..screen.end_offset.get() as usize], expected.as_bytes());
        assert!(!scanner.is_complete());
        assert!(scanner.scanned_chunks() < cap.chunk_count());
    }

    #[test]
    fn pending_far_jump_and_resumption() {
        let text: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let cap = sample_capture(text.as_bytes(), 32);
        let mut scanner = ResumableLineScanner::new(5);
        scanner.step(&cap, 1).unwrap();
        let far = LineNumber::new(80).unwrap();
        assert!(matches!(scanner.jump_to_line(far), LineJumpResult::PendingFarJump { .. }));
        let mut resolved = false;
        for _ in 0..cap.chunk_count() {
            let before = scanner.scanned_chunks();
            let result = scanner.resume_until_line(&cap, far, 2).unwrap();
            assert!(scanner.scanned_chunks() - before <= 2);
            if let LineJumpResult::Resolved { line, offset } = result {
                assert_eq!(line, far);
                assert!(text[offset.get() as usize..].starts_with("line 80\n"));
                resolved = true; break;
            }
        }
        assert!(resolved);
    }

    #[test]
    fn zero_and_maximum_step_budgets_are_safe() {
        let cap = sample_capture(b"a\nb\nc\n", 1);
        let mut scanner = ResumableLineScanner::new(1);
        let far = LineNumber::new(3).unwrap();
        assert!(matches!(scanner.resume_until_line(&cap, far, 0).unwrap(), LineJumpResult::PendingFarJump { .. }));
        assert_eq!(scanner.scanned_chunks(), 0);
        scanner.step(&cap, 1).unwrap();
        assert!(scanner.step(&cap, usize::MAX).unwrap());
    }

    #[test]
    fn terminal_split_crlf_never_invents_or_moves_a_line() {
        for bytes in [b"a\r\n".as_slice(), b"\r\n", b"a\n\r\n", b"a\r\nb"] {
            for chunk in 1..=bytes.len() {
                let cap = sample_capture(bytes, chunk);
                let mut scanner = ResumableLineScanner::new(1);
                let mut resolved = Vec::new();
                while !scanner.is_complete() {
                    scanner.step(&cap, 1).unwrap();
                    for line in 1..=scanner.current_line() {
                        if let LineJumpResult::Resolved { offset, .. } = scanner.jump_to_line(LineNumber::new(line).unwrap()) {
                            if let Some(&prior) = resolved.get(line as usize - 1) { assert_eq!(prior, offset); }
                            else { resolved.push(offset); }
                        }
                    }
                }
                let index = scanner.finish(&cap).unwrap();
                let expected = if bytes == b"a\n\r\n" || bytes == b"a\r\nb" { 2 } else { 1 };
                assert_eq!(index.total_lines(), expected);
            }
        }
    }

    #[test]
    fn a_resolved_scanner_cannot_be_reused_for_another_capture() {
        let first = sample_capture(b"a\nb\n", 2);
        let other = sample_capture(b"abcd", 2);
        let mut scanner = ResumableLineScanner::new(1);
        scanner.step(&first, 1).unwrap();
        assert_eq!(scanner.step(&other, 1), Err(SourceError::MetadataMismatch));
        assert_eq!(scanner.scanned_chunks(), 1);
        scanner.finish(&first).unwrap();
        assert_eq!(scanner.finish(&other), Err(SourceError::MetadataMismatch));
    }

    #[test]
    fn first_screen_includes_the_final_line_and_eof_is_not_pending_forever() {
        let cap = sample_capture(b"only\r\n", 1);
        let mut scanner = ResumableLineScanner::new(1);
        let screen = scanner.locate_first_screen(&cap, 1).unwrap();
        assert_eq!(screen.end_offset.get(), 6);
        assert_eq!(scanner.resume_until_line(&cap, LineNumber::new(2).unwrap(), 1), Err(SourceError::RangeOutOfBounds));
        assert_eq!(scanner.locate_first_screen(&cap, 0), Err(SourceError::InvalidRange));
    }
}

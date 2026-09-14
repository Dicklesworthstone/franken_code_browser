#![forbid(unsafe_code)]

//! Resumable sparse line index and exact byte/line translation (FCB-012.A / fcb-37r.1).
//!
//! §10.3, §10.6, §10.8:
//! A line index stores sparse absolute `u64` checkpoints plus dense local offsets within
//! selected chunks. `line → byte` and `byte → line` have explicit complexity bounds and
//! return checked results without 32-bit truncation.
//!
//! Building the line index is a resumable scan. The first visible screen can be located
//! without a full-file pass. An exact jump to a far line exposes a pending state
//! ([`LineJumpResult::PendingFarJump`]) rather than freezing the caller.
//!
//! Newline semantics explicitly handle empty files, final newlines, CRLF, bare CR, bare LF,
//! and boundary-split CRLF pairs across chunk boundaries.

use fcb_core::ByteOffset;

use crate::chunk::{ChunkedCapture, SourceChunk};
use crate::SourceError;

/// A 1-indexed logical source line number.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineNumber(u64);

impl LineNumber {
    /// Construct a 1-indexed line number. Line 1 is the first line.
    pub const fn new(one_based: u64) -> Result<Self, SourceError> {
        if one_based == 0 {
            Err(SourceError::InvalidRange)
        } else {
            Ok(Self(one_based))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn zero_based(self) -> u64 {
        self.0 - 1
    }
}

impl std::fmt::Display for LineNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Sparse checkpoint pairing a logical line number with its absolute byte offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineCheckpoint {
    pub line: LineNumber,
    pub byte_offset: ByteOffset,
}

/// The byte range corresponding to a span of logical lines.
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
    /// The line was already indexed and resolved to an exact byte offset.
    Resolved {
        line: LineNumber,
        offset: ByteOffset,
    },
    /// The requested line lies beyond the currently indexed scan extent.
    /// Exposes indexing state to allow asynchronous resumption without freezing UI.
    PendingFarJump {
        indexed_through_line: u64,
        requested_line: LineNumber,
    },
}

/// Sparse line index supporting O(log N) line ↔ byte mapping for files up to 2^64 bytes.
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
        Self {
            total_lines: 0,
            total_bytes: 0,
            stride: stride.max(1),
            checkpoints: Vec::new(),
            line_starts: Vec::new(),
            is_complete: false,
        }
    }

    pub const fn total_lines(&self) -> u64 {
        self.total_lines
    }

    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub const fn stride(&self) -> u64 {
        self.stride
    }

    pub const fn is_complete(&self) -> bool {
        self.is_complete
    }

    pub fn checkpoints(&self) -> &[LineCheckpoint] {
        &self.checkpoints
    }

    /// Resolve a logical line number to its exact starting byte offset.
    pub fn line_to_byte(&self, line: LineNumber) -> Result<ByteOffset, SourceError> {
        let line_val = line.get();
        if line_val > self.total_lines {
            return Err(SourceError::RangeOutOfBounds);
        }

        let idx = (line_val - 1) as usize;
        if idx < self.line_starts.len() {
            return Ok(ByteOffset::new(self.line_starts[idx]));
        }

        // Search sparse checkpoints
        let cp_idx = self
            .checkpoints
            .binary_search_by_key(&line_val, |cp| cp.line.get());

        match cp_idx {
            Ok(exact) => Ok(self.checkpoints[exact].byte_offset),
            Err(insert_idx) => {
                if insert_idx > 0 && insert_idx - 1 < self.checkpoints.len() {
                    Ok(self.checkpoints[insert_idx - 1].byte_offset)
                } else {
                    Err(SourceError::RangeOutOfBounds)
                }
            }
        }
    }

    /// Resolve an absolute byte offset to its containing logical line number.
    pub fn byte_to_line(&self, offset: ByteOffset) -> Result<LineNumber, SourceError> {
        let off = offset.get();
        if off > self.total_bytes {
            return Err(SourceError::RangeOutOfBounds);
        }

        if self.total_lines == 0 {
            return Err(SourceError::RangeOutOfBounds);
        }

        // Binary search line_starts
        let idx = match self.line_starts.binary_search(&off) {
            Ok(exact) => exact,
            Err(next) => {
                if next == 0 {
                    0
                } else {
                    next - 1
                }
            }
        };

        LineNumber::new((idx as u64) + 1)
    }
}

/// Resumable line scanner that indexes chunked source captures incrementally.
#[derive(Clone, Debug)]
pub struct ResumableLineScanner {
    stride: u64,
    scanned_chunks: usize,
    current_byte_offset: u64,
    current_line: u64,
    last_chunk_ended_with_cr: bool,
    checkpoints: Vec<LineCheckpoint>,
    line_starts: Vec<u64>,
    is_complete: bool,
}

impl ResumableLineScanner {
    /// Create a new resumable scanner with sparse checkpoint stride.
    pub fn new(stride: u64) -> Self {
        Self {
            stride: stride.max(1),
            scanned_chunks: 0,
            current_byte_offset: 0,
            current_line: 0,
            last_chunk_ended_with_cr: false,
            checkpoints: Vec::new(),
            line_starts: Vec::new(),
            is_complete: false,
        }
    }

    pub const fn scanned_chunks(&self) -> usize {
        self.scanned_chunks
    }

    pub const fn current_line(&self) -> u64 {
        self.current_line
    }

    pub const fn is_complete(&self) -> bool {
        self.is_complete
    }

    /// Scan a limited budget of chunks towards indexing completion.
    ///
    /// Returns `true` if scanning completed all chunks, `false` if more chunks remain.
    pub fn step(
        &mut self,
        capture: &ChunkedCapture,
        chunk_budget: usize,
    ) -> Result<bool, SourceError> {
        if self.is_complete {
            return Ok(true);
        }

        let total_chunks = capture.chunk_count();
        if total_chunks == 0 {
            self.is_complete = true;
            return Ok(true);
        }

        let end_chunk = (self.scanned_chunks + chunk_budget).min(total_chunks);

        for i in self.scanned_chunks..end_chunk {
            let chunk = capture.chunk(i).ok_or(SourceError::ChunkOutOfBounds)?;
            let is_last_chunk = i + 1 == total_chunks;
            self.scan_chunk(chunk, is_last_chunk)?;
        }

        self.scanned_chunks = end_chunk;
        if self.scanned_chunks == total_chunks {
            self.is_complete = true;
        }

        Ok(self.is_complete)
    }

    fn scan_chunk(&mut self, chunk: &SourceChunk, is_last_chunk: bool) -> Result<(), SourceError> {
        let bytes = chunk.bytes();
        let chunk_start_offset = chunk.range().start().get();
        let len = bytes.len();

        if len == 0 {
            return Ok(());
        }

        let mut idx = 0;

        // First line start at beginning of document
        if self.current_line == 0 && self.current_byte_offset == 0 {
            self.current_line = 1;
            self.line_starts.push(0);
            let line_one = LineNumber::new(1)?;
            self.checkpoints.push(LineCheckpoint {
                line: line_one,
                byte_offset: ByteOffset::new(0),
            });
        }

        // Handle split CRLF across chunk boundary
        if self.last_chunk_ended_with_cr {
            self.last_chunk_ended_with_cr = false;
            if bytes[0] == b'\n' {
                // The '\n' belongs to the CRLF that started at the end of the previous chunk.
                // Adjust the start of the current line to point after this '\n'.
                let new_line_start = chunk_start_offset + 1;
                if let Some(last_start) = self.line_starts.last_mut() {
                    *last_start = new_line_start;
                }
                if let Some(last_cp) = self.checkpoints.last_mut() {
                    if last_cp.line.get() == self.current_line {
                        last_cp.byte_offset = ByteOffset::new(new_line_start);
                    }
                }
                idx = 1;
            }
        }

        while idx < len {
            let byte = bytes[idx];

            if byte == b'\r' {
                if idx + 1 < len {
                    if bytes[idx + 1] == b'\n' {
                        // CRLF within chunk
                        idx += 2;
                        let next_line_start = chunk_start_offset + (idx as u64);
                        self.record_next_line(next_line_start, idx < len || !is_last_chunk)?;
                    } else {
                        // Bare CR within chunk
                        idx += 1;
                        let next_line_start = chunk_start_offset + (idx as u64);
                        self.record_next_line(next_line_start, idx < len || !is_last_chunk)?;
                    }
                } else {
                    // '\r' at very last byte of chunk
                    idx += 1;
                    let next_line_start = chunk_start_offset + (idx as u64);
                    if is_last_chunk {
                        // Terminal bare CR
                        self.record_next_line(next_line_start, false)?;
                    } else {
                        // Flag that chunk ended with CR; next chunk might start with '\n'
                        self.last_chunk_ended_with_cr = true;
                        self.record_next_line(next_line_start, true)?;
                    }
                }
            } else if byte == b'\n' {
                // Bare LF
                idx += 1;
                let next_line_start = chunk_start_offset + (idx as u64);
                self.record_next_line(next_line_start, idx < len || !is_last_chunk)?;
            } else {
                idx += 1;
            }
        }

        self.current_byte_offset = chunk_start_offset + (len as u64);
        Ok(())
    }

    fn record_next_line(&mut self, next_start: u64, has_more_bytes: bool) -> Result<(), SourceError> {
        // If this newline is at the very end of the file, we don't start a phantom empty line
        if !has_more_bytes {
            return Ok(());
        }

        self.current_line += 1;
        self.line_starts.push(next_start);

        if self.current_line % self.stride == 0 {
            let line_num = LineNumber::new(self.current_line)?;
            self.checkpoints.push(LineCheckpoint {
                line: line_num,
                byte_offset: ByteOffset::new(next_start),
            });
        }

        Ok(())
    }

    /// Locate the first screen's line range without scanning the full file.
    ///
    /// Scans only as many chunks as needed to index `screen_lines` lines.
    pub fn locate_first_screen(
        &mut self,
        capture: &ChunkedCapture,
        screen_lines: u32,
    ) -> Result<LineRangeOffsets, SourceError> {
        let target = screen_lines as u64;

        while self.current_line < target && !self.is_complete {
            self.step(capture, 1)?;
        }

        if self.current_line == 0 {
            return Err(SourceError::CaptureUnavailable);
        }

        let start_line = LineNumber::new(1)?;
        let end_line_val = self.current_line.min(target);
        let end_line = LineNumber::new(end_line_val)?;

        let start_offset = ByteOffset::new(0);
        let end_idx = (end_line_val - 1) as usize;
        let end_offset = if end_idx < self.line_starts.len() {
            ByteOffset::new(self.line_starts[end_idx])
        } else {
            ByteOffset::new(self.current_byte_offset)
        };

        Ok(LineRangeOffsets {
            start_line,
            end_line,
            start_offset,
            end_offset,
        })
    }

    /// Attempt to jump to an exact line number.
    ///
    /// If the line has already been indexed, returns `Resolved`. If the line lies
    /// beyond the current scan boundary, returns `PendingFarJump` without freezing.
    pub fn jump_to_line(
        &self,
        target: LineNumber,
    ) -> LineJumpResult {
        let target_val = target.get();
        if target_val <= self.current_line {
            let idx = (target_val - 1) as usize;
            if idx < self.line_starts.len() {
                return LineJumpResult::Resolved {
                    line: target,
                    offset: ByteOffset::new(self.line_starts[idx]),
                };
            }
        }

        LineJumpResult::PendingFarJump {
            indexed_through_line: self.current_line,
            requested_line: target,
        }
    }

    /// Advance scanning incrementally until the requested line is resolved or EOF is reached.
    pub fn resume_until_line(
        &mut self,
        capture: &ChunkedCapture,
        target: LineNumber,
        chunk_budget: usize,
    ) -> Result<LineJumpResult, SourceError> {
        let target_val = target.get();
        while self.current_line < target_val && !self.is_complete {
            self.step(capture, chunk_budget)?;
        }

        Ok(self.jump_to_line(target))
    }

    /// Scan all remaining chunks and produce the immutable SparseLineIndex.
    pub fn finish(&mut self, capture: &ChunkedCapture) -> Result<SparseLineIndex, SourceError> {
        while !self.is_complete {
            self.step(capture, 32)?;
        }

        Ok(SparseLineIndex {
            total_lines: self.current_line,
            total_bytes: capture.total_length().get(),
            stride: self.stride,
            checkpoints: self.checkpoints.clone(),
            line_starts: self.line_starts.clone(),
            is_complete: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use fcb_core::{ArenaOwnerId, ByteLength, ByteRange, FileId, SourceRevision};
    use crate::chunk::ChunkSize;
    use crate::CaptureRequest;

    fn owner(id: u64) -> ArenaOwnerId {
        ArenaOwnerId::new(id).unwrap()
    }

    fn file(owner_id: ArenaOwnerId, val: u64) -> FileId {
        FileId::new(owner_id, val).unwrap()
    }

    fn revision(owner_id: ArenaOwnerId, val: u64) -> SourceRevision {
        SourceRevision::new(owner_id, val).unwrap()
    }

    fn sample_capture(data: &[u8], chunk_size_bytes: usize) -> ChunkedCapture {
        let o = owner(1);
        let f = file(o, 1);
        let r = revision(o, 1);
        let req = CaptureRequest::new(f, r).unwrap();

        let chunk_size = ChunkSize::bounded(chunk_size_bytes).unwrap();
        let mut chunks = Vec::new();
        let mut offset = 0;
        let mut chunk_idx = 0;

        while offset < data.len() {
            let end = (offset + chunk_size_bytes).min(data.len());
            let slice = &data[offset..end];
            let range = ByteRange::new(
                ByteOffset::new(offset as u64),
                ByteOffset::new(end as u64),
            )
            .unwrap();
            let chunk = SourceChunk::new(chunk_idx, range, Arc::from(slice)).unwrap();
            chunks.push(chunk);
            chunk_idx += 1;
            offset = end;
        }

        ChunkedCapture::new(
            req,
            ByteLength::new(data.len() as u64),
            chunk_size,
            chunks,
        )
        .unwrap()
    }

    #[test]
    fn empty_file_line_indexing() {
        let cap = sample_capture(b"", 16);
        let mut scanner = ResumableLineScanner::new(4);
        let index = scanner.finish(&cap).unwrap();

        assert_eq!(index.total_lines(), 0);
        assert_eq!(index.total_bytes(), 0);
        assert!(index.is_complete());
    }

    #[test]
    fn single_line_without_newline() {
        let cap = sample_capture(b"hello world", 16);
        let mut scanner = ResumableLineScanner::new(4);
        let index = scanner.finish(&cap).unwrap();

        assert_eq!(index.total_lines(), 1);
        assert_eq!(index.line_to_byte(LineNumber::new(1).unwrap()).unwrap().get(), 0);
    }

    #[test]
    fn multiline_with_lf_and_crlf() {
        let text = b"line 1\nline 2\r\nline 3\n";
        let cap = sample_capture(text, 16);
        let mut scanner = ResumableLineScanner::new(2);
        let index = scanner.finish(&cap).unwrap();

        assert_eq!(index.total_lines(), 3);
        assert_eq!(index.line_to_byte(LineNumber::new(1).unwrap()).unwrap().get(), 0);
        assert_eq!(index.line_to_byte(LineNumber::new(2).unwrap()).unwrap().get(), 7);
        assert_eq!(index.line_to_byte(LineNumber::new(3).unwrap()).unwrap().get(), 15);

        assert_eq!(index.byte_to_line(ByteOffset::new(0)).unwrap().get(), 1);
        assert_eq!(index.byte_to_line(ByteOffset::new(5)).unwrap().get(), 1);
        assert_eq!(index.byte_to_line(ByteOffset::new(7)).unwrap().get(), 2);
        assert_eq!(index.byte_to_line(ByteOffset::new(15)).unwrap().get(), 3);
    }

    #[test]
    fn boundary_split_crlf_across_chunks() {
        // Chunk 0 ends with '\r' (offset 7)
        // Chunk 1 starts with '\n' (offset 8)
        let text = b"alpha 1\r\nbeta 2\n";
        // chunk size 8 puts '\r' at index 7 of chunk 0, '\n' at index 0 of chunk 1
        let cap = sample_capture(text, 8);
        assert_eq!(cap.chunk_count(), 2);

        let mut scanner = ResumableLineScanner::new(2);
        let index = scanner.finish(&cap).unwrap();

        assert_eq!(index.total_lines(), 2);
        assert_eq!(index.line_to_byte(LineNumber::new(1).unwrap()).unwrap().get(), 0);
        // Line 2 starts after '\r\n', at offset 9
        assert_eq!(index.line_to_byte(LineNumber::new(2).unwrap()).unwrap().get(), 9);
    }

    #[test]
    fn early_first_screen_without_full_indexing() {
        let mut text = Vec::new();
        for i in 1..=200 {
            text.extend_from_slice(format!("line {i}\n").as_bytes());
        }
        let cap = sample_capture(&text, 64);

        let mut scanner = ResumableLineScanner::new(10);
        // Request first 20 lines
        let screen = scanner.locate_first_screen(&cap, 20).unwrap();

        assert_eq!(screen.start_line.get(), 1);
        assert_eq!(screen.end_line.get(), 20);
        assert_eq!(screen.start_offset.get(), 0);
        // Not all chunks were scanned
        assert!(!scanner.is_complete());
        assert!(scanner.scanned_chunks() < cap.chunk_count());
    }

    #[test]
    fn pending_far_jump_and_resumption() {
        let mut text = Vec::new();
        for i in 1..=100 {
            text.extend_from_slice(format!("line {i}\n").as_bytes());
        }
        let cap = sample_capture(&text, 32);

        let mut scanner = ResumableLineScanner::new(5);
        // Index only first chunk
        scanner.step(&cap, 1).unwrap();

        let far_line = LineNumber::new(80).unwrap();
        let jump = scanner.jump_to_line(far_line);

        match jump {
            LineJumpResult::PendingFarJump { indexed_through_line, requested_line } => {
                assert_eq!(requested_line, far_line);
                assert!(indexed_through_line < 80);
            }
            _ => panic!("expected PendingFarJump"),
        }

        // Resume until line 80
        let resolved = scanner.resume_until_line(&cap, far_line, 10).unwrap();
        match resolved {
            LineJumpResult::Resolved { line, offset } => {
                assert_eq!(line, far_line);
                assert!(offset.get() > 0);
            }
            _ => panic!("expected Resolved"),
        }
    }
}

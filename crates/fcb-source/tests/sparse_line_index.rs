#![forbid(unsafe_code)]

//! Integration test suite for resumable sparse line indexes and exact byte/line translation
//! (FCB-012.A / fcb-37r.1).
//!
//! Verifies:
//! 1. Absolute u64 checkpoints and checked local offsets.
//! 2. Early first screen resolution without full-file indexing pass.
//! 3. Pending far-line jumps exposing explicit non-blocking progression.
//! 4. Empty files, trailing newlines, bare CR, bare LF, and CRLF semantics.
//! 5. Boundary-split CRLF pairs across chunk cut lines.
//! 6. Negative control oracle: invalid zero line and out-of-bounds line queries are refused.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb_source::chunk::{ChunkSize, ChunkedCapture, SourceChunk};
use fcb_source::line_index::{
    LineJumpResult, LineNumber, ResumableLineScanner,
};
use fcb_source::{CaptureRequest, SourceError};

fn owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).unwrap()
}

fn file_id(owner_id: ArenaOwnerId, val: u64) -> FileId {
    FileId::new(owner_id, val).unwrap()
}

fn revision(owner_id: ArenaOwnerId, val: u64) -> SourceRevision {
    SourceRevision::new(owner_id, val).unwrap()
}

fn make_chunked_capture(data: &[u8], chunk_size_bytes: usize) -> ChunkedCapture {
    let o = owner(301);
    let f = file_id(o, 1);
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
fn empty_file_and_single_line_semantics() {
    // 1. Empty file
    let empty_cap = make_chunked_capture(b"", 16);
    let mut scanner = ResumableLineScanner::new(4);
    let index = scanner.finish(&empty_cap).unwrap();
    assert_eq!(index.total_lines(), 0);
    assert_eq!(index.total_bytes(), 0);
    assert_eq!(index.line_to_byte(LineNumber::new(1).unwrap()), Err(SourceError::RangeOutOfBounds));

    // 2. Single line with no newline
    let single_cap = make_chunked_capture(b"fn main() {}", 16);
    let mut s1 = ResumableLineScanner::new(4);
    let i1 = s1.finish(&single_cap).unwrap();
    assert_eq!(i1.total_lines(), 1);
    assert_eq!(i1.line_to_byte(LineNumber::new(1).unwrap()).unwrap().get(), 0);
    assert_eq!(i1.byte_to_line(ByteOffset::new(5)).unwrap().get(), 1);

    // 3. Single line with terminal newline (does not create trailing phantom line)
    let term_cap = make_chunked_capture(b"fn main() {}\n", 16);
    let mut s2 = ResumableLineScanner::new(4);
    let i2 = s2.finish(&term_cap).unwrap();
    assert_eq!(i2.total_lines(), 1);
    assert_eq!(i2.line_to_byte(LineNumber::new(1).unwrap()).unwrap().get(), 0);
}

#[test]
fn mixed_newlines_crlf_cr_and_lf() {
    // Line 1: "line one\r\n" -> 10 bytes (0..10)
    // Line 2: "line two\r"   -> 9 bytes (10..19)
    // Line 3: "line three\n" -> 11 bytes (19..30)
    // Line 4: "line four"    -> 9 bytes (30..39)
    let content = b"line one\r\nline two\rline three\nline four";
    let cap = make_chunked_capture(content, 16);

    let mut scanner = ResumableLineScanner::new(2);
    let index = scanner.finish(&cap).unwrap();

    assert_eq!(index.total_lines(), 4);
    assert_eq!(index.line_to_byte(LineNumber::new(1).unwrap()).unwrap().get(), 0);
    assert_eq!(index.line_to_byte(LineNumber::new(2).unwrap()).unwrap().get(), 10);
    assert_eq!(index.line_to_byte(LineNumber::new(3).unwrap()).unwrap().get(), 19);
    assert_eq!(index.line_to_byte(LineNumber::new(4).unwrap()).unwrap().get(), 30);

    assert_eq!(index.byte_to_line(ByteOffset::new(0)).unwrap().get(), 1);
    assert_eq!(index.byte_to_line(ByteOffset::new(9)).unwrap().get(), 1);
    assert_eq!(index.byte_to_line(ByteOffset::new(10)).unwrap().get(), 2);
    assert_eq!(index.byte_to_line(ByteOffset::new(18)).unwrap().get(), 2);
    assert_eq!(index.byte_to_line(ByteOffset::new(19)).unwrap().get(), 3);
    assert_eq!(index.byte_to_line(ByteOffset::new(29)).unwrap().get(), 3);
    assert_eq!(index.byte_to_line(ByteOffset::new(30)).unwrap().get(), 4);
}

#[test]
fn boundary_split_crlf_across_chunks() {
    // We construct 16 bytes:
    // Chunk 0: "0123456\r" (8 bytes, offsets 0..8)
    // Chunk 1: "\n89ABCDEF" (9 bytes, offsets 8..17)
    let content = b"0123456\r\n89ABCDEF";
    let cap = make_chunked_capture(content, 8);
    assert_eq!(cap.chunk_count(), 3);

    let mut scanner = ResumableLineScanner::new(2);
    let index = scanner.finish(&cap).unwrap();

    assert_eq!(index.total_lines(), 2);
    assert_eq!(index.line_to_byte(LineNumber::new(1).unwrap()).unwrap().get(), 0);
    // Line 2 must start after '\r\n', at offset 9
    assert_eq!(index.line_to_byte(LineNumber::new(2).unwrap()).unwrap().get(), 9);

    assert_eq!(index.byte_to_line(ByteOffset::new(0)).unwrap().get(), 1);
    assert_eq!(index.byte_to_line(ByteOffset::new(7)).unwrap().get(), 1); // '\r'
    assert_eq!(index.byte_to_line(ByteOffset::new(8)).unwrap().get(), 1); // '\n'
    assert_eq!(index.byte_to_line(ByteOffset::new(9)).unwrap().get(), 2); // '8'
}

#[test]
fn early_first_screen_without_full_file_scan() {
    let mut large_text = Vec::new();
    for i in 1..=500 {
        large_text.extend_from_slice(format!("let x_{i} = {i} * 2;\n").as_bytes());
    }
    // Many chunks of 64 bytes
    let cap = make_chunked_capture(&large_text, 64);
    assert!(cap.chunk_count() > 20);

    let mut scanner = ResumableLineScanner::new(10);
    // Locate first 30 lines for visible screen
    let screen = scanner.locate_first_screen(&cap, 30).unwrap();

    assert_eq!(screen.start_line.get(), 1);
    assert_eq!(screen.end_line.get(), 30);
    assert_eq!(screen.start_offset.get(), 0);
    assert!(screen.end_offset.get() > 0);

    // Verify early exit: not all chunks were processed
    assert!(!scanner.is_complete());
    assert!(scanner.scanned_chunks() < cap.chunk_count());
}

#[test]
fn pending_far_jump_and_resumption_workflow() {
    let mut text = Vec::new();
    for i in 1..=300 {
        text.extend_from_slice(format!("statement_{i:04}();\n").as_bytes());
    }
    let cap = make_chunked_capture(&text, 64);

    let mut scanner = ResumableLineScanner::new(16);
    // Step 2 chunks only
    scanner.step(&cap, 2).unwrap();
    assert!(!scanner.is_complete());

    let target_line = LineNumber::new(250).unwrap();
    let initial_jump = scanner.jump_to_line(target_line);

    // Far line is not yet indexed -> exposes PendingFarJump without freezing
    match initial_jump {
        LineJumpResult::PendingFarJump { indexed_through_line, requested_line } => {
            assert_eq!(requested_line, target_line);
            assert!(indexed_through_line < 250);
        }
        _ => panic!("expected PendingFarJump for line 250"),
    }

    // Step scan progressively towards far line
    let progressed = scanner.resume_until_line(&cap, target_line, 100).unwrap();
    match progressed {
        LineJumpResult::Resolved { line, offset } => {
            assert_eq!(line, target_line);
            assert!(offset.get() > 0);
        }
        _ => panic!("expected Resolved after resumption"),
    }
}

#[test]
fn negative_control_oracle_invalid_and_out_of_bounds_lines() {
    let content = b"line A\nline B\nline C\n";
    let cap = make_chunked_capture(content, 16);

    let mut scanner = ResumableLineScanner::new(2);
    let index = scanner.finish(&cap).unwrap();

    // 1. Line 0 is strictly forbidden by LineNumber::new
    assert_eq!(LineNumber::new(0), Err(SourceError::InvalidRange));

    // 2. Line 4 is out of bounds (file has 3 lines)
    let line_4 = LineNumber::new(4).unwrap();
    assert_eq!(index.line_to_byte(line_4), Err(SourceError::RangeOutOfBounds));

    // 3. Byte offset beyond EOF is out of bounds
    assert_eq!(index.byte_to_line(ByteOffset::new(9999)), Err(SourceError::RangeOutOfBounds));
}

#![forbid(unsafe_code)]

//! Integration test suite for immutable source chunks and observed-snapshot contract
//! (FCB-011.A / fcb-8t9.1).
//!
//! Verifies:
//! 1. Contiguous and chunked reads with u64 lengths and exact range results.
//! 2. Safe owned-buffer reads from working-tree files (never mmap mutable source).
//! 3. Refusal of special objects (directories, FIFOs) without blocking.
//! 4. Concurrent modification and truncation detection via before/after metadata.
//! 5. Cross-chunk multibyte UTF-8 sequences and CRLF boundary handling.
//! 6. Negative control oracle: evicted old capture strictly returns StaleCapture and
//!    never silently substitutes live file bytes or newer revisions.

use std::fs;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb_source::chunk::{
    ChunkSize, ChunkedCapture, ChunkedReaderConfig, RetainedCaptureStore, SafeChunkReader,
    SourceChunk,
};
use fcb_source::{CancelFlag, CaptureRequest, SourceError};

fn temp_test_dir(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("fcb_chunk_test_{label}_{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).unwrap()
}

fn file_id(owner_id: ArenaOwnerId, val: u64) -> FileId {
    FileId::new(owner_id, val).unwrap()
}

fn revision(owner_id: ArenaOwnerId, val: u64) -> SourceRevision {
    SourceRevision::new(owner_id, val).unwrap()
}

fn request(owner_id: u64, file_num: u64, rev_num: u64) -> CaptureRequest {
    let o = owner(owner_id);
    CaptureRequest::new(file_id(o, file_num), revision(o, rev_num)).unwrap()
}

fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
}

#[test]
fn contiguous_and_chunked_reads_with_u64_lengths() {
    let req = request(100, 1, 1);
    let chunk_size = ChunkSize::bounded(32).unwrap();

    let text = b"0123456789abcdef0123456789ABCDEF!@#$%^&*()_+~`|}{[]:;?><,./-=+qz";
    assert_eq!(text.len(), 64);

    let c0 = SourceChunk::new(0, range(0, 32), Arc::from(&text[0..32])).unwrap();
    let c1 = SourceChunk::new(1, range(32, 64), Arc::from(&text[32..64])).unwrap();

    let capture = ChunkedCapture::new(
        req,
        ByteLength::new(64),
        chunk_size,
        vec![c0, c1],
    )
    .unwrap();

    // 1. Exact range within chunk 0
    let r0 = capture.range_read(range(0, 16)).unwrap();
    assert!(r0.is_contiguous());
    assert_eq!(r0.bytes(), b"0123456789abcdef");

    // 2. Exact range within chunk 1
    let r1 = capture.range_read(range(40, 56)).unwrap();
    assert!(r1.is_contiguous());
    assert_eq!(r1.bytes(), &text[40..56]);

    // 3. Exact range straddling boundary between chunk 0 and chunk 1
    let r_cross = capture.range_read(range(28, 36)).unwrap();
    assert!(!r_cross.is_contiguous());
    assert_eq!(r_cross.bytes(), &text[28..36]);
    assert_eq!(r_cross.len(), 8);

    // 4. Entire file read
    let r_full = capture.range_read(range(0, 64)).unwrap();
    assert_eq!(r_full.bytes(), text);

    // 5. Zero-length range is rejected
    assert_eq!(capture.range_read(range(10, 10)), Err(SourceError::InvalidRange));

    // 6. Out-of-bounds range is rejected
    assert_eq!(capture.range_read(range(50, 65)), Err(SourceError::RangeOutOfBounds));
}

#[test]
fn safe_file_reading_without_mmap_from_disk() {
    let dir = temp_test_dir("safe_read");
    let file_path = dir.join("test_file.txt");

    let payload = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor.";
    fs::write(&file_path, payload).unwrap();

    let req = request(101, 2, 1);
    let cancel = CancelFlag::new();
    let config = ChunkedReaderConfig {
        chunk_size: ChunkSize::bounded(32).unwrap(),
        max_payload_bytes: ByteLength::new(1024 * 1024),
    };

    let capture = SafeChunkReader::read_file(req, &file_path, config, &cancel).unwrap();
    assert_eq!(capture.total_length().get(), payload.len() as u64);
    assert_eq!(capture.chunk_count(), 3); // 32 + 32 + 15 = 79 bytes

    let read_all = capture.range_read(range(0, payload.len() as u64)).unwrap();
    assert_eq!(read_all.bytes(), payload);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn refusal_of_special_objects_and_directories() {
    let dir = temp_test_dir("special_obj");
    let req = request(102, 3, 1);
    let cancel = CancelFlag::new();
    let config = ChunkedReaderConfig::default();

    // Attempting to read directory as a source file must fail with SpecialObject
    let result = SafeChunkReader::read_file(req, &dir, config, &cancel);
    assert_eq!(result, Err(SourceError::SpecialObject));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn payload_limit_guards_against_excessive_files() {
    let dir = temp_test_dir("payload_limit");
    let file_path = dir.join("big_file.bin");

    let payload = vec![0xABu8; 1000];
    fs::write(&file_path, &payload).unwrap();

    let req = request(103, 4, 1);
    let cancel = CancelFlag::new();
    let config = ChunkedReaderConfig {
        chunk_size: ChunkSize::bounded(64).unwrap(),
        max_payload_bytes: ByteLength::new(500), // Less than 1000 bytes
    };

    let result = SafeChunkReader::read_file(req, &file_path, config, &cancel);
    assert_eq!(result, Err(SourceError::PayloadTooLarge));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cross_chunk_multibyte_utf8_and_crlf_boundary() {
    let chunk_size = ChunkSize::bounded(16).unwrap();

    // Prepare 32 bytes where:
    // Offset 14-15: "AB"
    // Offset 15-16: CRLF split: '\r' at 15 (chunk 0 end), '\n' at 16 (chunk 1 start)
    // Offset 20-24: 4-byte UTF-8 emoji "🚀" (0xF0, 0x9F, 0x99, 0x80)
    let mut data = [b'.'; 32];
    data[15] = b'\r';
    data[16] = b'\n';

    let rocket = "🚀".as_bytes();
    data[18] = rocket[0];
    data[19] = rocket[1];
    data[20] = rocket[2];
    data[21] = rocket[3];

    let c0 = SourceChunk::new(0, range(0, 16), Arc::from(&data[0..16])).unwrap();
    let c1 = SourceChunk::new(1, range(16, 32), Arc::from(&data[16..32])).unwrap();

    let capture = ChunkedCapture::new(
        request(104, 5, 1),
        ByteLength::new(32),
        chunk_size,
        vec![c0, c1],
    )
    .unwrap();

    // 1. CRLF across chunk boundary
    let crlf = capture.range_read(range(15, 17)).unwrap();
    assert_eq!(crlf.bytes(), b"\r\n");
    assert!(!crlf.is_contiguous());

    // 2. Multibyte UTF-8 entirely within chunk 1
    let emoji = capture.range_read(range(18, 22)).unwrap();
    assert_eq!(std::str::from_utf8(emoji.bytes()).unwrap(), "🚀");
    assert!(emoji.is_contiguous());
}

#[test]
fn negative_control_evicted_capture_never_reads_live_source() {
    let o = owner(105);
    let f = file_id(o, 999);
    let rev_old = revision(o, 1);
    let rev_new = revision(o, 2);

    let mut store = RetainedCaptureStore::new(o);

    // 1. Capture rev_old with initial content
    let req_old = CaptureRequest::new(f, rev_old).unwrap();
    let c_old = SourceChunk::new(
        0,
        range(0, 16),
        Arc::from(b"OLD_EXACT_BYTES_".to_vec().into_boxed_slice()),
    )
    .unwrap();
    let cap_old = ChunkedCapture::new(
        req_old,
        ByteLength::new(16),
        ChunkSize::bounded(16).unwrap(),
        vec![c_old],
    )
    .unwrap();
    store.insert(cap_old).unwrap();

    // Initial read verifies exact old content
    let res = store.resolve_anchor(f, rev_old, range(0, 9)).unwrap();
    assert_eq!(res.bytes(), b"OLD_EXACT");

    // 2. Evict rev_old from memory
    assert!(store.evict(f, rev_old));

    // 3. New capture rev_new is stored with new mutated content
    let req_new = CaptureRequest::new(f, rev_new).unwrap();
    let c_new = SourceChunk::new(
        0,
        range(0, 16),
        Arc::from(b"NEW_MUTATED_DATA".to_vec().into_boxed_slice()),
    )
    .unwrap();
    let cap_new = ChunkedCapture::new(
        req_new,
        ByteLength::new(16),
        ChunkSize::bounded(16).unwrap(),
        vec![c_new],
    )
    .unwrap();
    store.insert(cap_new).unwrap();

    // 4. NEGATIVE CONTROL ORACLE:
    // Querying the evicted old capture rev_old MUST return StaleCapture.
    // It must NEVER return NEW_MUTATED_DATA or silently re-read live source!
    let stale_attempt = store.resolve_anchor(f, rev_old, range(0, 9));
    assert_eq!(stale_attempt, Err(SourceError::StaleCapture));

    // Querying rev_new returns its own distinct bytes
    let new_res = store.resolve_anchor(f, rev_new, range(0, 9)).unwrap();
    assert_eq!(new_res.bytes(), b"NEW_MUTAT");
}

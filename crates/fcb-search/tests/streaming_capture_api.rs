#![forbid(unsafe_code)]

//! Public API regression scenarios: captured source -> scan -> exact navigation
//! ranges. No filesystem, runtime, native bridge, or external search tool.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, QueryGeneration, SourceRevision};
use fcb_search::{DirectSourceScanner, QueryError, QueryOptions, SearchCoverage, SearchMode};
use fcb_source::{CaptureRequest, ChunkSize, ChunkedCapture, CompleteCapture, DetectedEncoding, SourceChunk};

fn fixtures(bytes: &[u8], chunk_bytes: usize) -> (CompleteCapture, ChunkedCapture, QueryOptions) {
    let owner = ArenaOwnerId::new(41).unwrap();
    let file = FileId::new(owner, 7).unwrap();
    let revision = SourceRevision::new(owner, 11).unwrap();
    let complete = CompleteCapture::new(CaptureRequest::new(file, revision).unwrap(),
        ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap();
    let chunks = bytes.chunks(chunk_bytes).enumerate().map(|(index, part)| {
        let start = index * chunk_bytes;
        let range = ByteRange::new(ByteOffset::new(start as u64),
            ByteOffset::new((start + part.len()) as u64)).unwrap();
        SourceChunk::new(index as u32, range, Arc::from(part)).unwrap()
    }).collect();
    let chunked = ChunkedCapture::new(CaptureRequest::new(file, revision).unwrap(),
        ByteLength::new(bytes.len() as u64), ChunkSize::bounded(chunk_bytes).unwrap(), chunks).unwrap();
    (complete, chunked, QueryOptions::new(QueryGeneration::new(owner, 5).unwrap()))
}

#[test]
fn complete_and_chunked_routes_agree_for_overlaps_budgets_and_limits() {
    for mode in [SearchMode::RawBytes, SearchMode::default()] {
        for chunk in [1, 2, 3, 5, 64] {
            let (complete, chunked, base) = fixtures(b"abababababa", chunk);
            for budget in 0..=12 {
                for limit in [0, 1, 2, 4, 10] {
                    let mut options = base.clone().with_mode(mode).with_max_matches(limit);
                    options.max_bytes_scanned = Some(budget);
                    let contiguous = DirectSourceScanner::scan_complete_capture(&complete, "ababa", &options).unwrap();
                    let fragmented = DirectSourceScanner::scan_chunked_capture(&chunked, "ababa", &options).unwrap();
                    assert_eq!(contiguous, fragmented, "chunk={chunk}, budget={budget}, limit={limit}");
                    assert!(contiguous.scanned_bytes <= budget);
                    assert!(contiguous.matches.len() <= limit);
                    if contiguous.is_complete() {
                        assert_eq!(contiguous.match_count(), 4);
                    }
                }
            }
        }
    }
}

#[test]
fn text_search_keeps_original_utf8_and_utf16_ranges_under_every_fragmentation() {
    let text = "q🙂xq🙂x";
    let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
    utf8_bom.extend_from_slice(text.as_bytes());
    let mut utf16_le = vec![0xFF, 0xFE];
    let mut utf16_be = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        utf16_le.extend_from_slice(&unit.to_le_bytes());
        utf16_be.extend_from_slice(&unit.to_be_bytes());
    }
    for (bytes, expected) in [
        (text.as_bytes().to_vec(), [(1, 6), (7, 12)]),
        (utf8_bom, [(4, 9), (10, 15)]),
        (utf16_le, [(4, 10), (12, 18)]),
        (utf16_be, [(4, 10), (12, 18)]),
    ] {
        for chunk in 1..=bytes.len() {
            let (complete, chunked, options) = fixtures(&bytes, chunk);
            let result = DirectSourceScanner::scan_chunked_capture(&chunked, "🙂x", &options).unwrap();
            assert!(result.is_complete());
            assert_eq!(result.matches.len(), 2);
            assert_eq!(result, DirectSourceScanner::scan_complete_capture(&complete, "🙂x", &options).unwrap());
            for (index, (hit, &(start, end))) in result.matches.iter().zip(&expected).enumerate() {
                assert_eq!(hit.original_byte_range.start().get(), start);
                assert_eq!(hit.original_byte_range.end().get(), end);
                assert_eq!(hit.revision, complete.request().revision());
                assert_eq!(hit.file_id, complete.request().file());
                assert_eq!(hit.occurrence_id, index as u64 + 1);
                assert_eq!(hit.matched_text, "🙂x");
            }
        }
    }
}

#[test]
fn declared_utf16_without_bom_is_honored_and_raw_bytes_remain_a_distinct_mode() {
    let raw: Vec<u8> = "hello".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let (complete, chunked, options) = fixtures(&raw, 1);
    let options = options.with_encoding(DetectedEncoding::Utf16Le);
    let text = DirectSourceScanner::scan_chunked_capture(&chunked, "hello", &options).unwrap();
    assert_eq!(text.match_count(), 1);
    assert_eq!(text.matches[0].original_byte_range.start().get(), 0);
    assert_eq!(text.matches[0].original_byte_range.end().get(), 10);
    assert_eq!(text, DirectSourceScanner::scan_complete_capture(&complete, "hello", &options).unwrap());
    let raw_result = DirectSourceScanner::scan_chunked_capture(&chunked, "hello",
        &options.with_mode(SearchMode::RawBytes)).unwrap();
    assert!(raw_result.is_complete());
    assert_eq!(raw_result.match_count(), 0);
}

#[test]
fn disabling_cross_chunk_matching_does_not_disable_in_chunk_overlap() {
    let (_, chunked, mut options) = fixtures(b"aaaaaa", 3);
    options.cross_chunk = false;
    for mode in [SearchMode::RawBytes, SearchMode::default()] {
        options.mode = mode;
        let result = DirectSourceScanner::scan_chunked_capture(&chunked, "aa", &options).unwrap();
        assert!(result.is_complete());
        assert_eq!(result.matches.iter().map(|hit| hit.original_byte_range.start().get()).collect::<Vec<_>>(), [0, 1, 3, 4]);
    }
}

#[test]
fn cancellation_and_query_admission_use_the_public_capture_api() {
    let (complete, chunked, options) = fixtures(b"abcabc", 1);
    let result = DirectSourceScanner::scan_complete_capture_with_cancel(&complete, "abc", &options, || true).unwrap();
    assert_eq!(result.coverage, SearchCoverage::CanceledEarly);
    assert_eq!(result.scanned_bytes, 0);
    assert_eq!(result, DirectSourceScanner::scan_chunked_capture_with_cancel(&chunked, "abc", &options, || true).unwrap());
    assert_eq!(DirectSourceScanner::scan_chunked_capture(&chunked, "", &options), Err(QueryError::EmptyNeedle));
    let oversized = "a".repeat(fcb_search::stream::MAX_STREAM_NEEDLE_BYTES + 1);
    assert_eq!(DirectSourceScanner::scan_complete_capture(&complete, &oversized, &options), Err(QueryError::NeedleTooLong));
}

#[test]
fn invalid_text_never_claims_complete_but_explicit_byte_search_still_works() {
    let (complete, chunked, options) = fixtures(b"abc\xFFabc", 1);
    let result = DirectSourceScanner::scan_chunked_capture(&chunked, "missing", &options).unwrap();
    assert!(!result.is_complete());
    assert_eq!(result.unsupported_files, [complete.request().file()]);
    let result = DirectSourceScanner::scan_raw_bytes(complete.bytes(), complete.request().file(),
        complete.request().revision(), &[0xFF], &options).unwrap();
    assert!(result.is_complete());
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].original_byte_range.start().get(), 3);
}

#![forbid(unsafe_code)]

//! Public capture-route regressions for bounded normalized search (fcb-8ii.1).

use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, QueryGeneration, SourceRevision};
use fcb_search::{DirectSourceScanner, QueryOptions, SearchCoverage, SearchMode, UnicodeNormalization};
use fcb_source::{CaptureRequest, ChunkSize, ChunkedCapture, CompleteCapture, SourceChunk};

fn fixtures(bytes: &[u8], chunk_bytes: usize) -> (CompleteCapture, ChunkedCapture, QueryOptions) {
    let owner = ArenaOwnerId::new(61).unwrap();
    let file = FileId::new(owner, 7).unwrap();
    let revision = SourceRevision::new(owner, 11).unwrap();
    let request = CaptureRequest::new(file, revision).unwrap();
    let complete = CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap();
    let chunks = bytes.chunks(chunk_bytes).enumerate().map(|(index, part)| {
        let start = index * chunk_bytes;
        let range = ByteRange::new(ByteOffset::new(start as u64),
            ByteOffset::new((start + part.len()) as u64)).unwrap();
        SourceChunk::new(index as u32, range, Arc::from(part)).unwrap()
    }).collect();
    let chunked = ChunkedCapture::new(CaptureRequest::new(file, revision).unwrap(),
        ByteLength::new(bytes.len() as u64), ChunkSize::bounded(chunk_bytes).unwrap(), chunks).unwrap();
    let options = QueryOptions::new(QueryGeneration::new(owner, 5).unwrap()).with_mode(
        SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::CaseFold });
    (complete, chunked, options)
}

fn encodings(text: &str) -> Vec<Vec<u8>> {
    let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
    utf8_bom.extend_from_slice(text.as_bytes());
    let mut little = vec![0xFF, 0xFE];
    let mut big = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        little.extend_from_slice(&unit.to_le_bytes());
        big.extend_from_slice(&unit.to_be_bytes());
    }
    vec![text.as_bytes().to_vec(), utf8_bom, little, big]
}

#[test]
fn normalized_complete_and_chunked_routes_agree_under_budgets_and_caps() {
    for bytes in encodings("ßßS Ée\u{0301} Kİ🙂") {
        for chunk in [1, 2, 3, 5, 64] {
            let (complete, chunked, base) = fixtures(&bytes, chunk);
            for (normalization, query) in [(UnicodeNormalization::CaseFold, "s"),
                (UnicodeNormalization::Canonical, "é"), (UnicodeNormalization::Exact, "i\u{0307}")] {
                for budget in 0..=bytes.len() as u64 + 1 {
                    for limit in [0, 1, 2, 8] {
                        let mut options = base.clone().with_mode(SearchMode::DecodedText {
                            case_sensitive: false, normalization,
                        }).with_max_matches(limit);
                        options.max_bytes_scanned = Some(budget);
                        let contiguous = DirectSourceScanner::scan_complete_capture(&complete, query, &options).unwrap();
                        let fragmented = DirectSourceScanner::scan_chunked_capture(&chunked, query, &options).unwrap();
                        assert_eq!(contiguous, fragmented, "{normalization:?}: chunk={chunk}, budget={budget}, cap={limit}");
                        assert!(contiguous.scanned_bytes <= budget);
                        assert!(contiguous.matches.len() <= limit);
                        for hit in &contiguous.matches {
                            assert_eq!(hit.file_id, complete.request().file());
                            assert_eq!(hit.revision, complete.request().revision());
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn cross_chunk_candidates_do_not_consume_the_eligible_hit_limit() {
    let (_, chunked, mut options) = fixtures(b"xABABAAB", 2);
    options.cross_chunk = false;
    options.max_matches = 1;
    let result = DirectSourceScanner::scan_chunked_capture(&chunked, "ab", &options).unwrap();
    assert!(result.is_complete());
    assert_eq!(result.match_count(), 1);
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].original_byte_range.start().get(), 6);
    assert_eq!(result.matches[0].original_byte_range.end().get(), 8);
    assert_eq!(result.matches[0].matched_text, "AB");
    assert_eq!(result.matches[0].occurrence_id, 1);
}

#[test]
fn normalized_search_passes_the_former_whole_capture_ceiling() {
    let prefix = "x".repeat(fcb_search::MAX_NORMALIZED_SCAN_BYTES + 17);
    let source = format!("{prefix}Straße");
    let (complete, chunked, options) = fixtures(source.as_bytes(), 4096);
    let result = DirectSourceScanner::scan_complete_capture(&complete, "STRASSE", &options).unwrap();
    assert!(result.is_complete());
    assert_eq!(result.match_count(), 1);
    assert_eq!(result.scanned_bytes, source.len() as u64);
    assert_eq!(result.matches[0].original_byte_range.start().get(), prefix.len() as u64);
    assert_eq!(result.matches[0].original_byte_range.end().get(), source.len() as u64);
    assert_eq!(result.matches[0].matched_text, "Straße");
    assert_eq!(result, DirectSourceScanner::scan_chunked_capture(&chunked, "STRASSE", &options).unwrap());
}

#[test]
fn partial_normalized_budget_keeps_hits_without_inventing_malformed_text() {
    for (bytes, budget, expected_range) in [
        ("ß🙂tail".as_bytes().to_vec(), 4, (0, 2)),
        (vec![0xFF, 0xFE, 0xDF, 0, 0x3D, 0xD8, 0x42, 0xDE], 6, (2, 4)),
    ] {
        let (complete, _, mut options) = fixtures(&bytes, 1);
        options.max_bytes_scanned = Some(budget);
        let result = DirectSourceScanner::scan_complete_capture(&complete, "s", &options).unwrap();
        assert_eq!(result.coverage, SearchCoverage::BudgetExhausted { bytes_scanned: budget });
        assert!(result.unsupported_files.is_empty());
        assert_eq!(result.match_count(), 2);
        assert_ne!(result.matches[0].occurrence_id, result.matches[1].occurrence_id);
        for hit in result.matches {
            assert_eq!(hit.matched_text, "ß");
            assert_eq!((hit.original_byte_range.start().get(), hit.original_byte_range.end().get()), expected_range);
        }
    }
}

#[test]
fn normalized_work_observes_cancellation_before_consuming_the_source() {
    let source = "x".repeat(1024 * 1024);
    let (complete, _, options) = fixtures(source.as_bytes(), 4096);
    let mut checks = 0;
    let result = DirectSourceScanner::scan_complete_capture_with_cancel(&complete, "absent", &options, || {
        checks += 1;
        checks >= 8
    }).unwrap();
    assert_eq!(result.coverage, SearchCoverage::CanceledEarly);
    assert!(result.scanned_bytes > 0);
    assert!(result.scanned_bytes < source.len() as u64);
    assert!(!result.is_complete());
}

#[test]
fn normalized_matches_cross_decoder_windows_but_not_bom_headers() {
    let text = format!("{}Straße🙂\u{FEFF}É", "x".repeat(16_381));
    for bytes in encodings(&text) {
        let (complete, chunked, options) = fixtures(&bytes, 3);
        let query = "STRASSE🙂\u{FEFF}é";
        let result = DirectSourceScanner::scan_complete_capture(&complete, query, &options).unwrap();
        assert!(result.is_complete());
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].matched_text, "Straße🙂\u{FEFF}É");
        assert_eq!(result.matches[0].decoded_range.unwrap().start().get(), 16_381);
        assert_eq!(result.matches[0].original_byte_range.end().get(), bytes.len() as u64);
        assert_eq!(result, DirectSourceScanner::scan_chunked_capture(&chunked, query, &options).unwrap());
    }
}

#[test]
fn malformed_suffix_cannot_become_a_complete_normalized_result() {
    let (complete, chunked, options) = fixtures(b"ABC\xFF", 1);
    for result in [DirectSourceScanner::scan_complete_capture(&complete, "abc", &options).unwrap(),
        DirectSourceScanner::scan_chunked_capture(&chunked, "abc", &options).unwrap()] {
        assert!(!result.is_complete());
        assert_eq!(result.unsupported_files, [complete.request().file()]);
        assert!(result.matches.is_empty());
        assert_eq!(result.match_count(), 0);
    }
}

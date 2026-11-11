#![forbid(unsafe_code)]
#![cfg(feature = "search")]

use std::{io::{self, Read}, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{CaptureRequest, CompleteCapture, DirectSourceScanner, QueryGeneration,
    QueryOptions, ReaderSearch, ResourceAllocationId, ResourceBudget, SearchMode,
    StreamReadOptions, StreamReadState, StreamReadStep, StreamingNeedle, UnicodeNormalization};
use fcb::search::streaming::STREAM_BUFFER_BYTES;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1245).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}
struct Chunks<'a> { remaining: &'a [u8], chunk: usize }
impl Read for Chunks<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = output.len().min(self.chunk).min(self.remaining.len());
        output[..count].copy_from_slice(&self.remaining[..count]);
        self.remaining = &self.remaining[count..];
        Ok(count)
    }
}
fn finish<R: Read>(query: &mut ReaderSearch<'_, R>, quantum: usize) {
    for _ in 0..1_000_000 {
        if query.state() != StreamReadState::Pending { return; }
        let before = query.matches_seen();
        query.step(StreamReadStep { max_bytes: quantum, max_hits: 1, max_calls: 1 }, generation(), || false).unwrap();
        assert!(query.stats().last_step_read <= quantum.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_scanned <= quantum.min(STREAM_BUFFER_BYTES));
        assert!(query.matches_seen() <= before + 1);
        assert!(query.stats().peak_buffer_bytes <= STREAM_BUFFER_BYTES);
    }
    panic!("lowercase streaming search did not terminate");
}

#[test]
fn lowercase_streaming_agrees_with_capture_oracle_at_scalar_and_surrogate_boundaries() {
    let text = "BANANA banana Kk K İi\u{0307} 😀CAFÉ café\r\n\u{feff}Straße STRASSE";
    let mut bom = vec![0xef, 0xbb, 0xbf]; bom.extend_from_slice(text.as_bytes());
    for bytes in [text.as_bytes().to_vec(), bom, utf16(text, true), utf16(text, false)] {
        for needle in ["ana", "k", "i", "\u{0307}", "i\u{0307}", "café", "😀c", "strasse", "\u{feff}", "missing"] {
            let budget = budget();
            let pattern = StreamingNeedle::text_matching(owner(), needle, false, &budget, allocation(1)).unwrap();
            let capture = CompleteCapture::new(request(), ByteLength::new(bytes.len() as u64), Arc::from(bytes.as_slice())).unwrap();
            let options = QueryOptions::new(generation()).with_mode(SearchMode::DecodedText {
                case_sensitive: false, normalization: UnicodeNormalization::Exact,
            });
            let oracle = DirectSourceScanner::scan_complete_capture(&capture, needle, &options).unwrap();
            for chunk in [1, 2, 3, 7, STREAM_BUFFER_BYTES] {
                for quantum in [1, 3, STREAM_BUFFER_BYTES] {
                    for retained in [0, 1, 16] {
                        let mut options = StreamReadOptions::new(generation()); options.max_matches = retained;
                        let reader = Chunks { remaining: &bytes, chunk };
                        let mut query = ReaderSearch::new_counting(reader, request(), ByteLength::new(bytes.len() as u64),
                            &pattern, options, &budget, allocation(2)).unwrap();
                        finish(&mut query, quantum);
                        let (_, report) = query.finish().unwrap();
                        assert!(report.input_complete(), "{needle:?}, chunk {chunk}, quantum {quantum}: {:?}", report.state());
                        assert_eq!(report.matches_seen(), oracle.matches.len() as u64);
                        assert_eq!(report.hits().len(), retained.min(oracle.matches.len()));
                        assert_eq!(report.hits_truncated(), oracle.matches.len() > retained);
                        assert_eq!(report.stats().scanned_bytes, bytes.len() as u64);
                        let mut witness_bytes = 0;
                        for (ordinal, (hit, expected)) in report.hits().iter().zip(&oracle.matches).enumerate() {
                            assert_eq!(hit.occurrence_id(), ordinal as u64 + 1);
                            assert_eq!(hit.original_range(), expected.original_byte_range);
                            assert_eq!(report.matched_text(ordinal).unwrap(), expected.matched_text);
                            let (start, end) = hit.original_range().as_usize_bounds().unwrap();
                            assert_eq!(report.witness_bytes(ordinal).unwrap(), &bytes[start..end]);
                            witness_bytes += end - start;
                        }
                        assert_eq!(report.retained_witness_bytes(), witness_bytes);
                    }
                }
            }
        }
    }
}

#[test]
fn longer_than_window_lowercase_match_retains_original_spelling_without_source_buffering() {
    let needle = "abK".repeat(6000);
    let spelling = "ABK".repeat(6000);
    let text = format!("x{spelling}y{spelling}");
    let budget = budget();
    let pattern = StreamingNeedle::text_matching(owner(), &needle, false, &budget, allocation(1)).unwrap();
    let mut options = StreamReadOptions::new(generation()); options.max_matches = 2;
    let mut query = ReaderSearch::new_counting(Chunks { remaining: text.as_bytes(), chunk: 37 }, request(),
        ByteLength::new(text.len() as u64), &pattern, options, &budget, allocation(2)).unwrap();
    finish(&mut query, STREAM_BUFFER_BYTES);
    let (_, report) = query.finish().unwrap();
    assert!(report.input_complete());
    assert_eq!(report.matches_seen(), 2);
    assert_eq!(report.hits()[0].original_range().start().get(), 1);
    assert_eq!(report.hits()[1].original_range().start().get(), spelling.len() as u64 + 2);
    for ordinal in 0..2 {
        assert_eq!(report.matched_text(ordinal).unwrap(), spelling);
        assert_eq!(report.witness_bytes(ordinal).unwrap(), spelling.as_bytes());
    }
}

#[test]
fn lowercase_limits_cancellation_and_malformed_tail_never_claim_complete_counts() {
    let budget = budget();
    let pattern = StreamingNeedle::text_matching(owner(), "k", false, &budget, allocation(1)).unwrap();
    for (count_all, retained, expected) in [(false, 0, 1), (false, 1, 2), (true, 0, 4), (true, 1, 4)] {
        let text = "KKkK";
        let mut options = StreamReadOptions::new(generation()); options.max_matches = retained;
        let mut query = if count_all {
            ReaderSearch::new_counting(text.as_bytes(), request(), ByteLength::new(text.len() as u64),
                &pattern, options, &budget, allocation(2))
        } else {
            ReaderSearch::new(text.as_bytes(), request(), ByteLength::new(text.len() as u64),
                &pattern, options, &budget, allocation(2))
        }.unwrap();
        finish(&mut query, 1);
        let (_, report) = query.finish().unwrap();
        assert_eq!(report.matches_seen(), expected);
        assert_eq!(report.input_complete(), count_all);
        assert_eq!(report.hits().len(), retained);
    }
    for bytes in [b"Kk\xff".as_slice(), b"Kk\xe2\x84".as_slice()] {
        let mut query = ReaderSearch::new_counting(Chunks { remaining: bytes, chunk: 1 }, request(),
            ByteLength::new(bytes.len() as u64), &pattern, StreamReadOptions::new(generation()), &budget, allocation(2)).unwrap();
        finish(&mut query, 1);
        let (_, report) = query.finish().unwrap();
        assert_eq!(report.state(), StreamReadState::UnsupportedText);
        assert_eq!(report.matches_seen(), 0);
        assert!(report.hits().is_empty());
        assert_eq!(report.retained_witness_bytes(), 0);
    }
    let mut options = StreamReadOptions::new(generation()); options.max_bytes = 4;
    let mut query = ReaderSearch::new_counting("KKk".as_bytes(), request(), ByteLength::new(5),
        &pattern, options, &budget, allocation(2)).unwrap();
    finish(&mut query, 1);
    assert_eq!(query.state(), StreamReadState::ByteLimit);
    assert_eq!(query.matches_seen(), 2);
    drop(query);
    let mut query = ReaderSearch::new_counting(b"Kkk".as_slice(), request(), ByteLength::new(3),
        &pattern, StreamReadOptions::new(generation()), &budget, allocation(2)).unwrap();
    query.step(StreamReadStep::default(), generation(), || true).unwrap();
    assert_eq!(query.state(), StreamReadState::Canceled);
    assert_eq!(query.stats().bytes_read, 0);
}

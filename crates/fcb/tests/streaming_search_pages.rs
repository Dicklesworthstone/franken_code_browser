#![forbid(unsafe_code)]
#![cfg(feature = "search")]

use std::{io::{self, Read}, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{CaptureRequest, CompleteCapture, DirectSourceScanner, QueryGeneration,
    QueryOptions, ReaderSearch, ResourceAllocationId, ResourceBudget, SearchMode,
    StreamReadError, StreamReadOptions, StreamReadState, StreamReadStep, StreamingNeedle,
    UnicodeNormalization};
use fcb::search::streaming::{MAX_STREAM_STEP_HITS, STREAM_BUFFER_BYTES};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1251).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }

struct Chunks<'a> { remaining: &'a [u8], chunk: usize }
impl Read for Chunks<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = output.len().min(self.chunk).min(self.remaining.len());
        output[..count].copy_from_slice(&self.remaining[..count]);
        self.remaining = &self.remaining[count..];
        Ok(count)
    }
}
fn finish<R: Read>(query: &mut ReaderSearch<'_, R>, step: StreamReadStep) {
    for _ in 0..100_000 {
        if query.state() != StreamReadState::Pending { return; }
        let before = query.matches_seen();
        query.step(step, generation(), || false).unwrap();
        assert!(query.stats().last_step_read <= step.max_bytes.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_scanned <= step.max_bytes.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_calls <= step.max_calls.min(32));
        // Skipping results must not evade the per-step matching-work allowance.
        assert!(query.matches_seen() - before <= step.max_hits.min(MAX_STREAM_STEP_HITS) as u64);
        assert!(query.stats().peak_buffer_bytes <= STREAM_BUFFER_BYTES);
    }
    panic!("paged streaming search did not terminate");
}
fn encode(text: &str, encoding: usize) -> Vec<u8> {
    match encoding {
        0 => text.as_bytes().to_vec(),
        1 => [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
        _ => {
            let mut bytes = if encoding == 2 { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if encoding == 2 { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            bytes
        }
    }
}

#[test]
fn literal_pages_preserve_global_occurrences_encodings_overlaps_and_lowercase_spelling() {
    let text = "bananana BANANANA KkK İi\u{307}İ 😀ana\r\nana\u{feff}ana";
    for encoding in 0..4 {
        let bytes = encode(text, encoding);
        for case_sensitive in [true, false] {
            for needle in ["ana", "k", "i", "\u{307}", "i\u{307}", "😀", "absent"] {
                let budget = budget();
                let pattern = StreamingNeedle::text_matching(owner(), needle, case_sensitive, &budget, allocation(1)).unwrap();
                let capture = CompleteCapture::new(request(), ByteLength::new(bytes.len() as u64), Arc::from(bytes.as_slice())).unwrap();
                let options = QueryOptions::new(generation()).with_mode(SearchMode::DecodedText {
                    case_sensitive, normalization: UnicodeNormalization::Exact,
                });
                let oracle = DirectSourceScanner::scan_complete_capture(&capture, needle, &options).unwrap();
                for skip in [0, 1, 2, 4, oracle.matches.len() as u64, u64::MAX] {
                    let start = usize::try_from(skip).unwrap_or(usize::MAX).min(oracle.matches.len());
                    let expected = &oracle.matches[start..oracle.matches.len().min(start + 2)];
                    for chunk in [1, 3, STREAM_BUFFER_BYTES] {
                        let mut options = StreamReadOptions::new(generation()); options.max_matches = 2;
                        let input = Chunks { remaining: &bytes, chunk };
                        let mut query = ReaderSearch::new_page(input, request(), ByteLength::new(bytes.len() as u64),
                            &pattern, options, skip, false, &budget, allocation(2)).unwrap();
                        finish(&mut query, StreamReadStep { max_bytes: 3, max_hits: 1, max_calls: 1 });
                        let (_, report) = query.finish().unwrap();
                        let more = start + expected.len() < oracle.matches.len();
                        assert_eq!(report.page_start_after(), skip);
                        assert_eq!(report.skipped_matches(), start as u64);
                        assert_eq!(report.has_more_matches(), more);
                        assert_eq!(report.input_complete(), !more);
                        assert_eq!(report.matches_seen(), (start + expected.len() + usize::from(more)) as u64);
                        assert_eq!(report.hits().len(), expected.len());
                        for (index, (hit, expected)) in report.hits().iter().zip(expected).enumerate() {
                            assert_eq!(hit.occurrence_id(), skip + index as u64 + 1);
                            assert_eq!(hit.original_range(), expected.original_byte_range);
                            assert_eq!(report.matched_text(index).unwrap(), expected.matched_text);
                            let (start, end) = hit.original_range().as_usize_bounds().unwrap();
                            assert_eq!(report.witness_bytes(index).unwrap(), &bytes[start..end]);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn raw_and_whole_token_pages_share_the_same_bounded_ordinal_contract() {
    let bytes = b"foo foobar foo_ $foo foo foo";
    let expected_tokens = [(0, 3), (21, 24), (25, 28)];
    for token in [false, true] {
        let budget = budget();
        let pattern = if token {
            StreamingNeedle::identifier(owner(), "foo", &budget, allocation(1)).unwrap()
        } else {
            StreamingNeedle::raw(owner(), b"foo", &budget, allocation(1)).unwrap()
        };
        let expected: Vec<(u64, u64)> = if token { expected_tokens.to_vec() } else {
            bytes.windows(3).enumerate().filter_map(|(index, value)|
                (value == b"foo").then_some((index as u64, index as u64 + 3))).collect()
        };
        for skip in 0..=expected.len() as u64 {
            let mut options = StreamReadOptions::new(generation()); options.max_matches = 1;
            let input = Chunks { remaining: bytes, chunk: 1 };
            let mut query = ReaderSearch::new_page(input, request(), ByteLength::new(bytes.len() as u64),
                &pattern, options, skip, false, &budget, allocation(2)).unwrap();
            finish(&mut query, StreamReadStep { max_bytes: 1, max_hits: 1, max_calls: 1 });
            let (_, report) = query.finish().unwrap();
            assert_eq!(report.skipped_matches(), skip);
            assert_eq!(report.hits().len(), usize::from(skip < expected.len() as u64));
            if let Some(hit) = report.hits().first() {
                let expected = expected[skip as usize];
                assert_eq!((hit.original_range().start().get(), hit.original_range().end().get()), expected);
                assert_eq!(hit.occurrence_id(), skip + 1);
                assert_eq!(report.witness_bytes(0).unwrap(), b"foo");
            }
            assert_eq!(report.has_more_matches(), skip + 1 < expected.len() as u64);
            assert_eq!(report.input_complete(), skip + 1 >= expected.len() as u64);
        }
    }
}

#[test]
fn counted_pages_distinguish_earlier_omissions_from_later_omissions_including_zero_rows() {
    let bytes = b"aaaaa";
    let budget = budget();
    let pattern = StreamingNeedle::text(owner(), "aa", &budget, allocation(1)).unwrap();
    for skip in [0, 2, 4, u64::MAX] {
        for retained in [0, 1, 4] {
            let mut options = StreamReadOptions::new(generation()); options.max_matches = retained;
            let mut query = ReaderSearch::new_page(bytes.as_slice(), request(), ByteLength::new(5),
                &pattern, options, skip, true, &budget, allocation(2)).unwrap();
            finish(&mut query, StreamReadStep::default());
            let (_, report) = query.finish().unwrap();
            let skipped = skip.min(4);
            let expected = (4 - skipped).min(retained as u64);
            assert!(report.input_complete());
            assert!(report.count_all_matches());
            assert_eq!(report.matches_seen(), 4);
            assert_eq!(report.skipped_matches(), skipped);
            assert_eq!(report.hits().len() as u64, expected);
            assert_eq!(report.has_more_matches(), 4 - skipped > expected);
            assert_eq!(report.hits_truncated(), expected < 4);
            for (index, hit) in report.hits().iter().enumerate() {
                assert_eq!(hit.occurrence_id(), skip + index as u64 + 1);
            }
        }
    }
    let mut options = StreamReadOptions::new(generation()); options.max_matches = 0;
    let mut query = ReaderSearch::new_page(bytes.as_slice(), request(), ByteLength::new(5),
        &pattern, options, 2, false, &budget, allocation(2)).unwrap();
    finish(&mut query, StreamReadStep::default());
    let (_, report) = query.finish().unwrap();
    assert_eq!(report.state(), StreamReadState::Truncated);
    assert_eq!(report.matches_seen(), 3);
    assert_eq!(report.skipped_matches(), 2);
    assert!(report.hits().is_empty());
    assert!(report.has_more_matches());
}

#[test]
fn limits_and_cancellation_while_skipping_never_claim_the_requested_page_or_end() {
    let budget = budget();
    let pattern = StreamingNeedle::text(owner(), "a", &budget, allocation(1)).unwrap();
    for byte_limit in [true, false] {
        let mut options = StreamReadOptions::new(generation()); options.max_matches = 1;
        if byte_limit { options.max_bytes = 3; } else { options.max_read_calls = 3; }
        let input = Chunks { remaining: b"aaaaaaaa", chunk: 1 };
        let mut query = ReaderSearch::new_page(input, request(), ByteLength::new(8),
            &pattern, options, 100, false, &budget, allocation(2)).unwrap();
        finish(&mut query, StreamReadStep { max_bytes: 1, max_hits: 1, max_calls: 1 });
        let (_, report) = query.finish().unwrap();
        assert_eq!(report.state(), if byte_limit { StreamReadState::ByteLimit } else { StreamReadState::CallLimit });
        assert_eq!(report.page_start_after(), 100);
        assert_eq!(report.matches_seen(), 3);
        assert_eq!(report.skipped_matches(), 3);
        assert!(report.hits().is_empty());
        assert!(!report.has_more_matches());
        assert!(!report.input_complete());
    }
    for stale in [false, true] {
        let options = StreamReadOptions::new(generation());
        let mut query = ReaderSearch::new_page(b"aaaa".as_slice(), request(), ByteLength::new(4),
            &pattern, options, 100, false, &budget, allocation(2)).unwrap();
        query.step(StreamReadStep { max_bytes: 1, max_hits: 1, max_calls: 1 }, generation(), || false).unwrap();
        if stale {
            let foreign = QueryGeneration::new(owner(), 2).unwrap();
            assert_eq!(query.step(StreamReadStep::default(), foreign, || false), Err(StreamReadError::StaleGeneration));
        } else {
            assert_eq!(query.step(StreamReadStep::default(), generation(), || true).unwrap(), StreamReadState::Canceled);
        }
        let (_, report) = query.finish().unwrap();
        assert_eq!(report.state(), StreamReadState::Canceled);
        assert_eq!(report.matches_seen(), 1);
        assert_eq!(report.skipped_matches(), 1);
        assert_eq!(report.stats().bytes_read, 1);
        assert!(!report.input_complete());
        assert!(!report.has_more_matches());
    }
}

#[test]
fn skipped_prefix_still_validates_text_and_short_reads_remain_partial() {
    let budget = budget();
    let pattern = StreamingNeedle::text(owner(), "a", &budget, allocation(1)).unwrap();
    let mut options = StreamReadOptions::new(generation()); options.max_matches = 1;
    let input = Chunks { remaining: b"aaa\xff", chunk: 1 };
    let mut query = ReaderSearch::new_page(input, request(), ByteLength::new(4),
        &pattern, options, 1, true, &budget, allocation(2)).unwrap();
    finish(&mut query, StreamReadStep { max_bytes: 1, max_hits: 1, max_calls: 1 });
    let (_, report) = query.finish().unwrap();
    assert_eq!(report.state(), StreamReadState::UnsupportedText);
    assert_eq!(report.page_start_after(), 1);
    assert_eq!(report.matches_seen(), 0);
    assert_eq!(report.skipped_matches(), 0);
    assert!(report.hits().is_empty());
    assert!(!report.input_complete());
    drop(report);
    let mut query = ReaderSearch::new_page(b"aaaa".as_slice(), request(), ByteLength::new(10),
        &pattern, options, 1, true, &budget, allocation(2)).unwrap();
    finish(&mut query, StreamReadStep::default());
    let (_, report) = query.finish().unwrap();
    assert_eq!(report.state(), StreamReadState::ShortRead);
    assert_eq!(report.matches_seen(), 4);
    assert_eq!(report.skipped_matches(), 1);
    assert_eq!(report.hits()[0].occurrence_id(), 2);
    assert!(report.has_more_matches());
    assert!(!report.input_complete());
}

#[test]
fn page_offsets_do_not_grow_resource_reservations_and_report_drop_releases_them() {
    let budget = budget();
    let pattern = StreamingNeedle::text_matching(owner(), "k", false, &budget, allocation(1)).unwrap();
    let base = budget.accounting().reserved().get();
    let mut expected_charge = None;
    for skip in [0, 1, 100, u64::MAX] {
        let mut options = StreamReadOptions::new(generation()); options.max_matches = 1;
        let mut query = ReaderSearch::new_page("KkK".as_bytes(), request(), ByteLength::new(5),
            &pattern, options, skip, true, &budget, allocation(2)).unwrap();
        let charge = budget.accounting().reserved().get() - base;
        if let Some(expected) = expected_charge { assert_eq!(charge, expected); } else { expected_charge = Some(charge); }
        finish(&mut query, StreamReadStep::default());
        let (_, report) = query.finish().unwrap();
        assert_eq!(budget.accounting().reserved().get(), base + charge);
        assert_eq!(report.retained_witness_bytes(), if skip == 0 { 3 } else if skip == 1 { 1 } else { 0 });
        drop(report);
        assert_eq!(budget.accounting().reserved().get(), base);
    }
    drop(pattern);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#![forbid(unsafe_code)]
#![cfg(feature = "search")]

use std::{io::{self, Read}, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{CaptureRequest, CompleteCapture, DirectSourceScanner, QueryGeneration,
    QueryOptions, ReaderSearch, ResourceAllocationId, ResourceBudget, StreamReadError,
    StreamReadOptions, StreamReadState, StreamReadStep, StreamingNeedle};
use fcb::search::streaming::STREAM_BUFFER_BYTES;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1215).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
struct Chunks<'a> { remaining: &'a [u8], chunk: usize, calls: usize }
impl Read for Chunks<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        let count = output.len().min(self.chunk).min(self.remaining.len());
        output[..count].copy_from_slice(&self.remaining[..count]);
        self.remaining = &self.remaining[count..];
        Ok(count)
    }
}
fn finish<R: Read>(query: &mut ReaderSearch<'_, R>, step: StreamReadStep) {
    for _ in 0..1_000_000 {
        if query.state() != StreamReadState::Pending { return; }
        let _ = query.step(step, query.generation(), || false);
        assert!(query.stats().last_step_read <= step.max_bytes.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_scanned <= step.max_bytes.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_calls <= step.max_calls.min(32));
    }
    panic!("stream did not terminate");
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}

#[test]
fn streaming_matches_the_capture_oracle_across_encodings_and_every_short_read_boundary() {
    let text = "\u{feff}banana 😀\r\nana café\u{feff}😀";
    let mut utf8_bom = vec![0xef, 0xbb, 0xbf]; utf8_bom.extend_from_slice(text.as_bytes());
    for bytes in [text.as_bytes().to_vec(), utf8_bom, utf16(text, true), utf16(text, false)] {
        for needle in ["ana", "😀", "café", "\r\n", "\u{feff}", "absent"] {
            let budget = budget();
            let pattern = StreamingNeedle::text(owner(), needle, &budget, allocation(1)).unwrap();
            let capture = CompleteCapture::new(request(), ByteLength::new(bytes.len() as u64), Arc::from(bytes.as_slice())).unwrap();
            let oracle = DirectSourceScanner::scan_complete_capture(&capture, needle, &QueryOptions::new(generation(1))).unwrap();
            let expected: Vec<_> = oracle.matches.iter().map(|hit| hit.original_byte_range).collect();
            for chunk in [1, 2, 3, 4, 7, STREAM_BUFFER_BYTES] {
                for quantum in [1, 3, STREAM_BUFFER_BYTES] {
                    let input = Chunks { remaining: &bytes, chunk, calls: 0 };
                    let mut query = ReaderSearch::new(input, request(), ByteLength::new(bytes.len() as u64),
                        &pattern, StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
                    finish(&mut query, StreamReadStep { max_bytes: quantum, max_hits: 1, max_calls: 2 });
                    let (_, report) = query.finish().unwrap();
                    assert!(report.input_complete(), "needle={needle}, chunk={chunk}, quantum={quantum}: {:?}", report.state());
                    assert_eq!(report.hits().iter().map(|hit| hit.original_range()).collect::<Vec<_>>(), expected);
                    for (index, hit) in report.hits().iter().enumerate() {
                        let (start, end) = hit.original_range().as_usize_bounds().unwrap();
                        assert_eq!(report.witness_bytes(index).unwrap(), &bytes[start..end]);
                        assert_eq!(hit.occurrence_id(), index as u64 + 1);
                    }
                }
            }
        }
    }
}

#[test]
fn literals_longer_than_the_input_buffer_cross_arbitrarily_many_reads() {
    let literal = "abc".repeat(7000);
    let bytes = format!("x{literal}y{literal}z").into_bytes();
    let budget = budget();
    let pattern = StreamingNeedle::text(owner(), &literal, &budget, allocation(1)).unwrap();
    let input = Chunks { remaining: &bytes, chunk: 37, calls: 0 };
    let mut query = ReaderSearch::new(input, request(), ByteLength::new(bytes.len() as u64), &pattern,
        StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    finish(&mut query, StreamReadStep::default());
    let (_, report) = query.finish().unwrap();
    assert!(report.input_complete());
    assert_eq!(report.hits().iter().map(|hit| hit.original_range().start().get()).collect::<Vec<_>>(), [1, 21_002]);
    assert!(report.stats().peak_buffer_bytes <= STREAM_BUFFER_BYTES);
}

#[test]
fn encoded_utf16_byte_matches_at_odd_offsets_are_not_text_matches() {
    let bytes = [0xff, 0xfe, 0x00, 0x61, 0x00, 0x00];
    let budget = budget();
    let text = StreamingNeedle::text(owner(), "a", &budget, allocation(1)).unwrap();
    let raw = StreamingNeedle::raw(owner(), &[0x61, 0x00], &budget, allocation(2)).unwrap();
    for (pattern, matches) in [(&text, 0), (&raw, 1)] {
        let mut query = ReaderSearch::new(bytes.as_slice(), request(), ByteLength::new(bytes.len() as u64), pattern,
            StreamReadOptions::new(generation(1)), &budget, allocation(3)).unwrap();
        finish(&mut query, StreamReadStep::default());
        let (_, report) = query.finish().unwrap();
        assert!(report.input_complete()); assert_eq!(report.hits().len(), matches);
    }
}

#[test]
fn exact_result_capacity_uses_one_lookahead_and_zero_capacity_can_prove_absence() {
    let budget = budget();
    let pattern = StreamingNeedle::raw(owner(), b"ana", &budget, allocation(1)).unwrap();
    for (limit, state, count) in [(0, StreamReadState::Truncated, 1), (1, StreamReadState::Truncated, 2),
        (2, StreamReadState::Complete, 2), (3, StreamReadState::Complete, 2)] {
        let mut options = StreamReadOptions::new(generation(1)); options.max_matches = limit;
        let mut query = ReaderSearch::new(b"banana".as_slice(), request(), ByteLength::new(6), &pattern,
            options, &budget, allocation(2)).unwrap();
        finish(&mut query, StreamReadStep { max_bytes: 1, max_hits: 1, max_calls: 1 });
        let (_, report) = query.finish().unwrap();
        assert_eq!(report.state(), state); assert_eq!(report.matches_seen(), count);
        assert_eq!(report.hits().len(), limit.min(2));
    }
    let mut options = StreamReadOptions::new(generation(1)); options.max_matches = 0;
    let mut query = ReaderSearch::new(b"nothing".as_slice(), request(), ByteLength::new(7), &pattern,
        options, &budget, allocation(2)).unwrap();
    finish(&mut query, StreamReadStep::default());
    assert_eq!(query.state(), StreamReadState::Complete);
}

#[test]
fn byte_and_call_limits_are_not_eof_and_do_not_finalize_partial_unicode() {
    let budget = budget();
    let pattern = StreamingNeedle::text(owner(), "é", &budget, allocation(1)).unwrap();
    for limit in [0, 1] {
        let mut options = StreamReadOptions::new(generation(1)); options.max_bytes = limit;
        let mut query = ReaderSearch::new("é".as_bytes(), request(), ByteLength::new(2), &pattern,
            options, &budget, allocation(2)).unwrap();
        finish(&mut query, StreamReadStep::default());
        let (_, report) = query.finish().unwrap();
        assert_eq!(report.state(), StreamReadState::ByteLimit);
        assert_eq!(report.stats().bytes_read, limit); assert_eq!(report.unsupported_at(), None);
    }
    struct Interrupted;
    impl Read for Interrupted { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Err(io::ErrorKind::Interrupted.into()) } }
    let mut options = StreamReadOptions::new(generation(1)); options.max_read_calls = 3;
    let mut query = ReaderSearch::new(Interrupted, request(), ByteLength::new(100), &pattern,
        options, &budget, allocation(2)).unwrap();
    finish(&mut query, StreamReadStep { max_calls: 1, ..Default::default() });
    assert_eq!(query.state(), StreamReadState::CallLimit);
    assert_eq!(query.stats().read_calls, 3); assert_eq!(query.stats().interrupted_calls, 3);
}

#[test]
fn short_reads_and_malformed_suffixes_cannot_claim_complete_text() {
    let budget = budget();
    let pattern = StreamingNeedle::text(owner(), "needle", &budget, allocation(1)).unwrap();
    let mut query = ReaderSearch::new(b"needle".as_slice(), request(), ByteLength::new(100), &pattern,
        StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    finish(&mut query, StreamReadStep::default());
    let (_, short) = query.finish().unwrap();
    assert_eq!(short.state(), StreamReadState::ShortRead); assert_eq!(short.hits().len(), 1);
    drop(short);
    for bytes in [b"needle\xff".to_vec(), [utf16("needle", true), vec![0x00, 0xd8]].concat(),
        [utf16("needle", false), vec![0xff]].concat()] {
        let input = Chunks { remaining: &bytes, chunk: 1, calls: 0 };
        let mut query = ReaderSearch::new(input, request(), ByteLength::new(bytes.len() as u64), &pattern,
            StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
        finish(&mut query, StreamReadStep::default());
        let (_, report) = query.finish().unwrap();
        assert_eq!(report.state(), StreamReadState::UnsupportedText);
        assert!(report.unsupported_at().is_some()); assert!(report.hits().is_empty());
    }
}

#[test]
fn cancellation_and_stale_queries_never_resume_the_reader() {
    let budget = budget();
    let pattern = StreamingNeedle::raw(owner(), b"x", &budget, allocation(1)).unwrap();
    for stale in [false, true] {
        let mut query = ReaderSearch::new(b"xxxxxxxx".as_slice(), request(), ByteLength::new(8), &pattern,
            StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
        query.step(StreamReadStep { max_bytes: 1, ..Default::default() }, generation(1), || false).unwrap();
        let before = query.stats().bytes_read;
        let result = query.step(StreamReadStep::default(), generation(if stale { 2 } else { 1 }), || !stale);
        if stale { assert_eq!(result, Err(StreamReadError::StaleGeneration)); }
        query.step(StreamReadStep::default(), generation(1), || false).unwrap();
        assert_eq!(query.state(), StreamReadState::Canceled); assert_eq!(query.stats().bytes_read, before);
        let (_, report) = query.finish().unwrap(); assert!(!report.input_complete());
    }
}

#[test]
fn input_length_does_not_control_working_memory_and_reports_hold_their_lease() {
    let budget = budget();
    let pattern = StreamingNeedle::raw(owner(), b"needle", &budget, allocation(1)).unwrap();
    let base = budget.accounting().reserved().get();
    let mut charges = Vec::new();
    for length in [1, 1 << 32, u64::MAX] {
        let mut query = ReaderSearch::new(io::empty(), request(), ByteLength::new(length), &pattern,
            StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
        charges.push(budget.accounting().reserved().get() - base);
        query.cancel();
        let (_, report) = query.finish().unwrap();
        assert!(budget.accounting().reserved().get() > base);
        drop(report); assert_eq!(budget.accounting().reserved().get(), base);
    }
    assert!(charges.windows(2).all(|pair| pair[0] == pair[1]));
    drop(pattern); assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn io_errors_and_invalid_read_counts_have_terminal_failure_states() {
    struct Invalid;
    impl Read for Invalid { fn read(&mut self, output: &mut [u8]) -> io::Result<usize> { Ok(output.len() + 1) } }
    struct Broken;
    impl Read for Broken { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Err(io::ErrorKind::Other.into()) } }
    let budget = budget();
    let pattern = StreamingNeedle::raw(owner(), b"x", &budget, allocation(1)).unwrap();
    let mut invalid = ReaderSearch::new(Invalid, request(), ByteLength::new(8), &pattern,
        StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    assert_eq!(invalid.step(StreamReadStep::default(), generation(1), || false), Err(StreamReadError::InvalidReadCount));
    assert_eq!(invalid.finish().unwrap().1.state(), StreamReadState::Failed(StreamReadError::InvalidReadCount));
    let mut broken = ReaderSearch::new(Broken, request(), ByteLength::new(8), &pattern,
        StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    assert_eq!(broken.step(StreamReadStep::default(), generation(1), || false), Err(StreamReadError::Io));
    assert_eq!(broken.finish().unwrap().1.state(), StreamReadState::Failed(StreamReadError::Io));
}

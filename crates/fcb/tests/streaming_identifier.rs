#![forbid(unsafe_code)]
#![cfg(feature = "search")]

use std::io::{self, Read};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{CaptureRequest, QueryGeneration, ReaderSearch, ResourceAllocationId,
    ResourceBudget, StreamReadError, StreamReadOptions, StreamReadState, StreamReadStep,
    StreamingMode, StreamingNeedle};
use fcb::search::streaming::{MAX_STREAM_STEP_HITS, STREAM_BUFFER_BYTES};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1217).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}

struct Chunks<'a> { bytes: &'a [u8], size: usize }
impl Read for Chunks<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = output.len().min(self.size).min(self.bytes.len());
        output[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        Ok(count)
    }
}

fn finish<R: Read>(query: &mut ReaderSearch<'_, R>, step: StreamReadStep) {
    for _ in 0..100_000 {
        if query.state() != StreamReadState::Pending { return; }
        let before = query.matches_seen();
        query.step(step, query.generation(), || false).unwrap();
        assert!(query.matches_seen().saturating_sub(before) <= step.max_hits.min(MAX_STREAM_STEP_HITS) as u64);
        assert!(query.stats().last_step_read <= step.max_bytes.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_scanned <= step.max_bytes.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_calls <= step.max_calls.min(32));
    }
    panic!("whole-token observation did not terminate");
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

// Independent whole-string oracle: literal occurrences qualify only if their
// neighboring scalars are separators. It does not use the production cursor,
// decoder maps, chunking or shared token predicate.
fn expected(text: &str, name: &str, encoding: usize) -> Vec<(u64, u64)> {
    let boundary = |ch: char| ch.is_whitespace() || ch.is_control()
        || (ch.is_ascii() && !ch.is_ascii_alphanumeric() && ch != '_' && ch != '$');
    let offset = |position: usize| match encoding {
        0 => position as u64,
        1 => position as u64 + 3,
        _ => text[..position].encode_utf16().count() as u64 * 2 + 2,
    };
    text.match_indices(name).filter_map(|(start, _)| {
        let end = start + name.len();
        (text[..start].chars().next_back().is_none_or(boundary)
            && text[end..].chars().next().is_none_or(boundary))
            .then_some((offset(start), offset(end)))
    }).collect()
}

#[test]
fn occurrences_preserve_token_policy_and_exact_witnesses_across_every_small_boundary() {
    let text = "foo foobar _foo foo_ $foo 1foo Foo foo. // foo\n\"foo\"\r\n\
        e\u{301} e e—next café Café 🚀🚀 🚀 go—next ß ss İ i foo\u{feff}foo\u{2003}foo";
    for encoding in 0..4 {
        let bytes = encode(text, encoding);
        for name in ["foo", "$foo", "e\u{301}", "e", "café", "🚀", "go—next", "ß", "İ", "foo\u{feff}foo"] {
            let budget = budget();
            let needle = StreamingNeedle::identifier(owner(), name, &budget, allocation(1)).unwrap();
            assert_eq!(needle.mode(), StreamingMode::Identifier);
            let expected = expected(text, name, encoding);
            for chunk in [1, 2, 3, 7, STREAM_BUFFER_BYTES] {
                for quantum in [1, 3, STREAM_BUFFER_BYTES] {
                    let input = Chunks { bytes: &bytes, size: chunk };
                    let mut search = ReaderSearch::new(input, request(), ByteLength::new(bytes.len() as u64),
                        &needle, StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
                    finish(&mut search, StreamReadStep { max_bytes: quantum, max_hits: 1, max_calls: 1 });
                    let (_, report) = search.finish().unwrap();
                    assert!(report.input_complete(), "{name:?}, encoding={encoding}, chunk={chunk}, quantum={quantum}");
                    let actual: Vec<_> = report.hits().iter().map(|hit|
                        (hit.original_range().start().get(), hit.original_range().end().get())).collect();
                    assert_eq!(actual, expected, "{name:?}, encoding={encoding}, chunk={chunk}, quantum={quantum}");
                    for (index, hit) in report.hits().iter().enumerate() {
                        let (start, end) = hit.original_range().as_usize_bounds().unwrap();
                        assert_eq!(report.witness_bytes(index).unwrap(), &bytes[start..end]);
                        assert_eq!(report.matched_text(index).unwrap(), name);
                        assert_eq!(hit.occurrence_id(), index as u64 + 1);
                    }
                }
            }
        }
    }
}

#[test]
fn cap_lookahead_and_count_only_modes_count_tokens_instead_of_literal_substrings() {
    let budget = budget();
    let needle = StreamingNeedle::identifier(owner(), "foo", &budget, allocation(1)).unwrap();
    for encoding in 0..4 {
        let bytes = encode("foo foobar foo foo", encoding);
        for limit in [0, 1, 3, 4] {
            for count_all in [false, true] {
                let options = StreamReadOptions { max_matches: limit, ..StreamReadOptions::new(generation(1)) };
                let input = Chunks { bytes: &bytes, size: 1 };
                let mut search = if count_all {
                    ReaderSearch::new_counting(input, request(), ByteLength::new(bytes.len() as u64),
                        &needle, options, &budget, allocation(2))
                } else {
                    ReaderSearch::new(input, request(), ByteLength::new(bytes.len() as u64),
                        &needle, options, &budget, allocation(2))
                }.unwrap();
                finish(&mut search, StreamReadStep { max_bytes: 3, max_hits: 1, max_calls: 1 });
                let (_, report) = search.finish().unwrap();
                assert_eq!(report.matches_seen(), if count_all { 3 } else { (limit + 1).min(3) as u64 });
                assert_eq!(report.hits().len(), limit.min(3));
                assert_eq!(report.hits_truncated(), limit < 3);
                assert_eq!(report.input_complete(), count_all || limit >= 3);
            }
        }
    }
}

#[test]
fn byte_and_call_limits_never_close_an_unfinished_token() {
    let budget = budget();
    let needle = StreamingNeedle::identifier(owner(), "foo", &budget, allocation(1)).unwrap();
    for (bytes, length, chunk, byte_limit, call_limit, state, matches) in [
        (b"foobar".as_slice(), 6, 3, 3, 100, StreamReadState::ByteLimit, 0),
        (b"foobar".as_slice(), 6, 3, 100, 1, StreamReadState::CallLimit, 0),
        (b"foo rest".as_slice(), 8, 3, 3, 100, StreamReadState::ByteLimit, 0),
        (b"foo rest".as_slice(), 8, 4, 4, 100, StreamReadState::ByteLimit, 1),
        (b"foo".as_slice(), 3, 3, 3, 100, StreamReadState::Complete, 1),
        (b"foo".as_slice(), 4, 3, 100, 100, StreamReadState::ShortRead, 1),
    ] {
        let options = StreamReadOptions { max_bytes: byte_limit, max_read_calls: call_limit,
            ..StreamReadOptions::new(generation(1)) };
        let input = Chunks { bytes, size: chunk };
        let mut search = ReaderSearch::new(input, request(), ByteLength::new(length),
            &needle, options, &budget, allocation(2)).unwrap();
        finish(&mut search, StreamReadStep { max_bytes: 4, max_hits: 1, max_calls: 1 });
        let (_, report) = search.finish().unwrap();
        assert_eq!(report.state(), state);
        assert_eq!(report.matches_seen(), matches);
    }
    for encoding in [2, 3] {
        let bytes = encode("foo\u{2003}rest", encoding);
        let options = StreamReadOptions { max_bytes: 9, ..StreamReadOptions::new(generation(1)) };
        let mut search = ReaderSearch::new(bytes.as_slice(), request(), ByteLength::new(bytes.len() as u64),
            &needle, options, &budget, allocation(2)).unwrap();
        finish(&mut search, StreamReadStep::default());
        assert_eq!(search.state(), StreamReadState::ByteLimit);
        assert_eq!(search.matches_seen(), 0, "a partial UTF-16 separator is not a boundary");
    }
}

#[test]
fn malformed_text_retracts_earlier_tokens_and_cancellation_cannot_flush_one() {
    let budget = budget();
    let needle = StreamingNeedle::identifier(owner(), "foo", &budget, allocation(1)).unwrap();
    for bytes in [b"foo foo \xff".to_vec(), [encode("foo foo ", 2), vec![0x00, 0xd8]].concat(),
        [encode("foo foo ", 3), vec![0xff]].concat()] {
        let mut search = ReaderSearch::new_counting(Chunks { bytes: &bytes, size: 1 }, request(),
            ByteLength::new(bytes.len() as u64), &needle, StreamReadOptions::new(generation(1)),
            &budget, allocation(2)).unwrap();
        finish(&mut search, StreamReadStep::default());
        assert_eq!(search.state(), StreamReadState::UnsupportedText);
        assert_eq!(search.matches_seen(), 0);
        assert!(search.hits().is_empty());
    }
    let mut search = ReaderSearch::new(b"foobar".as_slice(), request(), ByteLength::new(6),
        &needle, StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    search.step(StreamReadStep { max_bytes: 3, max_hits: 1, max_calls: 1 }, generation(1), || false).unwrap();
    assert_eq!(search.matches_seen(), 0);
    assert_eq!(search.step(StreamReadStep::default(), generation(2), || false), Err(StreamReadError::StaleGeneration));
    assert_eq!(search.state(), StreamReadState::Canceled);
    assert_eq!(search.stats().bytes_read, 3);
    assert!(search.hits().is_empty());
}

#[test]
fn huge_tokens_and_dense_counting_keep_the_same_reservation() {
    let bytes = format!("{} {}", "foo".repeat(700_000), "foo ".repeat(10_000)).into_bytes();
    let budget = budget();
    let needle = StreamingNeedle::identifier(owner(), "foo", &budget, allocation(1)).unwrap();
    let options = StreamReadOptions { max_matches: 1, ..StreamReadOptions::new(generation(1)) };
    let mut search = ReaderSearch::new_counting(bytes.as_slice(), request(), ByteLength::new(bytes.len() as u64),
        &needle, options, &budget, allocation(2)).unwrap();
    let charge = budget.accounting().reserved().get();
    let step = StreamReadStep { max_bytes: STREAM_BUFFER_BYTES, max_hits: 2, max_calls: 1 };
    for _ in 0..20_000 {
        if search.state() != StreamReadState::Pending { break; }
        let before = search.matches_seen();
        search.step(step, generation(1), || false).unwrap();
        assert!(search.matches_seen() - before <= 2);
        assert_eq!(budget.accounting().reserved().get(), charge);
        assert!(search.hits().len() <= 1);
    }
    let (_, report) = search.finish().unwrap();
    assert!(report.input_complete());
    assert_eq!(report.matches_seen(), 10_000);
    assert_eq!(report.hits()[0].original_range().start().get(), 2_100_001);
    assert_eq!(report.retained_witness_bytes(), 3);
    assert!(report.stats().peak_buffer_bytes <= STREAM_BUFFER_BYTES);
}

#[test]
fn invalid_names_and_resource_denial_fail_before_reading() {
    let budget = budget();
    for name in ["", "1foo", "foo bar", "foo-bar", "foo\0"] {
        assert!(matches!(StreamingNeedle::identifier(owner(), name, &budget, allocation(1)),
            Err(StreamReadError::InvalidIdentifier)));
    }
    assert!(matches!(StreamingNeedle::identifier(owner(), &"x".repeat(1025), &budget, allocation(1)),
        Err(StreamReadError::InvalidIdentifier)));
    let needle = StreamingNeedle::identifier(owner(), "foo", &budget, allocation(1)).unwrap();
    struct NoRead;
    impl Read for NoRead { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("denied query read source") } }
    let denied = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(ReaderSearch::new(NoRead, request(), ByteLength::new(u64::MAX), &needle,
        StreamReadOptions::new(generation(1)), &denied, allocation(2)), Err(StreamReadError::ResourceDenied)));
}

#[cfg(feature = "analysis")]
#[test]
fn captured_occurrences_and_streaming_occurrences_agree() {
    use fcb::analysis::references::CapturedReferences;
    let text = "foo foo_ $foo foobar // foo\n\"foo\" e\u{301} e—x e foo";
    for encoding in 0..4 {
        let bytes = encode(text, encoding);
        for name in ["foo", "$foo", "e", "e\u{301}", "e—x"] {
            let budget = budget();
            let captured = CapturedReferences::build(&bytes, request(), generation(1), name,
                None, 100, &budget, allocation(1), || false).unwrap();
            let needle = StreamingNeedle::identifier(owner(), name, &budget, allocation(2)).unwrap();
            let mut search = ReaderSearch::new(Chunks { bytes: &bytes, size: 1 }, request(),
                ByteLength::new(bytes.len() as u64), &needle, StreamReadOptions::new(generation(1)),
                &budget, allocation(3)).unwrap();
            finish(&mut search, StreamReadStep { max_bytes: 3, max_hits: 1, max_calls: 1 });
            assert_eq!(search.hits().iter().map(|hit| hit.original_range()).collect::<Vec<_>>(),
                captured.candidates().iter().map(|hit| hit.original_range()).collect::<Vec<_>>());
            assert_eq!(search.matches_seen(), captured.total_matches_counted() as u64);
        }
    }
}

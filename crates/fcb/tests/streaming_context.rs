#![forbid(unsafe_code)]
#![cfg(feature = "search")]

use std::io::{self, Read};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{CaptureRequest, ParsedQuery, QueryGeneration, ReaderSearch, ResourceAllocationId,
    ResourceBudget, StreamContextOptions, StreamReadError, StreamReadOptions, StreamReadReport,
    StreamReadState, StreamReadStep, StreamingNeedle, MAX_STREAM_CONTEXT_SIDE_BYTES};
use fcb::search::streaming::{MAX_STREAM_RESULTS, STREAM_BUFFER_BYTES};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1927).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }

struct Chunks<'a> { remaining: &'a [u8], chunk: usize, calls: u64 }
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
        query.step(step, generation(), || false).unwrap();
        assert!(query.stats().last_step_read <= step.max_bytes.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_scanned <= step.max_bytes.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_calls <= step.max_calls.min(32));
    }
    panic!("context scan did not terminate");
}
fn verify(report: &StreamReadReport<'_>, source: &[u8], options: StreamContextOptions) {
    assert_eq!(report.context_options(), Some(options));
    let mut retained = 0;
    for (ordinal, hit) in report.hits().iter().enumerate() {
        let context = report.context(ordinal).unwrap().unwrap();
        let matched = hit.original_range();
        let requested_start = matched.start().get().saturating_sub(options.before_bytes as u64);
        let requested_end = matched.end().get().saturating_add(options.after_bytes as u64)
            .min(report.observed_length().get());
        let retained_end = requested_end.min(report.stats().bytes_read);
        assert_eq!(context.match_range(), matched);
        assert_eq!(context.requested_range().start().get(), requested_start);
        assert_eq!(context.requested_range().end().get(), requested_end);
        assert_eq!(context.original_range().start().get(), requested_start);
        assert_eq!(context.original_range().end().get(), retained_end);
        assert_eq!(context.bytes(), &source[requested_start as usize..retained_end as usize]);
        assert_eq!(context.is_complete(), retained_end == requested_end);
        let match_start = (matched.start().get() - requested_start) as usize;
        let match_end = match_start + matched.len().get() as usize;
        assert_eq!(&context.bytes()[match_start..match_end], report.witness_bytes(ordinal).unwrap());
        retained += context.bytes().len();
    }
    assert_eq!(report.retained_context_bytes(), retained);
    assert!(matches!(report.context(report.hits().len()), Err(StreamReadError::InvalidRange)));
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn exact_lowercase_and_identifier_contexts_preserve_original_utf8_and_utf16_bytes() {
    let text = "a banana café 😀 İ K k Rust Rusty Rust ";
    let mut bom = vec![0xef, 0xbb, 0xbf]; bom.extend_from_slice(text.as_bytes());
    let context = StreamContextOptions { before_bytes: 5, after_bytes: 9 };
    for source in [text.as_bytes().to_vec(), bom, utf16(text, true), utf16(text, false)] {
        for mode in 0..3 {
            let budget = budget();
            let pattern = match mode {
                0 => StreamingNeedle::text(owner(), "ana", &budget, allocation(1)),
                1 => StreamingNeedle::text_matching(owner(), "k", false, &budget, allocation(1)),
                _ => StreamingNeedle::identifier(owner(), "Rust", &budget, allocation(1)),
            }.unwrap();
            for chunk in [1, 3, 17, STREAM_BUFFER_BYTES] {
                for quantum in [1, STREAM_BUFFER_BYTES] {
                    let input = Chunks { remaining: &source, chunk, calls: 0 };
                    let mut options = StreamReadOptions::new(generation()); options.max_matches = 4;
                    let mut query = ReaderSearch::new_counting(input, request(), ByteLength::new(source.len() as u64),
                        &pattern, options, &budget, allocation(2)).unwrap()
                        .with_context(context, &budget, allocation(3)).unwrap();
                    let reserved = budget.accounting().reserved();
                    finish(&mut query, StreamReadStep { max_bytes: quantum, max_calls: 1, max_hits: 1 });
                    let (input, report) = query.finish().unwrap();
                    assert!(report.input_complete());
                    assert_eq!(report.matches_seen(), 2);
                    assert_eq!(input.calls, report.stats().read_calls);
                    assert_eq!(budget.accounting().reserved(), reserved);
                    verify(&report, &source, context);
                }
            }
        }
    }
}

#[test]
fn circular_history_keeps_long_cross_buffer_matches_and_bounded_neighboring_bytes() {
    let budget = budget();
    let mut literal = vec![b'a'; STREAM_BUFFER_BYTES + 29]; literal.push(b'!');
    let pattern = StreamingNeedle::raw(owner(), &literal, &budget, allocation(1)).unwrap();
    let mut source = vec![b'x'; 3 * STREAM_BUFFER_BYTES - 5];
    source.extend_from_slice(&literal); source.extend_from_slice(&vec![b'z'; 3 * STREAM_BUFFER_BYTES]);
    let context = StreamContextOptions { before_bytes: MAX_STREAM_CONTEXT_SIDE_BYTES,
        after_bytes: MAX_STREAM_CONTEXT_SIDE_BYTES };
    let mut options = StreamReadOptions::new(generation()); options.max_matches = 2;
    for chunk in [127, STREAM_BUFFER_BYTES] {
        let input = Chunks { remaining: &source, chunk, calls: 0 };
        let mut query = ReaderSearch::new_counting(input, request(), ByteLength::new(source.len() as u64),
            &pattern, options, &budget, allocation(2)).unwrap()
            .with_context(context, &budget, allocation(3)).unwrap();
        finish(&mut query, StreamReadStep::default());
        let (_, report) = query.finish().unwrap();
        assert!(report.input_complete()); assert_eq!(report.matches_seen(), 1);
        assert_eq!(report.hits()[0].original_range().start().get(), (3 * STREAM_BUFFER_BYTES - 5) as u64);
        verify(&report, &source, context);
    }
}

#[test]
fn retained_pages_do_not_store_skipped_hits_or_add_input_reads() {
    let budget = budget();
    let source = b"banana".repeat(6000);
    let pattern = StreamingNeedle::raw(owner(), b"ana", &budget, allocation(1)).unwrap();
    let context = StreamContextOptions { before_bytes: 31, after_bytes: 47 };
    for counting in [false, true] {
        for limit in [0, 2] {
            let mut expected = None;
            for capture in [false, true] {
                let input = Chunks { remaining: &source, chunk: 7, calls: 0 };
                let mut options = StreamReadOptions::new(generation()); options.max_matches = limit;
                let query = ReaderSearch::new_page(input, request(), ByteLength::new(source.len() as u64),
                    &pattern, options, 17, counting, &budget, allocation(2)).unwrap();
                let mut query = if capture { query.with_context(context, &budget, allocation(3)).unwrap() } else { query };
                finish(&mut query, StreamReadStep::default());
                let (input, report) = query.finish().unwrap();
                assert_eq!(report.hits().len(), limit);
                assert_eq!(report.skipped_matches(), 17);
                if limit > 0 { assert_eq!(report.hits()[0].occurrence_id(), 18); }
                let observed = (report.state(), report.stats(), report.matches_seen(), input.calls);
                if capture {
                    assert_eq!(Some(observed), expected, "context must never perform an extra read");
                    verify(&report, &source, context);
                } else {
                    expected = Some(observed);
                    assert_eq!(report.context_options(), None);
                    assert_eq!(report.retained_context_bytes(), 0);
                    if limit > 0 { assert!(report.context(0).unwrap().is_none()); }
                }
            }
        }
    }
}

#[test]
fn stops_and_cancellation_preserve_only_observed_suffixes() {
    let budget = budget();
    let source = b"prefix needle suffix beyond";
    let pattern = StreamingNeedle::text(owner(), "needle", &budget, allocation(1)).unwrap();
    let context = StreamContextOptions { before_bytes: 20, after_bytes: 20 };
    for (bytes, calls, actual_length, canceled, state) in [
        (15, 100, source.len(), false, StreamReadState::ByteLimit),
        (100, 4, source.len(), false, StreamReadState::CallLimit),
        (100, 100, 15, false, StreamReadState::ShortRead),
        (100, 100, source.len(), true, StreamReadState::Canceled),
    ] {
        let mut options = StreamReadOptions::new(generation());
        options.max_bytes = bytes; options.max_read_calls = calls; options.max_matches = 2;
        let input = Chunks { remaining: &source[..actual_length], chunk: 4, calls: 0 };
        let mut query = ReaderSearch::new(input, request(), ByteLength::new(source.len() as u64),
            &pattern, options, &budget, allocation(2)).unwrap()
            .with_context(context, &budget, allocation(3)).unwrap();
        if canceled {
            while query.hits().is_empty() {
                query.step(StreamReadStep::default(), generation(), || false).unwrap();
                assert_eq!(query.state(), StreamReadState::Pending);
            }
            query.cancel();
        } else { finish(&mut query, StreamReadStep::default()); }
        let (input, report) = query.finish().unwrap();
        assert_eq!(report.state(), state); assert_eq!(report.hits().len(), 1);
        assert!(!report.context(0).unwrap().unwrap().is_complete());
        assert_eq!(report.stats().read_calls, input.calls);
        verify(&report, source, context);
    }
}

#[test]
fn expressions_publish_context_only_after_all_predicates_qualify() {
    let budget = budget();
    let parsed = ParsedQuery::parse("anchor required -forbidden").unwrap();
    let context = StreamContextOptions { before_bytes: 3, after_bytes: 10 };
    for case_sensitive in [false, true] {
        let pattern = StreamingNeedle::expression(owner(), &parsed, case_sensitive, &budget, allocation(1)).unwrap();
        for (source, cancel, expected_hits) in [
            (b"anchor anchor required".as_slice(), false, 1),
            (b"anchor anchor required forbidden".as_slice(), false, 0),
            (b"anchor anchor required\xff".as_slice(), false, 0),
            (b"anchor anchor required".as_slice(), true, 0),
        ] {
            let input = Chunks { remaining: source, chunk: 3, calls: 0 };
            let mut options = StreamReadOptions::new(generation()); options.max_matches = 1;
            let mut query = ReaderSearch::new_page(input, request(), ByteLength::new(source.len() as u64),
                &pattern, options, 1, false, &budget, allocation(2)).unwrap()
                .with_context(context, &budget, allocation(3)).unwrap();
            if cancel {
                for _ in 0..5 { query.step(StreamReadStep::default(), generation(), || false).unwrap(); }
                query.cancel();
            } else { finish(&mut query, StreamReadStep::default()); }
            let (_, report) = query.finish().unwrap();
            assert_eq!(report.hits().len(), expected_hits);
            if expected_hits == 1 {
                assert_eq!(report.expression_qualified(), Some(true));
                assert_eq!(report.hits()[0].occurrence_id(), 2);
            } else { assert_eq!(report.retained_context_bytes(), 0); }
            verify(&report, source, context);
        }
    }
}

#[test]
fn raw_context_accepts_binary_and_reports_clipped_source_boundaries() {
    let budget = budget();
    let source = b"\xff\0a\xfe\0a";
    let pattern = StreamingNeedle::raw(owner(), b"\0a", &budget, allocation(1)).unwrap();
    let context = StreamContextOptions::default();
    let mut query = ReaderSearch::new(source.as_slice(), request(), ByteLength::new(source.len() as u64),
        &pattern, StreamReadOptions::new(generation()), &budget, allocation(2)).unwrap()
        .with_context(context, &budget, allocation(3)).unwrap();
    finish(&mut query, StreamReadStep { max_bytes: 1, max_calls: 1, max_hits: 1 });
    let (_, report) = query.finish().unwrap();
    assert_eq!(report.hits().len(), 2);
    for ordinal in 0..2 { assert_eq!(report.context(ordinal).unwrap().unwrap().bytes(), source); }
    verify(&report, source, context);
}

#[test]
fn context_admission_is_bounded_charged_and_released_with_the_report() {
    let budget = budget();
    let pattern = StreamingNeedle::raw(owner(), b"a", &budget, allocation(1)).unwrap();
    let baseline = budget.accounting().reserved();
    let make = |options| ReaderSearch::new(b"aaa".as_slice(), request(), ByteLength::new(3),
        &pattern, options, &budget, allocation(2)).unwrap();
    let options = StreamReadOptions::new(generation());
    assert!(matches!(make(options).with_context(StreamContextOptions {
        before_bytes: MAX_STREAM_CONTEXT_SIDE_BYTES + 1, after_bytes: 0 }, &budget, allocation(3)),
        Err(StreamReadError::InvalidLimits)));
    assert_eq!(budget.accounting().reserved(), baseline);
    let mut excessive = options; excessive.max_matches = MAX_STREAM_RESULTS;
    assert!(matches!(make(excessive).with_context(StreamContextOptions {
        before_bytes: MAX_STREAM_CONTEXT_SIDE_BYTES, after_bytes: MAX_STREAM_CONTEXT_SIDE_BYTES }, &budget, allocation(3)),
        Err(StreamReadError::InvalidLimits)));
    assert_eq!(budget.accounting().reserved(), baseline);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(make(options).with_context(StreamContextOptions::default(), &tiny, allocation(3)),
        Err(StreamReadError::ResourceDenied)));
    assert_eq!(budget.accounting().reserved(), baseline);
    let query = make(options).with_context(StreamContextOptions::default(), &budget, allocation(3)).unwrap();
    assert!(matches!(query.with_context(StreamContextOptions::default(), &budget, allocation(4)),
        Err(StreamReadError::InvalidLimits)));
    assert_eq!(budget.accounting().reserved(), baseline);
    let mut read_started = make(options);
    read_started.step(StreamReadStep { max_bytes: 1, ..Default::default() }, generation(), || false).unwrap();
    assert!(matches!(read_started.with_context(StreamContextOptions::default(), &budget, allocation(3)),
        Err(StreamReadError::InvalidLimits)));
    assert_eq!(budget.accounting().reserved(), baseline);
    let mut query = make(options).with_context(StreamContextOptions::default(), &budget, allocation(3)).unwrap();
    let reserved = budget.accounting().reserved();
    assert!(reserved > baseline);
    finish(&mut query, StreamReadStep::default());
    let (_, report) = query.finish().unwrap();
    assert_eq!(budget.accounting().reserved(), reserved);
    drop(report);
    assert_eq!(budget.accounting().reserved(), baseline);
    let mut prior_charge = None;
    for length in [3, u64::MAX] {
        let query = ReaderSearch::new(b"aaa".as_slice(), request(), ByteLength::new(length), &pattern,
            options, &budget, allocation(2)).unwrap()
            .with_context(StreamContextOptions::default(), &budget, allocation(3)).unwrap();
        let charge = budget.accounting().reserved();
        if let Some(prior) = prior_charge { assert_eq!(charge, prior, "source length never changes context admission"); }
        prior_charge = Some(charge);
        drop(query);
        assert_eq!(budget.accounting().reserved(), baseline);
    }
}

#![forbid(unsafe_code)]
#![cfg(feature = "search")]

use std::{io::{self, Read}, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{CaptureRequest, CompleteCapture, ParsedQuery, QueryGeneration, QueryOptions,
    ReaderSearch, ResourceAllocationId, ResourceBudget, SearchMode, StreamReadError,
    StreamReadOptions, StreamReadState, StreamReadStep, StreamingNeedle, UnicodeNormalization};
use fcb::search::oracle::{ReferenceScanOracle, SearchDocument};
use fcb::search::streaming::STREAM_BUFFER_BYTES;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1621).unwrap() }
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
        assert!(query.is_expression());
        assert!(query.hits().is_empty(), "candidate witnesses must remain private");
        assert_eq!(query.matches_seen(), 0);
        assert_eq!(query.expression_qualified(), None);
        query.step(step, query.generation(), || false).unwrap();
        assert!(query.stats().last_step_read <= step.max_bytes.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_scanned <= step.max_bytes.min(STREAM_BUFFER_BYTES));
        assert!(query.stats().last_step_calls <= step.max_calls.min(32));
    }
    panic!("expression did not terminate");
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn expressions_match_the_capture_oracle_with_one_observation_across_encodings_and_quanta() {
    let text = "banana café 😀\r\nANA İ K \u{feff} required phrase";
    let mut bom = vec![0xef, 0xbb, 0xbf]; bom.extend_from_slice(text.as_bytes());
    for bytes in [text.as_bytes().to_vec(), bom, utf16(text, true), utf16(text, false)] {
        for raw in ["ana required -forbidden", "ana absent -forbidden", "ana required -café",
            "\"café 😀\" \"required phrase\" -missing", "k \"i\u{0307}\" -missing",
            "\u{feff} required", "ana AND", "ana path:src/ lang:rust"] {
            let parsed = ParsedQuery::parse(raw).unwrap();
            for case_sensitive in [true, false] {
                let budget = budget();
                let pattern = StreamingNeedle::expression(owner(), &parsed, case_sensitive, &budget, allocation(1)).unwrap();
                let capture = CompleteCapture::new(request(), ByteLength::new(bytes.len() as u64),
                    Arc::from(bytes.as_slice())).unwrap();
                let options = QueryOptions::new(generation(1)).with_mode(SearchMode::DecodedText {
                    case_sensitive, normalization: UnicodeNormalization::Exact,
                });
                let document = SearchDocument::new(request().file(), "src/example.rs", &capture);
                let expected = ReferenceScanOracle::scan_document_counting_with_cancel(
                    &document, &parsed, &options, || false).unwrap();
                assert!(expected.is_complete());
                for chunk in [1, 3, 7, STREAM_BUFFER_BYTES] {
                    for quantum in [1, STREAM_BUFFER_BYTES] {
                        for limit in [0, 1, 3] {
                            let mut options = StreamReadOptions::new(generation(1)); options.max_matches = limit;
                            let input = Chunks { remaining: &bytes, chunk, calls: 0 };
                            let mut query = ReaderSearch::new(input, request(), ByteLength::new(bytes.len() as u64),
                                &pattern, options, &budget, allocation(2)).unwrap();
                            finish(&mut query, StreamReadStep { max_bytes: quantum, max_calls: 1, max_hits: 1 });
                            let (input, report) = query.finish().unwrap();
                            assert!(report.input_complete(), "{raw}: {:?}", report.state());
                            assert!(report.is_expression());
                            assert!(!report.count_all_matches());
                            assert_eq!(report.expression_qualified(), Some(!expected.matches.is_empty()));
                            assert_eq!(report.matches_seen(), expected.match_count() as u64);
                            assert_eq!(report.hits_truncated(), expected.match_count() > limit);
                            assert_eq!(report.stats().bytes_read, bytes.len() as u64);
                            assert_eq!(report.stats().read_calls, input.calls as u64);
                            assert!(input.remaining.is_empty(), "all predicates consume the same forward reader");
                            let retained = expected.matches.len().min(limit);
                            assert_eq!(report.hits().len(), retained);
                            for (index, hit) in report.hits().iter().enumerate() {
                                assert_eq!(hit.original_range(), expected.matches[index].original_byte_range);
                                assert_eq!(hit.occurrence_id(), index as u64 + 1);
                                assert_eq!(report.matched_text(index).unwrap(), expected.matches[index].matched_text);
                                let (start, end) = hit.original_range().as_usize_bounds().unwrap();
                                assert_eq!(report.witness_bytes(index).unwrap(), &bytes[start..end]);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn late_predicates_are_checked_after_a_dense_primary_page_fills_without_source_sized_storage() {
    let mut bytes = b"ana ".repeat(STREAM_BUFFER_BYTES);
    bytes.extend_from_slice(b"required");
    for forbidden_suffix in [false, true] {
        let mut source = bytes.clone();
        if forbidden_suffix { source.extend_from_slice(b" forbidden"); }
        for count_all in [false, true] {
            let budget = budget();
            let parsed = ParsedQuery::parse("ana required -forbidden").unwrap();
            let pattern = StreamingNeedle::expression(owner(), &parsed, true, &budget, allocation(1)).unwrap();
            let mut options = StreamReadOptions::new(generation(1)); options.max_matches = 2;
            let input = Chunks { remaining: &source, chunk: STREAM_BUFFER_BYTES, calls: 0 };
            let mut query = ReaderSearch::new_page(input, request(), ByteLength::new(source.len() as u64),
                &pattern, options, 1, count_all, &budget, allocation(2)).unwrap();
            let charge = budget.accounting().reserved();
            finish(&mut query, StreamReadStep::default());
            let (_, report) = query.finish().unwrap();
            assert_eq!(budget.accounting().reserved(), charge);
            assert!(report.input_complete());
            assert_eq!(report.count_all_matches(), count_all);
            assert_eq!(report.expression_qualified(), Some(!forbidden_suffix));
            assert_eq!(report.stats().bytes_read, source.len() as u64);
            assert!(report.stats().peak_buffer_bytes <= STREAM_BUFFER_BYTES);
            if forbidden_suffix {
                assert_eq!(report.matches_seen(), 0);
                assert!(report.hits().is_empty());
                assert!(!report.hits_truncated());
                assert_eq!(report.retained_witness_bytes(), 0);
            } else {
                assert_eq!(report.matches_seen(), STREAM_BUFFER_BYTES as u64);
                assert_eq!(report.hits().len(), 2);
                assert_eq!(report.hits()[0].occurrence_id(), 2);
                assert_eq!(report.hits()[0].original_range().start().get(), 4);
                assert_eq!(report.skipped_matches(), 1);
                assert!(report.has_more_matches());
                assert!(report.hits_truncated());
                assert_eq!(report.retained_witness_bytes(), 3);
            }
        }
    }
}

#[test]
fn byte_call_and_short_read_limits_never_prove_expression_absence_or_publish_candidates() {
    let source = b"anchor anchor required";
    for case_sensitive in [true, false] {
        for (byte_limit, call_limit, declared_extra, state) in [
            (0, 100, 0, StreamReadState::ByteLimit),
            (13, 100, 0, StreamReadState::ByteLimit),
            (100, 2, 0, StreamReadState::CallLimit),
            (100, 100, 5, StreamReadState::ShortRead),
        ] {
            let budget = budget();
            let parsed = ParsedQuery::parse("anchor required -forbidden").unwrap();
            let pattern = StreamingNeedle::expression(owner(), &parsed, case_sensitive, &budget, allocation(1)).unwrap();
            let mut options = StreamReadOptions::new(generation(1));
            options.max_bytes = byte_limit; options.max_read_calls = call_limit; options.max_matches = 1;
            let input = Chunks { remaining: source, chunk: 6, calls: 0 };
            let mut query = ReaderSearch::new(input, request(), ByteLength::new(source.len() as u64 + declared_extra),
                &pattern, options, &budget, allocation(2)).unwrap();
            finish(&mut query, StreamReadStep { max_bytes: 6, max_calls: 1, max_hits: 1 });
            let (_, report) = query.finish().unwrap();
            assert_eq!(report.state(), state);
            assert!(!report.input_complete());
            assert_eq!(report.expression_qualified(), None);
            assert!(report.hits().is_empty());
            assert_eq!(report.matches_seen(), 0);
            assert_eq!(report.retained_witness_bytes(), 0);
            assert!(!report.hits_truncated());
        }
    }
}

#[test]
fn malformed_suffix_never_turns_an_unseen_exclusion_into_success() {
    for case_sensitive in [true, false] {
        let budget = budget();
        let parsed = ParsedQuery::parse("anchor required -forbidden").unwrap();
        let pattern = StreamingNeedle::expression(owner(), &parsed, case_sensitive, &budget, allocation(1)).unwrap();
        let source = b"anchor required\xff";
        let input = Chunks { remaining: source, chunk: 6, calls: 0 };
        let mut query = ReaderSearch::new(input, request(), ByteLength::new(source.len() as u64),
            &pattern, StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
        finish(&mut query, StreamReadStep::default());
        let (_, report) = query.finish().unwrap();
        assert_eq!(report.state(), StreamReadState::UnsupportedText);
        assert_eq!(report.expression_qualified(), None);
        assert!(report.unsupported_at().is_some());
        assert!(report.hits().is_empty());
        assert_eq!(report.matches_seen(), 0);
        assert!(report.witness_bytes(0).is_err());
    }
}

#[test]
fn exact_predicates_ignore_initial_bom_and_odd_utf16_candidates_but_find_later_aligned_terms() {
    for little in [true, false] {
        let odd = if little { "\u{6100}\0x" } else { "\0\u{6100}x" };
        for suffix in ["", "a", "\u{feff}"] {
            let source = utf16(&format!("{odd}{suffix}"), little);
            for raw in ["x a", "x -a", "x \u{feff}", "x -\u{feff}"] {
                let budget = budget();
                let parsed = ParsedQuery::parse(raw).unwrap();
                let pattern = StreamingNeedle::expression(owner(), &parsed, true, &budget, allocation(1)).unwrap();
                let input = Chunks { remaining: &source, chunk: 1, calls: 0 };
                let mut query = ReaderSearch::new(input, request(), ByteLength::new(source.len() as u64),
                    &pattern, StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
                finish(&mut query, StreamReadStep { max_bytes: 1, max_calls: 1, max_hits: 1 });
                let (_, report) = query.finish().unwrap();
                let expected = match raw {
                    "x a" => suffix == "a", "x -a" => suffix != "a",
                    "x \u{feff}" => suffix == "\u{feff}", _ => suffix != "\u{feff}",
                };
                assert_eq!(report.expression_qualified(), Some(expected), "{little}, {raw}, {suffix:?}");
                assert_eq!(report.hits().len(), usize::from(expected));
                assert!(report.input_complete());
            }
        }
    }
}

#[test]
fn cancel_and_stale_generation_hide_provisional_and_previously_qualified_results() {
    let budget = budget();
    let parsed = ParsedQuery::parse("anchor required -forbidden").unwrap();
    let pattern = StreamingNeedle::expression(owner(), &parsed, false, &budget, allocation(1)).unwrap();
    let source = b"anchor required";
    for stale in [false, true] {
        let mut query = ReaderSearch::new(source.as_slice(), request(), ByteLength::new(source.len() as u64),
            &pattern, StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
        query.step(StreamReadStep { max_bytes: 6, max_calls: 1, max_hits: 1 }, generation(1), || false).unwrap();
        assert_eq!(query.state(), StreamReadState::Pending);
        assert!(query.hits().is_empty()); assert_eq!(query.matches_seen(), 0);
        if stale {
            assert_eq!(query.step(StreamReadStep::default(), generation(2), || false), Err(StreamReadError::StaleGeneration));
        } else {
            finish(&mut query, StreamReadStep::default());
            assert_eq!(query.expression_qualified(), Some(true));
            assert_eq!(query.hits().len(), 1);
            query.cancel();
        }
        assert_eq!(query.expression_qualified(), None);
        assert!(query.hits().is_empty()); assert_eq!(query.matches_seen(), 0);
        let (_, report) = query.finish().unwrap();
        assert_eq!(report.state(), StreamReadState::Canceled);
        assert_eq!(report.expression_qualified(), None);
        assert!(report.hits().is_empty());
        assert_eq!(report.retained_witness_bytes(), 0);
    }
}

struct FailsAfterAnchor(bool);
impl Read for FailsAfterAnchor {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.0 { return Err(io::Error::other("injected read failure")); }
        self.0 = true;
        output[..6].copy_from_slice(b"anchor");
        Ok(6)
    }
}
#[test]
fn read_failure_discards_private_anchor_witnesses() {
    let budget = budget();
    let parsed = ParsedQuery::parse("anchor -forbidden").unwrap();
    let pattern = StreamingNeedle::expression(owner(), &parsed, true, &budget, allocation(1)).unwrap();
    let mut query = ReaderSearch::new(FailsAfterAnchor(false), request(), ByteLength::new(100),
        &pattern, StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    query.step(StreamReadStep::default(), generation(1), || false).unwrap();
    assert!(query.hits().is_empty());
    assert_eq!(query.step(StreamReadStep::default(), generation(1), || false), Err(StreamReadError::Io));
    let (_, report) = query.finish().unwrap();
    assert_eq!(report.state(), StreamReadState::Failed(StreamReadError::Io));
    assert_eq!(report.expression_qualified(), None);
    assert!(report.hits().is_empty()); assert_eq!(report.matches_seen(), 0);
}

#[test]
fn empty_observations_reject_the_primary_and_public_ast_mutation_cannot_bypass_parser_limits() {
    let budget = budget();
    let parsed = ParsedQuery::parse("anchor required -forbidden").unwrap();
    let pattern = StreamingNeedle::expression(owner(), &parsed, true, &budget, allocation(1)).unwrap();
    let query = ReaderSearch::new(&b""[..], request(), ByteLength::new(0), &pattern,
        StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    let (_, report) = query.finish().unwrap();
    assert!(report.input_complete());
    assert_eq!(report.expression_qualified(), Some(false));
    assert_eq!(report.matches_seen(), 0);
    drop(report); drop(pattern);
    assert_eq!(budget.accounting().reserved().get(), 0);
    for mutation in 0..4 {
        let mut forged = parsed.clone();
        match mutation {
            0 => forged.primary_needle = "different".into(),
            1 => forged.exclusion_terms.clear(),
            2 => forged.conjunction_terms.push("extra".into()),
            _ => forged.raw_query = "x".repeat(fcb::search::MAX_QUERY_LEN + 1),
        }
        assert!(matches!(StreamingNeedle::expression(owner(), &forged, true, &budget, allocation(1)),
            Err(StreamReadError::InvalidExpression)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(StreamingNeedle::expression(owner(), &parsed, true, &tiny, allocation(1)),
        Err(StreamReadError::ResourceDenied)));
    assert_eq!(tiny.accounting().reserved().get(), 0);
}

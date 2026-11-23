#![forbid(unsafe_code)]

//! Production capture predicates share one decoder pass. The independent
//! per-term oracle remains a differential reference with unlimited work.
//! These source fixtures require the independent strict-RCH verification lane.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_search::{CaptureQueryScanner, EphemeralIndex, IndexError, IndexLimits,
    IndexedQuery, ManifestLimits, MembershipState, ParsedQuery, QueryError, QueryOptions,
    ReferenceScanOracle, SearchCoverage, SearchDocument, SearchManifest, SearchManifestId,
    SearchMode, SearchResult, UnicodeNormalization};
use fcb_source::{CaptureRequest, CompleteCapture};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(619).unwrap() }
fn allocation(value: u64) -> ResourceAllocationId { ResourceAllocationId::new(value).unwrap() }
fn options() -> QueryOptions {
    QueryOptions::new(QueryGeneration::new(owner(), 1).unwrap()).with_max_matches(16)
}
fn capture(bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 7).unwrap()).unwrap(), ByteLength::new(bytes.len() as u64),
        Arc::from(bytes)).unwrap()
}
fn budget(bytes: u64) -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(bytes)).unwrap() }
fn utf16(text: &str, little: bool) -> Vec<u8> {
    "\u{feff}".encode_utf16().chain(text.encode_utf16())
        .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}
fn scan(bytes: &[u8], expression: &str, opts: &QueryOptions, counted: bool) -> SearchResult {
    let source = capture(bytes);
    let document = SearchDocument::new(source.request().file(), "src/example.rs", &source);
    let query = ParsedQuery::parse(expression).unwrap();
    if counted {
        CaptureQueryScanner::scan_document_counting_with_cancel(&document, &query, opts, || false).unwrap()
    } else {
        CaptureQueryScanner::scan_document(&document, &query, opts).unwrap()
    }
}
fn differential(bytes: &[u8], expression: &str, mut opts: QueryOptions, counted: bool) -> SearchResult {
    let source = capture(bytes);
    let document = SearchDocument::new(source.request().file(), "src/example.rs", &source);
    let query = ParsedQuery::parse(expression).unwrap();
    opts.max_bytes_scanned = None;
    let expected = if counted {
        ReferenceScanOracle::scan_document_counting_with_cancel(&document, &query, &opts, || false).unwrap()
    } else {
        ReferenceScanOracle::scan_document(&document, &query, &opts).unwrap()
    };
    opts.max_bytes_scanned = Some(bytes.len() as u64);
    let actual = if counted {
        CaptureQueryScanner::scan_document_counting_with_cancel(&document, &query, &opts, || false).unwrap()
    } else {
        CaptureQueryScanner::scan_document(&document, &query, &opts).unwrap()
    };
    ReferenceScanOracle::verify_oracle_match(&actual, &expected).unwrap();
    assert!(actual.scanned_bytes <= bytes.len() as u64);
    actual
}

#[test]
fn exact_source_allowance_proves_all_predicates_and_preserves_primary_overlap() {
    let bytes = b"banana required required ready";
    let expression = "ana required ready -blocked -forbidden lang:rust path:src/";
    let actual = differential(bytes, expression, options(), false);
    assert!(actual.is_complete());
    assert_eq!(actual.scanned_bytes, bytes.len() as u64);
    assert_eq!(actual.match_count(), 2);
    assert_eq!(actual.matches.iter().map(|hit| hit.original_byte_range.start().get())
        .collect::<Vec<_>>(), [1, 3]);
    assert_eq!(actual.matches.iter().map(|hit| hit.occurrence_id).collect::<Vec<_>>(), [1, 2]);

    // Negative control: preserve the reference's deliberately separate passes.
    let source = capture(bytes);
    let document = SearchDocument::new(source.request().file(), "src/example.rs", &source);
    let query = ParsedQuery::parse(expression).unwrap();
    let mut limited = options(); limited.max_bytes_scanned = Some(bytes.len() as u64);
    let reference = ReferenceScanOracle::scan_document(&document, &query, &limited).unwrap();
    assert!(!reference.is_complete());
    assert!(reference.matches.is_empty());
}

#[test]
fn utf8_utf16_and_surrogate_window_boundaries_keep_original_spans() {
    for prefix in [0, 8190, 16383] {
        let text = format!("{}🙂needle required needle", "x".repeat(prefix));
        for bytes in [text.as_bytes().to_vec(), utf16(&text, true), utf16(&text, false)] {
            let result = differential(&bytes, "\"🙂needle\" required -absent", options(), true);
            assert!(result.is_complete());
            assert_eq!(result.match_count(), 1);
            assert_eq!(result.scanned_bytes, bytes.len() as u64);
            assert_eq!(result.matches[0].matched_text, "🙂needle");
        }
    }
}

#[test]
fn normalized_predicates_reuse_scalar_semantics_and_do_not_collapse_expansion_hits() {
    for (text, expression, normalization, count) in [
        ("İ READY", "i ready -blocked", UnicodeNormalization::Exact, 1),
        ("ßß READY", "s ready -blocked", UnicodeNormalization::CaseFold, 4),
        ("Ée\u{0301} READY", "é ready -blocked", UnicodeNormalization::Canonical, 2),
    ] {
        let opts = options().with_mode(SearchMode::DecodedText { case_sensitive: false, normalization });
        for bytes in [text.as_bytes().to_vec(), utf16(text, true), utf16(text, false)] {
            let result = differential(&bytes, expression, opts.clone(), true);
            assert!(result.is_complete());
            assert_eq!(result.match_count(), count);
            if count == 4 {
                assert_eq!(result.matches[0].original_byte_range, result.matches[1].original_byte_range);
                assert_ne!(result.matches[0].occurrence_id, result.matches[1].occurrence_id);
                assert!(result.matches.iter().all(|hit| hit.matched_text == "ß" && hit.multiplicity == 1));
            }
        }
    }
}

#[test]
fn row_limits_do_not_skip_late_predicates_or_inflate_noncounting_lookahead() {
    let text = format!("needle needle needle {} required", "x".repeat(40_000));
    for mode in [SearchMode::RawBytes, SearchMode::DecodedText {
        case_sensitive: true, normalization: UnicodeNormalization::Exact },
        SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::Exact }] {
        for counted in [false, true] {
            for rows in [0, 1, 3, 4] {
                let opts = options().with_mode(mode).with_max_matches(rows);
                let result = differential(text.as_bytes(), "needle required -blocked", opts, counted);
                assert_eq!(result.matches.len(), rows.min(3));
                assert_eq!(result.match_count(), if counted { 3 } else if rows == 0 { 0 } else { 3.min(rows + 1) });
                assert_eq!(result.is_complete(), counted || rows >= 3);
                if counted || rows > 0 { assert_eq!(result.scanned_bytes, text.len() as u64); }
            }
        }
        let rejected = format!("{text} blocked");
        let result = differential(rejected.as_bytes(), "needle required -blocked",
            options().with_mode(mode).with_max_matches(1), false);
        assert!(result.is_complete()); assert!(result.matches.is_empty());
        assert_eq!(result.match_count(), 0);
    }
}

#[test]
fn incomplete_predicates_never_publish_tentative_hits() {
    let text = format!("needle {} required", "x".repeat(40_000));
    for expression in ["needle required", "needle -blocked", "needle required -blocked"] {
        for bytes in [text.as_bytes().to_vec(), utf16(&text, true), utf16(&text, false)] {
            for allowance in [0, 1, 2, 7, 16384] {
                let mut opts = options(); opts.max_bytes_scanned = Some(allowance);
                let result = scan(&bytes, expression, &opts, true);
                assert_eq!(result.coverage, SearchCoverage::BudgetExhausted { bytes_scanned: allowance });
                assert!(result.matches.is_empty()); assert_eq!(result.match_count(), 0);
                assert!(result.unsupported_files.is_empty());
            }
        }
    }
}

#[test]
fn observed_exclusions_do_not_complete_budget_limited_decoded_searches() {
    let text = format!("needle needle required blocked {}🙂", "x".repeat(20_000));
    let query = ParsedQuery::parse("needle required -blocked").unwrap();
    for bytes in [text.as_bytes().to_vec(), utf16(&text, true), utf16(&text, false)] {
        let source = capture(&bytes);
        let document = SearchDocument::new(source.request().file(), "source.rs", &source);
        for (case_sensitive, normalization) in [(true, UnicodeNormalization::Exact),
            (false, UnicodeNormalization::Exact), (false, UnicodeNormalization::CaseFold)] {
            for allowance in [64, 16_384, bytes.len() as u64 - 1] {
                let mut opts = options().with_max_matches(1)
                    .with_mode(SearchMode::DecodedText { case_sensitive, normalization });
                opts.max_bytes_scanned = Some(allowance);
                // The exclusion is in the admitted prefix in every encoding.
                // The suffix still needs validation, including an allowance
                // ending inside the final UTF-8 scalar or UTF-16 surrogate.
                for counted in [false, true] {
                    let ordinary = scan(&bytes, "needle required -blocked", &opts, counted);
                    let window = CaptureQueryScanner::scan_document_window_with_cancel(
                        &document, &query, &opts, 1, counted, || false).unwrap();
                    for result in [ordinary, window] {
                        assert_eq!(result.coverage,
                            SearchCoverage::BudgetExhausted { bytes_scanned: allowance });
                        assert_eq!(result.scanned_bytes, allowance);
                        assert!(!result.is_complete());
                        assert!(result.matches.is_empty());
                        assert_eq!(result.match_count(), 0);
                        assert!(result.unsupported_files.is_empty());
                    }
                }
            }
            let opts = options().with_mode(SearchMode::DecodedText { case_sensitive, normalization });
            let complete = CaptureQueryScanner::scan_document_counting_with_cancel(
                &document, &query, &opts, || false).unwrap();
            assert!(complete.is_complete());
            assert_eq!(complete.scanned_bytes, bytes.len() as u64);
            assert!(complete.matches.is_empty());
            assert_eq!(complete.match_count(), 0);
        }
    }
}

#[test]
fn raw_exclusion_still_proves_a_negative_without_decoding_the_suffix() {
    let mut bytes = format!("needle blocked {}", "x".repeat(20_000)).into_bytes();
    bytes.push(0xff);
    let mut opts = options().with_mode(SearchMode::RawBytes).with_max_matches(1);
    opts.max_bytes_scanned = Some(64);
    for counted in [false, true] {
        let result = scan(&bytes, "needle -blocked", &opts, counted);
        assert!(result.is_complete());
        assert!(result.scanned_bytes <= 64 && result.scanned_bytes < bytes.len() as u64);
        assert!(result.matches.is_empty());
        assert_eq!(result.match_count(), 0);
        assert!(result.unsupported_files.is_empty());
    }
}

#[test]
fn indexed_excluded_documents_keep_verification_budget_failure() {
    let text = format!("needle blocked {}", "x".repeat(20_000));
    let query = ParsedQuery::parse("needle -blocked").unwrap();
    for bytes in [text.as_bytes().to_vec(), utf16(&text, true), utf16(&text, false)] {
        let source = capture(&bytes);
        let documents = [SearchDocument::new(source.request().file(), "source.rs", &source)];
        for max_source_bytes_per_file in [0, 65_536] {
            let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(),
                &documents, &[], MembershipState::Closed, ManifestLimits::default()).unwrap();
            let index_budget = budget(8 * 1024 * 1024);
            let index = EphemeralIndex::build(manifest, IndexLimits {
                max_source_bytes_per_file, ..IndexLimits::default()
            }, &index_budget, allocation(1), || false).unwrap();
            let output_budget = budget(128 * 1024);
            let mut opts = options().with_max_matches(1);
            opts.max_bytes_scanned = Some(16_384);
            for counted in [false, true] {
                let report = if counted {
                    index.search_counting(&query, opts.clone(), &output_budget, allocation(2), || false)
                } else {
                    index.search(&query, opts.clone(), &output_budget, allocation(2), || false)
                }.unwrap();
                assert!(report.is_terminal());
                assert_eq!(report.examined_files(), 1);
                assert_eq!(report.scan_attempts(), 1);
                assert!(!report.is_complete());
                let result = report.capture_results();
                assert_eq!(result.coverage, SearchCoverage::BudgetExhausted { bytes_scanned: 16_384 });
                assert_eq!(result.scanned_bytes, 16_384);
                assert!(result.matches.is_empty());
                assert_eq!(result.match_count(), 0);
                assert!(result.unsupported_files.is_empty());
            }
        }
    }
}

#[test]
fn malformed_suffix_invalidates_tentative_matches_even_after_row_storage_fills() {
    let mut bytes = format!("needle needle required {}", "x".repeat(40_000)).into_bytes();
    bytes.push(0xff);
    for counted in [false, true] {
        let result = differential(&bytes, "needle required -blocked", options().with_max_matches(1), counted);
        assert!(result.matches.is_empty()); assert_eq!(result.match_count(), 0);
        assert_eq!(result.unsupported_files, [FileId::new(owner(), 1).unwrap()]);
        assert!(!result.is_complete());
    }
    let mut excluded = format!("needle blocked required {}", "x".repeat(40_000)).into_bytes();
    excluded.push(0xff);
    let unavailable = differential(&excluded, "needle required -blocked", options(), true);
    assert!(unavailable.matches.is_empty());
    assert!(!unavailable.is_complete());
    assert_eq!(unavailable.unsupported_files, [FileId::new(owner(), 1).unwrap()]);

    // The independent oracle stops its exclusion probe after a second hit.
    // Production deliberately finishes a complete decoded pass even then: a
    // malformed suffix remains unavailable rather than a decoded negative.
    let mut twice = format!("needle blocked blocked required {}", "x".repeat(40_000)).into_bytes();
    twice.push(0xff);
    let source = capture(&twice);
    let document = SearchDocument::new(source.request().file(), "src/example.rs", &source);
    let query = ParsedQuery::parse("needle required -blocked").unwrap();
    let reference = ReferenceScanOracle::scan_document(&document, &query, &options()).unwrap();
    assert!(reference.is_complete()); assert!(reference.matches.is_empty());
    let production = CaptureQueryScanner::scan_document(&document, &query, &options()).unwrap();
    assert!(!production.is_complete()); assert!(production.matches.is_empty());
    assert_eq!(production.unsupported_files, [source.request().file()]);
}

#[test]
fn raw_expressions_preserve_byte_mode_through_nontext_source() {
    let bytes = b"\xff banana required \xfe banana";
    let result = differential(bytes, "ana required -blocked", options().with_mode(SearchMode::RawBytes), true);
    assert_eq!(result.match_count(), 4);
    assert!(result.is_complete()); assert!(result.unsupported_files.is_empty());
    assert!(result.matches.iter().all(|hit| hit.decoded_range.is_none()));
}

#[test]
fn cancellation_cannot_turn_unproved_exclusion_absence_into_eligibility() {
    let text = format!("needle required {}", "x".repeat(80_000));
    let source = capture(text.as_bytes());
    let document = SearchDocument::new(source.request().file(), "src/example.rs", &source);
    let query = ParsedQuery::parse("needle required -blocked").unwrap();
    for mode in [SearchMode::RawBytes, SearchMode::DecodedText {
        case_sensitive: false, normalization: UnicodeNormalization::Exact }] {
        let opts = options().with_mode(mode);
        let mut polls = 0;
        let result = CaptureQueryScanner::scan_document_counting_with_cancel(&document, &query, &opts,
            || { polls += 1; polls > 8 }).unwrap();
        assert_eq!(result.coverage, SearchCoverage::CanceledEarly);
        assert!(result.scanned_bytes <= text.len() as u64);
        assert!(result.matches.is_empty()); assert_eq!(result.match_count(), 0);
    }
}

#[test]
fn cancellation_during_final_predicate_window_cannot_publish_a_qualified_prefix() {
    let text = format!("needle required {} blocked", "x".repeat(17_000));
    for bytes in [text.as_bytes().to_vec(), utf16(&text, true)] {
        let source = capture(&bytes);
        let document = SearchDocument::new(source.request().file(), "src/example.rs", &source);
        let query = ParsedQuery::parse("needle required -blocked").unwrap();
        for case_sensitive in [true, false] {
            let opts = options().with_mode(SearchMode::DecodedText {
                case_sensitive, normalization: UnicodeNormalization::Exact });
            let mut checkpoints = 0;
            let complete = CaptureQueryScanner::scan_document_counting_with_cancel(&document, &query, &opts,
                || { checkpoints += 1; false }).unwrap();
            assert!(complete.is_complete()); assert!(complete.matches.is_empty());
            // Exercise every cancellation checkpoint, including the one between
            // source EOF and completion of the final predicate decoder window.
            for stop in 1..=checkpoints {
                let mut polls = 0;
                let result = CaptureQueryScanner::scan_document_counting_with_cancel(&document, &query, &opts,
                    || { polls += 1; polls == stop }).unwrap();
                assert_eq!(result.coverage, SearchCoverage::CanceledEarly);
                assert!(result.matches.is_empty(), "published prefix at checkpoint {stop}");
                assert_eq!(result.match_count(), 0);
            }
        }
    }
}

#[test]
fn indexed_and_fallback_verification_share_the_single_pass_allowance() {
    let source = capture(b"needle required needle ready");
    let documents = [SearchDocument::new(source.request().file(), "src/example.rs", &source)];
    let query = ParsedQuery::parse("needle required ready -blocked").unwrap();
    let expected = ReferenceScanOracle::scan_collection(&documents, &query, &options()).unwrap();
    for max_source_bytes_per_file in [0, 65_536] {
        let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), &documents, &[],
            MembershipState::Closed, ManifestLimits::default()).unwrap();
        let index_budget = budget(8 * 1024 * 1024);
        let index = EphemeralIndex::build(manifest, IndexLimits { max_source_bytes_per_file,
            ..IndexLimits::default() }, &index_budget, allocation(1), || false).unwrap();
        let output_budget = budget(128 * 1024);
        let mut opts = options(); opts.max_bytes_scanned = Some(source.bytes().len() as u64);
        let report = index.search(&query, opts, &output_budget, allocation(2), || false).unwrap();
        assert!(report.is_complete());
        assert_eq!(report.capture_results().scanned_bytes, source.bytes().len() as u64);
        ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected).unwrap();
        assert_eq!(report.fallback_attempts(), usize::from(max_source_bytes_per_file == 0));
    }
}

#[test]
fn simultaneous_predicate_capacity_is_admitted_before_index_execution() {
    let source = capture(b"needle required");
    let documents = [SearchDocument::new(source.request().file(), "src/example.rs", &source)];
    let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), &documents, &[],
        MembershipState::Closed, ManifestLimits::default()).unwrap();
    let index_budget = budget(8 * 1024 * 1024);
    let index = EphemeralIndex::build(manifest, IndexLimits::default(), &index_budget, allocation(1), || false).unwrap();
    // Public ASTs permit longer individual terms than the textual parser, up
    // to the shared needle limit. Charge their simultaneous cursor storage.
    let mut query = ParsedQuery::parse("needle required").unwrap();
    query.conjunction_terms = vec!["x".repeat(16_384); 4];
    let opts = options().with_mode(SearchMode::DecodedText {
        case_sensitive: false, normalization: UnicodeNormalization::Exact });
    let scratch = CaptureQueryScanner::predicate_scratch_bytes(&query, &opts).unwrap();
    assert!(scratch > 64 * 1024);
    assert_eq!(IndexedQuery::new(&index, &query, opts, &budget(64 * 1024), allocation(2)).err(),
        Some(IndexError::ResourceDenied));
    query.conjunction_terms[0].clear();
    assert_eq!(CaptureQueryScanner::scan_document(&documents[0], &query, &options()).err(), Some(QueryError::EmptyNeedle));
}

#[test]
fn capture_windows_keep_overlap_and_same_span_expansions_in_every_matching_route() {
    for (text, expression, mode) in [
        ("banana required", "ana required -blocked", SearchMode::RawBytes),
        ("banana required", "ana required -blocked", SearchMode::DecodedText {
            case_sensitive: true, normalization: UnicodeNormalization::Exact }),
        ("ßß required", "s required -blocked", SearchMode::DecodedText {
            case_sensitive: false, normalization: UnicodeNormalization::CaseFold }),
    ] {
        let mut variants = vec![text.as_bytes().to_vec()];
        if mode != SearchMode::RawBytes { variants.extend([utf16(text, true), utf16(text, false)]); }
        for bytes in variants {
            let source = capture(&bytes);
            let document = SearchDocument::new(source.request().file(), "source.rs", &source);
            let query = ParsedQuery::parse(expression).unwrap();
            let options = options().with_mode(mode);
            let reference = ReferenceScanOracle::scan_document(&document, &query, &options).unwrap();
            for offset in 0..=reference.matches.len() + 1 {
                let options = options.clone().with_max_matches(1);
                let actual = CaptureQueryScanner::scan_document_window_with_cancel(&document, &query,
                    &options, offset as u64, true, || false).unwrap();
                assert_eq!(actual.matches, reference.matches.iter().skip(offset).take(1).cloned().collect::<Vec<_>>());
                assert_eq!(actual.total_matches_counted, reference.total_matches_counted);
                assert!(actual.is_complete());
            }
        }
    }
}

#[test]
fn window_prefix_does_not_establish_predicate_eligibility_or_valid_text_suffix() {
    let prefix = "needle ".repeat(1200);
    for suffix in [" required blocked", " unrelated"] {
        let source = capture(format!("{prefix}{suffix}").as_bytes());
        let doc = SearchDocument::new(source.request().file(), "a.rs", &source);
        let query = ParsedQuery::parse("needle required -blocked").unwrap();
        let opts = options().with_max_matches(1);
        let result = CaptureQueryScanner::scan_document_window_with_cancel(&doc, &query, &opts, 1000, true, || false).unwrap();
        assert!(result.matches.is_empty()); assert_eq!(result.total_matches_counted, 0);
        assert!(result.is_complete());
    }
    let mut bytes = format!("{prefix} required").into_bytes(); bytes.push(0xff);
    let source = capture(&bytes);
    let doc = SearchDocument::new(source.request().file(), "a.rs", &source);
    let query = ParsedQuery::parse("needle required -blocked").unwrap();
    let result = CaptureQueryScanner::scan_document_window_with_cancel(&doc, &query,
        &options().with_max_matches(1), 1000, true, || false).unwrap();
    assert!(result.matches.is_empty()); assert_eq!(result.total_matches_counted, 0);
    assert!(!result.unsupported_files.is_empty());
}

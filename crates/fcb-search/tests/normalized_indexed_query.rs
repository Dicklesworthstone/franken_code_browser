#![forbid(unsafe_code)]

//! Normalized result admission must depend on the query, not capture size.
use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_search::{EphemeralIndex, IndexError, IndexLimits, IndexedQuery, ManifestLimits,
    MembershipState, ParsedQuery, QueryOptions, ReferenceScanOracle, SearchCoverage,
    SearchDocument, SearchManifest, SearchManifestId, SearchMode, UnicodeNormalization};
use fcb_search::index::export::SourceRetentionLimits;
use fcb_search::indexed_query::OwnedIndexedQuery;
use fcb_source::{CaptureRequest, CompleteCapture};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(91).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn budget(bytes: u64) -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(bytes)).unwrap() }
fn capture(file: u64, bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), file).unwrap(),
        SourceRevision::new(owner(), 7).unwrap()).unwrap(), ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn manifest<'a>(docs: &'a [SearchDocument<'a>]) -> SearchManifest<'a> {
    SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), docs, &[],
        MembershipState::Closed, ManifestLimits::default()).unwrap()
}
fn options(normalization: UnicodeNormalization) -> QueryOptions {
    QueryOptions::new(generation()).with_max_matches(4).with_mode(
        SearchMode::DecodedText { case_sensitive: false, normalization })
}
fn fallback_limits() -> IndexLimits {
    IndexLimits { max_source_bytes_per_file: 0, ..Default::default() }
}

#[test]
fn result_reservation_is_independent_of_largest_capture_and_releases_on_drop() {
    let query = ParsedQuery::parse("strasse").unwrap();
    let mut reservations = Vec::new();
    for prefix in [128, 1024 * 1024] {
        let text = format!("{}Straße", "x".repeat(prefix));
        let source = capture(1, text.as_bytes());
        let docs = [SearchDocument::new(source.request().file(), "large.rs", &source)];
        let index_budget = budget(8 * 1024 * 1024);
        let index = EphemeralIndex::build(manifest(&docs), fallback_limits(), &index_budget, allocation(1), || false).unwrap();
        // This is an output budget; the immutable input is already retained by
        // the caller. The former largest-capture-per-hit reservation fails it.
        let output_budget = budget(64 * 1024);
        let opts = options(UnicodeNormalization::CaseFold);
        let report = index.search(&query, opts.clone(), &output_budget, allocation(2), || false).unwrap();
        assert!(report.is_complete());
        assert_eq!(report.fallback_attempts(), 1);
        assert_eq!(report.capture_results().matches.len(), 1);
        let hit = &report.capture_results().matches[0];
        assert_eq!(hit.matched_text, "Straße");
        assert_eq!(hit.original_byte_range.start().get(), prefix as u64);
        assert_eq!(hit.original_byte_range.end().get(), text.len() as u64);
        assert_eq!(hit.revision, source.request().revision());
        reservations.push(report.reserved_output_bytes());
        drop(report);
        // Reusing the allocation proves that report drop returns its lease.
        let report = index.search(&query, opts, &output_budget, allocation(2), || false).unwrap();
        assert!(report.is_complete());
        assert_eq!(report.reserved_output_bytes(), *reservations.last().unwrap());
    }
    assert_eq!(reservations[0], reservations[1]);
    assert!(reservations[0] < 64 * 1024);
}

#[test]
fn owned_queries_share_the_query_sized_admission_and_progressive_verifier() {
    let text = format!("{}Straße", "x".repeat(1024 * 1024));
    let source = capture(1, text.as_bytes());
    let other = capture(2, b"unrelated");
    let docs = [SearchDocument::new(source.request().file(), "a.rs", &source),
        SearchDocument::new(other.request().file(), "b.rs", &other)];
    let index_budget = budget(16 * 1024 * 1024);
    let index = EphemeralIndex::build(manifest(&docs), fallback_limits(), &index_budget, allocation(1), || false).unwrap()
        .into_owned(SourceRetentionLimits::default(), &index_budget, allocation(2), || false).unwrap();
    let output_budget = budget(64 * 1024);
    let query = ParsedQuery::parse("strasse").unwrap();
    let opts = options(UnicodeNormalization::CaseFold);
    let expected = ReferenceScanOracle::scan_collection(&docs, &query, &opts).unwrap();
    let mut cursor = OwnedIndexedQuery::new(&index, query, opts, &output_budget,
        [allocation(3), allocation(4)], || false).unwrap();
    cursor.step(&index, 1, generation(), || false).unwrap();
    let early = cursor.report().capture_results().matches[0].clone();
    assert!(!cursor.report().is_complete());
    assert_eq!(cursor.report().examined_files(), 1);
    cursor.step(&index, 1, generation(), || false).unwrap();
    assert!(cursor.report().is_complete());
    assert_eq!(cursor.report().capture_results().matches[0], early);
    ReferenceScanOracle::verify_oracle_match(cursor.report().capture_results(), &expected).unwrap();
    assert!(cursor.report().reserved_output_bytes() < 64 * 1024);
}

#[test]
fn expanded_and_utf16_hits_fit_the_bound_without_collapsing_occurrences() {
    let raw: Vec<u8> = "\u{FEFF}K ßß Ée\u{0301}🙂".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let source = capture(1, &raw);
    let docs = [SearchDocument::new(source.request().file(), "utf16.rs", &source)];
    let index_budget = budget(8 * 1024 * 1024);
    let index = EphemeralIndex::build(manifest(&docs), IndexLimits::default(), &index_budget, allocation(1), || false).unwrap();
    let output_budget = budget(64 * 1024);
    for (needle, normalization, expected_count) in [("k", UnicodeNormalization::Exact, 1),
        ("s", UnicodeNormalization::CaseFold, 4), ("sss", UnicodeNormalization::CaseFold, 2),
        ("é", UnicodeNormalization::Canonical, 2)] {
        let query = ParsedQuery::parse(needle).unwrap();
        let opts = options(normalization);
        let expected = ReferenceScanOracle::scan_collection(&docs, &query, &opts).unwrap();
        let report = index.search(&query, opts, &output_budget, allocation(2), || false).unwrap();
        assert!(report.is_complete());
        assert_eq!(report.capture_results().match_count(), expected_count);
        ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected).unwrap();
        if needle == "s" {
            let hits = &report.capture_results().matches;
            assert_eq!(hits[0].original_byte_range, hits[1].original_byte_range);
            assert_eq!(hits[0].original_byte_range.start().get(), 6);
            assert_eq!(hits[0].original_byte_range.end().get(), 8);
            assert_ne!(hits[0].occurrence_id, hits[1].occurrence_id);
            assert!(hits.iter().all(|hit| hit.multiplicity == 1 && hit.matched_text == "ß"));
        }
    }
}

#[test]
fn normalized_predicates_and_partial_coverage_share_the_global_byte_budget() {
    let a = capture(1, "Straße ready".as_bytes());
    let b = capture(2, "STRASSE blocked".as_bytes());
    let docs = [SearchDocument::new(a.request().file(), "a.rs", &a),
        SearchDocument::new(b.request().file(), "b.rs", &b)];
    let index_budget = budget(8 * 1024 * 1024);
    let index = EphemeralIndex::build(manifest(&docs), fallback_limits(), &index_budget, allocation(1), || false).unwrap();
    let output_budget = budget(64 * 1024);
    for expression in ["strasse", "strasse ready", "strasse -blocked"] {
        let query = ParsedQuery::parse(expression).unwrap();
        for bytes in 0..=96 {
            let mut opts = options(UnicodeNormalization::CaseFold);
            opts.max_bytes_scanned = Some(bytes);
            let expected = ReferenceScanOracle::scan_collection(&docs, &query, &opts).unwrap();
            let report = index.search(&query, opts, &output_budget, allocation(2), || false).unwrap();
            assert_eq!(report.capture_results(), &expected, "query={expression}, budget={bytes}");
            assert!(report.capture_results().scanned_bytes <= bytes);
            if matches!(expected.coverage, SearchCoverage::BudgetExhausted { .. }) {
                assert!(!report.is_complete());
            }
        }
    }
}

#[test]
fn an_insufficient_output_budget_is_still_refused_without_poisoning_the_index() {
    let source = capture(1, "Straße".as_bytes());
    let docs = [SearchDocument::new(source.request().file(), "a.rs", &source)];
    let index_budget = budget(8 * 1024 * 1024);
    let index = EphemeralIndex::build(manifest(&docs), IndexLimits::default(), &index_budget, allocation(1), || false).unwrap();
    let query = ParsedQuery::parse("strasse").unwrap();
    let opts = options(UnicodeNormalization::CaseFold);
    assert_eq!(IndexedQuery::new(&index, &query, opts.clone(), &budget(1), allocation(2)).err(), Some(IndexError::ResourceDenied));
    let report = index.search(&query, opts, &budget(64 * 1024), allocation(2), || false).unwrap();
    assert!(report.is_complete());
    assert_eq!(report.capture_results().matches[0].matched_text, "Straße");
}

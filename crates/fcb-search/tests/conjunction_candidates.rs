#![forbid(unsafe_code)]

//! FCB-027.B: required-term certificates accelerate the production borrowed
//! and movable queries without changing exact occurrence or coverage semantics.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_search::{EphemeralIndex, IndexError, IndexLimits, IndexedQuery, IndexedQueryState,
    ManifestLimits, MembershipState, ParsedQuery, QueryError, QueryOptions,
    ReferenceScanOracle, SearchDocument, SearchManifest, SearchManifestId, SearchMode,
    UnicodeNormalization, MAX_QUERY_TOKENS};
use fcb_search::index::export::SourceRetentionLimits;
use fcb_search::indexed_query::OwnedIndexedQuery;
use fcb_source::{CaptureRequest, CompleteCapture, DetectedEncoding};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(29041).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn budget() -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap()
}
fn capture(id: u64, bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), id).unwrap(),
        SourceRevision::new(owner(), id).unwrap()).unwrap(),
        ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn document<'a>(path: &'a str, capture: &'a CompleteCapture) -> SearchDocument<'a> {
    SearchDocument::new(capture.request().file(), path, capture)
}
fn build_index<'a>(documents: &'a [SearchDocument<'a>], limits: IndexLimits,
    budget: &ResourceBudget, id: u64) -> EphemeralIndex<'a> {
    let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(),
        documents, &[], MembershipState::Closed, ManifestLimits::default()).unwrap();
    EphemeralIndex::build(manifest, limits, budget, allocation(id), || false).unwrap()
}
fn options() -> QueryOptions {
    QueryOptions::new(generation(1)).with_max_matches(100)
}
fn utf16(text: &str, little: bool, bom: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if bom { bytes.extend_from_slice(if little { &[0xff, 0xfe] } else { &[0xfe, 0xff] }); }
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn rare_conjunction_reaches_late_hits_with_only_the_matching_files_byte_budget() {
    let noise = b"anchor ".repeat(512);
    let target = b"anchor permit anchor";
    let mut captures: Vec<_> = (1..=64).map(|id| capture(id, &noise)).collect();
    captures.push(capture(65, target));
    let documents: Vec<_> = captures.iter().map(|capture| document("src/source.rs", capture)).collect();
    let budget = budget();
    let query = ParsedQuery::parse("anchor permit").unwrap();
    let expected = ReferenceScanOracle::scan_collection_counting_with_cancel(
        &documents, &query, &options(), || false).unwrap();
    assert_eq!(expected.match_count(), 2);
    let mut limits = options().with_max_matches(2);
    limits.max_bytes_scanned = Some(target.len() as u64);

    let index = build_index(&documents, IndexLimits::default(), &budget, 1);
    let retained = budget.accounting().reserved().get();
    let report = index.search_counting(&query, limits.clone(), &budget, allocation(2), || false).unwrap();
    ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected).unwrap();
    assert!(report.is_complete());
    assert_eq!(report.examined_files(), 65);
    assert_eq!(report.skipped_by_index(), 64);
    assert_eq!(report.scan_attempts(), 1);
    assert_eq!(report.capture_results().scanned_bytes, target.len() as u64);
    assert!(report.capture_results().matches.iter().all(|hit| hit.file_id.get() == 65));
    drop(report);
    assert_eq!(budget.accounting().reserved().get(), retained);

    let owned = index.into_owned(SourceRetentionLimits::default(), &budget, allocation(3), || false).unwrap();
    let statistics = owned.statistics();
    let mut cursor = OwnedIndexedQuery::new_counting(&owned, query.clone(), limits.clone(),
        &budget, [allocation(4), allocation(5)], || false).unwrap();
    let admitted = budget.accounting().reserved().get();
    for _ in 0..65 {
        cursor.step(&owned, 1, limits.generation, || false).unwrap();
        assert_eq!(budget.accounting().reserved().get(), admitted);
    }
    assert!(cursor.report().is_complete());
    assert_eq!(cursor.report().skipped_by_index(), 64);
    assert_eq!(cursor.report().scan_attempts(), 1);
    assert_eq!(owned.statistics(), statistics);
    ReferenceScanOracle::verify_oracle_match(cursor.report().capture_results(), &expected).unwrap();
    drop(cursor);
    drop(owned);
    assert_eq!(budget.accounting().reserved().get(), 0);

    // Negative control: without a valid segment certificate the SAME query
    // exhausts its byte quota on the first common-anchor file. A direct route
    // must not pretend that missing required terms were proved in its suffix.
    let uncovered = build_index(&documents, IndexLimits { max_total_grams: 0, ..Default::default() }, &budget, 10);
    let report = uncovered.search_counting(&query, limits, &budget, allocation(11), || false).unwrap();
    assert!(!report.is_complete());
    assert_eq!(report.skipped_by_index(), 0);
    assert_eq!(report.scan_attempts(), 1);
    assert!(report.capture_results().matches.is_empty());
}

#[test]
fn positives_short_terms_exclusions_and_false_positives_preserve_exact_occurrences() {
    let captures = [capture(1, b"banana allow abcXbcd"),
        capture(2, b"banana without permission"), capture(3, b"banana allow blocked")];
    let documents = [document("src/a.rs", &captures[0]), document("src/b.rs", &captures[1]),
        document("other/c.rs", &captures[2])];
    let budget = budget();
    let index = build_index(&documents, IndexLimits::default(), &budget, 1);
    let opts = options();
    // A short anchor still permits a certificate from a longer required term.
    assert!(!index.may_match(1, &ParsedQuery::parse("a allow").unwrap(), &opts).unwrap());
    // Short conjuncts cannot certify absence; exclusions/metadata are not
    // positive content needles. Existing exact filtering owns these decisions.
    for raw in ["ana z", "ana -missing", "ana -allow", "ana path:absent/ lang:python"] {
        assert!(index.may_match(0, &ParsedQuery::parse(raw).unwrap(), &opts).unwrap(), "{raw}");
    }
    // All grams can exist without the required literal being present.
    let false_positive = ParsedQuery::parse("ana abcd").unwrap();
    assert!(index.may_match(0, &false_positive, &opts).unwrap());
    for raw in ["ana allow", "a allow", "ana z", "ana abcd", "ana allow allow",
        "ana -missing", "ana -allow", "ana allow -blocked", "ana path:src/ lang:rust"] {
        let query = ParsedQuery::parse(raw).unwrap();
        for mode in [SearchMode::default(), SearchMode::RawBytes] {
            for limit in [0, 1, 2, 100] {
                let opts = options().with_mode(mode).with_max_matches(limit);
                let expected = ReferenceScanOracle::scan_collection(&documents, &query, &opts).unwrap();
                let report = index.search(&query, opts.clone(), &budget, allocation(2), || false).unwrap();
                ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected)
                    .unwrap_or_else(|error| panic!("{raw}, {mode:?}, limit {limit}: {error}"));
                drop(report);
                let expected = ReferenceScanOracle::scan_collection_counting_with_cancel(
                    &documents, &query, &opts, || false).unwrap();
                let report = index.search_counting(&query, opts, &budget, allocation(2), || false).unwrap();
                ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected)
                    .unwrap_or_else(|error| panic!("counting {raw}, {mode:?}, limit {limit}: {error}"));
            }
        }
    }
    let report = index.search(&ParsedQuery::parse("ana allow -blocked").unwrap(),
        opts, &budget, allocation(2), || false).unwrap();
    assert_eq!(report.capture_results().match_count(), 2);
    assert_eq!(report.capture_results().matches.iter().map(|hit| hit.original_byte_range.start().get())
        .collect::<Vec<_>>(), [1, 3]);
    assert_eq!(report.capture_results().matches.iter().map(|hit| hit.occurrence_id)
        .collect::<Vec<_>>(), [1, 2]);
    let mut wrong = report.capture_results().clone();
    wrong.matches.pop();
    assert!(ReferenceScanOracle::verify_oracle_match(&wrong, report.capture_results()).is_err());
}

#[test]
fn incompatible_decoders_normalization_and_malformed_sources_remain_candidates() {
    let exact = SearchMode::default();
    let insensitive = SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::Exact };
    let folded = SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::CaseFold };
    let canonical = SearchMode::DecodedText { case_sensitive: true, normalization: UnicodeNormalization::Canonical };
    let cases = [
        (utf16("banana allow", true, true), "ana allow", exact, None, 2, true),
        (utf16("banana allow", false, true), "ana allow", exact, None, 2, true),
        // BOM-less ASCII UTF-16 is also valid UTF-8 bytes. The declared decoder
        // must prevent using missing UTF-8 grams as a negative certificate.
        (utf16("banana allow", true, false), "ana allow", exact, Some(DetectedEncoding::Utf16Le), 2, true),
        (utf16("banana allow", false, false), "ana allow", exact, Some(DetectedEncoding::Utf16Be), 2, true),
        (b"BANANA ALLOW".to_vec(), "ana allow", insensitive, None, 2, true),
        ("banana Straße".as_bytes().to_vec(), "ana STRASSE", folded, None, 2, true),
        ("banana e\u{0301}".as_bytes().to_vec(), "ana é", canonical, None, 2, true),
        (b"banana\xff".to_vec(), "ana missing", exact, None, 0, false),
        (b"banana".to_vec(), "ana missing", exact, Some(DetectedEncoding::Unsupported), 0, false),
    ];
    for (bytes, raw, mode, encoding, count, complete) in cases {
        let capture = capture(1, &bytes);
        let documents = [document("source", &capture)];
        let budget = budget();
        let index = build_index(&documents, IndexLimits::default(), &budget, 1);
        let query = ParsedQuery::parse(raw).unwrap();
        let mut opts = options().with_mode(mode);
        opts.declared_encoding = encoding;
        assert!(index.may_match(0, &query, &opts).unwrap(), "{raw}, {mode:?}, {encoding:?}");
        let expected = ReferenceScanOracle::scan_collection(&documents, &query, &opts).unwrap();
        let report = index.search(&query, opts, &budget, allocation(2), || false).unwrap();
        ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected).unwrap();
        assert_eq!(report.is_complete(), complete);
        assert_eq!(report.capture_results().match_count(), count);
        assert_eq!(report.skipped_by_index(), 0);
        assert_eq!(report.scan_attempts(), 1);
        assert_eq!(report.fallback_attempts(), 1);
        if !complete { assert_eq!(report.capture_results().unsupported_files, [capture.request().file()]); }
    }
}

#[test]
fn quota_refusals_preserve_missing_conjunction_files_and_release_reservations() {
    let captures = [capture(1, b"banana allow"), capture(2, b"banana without permission")];
    let documents = [document("a.rs", &captures[0]), document("b.rs", &captures[1])];
    let query = ParsedQuery::parse("ana allow").unwrap();
    let expected = ReferenceScanOracle::scan_collection(&documents, &query, &options()).unwrap();
    for limits in [
        IndexLimits { max_source_bytes_per_file: 1, ..Default::default() },
        IndexLimits { max_scratch_bytes: 0, ..Default::default() },
        IndexLimits { max_grams_per_file: 0, ..Default::default() },
        IndexLimits { max_total_grams: 0, ..Default::default() },
        IndexLimits { max_source_bytes_total: 0, ..Default::default() },
    ] {
        let budget = budget();
        let index = build_index(&documents, limits, &budget, 1);
        let retained = budget.accounting().reserved().get();
        assert_eq!(index.statistics().uncovered_files, 2);
        assert!(index.may_match(1, &query, &options()).unwrap());
        let report = index.search(&query, options(), &budget, allocation(2), || false).unwrap();
        assert!(report.is_complete());
        assert_eq!(report.skipped_by_index(), 0);
        assert_eq!(report.scan_attempts(), 2);
        assert_eq!(report.fallback_attempts(), 2);
        ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected).unwrap();
        drop(report);
        assert_eq!(budget.accounting().reserved().get(), retained);
        drop(index);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn cancellation_after_a_conjunction_skip_keeps_later_candidates_unexamined() {
    let captures = [capture(1, b"banana without permission"), capture(2, b"banana allow")];
    let documents = [document("a.rs", &captures[0]), document("b.rs", &captures[1])];
    let budget = budget();
    let index = build_index(&documents, IndexLimits::default(), &budget, 1);
    let retained = budget.accounting().reserved().get();
    let query = ParsedQuery::parse("ana allow").unwrap();
    let mut cursor = IndexedQuery::new(&index, &query, options(), &budget, allocation(2)).unwrap();
    cursor.step(1, generation(1), || false).unwrap();
    assert_eq!(cursor.report().skipped_by_index(), 1);
    assert_eq!(cursor.report().scan_attempts(), 0);
    assert_eq!(cursor.report().capture_results().scanned_bytes, 0);
    cursor.step(1, generation(1), || true).unwrap();
    assert_eq!(cursor.report().state(), IndexedQueryState::Canceled);
    assert!(!cursor.report().is_complete());
    assert_eq!(cursor.report().examined_files(), 1);
    assert!(cursor.report().capture_results().matches.is_empty());
    drop(cursor);
    assert_eq!(budget.accounting().reserved().get(), retained);
}

#[test]
fn public_ast_cannot_expand_candidate_probe_work_beyond_the_token_limit() {
    let capture = capture(1, b"anchor");
    let documents = [document("source", &capture)];
    let budget = budget();
    let index = build_index(&documents, IndexLimits::default(), &budget, 1);
    let mut query = ParsedQuery::parse("anchor").unwrap();
    query.conjunction_terms = vec!["anchor".to_owned(); MAX_QUERY_TOKENS - 1];
    query.conjunction_terms.push("missing".to_owned());
    // The extra term falls outside the bounded certificate. It can produce
    // extra verification, never a false negative or an unbounded probe loop.
    assert!(index.may_match(0, &query, &options()).unwrap());
    assert!(matches!(index.search(&query, options(), &budget, allocation(2), || false),
        Err(IndexError::Query(QueryError::LimitExceeded))));
    query.conjunction_terms[MAX_QUERY_TOKENS - 2] = "missing".to_owned();
    assert!(!index.may_match(0, &query, &options()).unwrap());
}

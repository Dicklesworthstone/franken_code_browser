#![forbid(unsafe_code)]

//! FCB-027 public production route: closed captures -> admitted segments ->
//! progressive exact verification -> source-range navigation. No DB or runtime.

use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_search::{
    EphemeralIndex, IndexError, IndexLimits, IndexedQuery, IndexedQueryState,
    ManifestLimits, MembershipState, ParsedQuery, QueryError, QueryOptions,
    ReferenceScanOracle, SearchCoverage, SearchDocument, SearchManifest,
    SearchManifestId, SearchMode, SegmentCoverage, UnicodeNormalization,
};
use fcb_source::{CaptureRequest, CompleteCapture, DetectedEncoding};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(77).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn options() -> QueryOptions { QueryOptions::new(generation(1)).with_max_matches(50) }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn capture(file: u64, revision: u64, bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), file).unwrap(),
        SourceRevision::new(owner(), revision).unwrap()).unwrap(),
        ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn document<'a>(path: &'a str, capture: &'a CompleteCapture) -> SearchDocument<'a> {
    SearchDocument::new(capture.request().file(), path, capture)
}
fn manifest<'a>(docs: &'a [SearchDocument<'a>]) -> SearchManifest<'a> {
    SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), docs, &[],
        MembershipState::Closed, ManifestLimits::default()).unwrap()
}
fn index<'a>(docs: &'a [SearchDocument<'a>], limits: IndexLimits, budget: &ResourceBudget) -> EphemeralIndex<'a> {
    EphemeralIndex::build(manifest(docs), limits, budget, allocation(1), || false).unwrap()
}
fn utf16(text: &str, le: bool, bom: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if bom { bytes.extend_from_slice(if le { &[0xff, 0xfe] } else { &[0xfe, 0xff] }); }
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if le { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn candidate_elimination_reduces_source_scans_without_changing_exact_results() {
    let first = capture(1, 1, b"unrelated source");
    let second = capture(2, 1, b"banana bandana");
    let third = capture(3, 1, b"also unrelated");
    let docs = [document("a.rs", &first), document("b.rs", &second), document("c.rs", &third)];
    let budget = budget();
    let index = index(&docs, IndexLimits::default(), &budget);
    let query = ParsedQuery::parse("ana").unwrap();
    let oracle = ReferenceScanOracle::scan_collection(&docs, &query, &options()).unwrap();
    let result = index.search(&query, options(), &budget, allocation(2), || false).unwrap();
    ReferenceScanOracle::verify_oracle_match(result.capture_results(), &oracle).unwrap();
    assert!(result.is_complete());
    assert_eq!(result.skipped_by_index(), 2);
    assert_eq!(result.scan_attempts(), 1);
    assert!(result.capture_results().scanned_bytes < oracle.scanned_bytes);
    assert_eq!(result.capture_results().matches[0].original_byte_range.start().get(), 1);
}

#[test]
fn a_candidate_with_all_probed_grams_is_not_misreported_as_a_hit() {
    let c = capture(1, 1, b"abcXbcd");
    let docs = [document("a.rs", &c)];
    let budget = budget();
    let index = index(&docs, IndexLimits::default(), &budget);
    let query = ParsedQuery::parse("abcd").unwrap();
    assert!(index.may_match(0, &query, &options()).unwrap());
    let result = index.search(&query, options(), &budget, allocation(2), || false).unwrap();
    assert!(result.is_complete());
    assert_eq!(result.scan_attempts(), 1);
    assert!(result.capture_results().matches.is_empty());
}

#[test]
fn indexed_and_direct_results_agree_across_modes_filters_and_result_limits() {
    let captures = [capture(1, 1, b"banana red banana"), capture(2, 1, b"banana blue"),
        capture(3, 1, b"red bandana"), capture(4, 1, b"unrelated")];
    let docs = [document("src/a.rs", &captures[0]), document("src/b.py", &captures[1]),
        document("docs/c.md", &captures[2]), document("other.rs", &captures[3])];
    let budget = budget();
    let index = index(&docs, IndexLimits::default(), &budget);
    for mode in [SearchMode::RawBytes, SearchMode::default()] {
        for raw in ["ana", "a", "missing", "ana red", "ana -blue", "ana path:src/", "ana lang:rust", "ana -path:docs/"] {
            let query = ParsedQuery::parse(raw).unwrap();
            for limit in [0, 1, 2, 3, 4, 5, 20] {
                let options = options().with_mode(mode).with_max_matches(limit);
                let oracle = ReferenceScanOracle::scan_collection(&docs, &query, &options).unwrap();
                let result = index.search(&query, options, &budget, allocation(2), || false).unwrap();
                ReferenceScanOracle::verify_oracle_match(result.capture_results(), &oracle)
                    .unwrap_or_else(|error| panic!("{raw:?}, limit={limit}, mode={mode:?}: {error}"));
            }
        }
    }
}

#[test]
fn every_quota_route_searches_the_uncovered_source_instead_of_excluding_it() {
    let c = capture(1, 1, b"abcdefghi needle needle");
    let docs = [document("a.rs", &c)];
    let query = ParsedQuery::parse("needle").unwrap();
    let oracle = ReferenceScanOracle::scan_collection(&docs, &query, &options()).unwrap();
    for limits in [
        IndexLimits { max_source_bytes_per_file: 2, ..Default::default() },
        IndexLimits { max_scratch_bytes: 0, ..Default::default() },
        IndexLimits { max_grams_per_file: 1, ..Default::default() },
        IndexLimits { max_total_grams: 1, ..Default::default() },
        IndexLimits { max_source_bytes_total: 1, ..Default::default() },
    ] {
        let budget = budget();
        let index = index(&docs, limits, &budget);
        assert!(matches!(index.segment_coverage(c.request().file()), Some(SegmentCoverage::Uncovered(_))));
        let result = index.search(&query, options(), &budget, allocation(2), || false).unwrap();
        assert_eq!(result.fallback_attempts(), 1);
        assert!(result.is_complete());
        ReferenceScanOracle::verify_oracle_match(result.capture_results(), &oracle).unwrap();
    }
}

#[test]
fn high_entropy_quota_failure_does_not_publish_a_partial_negative_certificate() {
    let mut bytes = Vec::new();
    let mut state = 0x1234_5678u32;
    for _ in 0..4096 {
        state ^= state << 13; state ^= state >> 17; state ^= state << 5;
        bytes.push((state % 95 + 32) as u8);
    }
    bytes.extend_from_slice(b"UNIQUE_END_MARKER");
    let c = capture(1, 1, &bytes);
    let docs = [document("generated.txt", &c)];
    let budget = budget();
    let index = index(&docs, IndexLimits { max_grams_per_file: 8, ..Default::default() }, &budget);
    assert_eq!(index.statistics().uncovered_files, 1);
    let query = ParsedQuery::parse("UNIQUE_END_MARKER").unwrap();
    let result = index.search(&query, options(), &budget, allocation(2), || false).unwrap();
    assert!(result.is_complete());
    let hit = &result.capture_results().matches[0];
    let (start, end) = hit.original_byte_range.as_usize_bounds().unwrap();
    assert_eq!(&c.bytes()[start..end], b"UNIQUE_END_MARKER");
}

#[test]
fn utf16_surrogate_and_window_boundaries_fall_back_to_exact_capture_verification() {
    let text = format!("{}\u{1f680}needle", "x".repeat(8190));
    for le in [false, true] {
        for bom in [false, true] {
            let bytes = utf16(&text, le, bom);
            let c = capture(1, 1, &bytes);
            let docs = [document("utf16.txt", &c)];
            let budget = budget();
            let index = index(&docs, IndexLimits::default(), &budget);
            let query = ParsedQuery::parse("\u{1f680}needle").unwrap();
            let options = options().with_encoding(if le { DetectedEncoding::Utf16Le } else { DetectedEncoding::Utf16Be });
            let oracle = ReferenceScanOracle::scan_collection(&docs, &query, &options).unwrap();
            let result = index.search(&query, options, &budget, allocation(2), || false).unwrap();
            assert!(result.is_complete());
            assert_eq!(result.fallback_attempts(), 1);
            ReferenceScanOracle::verify_oracle_match(result.capture_results(), &oracle).unwrap();
            let hit = &result.capture_results().matches[0];
            let (start, end) = hit.original_byte_range.as_usize_bounds().unwrap();
            assert_eq!(&bytes[start..end], utf16("\u{1f680}needle", le, false));
        }
    }
}

#[test]
fn unsupported_text_is_reported_even_when_the_byte_index_has_no_candidate() {
    let c = capture(1, 1, b"prefix\xffsuffix");
    let docs = [document("bad.rs", &c)];
    let budget = budget();
    let index = index(&docs, IndexLimits::default(), &budget);
    let query = ParsedQuery::parse("needle").unwrap();
    let result = index.search(&query, options(), &budget, allocation(2), || false).unwrap();
    assert!(!result.is_complete());
    assert_eq!(result.capture_results().unsupported_files, [c.request().file()]);
    assert_eq!(result.fallback_attempts(), 1);
}

#[test]
fn existing_normalized_modes_keep_occurrence_multiplicity_and_source_ranges() {
    let c = capture(1, 1, "Straße café cafe\u{301}".as_bytes());
    let docs = [document("unicode.rs", &c)];
    let budget = budget();
    let index = index(&docs, IndexLimits::default(), &budget);
    for (needle, normalization) in [("s", UnicodeNormalization::CaseFold),
        ("ss", UnicodeNormalization::CaseFold), ("café", UnicodeNormalization::Canonical)] {
        let query = ParsedQuery::parse(needle).unwrap();
        let options = options().with_mode(SearchMode::DecodedText { case_sensitive: false, normalization });
        let oracle = ReferenceScanOracle::scan_collection(&docs, &query, &options).unwrap();
        let result = index.search(&query, options, &budget, allocation(2), || false).unwrap();
        assert_eq!(result.fallback_attempts(), 1);
        ReferenceScanOracle::verify_oracle_match(result.capture_results(), &oracle).unwrap();
        if needle == "s" { assert!(result.capture_results().matches.iter().any(|hit| hit.multiplicity == 2)); }
        if needle == "café" { assert_eq!(result.capture_results().matches.len(), 2); }
    }
}

#[test]
fn one_global_byte_budget_covers_predicates_and_primary_scans() {
    let a = capture(1, 1, b"needle red");
    let b = capture(2, 1, b"needle blue");
    let docs = [document("a.rs", &a), document("b.rs", &b)];
    let budget = budget();
    // No candidates eliminated, so the byte accounting must match the direct route exactly.
    let index = index(&docs, IndexLimits { max_total_grams: 0, ..Default::default() }, &budget);
    for raw in ["needle", "needle red", "needle -blue"] {
        let query = ParsedQuery::parse(raw).unwrap();
        for bytes in 0..=64 {
            let mut options = options(); options.max_bytes_scanned = Some(bytes);
            let oracle = ReferenceScanOracle::scan_collection(&docs, &query, &options).unwrap();
            let result = index.search(&query, options, &budget, allocation(2), || false).unwrap();
            assert_eq!(result.capture_results(), &oracle, "query={raw}, budget={bytes}");
            assert!(result.capture_results().scanned_bytes <= bytes);
        }
    }
}

#[test]
fn a_full_result_buffer_is_not_itself_proof_of_truncation() {
    let a = capture(1, 1, b"needle");
    let b = capture(2, 1, b"unrelated");
    let docs = [document("a.rs", &a), document("b.rs", &b)];
    let budget = budget();
    let index = index(&docs, IndexLimits::default(), &budget);
    let query = ParsedQuery::parse("needle").unwrap();
    let result = index.search(&query, options().with_max_matches(1), &budget, allocation(2), || false).unwrap();
    assert!(result.is_complete());
    assert_eq!(result.capture_results().coverage, SearchCoverage::Exhaustive);
    assert_eq!(result.capture_results().matches.len(), 1);
}

#[test]
fn progressive_steps_append_without_reordering_or_premature_completeness() {
    let a = capture(1, 1, b"needle"); let b = capture(2, 1, b"needle");
    let docs = [document("a.rs", &a), document("b.rs", &b)];
    let budget = budget();
    let index = index(&docs, IndexLimits::default(), &budget);
    let query = ParsedQuery::parse("needle").unwrap();
    let mut session = IndexedQuery::new(&index, &query, options(), &budget, allocation(2)).unwrap();
    session.step(0, generation(1), || false).unwrap();
    assert_eq!(session.report().examined_files(), 0);
    session.step(1, generation(1), || false).unwrap();
    assert_eq!(session.report().state(), IndexedQueryState::Running);
    assert!(!session.report().is_complete());
    let first = session.report().capture_results().matches.clone();
    session.step(1, generation(1), || false).unwrap();
    assert!(session.report().is_complete());
    assert_eq!(&session.report().capture_results().matches[..first.len()], first);
    assert_eq!(session.report().capture_results().matches[1].file_id, b.request().file());
}

#[test]
fn canceled_and_superseded_sessions_never_consume_later_files() {
    let a = capture(1, 1, b"needle"); let b = capture(2, 1, b"needle");
    let docs = [document("a.rs", &a), document("b.rs", &b)];
    let budget = budget();
    let index = index(&docs, IndexLimits::default(), &budget);
    let query = ParsedQuery::parse("needle").unwrap();
    for replace in [false, true] {
        let mut session = IndexedQuery::new(&index, &query, options(), &budget, allocation(2)).unwrap();
        session.step(1, generation(1), || false).unwrap();
        if replace {
            assert!(matches!(session.step(1, generation(2), || false), Err(IndexError::StaleQuery)));
        } else { session.step(1, generation(1), || true).unwrap(); }
        assert_eq!(session.report().state(), IndexedQueryState::Canceled);
        assert_eq!(session.report().examined_files(), 1);
        session.step(256, generation(1), || false).unwrap();
        assert_eq!(session.report().examined_files(), 1);
        assert!(!session.report().is_complete());
        assert!(session.report().validate_delivery(manifest(&docs).id(), generation(2)).is_err());
    }
}

#[test]
fn closed_empty_discovering_and_unavailable_universes_remain_distinct() {
    let budget = budget();
    let query = ParsedQuery::parse("needle").unwrap();
    let absent = [FileId::new(owner(), 99).unwrap()];
    for membership in [MembershipState::Closed, MembershipState::Discovering] {
        for unavailable in [&[][..], &absent[..]] {
            let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), &[],
                unavailable, membership, ManifestLimits::default()).unwrap();
            let index = EphemeralIndex::build(manifest, IndexLimits::default(), &budget, allocation(1), || false).unwrap();
            let result = index.search(&query, options(), &budget, allocation(2), || false).unwrap();
            assert_eq!(result.is_complete(), membership == MembershipState::Closed && unavailable.is_empty());
            assert_eq!(result.known_files(), unavailable.len());
            assert_eq!(result.unavailable_files(), unavailable);
            assert_eq!(result.state(), IndexedQueryState::Finished);
        }
    }
}

#[test]
fn old_queries_keep_the_old_capture_after_a_new_revision_is_indexed() {
    let old = capture(1, 1, b"old needle"); let new = capture(1, 2, b"new source");
    let old_docs = [document("same.rs", &old)]; let new_docs = [document("same.rs", &new)];
    let budget = budget();
    let old_index = index(&old_docs, IndexLimits::default(), &budget);
    let new_manifest = SearchManifest::new(SearchManifestId::new(owner(), 2).unwrap(), &new_docs,
        &[], MembershipState::Closed, ManifestLimits::default()).unwrap();
    let new_index = EphemeralIndex::build(new_manifest, IndexLimits::default(), &budget, allocation(2), || false).unwrap();
    let query = ParsedQuery::parse("needle").unwrap();
    let old_result = old_index.search(&query, options(), &budget, allocation(3), || false).unwrap();
    let new_result = new_index.search(&query, options(), &budget, allocation(4), || false).unwrap();
    assert_eq!(old_result.capture_results().matches[0].revision, old.request().revision());
    assert!(new_result.capture_results().matches.is_empty());
    assert!(old_result.validate_delivery(new_manifest.id(), generation(1)).is_err());
}

#[test]
fn report_ownership_keeps_output_charged_after_the_query_and_index_close() {
    let c = capture(1, 1, b"needle"); let docs = [document("a.rs", &c)];
    let budget = budget(); let query = ParsedQuery::parse("needle").unwrap();
    let index = index(&docs, IndexLimits::default(), &budget);
    let result = index.search(&query, options(), &budget, allocation(2), || false).unwrap();
    let output = result.reserved_output_bytes();
    drop(index);
    assert_eq!(budget.accounting().reserved().get(), output);
    assert_eq!(result.capture_results().matches.len(), 1);
    drop(result);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn malformed_queries_and_foreign_owners_are_rejected_before_candidate_skips() {
    let c = capture(1, 1, b"unrelated"); let docs = [document("a.rs", &c)];
    let budget = budget(); let index = index(&docs, IndexLimits::default(), &budget);
    let mut query = ParsedQuery::parse("needle").unwrap();
    let foreign = QueryOptions::new(QueryGeneration::new(ArenaOwnerId::new(88).unwrap(), 1).unwrap());
    assert!(matches!(index.search(&query, foreign, &budget, allocation(2), || false), Err(IndexError::OwnerMismatch)));
    query.primary_needle.clear();
    assert!(matches!(index.search(&query, options(), &budget, allocation(2), || false), Err(IndexError::Query(QueryError::EmptyNeedle))));
}

#[test]
fn independent_negative_control_detects_a_hit_relabelled_as_new_source() {
    let c = capture(1, 1, b"needle"); let docs = [document("a.rs", &c)];
    let budget = budget(); let index = index(&docs, IndexLimits::default(), &budget);
    let query = ParsedQuery::parse("needle").unwrap();
    let result = index.search(&query, options(), &budget, allocation(2), || false).unwrap();
    let oracle = ReferenceScanOracle::scan_collection(&docs, &query, &options()).unwrap();
    let mut corrupted = result.capture_results().clone();
    corrupted.matches[0].revision = SourceRevision::new(owner(), 2).unwrap();
    assert!(ReferenceScanOracle::verify_oracle_match(&corrupted, &oracle).is_err());
}

#![forbid(unsafe_code)]

//! FCB-028.A: real matchers yield inside captured files without treating a
//! scheduling boundary as source EOF. Independent executable verification is
//! required; fixtures are not a physical-Mac latency qualification.

use std::sync::Arc;

use fcb_core::{
    ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget,
    SourceRevision,
};
use fcb_search::index::export::SourceRetentionLimits;
use fcb_search::indexed_query::OwnedIndexedQuery;
use fcb_search::{
    CaptureQueryScan, CaptureQueryScanner, EphemeralIndex, IndexError, IndexLimits, IndexedQuery,
    IndexedQueryState, MAX_CAPTURE_QUERY_STEP_BYTES, ManifestLimits, MembershipState, ParsedQuery,
    QueryOptions, ReferenceScanOracle, SearchCoverage, SearchDocument, SearchManifest,
    SearchManifestId, SearchMode, SearchResult, UnicodeNormalization,
};
use fcb_source::{CaptureRequest, CompleteCapture};

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(9201).unwrap()
}
fn generation(n: u64) -> QueryGeneration {
    QueryGeneration::new(owner(), n).unwrap()
}
fn allocation(n: u64) -> ResourceAllocationId {
    ResourceAllocationId::new(n).unwrap()
}
fn budget(bytes: u64) -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(bytes)).unwrap()
}
fn capture(file: u64, bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(
        CaptureRequest::new(
            FileId::new(owner(), file).unwrap(),
            SourceRevision::new(owner(), 3).unwrap(),
        )
        .unwrap(),
        ByteLength::new(bytes.len() as u64),
        Arc::from(bytes),
    )
    .unwrap()
}
fn document(source: &CompleteCapture) -> SearchDocument<'_> {
    SearchDocument::new(source.request().file(), "src/example.rs", source)
}
fn options() -> QueryOptions {
    QueryOptions::new(generation(1)).with_max_matches(8)
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    "\u{feff}"
        .encode_utf16()
        .chain(text.encode_utf16())
        .flat_map(|unit| {
            if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            }
        })
        .collect()
}
fn finish(mut cursor: CaptureQueryScan, quantum: usize, source_len: usize) -> SearchResult {
    for _ in 0..=source_len.saturating_add(1) {
        if cursor.is_terminal() {
            return cursor.into_result().unwrap();
        }
        let before = cursor.scanned_bytes();
        let consumed = cursor.step(quantum, || false).unwrap();
        assert!(consumed <= quantum.min(MAX_CAPTURE_QUERY_STEP_BYTES));
        assert_eq!(cursor.scanned_bytes() - before, consumed as u64);
        assert!(consumed > 0 || cursor.is_terminal());
    }
    panic!("bounded verifier made no terminal progress");
}
fn manifest<'a>(docs: &'a [SearchDocument<'a>]) -> SearchManifest<'a> {
    SearchManifest::new(
        SearchManifestId::new(owner(), 1).unwrap(),
        docs,
        &[],
        MembershipState::Closed,
        ManifestLimits::default(),
    )
    .unwrap()
}
fn fallback() -> IndexLimits {
    IndexLimits {
        max_source_bytes_per_file: 0,
        ..IndexLimits::default()
    }
}

#[test]
fn every_small_quantum_preserves_utf8_utf16_expressions_windows_and_normalized_expansions() {
    let text = "banana required Straße ßß Ée\u{0301} 🙂 banana";
    for bytes in [
        text.as_bytes().to_vec(),
        utf16(text, true),
        utf16(text, false),
    ] {
        let source = capture(1, &bytes);
        let doc = document(&source);
        for (query, mode) in [
            ("ana required -blocked", SearchMode::default()),
            (
                "strasse required -blocked",
                SearchMode::DecodedText {
                    case_sensitive: false,
                    normalization: UnicodeNormalization::CaseFold,
                },
            ),
            (
                "s required",
                SearchMode::DecodedText {
                    case_sensitive: false,
                    normalization: UnicodeNormalization::CaseFold,
                },
            ),
            (
                "é required",
                SearchMode::DecodedText {
                    case_sensitive: false,
                    normalization: UnicodeNormalization::Canonical,
                },
            ),
            ("🙂", SearchMode::default()),
        ] {
            let query = ParsedQuery::parse(query).unwrap();
            for count in [false, true] {
                for skip in [0, 1, 3] {
                    let opts = options().with_mode(mode).with_max_matches(2);
                    let expected = CaptureQueryScanner::scan_document_window_with_cancel(
                        &doc,
                        &query,
                        &opts,
                        skip,
                        count,
                        || false,
                    )
                    .unwrap();
                    for quantum in [1, 2, 3, 7, 17, 16_384, usize::MAX] {
                        let actual = finish(
                            CaptureQueryScan::new(&doc, &query, &opts, skip, count).unwrap(),
                            quantum,
                            bytes.len(),
                        );
                        ReferenceScanOracle::verify_oracle_match(&actual, &expected).unwrap();
                    }
                }
            }
        }
    }
    let source = capture(1, b"banana required banana");
    let doc = document(&source);
    let query = ParsedQuery::parse("ana required -blocked").unwrap();
    let opts = options().with_mode(SearchMode::RawBytes);
    let expected = ReferenceScanOracle::scan_document(&doc, &query, &opts).unwrap();
    for quantum in 1..=9 {
        let actual = finish(
            CaptureQueryScan::new(&doc, &query, &opts, 0, false).unwrap(),
            quantum,
            source.bytes().len(),
        );
        ReferenceScanOracle::verify_oracle_match(&actual, &expected).unwrap();
    }
}

#[test]
fn giant_capture_pauses_inside_a_match_and_pins_the_original_owned_bytes() {
    let mut bytes = vec![b'x'; MAX_CAPTURE_QUERY_STEP_BYTES - 3];
    bytes.extend_from_slice(b"needle");
    bytes.extend(vec![b'x'; 2 * MAX_CAPTURE_QUERY_STEP_BYTES]);
    let source = capture(1, &bytes);
    let query = ParsedQuery::parse("needle").unwrap();
    let mut cursor =
        CaptureQueryScan::new(&document(&source), &query, &options(), 0, true).unwrap();
    assert_eq!(cursor.step(0, || false).unwrap(), 0);
    assert_eq!(cursor.scanned_bytes(), 0);
    assert_eq!(
        cursor.step(usize::MAX, || false).unwrap(),
        MAX_CAPTURE_QUERY_STEP_BYTES
    );
    assert!(!cursor.is_terminal());
    assert!(cursor.result().is_none());
    drop(source); // The cursor retains exact source, not a path to reopen.
    bytes.fill(b'z');
    let result = finish(cursor, 17, bytes.len());
    assert!(result.is_complete());
    assert_eq!(result.match_count(), 1);
    assert_eq!(
        result.matches[0].original_byte_range.start().get(),
        (MAX_CAPTURE_QUERY_STEP_BYTES - 3) as u64
    );
    assert_eq!(result.matches[0].matched_text, "needle");
}

#[test]
fn late_predicates_and_malformed_suffixes_cannot_publish_tentative_hits() {
    for suffix in [" required", " required blocked"] {
        let text = format!(
            "needle {}{suffix}",
            "x".repeat(2 * MAX_CAPTURE_QUERY_STEP_BYTES)
        );
        let source = capture(1, text.as_bytes());
        let query = ParsedQuery::parse("needle required -blocked").unwrap();
        let opts = options().with_max_matches(1);
        let mut cursor = CaptureQueryScan::new(&document(&source), &query, &opts, 0, true).unwrap();
        cursor.step(MAX_CAPTURE_QUERY_STEP_BYTES, || false).unwrap();
        assert!(!cursor.is_terminal());
        assert!(cursor.result().is_none());
        let result = finish(cursor, MAX_CAPTURE_QUERY_STEP_BYTES, text.len());
        assert!(result.is_complete());
        assert_eq!(
            result.match_count(),
            usize::from(!suffix.contains("blocked"))
        );
    }
    let mut malformed = format!(
        "needle blocked {}",
        "x".repeat(MAX_CAPTURE_QUERY_STEP_BYTES)
    )
    .into_bytes();
    malformed.push(0xff);
    let source = capture(1, &malformed);
    let query = ParsedQuery::parse("needle -blocked").unwrap();
    let result = finish(
        CaptureQueryScan::new(&document(&source), &query, &options(), 0, true).unwrap(),
        13,
        malformed.len(),
    );
    assert!(!result.is_complete());
    assert_eq!(result.unsupported_files, [source.request().file()]);
    assert_eq!(result.match_count(), 0);
}

#[test]
fn byte_budgets_inside_boms_and_scalars_are_partial_not_false_negatives() {
    for bytes in [
        utf16("🙂needle", true),
        utf16("🙂needle", false),
        "\u{feff}🙂needle".as_bytes().to_vec(),
    ] {
        let source = capture(1, &bytes);
        let query = ParsedQuery::parse("needle").unwrap();
        for limit in 0..bytes.len() {
            let mut opts = options();
            opts.max_bytes_scanned = Some(limit as u64);
            let expected =
                CaptureQueryScanner::scan_document(&document(&source), &query, &opts).unwrap();
            let result = finish(
                CaptureQueryScan::new(&document(&source), &query, &opts, 0, false).unwrap(),
                1,
                bytes.len(),
            );
            assert_eq!(result, expected);
            assert!(!result.is_complete());
            assert!(result.unsupported_files.is_empty());
        }
    }
}

#[test]
fn cancellation_discards_unqualified_hits_and_terminal_steps_do_not_rescan() {
    let text = format!(
        "needle {} required",
        "x".repeat(2 * MAX_CAPTURE_QUERY_STEP_BYTES)
    );
    let source = capture(1, text.as_bytes());
    let query = ParsedQuery::parse("needle required -blocked").unwrap();
    let mut cursor =
        CaptureQueryScan::new(&document(&source), &query, &options(), 0, true).unwrap();
    cursor.step(17, || false).unwrap();
    assert_eq!(cursor.step(17, || true).unwrap(), 0);
    assert!(cursor.is_terminal());
    assert_eq!(
        cursor.result().unwrap().coverage,
        SearchCoverage::CanceledEarly
    );
    assert!(cursor.result().unwrap().matches.is_empty());
    let before = cursor.scanned_bytes();
    assert_eq!(cursor.step(usize::MAX, || false).unwrap(), 0);
    assert_eq!(cursor.scanned_bytes(), before);
}

#[test]
fn indexed_steps_share_a_byte_quantum_and_publish_only_finalized_members() {
    let text = format!(
        "needle {} needle",
        "x".repeat(2 * MAX_CAPTURE_QUERY_STEP_BYTES)
    );
    let first = capture(1, text.as_bytes());
    let second = capture(2, b"needle");
    let docs = [document(&first), document(&second)];
    let budget = budget(64 * 1024 * 1024);
    let index = EphemeralIndex::build(manifest(&docs), fallback(), &budget, allocation(1), || {
        false
    })
    .unwrap();
    let query = ParsedQuery::parse("needle").unwrap();
    let opts = options().with_max_matches(1);
    let expected =
        ReferenceScanOracle::scan_collection_counting_with_cancel(&docs, &query, &opts, || false)
            .unwrap();
    let mut cursor =
        IndexedQuery::new_counting(&index, &query, opts, &budget, allocation(2)).unwrap();
    cursor.step(usize::MAX, generation(1), || false).unwrap();
    assert_eq!(cursor.report().state(), IndexedQueryState::Running);
    assert_eq!(cursor.report().examined_files(), 0);
    assert_eq!(cursor.report().scan_attempts(), 0);
    assert_eq!(cursor.report().fallback_attempts(), 0);
    assert_eq!(cursor.report().capture_results().match_count(), 0);
    assert!(cursor.report().capture_results().matches.is_empty());
    assert_eq!(
        cursor.report().capture_results().scanned_bytes,
        MAX_CAPTURE_QUERY_STEP_BYTES as u64
    );
    assert!(cursor.report().reserved_scan_bytes() > 0);
    let output = cursor.report().reserved_output_bytes();
    for _ in 0..8 {
        if cursor.report().is_terminal() {
            break;
        }
        let before = cursor.report().capture_results().scanned_bytes;
        cursor.step(usize::MAX, generation(1), || false).unwrap();
        assert!(
            cursor.report().capture_results().scanned_bytes - before
                <= MAX_CAPTURE_QUERY_STEP_BYTES as u64
        );
        assert_eq!(cursor.report().reserved_output_bytes(), output);
    }
    assert!(cursor.report().is_complete());
    assert_eq!(cursor.report().examined_files(), 2);
    assert_eq!(cursor.report().scan_attempts(), 2);
    assert_eq!(cursor.report().fallback_attempts(), 2);
    assert_eq!(cursor.report().reserved_scan_bytes(), 0);
    ReferenceScanOracle::verify_oracle_match(cursor.report().capture_results(), &expected).unwrap();
}

#[test]
fn scratch_is_admitted_before_source_work_and_released_on_cancellation() {
    let source = capture(1, &vec![b'a'; 2 * MAX_CAPTURE_QUERY_STEP_BYTES]);
    let docs = [document(&source)];
    let index_budget = budget(64 * 1024 * 1024);
    let index = EphemeralIndex::build(
        manifest(&docs),
        fallback(),
        &index_budget,
        allocation(1),
        || false,
    )
    .unwrap();
    let query = ParsedQuery::parse("needle").unwrap();
    let tiny = budget(64 * 1024);
    let mut refused = IndexedQuery::new(&index, &query, options(), &tiny, allocation(2)).unwrap();
    assert!(refused.report().reserved_output_bytes() < 64 * 1024);
    assert_eq!(
        refused.step(1, generation(1), || false).err(),
        Some(IndexError::ResourceDenied)
    );
    assert_eq!(refused.report().capture_results().scanned_bytes, 0);
    assert_eq!(refused.report().examined_files(), 0);
    drop(refused);
    assert_eq!(tiny.accounting().reserved().get(), 0);

    let query_budget = budget(16 * 1024 * 1024);
    let mut cursor =
        IndexedQuery::new(&index, &query, options(), &query_budget, allocation(2)).unwrap();
    cursor
        .step_with_budget(1, 0, generation(1), || false)
        .unwrap();
    assert_eq!(cursor.report().capture_results().scanned_bytes, 0);
    cursor
        .step_with_budget(1, 17, generation(1), || false)
        .unwrap();
    assert_eq!(cursor.report().capture_results().scanned_bytes, 17);
    assert!(cursor.report().reserved_scan_bytes() > 0);
    assert_eq!(
        cursor.step(1, generation(2), || false).err(),
        Some(IndexError::StaleQuery)
    );
    assert_eq!(cursor.report().state(), IndexedQueryState::Canceled);
    assert_eq!(cursor.report().reserved_scan_bytes(), 0);
    assert_eq!(
        query_budget.accounting().reserved().get(),
        cursor.report().reserved_output_bytes()
    );
}

#[test]
fn dropping_an_owned_index_keeps_a_suspended_capture_charged_until_query_drop() {
    let source = capture(1, &vec![b'a'; 2 * MAX_CAPTURE_QUERY_STEP_BYTES]);
    let docs = [document(&source)];
    let source_budget = budget(32 * 1024 * 1024);
    let index = EphemeralIndex::build(
        manifest(&docs),
        fallback(),
        &source_budget,
        allocation(1),
        || false,
    )
    .unwrap()
    .into_owned(
        SourceRetentionLimits::default(),
        &source_budget,
        allocation(2),
        || false,
    )
    .unwrap();
    let query_budget = budget(16 * 1024 * 1024);
    let mut cursor = OwnedIndexedQuery::new(
        &index,
        ParsedQuery::parse("needle").unwrap(),
        options(),
        &query_budget,
        [allocation(3), allocation(4)],
        || false,
    )
    .unwrap();
    cursor.step(&index, 1, generation(1), || false).unwrap();
    assert!(!cursor.report().is_terminal());
    assert_eq!(cursor.report().examined_files(), 0);
    drop(index);
    assert!(source_budget.accounting().reserved().get() >= source.bytes().len() as u64);
    drop(cursor);
    assert_eq!(source_budget.accounting().reserved().get(), 0);
    assert_eq!(query_budget.accounting().reserved().get(), 0);
}

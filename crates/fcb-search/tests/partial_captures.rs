#![forbid(unsafe_code)]

//! Whole-file search cannot infer source origin or coverage from the completion
//! of a ranged provider request. Extent search owns that separate contract.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId,
    QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_search::{CaptureQueryScanner, DirectSourceScanner, EphemeralIndex, IndexError,
    IndexLimits, ManifestLimits, MembershipState, ParsedQuery, QueryError, QueryOptions,
    ReferenceScanOracle, SearchDocument, SearchManifest, SearchManifestId, SearchMode,
    UnicodeNormalization};
use fcb_search::index::export::{OwnedIndexBuilder, SourceRetentionLimits};
use fcb_source::{CancelFlag, CaptureOutcome, CaptureRequest, ChunkSize, ChunkedCapture,
    CompleteCapture, InMemorySourceProvider, SourceChunk, SourceGrant};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(27051).unwrap() }
fn file() -> FileId { FileId::new(owner(), 1).unwrap() }
fn revision() -> SourceRevision { SourceRevision::new(owner(), 1).unwrap() }
fn allocation(value: u64) -> ResourceAllocationId { ResourceAllocationId::new(value).unwrap() }
fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
}
fn request(scope: Option<ByteRange>) -> CaptureRequest {
    let request = CaptureRequest::new(file(), revision()).unwrap();
    match scope { Some(scope) => request.with_range(scope).unwrap(), None => request }
}
fn capture(bytes: &[u8], scope: Option<ByteRange>) -> CompleteCapture {
    CompleteCapture::new(request(scope), ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn options(mode: SearchMode) -> QueryOptions {
    QueryOptions::new(QueryGeneration::new(owner(), 1).unwrap()).with_mode(mode).with_max_matches(8)
}
fn modes() -> [SearchMode; 4] {
    [SearchMode::RawBytes, SearchMode::default(),
        SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::CaseFold },
        SearchMode::DecodedText { case_sensitive: true, normalization: UnicodeNormalization::Canonical }]
}
fn budget() -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap()
}
fn manifest_id() -> SearchManifestId { SearchManifestId::new(owner(), 1).unwrap() }
fn retention() -> SourceRetentionLimits {
    SourceRetentionLimits { max_files: 2, max_source_bytes: 4096, max_path_bytes: 128 }
}

#[test]
fn provider_range_completion_is_not_whole_file_search_authority() {
    let mut provider = InMemorySourceProvider::new(owner(), ByteLength::new(1024));
    provider.insert(file(), revision(), Arc::<[u8]>::from(b"pre banana post".as_slice())).unwrap();
    let grant = SourceGrant::new(owner()).grant(file()).unwrap();
    let read = |scope| match provider.capture(&grant, &request(scope), &CancelFlag::new()).unwrap() {
        CaptureOutcome::Complete(capture) => capture,
        CaptureOutcome::Extent(_) => panic!("in-memory provider returns a completed request"),
    };
    let whole = read(None);
    let partial = read(Some(range(4, 10)));
    // Even identical bytes from an explicitly ranged request carry no separate
    // whole-file length authority in CompleteCapture.
    let ranged_whole = read(Some(range(0, 15)));
    assert_eq!(partial.bytes(), b"banana");
    assert_eq!(partial.declared_length().get(), 6);
    assert_eq!(whole.bytes(), ranged_whole.bytes());
    assert_eq!(whole.request().revision(), partial.request().revision());
    assert_eq!(QueryError::PartialCapture.code(), "QUERY_PARTIAL_CAPTURE");

    for mode in modes() {
        let opts = options(mode);
        let exact = DirectSourceScanner::scan_complete_capture(&whole, "ana", &opts).unwrap();
        assert!(exact.is_complete());
        assert_eq!(exact.total_matches_counted, 2);
        assert_eq!(exact.matches.iter().map(|hit| hit.original_byte_range).collect::<Vec<_>>(),
            [range(5, 8), range(7, 10)]);
        for subset in [&partial, &ranged_whole] {
            assert_eq!(DirectSourceScanner::scan_complete_capture(subset, "ana", &opts),
                Err(QueryError::PartialCapture));
            assert_eq!(DirectSourceScanner::scan_complete_capture_with_cancel(subset, "ana", &opts,
                || panic!("partial capture must be refused before scanning")), Err(QueryError::PartialCapture));
            assert_eq!(DirectSourceScanner::scan_complete_capture_counting_with_cancel(subset, "ana", &opts,
                || panic!("partial capture must be refused before counting")), Err(QueryError::PartialCapture));
        }
        let counted = DirectSourceScanner::scan_complete_capture_counting_with_cancel(
            &whole, "ana", &opts.with_max_matches(0), || false).unwrap();
        assert!(counted.is_complete());
        assert!(counted.matches.is_empty());
        assert_eq!(counted.total_matches_counted, 2);

        // Negative control: the reference comparison catches the former
        // subset-relative labels even when identities and hit counts agree.
        let mut wrong_origin = exact.clone();
        wrong_origin.matches[0].original_byte_range = range(1, 4);
        assert!(ReferenceScanOracle::verify_oracle_match(&wrong_origin, &exact).is_err());
    }
}

#[test]
fn utf16_range_start_is_not_reinterpreted_as_a_file_bom() {
    for little in [false, true] {
        let bytes: Vec<u8> = "\u{feff}x\u{feff}ana".encode_utf16()
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
        let whole = capture(&bytes, None);
        let partial = capture(&bytes[4..], Some(range(4, bytes.len() as u64)));
        let opts = options(SearchMode::default());
        let result = DirectSourceScanner::scan_complete_capture(&whole, "\u{feff}ana", &opts).unwrap();
        assert!(result.is_complete());
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].original_byte_range, range(4, 12));
        assert_eq!(DirectSourceScanner::scan_complete_capture(&partial, "\u{feff}ana", &opts),
            Err(QueryError::PartialCapture));
    }
}

#[test]
fn expression_windows_filters_and_count_only_scans_cannot_certify_a_subset() {
    for scope in [range(0, 12), range(100, 112)] {
        let partial = capture(b"banana ready", Some(scope));
        let doc = SearchDocument::new(file(), "src/example.rs", &partial);
        for mode in modes() {
            for limit in [0, 8] {
                let opts = options(mode).with_max_matches(limit);
                for expression in ["ana", "ana ready -blocked", "ana -banana",
                    "ana path:elsewhere/", "ana lang:python"] {
                    let query = ParsedQuery::parse(expression).unwrap();
                    let results = [
                        CaptureQueryScanner::scan_document(&doc, &query, &opts),
                        CaptureQueryScanner::scan_document_counting_with_cancel(&doc, &query, &opts, || false),
                        CaptureQueryScanner::scan_document_window_with_cancel(&doc, &query, &opts, 1, false, || false),
                        CaptureQueryScanner::scan_document_window_with_cancel(&doc, &query, &opts, 1, true, || false),
                        ReferenceScanOracle::scan_document(&doc, &query, &opts),
                        ReferenceScanOracle::scan_document_counting_with_cancel(&doc, &query, &opts, || false),
                        ReferenceScanOracle::scan_collection(std::slice::from_ref(&doc), &query, &opts),
                        ReferenceScanOracle::scan_collection_counting_with_cancel(
                            std::slice::from_ref(&doc), &query, &opts, || false),
                    ];
                    for result in results { assert_eq!(result, Err(QueryError::PartialCapture)); }
                }
            }
        }
    }
    let whole = capture(b"banana ready", None);
    let doc = SearchDocument::new(file(), "src/example.rs", &whole);
    let query = ParsedQuery::parse("ana ready -blocked").unwrap();
    let opts = options(SearchMode::default());
    let exact = ReferenceScanOracle::scan_document(&doc, &query, &opts).unwrap();
    let actual = CaptureQueryScanner::scan_document(&doc, &query, &opts).unwrap();
    ReferenceScanOracle::verify_oracle_match(&actual, &exact).unwrap();
    assert!(actual.is_complete());
    assert_eq!(actual.total_matches_counted, 2);
    let window = CaptureQueryScanner::scan_document_window_with_cancel(
        &doc, &query, &opts, 1, true, || false).unwrap();
    assert!(window.is_complete());
    assert_eq!(window.total_matches_counted, 2);
    assert_eq!(window.matches.len(), 1);
    assert_eq!(window.matches[0].original_byte_range, range(3, 6));
}

#[test]
fn chunk_completion_does_not_rebase_ranged_sources_to_zero() {
    let chunks = || vec![
        SourceChunk::new(0, range(0, 3), Arc::from(b"ban".as_slice())).unwrap(),
        SourceChunk::new(1, range(3, 6), Arc::from(b"ana".as_slice())).unwrap(),
    ];
    for scope in [None, Some(range(0, 6)), Some(range(100, 106))] {
        let capture = ChunkedCapture::new(request(scope), ByteLength::new(6),
            ChunkSize::bounded(3).unwrap(), chunks()).unwrap();
        for mode in modes() {
            let opts = options(mode);
            let result = DirectSourceScanner::scan_chunked_capture(&capture, "ana", &opts);
            if scope.is_some() {
                assert_eq!(result, Err(QueryError::PartialCapture));
                assert_eq!(DirectSourceScanner::scan_chunked_capture_with_cancel(&capture, "ana", &opts,
                    || panic!("partial chunks must be refused before scanning")), Err(QueryError::PartialCapture));
            } else {
                let result = result.unwrap();
                assert!(result.is_complete());
                assert_eq!(result.matches.iter().map(|hit| hit.original_byte_range).collect::<Vec<_>>(),
                    [range(1, 4), range(3, 6)]);
            }
        }
    }
}

#[test]
fn manifest_admission_prevents_false_negative_segments_and_complete_counts() {
    for scope in [range(0, 6), range(100, 106)] {
        let partial = capture(b"banana", Some(scope));
        let docs = [SearchDocument::new(file(), "src/example.rs", &partial)];
        for membership in [MembershipState::Closed, MembershipState::Discovering] {
            assert!(matches!(SearchManifest::new(manifest_id(), &docs, &[], membership, ManifestLimits::default()),
                Err(IndexError::Query(QueryError::PartialCapture))));
        }
    }
    let whole = capture(b"banana", None);
    let docs = [SearchDocument::new(file(), "src/example.rs", &whole)];
    let manifest = SearchManifest::new(manifest_id(), &docs, &[], MembershipState::Closed,
        ManifestLimits::default()).unwrap();
    let query = ParsedQuery::parse("ana").unwrap();
    let opts = options(SearchMode::default()).with_max_matches(0);
    for limits in [IndexLimits::default(), IndexLimits { max_total_grams: 0, ..Default::default() }] {
        let budget = budget();
        let index = EphemeralIndex::build(manifest, limits, &budget, allocation(1), || false).unwrap();
        let report = index.search_counting(&query, opts.clone(), &budget, allocation(2), || false).unwrap();
        assert!(report.is_complete());
        assert_eq!(report.capture_results().total_matches_counted, 2);
        assert!(report.capture_results().matches.is_empty());
        let mut owned = index.into_owned(retention(), &budget, allocation(3), || false).unwrap();
        owned.with_index(&budget, allocation(4), || false, |index| {
            let report = index.search_counting(&query, opts.clone(), &budget, allocation(5), || false).unwrap();
            assert!(report.is_complete());
            assert_eq!(report.capture_results().total_matches_counted, 2);
        }).unwrap();
    }
}

#[test]
fn refusing_a_partial_append_cannot_publish_a_closed_owned_index() {
    for scope in [None, Some(range(0, 6)), Some(range(100, 106))] {
        let source = capture(b"banana", scope);
        let budget = budget();
        let mut builder = OwnedIndexBuilder::new(manifest_id(), retention(), IndexLimits::default(),
            &budget, allocation(1), allocation(2), || false).unwrap();
        let result = builder.push_capture(&source, "src/example.rs", &budget, allocation(3), || false);
        if scope.is_some() {
            assert_eq!(result, Err(IndexError::Query(QueryError::PartialCapture)));
            assert!(builder.is_failed());
            assert_eq!(builder.captured_files(), 0);
            assert_eq!(builder.source_bytes(), 0);
            assert!(matches!(builder.finish(MembershipState::Closed, || false), Err(IndexError::InvalidManifest)));
        } else {
            result.unwrap();
            let index = builder.finish(MembershipState::Closed, || false).unwrap();
            assert_eq!(index.captured_files(), 1);
            assert_eq!(index.capture(file()).unwrap().bytes(), b"banana");
            drop(index);
        }
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

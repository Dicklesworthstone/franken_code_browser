#![forbid(unsafe_code)]

use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb_search::{EphemeralIndex, IndexError, IndexLimits, ManifestLimits, MembershipState,
    ParsedQuery, QueryOptions, ReferenceScanOracle, SearchDocument, SearchManifest, SearchManifestId};
use fcb_search::index::export::{OwnedEphemeralIndex, SourceRetentionLimits};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(27001).unwrap() }
fn file(n: u64) -> FileId { FileId::new(owner(), n).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn capture(n: u64, bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(file(n), SourceRevision::new(owner(), 7).unwrap()).unwrap(),
        ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn owned(budget: &ResourceBudget, limits: IndexLimits, membership: MembershipState) -> OwnedEphemeralIndex {
    let a = capture(1, b"banana needle"); let b = capture(2, b"unrelated content");
    let docs = [SearchDocument::new(file(1), "a.rs", &a), SearchDocument::new(file(2), "b.rs", &b)];
    let manifest = SearchManifest::new(SearchManifestId::new(owner(), 3).unwrap(), &docs, &[],
        membership, ManifestLimits::default()).unwrap();
    EphemeralIndex::build(manifest, limits, budget, allocation(1), || false).unwrap()
        .into_owned(Default::default(), budget, allocation(2), || false).unwrap()
}
fn query(index: &mut OwnedEphemeralIndex, budget: &ResourceBudget, text: &str, id: u64) -> (usize, bool, usize) {
    index.with_index(budget, allocation(id), || false, |view| {
        let query = ParsedQuery::parse(text).unwrap();
        let report = view.search(&query, QueryOptions::new(QueryGeneration::new(owner(), id).unwrap()),
            budget, allocation(id + 1), || false).unwrap();
        (report.capture_results().matches.len(), report.is_complete(), report.skipped_by_index())
    }).unwrap()
}

#[test]
fn prepared_index_and_complete_sources_outlive_all_original_descriptors() {
    let budget = budget(); let mut index = owned(&budget, Default::default(), MembershipState::Closed);
    assert_eq!(index.id().revision(), 3); assert_eq!(index.captured_files(), 2);
    assert_eq!(index.capture(file(2)).unwrap().bytes(), b"unrelated content");
    assert_eq!(query(&mut index, &budget, "needle", 10), (1, true, 1));
    assert_eq!(query(&mut index, &budget, "unrelated", 20), (1, true, 1));
    assert_eq!(query(&mut index, &budget, "absent", 30), (0, true, 2));
}

#[test]
fn source_and_posting_allocations_are_shared_or_moved_not_rebuilt() {
    let budget = budget(); let source = capture(1, b"banana needle");
    let source_ptr = source.bytes().as_ptr();
    let docs = [SearchDocument::new(file(1), "a.rs", &source)];
    let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), &docs, &[],
        MembershipState::Closed, Default::default()).unwrap();
    let index = EphemeralIndex::build(manifest, Default::default(), &budget, allocation(1), || false).unwrap();
    let gram_ptr = index.segment_image(0).unwrap().grams().as_ptr() as usize;
    let stats = index.statistics();
    let mut index = index.into_owned(Default::default(), &budget, allocation(2), || false).unwrap();
    assert_eq!(index.capture(file(1)).unwrap().bytes().as_ptr(), source_ptr);
    for id in 10..14 {
        let (ptr, current) = index.with_index(&budget, allocation(id), || false, |view|
            (view.segment_image(0).unwrap().grams().as_ptr() as usize, view.statistics())).unwrap();
        assert_eq!(ptr, gram_ptr); assert_eq!(current, stats);
    }
}

#[test]
fn callback_failure_panic_and_late_cancellation_restore_postings() {
    let budget = budget(); let mut index = owned(&budget, Default::default(), MembershipState::Closed);
    let result: Result<(), &str> = index.with_index(&budget, allocation(10), || false, |_| Err("refused")).unwrap();
    assert_eq!(result, Err("refused"));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = index.with_index(&budget, allocation(11), || false, |_| panic!("test unwind"));
    })).is_err());
    let cancel = std::cell::Cell::new(false);
    assert_eq!(index.with_index(&budget, allocation(12), || cancel.get(), |_| cancel.set(true)).err(), Some(IndexError::Canceled));
    assert_eq!(query(&mut index, &budget, "needle", 20), (1, true, 1));
}

#[test]
fn failed_descriptor_admission_or_precancel_never_changes_retained_index() {
    let budget = budget(); let mut index = owned(&budget, Default::default(), MembershipState::Closed);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert_eq!(index.with_index(&tiny, allocation(10), || false, |_| ()).err(), Some(IndexError::ResourceDenied));
    assert_eq!(index.with_index(&budget, allocation(11), || true, |_| ()).err(), Some(IndexError::Canceled));
    assert_eq!(query(&mut index, &budget, "needle", 20), (1, true, 1));
}

#[test]
fn uncovered_segments_and_short_queries_still_search_all_captured_sources() {
    let budget = budget(); let mut index = owned(&budget,
        IndexLimits { max_total_grams: 0, ..Default::default() }, MembershipState::Closed);
    assert_eq!(index.statistics().uncovered_files, 2);
    assert_eq!(query(&mut index, &budget, "needle", 10), (1, true, 0));
    assert_eq!(query(&mut index, &budget, "an", 20), (2, true, 0));
}

#[test]
fn utf16_exact_verification_matches_the_reference_oracle_after_transfer() {
    let budget = budget();
    let bytes: Vec<_> = "\u{feff}🦀 needle needle".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let source = capture(1, &bytes);
    let docs = [SearchDocument::new(file(1), "utf16.rs", &source)];
    let parsed = ParsedQuery::parse("needle").unwrap();
    let options = QueryOptions::new(QueryGeneration::new(owner(), 1).unwrap());
    let oracle = ReferenceScanOracle::scan_collection(&docs, &parsed, &options).unwrap();
    let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), &docs, &[],
        MembershipState::Closed, Default::default()).unwrap();
    let mut index = EphemeralIndex::build(manifest, Default::default(), &budget, allocation(1), || false).unwrap()
        .into_owned(Default::default(), &budget, allocation(2), || false).unwrap();
    let result = index.with_index(&budget, allocation(3), || false, |view| {
        let report = view.search(&parsed, options, &budget, allocation(4), || false).unwrap();
        assert!(report.is_complete()); assert_eq!(report.fallback_attempts(), 1);
        report.capture_results().clone()
    }).unwrap();
    assert_eq!(result, oracle);
    assert_eq!(result.matches[0].original_byte_range.len().get(), 12);
}

#[test]
fn missing_captures_and_open_membership_never_become_complete_negatives() {
    let budget = budget(); let mut open = owned(&budget, Default::default(), MembershipState::Discovering);
    assert!(!query(&mut open, &budget, "absent", 10).1);
    let missing = [file(9)];
    let manifest = SearchManifest::new(SearchManifestId::new(owner(), 2).unwrap(), &[], &missing,
        MembershipState::Closed, Default::default()).unwrap();
    let mut index = EphemeralIndex::build(manifest, Default::default(), &budget, allocation(20), || false).unwrap()
        .into_owned(Default::default(), &budget, allocation(21), || false).unwrap();
    assert_eq!(index.unavailable_files(), &missing);
    assert_eq!(query(&mut index, &budget, "absent", 30), (0, false, 0));
}

#[test]
fn source_pin_limits_and_resource_denial_are_independent_from_gram_admission() {
    let budget = budget(); let source = capture(1, b"banana");
    let docs = [SearchDocument::new(file(1), "a.rs", &source)];
    let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), &docs, &[],
        MembershipState::Closed, Default::default()).unwrap();
    for (i, limits) in [SourceRetentionLimits { max_source_bytes: 5, ..Default::default() },
        SourceRetentionLimits { max_files: 0, ..Default::default() },
        SourceRetentionLimits { max_path_bytes: 3, ..Default::default() }].into_iter().enumerate() {
        let index = EphemeralIndex::build(manifest, Default::default(), &budget, allocation(10 + i as u64), || false).unwrap();
        assert_eq!(index.into_owned(limits, &budget, allocation(20 + i as u64), || false).err(), Some(IndexError::LimitExceeded));
    }
    let index = EphemeralIndex::build(manifest, Default::default(), &budget, allocation(30), || false).unwrap();
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert_eq!(index.into_owned(Default::default(), &tiny, allocation(31), || false).err(), Some(IndexError::ResourceDenied));
}

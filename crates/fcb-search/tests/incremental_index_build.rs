#![forbid(unsafe_code)]

use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb_search::{EphemeralIndex, IndexError, IndexLimits, ManifestLimits, MembershipState,
    ParsedQuery, QueryOptions, SearchDocument, SearchManifest, SearchManifestId};
use fcb_search::index::export::{OwnedIndexBuilder, SourceRetentionLimits};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(9301).unwrap() }
fn id() -> SearchManifestId { SearchManifestId::new(owner(), 9).unwrap() }
fn alloc(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn file(n: u64) -> FileId { FileId::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn capture(n: u64, bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(file(n), SourceRevision::new(owner(), n).unwrap()).unwrap(),
        ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn retention() -> SourceRetentionLimits {
    SourceRetentionLimits { max_files: 8, max_source_bytes: 65536, max_path_bytes: 1024 }
}
fn build(budget: &ResourceBudget, limits: IndexLimits) -> OwnedIndexBuilder {
    OwnedIndexBuilder::new(id(), retention(), limits, budget, alloc(1), alloc(2), || false).unwrap()
}

#[test]
fn incremental_segments_match_batch_engine_under_every_coverage_quota() {
    let sources = [capture(1, b"banana banana"), capture(2, b"abcdefghijk"), capture(3, b"banana\xff")];
    let documents: Vec<_> = sources.iter().map(|c| SearchDocument::new(c.request().file(), "file.rs", c)).collect();
    for limits in [IndexLimits::default(),
        IndexLimits { max_total_grams: 4, ..Default::default() },
        IndexLimits { max_grams_per_file: 2, ..Default::default() },
        IndexLimits { max_source_bytes_total: 14, ..Default::default() },
        IndexLimits { max_source_bytes_per_file: 8, ..Default::default() },
        IndexLimits { max_scratch_bytes: 4, ..Default::default() }] {
        let budget = budget();
        let manifest = SearchManifest::new(id(), &documents, &[], MembershipState::Closed, ManifestLimits::default()).unwrap();
        let batch = EphemeralIndex::build(manifest, limits, &budget, alloc(3), || false).unwrap();
        let mut builder = build(&budget, limits);
        for source in &sources { builder.push_capture(source, "file.rs", &budget, alloc(4), || false).unwrap(); }
        let mut owned = builder.finish(MembershipState::Closed, || false).unwrap();
        owned.with_index(&budget, alloc(5), || false, |view| {
            for i in 0..sources.len() {
                let a = batch.segment_image(i).unwrap(); let b = view.segment_image(i).unwrap();
                assert_eq!(a.coverage(), b.coverage()); assert_eq!(a.grams(), b.grams());
                assert_eq!(a.source_is_utf8(), b.source_is_utf8());
            }
        }).unwrap();
        assert_eq!(batch.statistics().source_bytes_examined, owned.statistics().source_bytes_examined);
    }
}

#[test]
fn source_backing_and_prior_nonmatches_survive_the_original_owners() {
    let budget = budget(); let mut builder = build(&budget, IndexLimits::default());
    let source = capture(1, b"previously not searched target"); let address = source.bytes().as_ptr();
    builder.push_capture(&source, "one.rs", &budget, alloc(3), || false).unwrap(); drop(source);
    let mut index = builder.finish(MembershipState::Closed, || false).unwrap();
    assert_eq!(index.capture(file(1)).unwrap().bytes().as_ptr(), address);
    index.with_index(&budget, alloc(4), || false, |view| {
        let result = view.search(&ParsedQuery::parse("target").unwrap(),
            QueryOptions::new(QueryGeneration::new(owner(), 10).unwrap()), &budget, alloc(5), || false).unwrap();
        assert!(result.is_complete()); assert_eq!(result.capture_results().matches.len(), 1);
    }).unwrap();
}

#[test]
fn utf16_and_uncovered_sources_keep_exact_scan_fallback() {
    let bytes: Vec<u8> = "\u{feff}banana 🦀".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let source = capture(1, &bytes); let budget = budget();
    let mut builder = build(&budget, IndexLimits { max_total_grams: 0, ..Default::default() });
    builder.push_capture(&source, "one.rs", &budget, alloc(3), || false).unwrap();
    let mut index = builder.finish(MembershipState::Closed, || false).unwrap();
    index.with_index(&budget, alloc(4), || false, |view| {
        let result = view.search(&ParsedQuery::parse("banana").unwrap(),
            QueryOptions::new(QueryGeneration::new(owner(), 10).unwrap()), &budget, alloc(5), || false).unwrap();
        assert!(result.is_complete()); assert_eq!(result.fallback_attempts(), 1);
        assert_eq!(result.capture_results().matches[0].original_byte_range.start().get(), 2);
        assert_eq!(result.capture_results().matches[0].original_byte_range.len().get(), 12);
    }).unwrap();
}

#[test]
fn unavailable_and_open_membership_never_become_complete_negative_answers() {
    for missing in [false, true] {
        let budget = budget(); let mut builder = build(&budget, IndexLimits::default());
        builder.push_capture(&capture(1, b"known"), "one", &budget, alloc(3), || false).unwrap();
        if missing { builder.push_unavailable(file(2), || false).unwrap(); }
        let membership = if missing { MembershipState::Closed } else { MembershipState::Discovering };
        let mut index = builder.finish(membership, || false).unwrap();
        index.with_index(&budget, alloc(4), || false, |view| {
            let report = view.search(&ParsedQuery::parse("absent").unwrap(),
                QueryOptions::new(QueryGeneration::new(owner(), 1).unwrap()), &budget, alloc(5), || false).unwrap();
            assert!(!report.is_complete()); assert!(report.capture_results().matches.is_empty());
            assert_eq!(report.unavailable_files().len(), usize::from(missing));
        }).unwrap();
    }
}

#[test]
fn failed_appends_cannot_be_finished_as_successful_partial_indexes() {
    for fault in 0..4 {
        let budget = budget(); let mut builder = build(&budget, IndexLimits::default());
        builder.push_capture(&capture(2, b"retained"), "two", &budget, alloc(3), || false).unwrap();
        let result = match fault {
            0 => builder.push_unavailable(file(2), || false),
            1 => builder.push_capture(&capture(1, b"out of order"), "one", &budget, alloc(3), || false),
            2 => builder.push_capture(&capture(3, &vec![b'x'; 65537]), "large", &budget, alloc(3), || false),
            _ => builder.push_unavailable(FileId::new(ArenaOwnerId::new(9302).unwrap(), 1).unwrap(), || false),
        };
        assert!(result.is_err()); assert!(builder.is_failed());
        assert!(builder.finish(MembershipState::Closed, || false).is_err());
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn cancellation_at_each_append_checkpoint_never_publishes_half_a_segment() {
    let source = capture(1, b"banana banana banana");
    for at in 1..=20 {
        let budget = budget(); let mut builder = build(&budget, IndexLimits::default()); let mut polls = 0;
        let result = builder.push_capture(&source, "one", &budget, alloc(3), || { polls += 1; polls == at });
        if result.is_err() {
            assert!(builder.is_failed()); assert!(builder.finish(MembershipState::Closed, || false).is_err());
        } else {
            assert_eq!(builder.finish(MembershipState::Closed, || false).unwrap().captured_files(), 1);
        }
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn panic_during_append_poisoning_and_pre_admission_refusals_release_capacity() {
    let source = capture(1, b"banana"); let budget = budget(); let mut builder = build(&budget, IndexLimits::default());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = builder.push_capture(&source, "one", &budget, alloc(3), || panic!("interrupted"));
    })).is_err());
    assert!(builder.is_failed()); drop(builder);
    assert_eq!(budget.accounting().reserved().get(), 0);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(OwnedIndexBuilder::new(id(), retention(), IndexLimits::default(), &tiny,
        alloc(1), alloc(2), || false), Err(IndexError::ResourceDenied)));
    assert_eq!(tiny.accounting().reserved().get(), 0);
}

#[test]
fn empty_finalization_and_cross_worker_append_keep_membership_and_identity() {
    let budget = budget();
    let empty = build(&budget, IndexLimits::default()).finish(MembershipState::Closed, || false).unwrap();
    assert_eq!(empty.captured_files(), 0); assert_eq!(empty.membership(), MembershipState::Closed); drop(empty);
    let builder = build(&budget, IndexLimits::default());
    let (builder, budget) = std::thread::spawn(move || {
        let mut builder = builder;
        builder.push_capture(&capture(1, b"one"), "one", &budget, alloc(3), || false).unwrap();
        (builder, budget)
    }).join().unwrap();
    let index = builder.finish(MembershipState::Closed, || false).unwrap();
    assert_eq!(index.id(), id()); assert_eq!(index.source_bytes(), 3);
    drop(index); assert_eq!(budget.accounting().reserved().get(), 0);
}

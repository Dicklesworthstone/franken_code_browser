#![forbid(unsafe_code)]

//! Production verifier and real prepared segments, not a second search model.
use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb_search::{EphemeralIndex, IndexError, IndexLimits, IndexedQueryState, ManifestLimits,
    MembershipState, ParsedQuery, QueryOptions, ReferenceScanOracle, SearchDocument, SearchManifest, SearchManifestId};
use fcb_search::index::export::{OwnedEphemeralIndex, SourceRetentionLimits};
use fcb_search::indexed_query::{OwnedIndexedQuery, MAX_OWNED_QUERY_BYTES};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(29001).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn file(n: u64) -> FileId { FileId::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn owned(parts: &[(&str, &[u8])], membership: MembershipState, missing: &[FileId],
    limits: IndexLimits, budget: &ResourceBudget, id: u64) -> OwnedEphemeralIndex {
    let captures: Vec<_> = parts.iter().enumerate().map(|(i, (_, bytes))|
        CompleteCapture::new(CaptureRequest::new(file(i as u64 + 1), SourceRevision::new(owner(), 7).unwrap()).unwrap(),
            ByteLength::new(bytes.len() as u64), Arc::from(*bytes)).unwrap()).collect();
    let docs: Vec<_> = parts.iter().zip(&captures).map(|((path, _), source)| SearchDocument::new(source.request().file(), path, source)).collect();
    let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), &docs, missing,
        membership, ManifestLimits::default()).unwrap();
    EphemeralIndex::build(manifest, limits, budget, allocation(id), || false).unwrap()
        .into_owned(SourceRetentionLimits::default(), budget, allocation(id + 1), || false).unwrap()
}
fn start(index: &OwnedEphemeralIndex, query: &str, max_matches: usize, budget: &ResourceBudget, id: u64) -> OwnedIndexedQuery {
    OwnedIndexedQuery::new(index, ParsedQuery::parse(query).unwrap(),
        QueryOptions::new(generation(id)).with_max_matches(max_matches), budget,
        [allocation(id), allocation(id + 1)], || false).unwrap()
}
fn finish(cursor: &mut OwnedIndexedQuery, index: &OwnedEphemeralIndex, batch: usize) {
    let generation = cursor.report().generation();
    for _ in 0..=index.captured_files() {
        if cursor.report().is_terminal() { return; }
        cursor.step(index, batch, generation, || false).unwrap();
    }
    panic!("cursor did not terminate");
}

#[test]
fn one_document_steps_preserve_early_hits_and_reuse_prepared_source() {
    let budget = budget();
    let mut index = owned(&[("a.rs", b"needle a"), ("b.rs", b"nothing"), ("c.rs", b"needle c")],
        MembershipState::Closed, &[], IndexLimits::default(), &budget, 1);
    let backing = index.capture(file(1)).unwrap().bytes().as_ptr();
    let stats = index.statistics();
    let mut cursor = start(&index, "needle", 10, &budget, 10);
    assert_eq!(cursor.report().examined_files(), 0);
    assert_eq!(cursor.report().capture_results().scanned_bytes, 0);
    cursor.step(&index, 1, generation(10), || false).unwrap();
    let early = cursor.report().capture_results().matches[0].clone();
    assert_eq!(cursor.report().state(), IndexedQueryState::Running);
    // Existing scoped consumers can still use the same index between steps.
    index.with_index(&budget, allocation(50), || false, |view| {
        assert!(view.may_match(0, &ParsedQuery::parse("needle").unwrap(), &QueryOptions::new(generation(20))).unwrap());
    }).unwrap();
    finish(&mut cursor, &index, 1);
    assert_eq!(cursor.report().capture_results().matches[0], early);
    assert_eq!(cursor.report().capture_results().matches.len(), 2);
    assert_eq!(cursor.report().skipped_by_index(), 1);
    assert!(cursor.report().is_complete());
    assert_eq!(index.statistics(), stats);
    assert_eq!(index.capture(file(1)).unwrap().bytes().as_ptr(), backing);
    let before = cursor.report().capture_results().clone();
    cursor.step(&index, 100, generation(10), || false).unwrap();
    assert_eq!(cursor.report().capture_results(), &before);
}

#[test]
fn borrowed_and_movable_queries_agree_for_utf16_filters_and_lookahead() {
    let utf16: Vec<u8> = "\u{feff}banana needle".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let budget = budget();
    let mut index = owned(&[("src/a.rs", b"banana needle"), ("src/b.py", &utf16),
        ("test.rs", b"banana banned"), ("empty.rs", b"")], MembershipState::Closed, &[],
        IndexLimits::default(), &budget, 1);
    for (i, expression) in ["ana", "needle", "banana -banned", "banana path:src/", "banana lang:rust"].iter().enumerate() {
        let query = ParsedQuery::parse(expression).unwrap();
        let opts = QueryOptions::new(generation(100 + i as u64)).with_max_matches(2);
        let expected = index.with_index(&budget, allocation(80), || false, |view|
            ReferenceScanOracle::scan_collection(view.manifest().documents(), &query, &opts).unwrap()).unwrap();
        let mut cursor = OwnedIndexedQuery::new(&index, query, opts, &budget,
            [allocation(100 + i as u64 * 2), allocation(101 + i as u64 * 2)], || false).unwrap();
        finish(&mut cursor, &index, 1);
        assert_eq!(cursor.report().capture_results().matches, expected.matches, "{expression}");
        assert_eq!(cursor.report().capture_results().total_matches_counted, expected.total_matches_counted, "{expression}");
        assert_eq!(cursor.report().capture_results().coverage, expected.coverage, "{expression}");
    }
}

#[test]
fn equal_numeric_manifests_cannot_rebind_a_paused_cursor() {
    let budget = budget();
    let index = owned(&[("same.rs", b"old needle")], MembershipState::Closed, &[], IndexLimits::default(), &budget, 1);
    let replacement = owned(&[("same.rs", b"new needle")], MembershipState::Closed, &[], IndexLimits::default(), &budget, 3);
    assert_eq!(index.id(), replacement.id());
    let mut cursor = start(&index, "needle", 10, &budget, 10);
    assert_eq!(cursor.step(&replacement, 1, generation(10), || false).err(), Some(IndexError::InvalidManifest));
    assert_eq!(cursor.report().examined_files(), 0);
    finish(&mut cursor, &index, 1);
    assert!(cursor.report().is_complete());
}

#[test]
fn moving_index_and_cursors_between_explicit_workers_needs_no_self_reference() {
    let budget = budget();
    let index = owned(&[("a", b"one"), ("b", b"two")], MembershipState::Closed, &[], IndexLimits::default(), &budget, 1);
    let mut first = start(&index, "one", 10, &budget, 10);
    let mut second = start(&index, "two", 10, &budget, 20);
    first.step(&index, 1, generation(10), || false).unwrap();
    let (index, mut first, second) = std::thread::spawn(move || {
        second.step(&index, 1, generation(20), || false).unwrap();
        (index, first, second)
    }).join().unwrap();
    drop(second);
    finish(&mut first, &index, 1);
    assert_eq!(first.report().capture_results().matches[0].file_id, file(1));
}

#[test]
fn cancellation_and_stale_generation_do_not_restart_or_mutate_the_index() {
    let budget = budget();
    let index = owned(&[("a", b"needle"), ("b", b"needle")], MembershipState::Closed, &[], IndexLimits::default(), &budget, 1);
    let mut cursor = start(&index, "needle", 10, &budget, 10);
    cursor.step(&index, 1, generation(10), || false).unwrap();
    cursor.step(&index, 1, generation(10), || true).unwrap();
    assert_eq!(cursor.report().state(), IndexedQueryState::Canceled);
    cursor.step(&index, 1, generation(10), || false).unwrap();
    assert_eq!(cursor.report().examined_files(), 1);
    let mut next = start(&index, "needle", 10, &budget, 20);
    assert_eq!(next.step(&index, 1, generation(21), || false).err(), Some(IndexError::StaleQuery));
    assert_eq!(next.report().state(), IndexedQueryState::Canceled);
    let mut good = start(&index, "needle", 10, &budget, 30);
    finish(&mut good, &index, 1);
    assert!(good.report().is_complete());
}

#[test]
fn panic_during_verification_retires_only_the_affected_cursor() {
    let budget = budget();
    let index = owned(&[("a", b"needle")], MembershipState::Closed, &[], IndexLimits::default(), &budget, 1);
    let mut cursor = start(&index, "needle", 10, &budget, 10);
    let mut polls = 0;
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cursor.step(&index, 1, generation(10), || { polls += 1; if polls == 3 { panic!("verifier callback"); } false }).unwrap();
    })).is_err());
    assert_eq!(cursor.report().state(), IndexedQueryState::Failed);
    cursor.step(&index, 1, generation(10), || false).unwrap();
    assert_eq!(cursor.report().examined_files(), 0);
    let mut good = start(&index, "needle", 10, &budget, 20);
    finish(&mut good, &index, 1);
    assert!(good.report().is_complete());
}

#[test]
fn unavailable_and_open_membership_survive_owned_reports_and_index_destruction() {
    let budget = budget();
    let index = owned(&[("a", b"needle")], MembershipState::Discovering, &[file(2)], IndexLimits::default(), &budget, 1);
    let mut cursor = start(&index, "needle", 10, &budget, 10);
    finish(&mut cursor, &index, 1);
    let report = cursor.into_report();
    drop(index);
    assert_eq!(report.unavailable_files(), &[file(2)]);
    assert_eq!(report.known_files(), 2);
    assert_eq!(report.examined_files(), 1);
    assert!(!report.is_complete());
    assert_eq!(report.capture_results().matches.len(), 1);
}

#[test]
fn byte_budget_and_uncovered_fallback_are_shared_across_steps() {
    let budget = budget();
    let index = owned(&[("a", b"a needle"), ("b", b"b needle")], MembershipState::Closed, &[],
        IndexLimits { max_total_grams: 0, ..Default::default() }, &budget, 1);
    let mut opts = QueryOptions::new(generation(10)); opts.max_bytes_scanned = Some(9);
    let mut cursor = OwnedIndexedQuery::new(&index, ParsedQuery::parse("needle").unwrap(), opts,
        &budget, [allocation(10), allocation(11)], || false).unwrap();
    finish(&mut cursor, &index, 1);
    assert_eq!(cursor.report().fallback_attempts(), 2);
    assert!(cursor.report().capture_results().scanned_bytes <= 9);
    assert!(!cursor.report().is_complete());
    assert_eq!(cursor.report().capture_results().matches.len(), 1);
}

#[test]
fn one_step_does_not_revisit_repository_metadata_and_zero_is_inert() {
    let budget = budget();
    let parts = vec![("file", b"abc".as_slice()); 1024];
    let index = owned(&parts, MembershipState::Closed, &[], IndexLimits::default(), &budget, 1);
    let mut cursor = start(&index, "missing", 1, &budget, 10);
    cursor.step(&index, 0, generation(10), || false).unwrap();
    assert_eq!(cursor.report().examined_files(), 0);
    let mut polls = 0;
    cursor.step(&index, 1, generation(10), || { polls += 1; false }).unwrap();
    assert_eq!(cursor.report().examined_files(), 1);
    assert!(polls <= 8, "one skip must not rebuild 1024 descriptors: {polls}");
    assert_eq!(cursor.report().capture_results().scanned_bytes, 0);
}

#[test]
fn admission_failure_releases_candidate_state_and_keeps_index_usable() {
    let budget = budget();
    let index = owned(&[("a", b"needle")], MembershipState::Closed, &[], IndexLimits::default(), &budget, 1);
    let before = budget.accounting().reserved();
    let mut ast = ParsedQuery::parse("needle").unwrap();
    ast.raw_query.reserve_exact(MAX_OWNED_QUERY_BYTES + 1);
    assert_eq!(OwnedIndexedQuery::new(&index, ast, QueryOptions::new(generation(10)), &budget,
        [allocation(10), allocation(11)], || false).err(), Some(IndexError::LimitExceeded));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert_eq!(OwnedIndexedQuery::new(&index, ParsedQuery::parse("needle").unwrap(), QueryOptions::new(generation(10)),
        &tiny, [allocation(10), allocation(11)], || false).err(), Some(IndexError::ResourceDenied));
    assert_eq!(budget.accounting().reserved(), before);
    let mut cursor = start(&index, "needle", 10, &budget, 20);
    finish(&mut cursor, &index, 1);
    drop(cursor);
    assert_eq!(budget.accounting().reserved(), before);
}

#[test]
fn empty_universe_terminates_without_verification() {
    let budget = budget();
    let index = owned(&[], MembershipState::Closed, &[], IndexLimits::default(), &budget, 1);
    let mut cursor = start(&index, "needle", 0, &budget, 10);
    cursor.step(&index, 1, generation(10), || false).unwrap();
    assert!(cursor.report().is_complete());
    assert_eq!(cursor.report().scan_attempts(), 0);
}

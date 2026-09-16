#![forbid(unsafe_code)]

use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, RootId};
use fcb_search::{MembershipState, SearchManifestId};
use fcb_search::paths::{MAX_PATH_BYTES, PathEntry, PathIndex, PathIndexLimits, PathSearch,
    PathSearchError, PathSearchOptions, PathSearchState, PathSelection, PathStepBudget, RawPath};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(451).unwrap() }
fn file(id: u64) -> FileId { FileId::new(owner(), id).unwrap() }
fn root() -> RootId { RootId::new(owner(), 1).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn manifest(id: u64) -> SearchManifestId { SearchManifestId::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn build(paths: &[RawPath], budget: &ResourceBudget) -> PathIndex {
    let entries: Vec<_> = paths.iter().enumerate().map(|(i, path)| PathEntry::new(file(i as u64 + 1), root(), path)).collect();
    PathIndex::build(manifest(1), MembershipState::Closed, &entries,
        PathIndexLimits::default(), budget, allocation(1), || false).unwrap()
}

#[test]
fn completing_a_utf8_scalar_is_not_a_sound_raw_byte_prefix_refinement() {
    let budget = budget();
    let index = build(&[RawPath::from_str("é.rs")], &budget);
    let mut first = PathSearch::new(&index, &[0xc3], PathSearchOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    first.run_to_completion(|| false).unwrap();
    assert_eq!(first.matches_seen(), 0);
    let mut next = first.refine("é".as_bytes(), PathSearchOptions::new(generation(2)), &budget, allocation(3)).unwrap();
    assert!(!next.reused_candidates(), "an invalid byte token became a different Unicode scalar");
    next.run_to_completion(|| false).unwrap();
    assert_eq!(next.matches_seen(), 1);
    assert_eq!(next.ranked_matches()[0].file_id(), file(1));
}

#[test]
fn full_width_ids_remain_exact_and_exhausted_generations_cannot_wrap() {
    let budget = budget();
    let high_file = file(u64::MAX);
    let high_root = RootId::new(owner(), u64::MAX).unwrap();
    let raw = RawPath::from_str("last.rs");
    let index = PathIndex::build(manifest(u64::MAX - 1), MembershipState::Closed,
        &[PathEntry::new(high_file, high_root, &raw)], PathIndexLimits::default(),
        &budget, allocation(1), || false).unwrap();
    let mut options = PathSearchOptions::new(generation(u64::MAX));
    options.selected_file = Some(high_file);
    let mut query = PathSearch::new(&index, b"last", options, &budget, allocation(2)).unwrap();
    query.run_to_completion(|| false).unwrap();
    assert_eq!(query.ranked_matches()[0].file_id().get(), u64::MAX);
    assert_eq!(query.ranked_matches()[0].root_id().get(), u64::MAX);
    assert!(matches!(query.selection(), PathSelection::Matched(hit) if hit.file_id() == high_file));
    assert!(matches!(query.refine(b"last.rs", PathSearchOptions::new(generation(1)), &budget, allocation(3)),
        Err(PathSearchError::StaleQuery)));
    let final_index = index.updated(manifest(u64::MAX), MembershipState::Closed, &[], &[],
        &budget, allocation(3), || false).unwrap();
    assert!(matches!(final_index.updated(manifest(1), MembershipState::Closed, &[], &[],
        &budget, allocation(4), || false), Err(PathSearchError::StaleIndex)));
}

#[test]
fn final_ranking_is_step_invariant_while_pending_visible_order_is_explicit() {
    let budget = budget();
    let mut paths = vec![RawPath::from_str("a__b__c.rs")];
    for i in 1..32 { paths.push(RawPath::from_str(&format!("dir_{i}/abc"))); }
    let index = build(&paths, &budget);
    let mut options = PathSearchOptions::new(generation(1)); options.max_results = 3;
    let mut stepped = PathSearch::new(&index, b"abc", options, &budget, allocation(2)).unwrap();
    let one = PathStepBudget { max_candidates: 1, ..Default::default() };
    stepped.step(one, generation(1), || false).unwrap();
    stepped.select(file(1)).unwrap();
    assert!(stepped.ordering_protected());
    assert!(!stepped.has_pending_ordering());
    while stepped.state() == PathSearchState::Running {
        stepped.step(one, generation(1), || false).unwrap();
        assert!(stepped.last_step_units() <= one.max_units);
    }
    assert!(stepped.is_complete());
    assert!(stepped.has_pending_ordering());
    assert_eq!(stepped.visible_matches()[0].file_id(), file(1));
    options.generation = generation(2);
    let mut whole = PathSearch::new(&index, b"abc", options, &budget, allocation(3)).unwrap();
    whole.run_to_completion(|| false).unwrap();
    let summarize = |query: &PathSearch<'_>| query.ranked_matches().iter()
        .map(|hit| (hit.file_id(), hit.rank())).collect::<Vec<_>>();
    assert_eq!(summarize(&stepped), summarize(&whole));
    stepped.release_ordering();
    assert!(!stepped.has_pending_ordering());
    assert!(matches!(stepped.selection(), PathSelection::Matched(hit) if hit.file_id() == file(1)));
}

#[test]
fn maximum_length_path_is_admitted_before_matching_and_releases_its_capacity() {
    let budget = budget();
    let mut bytes = vec![b'x'; MAX_PATH_BYTES - 4]; bytes.extend_from_slice(b"z.rs");
    let raw = RawPath::from_bytes(bytes);
    let index = build(&[raw], &budget);
    let mut options = PathSearchOptions::new(generation(1)); options.max_results = 4096;
    let mut query = PathSearch::new(&index, b"z.rs", options, &budget, allocation(2)).unwrap();
    let units = query.next_work_units();
    assert!(units <= PathStepBudget::default().max_units);
    query.step(PathStepBudget { max_candidates: 1, max_units: units }, generation(1), || false).unwrap();
    assert_eq!(query.last_step_units(), units);
    assert_eq!(query.matches_seen(), 1);
    assert!(query.is_complete());
    drop(query); drop(index);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

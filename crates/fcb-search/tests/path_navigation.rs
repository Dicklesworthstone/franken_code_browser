#![forbid(unsafe_code)]

//! Public path-index/query regressions. No source capture, filesystem, database,
//! runtime, GPU, or third-party matcher participates in these production calls.

use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, RootId};
use fcb_search::{MembershipState, SearchManifestId};
use fcb_search::paths::{PathCase, PathEntry, PathIndex, PathIndexLimits, PathMatchKind,
    PathMatchMode, PathSearch, PathSearchError, PathSearchOptions, PathSearchState,
    PathSelection, PathStepBudget, RawPath};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(251).unwrap() }
fn file(id: u64) -> FileId { FileId::new(owner(), id).unwrap() }
fn root(id: u64) -> RootId { RootId::new(owner(), id).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn manifest(id: u64) -> SearchManifestId { SearchManifestId::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn options(id: u64) -> PathSearchOptions { PathSearchOptions::new(generation(id)) }
fn build(names: &[&[u8]], budget: &ResourceBudget) -> PathIndex {
    let paths: Vec<_> = names.iter().map(|name| RawPath::from_bytes(*name)).collect();
    let entries: Vec<_> = paths.iter().enumerate().map(|(i, path)| PathEntry::new(file(i as u64 + 1), root(1), path)).collect();
    PathIndex::build(manifest(1), MembershipState::Closed, &entries,
        PathIndexLimits::default(), budget, allocation(1), || false).unwrap()
}
fn search<'a>(index: &'a PathIndex, query: &[u8], options: PathSearchOptions, budget: &ResourceBudget) -> PathSearch<'a> {
    let mut result = PathSearch::new(index, query, options, budget, allocation(2)).unwrap();
    result.run_to_completion(|| false).unwrap();
    result
}
fn ids(search: &PathSearch<'_>) -> Vec<FileId> { search.ranked_matches().iter().map(|hit| hit.file_id()).collect() }

#[test]
fn exact_filename_component_prefix_and_subsequence_have_explicit_order() {
    let budget = budget();
    let index = build(&[b"foo", b"foo/bar.rs", b"foobar", b"f_a_o_o.rs"], &budget);
    let result = search(&index, b"foo", options(1), &budget);
    assert_eq!(ids(&result), [file(1), file(2), file(3), file(4)]);
    assert_eq!(result.ranked_matches().iter().map(|hit| hit.rank().kind).collect::<Vec<_>>(),
        [PathMatchKind::ExactFilename, PathMatchKind::ExactComponent,
         PathMatchKind::FilenamePrefix, PathMatchKind::FilenameSubsequence]);
    assert!(result.is_complete());
}

#[test]
fn exact_and_prefix_use_components_without_counting_a_file_multiple_times() {
    let budget = budget();
    let index = build(&[b"alpha/alpha/alpha", b"alpha/beta", b"unrelated.rs"], &budget);
    for mode in [PathMatchMode::Exact, PathMatchMode::Prefix] {
        let mut opts = options(1); opts.mode = mode;
        let result = search(&index, b"alpha", opts, &budget);
        assert_eq!(result.matches_seen(), 2);
        assert_eq!(result.files_examined(), 2);
        assert_eq!(ids(&result), [file(1), file(2)]);
        assert!(result.is_complete());
    }
}

#[test]
fn case_sensitive_identities_and_case_insensitive_lookup_do_not_alias() {
    let budget = budget();
    let index = build(&[b"src/Foo.rs", b"src/foo.rs"], &budget);
    let mut opts = options(1); opts.mode = PathMatchMode::Exact;
    let insensitive = search(&index, b"Foo.rs", opts, &budget);
    assert_eq!(ids(&insensitive), [file(1), file(2)]);
    drop(insensitive);
    opts.case = PathCase::Sensitive;
    let sensitive = search(&index, b"Foo.rs", opts, &budget);
    assert_eq!(ids(&sensitive), [file(1)]);
    assert_ne!(index.get(file(1)).unwrap().raw_path(), index.get(file(2)).unwrap().raw_path());
}

#[test]
fn unicode_lowercase_search_preserves_native_names_and_scalar_boundaries() {
    let budget = budget();
    let index = build(&["résumé.rs".as_bytes(), "Ã©.rs".as_bytes(), "é.rs".as_bytes()], &budget);
    let mut opts = options(1); opts.mode = PathMatchMode::Exact;
    let result = search(&index, "RÉSUMÉ.rs".as_bytes(), opts, &budget);
    assert_eq!(ids(&result), [file(1)]);
    assert_eq!(result.ranked_matches()[0].path().raw_path().as_bytes(), "résumé.rs".as_bytes());
    drop(result);
    let result = search(&index, "é".as_bytes(), options(2), &budget);
    assert!(!ids(&result).contains(&file(2)), "must not match UTF-8 bytes across unrelated scalars");
    assert!(ids(&result).contains(&file(3)));
}

#[test]
fn invalid_native_bytes_are_not_replacement_characters_or_escape_labels() {
    let budget = budget();
    let index = build(&[b"x\xff.rs", "x\u{fffd}.rs".as_bytes(), b"x\\xff.rs", b"src\\file.rs", b"src/file.rs"], &budget);
    let result = search(&index, &[0xff], options(1), &budget);
    assert_eq!(ids(&result), [file(1)]);
    assert_eq!(result.ranked_matches()[0].path().raw_path().as_bytes(), b"x\xff.rs");
    drop(result);
    let mut opts = options(2); opts.mode = PathMatchMode::Exact;
    let result = search(&index, b"src\\file.rs", opts, &budget);
    assert_eq!(ids(&result), [file(4)]);
}

#[test]
fn equal_relative_paths_in_separate_roots_remain_distinct_and_scoped() {
    let budget = budget();
    let raw = RawPath::from_str("lib.rs");
    let entries = [PathEntry::new(file(1), root(1), &raw), PathEntry::new(file(2), root(2), &raw)];
    let index = PathIndex::build(manifest(1), MembershipState::Closed, &entries,
        PathIndexLimits::default(), &budget, allocation(1), || false).unwrap();
    let mut opts = options(1); opts.preferred_root = Some(root(2));
    let result = search(&index, b"lib.rs", opts, &budget);
    assert_eq!(ids(&result), [file(2), file(1)]);
    drop(result);
    opts.scope_root = Some(root(1));
    let result = search(&index, b"lib.rs", opts, &budget);
    assert_eq!(ids(&result), [file(1)]);
}

#[test]
fn recency_breaks_lexical_ties_but_cannot_beat_a_better_match_class() {
    let budget = budget();
    let names: Vec<_> = ["aa/foo.rs", "zz/foo.rs", "foobar.rs"].map(RawPath::from_str).into();
    let mut entries: Vec<_> = names.iter().enumerate().map(|(i, path)| PathEntry::new(file(i as u64 + 1), root(1), path)).collect();
    entries[1].recent_weight = 10;
    entries[2].recent_weight = u16::MAX;
    let index = PathIndex::build(manifest(1), MembershipState::Closed, &entries,
        PathIndexLimits::default(), &budget, allocation(1), || false).unwrap();
    let result = search(&index, b"foo", options(1), &budget);
    assert_eq!(ids(&result)[..2], [file(3), file(2)], "same filename-prefix class permits recency preference");
    drop(result);
    let result = search(&index, b"foo.rs", options(2), &budget);
    assert_eq!(ids(&result)[..2], [file(2), file(1)], "exact filenames beat a high-recency fuzzy file");
}

#[test]
fn atomic_rename_insert_delete_reuses_unmodified_keys_and_keeps_old_readers() {
    let budget = budget();
    let old = build(&[b"a.rs", b"b.rs", b"c.rs"], &budget);
    let renamed = RawPath::from_str("renamed.rs");
    let inserted = RawPath::from_str("new.rs");
    let upserts = [PathEntry::new(file(2), root(1), &renamed), PathEntry::new(file(4), root(1), &inserted)];
    let next = old.updated(manifest(2), MembershipState::Discovering, &upserts, &[file(1)],
        &budget, allocation(3), || false).unwrap();
    assert_eq!(next.len(), 3);
    assert!(next.get(file(1)).is_none());
    assert_eq!(old.get(file(2)).unwrap().raw_path().as_bytes(), b"b.rs");
    assert_eq!(next.get(file(2)).unwrap().raw_path().as_bytes(), b"renamed.rs");
    assert!(std::ptr::eq(old.get(file(3)).unwrap(), next.get(file(3)).unwrap()));
    let old_result = search(&old, b"b.rs", options(1), &budget);
    assert_eq!(ids(&old_result), [file(2)]);
    assert_eq!(old_result.validate_delivery(next.id(), generation(1)), Err(PathSearchError::StaleIndex));
    drop(old_result);
    let result = search(&next, b"renamed", options(2), &budget);
    assert_eq!(ids(&result), [file(2)]);
    assert!(!result.is_complete(), "discovering membership is not a complete workspace answer");
}

#[test]
fn canceled_or_rejected_replacement_keeps_old_membership_and_releases_candidate_charge() {
    let budget = budget();
    let old = build(&[b"a.rs", b"b.rs"], &budget);
    let baseline = budget.accounting().reserved().get();
    let collision = RawPath::from_str("a.rs");
    let upsert = [PathEntry::new(file(2), root(1), &collision)];
    assert!(matches!(old.updated(manifest(2), MembershipState::Closed, &upsert, &[],
        &budget, allocation(3), || false), Err(PathSearchError::DuplicatePath)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    let renamed = RawPath::from_str("new.rs");
    let upsert = [PathEntry::new(file(2), root(1), &renamed)];
    assert!(matches!(old.updated(manifest(2), MembershipState::Closed, &upsert, &[],
        &budget, allocation(3), || budget.accounting().reserved().get() > baseline), Err(PathSearchError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    assert_eq!(old.get(file(2)).unwrap().raw_path().as_bytes(), b"b.rs");
    let denied = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(old.updated(manifest(2), MembershipState::Closed, &upsert, &[],
        &denied, allocation(3), || false), Err(PathSearchError::ResourceDenied)));
}

#[test]
fn partial_discovery_does_not_delete_omitted_files() {
    let budget = budget();
    let old = build(&[b"a.rs", b"b.rs"], &budget);
    let raw = RawPath::from_str("c.rs");
    let next = old.updated(manifest(2), MembershipState::Discovering,
        &[PathEntry::new(file(3), root(1), &raw)], &[], &budget, allocation(3), || false).unwrap();
    assert_eq!(next.paths().map(|path| path.file_id()).collect::<Vec<_>>(), [file(1), file(2), file(3)]);
    drop(old);
    assert!(budget.accounting().reserved().get() > 0);
    drop(next);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn work_allowance_is_checked_before_visiting_a_file_and_resumes_exactly() {
    let budget = budget();
    let index = build(&[b"alpha.rs", b"another.rs"], &budget);
    let mut query = PathSearch::new(&index, b"a", options(1), &budget, allocation(2)).unwrap();
    let needed = query.next_work_units();
    assert_eq!(query.step(PathStepBudget { max_candidates: 1, max_units: needed - 1 }, generation(1), || false),
        Err(PathSearchError::StepBudgetTooSmall));
    assert_eq!(query.files_examined(), 0);
    assert_eq!(query.work_units(), 0);
    query.step(PathStepBudget { max_candidates: 1, max_units: needed }, generation(1), || false).unwrap();
    assert_eq!(query.files_examined(), 1);
    assert_eq!(query.last_step_units(), needed);
    assert_eq!(query.state(), PathSearchState::Running);
    query.run_to_completion(|| false).unwrap();
    assert!(query.is_complete());
    assert_eq!(query.matches_seen(), 2);
}

#[test]
fn selection_and_visible_order_survive_later_higher_ranked_results() {
    let budget = budget();
    let index = build(&[b"f_a_o_o.rs", b"foo"], &budget);
    let mut opts = options(1); opts.max_results = 1;
    let mut query = PathSearch::new(&index, b"foo", opts, &budget, allocation(2)).unwrap();
    query.step(PathStepBudget { max_candidates: 1, ..Default::default() }, generation(1), || false).unwrap();
    query.select(file(1)).unwrap();
    query.run_to_completion(|| false).unwrap();
    assert_eq!(query.visible_matches()[0].file_id(), file(1));
    assert_eq!(query.ranked_matches()[0].file_id(), file(2));
    assert!(matches!(query.selection(), PathSelection::Matched(hit) if hit.file_id() == file(1)));
    query.release_ordering();
    assert_eq!(query.visible_matches()[0].file_id(), file(2));
    assert!(matches!(query.selection(), PathSelection::Matched(hit) if hit.file_id() == file(1)));
    assert_eq!(query.matches_seen(), 2);
    assert!(query.truncated());
}

#[test]
fn refinement_uses_all_candidates_not_just_the_previous_visible_top_k() {
    let budget = budget();
    let index = build(&[b"a.rs", b"abc.rs", b"unrelated.rs"], &budget);
    let mut opts = options(1); opts.max_results = 1;
    let first = search(&index, b"a", opts, &budget);
    assert_eq!(ids(&first), [file(1)]);
    opts.generation = generation(2);
    let mut next = first.refine(b"abc", opts, &budget, allocation(3)).unwrap();
    assert!(next.reused_candidates());
    next.run_to_completion(|| false).unwrap();
    assert_eq!(ids(&next), [file(2)]);
    assert!(next.is_complete());
    // Negative control: a cache made from visible rows would lose this result.
    assert!(!ids(&first).contains(&next.ranked_matches()[0].file_id()));
}

#[test]
fn exact_mode_backspace_case_scope_and_partial_queries_restart_the_universe() {
    let budget = budget();
    let index = build(&[b"foo", b"foobar", b"another.rs"], &budget);
    let mut opts = options(1); opts.mode = PathMatchMode::Exact;
    let first = search(&index, b"foo", opts, &budget);
    opts.generation = generation(2);
    let mut next = first.refine(b"foobar", opts, &budget, allocation(3)).unwrap();
    assert!(!next.reused_candidates());
    next.run_to_completion(|| false).unwrap();
    assert_eq!(ids(&next), [file(2)]);
    drop(next); drop(first);
    let first = search(&index, b"foo", options(1), &budget);
    for (needle, case, scope) in [(b"f".as_slice(), PathCase::UnicodeLowercase, None),
        (b"foob", PathCase::Sensitive, None), (b"foob", PathCase::UnicodeLowercase, Some(root(1)))] {
        let mut opts = options(2); opts.case = case; opts.scope_root = scope;
        let next = first.refine(needle, opts, &budget, allocation(3)).unwrap();
        assert!(!next.reused_candidates());
    }
    drop(first);
    let mut first = PathSearch::new(&index, b"f", options(1), &budget, allocation(2)).unwrap();
    first.step(PathStepBudget { max_candidates: 1, ..Default::default() }, generation(1), || false).unwrap();
    let mut next = first.refine(b"foob", options(2), &budget, allocation(3)).unwrap();
    assert!(!next.reused_candidates());
    next.run_to_completion(|| false).unwrap();
    assert_eq!(ids(&next), [file(2)]);
}

#[test]
fn canceled_and_superseded_queries_cannot_resume_or_claim_completeness() {
    let budget = budget();
    let index = build(&[b"alpha", b"another"], &budget);
    for superseded in [false, true] {
        let mut query = PathSearch::new(&index, b"a", options(1), &budget, allocation(2)).unwrap();
        query.step(PathStepBudget { max_candidates: 1, ..Default::default() }, generation(1), || false).unwrap();
        if superseded {
            assert_eq!(query.step(PathStepBudget::default(), generation(2), || false), Err(PathSearchError::StaleQuery));
        } else { query.step(PathStepBudget::default(), generation(1), || true).unwrap(); }
        query.run_to_completion(|| false).unwrap();
        assert_eq!(query.files_examined(), 1);
        assert_eq!(query.state(), PathSearchState::Canceled);
        assert!(!query.is_complete());
        assert_eq!(query.validate_delivery(index.id(), generation(2)), Err(PathSearchError::StaleQuery));
    }
}

#[test]
fn missing_selection_and_discovering_membership_are_not_silently_replaced() {
    let budget = budget();
    for membership in [MembershipState::Closed, MembershipState::Discovering] {
        let index = PathIndex::build(manifest(1), membership, &[], PathIndexLimits::default(),
            &budget, allocation(1), || false).unwrap();
        let mut opts = options(1); opts.selected_file = Some(file(99));
        let result = search(&index, b"file", opts, &budget);
        assert_eq!(result.is_complete(), membership == MembershipState::Closed);
        match membership {
            MembershipState::Closed => assert!(matches!(result.selection(), PathSelection::Missing(id) if id == file(99))),
            MembershipState::Discovering => assert!(matches!(result.selection(), PathSelection::Pending(id) if id == file(99))),
        }
    }
}

#[test]
fn zero_result_capacity_still_counts_without_allocating_repository_sized_hits() {
    let budget = budget();
    let index = build(&[b"aa", b"ab", b"ac"], &budget);
    let mut opts = options(1); opts.max_results = 0;
    let result = search(&index, b"a", opts, &budget);
    assert!(result.ranked_matches().is_empty());
    assert_eq!(result.matches_seen(), 3);
    assert!(result.truncated());
    assert!(result.is_complete());
}

#[test]
fn malformed_membership_and_unadmitted_queries_are_rejected() {
    let budget = budget();
    for name in [b"".as_slice(), b"/absolute", b"x//y", b"x/../y", b"x/./y", b"x\0y"] {
        let raw = RawPath::from_bytes(name);
        assert!(matches!(PathIndex::build(manifest(1), MembershipState::Closed,
            &[PathEntry::new(file(1), root(1), &raw)], PathIndexLimits::default(),
            &budget, allocation(1), || false), Err(PathSearchError::InvalidUpdate)));
    }
    let index = build(&[b"ok.rs"], &budget);
    assert!(matches!(PathSearch::new(&index, b"", options(1), &budget, allocation(2)), Err(PathSearchError::EmptyQuery)));
    assert!(matches!(PathSearch::new(&index, &[b'a'; 257], options(1), &budget, allocation(2)), Err(PathSearchError::QueryTooLong)));
    let mut opts = options(1);
    opts.generation = QueryGeneration::new(ArenaOwnerId::new(999).unwrap(), 1).unwrap();
    assert!(matches!(PathSearch::new(&index, b"ok", opts, &budget, allocation(2)), Err(PathSearchError::OwnerMismatch)));
    assert!(matches!(index.updated(manifest(1), MembershipState::Closed, &[], &[],
        &budget, allocation(3), || false), Err(PathSearchError::StaleIndex)));
    assert!(matches!(index.updated(manifest(2), MembershipState::Closed, &[], &[file(99)],
        &budget, allocation(3), || false), Err(PathSearchError::MissingFile)));
}

#[test]
fn all_query_modes_agree_with_a_simple_independent_path_scan() {
    let budget = budget();
    let names: Vec<String> = (0..128).map(|i| format!("Dir_{i:03}/{}_{}.rs", ['a', 'b', 'c'][i % 3], i % 7)).collect();
    let refs: Vec<&[u8]> = names.iter().map(|name| name.as_bytes()).collect();
    let index = build(&refs, &budget);
    for case in [PathCase::Sensitive, PathCase::UnicodeLowercase] {
        for mode in [PathMatchMode::Exact, PathMatchMode::Prefix, PathMatchMode::Fuzzy] {
            for needle in ["Dir_000", "dir_0", "a", "a_0.rs", "D0a", "rs", "/", "none"] {
                let mut opts = options(1); opts.mode = mode; opts.case = case; opts.max_results = 128;
                let result = search(&index, needle.as_bytes(), opts, &budget);
                let mut actual = ids(&result); actual.sort_unstable();
                let expected: Vec<_> = names.iter().enumerate().filter_map(|(i, path)| {
                    reference_match(path, needle, mode, case).then_some(file(i as u64 + 1))
                }).collect();
                assert_eq!(actual, expected, "mode={mode:?} case={case:?} needle={needle}");
                assert_eq!(result.matches_seen(), expected.len());
                assert!(result.is_complete());
            }
        }
    }
}
fn reference_match(path: &str, needle: &str, mode: PathMatchMode, case: PathCase) -> bool {
    let normalize = |s: &str| match case {
        PathCase::Sensitive => s.to_owned(),
        PathCase::UnicodeLowercase => s.chars().flat_map(char::to_lowercase).collect(),
    };
    let path = normalize(path); let needle = normalize(needle);
    match mode {
        PathMatchMode::Exact => path == needle || path.split('/').any(|part| part == needle),
        PathMatchMode::Prefix => path.starts_with(&needle) || path.split('/').any(|part| part.starts_with(&needle)),
        PathMatchMode::Fuzzy => {
            let mut chars = path.chars();
            needle.chars().all(|wanted| chars.by_ref().any(|ch| ch == wanted))
        }
    }
}

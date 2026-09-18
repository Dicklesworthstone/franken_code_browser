#![forbid(unsafe_code)]

//! FCB-029.A: publication uses bounded real ResultsState transactions.
//! Native presentation and worker execution are not asserted by these tests.

use fcb::{ArenaOwnerId, FileId};
use fcb_core::QueryGeneration;
use fcb::ui::sidebar::{ResultsState, SearchFailure, SearchResultEntry,
    MAX_UI_QUERY_BYTES, MAX_UI_SEARCH_RESULTS, MAX_UI_RESULT_BYTES};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2901).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn entry(id: u64) -> SearchResultEntry {
    SearchResultEntry { id, file_id: FileId::new(owner(), 1).unwrap(),
        path: "src/main.rs".into(), line_number: 1,
        byte_range: (id * 4, id * 4 + 3), excerpt: "hit".into() }
}
fn populated() -> ResultsState {
    let mut state = ResultsState::new();
    state.publish("old", generation(1), vec![entry(1), entry(2), entry(3)], false).unwrap();
    state.select_next();
    state
}

#[test]
fn publication_replaces_query_rows_generation_and_completeness_together() {
    let mut state = populated();
    state.is_searching = true;
    state.publish("new", generation(2), vec![entry(2), entry(4)], true).unwrap();
    assert_eq!(state.query, "new");
    assert_eq!(state.query_generation, Some(generation(2)));
    assert_eq!(state.selected_result().unwrap().id, 2);
    assert_eq!(state.selected_index, Some(0));
    assert!(state.has_more);
    assert!(!state.is_searching);
}

#[test]
fn selected_occurrence_survives_every_ranking_permutation() {
    let mut state = populated();
    for order in [[1, 2, 3], [1, 3, 2], [2, 1, 3], [2, 3, 1], [3, 1, 2], [3, 2, 1]] {
        state.publish("old", generation(1), order.into_iter().map(entry).collect(), false).unwrap();
        assert_eq!(state.selected_result().unwrap().id, 2);
    }
    state.publish("new", generation(2), vec![entry(7)], false).unwrap();
    assert_eq!(state.selected_result().unwrap().id, 7);
}

#[test]
fn equal_spans_do_not_merge_distinct_occurrence_ids() {
    let mut state = ResultsState::new();
    let first = entry(1);
    let mut second = first.clone(); second.id = 2;
    state.publish("same span", generation(1), vec![first.clone(), second.clone()], false).unwrap();
    state.select_next();
    state.publish("same span", generation(1), vec![second, first], false).unwrap();
    assert_eq!(state.selected_index, Some(0));
    assert_eq!(state.selected_result().unwrap().id, 2);
}

#[test]
fn foreign_owner_and_reversed_range_refuse_the_entire_replacement() {
    for foreign in [true, false] {
        let mut state = populated();
        let before = state.clone();
        let mut bad = entry(9);
        if foreign { bad.file_id = FileId::new(ArenaOwnerId::new(2902).unwrap(), 1).unwrap(); }
        else { bad.byte_range = (9, 3); }
        assert_eq!(state.publish("bad", generation(2), vec![entry(4), bad], true),
            Err(SearchFailure::InvalidResults));
        assert_eq!(state, before);
    }
}

#[test]
fn oversized_rows_and_payloads_preserve_all_published_state() {
    let mut state = populated();
    let before = state.clone();
    let rows = (0..=MAX_UI_SEARCH_RESULTS as u64).map(entry).collect();
    assert_eq!(state.publish("too many", generation(2), rows, true), Err(SearchFailure::ResourceDenied));
    assert_eq!(state, before);
    let mut large = entry(4); large.excerpt = "x".repeat(MAX_UI_RESULT_BYTES);
    assert_eq!(state.publish("too large", generation(3), vec![large], false), Err(SearchFailure::ResourceDenied));
    assert_eq!(state, before);
    assert_eq!(state.publish(&"x".repeat(MAX_UI_QUERY_BYTES + 1), generation(4), Vec::new(), false),
        Err(SearchFailure::ResourceDenied));
    assert_eq!(state, before);
}

#[test]
fn spare_vector_and_string_capacity_counts_toward_retention_limits() {
    let mut state = populated();
    let before = state.clone();
    let rows = Vec::with_capacity(MAX_UI_SEARCH_RESULTS + 1);
    assert_eq!(state.publish("spare rows", generation(2), rows, false), Err(SearchFailure::ResourceDenied));
    let mut row = entry(4);
    row.excerpt = String::with_capacity(MAX_UI_RESULT_BYTES + 1);
    assert_eq!(state.publish("spare text", generation(3), vec![row], false), Err(SearchFailure::ResourceDenied));
    assert_eq!(state, before);
}

#[test]
fn empty_success_is_distinct_from_failure_and_clears_old_selection() {
    let mut state = populated();
    state.publish("nothing", generation(2), Vec::new(), false).unwrap();
    assert_eq!(state.query, "nothing");
    assert_eq!(state.query_generation, Some(generation(2)));
    assert!(state.selected_result().is_none());
    assert!(!state.has_more);
    assert!(!state.is_searching);
}

#[test]
fn public_selection_index_cannot_overflow_keyboard_navigation() {
    let mut state = populated();
    state.selected_index = Some(usize::MAX);
    assert_eq!(state.select_next().unwrap().id, 3);
    state.selected_index = Some(usize::MAX);
    assert_eq!(state.select_prev().unwrap().id, 3);
    state.clear();
    assert!(state.select_next().is_none());
    assert!(state.select_prev().is_none());
}

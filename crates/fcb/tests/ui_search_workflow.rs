#![forbid(unsafe_code)]

//! FCB-028 / FCB-029.A: production reducer and managed-pane workflows.
//! Tests exercise real state transitions; they do not claim native execution.

use fcb::{ArenaOwnerId, FileId};
use fcb_core::QueryGeneration;
use fcb::ui::{FocusTarget, UiAction, UiCommand, UiReducer, UiState};
use fcb::ui::sidebar::{SearchFailure, SearchResultEntry, SidebarPanel, MAX_UI_QUERY_BYTES};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2904).unwrap() }
fn file(id: u64) -> FileId { FileId::new(owner(), id).unwrap() }
fn entry(id: u64) -> SearchResultEntry {
    SearchResultEntry { id, file_id: file(id), path: format!("src/{id}.rs"),
        line_number: id as usize + 1, byte_range: (id * 4, id * 4 + 3), excerpt: "hit".into() }
}
fn begin(state: &mut UiState, query: &str) -> QueryGeneration {
    let out = UiReducer::reduce(state, UiAction::SearchQueryChanged(query.into()), 0);
    let generation = state.active_query_generation.unwrap();
    assert!(out.commands.iter().any(|command| matches!(command,
        UiCommand::DispatchSearch { query_generation, .. } if *query_generation == generation)));
    generation
}
fn publish(state: &mut UiState, generation: QueryGeneration, rows: Vec<SearchResultEntry>) {
    UiReducer::reduce(state, UiAction::SearchResultsReceived {
        query_generation: generation, results: rows, has_more: false,
    }, 0);
}
fn populated() -> (UiState, QueryGeneration) {
    let mut state = UiState::new(owner());
    let generation = begin(&mut state, "old");
    publish(&mut state, generation, vec![entry(1), entry(2), entry(3)]);
    UiReducer::reduce(&mut state, UiAction::SelectNextSearchResult, 0);
    (state, generation)
}

#[test]
fn replacement_keeps_published_label_generation_and_selected_occurrence() {
    let (mut state, old) = populated();
    let rows = state.sidebar.results.results.clone();
    let out = UiReducer::reduce(&mut state, UiAction::SearchQueryChanged("new".into()), 1);
    assert_eq!(state.search_input, "new");
    assert_eq!(state.sidebar.results.query, "old");
    assert_eq!(state.sidebar.results.query_generation, Some(old));
    assert_eq!(state.sidebar.results.results, rows);
    assert_eq!(state.sidebar.results.selected_result().unwrap().id, 2);
    assert!(state.sidebar.results.is_searching);
    assert!(out.commands.contains(&UiCommand::CancelSearch { query_generation: old }));
}

#[test]
fn rapid_typing_rejects_out_of_order_success_and_failure() {
    let (mut state, old) = populated();
    let first = begin(&mut state, "first");
    let second = begin(&mut state, "second");
    publish(&mut state, first, vec![entry(9)]);
    UiReducer::reduce(&mut state, UiAction::SearchResultsFailed {
        query_generation: first, failure: SearchFailure::Unavailable,
    }, 0);
    assert_eq!(state.sidebar.results.query_generation, Some(old));
    assert_eq!(state.search_error, None);
    assert!(state.sidebar.results.is_searching);
    publish(&mut state, second, vec![entry(2), entry(4)]);
    assert_eq!(state.sidebar.results.query, "second");
    assert_eq!(state.sidebar.results.query_generation, Some(second));
    assert_eq!(state.sidebar.results.selected_result().unwrap().id, 2);
}

#[test]
fn failed_refinement_keeps_old_rows_and_rejects_late_success() {
    for failure in [SearchFailure::Unavailable, SearchFailure::Canceled, SearchFailure::ResourceDenied] {
        let (mut state, old) = populated();
        let request = begin(&mut state, "unavailable");
        UiReducer::reduce(&mut state, UiAction::SearchResultsFailed {
            query_generation: request, failure,
        }, 0);
        assert_eq!(state.sidebar.results.query_generation, Some(old));
        assert_eq!(state.sidebar.results.selected_result().unwrap().id, 2);
        assert_eq!(state.search_error, Some(failure));
        assert!(!state.sidebar.results.is_searching);
        publish(&mut state, request, Vec::new());
        assert_eq!(state.sidebar.results.results.len(), 3);
        assert_eq!(state.sidebar.results.query, "old");
    }
}

#[test]
fn empty_query_clears_without_dispatch_and_never_reuses_a_generation() {
    let (mut state, old) = populated();
    let out = UiReducer::reduce(&mut state, UiAction::SearchQueryChanged(String::new()), 0);
    assert!(state.sidebar.results.results.is_empty());
    assert!(state.sidebar.results.query_generation.is_none());
    assert!(state.active_query_generation.is_none());
    assert!(state.search_input.is_empty());
    assert!(!out.commands.iter().any(|c| matches!(c, UiCommand::DispatchSearch { .. })));
    assert!(out.commands.contains(&UiCommand::CancelSearch { query_generation: old }));
    publish(&mut state, old, vec![entry(1)]);
    assert!(state.sidebar.results.results.is_empty());
    assert!(begin(&mut state, "old").get() > old.get());
}

#[test]
fn event_ring_counter_is_not_a_query_identity_allocator() {
    let mut state = UiState::new(owner());
    state.seq_counter = u64::MAX;
    let first = begin(&mut state, "one");
    state.seq_counter = 0;
    let second = begin(&mut state, "two");
    assert_eq!(second.get(), first.get() + 1);
}

#[test]
fn foreign_owner_results_and_oversized_drafts_cannot_replace_usable_rows() {
    let (mut state, old) = populated();
    let generation = begin(&mut state, "bad batch");
    let mut bad = entry(9);
    bad.file_id = FileId::new(ArenaOwnerId::new(2905).unwrap(), 1).unwrap();
    publish(&mut state, generation, vec![entry(4), bad]);
    assert_eq!(state.search_error, Some(SearchFailure::InvalidResults));
    assert_eq!(state.sidebar.results.query_generation, Some(old));
    assert!(state.active_query_generation.is_none());
    let out = UiReducer::reduce(&mut state,
        UiAction::SearchQueryChanged("x".repeat(MAX_UI_QUERY_BYTES + 1)), 0);
    assert_eq!(state.search_error, Some(SearchFailure::InvalidQuery));
    assert!(!out.commands.iter().any(|c| matches!(c, UiCommand::DispatchSearch { .. })));
    assert_eq!(state.sidebar.results.selected_result().unwrap().id, 2);
}

#[test]
fn palette_and_results_arrows_do_not_steal_other_focus() {
    let (mut state, _) = populated();
    for target in [FocusTarget::SearchPalette, FocusTarget::Sidebar(SidebarPanel::Results)] {
        UiReducer::reduce(&mut state, UiAction::FocusChange(target), 0);
        UiReducer::reduce(&mut state, UiAction::KeyboardShortcut("ArrowDown".into()), 0);
        assert_eq!(state.sidebar.results.selected_result().unwrap().id, 3);
        UiReducer::reduce(&mut state, UiAction::KeyboardShortcut("ArrowUp".into()), 0);
        assert_eq!(state.sidebar.results.selected_result().unwrap().id, 2);
        assert_eq!(state.focus.current(), target);
    }
    UiReducer::reduce(&mut state, UiAction::FocusChange(FocusTarget::Atlas), 0);
    UiReducer::reduce(&mut state, UiAction::KeyboardShortcut("ArrowDown".into()), 0);
    assert_eq!(state.sidebar.results.selected_result().unwrap().id, 2);
}

#[test]
fn enter_during_pending_replacement_opens_managed_pane_and_escape_restores_focus() {
    let (mut state, old) = populated();
    UiReducer::reduce(&mut state, UiAction::FocusChange(FocusTarget::TreeProjection), 0);
    UiReducer::reduce(&mut state, UiAction::OpenSearchPalette, 0);
    let pending = begin(&mut state, "replacement");
    let out = UiReducer::reduce(&mut state, UiAction::KeyboardShortcut("Enter".into()), 0);
    let pane = state.reading_panes.active_pane().unwrap();
    assert_eq!(pane.file_id, file(2));
    assert_eq!(pane.target_line, Some(3));
    assert_eq!(state.reading_panes.len(), 1);
    assert_eq!(state.sidebar.inspector.file_id, Some(file(2)));
    assert_eq!(state.sidebar.history.entries.last().unwrap().path, "src/2.rs");
    assert!(state.breadcrumbs.as_display_string().ends_with("2.rs"));
    assert_eq!(state.sidebar.results.query_generation, Some(old));
    assert_eq!(state.active_query_generation, Some(pending));
    assert!(!state.search_palette_open);
    assert_eq!(out.commands.iter().filter(|c| matches!(c, UiCommand::RequestCapture { .. })).count(), 1);
    UiReducer::reduce(&mut state, UiAction::KeyboardShortcut("Escape".into()), 0);
    assert!(state.reading_panes.is_empty());
    assert!(!state.reading_lens_open);
    assert_eq!(state.focus.current(), FocusTarget::TreeProjection);
}

#[test]
fn pinned_reader_survives_search_activation_and_temporary_pane_dismissal() {
    let (mut state, _) = populated();
    UiReducer::reduce(&mut state, UiAction::OpenReadingLens {
        file_id: file(9), path: "pinned.rs".into(), line: Some(17),
    }, 0);
    let pinned = state.reading_panes.active_pane_id().unwrap();
    UiReducer::reduce(&mut state, UiAction::PinReadingPane(pinned), 0);
    UiReducer::reduce(&mut state, UiAction::OpenSearchPalette, 0);
    UiReducer::reduce(&mut state, UiAction::KeyboardShortcut("Enter".into()), 0);
    assert_eq!(state.reading_panes.len(), 2);
    UiReducer::reduce(&mut state, UiAction::KeyboardShortcut("Escape".into()), 0);
    assert_eq!(state.reading_panes.len(), 1);
    assert_eq!(state.reading_panes.active_pane_id(), Some(pinned));
    assert_eq!(state.reading_lens_file.as_ref().unwrap().0, file(9));
    assert_eq!(state.focus.current(), FocusTarget::ReadingLens);
}

#[test]
fn changing_selected_file_does_not_relabel_old_inspector_facts() {
    let mut state = UiState::new(owner());
    UiReducer::reduce(&mut state, UiAction::SelectFile { file_id: file(1), path: "old.rs".into() }, 0);
    state.sidebar.inspector.language = Some("Rust".into());
    state.sidebar.inspector.line_count = Some(999);
    state.sidebar.outline.file_path = Some("old.rs".into());
    UiReducer::reduce(&mut state, UiAction::SelectFile { file_id: file(2), path: "new.py".into() }, 0);
    assert_eq!(state.sidebar.inspector.file_id, Some(file(2)));
    assert!(state.sidebar.inspector.language.is_none());
    assert!(state.sidebar.inspector.line_count.is_none());
    assert!(state.sidebar.outline.file_path.is_none());
    assert_eq!(state.focus.current(), FocusTarget::Atlas);
}

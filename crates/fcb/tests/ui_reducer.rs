#![forbid(unsafe_code)]

//! Production unit and boundary tests for UI reducer, sidebar, focus model,
//! and command routing (FCB-020.A / fcb-vpz.1).
//!
//! Required verification cases:
//! 1. `focus_separate_from_selection_invariance` — selection changes do not steal focus; focus changes preserve selection.
//! 2. `predictable_focus_return_stack_on_overlay_dismissal` — overlay dismissal (SearchPalette/ReadingLens) returns to exact prior focus.
//! 3. `tab_ring_navigation_cycle` — forward and backward cycling across main surfaces with and without lens.
//! 4. `sidebar_panels_and_inspector_facts` — panel switching, certainty states (authoritative, heuristic, unknown), and width clamping.
//! 5. `search_results_generation_validation_and_stale_rejection` — negative control: stale query generation rejected; latest accepted.
//! 6. `scope_breadcrumbs_parsing_and_navigation` — path parsing, left/right traversal, and truncation zoom.
//! 7. `conventional_tree_projection_and_keyboard_navigation` — visible row flattening, arrow traversal, and leaf expansion refusal.
//! 8. `navigation_history_stack_and_back_forward` — visited file tracking, chronological back/forward, and forward truncation on branch.
//! 9. `real_repository_journey_by_pointer_and_keyboard` — full end-to-end user loop across tree, inspector, search, lens, and back.
//! 10. `bounded_event_ring_retention_and_no_bulk_drop` — 300 events in 256-capacity ring without unbounded growth.

use fcb::{
    ui::{
        FactCertainty, FocusDirection, FocusStack, FocusTarget, InspectorFact,
        SearchResultEntry, SidebarPanel, TreeError, TreeNode, TreeNodeKind, TreeProjection,
        UiAction, UiCommand, UiEventKind, UiReducer, UiState,
    },
    ArenaOwnerId, FileId,
};

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(930).unwrap()
}

fn file(id: u64) -> FileId {
    FileId::new(owner(), id).unwrap()
}

#[test]
fn focus_separate_from_selection_invariance() {
    let mut state = UiState::new(owner());
    assert_eq!(state.focus.current(), FocusTarget::Atlas);
    assert_eq!(state.selected_file, None);

    // 1. Selecting a file MUST NOT change focus
    let outcome = UiReducer::reduce(
        &mut state,
        UiAction::SelectFile {
            file_id: file(1),
            path: "crates/fcb/src/lib.rs".to_string(),
        },
        1000,
    );

    assert_eq!(state.focus.current(), FocusTarget::Atlas);
    assert_eq!(
        state.selected_file,
        Some((file(1), "crates/fcb/src/lib.rs".to_string()))
    );
    assert!(outcome.changed);

    // 2. Changing focus MUST NOT alter the selected file
    let outcome2 = UiReducer::reduce(
        &mut state,
        UiAction::FocusChange(FocusTarget::Sidebar(SidebarPanel::Inspector)),
        2000,
    );

    assert_eq!(
        state.focus.current(),
        FocusTarget::Sidebar(SidebarPanel::Inspector)
    );
    assert_eq!(
        state.selected_file,
        Some((file(1), "crates/fcb/src/lib.rs".to_string()))
    );
    assert!(outcome2.changed);

    // 3. Clearing selection does not change focus
    let outcome3 = UiReducer::reduce(&mut state, UiAction::ClearSelection, 3000);
    assert_eq!(
        state.focus.current(),
        FocusTarget::Sidebar(SidebarPanel::Inspector)
    );
    assert_eq!(state.selected_file, None);
    assert!(outcome3.changed);
}

#[test]
fn predictable_focus_return_stack_on_overlay_dismissal() {
    let mut state = UiState::new(owner());

    // Navigate to TreeProjection
    UiReducer::reduce(
        &mut state,
        UiAction::FocusChange(FocusTarget::TreeProjection),
        1000,
    );
    assert_eq!(state.focus.current(), FocusTarget::TreeProjection);

    // Open SearchPalette (modal)
    UiReducer::reduce(&mut state, UiAction::OpenSearchPalette, 2000);
    assert_eq!(state.focus.current(), FocusTarget::SearchPalette);
    assert!(state.search_palette_open);

    // Dismiss with Escape shortcut -> focus MUST return to TreeProjection
    UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Escape".to_string()),
        3000,
    );
    assert!(!state.search_palette_open);
    assert_eq!(state.focus.current(), FocusTarget::TreeProjection);

    // Open ReadingLens
    UiReducer::reduce(
        &mut state,
        UiAction::OpenReadingLens {
            file_id: file(2),
            path: "src/main.rs".to_string(),
            line: Some(42),
        },
        4000,
    );
    assert_eq!(state.focus.current(), FocusTarget::ReadingLens);
    assert!(state.reading_lens_open);

    // Close ReadingLens with Escape -> focus MUST return to TreeProjection
    UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Escape".to_string()),
        5000,
    );
    assert!(!state.reading_lens_open);
    assert_eq!(state.focus.current(), FocusTarget::TreeProjection);

    // Test bounded capacity of FocusStack (does not allocate unboundedly or crash)
    let mut stack = FocusStack::new(4);
    for i in 0..10 {
        let panel = match i % 4 {
            0 => SidebarPanel::Inspector,
            1 => SidebarPanel::Results,
            2 => SidebarPanel::History,
            _ => SidebarPanel::Outline,
        };
        stack.push(FocusTarget::Sidebar(panel));
    }
    assert_eq!(stack.len(), 4);
}

#[test]
fn tab_ring_navigation_cycle() {
    let mut state = UiState::new(owner());
    assert_eq!(state.focus.current(), FocusTarget::Atlas);

    // Forward tab ring navigation (lens closed):
    // Atlas -> Breadcrumbs -> TreeProjection -> Sidebar(Inspector) -> Atlas
    let t1 = state.focus.navigate_tab(FocusDirection::Next, false);
    assert_eq!(t1, FocusTarget::Breadcrumbs);

    let t2 = state.focus.navigate_tab(FocusDirection::Next, false);
    assert_eq!(t2, FocusTarget::TreeProjection);

    let t3 = state.focus.navigate_tab(FocusDirection::Next, false);
    assert_eq!(t3, FocusTarget::Sidebar(SidebarPanel::Inspector));

    let t4 = state.focus.navigate_tab(FocusDirection::Next, false);
    assert_eq!(t4, FocusTarget::Atlas);

    // Backward tab ring navigation (lens open):
    // Atlas -> ReadingLens -> Sidebar(Inspector) -> TreeProjection -> Breadcrumbs -> Atlas
    let b1 = state.focus.navigate_tab(FocusDirection::Prev, true);
    assert_eq!(b1, FocusTarget::ReadingLens);

    let b2 = state.focus.navigate_tab(FocusDirection::Prev, true);
    assert_eq!(b2, FocusTarget::Sidebar(SidebarPanel::Inspector));

    let b3 = state.focus.navigate_tab(FocusDirection::Prev, true);
    assert_eq!(b3, FocusTarget::TreeProjection);

    let b4 = state.focus.navigate_tab(FocusDirection::Prev, true);
    assert_eq!(b4, FocusTarget::Breadcrumbs);

    let b5 = state.focus.navigate_tab(FocusDirection::Prev, true);
    assert_eq!(b5, FocusTarget::Atlas);
}

#[test]
fn sidebar_panels_and_inspector_facts() {
    let mut state = UiState::new(owner());
    assert_eq!(state.sidebar.active_panel, SidebarPanel::Inspector);

    // Switch sidebar panels across all distinct targets
    for panel in [
        SidebarPanel::Results,
        SidebarPanel::History,
        SidebarPanel::Outline,
        SidebarPanel::Inspector,
    ] {
        let outcome = UiReducer::reduce(&mut state, UiAction::SelectSidebarTab(panel), 1000);
        assert_eq!(state.sidebar.active_panel, panel);
        assert!(outcome.changed);
        assert_eq!(
            outcome.emitted_events,
            vec![UiEventKind::PanelChanged { panel }]
        );
    }

    // Populate Inspector facts with explicit certainty
    state.sidebar.inspector.facts.push(InspectorFact {
        key: "language".to_string(),
        label: "Language".to_string(),
        value: Some("Rust (2024)".to_string()),
        certainty: FactCertainty::Authoritative,
    });
    state.sidebar.inspector.facts.push(InspectorFact {
        key: "symbols".to_string(),
        label: "Estimated Symbols".to_string(),
        value: Some("42".to_string()),
        certainty: FactCertainty::Heuristic {
            reason: "Regex pattern match without AST parse".to_string(),
        },
    });
    state.sidebar.inspector.facts.push(InspectorFact {
        key: "doc_coverage".to_string(),
        label: "Documentation Coverage".to_string(),
        value: None,
        certainty: FactCertainty::Unknown,
    });

    assert_eq!(state.sidebar.inspector.facts.len(), 3);
    assert_eq!(
        state.sidebar.inspector.facts[0].certainty,
        FactCertainty::Authoritative
    );
    assert!(matches!(
        state.sidebar.inspector.facts[1].certainty,
        FactCertainty::Heuristic { .. }
    ));
    assert_eq!(
        state.sidebar.inspector.facts[2].certainty,
        FactCertainty::Unknown
    );

    // Sidebar width clamping (min 200, max 800)
    UiReducer::reduce(&mut state, UiAction::SetSidebarWidth(100.0), 2000);
    assert_eq!(state.sidebar.width, 200.0);

    UiReducer::reduce(&mut state, UiAction::SetSidebarWidth(1200.0), 3000);
    assert_eq!(state.sidebar.width, 800.0);

    UiReducer::reduce(&mut state, UiAction::SetSidebarWidth(450.0), 4000);
    assert_eq!(state.sidebar.width, 450.0);

    // Sidebar toggle collapse
    assert!(!state.sidebar.is_collapsed);
    UiReducer::reduce(&mut state, UiAction::ToggleSidebar, 5000);
    assert!(state.sidebar.is_collapsed);
    UiReducer::reduce(&mut state, UiAction::ToggleSidebar, 6000);
    assert!(!state.sidebar.is_collapsed);
}

#[test]
fn search_results_generation_validation_and_stale_rejection() {
    let mut state = UiState::new(owner());

    // 1. Dispatch first search query "alpha"
    let out1 = UiReducer::reduce(
        &mut state,
        UiAction::SearchQueryChanged("alpha".to_string()),
        1000,
    );
    assert!(out1.changed);
    assert!(state.sidebar.results.is_searching);
    let gen_alpha = state.active_query_generation.unwrap();

    // 2. User quickly types next query "beta" before "alpha" arrives
    let out2 = UiReducer::reduce(
        &mut state,
        UiAction::SearchQueryChanged("beta".to_string()),
        2000,
    );
    assert!(out2.changed);
    let gen_beta = state.active_query_generation.unwrap();
    assert_ne!(gen_alpha, gen_beta);

    // 3. Stale results for "alpha" arrive from slow worker -> MUST BE REJECTED!
    let stale_results = vec![SearchResultEntry {
        id: 1,
        file_id: file(10),
        path: "stale.rs".to_string(),
        line_number: 5,
        byte_range: (10, 20),
        excerpt: "fn alpha() {}".to_string(),
    }];

    let out_stale = UiReducer::reduce(
        &mut state,
        UiAction::SearchResultsReceived {
            query_generation: gen_alpha,
            results: stale_results,
            has_more: false,
        },
        3000,
    );

    // Invariant: Stale batch rejected; ResultsState remains empty; event logged!
    assert!(!out_stale.changed);
    assert!(state.sidebar.results.results.is_empty());
    assert_eq!(
        out_stale.emitted_events,
        vec![UiEventKind::StaleResultsRejected {
            current_gen: Some(gen_beta.get()),
            rejected_gen: gen_alpha.get(),
        }]
    );

    // 4. Current results for "beta" arrive -> ACCEPTED
    let fresh_results = vec![
        SearchResultEntry {
            id: 2,
            file_id: file(20),
            path: "beta1.rs".to_string(),
            line_number: 10,
            byte_range: (30, 40),
            excerpt: "fn beta_one() {}".to_string(),
        },
        SearchResultEntry {
            id: 3,
            file_id: file(21),
            path: "beta2.rs".to_string(),
            line_number: 25,
            byte_range: (50, 60),
            excerpt: "fn beta_two() {}".to_string(),
        },
    ];

    let out_fresh = UiReducer::reduce(
        &mut state,
        UiAction::SearchResultsReceived {
            query_generation: gen_beta,
            results: fresh_results.clone(),
            has_more: false,
        },
        4000,
    );

    assert!(out_fresh.changed);
    assert_eq!(state.sidebar.results.results.len(), 2);
    assert_eq!(state.sidebar.results.selected_index, Some(0));
    assert_eq!(state.sidebar.results.results, fresh_results);

    // 5. Result selection navigation (next, previous)
    UiReducer::reduce(&mut state, UiAction::SelectNextSearchResult, 5000);
    assert_eq!(state.sidebar.results.selected_index, Some(1));

    UiReducer::reduce(&mut state, UiAction::SelectPrevSearchResult, 6000);
    assert_eq!(state.sidebar.results.selected_index, Some(0));

    // 6. Activating search result opens reading lens at matching line
    let out_act = UiReducer::reduce(&mut state, UiAction::ActivateSearchResult, 7000);
    assert!(out_act.changed);
    assert!(state.reading_lens_open);
    assert_eq!(
        state.reading_lens_file,
        Some((file(20), "beta1.rs".to_string(), Some(10)))
    );
    assert_eq!(state.focus.current(), FocusTarget::ReadingLens);
}

#[test]
fn scope_breadcrumbs_parsing_and_navigation() {
    let mut state = UiState::new(owner());

    // Parse deep path
    state
        .breadcrumbs
        .set_from_path("crates/fcb/src/ui/reducer.rs");
    assert_eq!(
        state.breadcrumbs.as_display_string(),
        "Root › crates › fcb › src › ui › reducer.rs"
    );
    assert_eq!(state.breadcrumbs.segments.len(), 6);
    assert_eq!(state.breadcrumbs.focused_index, Some(5));

    // Navigate left across breadcrumbs
    let seg4 = state.breadcrumbs.navigate_left().unwrap();
    assert_eq!(seg4.label, "ui");

    let seg3 = state.breadcrumbs.navigate_left().unwrap();
    assert_eq!(seg3.label, "src");

    // Navigate right
    let seg_back = state.breadcrumbs.navigate_right().unwrap();
    assert_eq!(seg_back.label, "ui");

    // Click/select segment at index 2 ("fcb")
    let out = UiReducer::reduce(&mut state, UiAction::BreadcrumbSelect(2), 1000);
    assert!(out.changed);
    assert_eq!(
        state.breadcrumbs.as_display_string(),
        "Root › crates › fcb"
    );
    assert_eq!(
        out.commands,
        vec![
            UiCommand::RequestCameraFocus {
                target_description: "fcb".to_string(),
            },
            UiCommand::RequestRedraw,
        ]
    );
}

#[test]
fn conventional_tree_projection_and_keyboard_navigation() {
    let nodes = vec![
        TreeNode {
            id: 1,
            parent_id: None,
            name: "src".to_string(),
            path: "src".to_string(),
            kind: TreeNodeKind::Directory,
            depth: 0,
            is_expanded: true,
            file_id: None,
        },
        TreeNode {
            id: 2,
            parent_id: Some(1),
            name: "lib.rs".to_string(),
            path: "src/lib.rs".to_string(),
            kind: TreeNodeKind::File,
            depth: 1,
            is_expanded: false,
            file_id: Some(file(1)),
        },
        TreeNode {
            id: 3,
            parent_id: Some(1),
            name: "ui".to_string(),
            path: "src/ui".to_string(),
            kind: TreeNodeKind::Directory,
            depth: 1,
            is_expanded: false,
            file_id: None,
        },
        TreeNode {
            id: 4,
            parent_id: Some(3),
            name: "mod.rs".to_string(),
            path: "src/ui/mod.rs".to_string(),
            kind: TreeNodeKind::File,
            depth: 2,
            is_expanded: false,
            file_id: Some(file(2)),
        },
    ];

    let mut tree = TreeProjection::with_nodes(nodes);

    // Initial visible rows (id 3 "ui" is collapsed, so id 4 "mod.rs" is hidden):
    assert_eq!(tree.visible_rows(), &[1, 2, 3]);

    // Negative control: cannot expand a file!
    assert_eq!(tree.expand(2), Err(TreeError::CannotExpandFile));

    // Expand directory "ui" (id 3)
    let ok = tree.expand(3);
    assert_eq!(ok, Ok(()));
    assert_eq!(tree.visible_rows(), &[1, 2, 3, 4]);

    // Keyboard navigation:
    // Move down from 1 -> 2
    assert_eq!(tree.focused_id(), Some(1));
    let n2 = tree.move_down().unwrap();
    assert_eq!(n2.id, 2);

    // Move down from 2 -> 3
    let n3 = tree.move_down().unwrap();
    assert_eq!(n3.id, 3);

    // Move left on expanded directory 3 -> collapses it
    let n3_collapsed = tree.move_left().unwrap();
    assert_eq!(n3_collapsed.id, 3);
    assert!(!tree.get_node(3).unwrap().is_expanded);
    assert_eq!(tree.visible_rows(), &[1, 2, 3]);

    // Move right on collapsed directory 3 -> expands it
    let n3_expanded = tree.move_right().unwrap();
    assert_eq!(n3_expanded.id, 3);
    assert!(tree.get_node(3).unwrap().is_expanded);
    assert_eq!(tree.visible_rows(), &[1, 2, 3, 4]);
}

#[test]
fn navigation_history_stack_and_back_forward() {
    let mut state = UiState::new(owner());

    // Visit File A, File B, File C
    UiReducer::reduce(
        &mut state,
        UiAction::SelectFile {
            file_id: file(1),
            path: "a.rs".to_string(),
        },
        1000,
    );
    UiReducer::reduce(
        &mut state,
        UiAction::SelectFile {
            file_id: file(2),
            path: "b.rs".to_string(),
        },
        2000,
    );
    UiReducer::reduce(
        &mut state,
        UiAction::SelectFile {
            file_id: file(3),
            path: "c.rs".to_string(),
        },
        3000,
    );

    assert_eq!(state.sidebar.history.entries.len(), 3);
    assert!(state.sidebar.history.can_go_back());
    assert!(!state.sidebar.history.can_go_forward());

    // Back -> B
    let out_b = UiReducer::reduce(&mut state, UiAction::HistoryNavigateBack, 4000);
    assert!(out_b.changed);
    assert_eq!(state.breadcrumbs.as_display_string(), "Root › b.rs");
    assert!(state.sidebar.history.can_go_back());
    assert!(state.sidebar.history.can_go_forward());

    // Back -> A
    let out_a = UiReducer::reduce(&mut state, UiAction::HistoryNavigateBack, 5000);
    assert!(out_a.changed);
    assert_eq!(state.breadcrumbs.as_display_string(), "Root › a.rs");
    assert!(!state.sidebar.history.can_go_back());
    assert!(state.sidebar.history.can_go_forward());

    // Forward -> B
    let out_fwd = UiReducer::reduce(&mut state, UiAction::HistoryNavigateForward, 6000);
    assert!(out_fwd.changed);
    assert_eq!(state.breadcrumbs.as_display_string(), "Root › b.rs");

    // Branching: selecting D from B truncates forward history (C is dropped)
    UiReducer::reduce(
        &mut state,
        UiAction::SelectFile {
            file_id: file(4),
            path: "d.rs".to_string(),
        },
        7000,
    );
    assert_eq!(state.sidebar.history.entries.len(), 3); // A, B, D
    assert_eq!(state.sidebar.history.entries[2].path, "d.rs");
    assert!(!state.sidebar.history.can_go_forward());
}

#[test]
fn real_repository_journey_by_pointer_and_keyboard() {
    let mut state = UiState::new(owner());

    // Step 1: User clicks on Tree to focus it
    let out1 = UiReducer::reduce(
        &mut state,
        UiAction::PointerClick {
            target: FocusTarget::TreeProjection,
            select: true,
        },
        1000,
    );
    assert_eq!(state.focus.current(), FocusTarget::TreeProjection);
    assert!(out1.changed);

    // Step 2: User selects a file via Tree
    let nodes = vec![
        TreeNode {
            id: 10,
            parent_id: None,
            name: "crates".to_string(),
            path: "crates".to_string(),
            kind: TreeNodeKind::Directory,
            depth: 0,
            is_expanded: true,
            file_id: None,
        },
        TreeNode {
            id: 11,
            parent_id: Some(10),
            name: "lib.rs".to_string(),
            path: "crates/lib.rs".to_string(),
            kind: TreeNodeKind::File,
            depth: 1,
            is_expanded: false,
            file_id: Some(file(50)),
        },
    ];
    state.tree = TreeProjection::with_nodes(nodes);

    let out2 = UiReducer::reduce(&mut state, UiAction::TreeSelect(11), 2000);
    assert!(out2.changed);
    assert_eq!(
        state.selected_file,
        Some((file(50), "crates/lib.rs".to_string()))
    );
    assert_eq!(
        state.breadcrumbs.as_display_string(),
        "Root › crates › lib.rs"
    );

    // Step 3: User presses Cmd+B to toggle sidebar collapsed
    let out3 = UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Cmd+B".to_string()),
        3000,
    );
    assert!(out3.changed);
    assert!(state.sidebar.is_collapsed);

    // Toggle back
    UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Cmd+B".to_string()),
        3500,
    );
    assert!(!state.sidebar.is_collapsed);

    // Step 4: User opens SearchPalette via Cmd+P
    let out4 = UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Cmd+P".to_string()),
        4000,
    );
    assert!(out4.changed);
    assert!(state.search_palette_open);
    assert_eq!(state.focus.current(), FocusTarget::SearchPalette);

    // Step 5: User types search query
    let out5 = UiReducer::reduce(
        &mut state,
        UiAction::SearchQueryChanged("pub struct".to_string()),
        5000,
    );
    assert!(out5.changed);
    let search_gen = state.active_query_generation.unwrap();

    // Step 6: Worker delivers search results
    let results = vec![SearchResultEntry {
        id: 100,
        file_id: file(50),
        path: "crates/lib.rs".to_string(),
        line_number: 14,
        byte_range: (200, 220),
        excerpt: "pub struct UiState".to_string(),
    }];

    UiReducer::reduce(
        &mut state,
        UiAction::SearchResultsReceived {
            query_generation: search_gen,
            results,
            has_more: false,
        },
        6000,
    );

    // Step 7: Activate result -> opens ReadingLens at line 14
    let out7 = UiReducer::reduce(&mut state, UiAction::ActivateSearchResult, 7000);
    assert!(out7.changed);
    assert!(state.reading_lens_open);
    assert_eq!(
        state.reading_lens_file,
        Some((file(50), "crates/lib.rs".to_string(), Some(14)))
    );
    assert_eq!(state.focus.current(), FocusTarget::ReadingLens);

    // Step 8: User presses Escape -> closes ReadingLens, focus returns to TreeProjection!
    let out8 = UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Escape".to_string()),
        8000,
    );
    assert!(out8.changed);
    assert!(!state.reading_lens_open);
    assert_eq!(state.focus.current(), FocusTarget::TreeProjection);
}

#[test]
fn bounded_event_ring_retention_and_no_bulk_drop() {
    let mut state = UiState::new(owner());

    // Dispatch 300 actions through reducer
    for i in 1..=300 {
        let action = if i % 2 == 0 {
            UiAction::FocusChange(FocusTarget::Atlas)
        } else {
            UiAction::FocusChange(FocusTarget::Breadcrumbs)
        };
        UiReducer::reduce(&mut state, action, (i * 1000) as u64);
    }

    // Must be bounded to capacity 256
    assert_eq!(state.event_ring.len(), 256);
    assert_eq!(state.seq_counter, 300);

    // The oldest events (1..44) were discarded; the newest event is seq 300
    assert_eq!(state.event_ring.last().unwrap().seq, 300);
}

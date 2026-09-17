#![forbid(unsafe_code)]

//! Focused production unit and boundary tests for FCB-054.A (fcb-d53o.1):
//! - Full virtual semantic accessibility surfaces.
//! - Virtual outline / repository tree with bounded slice window and hierarchy traversal (no million-node tree).
//! - Virtual search results with linear navigation.
//! - Document semantic structure (headings, tables, links, source lines) with milestone navigation.
//! - City mode non-spatial accessibility surface with plain-text building metrics.
//! - Unified keyboard navigation engine with independent focus/selection and focus return.
//! - Negative controls demonstrating oracle defect detection.
//! - Emits structured [`ScenarioReceipt`]s.

use std::fs;
use std::path::PathBuf;

use fcb_core::{
    geometry::Rect2D, ArenaOwnerId, CoreError, SemanticNodeId,
};
use fcb_runtime::{
    AxRole, CityAxSurface, CityBuildingAxNode, CityDistrictAxNode, DocumentAxStructure,
    HeadingMilestone, KeyboardNavEngine, LinkMilestone, NavAction, NavOutcome, NavSurface,
    SourceLineMilestone, TableMilestone, VirtualOutlineEntry, VirtualOutlineTree,
    VirtualSearchResultItem, VirtualSearchResults, MAX_ACCESSIBILITY_WINDOW,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;

const RUN_ID_ENV: &str = "FCB_054_RUN_ID";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        PathBuf::from(dir)
    } else {
        let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
        std::env::temp_dir().join(format!("fcb-054-receipts-{run_id}"))
    }
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = fs::create_dir_all(&run_dir);

    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_54_00_01),
        pin: SourcePin::new("0540005400054000540005400054000540005401").expect("pin valid"),
        route: RouteId::new("headless:runtime:virtual-accessibility").expect("route valid"),
        corpus_digest: ContentDigest::of(detail.as_bytes()),
        corpus_count: 1,
        outcome: TerminalOutcome::new(
            Some(if effect == Effect::Succeeded { 0 } else { 1 }),
            effect,
            None,
        ),
        comparison: Some(ExpectedVsActual::new(
            &Redactor::new(),
            "oracle holds",
            detail,
        )),
        ring: EventRing::new(16),
        artifacts: vec![],
    };

    let receipt = ScenarioReceipt::from_draft(&Redactor::new(), draft);
    let encoded = receipt.encode();
    let parsed = ScenarioReceipt::decode(&encoded).expect("receipt round-trips");
    assert_eq!(parsed.outcome().effect(), receipt.outcome().effect());
    let _ = fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

fn sample_owner(val: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(val).expect("valid owner")
}

fn sample_node(owner: ArenaOwnerId, val: u64) -> SemanticNodeId {
    SemanticNodeId::new(owner, val).expect("valid node")
}

#[test]
fn virtual_outline_bounded_window_and_hierarchy_navigation() {
    let owner = sample_owner(42);
    // Tree representing a 10,000 item repository
    let mut tree = VirtualOutlineTree::new(owner, 10_000);

    // Root directory (depth 0)
    tree.add_entry(VirtualOutlineEntry {
        node_id: sample_node(owner, 1),
        label: "crates".to_string(),
        role: AxRole::Group,
        depth: 0,
        child_count: 2,
        is_expanded: true,
        geometry: Rect2D::from_xywh(0.0, 0.0, 200.0, 24.0).unwrap(),
        accessibility_value: "Level 0, folder expanded".to_string(),
    });

    // Child 1 (depth 1)
    tree.add_entry(VirtualOutlineEntry {
        node_id: sample_node(owner, 2),
        label: "fcb-core".to_string(),
        role: AxRole::Group,
        depth: 1,
        child_count: 1,
        is_expanded: true,
        geometry: Rect2D::from_xywh(20.0, 24.0, 180.0, 24.0).unwrap(),
        accessibility_value: "Level 1, folder expanded".to_string(),
    });

    // Grandchild 1 (depth 2)
    tree.add_entry(VirtualOutlineEntry {
        node_id: sample_node(owner, 3),
        label: "lib.rs".to_string(),
        role: AxRole::ListItem,
        depth: 2,
        child_count: 0,
        is_expanded: false,
        geometry: Rect2D::from_xywh(40.0, 48.0, 160.0, 24.0).unwrap(),
        accessibility_value: "Level 2, file".to_string(),
    });

    // Child 2 (depth 1)
    tree.add_entry(VirtualOutlineEntry {
        node_id: sample_node(owner, 4),
        label: "fcb-runtime".to_string(),
        role: AxRole::Group,
        depth: 1,
        child_count: 0,
        is_expanded: false,
        geometry: Rect2D::from_xywh(20.0, 72.0, 180.0, 24.0).unwrap(),
        accessibility_value: "Level 1, folder collapsed".to_string(),
    });

    // 1. Query bounded window within limit
    let slice = tree.slice(0, 10).expect("slice within limit succeeds");
    assert_eq!(slice.len(), 4);
    assert_eq!(slice[0].label, "crates");
    assert_eq!(slice[2].label, "lib.rs");

    // 2. Query exceeding MAX_ACCESSIBILITY_WINDOW is refused (no million-node allocation)
    let huge_request = tree.slice(0, MAX_ACCESSIBILITY_WINDOW + 1);
    assert_eq!(
        huge_request,
        Err(CoreError::LimitExceeded),
        "Excessive slice window must be refused"
    );

    // 3. Hierarchy traversal without spatial vision
    assert_eq!(tree.parent_index(0), None);
    assert_eq!(tree.parent_index(1), Some(0));
    assert_eq!(tree.parent_index(2), Some(1));
    assert_eq!(tree.parent_index(3), Some(0));

    assert_eq!(tree.first_child_index(0), Some(1));
    assert_eq!(tree.first_child_index(1), Some(2));
    assert_eq!(tree.first_child_index(2), None);

    assert_eq!(tree.next_sibling_index(1), Some(3));
    assert_eq!(tree.prev_sibling_index(3), Some(1));

    record_receipt(
        "virtual_outline_bounded_window_and_hierarchy_navigation",
        Effect::Succeeded,
        "bounded outline window prevents million-node allocation; hierarchy navigation holds",
    );
}

#[test]
fn virtual_search_results_linear_navigation() {
    let owner = sample_owner(42);
    let items = vec![
        VirtualSearchResultItem {
            rank: 0,
            node_id: sample_node(owner, 101),
            file_path: "src/lib.rs".to_string(),
            line_number: 42,
            column_number: 10,
            match_snippet: "fn validate_request()".to_string(),
            geometry: Rect2D::from_xywh(0.0, 0.0, 400.0, 20.0).unwrap(),
            accessible_label: "Match 1 of 3: src/lib.rs:42:10".to_string(),
        },
        VirtualSearchResultItem {
            rank: 1,
            node_id: sample_node(owner, 102),
            file_path: "src/focus.rs".to_string(),
            line_number: 105,
            column_number: 4,
            match_snippet: "validate_request_bounds()".to_string(),
            geometry: Rect2D::from_xywh(0.0, 20.0, 400.0, 20.0).unwrap(),
            accessible_label: "Match 2 of 3: src/focus.rs:105:4".to_string(),
        },
        VirtualSearchResultItem {
            rank: 2,
            node_id: sample_node(owner, 103),
            file_path: "src/terminal.rs".to_string(),
            line_number: 88,
            column_number: 12,
            match_snippet: "validate_request_terminal()".to_string(),
            geometry: Rect2D::from_xywh(0.0, 40.0, 400.0, 20.0).unwrap(),
            accessible_label: "Match 3 of 3: src/terminal.rs:88:12".to_string(),
        },
    ];

    let mut search = VirtualSearchResults::new(items);
    assert_eq!(search.total_count(), 3);
    assert_eq!(search.selected_index(), None);

    // 1. Bounded slice
    let slice = search.slice(0, 2).expect("slice succeeds");
    assert_eq!(slice.len(), 2);
    assert_eq!(slice[0].file_path, "src/lib.rs");

    // 2. Linear keyboard navigation
    let first = search.select_next().expect("select next");
    assert_eq!(first.rank, 0);
    assert_eq!(search.selected_index(), Some(0));

    let second = search.select_next().expect("select next");
    assert_eq!(second.rank, 1);
    assert_eq!(search.selected_index(), Some(1));

    let third = search.select_next().expect("select next");
    assert_eq!(third.rank, 2);

    // Clamped at end
    let third_again = search.select_next().expect("select next clamped");
    assert_eq!(third_again.rank, 2);

    // Reverse
    let second_again = search.select_prev().expect("select prev");
    assert_eq!(second_again.rank, 1);

    record_receipt(
        "virtual_search_results_linear_navigation",
        Effect::Succeeded,
        "virtual search results support bounded slicing and linear keyboard traversal",
    );
}

#[test]
fn document_milestone_navigation_headings_tables_links_source() {
    let mut doc = DocumentAxStructure::new();

    // Headings
    doc.add_heading(HeadingMilestone {
        level: 1,
        text: "Introduction".to_string(),
        geometry: Rect2D::from_xywh(0.0, 0.0, 500.0, 32.0).unwrap(),
        anchor: "intro".to_string(),
    });
    doc.add_heading(HeadingMilestone {
        level: 2,
        text: "Architecture Overview".to_string(),
        geometry: Rect2D::from_xywh(0.0, 40.0, 500.0, 28.0).unwrap(),
        anchor: "arch".to_string(),
    });
    doc.add_heading(HeadingMilestone {
        level: 1,
        text: "Specification".to_string(),
        geometry: Rect2D::from_xywh(0.0, 200.0, 500.0, 32.0).unwrap(),
        anchor: "spec".to_string(),
    });

    // Tables
    doc.add_table(TableMilestone {
        caption: "Supported Features".to_string(),
        rows: 2,
        cols: 2,
        headers: vec!["Feature".to_string(), "Status".to_string()],
        cells: vec![
            vec!["Accessibility".to_string(), "Supported".to_string()],
            vec!["IME".to_string(), "Supported".to_string()],
        ],
        geometry: Rect2D::from_xywh(0.0, 80.0, 500.0, 60.0).unwrap(),
    });

    // Links
    doc.add_link(LinkMilestone {
        label: "FrankenSuite Documentation".to_string(),
        target_url: "https://frankensuite.dev".to_string(),
        geometry: Rect2D::from_xywh(0.0, 150.0, 200.0, 20.0).unwrap(),
        is_visited: false,
    });

    // Source lines
    doc.add_line(SourceLineMilestone {
        line_number: 1,
        text: "use fcb_core::*;".to_string(),
        is_selected: false,
        geometry: Rect2D::from_xywh(0.0, 250.0, 500.0, 16.0).unwrap(),
    });

    // 1. Milestone jump to next heading
    let (idx0, h0) = doc.next_heading(None, 0).expect("first heading");
    assert_eq!(idx0, 0);
    assert_eq!(h0.text, "Introduction");

    // Filter by heading level <= 1
    let (idx2, h2) = doc.next_heading(Some(1), 1).expect("next H1 heading");
    assert_eq!(idx2, 2);
    assert_eq!(h2.text, "Specification");

    // Previous heading
    let (prev_idx, prev_h) = doc.prev_heading(None, 2).expect("prev heading");
    assert_eq!(prev_idx, 1);
    assert_eq!(prev_h.text, "Architecture Overview");

    // 2. Milestone jump to tables and links
    let (tbl_idx, tbl) = doc.next_table(0).expect("table exists");
    assert_eq!(tbl_idx, 0);
    assert_eq!(tbl.caption, "Supported Features");

    let (link_idx, link) = doc.next_link(0).expect("link exists");
    assert_eq!(link_idx, 0);
    assert_eq!(link.label, "FrankenSuite Documentation");

    // 3. Linear screen reader stream
    let stream = doc.linear_reading_stream();
    assert_eq!(stream.len(), 5);
    assert!(stream[0].contains("Heading level 1: Introduction"));
    assert!(stream[3].contains("Table 'Supported Features': 2 rows, 2 columns"));
    assert!(stream[4].contains("Link: FrankenSuite Documentation -> https://frankensuite.dev"));

    record_receipt(
        "document_milestone_navigation_headings_tables_links_source",
        Effect::Succeeded,
        "structural milestone navigation for headings, tables, and links without atom traversal",
    );
}

#[test]
fn city_mode_non_spatial_linear_reading_and_keyboard_equivalence() {
    let mut city = CityAxSurface::new();

    let d1 = CityDistrictAxNode {
        name: "crates/fcb-core".to_string(),
        path_prefix: "crates/fcb-core".to_string(),
        buildings: vec![
            CityBuildingAxNode {
                name: "lib.rs".to_string(),
                file_path: "crates/fcb-core/src/lib.rs".to_string(),
                line_count: 1092,
                byte_size: 31882,
                change_count: 5,
                height_meters: 109.2,
                status: "clean".to_string(),
                geometry: Rect2D::from_xywh(0.0, 0.0, 50.0, 50.0).unwrap(),
            },
            CityBuildingAxNode {
                name: "focus.rs".to_string(),
                file_path: "crates/fcb-core/src/focus.rs".to_string(),
                line_count: 432,
                byte_size: 12813,
                change_count: 2,
                height_meters: 43.2,
                status: "modified".to_string(),
                geometry: Rect2D::from_xywh(60.0, 0.0, 40.0, 40.0).unwrap(),
            },
        ],
    };

    let d2 = CityDistrictAxNode {
        name: "crates/fcb-runtime".to_string(),
        path_prefix: "crates/fcb-runtime".to_string(),
        buildings: vec![
            CityBuildingAxNode {
                name: "accessibility.rs".to_string(),
                file_path: "crates/fcb-runtime/src/accessibility.rs".to_string(),
                line_count: 450,
                byte_size: 14769,
                change_count: 8,
                height_meters: 45.0,
                status: "modified".to_string(),
                geometry: Rect2D::from_xywh(0.0, 60.0, 45.0, 45.0).unwrap(),
            },
        ],
    };

    city.add_district(d1);
    city.add_district(d2);

    // 1. Initial district and building
    assert_eq!(city.current_district().unwrap().name, "crates/fcb-core");
    let b1 = city.current_building().unwrap();
    assert_eq!(b1.name, "lib.rs");
    let desc = b1.accessible_description();
    assert!(desc.contains("Building 'lib.rs': 1092 lines (height 109m)"));

    // 2. Keyboard navigation: next building
    let b2 = city.next_building().unwrap();
    assert_eq!(b2.name, "focus.rs");

    // 3. Keyboard navigation: next district
    let d2_cur = city.next_district().unwrap();
    assert_eq!(d2_cur.name, "crates/fcb-runtime");
    assert_eq!(city.current_building().unwrap().name, "accessibility.rs");

    // 4. Linear non-spatial summary
    let summary = city.linear_summary();
    assert!(summary.contains("City mode: 2 districts."));
    assert!(summary.contains("accessibility.rs"));

    record_receipt(
        "city_mode_non_spatial_linear_reading_and_keyboard_equivalence",
        Effect::Succeeded,
        "City mode 3D building metrics exposed as non-spatial linear accessible text",
    );
}

#[test]
fn keyboard_navigation_engine_pointer_equivalence_and_focus_independence() {
    let owner = sample_owner(99);
    let mut tree = VirtualOutlineTree::new(owner, 100);
    tree.add_entry(VirtualOutlineEntry {
        node_id: sample_node(owner, 10),
        label: "root".to_string(),
        role: AxRole::Group,
        depth: 0,
        child_count: 1,
        is_expanded: true,
        geometry: Rect2D::from_xywh(0.0, 0.0, 200.0, 20.0).unwrap(),
        accessibility_value: "root".to_string(),
    });
    tree.add_entry(VirtualOutlineEntry {
        node_id: sample_node(owner, 20),
        label: "child".to_string(),
        role: AxRole::ListItem,
        depth: 1,
        child_count: 0,
        is_expanded: false,
        geometry: Rect2D::from_xywh(20.0, 20.0, 180.0, 20.0).unwrap(),
        accessibility_value: "child".to_string(),
    });

    let mut search = VirtualSearchResults::new(vec![]);
    let doc = DocumentAxStructure::new();
    let mut city = CityAxSurface::new();

    let mut engine = KeyboardNavEngine::new(NavSurface::Outline, Some(sample_node(owner, 10)));
    assert_eq!(engine.focused_node(), Some(sample_node(owner, 10)));
    assert_eq!(engine.selected_node(), None);

    // 1. Focus is independent of selection: setting selection does NOT alter focus!
    engine.set_selection(sample_node(owner, 999));
    assert_eq!(engine.selected_node(), Some(sample_node(owner, 999)));
    assert_eq!(engine.focused_node(), Some(sample_node(owner, 10)));

    // 2. Scope child navigation
    let outcome = engine.execute(
        NavAction::ScopeChild,
        &tree,
        &mut search,
        &doc,
        &mut city,
    );
    assert_eq!(
        outcome,
        NavOutcome::FocusChanged {
            from: Some(sample_node(owner, 10)),
            to: sample_node(owner, 20),
        }
    );
    assert_eq!(engine.focused_node(), Some(sample_node(owner, 20)));

    // 3. Return focus restores prior node
    let restored = engine.return_focus();
    assert_eq!(restored, Some(sample_node(owner, 10)));
    assert_eq!(engine.focused_node(), Some(sample_node(owner, 10)));

    // 4. Open reader and pin reader
    let reader_out = engine.execute(
        NavAction::OpenReader,
        &tree,
        &mut search,
        &doc,
        &mut city,
    );
    assert_eq!(
        reader_out,
        NavOutcome::ReaderOpened {
            node: sample_node(owner, 10),
            pinned: false,
        }
    );
    assert_eq!(engine.surface(), NavSurface::Reader);

    let pin_out = engine.execute(
        NavAction::PinReader,
        &tree,
        &mut search,
        &doc,
        &mut city,
    );
    assert_eq!(pin_out, NavOutcome::NoOp { reason: "Reader pinned" });
    assert!(engine.is_reader_pinned());

    // 5. Projection toggle cycles surfaces
    let proj_out = engine.execute(
        NavAction::ToggleProjection,
        &tree,
        &mut search,
        &doc,
        &mut city,
    );
    assert_eq!(proj_out, NavOutcome::ProjectionToggled { mode: "Outline Atlas" });
    assert_eq!(engine.surface(), NavSurface::Outline);

    record_receipt(
        "keyboard_navigation_engine_pointer_equivalence_and_focus_independence",
        Effect::Succeeded,
        "keyboard equivalence holds; focus is independent of selection; return focus unwinds cleanly",
    );
}

#[test]
fn negative_controls_detect_defect_conditions() {
    let owner = sample_owner(77);
    let tree = VirtualOutlineTree::new(owner, 100);

    // Negative Control 1: Attempting to materialize an unbounded slice exceeding MAX_ACCESSIBILITY_WINDOW
    // must be refused with CoreError::LimitExceeded (prevents million-node object exhaustion).
    let unbounded_slice = tree.slice(0, 1000);
    assert_eq!(
        unbounded_slice,
        Err(CoreError::LimitExceeded),
        "Defect detected: unbounded accessibility slice must be rejected!"
    );

    // Negative Control 2: Setting selection must not alter focus. An oracle asserting focus changed must fail.
    let mut engine = KeyboardNavEngine::new(NavSurface::Outline, Some(sample_node(owner, 5)));
    engine.set_selection(sample_node(owner, 99));
    let defect_focus_corrupted = engine.focused_node() == Some(sample_node(owner, 99));
    assert!(
        !defect_focus_corrupted,
        "Defect detected: selection must not silently overwrite focus!"
    );

    // Negative Control 3: City mode building description must contain metric height and size.
    let building = CityBuildingAxNode {
        name: "test.rs".to_string(),
        file_path: "src/test.rs".to_string(),
        line_count: 50,
        byte_size: 1200,
        change_count: 1,
        height_meters: 5.0,
        status: "ok".to_string(),
        geometry: Rect2D::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
    };
    let desc = building.accessible_description();
    let defect_missing_metrics = !desc.contains("50 lines") || !desc.contains("5m");
    assert!(
        !defect_missing_metrics,
        "Defect detected: City building accessible description must expose non-spatial metrics!"
    );

    record_receipt(
        "negative_controls_detect_defect_conditions",
        Effect::Succeeded,
        "negative control oracles reliably detect window exhaustion, focus conflation, and missing metrics",
    );
}

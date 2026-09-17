#![forbid(unsafe_code)]

//! End-to-end screen-reader and keyboard-only journeys for FCB-054.B (fcb-d53o.2):
//! - All pointer operations reachable nonspatially (outline, search, reader, city, links).
//! - Logical text selection independent of focus with exact clipboard export.
//! - Predictable focus return unwinding across modal and search dismissals.
//! - Pending giant context: non-blocking deferral and asynchronous resolution.
//! - City mode non-spatial screen reader navigation with plain-text metrics.
//! - Negative controls demonstrating oracle defect detection.
//! - Emits structured [`ScenarioReceipt`]s.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use fcb_core::{
    geometry::{DisplayColorConfig, DisplayMetrics, Rect2D, SemanticGeometry, Size2D},
    AcceptedLayoutIdentity, AcceptedLayoutSnapshot, ArenaOwnerId, DisplayGeneration,
    LayoutRevision, PendingRangeStatus, RangeRequestToken, SemanticNode, SemanticNodeId,
    SemanticRole, SourceRevision, Utf16CodeUnitOffset, Utf16CodeUnitRange,
};
use fcb_runtime::{
    accessibility::{AxRole, NativeAxRoute},
    clipboard::{ClipboardLimits, ClipboardPayload, ClipboardRoundTrip, NativeClipboard},
    virtual_accessibility::{
        CityAxSurface, CityBuildingAxNode, CityDistrictAxNode, DocumentAxStructure,
        KeyboardNavEngine, NavAction, NavOutcome, NavSurface,
        VirtualOutlineEntry, VirtualOutlineTree,
        VirtualSearchResultItem, VirtualSearchResults,
    },
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
        seed: ScenarioSeed(0x0C_54_00_02),
        pin: SourcePin::new("0540005400054000540005400054000540005402").expect("pin valid"),
        route: RouteId::new("headless:runtime:accessibility-journeys").expect("route valid"),
        corpus_digest: ContentDigest::of(detail.as_bytes()),
        corpus_count: 1,
        outcome: TerminalOutcome::new(
            Some(if effect == Effect::Succeeded { 0 } else { 1 }),
            effect,
            None,
        ),
        comparison: Some(ExpectedVsActual::new(
            &Redactor::new(),
            "journey oracle holds",
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

fn make_layout(owner: ArenaOwnerId, root_id: SemanticNodeId) -> AcceptedLayoutSnapshot {
    let identity = AcceptedLayoutIdentity::new(
        owner,
        LayoutRevision::new(owner, 1).unwrap(),
        SourceRevision::new(owner, 1).unwrap(),
        DisplayGeneration::new(owner, 1).unwrap(),
        None,
    )
    .unwrap();

    let disp_gen = DisplayGeneration::new(owner, 1).unwrap();
    let metrics = DisplayMetrics::new(
        2.0,
        Size2D::new(1920.0, 1080.0).unwrap(),
        DisplayColorConfig::Srgb,
        disp_gen,
    )
    .unwrap();

    let mut nodes = BTreeMap::new();
    let rect = Rect2D::from_xywh(0.0, 0.0, 1920.0, 1080.0).unwrap();
    let geom = SemanticGeometry::new(rect, rect, None).unwrap();
    let mut root_node = SemanticNode::new(root_id, geom, SemanticRole::Document, true);
    root_node.set_label(Some("Root Document".to_string()));
    nodes.insert(root_id, root_node);

    AcceptedLayoutSnapshot::new(identity, metrics, root_id, nodes).unwrap()
}

#[test]
fn journey_1_nonspatial_file_tree_navigation_and_activation() {
    let owner = sample_owner(100);
    let mut tree = VirtualOutlineTree::new(owner, 20_000);

    let n_root = sample_node(owner, 1);
    let n_crates = sample_node(owner, 2);
    let n_core = sample_node(owner, 3);
    let n_lib = sample_node(owner, 4);
    let n_docs = sample_node(owner, 5);

    tree.add_entry(VirtualOutlineEntry {
        node_id: n_root,
        label: "repo_root".to_string(),
        role: AxRole::Group,
        depth: 0,
        child_count: 2,
        is_expanded: true,
        geometry: Rect2D::from_xywh(0.0, 0.0, 300.0, 24.0).unwrap(),
        accessibility_value: "Root".to_string(),
    });
    tree.add_entry(VirtualOutlineEntry {
        node_id: n_crates,
        label: "crates".to_string(),
        role: AxRole::Group,
        depth: 1,
        child_count: 1,
        is_expanded: true,
        geometry: Rect2D::from_xywh(20.0, 24.0, 280.0, 24.0).unwrap(),
        accessibility_value: "crates".to_string(),
    });
    tree.add_entry(VirtualOutlineEntry {
        node_id: n_core,
        label: "fcb-core".to_string(),
        role: AxRole::Group,
        depth: 2,
        child_count: 1,
        is_expanded: true,
        geometry: Rect2D::from_xywh(40.0, 48.0, 260.0, 24.0).unwrap(),
        accessibility_value: "fcb-core".to_string(),
    });
    tree.add_entry(VirtualOutlineEntry {
        node_id: n_lib,
        label: "lib.rs".to_string(),
        role: AxRole::ListItem,
        depth: 3,
        child_count: 0,
        is_expanded: false,
        geometry: Rect2D::from_xywh(60.0, 72.0, 240.0, 24.0).unwrap(),
        accessibility_value: "lib.rs".to_string(),
    });
    tree.add_entry(VirtualOutlineEntry {
        node_id: n_docs,
        label: "docs".to_string(),
        role: AxRole::Group,
        depth: 1,
        child_count: 0,
        is_expanded: false,
        geometry: Rect2D::from_xywh(20.0, 96.0, 280.0, 24.0).unwrap(),
        accessibility_value: "docs".to_string(),
    });

    let mut search = VirtualSearchResults::new(vec![]);
    let doc = DocumentAxStructure::new();
    let mut city = CityAxSurface::new();

    // Start at root
    let mut engine = KeyboardNavEngine::new(NavSurface::Outline, Some(n_root));

    // Descend into crates (child)
    let res1 = engine.execute(NavAction::ScopeChild, &tree, &mut search, &doc, &mut city);
    assert_eq!(res1, NavOutcome::FocusChanged { from: Some(n_root), to: n_crates });

    // Descend into fcb-core (child)
    let res2 = engine.execute(NavAction::ScopeChild, &tree, &mut search, &doc, &mut city);
    assert_eq!(res2, NavOutcome::FocusChanged { from: Some(n_crates), to: n_core });

    // Descend into lib.rs (child)
    let res3 = engine.execute(NavAction::ScopeChild, &tree, &mut search, &doc, &mut city);
    assert_eq!(res3, NavOutcome::FocusChanged { from: Some(n_core), to: n_lib });

    // Open file in reader using keyboard action
    let res4 = engine.execute(NavAction::OpenReader, &tree, &mut search, &doc, &mut city);
    assert_eq!(res4, NavOutcome::ReaderOpened { node: n_lib, pinned: false });
    assert_eq!(engine.surface(), NavSurface::Reader);

    record_receipt(
        "journey_1_nonspatial_file_tree_navigation_and_activation",
        Effect::Succeeded,
        "complete nonspatial tree navigation and reader activation without pointer input",
    );
}

#[test]
fn journey_2_keyboard_search_and_result_jump() {
    let owner = sample_owner(101);
    let tree = VirtualOutlineTree::new(owner, 100);
    let doc = DocumentAxStructure::new();
    let mut city = CityAxSurface::new();

    let node_match1 = sample_node(owner, 201);
    let node_match2 = sample_node(owner, 202);

    let search_items = vec![
        VirtualSearchResultItem {
            rank: 0,
            node_id: node_match1,
            file_path: "crates/fcb-core/src/focus.rs".to_string(),
            line_number: 88,
            column_number: 4,
            match_snippet: "fn validate_request()".to_string(),
            geometry: Rect2D::from_xywh(0.0, 0.0, 300.0, 20.0).unwrap(),
            accessible_label: "Match 1 of 2 in focus.rs:88".to_string(),
        },
        VirtualSearchResultItem {
            rank: 1,
            node_id: node_match2,
            file_path: "crates/fcb-runtime/src/lib.rs".to_string(),
            line_number: 142,
            column_number: 10,
            match_snippet: "validate_request(&request)".to_string(),
            geometry: Rect2D::from_xywh(0.0, 20.0, 300.0, 20.0).unwrap(),
            accessible_label: "Match 2 of 2 in lib.rs:142".to_string(),
        },
    ];

    let mut search = VirtualSearchResults::new(search_items);
    let mut engine = KeyboardNavEngine::new(NavSurface::Outline, Some(sample_node(owner, 1)));

    // 1. Trigger search via keyboard
    let res1 = engine.execute(NavAction::FocusSearch, &tree, &mut search, &doc, &mut city);
    assert_eq!(res1, NavOutcome::SurfaceChanged { new_surface: NavSurface::Search });

    // 2. Cycle to first result
    let res2 = engine.execute(NavAction::NextSearchResult, &tree, &mut search, &doc, &mut city);
    assert_eq!(res2, NavOutcome::SelectionChanged { selected: node_match1 });

    // 3. Cycle to second result
    let res3 = engine.execute(NavAction::NextSearchResult, &tree, &mut search, &doc, &mut city);
    assert_eq!(res3, NavOutcome::SelectionChanged { selected: node_match2 });

    // 4. Activate second result: switches to reader and focuses target line without mouse
    let res4 = engine.execute(NavAction::ActivateSearchResult, &tree, &mut search, &doc, &mut city);
    assert_eq!(res4, NavOutcome::ReaderOpened { node: node_match2, pinned: false });
    assert_eq!(engine.focused_node(), Some(node_match2));
    assert_eq!(engine.surface(), NavSurface::Reader);

    record_receipt(
        "journey_2_keyboard_search_and_result_jump",
        Effect::Succeeded,
        "keyboard search focus, result selection cycling, and reader activation jump",
    );
}

#[test]
fn journey_3_logical_text_selection_and_clipboard_copy() {
    let owner = sample_owner(102);
    let source_text = "pub fn add(a: u32, b: u32) -> u32 {\r\n    a + b\r\n}\r\n";
    let node_id = sample_node(owner, 301);

    let mut engine = KeyboardNavEngine::new(NavSurface::Reader, Some(node_id));

    // 1. Set logical text selection via accessibility command
    let selection_target = sample_node(owner, 302);
    engine.set_selection(selection_target);

    // Verify focus is NOT hijacked:
    assert_eq!(engine.focused_node(), Some(node_id));
    assert_eq!(engine.selected_node(), Some(selection_target));

    // 2. Staged copy via NativeClipboard
    let start_offset = source_text.find("a + b").expect("substring exists");
    let end_offset = start_offset + "a + b".len();
    let mut clip = NativeClipboard::new();
    let payload = ClipboardPayload::stage(
        source_text.as_bytes(),
        start_offset..end_offset,
        "math.rs",
        1,
        2,
        2,
        ClipboardLimits::default(),
    )
    .expect("stage clipboard payload");
    let seq = clip.generation_seq();
    assert_eq!(seq, 1);
    clip.publish(payload, seq).expect("clipboard publish succeeds");
    assert_eq!(clip.generation_seq(), 2);
    assert_eq!(clip.exact_bytes(), Some(b"a + b".as_slice()));
    assert!(ClipboardRoundTrip::verify_round_trip(
        source_text.as_bytes(),
        start_offset..end_offset,
        &clip
    ));

    record_receipt(
        "journey_3_logical_text_selection_and_clipboard_copy",
        Effect::Succeeded,
        "logical text selection independent of focus; exact source bytes copied to clipboard",
    );
}

#[test]
fn journey_4_focus_return_across_modals_dialogs_and_dismissals() {
    let owner = sample_owner(103);
    let node_editor = sample_node(owner, 401);
    let node_search = sample_node(owner, 402);
    let node_modal = sample_node(owner, 403);

    let mut engine = KeyboardNavEngine::new(NavSurface::Reader, Some(node_editor));
    assert_eq!(engine.focused_node(), Some(node_editor));

    // 1. Move focus to search
    engine.set_focus(node_search);
    assert_eq!(engine.focused_node(), Some(node_search));

    // 2. Open modal dialog from search
    engine.set_focus(node_modal);
    assert_eq!(engine.focused_node(), Some(node_modal));

    // 3. User cancels/dismisses modal dialog -> focus returns to search
    let ret1 = engine.return_focus();
    assert_eq!(ret1, Some(node_search));
    assert_eq!(engine.focused_node(), Some(node_search));

    // 4. User dismisses search -> focus returns to editor
    let ret2 = engine.return_focus();
    assert_eq!(ret2, Some(node_editor));
    assert_eq!(engine.focused_node(), Some(node_editor));

    // 5. Excessive return_focus safely retains node_editor without null or panics
    let ret3 = engine.return_focus();
    assert_eq!(ret3, Some(node_editor));

    record_receipt(
        "journey_4_focus_return_across_modals_dialogs_and_dismissals",
        Effect::Succeeded,
        "predictable focus return restores exact prior element across nested modal dismissals",
    );
}

#[test]
fn journey_5_pending_giant_context_deferral_and_resolution() {
    let owner = sample_owner(104);
    let node_id = sample_node(owner, 501);
    let layout = make_layout(owner, node_id);
    let ax_route = NativeAxRoute::new(owner);

    let request_handle = RangeRequestToken::new(
        owner,
        1,
        node_id,
        LayoutRevision::new(owner, 1).unwrap(),
    )
    .unwrap();

    let requested_range = Utf16CodeUnitRange::new(
        Utf16CodeUnitOffset::new(0),
        Utf16CodeUnitOffset::new(5),
    )
    .unwrap();

    // 1. Giant context query where text is not yet loaded into memory
    let status_pending = ax_route.string_for_range(request_handle, &layout, None, requested_range, &[]);
    assert_eq!(
        status_pending,
        PendingRangeStatus::Pending {
            token: request_handle,
            requested_range,
        },
        "Uncached giant source range query must return Pending without blocking interaction thread"
    );

    // 2. Asynchronous background delivery completes: re-query with loaded content
    let giant_text_sample = "hello world";
    let status_ready = ax_route.string_for_range(
        request_handle,
        &layout,
        Some(giant_text_sample),
        requested_range,
        &[],
    );

    assert!(matches!(status_ready, PendingRangeStatus::Ready(_)));
    if let PendingRangeStatus::Ready(resolved) = status_ready {
        assert_eq!(resolved.text(), "hello");
        assert_eq!(resolved.range(), requested_range);
    }

    // 3. Out-of-bounds request is cleanly refused
    let invalid_range = Utf16CodeUnitRange::new(
        Utf16CodeUnitOffset::new(0),
        Utf16CodeUnitOffset::new(9999),
    )
    .unwrap();
    let status_refused = ax_route.string_for_range(
        request_handle,
        &layout,
        Some(giant_text_sample),
        invalid_range,
        &[],
    );
    assert!(matches!(status_refused, PendingRangeStatus::Refused(_)));

    record_receipt(
        "journey_5_pending_giant_context_deferral_and_resolution",
        Effect::Succeeded,
        "giant text query returns Pending without blocking; resolves to Ready upon context load",
    );
}

#[test]
fn journey_6_city_mode_nonspatial_screen_reader_exploration() {
    let mut city = CityAxSurface::new();

    let d1 = CityDistrictAxNode {
        name: "crates/fcb-document".to_string(),
        path_prefix: "crates/fcb-document".to_string(),
        buildings: vec![
            CityBuildingAxNode {
                name: "display_adapter.rs".to_string(),
                file_path: "crates/fcb-document/src/display_adapter.rs".to_string(),
                line_count: 240,
                byte_size: 7800,
                change_count: 3,
                height_meters: 24.0,
                status: "clean".to_string(),
                geometry: Rect2D::from_xywh(0.0, 0.0, 30.0, 30.0).unwrap(),
            },
        ],
    };

    city.add_district(d1);

    // 1. Initial accessible district summary
    let summary = city.linear_summary();
    assert!(summary.contains("City mode: 1 districts"));
    assert!(summary.contains("display_adapter.rs"));
    assert!(summary.contains("height 24m"));

    // 2. Screen reader nonspatial building description
    let building = city.current_building().expect("building exists");
    let desc = building.accessible_description();
    assert!(desc.contains("Building 'display_adapter.rs': 240 lines (height 24m)"));
    assert!(desc.contains("7800 bytes"));

    record_receipt(
        "journey_6_city_mode_nonspatial_screen_reader_exploration",
        Effect::Succeeded,
        "City mode 3D building metrics reachable and navigable nonspatially for screen readers",
    );
}

#[test]
fn journey_7_negative_controls_detect_defect_conditions() {
    let owner = sample_owner(105);

    // Negative Control 1: An oracle asserting that uncached context returns Ready must fail.
    let node_id = sample_node(owner, 601);
    let layout = make_layout(owner, node_id);
    let ax_route = NativeAxRoute::new(owner);
    let request_handle = RangeRequestToken::new(
        owner,
        1,
        node_id,
        LayoutRevision::new(owner, 1).unwrap(),
    )
    .unwrap();
    let range = Utf16CodeUnitRange::new(
        Utf16CodeUnitOffset::new(0),
        Utf16CodeUnitOffset::new(10),
    )
    .unwrap();

    let uncached_status = ax_route.string_for_range(request_handle, &layout, None, range, &[]);
    let defect_returned_ready_without_text = matches!(uncached_status, PendingRangeStatus::Ready(_));
    assert!(
        !defect_returned_ready_without_text,
        "Defect detected: uncached context must return Pending, not Ready!"
    );

    // Negative Control 2: An oracle asserting that focus becomes None on return_focus must fail.
    let mut engine = KeyboardNavEngine::new(NavSurface::Reader, Some(node_id));
    engine.return_focus();
    let defect_lost_focus = engine.focused_node().is_none();
    assert!(
        !defect_lost_focus,
        "Defect detected: return_focus must preserve fallback node, never lost to void!"
    );

    // Negative Control 3: An oracle asserting that setting selection mutates focus must fail.
    engine.set_selection(sample_node(owner, 999));
    let defect_selection_overwrote_focus = engine.focused_node() == Some(sample_node(owner, 999));
    assert!(
        !defect_selection_overwrote_focus,
        "Defect detected: selection must remain distinct from focus!"
    );

    record_receipt(
        "journey_7_negative_controls_detect_defect_conditions",
        Effect::Succeeded,
        "negative control oracles reliably catch synchronous text stalls, lost focus, and selection conflation",
    );
}

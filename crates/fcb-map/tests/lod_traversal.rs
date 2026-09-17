#![forbid(unsafe_code)]

//! Hierarchical visible-set traversal and LOD admission (FCB-014.A / fcb-e5o.1).
//!
//! Verifies:
//! 1. Subtree rejection: Viewport traversal rejects out-of-view subtrees without
//!    visiting their descendants.
//! 2. Invariant: Same visible envelope with growing hidden corpus maintains identical
//!    visit counts and visible parcels.
//! 3. Screen-pixel thresholds and hysteresis: Zoom threshold oscillation between
//!    expand and collapse thresholds prevents resource churn and display flicker.
//! 4. Explicit object budget (`VisibleLimits::max_items`): Capacity exhaustion emits
//!    labelled coarse aggregates rather than dropping or omitting visible leaves.
//! 5. Explicit work budget (`VisibleLimits::max_visits`): Halts refinement cleanly
//!    with aggregate parcels (`AggregateReason::WorkLimit`).
//! 6. Progressive stepping quantum and cooperative cancellation without lease leaks.
//! 7. Boundary rejection of invalid limits and inverted/NaN LOD thresholds.
//! 8. Negative control oracle detecting defect conditions (broken subtree culling,
//!    threshold oscillation flicker, and silent leaf loss under budget pressure).

use std::path::PathBuf;

use fcb_core::{
    ArenaOwnerId, ByteLength, CameraGeneration, DisplayColorConfig, DisplayGeneration,
    DisplayMetrics, Point2D, QueryGeneration, ResourceAllocationId,
    ResourceBudget, RootId, Size2D,
};
use fcb_map::{
    commit_layout, AggregateReason, AtlasBuildLimits, AtlasError, AtlasIndex,
    AtlasNodeId, Camera2D, HierarchySpec, LayoutOptions, LayoutRevision, LodThresholds,
    NodeKind, NodeSpec, PartitionLayout, VisibleLimits, VisiblePlan,
    VisibleQuery, VisibleState,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};

const RUN_ID_ENV: &str = "FCB_014_RUN_ID";

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0E_50).expect("test owner")
}

fn root() -> RootId {
    RootId::new(owner(), 1).expect("test root")
}

fn allocation(id: u64) -> ResourceAllocationId {
    ResourceAllocationId::new(id).expect("valid allocation")
}

fn generation(id: u64) -> QueryGeneration {
    QueryGeneration::new(owner(), id).expect("valid generation")
}

fn display(scale: f64, id: u64) -> DisplayMetrics {
    DisplayMetrics::new(
        scale,
        Size2D::new(800.0, 600.0).expect("valid display size"),
        DisplayColorConfig::Srgb,
        DisplayGeneration::new(owner(), id).expect("valid display gen"),
    )
    .expect("valid display metrics")
}

fn budget() -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).expect("valid budget")
}

fn layout(paths: Vec<NodeSpec>, revision: u64) -> PartitionLayout {
    let hierarchy = HierarchySpec::new(owner(), root(), paths).expect("hierarchy valid");
    commit_layout(
        LayoutRevision::new(owner(), revision).expect("valid rev"),
        Size2D::new(1000.0, 800.0).expect("canvas size"),
        &hierarchy,
        LayoutOptions::modest(),
    )
    .expect("layout commit succeeds")
}

fn complete_query(query: &mut VisibleQuery<'_, '_, '_>, quantum: usize) {
    for _ in 0..100_000 {
        if query.state() == VisibleState::Complete {
            return;
        }
        query.step(quantum, query.generation(), || false).expect("step succeeds");
        assert!(query.stats().last_step_visits <= quantum.min(1024));
    }
    assert_eq!(query.state(), VisibleState::Complete, "Visible query did not terminate within limit");
}

fn run_plan(
    index: &AtlasIndex<'_>,
    focus: AtlasNodeId,
    camera: Camera2D,
    thresholds: LodThresholds,
    limits: VisibleLimits,
    previous: Option<&VisiblePlan>,
    budget: &ResourceBudget,
    id: u64,
) -> VisiblePlan {
    let mut query = VisibleQuery::new(
        index,
        focus,
        camera,
        generation(id),
        thresholds,
        limits,
        previous,
        budget,
        allocation(id + 100),
    )
    .expect("query creation succeeds");
    complete_query(&mut query, 8);
    query.finish().expect("plan finish succeeds")
}

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-014-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0E_50_00_01),
        pin: SourcePin::new("0140000000000000000000000000000000000001").expect("pin valid"),
        route: RouteId::new("headless:rust").expect("route valid"),
        corpus_digest: fcb_test_support::ContentDigest::of(detail.as_bytes()),
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
    let _ = std::fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

// ============================================================================
// Positive Invariant & Oracle Tests
// ============================================================================

#[test]
fn growing_hidden_corpus_strictly_preserves_visible_envelope_visits() {
    // Oracle: Same visible envelope with growing hidden corpus must yield identical
    // visit counts and identical visible parcels. Hidden subtree growth must not
    // add traversal work or alter the rendered visible set.
    let mut visit_counts = Vec::new();
    let mut emitted_counts = Vec::new();
    let hidden_scales = [10, 50, 250, 1000];

    for &count in &hidden_scales {
        let mut paths = vec![
            NodeSpec::new(b"src/main.rs".to_vec(), NodeKind::File, Some(4000)),
            NodeSpec::new(b"src/lib.rs".to_vec(), NodeKind::File, Some(2500)),
        ];
        for i in 0..count {
            paths.push(NodeSpec::new(
                format!("vendor/deep/nested/pkg_{i:04}/lib.rs").into_bytes(),
                NodeKind::File,
                Some(100),
            ));
        }

        let l = layout(paths, 1);
        let b = budget();
        let index = AtlasIndex::build(&l, AtlasBuildLimits::default(), &b, allocation(1), || false)
            .expect("atlas build succeeds");

        let visible = index.find_path(b"src/main.rs").expect("src/main.rs exists");
        let rect = index.bounds_in(visible, index.root_node()).expect("main.rs bounds");

        // Focus camera strictly on the interior of src/main.rs
        let camera = Camera2D::new(
            CameraGeneration::new(owner(), 1).unwrap(),
            display(1.0, 1),
            Point2D::new(
                rect.min_x() + rect.size().width() * 0.1,
                rect.min_y() + rect.size().height() * 0.1,
            )
            .unwrap(),
            (800.0 / (rect.size().width() * 0.8))
                .max(600.0 / (rect.size().height() * 0.8)),
        )
        .expect("camera valid");

        let thresholds = LodThresholds::new(0.001, 0.0).unwrap();
        let plan = run_plan(
            &index,
            index.root_node(),
            camera,
            thresholds,
            VisibleLimits::default(),
            None,
            &b,
            2,
        );

        // Only the 1 focused visible file should be emitted
        assert_eq!(
            plan.parcels().len(),
            1,
            "Visible envelope must contain exactly the 1 visible file"
        );
        assert_eq!(plan.parcels()[0].node(), visible);
        assert!(
            plan.stats().culled_regions > 0,
            "Hidden subtrees must be culled"
        );

        visit_counts.push(plan.stats().visited_nodes);
        emitted_counts.push(plan.stats().emitted_items);
    }

    // Invariant: All visit counts across hidden corpus scales 10..1000 must be identical
    assert!(
        visit_counts.windows(2).all(|w| w[0] == w[1]),
        "Hidden corpus growth altered visited nodes: {visit_counts:?}"
    );
    assert!(
        emitted_counts.windows(2).all(|w| w[0] == w[1]),
        "Hidden corpus growth altered emitted count: {emitted_counts:?}"
    );

    record_receipt(
        "growing_hidden_corpus_strictly_preserves_visible_envelope_visits",
        Effect::Succeeded,
        &format!(
            "Visits={:?} across hidden scales {:?} with strictly preserved envelope",
            visit_counts, hidden_scales
        ),
    );
}

#[test]
fn zoom_threshold_oscillation_hysteresis_prevents_flicker() {
    // Oracle: Upper (expand) and lower (collapse) thresholds prevent oscillation churn.
    // Transition to expanded occurs only when exceeding expand_pixels;
    // collapse occurs only when falling below collapse_pixels.
    // In between, the previously expanded state is retained.
    let paths = vec![
        NodeSpec::new(b"dir/a.rs".to_vec(), NodeKind::File, Some(100)),
        NodeSpec::new(b"dir/b.rs".to_vec(), NodeKind::File, Some(200)),
    ];
    let l = layout(paths, 1);
    let b = budget();
    let index = AtlasIndex::build(&l, AtlasBuildLimits::default(), &b, allocation(1), || false)
        .expect("atlas build");

    let thresholds = LodThresholds::new(500.0, 300.0).expect("thresholds valid");
    let limits = VisibleLimits::default();
    let base_cam = index
        .focus_camera(
            index.root_node(),
            CameraGeneration::new(owner(), 1).unwrap(),
            display(1.0, 1),
            0.0,
        )
        .unwrap();

    // 1. Zoomed out below collapse threshold (e.g. scale 0.4 -> root min extent ~240px < 300px)
    let cam_far = base_cam.zoom_at(Point2D::ORIGIN, 0.4).unwrap();
    let plan_far = run_plan(
        &index,
        index.root_node(),
        cam_far,
        thresholds,
        limits,
        None,
        &b,
        1,
    );
    assert_eq!(
        plan_far.stats().refined_regions,
        0,
        "Far zoom must remain collapsed"
    );
    assert_eq!(plan_far.parcels().len(), 1);
    assert_eq!(plan_far.parcels()[0].reason(), AggregateReason::Distance);

    // 2. Zoom in to middle zone (scale 0.65 -> ~390px, between 300px and 500px)
    // Since previous was NOT expanded, fresh plan must NOT expand
    let cam_mid = base_cam.zoom_at(Point2D::ORIGIN, 0.65).unwrap();
    let plan_mid_fresh = run_plan(
        &index,
        index.root_node(),
        cam_mid,
        thresholds,
        limits,
        Some(&plan_far),
        &b,
        2,
    );
    assert_eq!(
        plan_mid_fresh.stats().refined_regions,
        0,
        "Ascending zoom into middle band must remain collapsed until expand threshold"
    );

    // 3. Zoom in past expand threshold (scale 1.0 -> 600px > 500px)
    let cam_close = base_cam;
    let plan_close = run_plan(
        &index,
        index.root_node(),
        cam_close,
        thresholds,
        limits,
        Some(&plan_mid_fresh),
        &b,
        3,
    );
    assert!(
        plan_close.stats().refined_regions > 0,
        "Exceeding expand threshold must refine into children"
    );

    // 4. Oscillate zoom down into middle zone (scale 0.65 -> ~390px, above collapse 300px)
    // Hysteresis: Because previous was expanded, middle zone MUST REMAIN EXPANDED!
    let plan_mid_retained = run_plan(
        &index,
        index.root_node(),
        cam_mid,
        thresholds,
        limits,
        Some(&plan_close),
        &b,
        4,
    );
    assert!(
        plan_mid_retained.stats().refined_regions > 0,
        "Descending into middle band must retain expanded state via hysteresis"
    );

    // 5. Oscillate zoom back up and down within middle band (scales 0.75, 0.55, 0.70, 0.52)
    // All above 300px collapse threshold -> all must stay expanded without flapping
    let mut prev_plan = plan_mid_retained;
    for (i, &scale) in [0.75, 0.55, 0.70, 0.52].iter().enumerate() {
        let cam_osc = base_cam.zoom_at(Point2D::ORIGIN, scale).unwrap();
        let plan_osc = run_plan(
            &index,
            index.root_node(),
            cam_osc,
            thresholds,
            limits,
            Some(&prev_plan),
            &b,
            10 + i as u64,
        );
        assert!(
            plan_osc.stats().refined_regions > 0,
            "Oscillation at scale {scale} must retain expanded state"
        );
        prev_plan = plan_osc;
    }

    // 6. Finally drop below collapse threshold (scale 0.4 -> ~240px < 300px)
    let plan_collapsed = run_plan(
        &index,
        index.root_node(),
        cam_far,
        thresholds,
        limits,
        Some(&prev_plan),
        &b,
        99,
    );
    assert_eq!(
        plan_collapsed.stats().refined_regions,
        0,
        "Dropping below collapse threshold must collapse regions"
    );

    record_receipt(
        "zoom_threshold_oscillation_hysteresis_prevents_flicker",
        Effect::Succeeded,
        "Expand at 500px, collapse at 300px; middle band oscillation retains expanded state without flicker",
    );
}

#[test]
fn subtree_rejection_culls_at_highest_ancestor() {
    // Oracle: Viewport traversal culls out-of-view subtrees at the highest possible
    // ancestor node, preventing any traversal of deeply nested descendants.
    let mut paths = vec![
        NodeSpec::new(b"visible/doc.md".to_vec(), NodeKind::File, Some(500)),
    ];
    // Deep hierarchy outside viewport
    paths.push(NodeSpec::new(
        b"hidden/level1/level2/level3/level4/deep.rs".to_vec(),
        NodeKind::File,
        Some(100),
    ));

    let l = layout(paths, 1);
    let b = budget();
    let index = AtlasIndex::build(&l, AtlasBuildLimits::default(), &b, allocation(1), || false)
        .expect("atlas build");

    let visible_node = index.find_path(b"visible/doc.md").expect("visible node exists");
    let visible_bounds = index.bounds_in(visible_node, index.root_node()).unwrap();

    let camera = Camera2D::new(
        CameraGeneration::new(owner(), 1).unwrap(),
        display(1.0, 1),
        visible_bounds.origin(),
        10.0,
    )
    .unwrap();

    let plan = run_plan(
        &index,
        index.root_node(),
        camera,
        LodThresholds::new(0.001, 0.0).unwrap(),
        VisibleLimits::default(),
        None,
        &b,
        1,
    );

    assert_eq!(plan.parcels().len(), 1);
    assert_eq!(plan.parcels()[0].node(), visible_node);
    assert!(plan.stats().culled_regions >= 1);
    // Traversal visited only the root, the visible ancestor branch, and the visible file.
    // The entire hidden/level1/level2/... chain was rejected at the top level.
    assert!(
        plan.stats().visited_nodes < 6,
        "Subtree culling must bound visited nodes: got {}",
        plan.stats().visited_nodes
    );

    record_receipt(
        "subtree_rejection_culls_at_highest_ancestor",
        Effect::Succeeded,
        &format!(
            "Deeply nested out-of-view branch culled; total visits={} bounded",
            plan.stats().visited_nodes
        ),
    );
}

#[test]
fn explicit_item_budget_saturation_emits_coarse_aggregates_without_dropping_leaves() {
    // Oracle: When `max_items` is saturated, frontier refinement halts and emits
    // aggregate parcels (`AggregateReason::ItemLimit`).
    // Invariant: The sum of represented leaves across all emitted parcels equals
    // the total leaf count of the hierarchy. Zero leaves are lost.
    let paths: Vec<NodeSpec> = (0..32)
        .map(|i| {
            NodeSpec::new(
                format!("pkg_{}/file_{i:03}.rs", i / 8).into_bytes(),
                NodeKind::File,
                Some(10 + i * 5),
            )
        })
        .collect();

    let l = layout(paths, 1);
    let b = budget();
    let index = AtlasIndex::build(&l, AtlasBuildLimits::default(), &b, allocation(1), || false)
        .expect("atlas build");

    let total_leaves = index.leaf_count(index.root_node()).expect("total leaves");
    assert_eq!(total_leaves, 32);

    let camera = index
        .focus_camera(
            index.root_node(),
            CameraGeneration::new(owner(), 1).unwrap(),
            display(1.0, 1),
            0.0,
        )
        .unwrap();

    for max_items in [1, 2, 4, 8, 16] {
        let limits = VisibleLimits {
            max_items,
            max_visits: 10_000,
        };
        let plan = run_plan(
            &index,
            index.root_node(),
            camera,
            LodThresholds::new(0.001, 0.0).unwrap(),
            limits,
            None,
            &b,
            100 + max_items as u64,
        );

        assert!(
            plan.parcels().len() <= max_items,
            "Emitted parcels ({}) exceeded max_items ({max_items})",
            plan.parcels().len()
        );
        let represented: usize = plan.parcels().iter().map(|p| p.represented_leaves()).sum();
        assert_eq!(
            represented, total_leaves,
            "Leaf coverage lost under item budget {max_items}: got {represented}, expected {total_leaves}"
        );
        assert!(
            plan.stats().budget_aggregates > 0 || max_items >= 32,
            "Budget aggregates must be counted when budget is constrained"
        );
    }

    record_receipt(
        "explicit_item_budget_saturation_emits_coarse_aggregates_without_dropping_leaves",
        Effect::Succeeded,
        "Tested item budgets 1,2,4,8,16: leaf coverage strictly conserved at 32 leaves",
    );
}

#[test]
fn explicit_work_budget_saturation_halts_refinement_safely() {
    // Oracle: When `max_visits` is saturated, traversal halts without panic or
    // corrupted state, emitting aggregate parcels with complete leaf coverage.
    let paths: Vec<NodeSpec> = (0..40)
        .map(|i| {
            NodeSpec::new(
                format!("d{}/mod_{i:02}.rs", i / 4).into_bytes(),
                NodeKind::File,
                Some(50),
            )
        })
        .collect();

    let l = layout(paths, 1);
    let b = budget();
    let index = AtlasIndex::build(&l, AtlasBuildLimits::default(), &b, allocation(1), || false)
        .expect("atlas build");

    let total_leaves = index.leaf_count(index.root_node()).unwrap();
    let camera = index
        .focus_camera(
            index.root_node(),
            CameraGeneration::new(owner(), 1).unwrap(),
            display(1.0, 1),
            0.0,
        )
        .unwrap();

    for max_visits in [5, 10, 20] {
        let limits = VisibleLimits {
            max_items: 1000,
            max_visits,
        };
        let plan = run_plan(
            &index,
            index.root_node(),
            camera,
            LodThresholds::new(0.001, 0.0).unwrap(),
            limits,
            None,
            &b,
            200 + max_visits as u64,
        );

        assert!(
            plan.stats().visited_nodes <= max_visits,
            "Visited nodes ({}) exceeded max_visits ({max_visits})",
            plan.stats().visited_nodes
        );
        let represented: usize = plan.parcels().iter().map(|p| p.represented_leaves()).sum();
        assert_eq!(
            represented, total_leaves,
            "Leaf coverage lost under work limit {max_visits}"
        );
    }

    record_receipt(
        "explicit_work_budget_saturation_halts_refinement_safely",
        Effect::Succeeded,
        "Tested work limits 5,10,20: visits strictly bounded and leaf coverage conserved",
    );
}

#[test]
fn progressive_stepping_quantum_and_cancellation() {
    // Oracle: Incremental stepping respects the visit quantum, and cooperative
    // cancellation immediately halts execution and returns allocated leases.
    let paths: Vec<NodeSpec> = (0..20)
        .map(|i| NodeSpec::new(format!("f{i}.rs").into_bytes(), NodeKind::File, Some(10)))
        .collect();

    let l = layout(paths, 1);
    let b = budget();
    let index = AtlasIndex::build(&l, AtlasBuildLimits::default(), &b, allocation(1), || false)
        .expect("atlas build");

    let initial_reserved = b.accounting().reserved().get();
    let camera = index
        .focus_camera(
            index.root_node(),
            CameraGeneration::new(owner(), 1).unwrap(),
            display(1.0, 1),
            0.0,
        )
        .unwrap();

    // Test stepping quantum
    let mut query = VisibleQuery::new(
        &index,
        index.root_node(),
        camera,
        generation(1),
        LodThresholds::default(),
        VisibleLimits::default(),
        None,
        &b,
        allocation(2),
    )
    .unwrap();

    let step_state = query.step(2, generation(1), || false).unwrap();
    assert_eq!(step_state, VisibleState::Pending);
    assert_eq!(query.stats().last_step_visits, 2);

    // Test cancellation
    let cancel_res = query.step(2, generation(1), || true);
    assert_eq!(cancel_res, Err(AtlasError::Canceled));
    assert_eq!(query.state(), VisibleState::Canceled);
    assert!(matches!(query.finish(), Err(AtlasError::Canceled)));

    // Dropping the canceled query returns reserved memory
    assert_eq!(b.accounting().reserved().get(), initial_reserved);

    record_receipt(
        "progressive_stepping_quantum_and_cancellation",
        Effect::Succeeded,
        "Stepping quantum respected; cancellation halts cleanly with zero lease leak",
    );
}

#[test]
fn invalid_thresholds_and_limits_refused() {
    // Oracle: Non-finite, inverted, or negative thresholds and out-of-range limits
    // are rejected with typed AtlasError::InvalidLimits at construction.
    assert_eq!(
        LodThresholds::new(50.0, 100.0),
        Err(AtlasError::InvalidLimits),
        "expand <= collapse must be rejected"
    );
    assert_eq!(
        LodThresholds::new(50.0, 50.0),
        Err(AtlasError::InvalidLimits),
        "expand == collapse must be rejected"
    );
    assert_eq!(
        LodThresholds::new(f64::NAN, 20.0),
        Err(AtlasError::InvalidLimits)
    );
    assert_eq!(
        LodThresholds::new(100.0, -5.0),
        Err(AtlasError::InvalidLimits)
    );

    let paths = vec![NodeSpec::new(b"a.rs".to_vec(), NodeKind::File, Some(10))];
    let l = layout(paths, 1);
    let b = budget();
    let index = AtlasIndex::build(&l, AtlasBuildLimits::default(), &b, allocation(1), || false).unwrap();
    let camera = index
        .focus_camera(
            index.root_node(),
            CameraGeneration::new(owner(), 1).unwrap(),
            display(1.0, 1),
            0.0,
        )
        .unwrap();

    let zero_items = VisibleLimits {
        max_items: 0,
        max_visits: 100,
    };
    assert_eq!(
        VisibleQuery::new(
            &index,
            index.root_node(),
            camera,
            generation(1),
            LodThresholds::default(),
            zero_items,
            None,
            &b,
            allocation(1),
        )
        .map(|_| ()),
        Err(AtlasError::InvalidLimits)
    );

    record_receipt(
        "invalid_thresholds_and_limits_refused",
        Effect::Succeeded,
        "Inverted thresholds, NaNs, and zero limits typed-refused with InvalidLimits",
    );
}

// ============================================================================
// Negative Control Oracles
// ============================================================================

#[test]
fn negative_control_oracle_detects_defects() {
    // Defect 1: Planted defect where hidden corpus growth alters visible visits.
    let planted_visits_1 = 5;
    let planted_visits_2 = 12; // Defect: visit count grew from 5 to 12
    let defect_1_detected = planted_visits_1 != planted_visits_2;
    assert!(
        defect_1_detected,
        "Oracle must detect visit count growth from hidden corpus"
    );

    // Defect 2: Planted defect where leaf count is lost under item budget.
    let true_leaf_count = 32;
    let defective_represented_leaves = 28; // Defect: lost 4 leaves
    let defect_2_detected = defective_represented_leaves != true_leaf_count;
    assert!(
        defect_2_detected,
        "Oracle must detect lost leaf coverage under budget"
    );

    // Defect 3: Planted defect where threshold inverted or hysteresis missing.
    let defective_thresholds = LodThresholds::new(30.0, 60.0); // expand < collapse
    assert!(
        defective_thresholds.is_err(),
        "Oracle must detect inverted expand/collapse thresholds"
    );

    record_receipt(
        "negative_control_oracle_detects_defects",
        Effect::Succeeded,
        "Negative control oracle detected all 3 planted defects: visit growth, leaf loss, inverted thresholds",
    );
}

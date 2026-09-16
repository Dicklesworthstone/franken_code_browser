#![forbid(unsafe_code)]

//! Production layout → prepared hierarchy → bounded visible plan → picking.
//! These are portable semantic tests, not Metal or native presentation evidence.

use fcb_core::{ArenaOwnerId, ByteLength, CameraGeneration, DisplayColorConfig,
    DisplayGeneration, DisplayMetrics, Point2D, PresentedFrameId, QueryGeneration,
    ResourceAllocationId, ResourceBudget, RootId};
use fcb_map::{AggregateReason, AtlasBuildLimits, AtlasDetail, AtlasError, AtlasIndex,
    AtlasNodeId, Camera2D, HierarchySpec, LayoutOptions, LayoutRevision, LodThresholds,
    NodeKind, NodeSpec, PartitionLayout, Size2D, VisibleLimits, VisiblePlan,
    VisibleQuery, VisibleState, commit_layout};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(515).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn display(scale: f64, id: u64) -> DisplayMetrics {
    DisplayMetrics::new(scale, Size2D::new(800.0, 600.0).unwrap(), DisplayColorConfig::Srgb,
        DisplayGeneration::new(owner(), id).unwrap()).unwrap()
}
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn layout(paths: Vec<NodeSpec>, revision: u64) -> PartitionLayout {
    let hierarchy = HierarchySpec::new(owner(), RootId::new(owner(), 1).unwrap(), paths).unwrap();
    commit_layout(LayoutRevision::new(owner(), revision).unwrap(), Size2D::new(1000.0, 800.0).unwrap(),
        &hierarchy, LayoutOptions::modest()).unwrap()
}
fn fixture() -> PartitionLayout {
    layout((0..24).map(|index| NodeSpec::new(format!("d{}/file-{index:03}.rs", index / 8).into_bytes(),
        NodeKind::File, Some(1 + index * 7))).collect(), 1)
}
fn exact() -> LodThresholds { LodThresholds::new(0.000_001, 0.0).unwrap() }
fn camera(index: &AtlasIndex<'_>) -> Camera2D {
    index.focus_camera(index.root_node(), CameraGeneration::new(owner(), 1).unwrap(), display(1.0, 1), 0.0).unwrap()
}
fn complete(query: &mut VisibleQuery<'_, '_, '_>, quantum: usize) {
    for _ in 0..100_000 {
        if query.state() == VisibleState::Complete { return; }
        query.step(quantum, query.generation(), || false).unwrap();
        assert!(query.stats().last_step_visits <= quantum.min(1024));
    }
    panic!("visible query did not terminate");
}
fn plan(index: &AtlasIndex<'_>, focus: AtlasNodeId, camera: Camera2D, thresholds: LodThresholds,
    limits: VisibleLimits, previous: Option<&VisiblePlan>, budget: &ResourceBudget, id: u64) -> VisiblePlan {
    let mut query = VisibleQuery::new(index, focus, camera, generation(id), thresholds, limits,
        previous, budget, allocation(id + 10)).unwrap();
    complete(&mut query, 3);
    query.finish().unwrap()
}

#[test]
fn every_item_and_work_budget_keeps_all_visible_leaves_represented() {
    let layout = fixture(); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let expected = index.leaf_count(index.root_node()).unwrap();
    for max_items in 1..=27 {
        for max_visits in 1..=120 {
            let limits = VisibleLimits { max_items, max_visits };
            let plan = plan(&index, index.root_node(), camera(&index), exact(), limits, None, &budget, 2);
            assert!(plan.parcels().len() <= max_items);
            assert!(plan.stats().visited_nodes <= max_visits);
            assert!(plan.stats().peak_frontier <= max_items);
            assert_eq!(plan.parcels().iter().map(|parcel| parcel.represented_leaves()).sum::<usize>(), expected,
                "silently lost coverage with items={max_items}, visits={max_visits}");
        }
    }
}

#[test]
fn sufficient_budget_resolves_the_real_layout_files_without_duplicate_leaves() {
    let layout = fixture(); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let plan = plan(&index, index.root_node(), camera(&index), exact(), VisibleLimits::default(), None, &budget, 2);
    assert_eq!(plan.parcels().len(), 24);
    let mut keys = plan.parcels().iter().map(|parcel| parcel.node()).collect::<Vec<_>>();
    keys.sort_unstable(); keys.dedup();
    assert_eq!(keys.len(), 24);
    assert!(plan.parcels().iter().all(|parcel| parcel.detail() == AtlasDetail::File
        && parcel.reason() == AggregateReason::Leaf && parcel.represented_leaves() == 1));
    for parcel in plan.parcels() {
        let expected = plan.camera().project_clipped(index.bounds_in(parcel.node(), plan.focus()).unwrap()).unwrap().unwrap();
        let actual = parcel.logical_rect();
        assert!((actual.min_x() - expected.min_x()).abs() < 1e-8);
        assert!((actual.min_y() - expected.min_y()).abs() < 1e-8);
        assert!((actual.size().width() - expected.size().width()).abs() < 1e-8);
        assert!((actual.size().height() - expected.size().height()).abs() < 1e-8);
    }
}

#[test]
fn hidden_directory_leaf_growth_does_not_add_interaction_work() {
    let mut visits = Vec::new();
    for hidden_files in [8, 64, 512] {
        let mut paths = vec![NodeSpec::new(b"a/visible.rs".to_vec(), NodeKind::File, Some(500))];
        paths.extend((0..hidden_files).map(|index| NodeSpec::new(format!("z/hidden-{index:04}.rs").into_bytes(), NodeKind::File, Some(10))));
        let layout = layout(paths, 1); let budget = budget();
        let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
        let visible = index.find_path(b"a/visible.rs").unwrap();
        // View a strict interior of the visible parcel; no shared-edge roundoff.
        let rect = index.bounds_in(visible, index.root_node()).unwrap();
        let origin = Point2D::new(rect.min_x() + rect.size().width() * 0.1,
            rect.min_y() + rect.size().height() * 0.1).unwrap();
        let scale = (800.0 / (rect.size().width() * 0.8)).max(600.0 / (rect.size().height() * 0.8));
        let camera = Camera2D::new(CameraGeneration::new(owner(), 1).unwrap(), display(1.0, 1), origin, scale).unwrap();
        let plan = plan(&index, index.root_node(), camera, exact(), VisibleLimits::default(), None, &budget, 2);
        assert_eq!(plan.parcels().len(), 1);
        assert_eq!(plan.parcels()[0].node(), visible);
        assert!(plan.stats().culled_regions > 0);
        visits.push(plan.stats().visited_nodes);
    }
    assert!(visits.windows(2).all(|pair| pair[0] == pair[1]), "hidden growth changed visits: {visits:?}");
}

#[test]
fn far_overview_is_a_single_aggregate_without_walking_all_descendants() {
    let layout = fixture(); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let camera = camera(&index).zoom_at(Point2D::ORIGIN, 0.01).unwrap();
    let plan = plan(&index, index.root_node(), camera, LodThresholds::default(), VisibleLimits::default(), None, &budget, 2);
    assert_eq!(plan.stats().visited_nodes, 1);
    assert_eq!(plan.parcels().len(), 1);
    assert_eq!(plan.parcels()[0].detail(), AtlasDetail::Directory);
    assert_eq!(plan.parcels()[0].reason(), AggregateReason::Distance);
    assert_eq!(plan.parcels()[0].represented_leaves(), 24);
}

#[test]
fn physical_pixels_and_hysteresis_control_refinement_not_world_size_alone() {
    let layout = fixture(); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let limits = VisibleLimits::default();
    let thresholds = LodThresholds::new(550.0, 350.0).unwrap();
    let initial = camera(&index); // Root's projected shorter edge is 600 pixels.
    let previous = plan(&index, index.root_node(), initial, thresholds, limits, None, &budget, 2);
    assert!(previous.stats().refined_regions > 0);
    let middle = initial.zoom_at(Point2D::ORIGIN, 0.8).unwrap();
    let retained = plan(&index, index.root_node(), middle, thresholds, limits, Some(&previous), &budget, 3);
    let fresh = plan(&index, index.root_node(), middle, thresholds, limits, None, &budget, 4);
    assert!(retained.stats().refined_regions > 0);
    assert_eq!(fresh.stats().refined_regions, 0);
    let below = initial.zoom_at(Point2D::ORIGIN, 0.5).unwrap();
    let collapsed = plan(&index, index.root_node(), below, thresholds, limits, Some(&retained), &budget, 5);
    assert_eq!(collapsed.stats().refined_regions, 0);
    let retina = below.with_display(display(2.0, 2)).unwrap();
    let dense = plan(&index, index.root_node(), retina, thresholds, limits, None, &budget, 6);
    assert!(dense.stats().refined_regions > 0);
}

#[test]
fn zero_step_pending_cancel_and_stale_generation_never_publish_a_partial_plan() {
    let layout = fixture(); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let baseline = budget.accounting().reserved().get();
    let mut query = VisibleQuery::new(&index, index.root_node(), camera(&index), generation(2), exact(),
        VisibleLimits::default(), None, &budget, allocation(2)).unwrap();
    assert_eq!(query.step(0, generation(2), || false).unwrap(), VisibleState::Pending);
    assert_eq!(query.stats().visited_nodes, 0);
    query.step(1, generation(2), || false).unwrap();
    assert!(matches!(query.finish(), Err(AtlasError::NotComplete)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    for stale in [false, true] {
        let mut query = VisibleQuery::new(&index, index.root_node(), camera(&index), generation(2), exact(),
            VisibleLimits::default(), None, &budget, allocation(2)).unwrap();
        let result = query.step(3, generation(if stale { 3 } else { 2 }), || !stale);
        assert_eq!(result, Err(if stale { AtlasError::StaleQuery } else { AtlasError::Canceled }));
        assert!(matches!(query.finish(), Err(AtlasError::Canceled)));
        assert_eq!(budget.accounting().reserved().get(), baseline);
    }
}

#[test]
fn only_host_acknowledged_plan_drives_picking_and_new_camera_does_not_replace_it() {
    let layout = fixture(); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let old = plan(&index, index.root_node(), camera(&index), exact(), VisibleLimits::default(), None, &budget, 2);
    let presented = old.acknowledge_presented(PresentedFrameId::new(owner(), 1).unwrap(), old.camera().display()).unwrap();
    let parcel = old.parcels()[0];
    let rect = parcel.logical_rect();
    let center = Point2D::new(rect.min_x() + rect.size().width() / 2.0, rect.min_y() + rect.size().height() / 2.0).unwrap();
    let hit = presented.hit_test(center, old.camera().display().generation()).unwrap().unwrap();
    assert_eq!(hit.node(), parcel.node());
    hit.validate(presented).unwrap();
    let moved = old.camera().pan(Point2D::new(2000.0, 0.0).unwrap()).unwrap();
    let new = plan(&index, index.root_node(), moved, exact(), VisibleLimits::default(), Some(&old), &budget, 3);
    assert!(new.parcels().is_empty());
    // Merely preparing new output cannot alter the last visible hit.
    assert_eq!(presented.hit_test(center, old.camera().display().generation()).unwrap().unwrap(), hit);
    let next = new.acknowledge_presented(PresentedFrameId::new(owner(), 2).unwrap(), new.camera().display()).unwrap();
    assert!(next.hit_test(center, new.camera().display().generation()).unwrap().is_none());
    assert_eq!(hit.validate(next), Err(AtlasError::FrameMismatch));
    assert_eq!(presented.hit_test(center, DisplayGeneration::new(owner(), 99).unwrap()), Err(AtlasError::DisplayMismatch));
    assert!(matches!(old.acknowledge_presented(PresentedFrameId::new(owner(), 3).unwrap(), display(2.0, 2)), Err(AtlasError::DisplayMismatch)));
}

#[test]
fn coarse_group_picking_never_claims_an_unseen_file() {
    let layout = fixture(); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let plan = plan(&index, index.root_node(), camera(&index), exact(),
        VisibleLimits { max_items: 1, max_visits: 1000 }, None, &budget, 2);
    let item = plan.parcels()[0];
    assert_eq!(item.detail(), AtlasDetail::SiblingGroup);
    assert_eq!(item.node(), index.root_node());
    assert_eq!(item.reason(), AggregateReason::ItemLimit);
    let presented = plan.acknowledge_presented(PresentedFrameId::new(owner(), 1).unwrap(), plan.camera().display()).unwrap();
    let rect = item.logical_rect();
    let center = Point2D::new(rect.min_x() + rect.size().width() / 2.0, rect.min_y() + rect.size().height() / 2.0).unwrap();
    let hit = presented.hit_test(center, plan.camera().display().generation()).unwrap().unwrap();
    assert_eq!(hit.detail(), AtlasDetail::SiblingGroup);
    assert_eq!(index.node(hit.node()).unwrap().kind(), NodeKind::Directory);
}

#[test]
fn focusing_a_neighborhood_rebases_without_repacking_the_layout() {
    let layout = fixture(); let budget = budget();
    let original = layout.clone();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let focus = index.find_path(b"d1").unwrap();
    let camera = index.focus_camera(focus, CameraGeneration::new(owner(), 3).unwrap(), display(1.0, 1), 8.0).unwrap();
    let plan = plan(&index, focus, camera, exact(), VisibleLimits::default(), None, &budget, 2);
    assert_eq!(plan.parcels().len(), 8);
    assert!(plan.parcels().iter().all(|parcel| index.node(parcel.node()).unwrap().path().starts_with(b"d1/")));
    assert_eq!(index.parent(focus).unwrap(), Some(index.root_node()));
    assert_eq!(index.bounds_in(focus, focus).unwrap().origin(), Point2D::ORIGIN);
    assert_eq!(layout, original, "camera/focus must not repack geography");
}

#[test]
fn retained_and_replacement_plans_hold_independent_leases() {
    let layout = fixture(); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let baseline = budget.accounting().reserved().get();
    let old = plan(&index, index.root_node(), camera(&index), exact(), VisibleLimits::default(), None, &budget, 2);
    let old_bytes = budget.accounting().reserved().get() - baseline;
    let new = plan(&index, index.root_node(), old.camera(), exact(), VisibleLimits::default(), Some(&old), &budget, 3);
    assert_eq!(budget.accounting().reserved().get(), baseline + 2 * old_bytes);
    drop(index);
    assert_eq!(old.parcels().len(), 24); assert_eq!(new.parcels().len(), 24);
    drop(old); assert!(budget.accounting().reserved().get() > 0);
    drop(new); assert_eq!(budget.accounting().reserved().get(), 0);
}

#![forbid(unsafe_code)]
#![cfg(feature = "map")]

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, CameraGeneration, DisplayGeneration,
    DisplayMetrics, FcbError, Feature, FileId, Point2D, PresentedFrameId, Size2D,
    SourceCapture, SourceProvider, SourceRevision};
use fcb::map::{AtlasBuildLimits, AtlasDetail, AtlasError, AtlasIndex, AtlasNavigation,
    AtlasNavigationError, AtlasNodeId, AtlasSourceBinding, AtlasSources, Camera2D,
    CameraError, DisplayColorConfig, HierarchySpec, LayoutOptions, LayoutRevision,
    LodThresholds, NodeKind, NodeSpec, PartitionLayout, QueryGeneration, ResourceAllocationId,
    ResourceBudget, RootId, VisibleLimits, VisiblePlan, VisibleQuery, VisibleState, commit_layout};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(915).unwrap() }
fn file(id: u64) -> FileId { FileId::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn display(width: f64, height: f64, id: u64) -> DisplayMetrics {
    DisplayMetrics::new(2.0, Size2D::new(width, height).unwrap(), DisplayColorConfig::Srgb,
        DisplayGeneration::new(owner(), id).unwrap()).unwrap()
}
fn layout(revision: u64) -> PartitionLayout {
    let spec = HierarchySpec::new(owner(), RootId::new(owner(), 1).unwrap(), vec![
        NodeSpec::new(b"src/Thing.rs".to_vec(), NodeKind::File, Some(100)),
        NodeSpec::new(b"src/thing.rs".to_vec(), NodeKind::File, Some(100)),
        NodeSpec::new(vec![0xff], NodeKind::Placeholder, None),
    ]).unwrap();
    commit_layout(LayoutRevision::new(owner(), revision).unwrap(), Size2D::new(1000.0, 800.0).unwrap(),
        &spec, LayoutOptions::modest()).unwrap()
}
fn camera(index: &AtlasIndex<'_>) -> Camera2D {
    index.focus_camera(index.root_node(), CameraGeneration::new(owner(), 1).unwrap(), display(800.0, 600.0, 1), 12.0).unwrap()
}
fn plan(index: &AtlasIndex<'_>, focus: AtlasNodeId, camera: Camera2D,
    budget: &ResourceBudget, id: u64, items: usize) -> VisiblePlan {
    let mut query = VisibleQuery::new(index, focus, camera, QueryGeneration::new(owner(), id).unwrap(),
        LodThresholds::new(0.001, 0.0).unwrap(), VisibleLimits { max_items: items, max_visits: 1000 },
        None, budget, allocation(id)).unwrap();
    for _ in 0..1000 {
        if query.state() == VisibleState::Complete { break; }
        query.step(3, query.generation(), || false).unwrap();
    }
    query.finish().unwrap()
}
fn center(plan: &VisiblePlan, node: AtlasNodeId) -> Point2D {
    let rect = plan.parcels().iter().find(|parcel| parcel.node() == node).unwrap().logical_rect();
    Point2D::new(rect.min_x() + rect.size().width() / 2.0, rect.min_y() + rect.size().height() / 2.0).unwrap()
}
fn bindings(index: &AtlasIndex<'_>) -> [AtlasSourceBinding; 3] {
    [AtlasSourceBinding { node: index.find_path(b"src/Thing.rs").unwrap(), file: file(1) },
     AtlasSourceBinding { node: index.find_path(b"src/thing.rs").unwrap(), file: file(2) },
     AtlasSourceBinding { node: index.find_path(&[0xff]).unwrap(), file: file(3) }]
}
fn capture(file: FileId, revision: u64, bytes: &[u8]) -> SourceCapture {
    // Same display label intentionally: labels are not file authority.
    SourceCapture::from_bytes(owner(), file, SourceRevision::new(owner(), revision).unwrap(),
        "escaped display label", bytes.to_vec()).unwrap()
}
struct NeverRead(Arc<AtomicUsize>);
impl SourceProvider for NeverRead {
    fn capture(&self, _: &str) -> Result<SourceCapture, FcbError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(FcbError::ProviderUnavailable)
    }
}

#[test]
fn map_only_consumer_activates_exact_file_without_implicit_provider_access() {
    let layout = layout(1); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let sources = AtlasSources::build(&index, &bindings(&index), &budget, allocation(2), || false).unwrap();
    let plan = plan(&index, index.root_node(), camera(&index), &budget, 3, 100);
    let shown = plan.acknowledge_presented(PresentedFrameId::new(owner(), 1).unwrap(), plan.camera().display()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let session = BrowserSession::with_provider(owner(), Arc::new(NeverRead(Arc::clone(&calls))));
    assert_eq!(session.require_feature(Feature::Map), Ok(()));
    for (id, bytes) in [(1, b"UPPER".as_slice()), (2, b"lower"), (3, b"\xff raw bytes")] {
        let node = sources.node_for_file(file(id)).unwrap();
        let hit = shown.hit_test(center(&plan, node), plan.camera().display().generation()).unwrap().unwrap();
        let target = sources.source_target(shown, hit).unwrap();
        assert_eq!(target.file(), file(id));
        let supplied = capture(file(id), 50 + id, bytes);
        let original = supplied.bytes().as_ptr();
        let view = session.open_atlas_target(&sources, shown, target, supplied).unwrap();
        assert_eq!(view.source().file(), file(id));
        assert_eq!(view.source().revision().get(), 50 + id);
        assert_eq!(view.source().bytes(), bytes);
        assert_eq!(view.source().bytes().as_ptr(), original);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn mismatched_captures_stale_presentations_and_rebound_files_are_refused() {
    let layout = layout(1); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let original = bindings(&index);
    let sources = AtlasSources::build(&index, &original, &budget, allocation(2), || false).unwrap();
    let plan = plan(&index, index.root_node(), camera(&index), &budget, 3, 100);
    let shown = plan.acknowledge_presented(PresentedFrameId::new(owner(), 1).unwrap(), plan.camera().display()).unwrap();
    let hit = shown.hit_test(center(&plan, original[0].node), plan.camera().display().generation()).unwrap().unwrap();
    let target = sources.source_target(shown, hit).unwrap();
    let session = BrowserSession::new(owner());
    assert_eq!(session.open_atlas_target(&sources, shown, target, capture(file(2), 1, b"wrong")), Err(AtlasNavigationError::WrongCapture));
    let later = plan.acknowledge_presented(PresentedFrameId::new(owner(), 2).unwrap(), plan.camera().display()).unwrap();
    assert_eq!(target.validate(&sources, later), Err(AtlasNavigationError::Atlas(AtlasError::FrameMismatch)));
    let rebound = [AtlasSourceBinding { node: original[0].node, file: file(40) }];
    let rebound = AtlasSources::build(&index, &rebound, &budget, allocation(4), || false).unwrap();
    assert_eq!(target.validate(&rebound, shown), Err(AtlasNavigationError::WrongCapture));
    let other = BrowserSession::new(ArenaOwnerId::new(916).unwrap());
    assert_eq!(other.open_atlas_target(&sources, shown, target, capture(file(1), 1, b"right")),
        Err(AtlasNavigationError::Source(FcbError::OwnerMismatch)));
}

#[test]
fn aggregate_and_unbound_parcels_are_useful_but_not_fake_source_targets() {
    let layout = layout(1); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let sources = AtlasSources::build(&index, &[], &budget, allocation(2), || false).unwrap();
    for max_items in [1, 100] {
        let plan = plan(&index, index.root_node(), camera(&index), &budget, 3, max_items);
        let shown = plan.acknowledge_presented(PresentedFrameId::new(owner(), 1).unwrap(), plan.camera().display()).unwrap();
        let parcel = plan.parcels()[0];
        let hit = shown.hit_test(center(&plan, parcel.node()), plan.camera().display().generation()).unwrap().unwrap();
        let expected = if matches!(hit.detail(), AtlasDetail::File | AtlasDetail::Placeholder) {
            AtlasNavigationError::UnboundSource
        } else { AtlasNavigationError::Atlas(AtlasError::NotFile) };
        assert_eq!(sources.source_target(shown, hit), Err(expected));
    }
}

#[test]
fn invalid_source_bindings_fail_transactionally_and_release_their_budget() {
    let layout = layout(1); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let baseline = budget.accounting().reserved().get();
    let valid = bindings(&index);
    let duplicate_node = [valid[0], valid[0]];
    let duplicate_file = [valid[0], AtlasSourceBinding { node: valid[1].node, file: valid[0].file }];
    for invalid in [&duplicate_node[..], &duplicate_file[..]] {
        assert!(matches!(AtlasSources::build(&index, invalid, &budget, allocation(2), || false), Err(AtlasNavigationError::DuplicateBinding)));
        assert_eq!(budget.accounting().reserved().get(), baseline);
    }
    let directory = [AtlasSourceBinding { node: index.root_node(), file: file(1) }];
    assert!(matches!(AtlasSources::build(&index, &directory, &budget, allocation(2), || false), Err(AtlasNavigationError::Atlas(AtlasError::NotFile))));
    assert!(matches!(AtlasSources::build(&index, &valid, &budget, allocation(2),
        || budget.accounting().reserved().get() > baseline), Err(AtlasNavigationError::Atlas(AtlasError::Canceled))));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(AtlasSources::build(&index, &valid, &tiny, allocation(2), || false), Err(AtlasNavigationError::Atlas(AtlasError::ResourceDenied))));
    assert_eq!(budget.accounting().reserved().get(), baseline);
}

#[test]
fn selection_focus_and_back_forward_restore_independent_semantic_state() {
    let layout = layout(1); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let mut nav = AtlasNavigation::new(&index, camera(&index), 8, &budget, allocation(2)).unwrap();
    let initial = nav.location();
    let first = index.find_path(b"src/Thing.rs").unwrap();
    let second = index.find_path(b"src/thing.rs").unwrap();
    nav.select(Some(first)).unwrap();
    assert_eq!(nav.location().focus(), initial.focus());
    assert_eq!(nav.location().camera(), initial.camera());
    assert_eq!(nav.back_len(), 0);
    nav.fit_selection(10.0).unwrap();
    nav.pan(Point2D::new(12.0, -8.0).unwrap()).unwrap();
    let first_location = nav.location();
    nav.select(Some(second)).unwrap();
    nav.fit_selection(10.0).unwrap();
    let before_back = nav.location().camera().generation().get();
    assert!(nav.go_back().unwrap());
    assert_eq!(nav.location().focus(), first_location.focus());
    assert_eq!(nav.location().camera().origin(), first_location.camera().origin());
    assert_eq!(nav.location().camera().points_per_unit(), first_location.camera().points_per_unit());
    // Selection before entering second was already second; it is not forced to focus.
    assert_eq!(nav.location().selection(), Some(second));
    assert!(nav.location().camera().generation().get() > before_back);
    assert!(nav.go_forward().unwrap());
    assert_eq!(nav.location().focus(), second);
    assert!(nav.go_back().unwrap());
    assert!(nav.go_back().unwrap());
    assert_eq!(nav.location().focus(), index.root_node());
    assert_eq!(nav.location().selection(), Some(first));
    assert_eq!(nav.location().camera().origin(), initial.camera().origin());
    assert!(!nav.go_back().unwrap());
}

#[test]
fn gestures_do_not_fill_history_and_a_new_route_retires_the_forward_branch() {
    let layout = layout(1); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let mut nav = AtlasNavigation::new(&index, camera(&index), 3, &budget, allocation(2)).unwrap();
    let reserved = budget.accounting().reserved().get();
    for _ in 0..200 {
        nav.pan(Point2D::new(0.25, -0.5).unwrap()).unwrap();
        nav.zoom_at(Point2D::new(100.0, 100.0).unwrap(), 1.001).unwrap();
    }
    assert_eq!(nav.back_len(), 0);
    let neighborhood = index.find_path(b"src").unwrap();
    for _ in 0..20 { nav.focus(neighborhood, 4.0).unwrap(); }
    assert_eq!(nav.back_len(), 3);
    assert_eq!(budget.accounting().reserved().get(), reserved);
    nav.go_back().unwrap(); assert!(nav.can_go_forward());
    nav.fit_project(4.0).unwrap(); assert!(!nav.can_go_forward());
    assert!(!nav.fit_parent(4.0).unwrap());
    assert_eq!(budget.accounting().reserved().get(), reserved);
}

#[test]
fn history_restores_on_current_display_without_resurrecting_old_display_metrics() {
    let layout = layout(1); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let mut nav = AtlasNavigation::new(&index, camera(&index), 8, &budget, allocation(2)).unwrap();
    let old_center = nav.location().camera().logical_to_local(Point2D::new(400.0, 300.0).unwrap()).unwrap();
    nav.focus(index.find_path(b"src").unwrap(), 10.0).unwrap();
    let current_display = display(1000.0, 400.0, 2);
    nav.set_display(current_display).unwrap();
    nav.go_back().unwrap();
    assert_eq!(nav.location().camera().display(), current_display);
    let center = nav.location().camera().logical_to_local(Point2D::new(500.0, 200.0).unwrap()).unwrap();
    assert!((center.x() - old_center.x()).abs() < 1e-8);
    assert!((center.y() - old_center.y()).abs() < 1e-8);
}

#[test]
fn failed_navigation_and_generation_exhaustion_leave_state_and_history_unchanged() {
    let layout = layout(1); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let initial = camera(&index);
    let max = Camera2D::new(CameraGeneration::new(owner(), u64::MAX).unwrap(), initial.display(),
        initial.origin(), initial.points_per_unit()).unwrap();
    let mut nav = AtlasNavigation::new(&index, max, 8, &budget, allocation(2)).unwrap();
    let before = nav.location();
    assert_eq!(nav.fit_project(1.0), Err(AtlasError::Camera(CameraError::GenerationExhausted)));
    assert_eq!(nav.location(), before); assert_eq!(nav.back_len(), 0);
    assert!(!nav.go_back().unwrap());
    let mut ordinary = AtlasNavigation::new(&index, initial, 8, &budget, allocation(3)).unwrap();
    let before = ordinary.location();
    assert!(ordinary.focus(index.root_node(), f64::NAN).is_err());
    assert_eq!(ordinary.location(), before); assert_eq!(ordinary.back_len(), 0);
}

#[test]
fn old_layout_bindings_and_navigation_do_not_alias_a_repacked_generation() {
    let old_layout = layout(1); let new_layout = layout(2); let budget = budget();
    let old = AtlasIndex::build(&old_layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let new = AtlasIndex::build(&new_layout, AtlasBuildLimits::default(), &budget, allocation(2), || false).unwrap();
    let mut nav = AtlasNavigation::new(&new, camera(&new), 8, &budget, allocation(3)).unwrap();
    assert_eq!(nav.select(Some(old.find_path(b"src/Thing.rs").unwrap())), Err(AtlasError::StaleLayout));
    assert!(matches!(AtlasSources::build(&new, &bindings(&old), &budget, allocation(4), || false),
        Err(AtlasNavigationError::Atlas(AtlasError::StaleLayout))));
    assert_eq!(nav.location().selection(), None);
}

#[test]
fn history_and_source_bindings_release_only_their_own_admitted_capacity() {
    let layout = layout(1); let budget = budget();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let baseline = budget.accounting().reserved().get();
    let sources = AtlasSources::build(&index, &bindings(&index), &budget, allocation(2), || false).unwrap();
    let source_charge = budget.accounting().reserved().get() - baseline;
    let nav = AtlasNavigation::new(&index, camera(&index), 2, &budget, allocation(3)).unwrap();
    assert!(budget.accounting().reserved().get() > baseline + source_charge);
    drop(nav); assert_eq!(budget.accounting().reserved().get(), baseline + source_charge);
    drop(sources); assert_eq!(budget.accounting().reserved().get(), baseline);
}

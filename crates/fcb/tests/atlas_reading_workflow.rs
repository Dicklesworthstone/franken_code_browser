#![forbid(unsafe_code)]
#![cfg(all(feature = "map", feature = "search"))]

//! Public consumer workflow across the actual map, search and reader engines.
//! Presentation is explicitly host-acknowledged; no native/GPU pass is claimed.

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, CameraGeneration, FcbError,
    FileId, Point2D, PresentedFrameId, Size2D, SourceCapture, SourceProvider, SourceRevision};
use fcb::map::{AtlasBuildLimits, AtlasIndex, AtlasNavigation, AtlasSourceBinding,
    AtlasSources, DisplayColorConfig, HierarchySpec, LayoutOptions, LayoutRevision,
    LodThresholds, NodeKind, NodeSpec, QueryGeneration, ResourceAllocationId,
    ResourceBudget, RootId, VisibleLimits, VisiblePlan, VisibleQuery, VisibleState, commit_layout};
use fcb::search::{DirectSourceScanner, QueryOptions, ReaderLimits, ReadingSeekState, ReadingWindowOptions};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1015).unwrap() }
fn file(id: u64) -> FileId { FileId::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn build_plan(index: &AtlasIndex<'_>, nav: &AtlasNavigation<'_, '_>, budget: &ResourceBudget, id: u64) -> VisiblePlan {
    let location = nav.location();
    let mut query = VisibleQuery::new(index, location.focus(), location.camera(), generation(id),
        LodThresholds::new(0.001, 0.0).unwrap(), VisibleLimits::default(), None, budget, allocation(id)).unwrap();
    for _ in 0..1000 {
        if query.state() == VisibleState::Complete { break; }
        query.step(2, generation(id), || false).unwrap();
    }
    query.finish().unwrap()
}
struct ForbiddenProvider(Arc<AtomicUsize>);
impl SourceProvider for ForbiddenProvider {
    fn capture(&self, _: &str) -> Result<SourceCapture, FcbError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(FcbError::ProviderUnavailable)
    }
}

#[test]
fn atlas_to_exact_reader_to_search_and_back_preserves_file_capture_and_spatial_place() {
    let calls = Arc::new(AtomicUsize::new(0));
    let session = BrowserSession::with_provider(owner(), Arc::new(ForbiddenProvider(Arc::clone(&calls))));
    let budget = ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap();
    let hierarchy = HierarchySpec::new(owner(), RootId::new(owner(), 1).unwrap(), vec![
        NodeSpec::new(b"src/Thing.rs".to_vec(), NodeKind::File, Some(100)),
        NodeSpec::new(b"src/thing.rs".to_vec(), NodeKind::File, Some(100)),
    ]).unwrap();
    let layout = commit_layout(LayoutRevision::new(owner(), 1).unwrap(), Size2D::new(1000.0, 800.0).unwrap(),
        &hierarchy, LayoutOptions::modest()).unwrap();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let selected_node = index.find_path(b"src/Thing.rs").unwrap();
    let sources = AtlasSources::build(&index, &[
        AtlasSourceBinding { node: selected_node, file: file(1) },
        AtlasSourceBinding { node: index.find_path(b"src/thing.rs").unwrap(), file: file(2) },
    ], &budget, allocation(2), || false).unwrap();
    let display = fcb::DisplayMetrics::new(2.0, Size2D::new(800.0, 600.0).unwrap(), DisplayColorConfig::Srgb,
        fcb::DisplayGeneration::new(owner(), 1).unwrap()).unwrap();
    let camera = index.focus_camera(index.root_node(), CameraGeneration::new(owner(), 1).unwrap(), display, 12.0).unwrap();
    let mut navigation = AtlasNavigation::new(&index, camera, 8, &budget, allocation(3)).unwrap();
    let overview = build_plan(&index, &navigation, &budget, 4);
    let shown = overview.acknowledge_presented(PresentedFrameId::new(owner(), 1).unwrap(), display).unwrap();
    let parcel = overview.parcels().iter().find(|parcel| parcel.node() == selected_node).unwrap();
    let rect = parcel.logical_rect();
    let click = Point2D::new(rect.min_x() + rect.size().width() / 2.0, rect.min_y() + rect.size().height() / 2.0).unwrap();
    let hit = shown.hit_test(click, display.generation()).unwrap().unwrap();
    navigation.select_hit(shown, hit).unwrap();
    let target = sources.source_target(shown, hit).unwrap();
    // Selection and exact file intent are already fixed before any camera flight.
    assert_eq!(navigation.location().selection(), Some(selected_node));
    assert_eq!(target.file(), file(1));
    navigation.fit_selection(12.0).unwrap();
    assert_eq!(navigation.location().focus(), selected_node);

    let mut raw = vec![0xff, 0xfe];
    for unit in "head\r\nalpha needle 😀\r\nlast".encode_utf16() { raw.extend_from_slice(&unit.to_le_bytes()); }
    let supplied = SourceCapture::from_bytes(owner(), file(1), SourceRevision::new(owner(), 7).unwrap(),
        "host lookup key, not a filesystem grant", raw.clone()).unwrap();
    let pointer = supplied.bytes().as_ptr();
    let view = session.open_atlas_target(&sources, shown, target, supplied).unwrap();
    assert_eq!(view.source().bytes().as_ptr(), pointer);
    let reader = view.source_reader(ReaderLimits::default(), &budget, allocation(5)).unwrap();
    let mut first = reader.seek(fcb::search::ReadingTarget::Byte(fcb::ByteOffset::new(0)), generation(5)).unwrap();
    let first_anchor = match first.step(4, generation(5), || false).unwrap() {
        ReadingSeekState::Ready(anchor) => anchor, state => panic!("first row not ready: {state:?}"),
    };
    let first_window = reader.window(first_anchor, generation(5), ReadingWindowOptions::default(), &budget, allocation(6), || false).unwrap();
    assert_eq!(first_window.line_text(0), Some("head"));
    assert_eq!(first_window.line_text(1), Some("alpha needle 😀"));

    let prepared = session.prepare_search_capture(view.source().clone()).unwrap();
    let results = DirectSourceScanner::scan_complete_capture(prepared.capture(), "needle", &QueryOptions::new(generation(6))).unwrap();
    assert!(results.is_complete()); assert_eq!(results.matches.len(), 1);
    let source_hit = &results.matches[0];
    assert_eq!(sources.node_for_file(source_hit.file_id).unwrap(), selected_node);
    let mut seek = reader.seek_hit(source_hit, generation(6)).unwrap();
    for _ in 0..1000 {
        if !matches!(seek.state(), ReadingSeekState::Pending) { break; }
        seek.step(4, generation(6), || false).unwrap();
    }
    let at = match seek.state() { ReadingSeekState::Ready(at) => at, state => panic!("hit not ready: {state:?}") };
    assert_eq!(at.line_number(), 2); assert_eq!(at.offset().get(), 26);
    let hit_window = reader.window(at, generation(6), ReadingWindowOptions::default(), &budget, allocation(7), || false).unwrap();
    let selected = hit_window.text_selection(hit_window.source_to_text(source_hit.original_byte_range).unwrap()).unwrap();
    assert_eq!(selected.text, "needle");
    assert_eq!(selected.original_bytes, prepared.hit_bytes(source_hit).unwrap());
    assert_eq!(hit_window.frame_plan().source().get(), 7);

    assert!(navigation.go_back().unwrap());
    assert_eq!(navigation.location().focus(), index.root_node());
    assert_eq!(navigation.location().selection(), Some(selected_node));
    assert_eq!(navigation.location().camera().origin(), camera.origin());
    assert_eq!(navigation.location().camera().points_per_unit(), camera.points_per_unit());
    assert!(navigation.location().camera().generation().get() > camera.generation().get());
    let returned = build_plan(&index, &navigation, &budget, 8);
    let again = returned.acknowledge_presented(PresentedFrameId::new(owner(), 2).unwrap(), display).unwrap();
    assert_eq!(again.hit_test(click, display.generation()).unwrap().unwrap().node(), selected_node);
    assert!(target.validate(&sources, again).is_err(), "returning must not resurrect an old frame token");
    assert_eq!(view.source().bytes(), raw);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "metadata, camera, reading and return cannot secretly recapture live source");
}

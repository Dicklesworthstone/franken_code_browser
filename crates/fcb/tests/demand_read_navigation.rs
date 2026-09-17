#![forbid(unsafe_code)]
#![cfg(all(feature = "search", unix))]

//! Real positioned-file read → retained extent → public navigation → decode →
//! exact source search. A sparse file exercises >4 GiB offsets without a huge
//! allocation. These tests are not native GUI/presentation qualification.

use std::{fs::{self, File, OpenOptions}, os::unix::fs::FileExt,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, ByteOffset, ByteRange, FcbError, FileId, SourceRevision};
use fcb::search::{CaptureRequest, DetectedEncoding, ExtentActivationError, ExtentQuery,
    ExtentQueryError, ExtentQueryOptions, ExtentQueryState, ExtentReadState, ExtentStepBudget,
    ExtentWindowRequest, FileRangeReader, MembershipState, NativeSourceIdentity,
    ObservedExtent, PathEntry, PathIndex, PathIndexLimits, PathNavigationTarget, PathSearch,
    PathSearchOptions, QueryGeneration, RawPath, ResourceAllocationId, ResourceBudget, RootId, SearchManifestId};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1411).unwrap() }
fn file() -> FileId { FileId::new(owner(), 1).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(32 * 1024 * 1024)).unwrap() }
fn range(start: u64, end: u64) -> ByteRange { ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap() }
fn request(revision: u64, range: ByteRange) -> CaptureRequest {
    CaptureRequest::new(file(), SourceRevision::new(owner(), revision).unwrap()).unwrap().with_range(range).unwrap()
}
fn utf16(text: &str) -> Vec<u8> { text.encode_utf16().flat_map(u16::to_le_bytes).collect() }
fn native_file() -> File {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fcb-demand-navigation-{}-{nanos}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&dir).unwrap();
    OpenOptions::new().create_new(true).read(true).write(true).open(dir.join("source.bin")).unwrap()
}
fn capture(reader: &mut FileRangeReader, revision: u64, range: ByteRange, budget: &ResourceBudget, id: u64) -> ObservedExtent {
    let mut pending = reader.begin(request(revision, range), budget, allocation(id)).unwrap();
    for _ in 0..1000 {
        if pending.state() == ExtentReadState::Ready { break; }
        pending.step(ExtentStepBudget { max_bytes: 7, max_calls: 1 }, || false).unwrap();
        assert!(pending.stats().last_step_bytes <= 7);
    }
    assert_eq!(pending.stats().bytes_read, range.len().get());
    pending.finish(|| false).unwrap()
}

#[test]
fn path_navigation_reads_only_a_window_and_old_hits_survive_live_file_changes() {
    let budget = budget();
    let session = BrowserSession::new(owner());
    let name = RawPath::from_str("src/Large.rs");
    let root = RootId::new(owner(), 1).unwrap();
    let manifest = SearchManifestId::new(owner(), 1).unwrap();
    let paths = PathIndex::build(manifest, MembershipState::Closed,
        &[PathEntry::new(file(), root, &name)], PathIndexLimits::default(), &budget, allocation(1), || false).unwrap();
    let mut lookup = PathSearch::new(&paths, b"Large.rs", PathSearchOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    lookup.run_to_completion(|| false).unwrap();
    let target = PathNavigationTarget::from_search(&lookup, file()).unwrap();
    let native = NativeSourceIdentity { root, path: &name };

    let base = (1u64 << 32) + 64;
    let handle = native_file();
    let bytes = utf16("AAAA\u{feff}😀needle ZZZZ");
    handle.set_len(base + bytes.len() as u64 + 128).unwrap();
    handle.write_all_at(&bytes, base).unwrap();
    let mut reader = FileRangeReader::new(target.file(), handle.try_clone().unwrap()).unwrap();
    let plan = ExtentWindowRequest::new(ByteOffset::new(base + 8), 18, reader.observed_length().unwrap()).unwrap();
    let captured = capture(&mut reader, 1, plan.capture, &budget, 3);
    assert!(captured.bytes().len() <= 34);
    assert!(!captured.covers_whole_observation());
    let view = session.open_path_extent(&target, manifest, generation(1), native, captured.clone()).unwrap();
    assert!(matches!(session.open_path_extent(&target, manifest, generation(2), native, captured),
        Err(ExtentActivationError::Source(FcbError::StaleGeneration))));
    let text = view.decode(plan.visible, DetectedEncoding::Utf16Le, generation(2), &budget, allocation(4), || false).unwrap();
    assert_eq!(text.text(), "\u{feff}😀needle");
    assert_eq!(text.first_line_number(), None);
    let mut query = ExtentQuery::text(&text, "needle", ExtentQueryOptions::new(generation(3)), &budget, allocation(5)).unwrap();
    while query.state() == ExtentQueryState::Pending { query.step(2, generation(3), || false).unwrap(); }
    assert!(query.scope_complete()); assert!(query.has_unsearched_source());
    assert_eq!(query.hits().len(), 1);
    let hit = query.hits()[0];
    assert_eq!(hit.original_range(), range(base + 14, base + 26));
    assert_eq!(hit.original_bytes().unwrap(), utf16("needle"));
    assert_eq!(text.frame_plan().file(), file());
    assert_eq!(text.frame_plan().bytes(), plan.visible);

    handle.write_all_at(&utf16("XXXXXX"), base + 14).unwrap();
    let current = capture(&mut reader, 2, plan.capture, &budget, 6);
    let current = session.open_path_extent(&target, manifest, generation(1), native, current).unwrap();
    assert_eq!(hit.validate_delivery(&current, generation(3)), Err(ExtentQueryError::StaleObservation));
    assert_eq!(hit.original_bytes().unwrap(), utf16("needle"));
    assert_eq!(current.raw_selection(range(base + 14, base + 26)).unwrap(), utf16("XXXXXX"));
    drop(reader);
    assert_eq!(text.text(), "\u{feff}😀needle");
    assert!(budget.accounting().reserved().get() < 128 * 1024, "logical file size must not become retained payload size");
}

#[test]
fn native_path_identity_is_still_checked_for_extent_activation() {
    let budget = budget(); let session = BrowserSession::new(owner());
    let name = RawPath::from_bytes(b"src/\xff.rs".as_slice());
    let root = RootId::new(owner(), 1).unwrap();
    let manifest = SearchManifestId::new(owner(), 1).unwrap();
    let index = PathIndex::build(manifest, MembershipState::Closed, &[PathEntry::new(file(), root, &name)],
        PathIndexLimits::default(), &budget, allocation(1), || false).unwrap();
    let mut query = PathSearch::new(&index, &[0xff], PathSearchOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    query.run_to_completion(|| false).unwrap();
    let target = PathNavigationTarget::from_search(&query, file()).unwrap();
    let extent = ObservedExtent::from_bytes(request(1, range(100, 104)), ByteLength::new(1000), b"data", &budget, allocation(3)).unwrap();
    let wrong = RawPath::from_str("src/\\xff.rs");
    assert!(matches!(session.open_path_extent(&target, manifest, generation(1),
        NativeSourceIdentity { root, path: &wrong }, extent.clone()), Err(ExtentActivationError::Source(FcbError::StaleGeneration))));
    let view = session.open_path_extent(&target, manifest, generation(1), NativeSourceIdentity { root, path: &name }, extent).unwrap();
    assert_eq!(view.raw_selection(range(100, 104)).unwrap(), b"data");
    assert!(view.raw_selection(range(99, 104)).is_err());
}

#[test]
#[cfg(feature = "map")]
fn presented_atlas_parcel_opens_a_partial_capture_without_requiring_whole_file_bytes() {
    use fcb::{CameraGeneration, DisplayGeneration, DisplayMetrics, Point2D, PresentedFrameId, Size2D};
    use fcb::map::{AtlasBuildLimits, AtlasIndex, AtlasSources, AtlasSourceBinding, DisplayColorConfig,
        HierarchySpec, LayoutOptions, LayoutRevision, LodThresholds, NodeKind, NodeSpec,
        VisibleLimits, VisibleQuery, VisibleState, commit_layout};
    let budget = budget(); let session = BrowserSession::new(owner());
    let spec = HierarchySpec::new(owner(), RootId::new(owner(), 1).unwrap(),
        vec![NodeSpec::new(b"large.rs".to_vec(), NodeKind::File, Some(1u64 << 35))]).unwrap();
    let layout = commit_layout(LayoutRevision::new(owner(), 1).unwrap(), Size2D::new(1000.0, 800.0).unwrap(), &spec, LayoutOptions::modest()).unwrap();
    let index = AtlasIndex::build(&layout, AtlasBuildLimits::default(), &budget, allocation(1), || false).unwrap();
    let node = index.find_path(b"large.rs").unwrap();
    let sources = AtlasSources::build(&index, &[AtlasSourceBinding { node, file: file() }], &budget, allocation(2), || false).unwrap();
    let display = DisplayMetrics::new(1.0, Size2D::new(800.0, 600.0).unwrap(), DisplayColorConfig::Srgb,
        DisplayGeneration::new(owner(), 1).unwrap()).unwrap();
    let camera = index.focus_camera(index.root_node(), CameraGeneration::new(owner(), 1).unwrap(), display, 0.0).unwrap();
    let mut pending = VisibleQuery::new(&index, index.root_node(), camera, generation(1),
        LodThresholds::new(0.001, 0.0).unwrap(), VisibleLimits::default(), None, &budget, allocation(3)).unwrap();
    while pending.state() == VisibleState::Pending { pending.step(8, generation(1), || false).unwrap(); }
    let plan = pending.finish().unwrap();
    let shown = plan.acknowledge_presented(PresentedFrameId::new(owner(), 1).unwrap(), display).unwrap();
    let rect = plan.parcels().iter().find(|parcel| parcel.node() == node).unwrap().logical_rect();
    let point = Point2D::new(rect.min_x() + rect.size().width() / 2.0, rect.min_y() + rect.size().height() / 2.0).unwrap();
    let hit = shown.hit_test(point, display.generation()).unwrap().unwrap();
    let target = sources.source_target(shown, hit).unwrap();
    let offset = 1u64 << 34;
    let extent = ObservedExtent::from_bytes(request(1, range(offset, offset + 6)), ByteLength::new(1u64 << 35),
        b"window", &budget, allocation(4)).unwrap();
    let view = session.open_atlas_extent(&sources, shown, target, extent).unwrap();
    assert_eq!(view.raw_selection(range(offset, offset + 6)).unwrap(), b"window");
    assert_eq!(view.frame_plan().bytes(), range(offset, offset + 6));
    assert_eq!(view.frame_plan().file(), file());
}

#![forbid(unsafe_code)]
#![cfg(all(feature = "map", feature = "search", unix))]

//! Actual directory discovery -> retained atlas -> acknowledged hit -> exact
//! supplied source. Portable logic only; no native presentation is claimed.
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, CameraGeneration, DisplayGeneration,
    DisplayMetrics, FileId, Point2D, PresentedFrameId, Size2D, SourceCapture, SourceRevision};
use fcb::map::{AtlasBuildLimits, AtlasError, AtlasIndex, AtlasNodeId, DisplayColorConfig,
    LayoutOptions, LayoutRevision, LodThresholds, QueryGeneration, ResourceAllocationId,
    ResourceBudget, RootId, VisibleLimits, VisibleQuery, VisibleState};
use fcb::map::workspace::{WorkspaceAtlas, WorkspaceAtlasError, WorkspaceAtlasLimits};
use fcb::search::{RawPath, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceError, WorkspaceLimits, WorkspaceStage};
use fcb::source::{CancelFlag, SourceError};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(710).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn revision(n: u64) -> LayoutRevision { LayoutRevision::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-workspace-atlas-{}-{now}-{id}", std::process::id()));
    fs::create_dir(&path).unwrap(); path
}
fn catalog(path: &PathBuf, budget: &ResourceBudget, max_files: usize) -> WorkspaceCatalog {
    let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(path));
    let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).unwrap(),
        FileId::new(owner(), 100).unwrap(), WorkspaceLimits { max_files, ..WorkspaceLimits::default() },
        false, budget, allocation(1)).unwrap();
    let cancel = CancelFlag::new();
    for _ in 0..4097 {
        if catalog.stage() != WorkspaceStage::Discovering { break; }
        catalog.step(&cancel).unwrap();
    }
    assert_eq!(catalog.stage(), WorkspaceStage::Ready); catalog
}
fn build<'a>(catalog: &'a WorkspaceCatalog, budget: &ResourceBudget, id: u64) -> WorkspaceAtlas<'a> {
    WorkspaceAtlas::build(catalog, &fcb::map::workspace::AtlasScope::All, revision(id), Size2D::new(1024.0, 768.0).unwrap(),
        LayoutOptions::modest(), WorkspaceAtlasLimits::default(), budget, allocation(id), || false).unwrap()
}

#[test]
#[cfg(target_os = "linux")] // Actual case-sensitive/raw-byte filesystem fixture.
fn real_discovery_binds_case_distinct_and_non_utf8_paths_without_payload_captures() {
    use std::os::unix::ffi::OsStringExt;
    let root = root(); fs::create_dir(root.join("src")).unwrap();
    fs::create_dir(root.join("empty")).unwrap(); fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("target/hidden.rs"), b"excluded").unwrap();
    fs::write(root.join("src/A.rs"), b"alpha").unwrap(); fs::write(root.join("src/a.rs"), b"beta").unwrap();
    let native = std::ffi::OsString::from_vec(b"src/raw-\xff.rs".to_vec());
    fs::write(root.join(native), b"original").unwrap();
    let budget = budget(); let catalog = catalog(&root, &budget, 4096);
    let atlas = build(&catalog, &budget, 2); let index = atlas.index(&budget, allocation(3), || false).unwrap();
    assert_eq!(atlas.file_count(), 3); assert_eq!(index.len(), 5);
    assert!(atlas.discovery_complete());
    assert!(index.find_path(b"target/hidden.rs").is_none()); assert!(index.find_path(b"empty").is_none());
    for path in [b"src/A.rs".as_slice(), b"src/a.rs", b"src/raw-\xff.rs"] {
        let node = index.find_path(path).unwrap(); let file = atlas.file(node).unwrap();
        assert_eq!(atlas.node_for_file(file).unwrap(), node);
        assert_eq!(atlas.entry(node).unwrap().path().as_bytes(), path);
    }
    assert_ne!(atlas.file(index.find_path(b"src/A.rs").unwrap()).unwrap(),
        atlas.file(index.find_path(b"src/a.rs").unwrap()).unwrap());
    assert!(atlas.file(index.find_path(b"src").unwrap()).is_err());
}

#[test]
fn discovered_parcel_opens_the_explicit_capture_and_not_its_display_label() {
    let root = root(); fs::write(root.join("file.rs"), b"old metadata observation").unwrap();
    let budget = budget(); let catalog = catalog(&root, &budget, 10);
    let atlas = build(&catalog, &budget, 2); let index = atlas.index(&budget, allocation(3), || false).unwrap();
    let sources = atlas.sources(&index, &budget, allocation(4), || false).unwrap();
    let display = DisplayMetrics::new(2.0, Size2D::new(800.0, 600.0).unwrap(), DisplayColorConfig::Srgb,
        DisplayGeneration::new(owner(), 1).unwrap()).unwrap();
    let camera = index.focus_camera(index.root_node(), CameraGeneration::new(owner(), 1).unwrap(), display, 12.0).unwrap();
    let generation = QueryGeneration::new(owner(), 1).unwrap();
    let mut query = VisibleQuery::new(&index, index.root_node(), camera, generation,
        LodThresholds::new(0.001, 0.0).unwrap(), VisibleLimits::default(), None, &budget, allocation(5)).unwrap();
    while query.state() == VisibleState::Pending { query.step(64, generation, || false).unwrap(); }
    let plan = query.finish().unwrap();
    let shown = plan.acknowledge_presented(PresentedFrameId::new(owner(), 1).unwrap(), display).unwrap();
    let selected = index.find_path(b"file.rs").unwrap();
    let rect = plan.parcels().iter().find(|p| p.node() == selected && p.is_source_parcel()).unwrap().logical_rect();
    let hit = shown.hit_test(Point2D::new(rect.min_x() + rect.size().width() / 2.0,
        rect.min_y() + rect.size().height() / 2.0).unwrap(), display.generation()).unwrap().unwrap();
    let target = sources.source_target(shown, hit).unwrap();
    // Atlas metadata does not claim to pin unread bytes. This explicit new
    // capture is allowed; an exact text-search hit has a different contract.
    fs::write(root.join("file.rs"), b"new exact source\r\n").unwrap();
    let bytes = fs::read(root.join(atlas.entry(selected).unwrap().path().raw().to_path_buf())).unwrap();
    let capture = SourceCapture::from_bytes(owner(), target.file(), SourceRevision::new(owner(), 7).unwrap(),
        "host key, never a path grant", bytes.clone()).unwrap();
    let view = BrowserSession::new(owner()).open_atlas_target(&sources, shown, target, capture).unwrap();
    assert_eq!(view.source().bytes(), bytes); assert_eq!(view.source().file(), catalog.file_id(0).unwrap());
}

#[test]
fn inferred_directories_share_one_node_and_layout_restores_deterministically() {
    let root = root(); fs::create_dir_all(root.join("a/nested")).unwrap();
    for path in ["a/z.rs", "a/nested/b.rs", "a/nested/a.rs", "a.rs"] { fs::write(root.join(path), b"x").unwrap(); }
    let budget = budget(); let catalog = catalog(&root, &budget, 10);
    let first = build(&catalog, &budget, 2); let second = build(&catalog, &budget, 3);
    assert_eq!(first.layout().nodes(), second.layout().nodes());
    assert_eq!(first.layout().nodes().len(), 7);
    assert_eq!(first.layout().restore(revision(2)).unwrap(), first.layout());
    let node = first.node_for_file(catalog.file_id(0).unwrap()).unwrap();
    assert!(matches!(second.file(node), Err(WorkspaceAtlasError::Atlas(AtlasError::StaleLayout))));
}

#[test]
fn partial_discovery_and_empty_scope_are_not_fabricated_complete_inventories() {
    let root = root();
    for name in ["a", "b", "c"] { fs::write(root.join(name), b"x").unwrap(); }
    let budget = budget(); let limited = catalog(&root, &budget, 1);
    let atlas = build(&limited, &budget, 2);
    assert_eq!(atlas.file_count(), 1); assert!(!atlas.discovery_complete());
    let empty = root.join("empty"); fs::create_dir(&empty).unwrap();
    drop(atlas); drop(limited);
    let empty = catalog(&empty, &budget, 1); let atlas = build(&empty, &budget, 2);
    assert!(atlas.discovery_complete()); assert_eq!(atlas.file_count(), 0); assert_eq!(atlas.layout().nodes().len(), 1);
}

#[test]
fn admission_counts_ancestors_and_rejects_cancellation_before_publication() {
    let root = root(); fs::create_dir_all(root.join("a/b")).unwrap(); fs::write(root.join("a/b/c"), b"x").unwrap();
    let budget = budget(); let catalog = catalog(&root, &budget, 10);
    for limits in [WorkspaceAtlasLimits { max_nodes: 3, ..WorkspaceAtlasLimits::default() },
        WorkspaceAtlasLimits { max_depth: 2, ..WorkspaceAtlasLimits::default() },
        WorkspaceAtlasLimits { max_total_path_bytes: 8, ..WorkspaceAtlasLimits::default() }] {
        assert!(matches!(WorkspaceAtlas::build(&catalog, &fcb::map::workspace::AtlasScope::All, revision(2), Size2D::new(100.0, 100.0).unwrap(),
            LayoutOptions::modest(), limits, &budget, allocation(2), || false), Err(WorkspaceAtlasError::InvalidLimits)));
    }
    assert!(matches!(WorkspaceAtlas::build(&catalog, &fcb::map::workspace::AtlasScope::All, revision(2), Size2D::new(100.0, 100.0).unwrap(),
        LayoutOptions::modest(), WorkspaceAtlasLimits::default(), &budget, allocation(2), || true),
        Err(WorkspaceAtlasError::Atlas(AtlasError::Canceled))));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(WorkspaceAtlas::build(&catalog, &fcb::map::workspace::AtlasScope::All, revision(2), Size2D::new(100.0, 100.0).unwrap(),
        LayoutOptions::modest(), WorkspaceAtlasLimits::default(), &tiny, allocation(2), || false),
        Err(WorkspaceAtlasError::Atlas(AtlasError::ResourceDenied))));
    assert_eq!(build(&catalog, &budget, 2).file_count(), 1);
}

#[test]
fn foreign_layout_same_numeric_identity_and_revoked_grant_cannot_activate() {
    let root = root(); fs::write(root.join("a"), b"x").unwrap();
    let budget = budget(); let catalog = catalog(&root, &budget, 10); let atlas = build(&catalog, &budget, 2);
    let copied = atlas.layout().clone();
    let index = AtlasIndex::build(&copied, AtlasBuildLimits::default(), &budget, allocation(3), || false).unwrap();
    assert!(matches!(atlas.sources(&index, &budget, allocation(4), || false), Err(WorkspaceAtlasError::WrongIndex)));
    let foreign = FileId::new(ArenaOwnerId::new(711).unwrap(), 100).unwrap();
    assert!(matches!(atlas.node_for_file(foreign), Err(WorkspaceAtlasError::Atlas(AtlasError::OwnerMismatch))));
    let original = atlas.node_for_file(catalog.file_id(0).unwrap()).unwrap();
    let stale = AtlasNodeId::new(original.root(), revision(99), original.ordinal());
    assert!(atlas.file(stale).is_err());
    catalog.grant().revoke();
    assert!(matches!(atlas.file(original), Err(WorkspaceAtlasError::Workspace(WorkspaceError::Source(SourceError::GrantRevoked)))));
    assert!(atlas.index(&budget, allocation(4), || false).is_err());
}

use fcb::map::workspace::path_search::{WorkspacePathIndex, WorkspacePathSearchError};
use fcb::search::{PathSearchError, PathSearchOptions};
fn path_options(limit: usize, generation: u64) -> PathSearchOptions {
    let mut options = PathSearchOptions::new(QueryGeneration::new(owner(), generation).unwrap());
    options.max_results = limit; options
}

#[test]
fn retained_path_search_matches_the_existing_engine_without_repacking_any_node() {
    let root = root();
    for dir in ["src", "src-old", "docs"] { fs::create_dir(root.join(dir)).unwrap(); }
    for path in ["src/parser.rs", "src/parser_test.rs", "src-old/parser.rs", "docs/parser.md", "readme"] {
        fs::write(root.join(path), b"source payload not needed for path queries").unwrap();
    }
    let budget = budget(); let catalog = catalog(&root, &budget, 10);
    let atlas = build(&catalog, &budget, 2); let spatial = atlas.index(&budget, allocation(3), || false).unwrap();
    let before = atlas.layout().nodes().to_vec();
    let paths = WorkspacePathIndex::build(&atlas, &budget, [allocation(4), allocation(5)], || false).unwrap();
    let options = path_options(100, 1);
    let matches = paths.search(b"parser", options, &budget, [allocation(6), allocation(7)], || false).unwrap();
    assert!(matches.is_complete()); assert!(!matches.truncated()); assert_eq!(matches.matches_seen(), 4);
    assert_eq!(matches.hits().len(), 4);
    for (path, expected) in [(b"".as_slice(), 4), (b"src", 2), (b"src-old", 1), (b"docs", 1), (b"readme", 0)] {
        assert_eq!(matches.retained_matches_in(spatial.find_path(path).unwrap()).unwrap(), expected);
    }
    for hit in matches.hits() { assert_eq!(atlas.file(hit.node).unwrap(), hit.file); }
    let refined = paths.search(b"parser_test", path_options(100, 2), &budget,
        [allocation(8), allocation(9)], || false).unwrap();
    assert_eq!(refined.hits().len(), 1); assert_eq!(matches.hits().len(), 4);
    assert_eq!(atlas.entry(refined.hits()[0].node).unwrap().path().as_bytes(), b"src/parser_test.rs");
    assert_eq!(atlas.layout().nodes(), before.as_slice());
    assert_eq!(spatial.revision(), atlas.layout().revision());
    assert!(matches.validate_delivery(&atlas, QueryGeneration::new(owner(), 2).unwrap()).is_err());
    refined.validate_delivery(&atlas, QueryGeneration::new(owner(), 2).unwrap()).unwrap();
}

#[test]
fn path_overlay_counting_distinguishes_truncated_rows_and_partial_membership() {
    let root = root(); for name in ["match-a", "match-b", "match-c"] { fs::write(root.join(name), b"unused").unwrap(); }
    let budget = budget(); let catalog = catalog(&root, &budget, 10); let atlas = build(&catalog, &budget, 2);
    let paths = WorkspacePathIndex::build(&atlas, &budget, [allocation(3), allocation(4)], || false).unwrap();
    for limit in [0, 1, 3] {
        let results = paths.search(b"match", path_options(limit, 1), &budget, [allocation(5), allocation(6)], || false).unwrap();
        assert!(results.is_complete()); assert_eq!(results.matches_seen(), 3);
        assert_eq!(results.hits().len(), limit); assert_eq!(results.truncated(), limit < 3);
        let node = AtlasNodeId::new(atlas.layout().root(), atlas.layout().revision(), 0);
        assert_eq!(results.retained_matches_in(node).unwrap(), limit);
    }
    drop(paths); drop(atlas); drop(catalog);
    let partial = self::catalog(&root, &budget, 1); let atlas = build(&partial, &budget, 2);
    let paths = WorkspacePathIndex::build(&atlas, &budget, [allocation(3), allocation(4)], || false).unwrap();
    let result = paths.search(b"absent", path_options(10, 1), &budget, [allocation(5), allocation(6)], || false).unwrap();
    assert!(!result.is_complete()); assert!(!result.truncated()); assert_eq!(result.matches_seen(), 0);
}

#[test]
fn path_overlays_reject_cancellation_foreign_queries_and_wrong_or_revoked_atlases() {
    let root = root(); fs::write(root.join("needle"), b"unused").unwrap();
    let budget = budget(); let catalog = catalog(&root, &budget, 10); let atlas = build(&catalog, &budget, 2);
    let paths = WorkspacePathIndex::build(&atlas, &budget, [allocation(3), allocation(4)], || false).unwrap();
    let result = paths.search(b"needle", path_options(10, 1), &budget, [allocation(5), allocation(6)], || false).unwrap();
    assert!(matches!(paths.search(b"needle", path_options(10, 2), &budget, [allocation(7), allocation(8)], || true),
        Err(WorkspacePathSearchError::Path(PathSearchError::Canceled))));
    let mut foreign = path_options(10, 2); foreign.generation = QueryGeneration::new(ArenaOwnerId::new(999).unwrap(), 2).unwrap();
    assert!(matches!(paths.search(b"needle", foreign, &budget, [allocation(7), allocation(8)], || false),
        Err(WorkspacePathSearchError::Path(PathSearchError::OwnerMismatch))));
    let other = build(&catalog, &budget, 9);
    assert!(matches!(result.validate_delivery(&other, result.generation()),
        Err(WorkspacePathSearchError::Path(PathSearchError::StaleIndex))));
    assert_eq!(result.hits().len(), 1); // Failed replacement cannot destroy the old overlay.
    catalog.grant().revoke();
    assert!(result.validate_delivery(&atlas, result.generation()).is_err());
    assert!(paths.search(b"needle", path_options(10, 3), &budget, [allocation(7), allocation(8)], || false).is_err());
}

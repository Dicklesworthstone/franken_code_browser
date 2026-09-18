#![cfg(all(feature = "map", feature = "search"))]
#![forbid(unsafe_code)]

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteLength, FileId, Size2D};
use fcb::map::{AtlasError, AtlasNodeId, LayoutOptions, LayoutRevision};
use fcb::map::workspace::{WorkspaceAtlas, WorkspaceAtlasError, WorkspaceAtlasLimits};
use fcb::map::workspace::retained::RetainedWorkspaceAtlas;
use fcb::search::{RawPath, ResourceAllocationId, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceLimits, WorkspaceStage};
use fcb::source::CancelFlag;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-retained-map-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(path.join("src/a.rs"), b"a\n").unwrap();
        fs::write(path.join("src/b.rs"), b"bbb\n").unwrap();
        fs::write(path.join("README.md"), b"# read\n").unwrap();
        Self(path)
    }
    fn catalog(&self, budget: &ResourceBudget, max_files: usize) -> WorkspaceCatalog {
        let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&self.0));
        let limits = WorkspaceLimits { max_files, max_total_path_bytes: 4096, max_file_bytes: 0,
            max_source_bytes: 0, ..WorkspaceLimits::default() };
        let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).unwrap(),
            FileId::new(owner(), 1).unwrap(), limits, false, budget, allocation(1)).unwrap();
        while catalog.stage() == WorkspaceStage::Discovering { catalog.step(&CancelFlag::new()).unwrap(); }
        catalog
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2815).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn build(catalog: WorkspaceCatalog, budget: &ResourceBudget) -> RetainedWorkspaceAtlas {
    RetainedWorkspaceAtlas::build(catalog, LayoutRevision::new(owner(), 1).unwrap(),
        Size2D::new(4096.0, 4096.0).unwrap(), LayoutOptions::modest(), WorkspaceAtlasLimits::default(),
        budget, [allocation(2), allocation(3)], || false).unwrap()
}

#[test]
fn real_catalog_bindings_and_layout_survive_moving_the_owner() {
    let fixture = Fixture::new(); let budget = budget(); let catalog = fixture.catalog(&budget, 8);
    let expected = {
        let atlas = WorkspaceAtlas::build(&catalog, LayoutRevision::new(owner(), 1).unwrap(),
            Size2D::new(4096.0, 4096.0).unwrap(), LayoutOptions::modest(), WorkspaceAtlasLimits::default(),
            &budget, allocation(2), || false).unwrap();
        let index = atlas.index(&budget, allocation(3), || false).unwrap();
        catalog.entries().iter().map(|entry| {
            let node = index.find_path(entry.path().as_bytes()).unwrap();
            (entry.path().as_bytes().to_vec(), atlas.file(node).unwrap(), index.bounds_in(node, index.root_node()).unwrap())
        }).collect::<Vec<_>>()
    };
    let moved = Box::new(build(catalog, &budget));
    let index = moved.index().unwrap();
    for (path, file, bounds) in expected {
        let node = index.find_path(&path).unwrap();
        assert_eq!(moved.file(node).unwrap(), file);
        assert_eq!(moved.node_for_file(file).unwrap(), node);
        assert_eq!(moved.entry(node).unwrap().path().as_bytes(), path);
        assert_eq!(index.bounds_in(node, index.root_node()).unwrap(), bounds);
    }
}

#[test]
fn repeated_views_do_not_rebuild_or_recharge_the_map() {
    let fixture = Fixture::new(); let budget = budget();
    let atlas = build(fixture.catalog(&budget, 8), &budget);
    let charge = budget.accounting().reserved();
    for _ in 0..100 {
        let a = atlas.index().unwrap(); let b = atlas.index().unwrap();
        assert!(std::ptr::eq(a.layout(), b.layout()));
        assert_eq!(budget.accounting().reserved(), charge);
        assert_eq!(a.root_node(), b.root_node());
    }
    drop(atlas);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn live_changes_do_not_rewrite_frozen_membership_or_size_observations() {
    let fixture = Fixture::new(); let budget = budget();
    let atlas = build(fixture.catalog(&budget, 8), &budget);
    fs::write(fixture.0.join("src/a.rs"), b"different live source\n").unwrap();
    fs::write(fixture.0.join("new.rs"), b"new\n").unwrap();
    let index = atlas.index().unwrap();
    let node = index.find_path(b"src/a.rs").unwrap();
    assert_eq!(atlas.entry(node).unwrap().observed_bytes(), 2);
    assert_eq!(atlas.file_count(), 3);
    assert!(index.find_path(b"new.rs").is_none());
}

#[test]
fn partial_discovery_stays_partial_after_retention() {
    let fixture = Fixture::new(); let budget = budget();
    let atlas = build(fixture.catalog(&budget, 1), &budget);
    assert!(!atlas.discovery_complete());
    assert_eq!(atlas.file_count(), 1);
}

#[test]
fn stale_nodes_and_directory_file_activations_are_refused() {
    let fixture = Fixture::new(); let budget = budget();
    let atlas = build(fixture.catalog(&budget, 8), &budget);
    let index = atlas.index().unwrap();
    assert!(atlas.file(index.root_node()).is_err());
    let stale = AtlasNodeId::new(index.root(), LayoutRevision::new(owner(), 2).unwrap(), 0);
    assert!(matches!(atlas.file(stale), Err(WorkspaceAtlasError::Atlas(AtlasError::StaleLayout))));
    assert!(atlas.node_for_file(FileId::new(owner(), 999).unwrap()).is_err());
}

#[test]
fn canceled_build_releases_catalog_and_prepared_geometry() {
    let fixture = Fixture::new(); let budget = budget();
    let catalog = fixture.catalog(&budget, 8);
    let result = RetainedWorkspaceAtlas::build(catalog, LayoutRevision::new(owner(), 1).unwrap(),
        Size2D::new(4096.0, 4096.0).unwrap(), LayoutOptions::modest(), WorkspaceAtlasLimits::default(),
        &budget, [allocation(2), allocation(3)], || true);
    assert!(matches!(result, Err(WorkspaceAtlasError::Atlas(AtlasError::Canceled))));
    assert_eq!(budget.accounting().reserved().get(), 0);
}

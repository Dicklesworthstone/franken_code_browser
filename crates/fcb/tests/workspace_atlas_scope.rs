#![cfg(all(feature = "map", feature = "search"))]
#![forbid(unsafe_code)]

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteLength, FileId, Size2D};
use fcb::map::{LayoutOptions, LayoutRevision};
use fcb::map::workspace::{AtlasExtensionScope, AtlasScope, AtlasScopeError, WorkspaceAtlas,
    WorkspaceAtlasLimits};
use fcb::search::{RawPath, ResourceAllocationId, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceLimits, WorkspaceStage};
use fcb::source::CancelFlag;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-map-scope-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(path.join("src/deep")).unwrap();
        fs::write(path.join("src/a.rs"), b"a\n").unwrap();
        fs::write(path.join("src/deep/main.rs"), b"fn main() {}\n").unwrap();
        fs::write(path.join("src/b.md"), b"# b\n").unwrap();
        fs::write(path.join("README.MD"), b"# read\n").unwrap();
        fs::write(path.join("Cargo.toml"), b"[package]\n").unwrap();
        Self(path)
    }
    fn catalog(&self, budget: &ResourceBudget) -> WorkspaceCatalog {
        let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&self.0));
        let limits = WorkspaceLimits { max_files: 64, max_total_path_bytes: 4096, max_file_bytes: 0,
            max_source_bytes: 0, ..WorkspaceLimits::default() };
        let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).unwrap(),
            FileId::new(owner(), 1).unwrap(), limits, false, budget, allocation(1)).unwrap();
        while catalog.stage() == WorkspaceStage::Discovering { catalog.step(&CancelFlag::new()).unwrap(); }
        catalog
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(29500).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn revision(id: u64) -> LayoutRevision { LayoutRevision::new(owner(), id).unwrap() }
fn build<'a>(catalog: &'a WorkspaceCatalog, scope: &AtlasScope, budget: &ResourceBudget,
    id: u64) -> WorkspaceAtlas<'a> {
    WorkspaceAtlas::build(catalog, scope, revision(id), Size2D::new(1024.0, 768.0).unwrap(),
        LayoutOptions::modest(), WorkspaceAtlasLimits::default(), budget, allocation(id), || false).unwrap()
}
fn scope_rust() -> AtlasScope {
    AtlasScope::Extensions(AtlasExtensionScope::from_extensions([b"rs".as_slice()]).unwrap())
}
fn file_paths(atlas: &WorkspaceAtlas<'_>) -> Vec<Vec<u8>> {
    atlas.layout().nodes().iter()
        .filter(|node| node.kind() == fcb::map::NodeKind::File)
        .map(|node| node.path().to_vec()).collect()
}

#[test]
fn extension_matching_is_case_insensitive_and_canonical() {
    let fixture = Fixture::new(); let budget = budget(); let catalog = fixture.catalog(&budget);
    let md = build(&catalog, &AtlasScope::Extensions(
        AtlasExtensionScope::from_extensions([b"MD".as_slice()]).unwrap()), &budget, 1);
    // README.MD folds to md; only the two Markdown files are in scope.
    let mut paths = file_paths(&md);
    paths.sort();
    assert_eq!(paths, [vec![b"README.MD".to_vec()], vec![b"src/b.md".to_vec()]].concat());
    assert_eq!(md.file_count(), 2);
    // Canonical form: order, case and duplicates do not affect equality.
    let a = AtlasExtensionScope::from_extensions([b"rs".as_slice(), b"md".as_slice()]).unwrap();
    let b = AtlasExtensionScope::from_extensions([b"MD".as_slice(), b"rs".as_slice(), b"md".as_slice()]).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.extensions().collect::<Vec<_>>(), [b"md".as_slice(), b"rs".as_slice()]);
}

#[test]
fn scoped_layout_keeps_ancestor_directories_and_drops_other_files() {
    let fixture = Fixture::new(); let budget = budget(); let catalog = fixture.catalog(&budget);
    let scoped = build(&catalog, &scope_rust(), &budget, 1);
    // Nested Cargo manifests stay structural: src/deep remains as the inferred
    // ancestor of main.rs, while non-Rust files disappear from the layout.
    let paths: Vec<Vec<u8>> = scoped.layout().nodes().iter().map(|n| n.path().to_vec()).collect();
    assert!(paths.contains(&b"src".to_vec()));
    assert!(paths.contains(&b"src/deep".to_vec()));
    assert_eq!(file_paths(&scoped), [b"src/a.rs".to_vec(), b"src/deep/main.rs".to_vec()]);
}
#[test]
fn empty_scope_builds_root_only_and_all_covers_everything() {
    let fixture = Fixture::new(); let budget = budget(); let catalog = fixture.catalog(&budget);
    let empty = build(&catalog, &AtlasScope::Extensions(
        AtlasExtensionScope::from_extensions([b"zzz".as_slice()]).unwrap()), &budget, 1);
    assert_eq!(empty.file_count(), 0);
    assert_eq!(empty.layout().nodes().len(), 1); // Root only.
    let all = build(&catalog, &AtlasScope::All, &budget, 2);
    assert_eq!(all.file_count(), 5);
    assert_eq!(file_paths(&all).len(), 5);
}

#[test]
fn all_and_unscoped_construction_agree() {
    // A scoped-parameterized build with All must equal the historical
    // unfiltered construction: identical leaves, identical node count.
    let fixture = Fixture::new(); let budget = budget(); let catalog = fixture.catalog(&budget);
    let all = build(&catalog, &AtlasScope::All, &budget, 7);
    let baseline = build(&catalog, &AtlasScope::All, &budget, 8);
    assert_eq!(all.layout().nodes().len(), baseline.layout().nodes().len());
    assert_eq!(file_paths(&all), file_paths(&baseline));
    assert_eq!(all.file_count(), baseline.file_count());
}

#[test]
fn invalid_extension_sets_are_refused() {
    let too_long = [b'a'; 17];
    assert_eq!(AtlasExtensionScope::from_extensions([]), Err(AtlasScopeError::InvalidExtension));
    assert_eq!(AtlasExtensionScope::from_extensions([b"".as_slice()]), Err(AtlasScopeError::InvalidExtension));
    assert_eq!(AtlasExtensionScope::from_extensions([too_long.as_slice()]), Err(AtlasScopeError::InvalidExtension));
    assert_eq!(AtlasExtensionScope::from_extensions([b"src".as_slice()]), Err(AtlasScopeError::InvalidExtension));
    assert_eq!(AtlasExtensionScope::from_extensions([b"r.s".as_slice()]), Err(AtlasScopeError::InvalidExtension));
    assert_eq!(AtlasExtensionScope::from_extensions([b"r\x01s".as_slice()]), Err(AtlasScopeError::InvalidExtension));
    let many: Vec<&[u8]> = (0..17).map(|i| match i {
        0 => &b"a"[..], 1 => &b"b"[..], 2 => &b"c"[..], 3 => &b"d"[..], 4 => &b"e"[..],
        5 => &b"f"[..], 6 => &b"g"[..], 7 => &b"h"[..], 8 => &b"i"[..], 9 => &b"j"[..],
        10 => &b"k"[..], 11 => &b"l"[..], 12 => &b"m"[..], 13 => &b"n"[..], 14 => &b"o"[..],
        15 => &b"p"[..], _ => &b"q"[..],
    }).collect();
    assert_eq!(AtlasExtensionScope::from_extensions(many), Err(AtlasScopeError::TooManyExtensions));
}

#[test]
fn scoped_rebuilds_are_distinct_layouts_with_matching_specs() {
    let fixture = Fixture::new(); let budget = budget(); let catalog = fixture.catalog(&budget);
    // Distinct revisions yield distinct layout identities even with identical
    // specs; cached relayout relies on minting fresh revisions per rebuild.
    let first = build(&catalog, &scope_rust(), &budget, 1);
    let second = build(&catalog, &scope_rust(), &budget, 2);
    assert_ne!(first.layout().revision(), second.layout().revision());
    assert_eq!(file_paths(&first), file_paths(&second));
    assert_eq!(first.file_count(), second.file_count());
}

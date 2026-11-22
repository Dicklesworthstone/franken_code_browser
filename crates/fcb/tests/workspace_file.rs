#![forbid(unsafe_code)]
#![cfg(all(feature = "search", unix))]

//! Exact-file catalogs reach selected members beyond root and parent limits,
//! preserve capture authority, and never export whole-project completeness.

use std::{fs, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicU64, Ordering}}};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{CaptureRequest, CompleteCapture, IndexLimits, MembershipState,
    ParsedQuery, QueryGeneration, QueryOptions, RawPath, ResourceAllocationId,
    ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{DiscoveryScope, RootGrant, RuleLimits, WorkspaceCaptures,
    WorkspaceCatalog, WorkspaceLimit, WorkspaceLimits, WorkspaceStage};
use fcb::source::{CancelFlag, SourceError, path::NormalizedPath};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-workspace-file-{}-{}-{}",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, name: impl AsRef<Path>, bytes: &[u8]) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn grant(&self) -> RootGrant {
        RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&self.0))
    }
}
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(5141).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(512 * 1024 * 1024)).unwrap()
}
fn path(bytes: &[u8]) -> NormalizedPath {
    let mut names = bytes.split(|byte| *byte == b'/');
    let mut path = NormalizedPath::from_dirent_name(names.next().unwrap()).unwrap();
    for name in names { path = path.join_segment(name).unwrap(); }
    path
}
fn finish(catalog: &mut WorkspaceCatalog) {
    for _ in 0..64 {
        if catalog.stage() != WorkspaceStage::Discovering { break; }
        catalog.step(&CancelFlag::new()).unwrap();
    }
    assert_eq!(catalog.stage(), WorkspaceStage::Ready);
}
fn read(tree: &Tree, request: CaptureRequest, path: &NormalizedPath,
    allowance: usize) -> Result<CompleteCapture, SourceError> {
    let bytes = fs::read(tree.0.join(path.raw().to_path_buf()))
        .map_err(|_| SourceError::CaptureUnavailable)?;
    if bytes.len() > allowance { return Err(SourceError::PayloadTooLarge); }
    CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes))
}
fn file_catalog(tree: &Tree, file: &[u8], limits: WorkspaceLimits,
    budget: &ResourceBudget) -> WorkspaceCatalog {
    let mut catalog = WorkspaceCatalog::open_for_file(tree.grant(), path(file),
        SearchManifestId::new(owner(), 1).unwrap(), FileId::new(owner(), 10).unwrap(),
        limits, true, budget, allocation(1)).unwrap();
    finish(&mut catalog);
    catalog
}
fn fill<'a>(tree: &Tree, catalog: &'a WorkspaceCatalog, budget: &ResourceBudget)
    -> WorkspaceCaptures<'a> {
    let mut captures = WorkspaceCaptures::new(catalog, SourceRevision::new(owner(), 20).unwrap(),
        budget, allocation(2)).unwrap();
    while !captures.finished() {
        captures.step(&CancelFlag::new(), |request, path, allowance| {
            assert_eq!(Some(path), catalog.file(), "a file catalog cannot capture a sibling");
            read(tree, request, path, allowance)
        }).unwrap();
    }
    captures
}

#[test]
fn file_omitted_from_both_root_and_parent_catalogs_gets_an_independent_closed_scope() {
    let tree = Tree::new();
    let candidates = ["selected/a.rs", "selected/b.rs", "selected/c.rs"];
    for name in candidates { tree.file(name, b"\xff\x00metadata only"); }
    let grant = tree.grant();
    let budget = budget();
    let limits = WorkspaceLimits { max_files: 1, max_file_bytes: 0, max_source_bytes: 0,
        ..Default::default() };
    let mut root = WorkspaceCatalog::open_rule_aware(grant.clone(),
        SearchManifestId::new(owner(), 1).unwrap(), FileId::new(owner(), 1).unwrap(),
        limits, RuleLimits::default(), &budget, [allocation(1), allocation(2)]).unwrap();
    finish(&mut root);
    let mut parent = WorkspaceCatalog::open_rule_aware_in_directory(grant.clone(), path(b"selected"),
        SearchManifestId::new(owner(), 2).unwrap(), FileId::new(owner(), 100).unwrap(),
        limits, RuleLimits::default(), &budget, [allocation(3), allocation(4)]).unwrap();
    finish(&mut parent);
    assert!(!root.discovery_complete());
    assert!(!parent.discovery_complete());
    assert!(root.is_workspace());
    assert!(!parent.is_workspace());
    assert_eq!(root.entries().len(), 1);
    assert_eq!(parent.entries().len(), 1);
    // Enumeration order is unspecified. Three candidates guarantee at least
    // one is absent from the union of these two one-file catalogs.
    let selected = candidates.into_iter().find(|candidate| {
        root.entries().iter().chain(parent.entries()).all(|entry| {
            entry.path().as_bytes() != candidate.as_bytes()
        })
    }).expect("root and parent caps cannot together contain three distinct files");
    let mut exact = WorkspaceCatalog::open_rule_aware_for_file(grant.clone(), path(selected.as_bytes()),
        SearchManifestId::new(owner(), 3).unwrap(), FileId::new(owner(), 1000).unwrap(),
        limits, RuleLimits::default(), &budget, [allocation(5), allocation(6)]).unwrap();
    finish(&mut exact);
    assert!(exact.discovery_complete());
    assert!(matches!(exact.discovery_scope(), DiscoveryScope::File(_)));
    assert!(!exact.is_workspace());
    assert_eq!(exact.directory(), None);
    assert_eq!(exact.file().unwrap().as_bytes(), selected.as_bytes());
    assert_eq!(exact.entries().len(), 1);
    assert_eq!(exact.entries()[0].path().as_bytes(), selected.as_bytes());
    assert_eq!(exact.file_id(0).unwrap().get(), 1000);
    assert_eq!(exact.grant().root_path(), grant.root_path());
    assert_eq!(exact.grant().root_id(), grant.root_id());
    assert!(Arc::ptr_eq(exact.grant().token(), grant.token()));
    assert_eq!(exact.limits().max_source_bytes, 0);
    grant.revoke();
    assert!(exact.validate_active().is_err());
    assert!(root.validate_active().is_err());
    assert!(parent.validate_active().is_err());
}

#[test]
fn exact_member_text_index_uses_original_utf16_capture_after_live_replacement() {
    let tree = Tree::new();
    let mut original = vec![0xff, 0xfe];
    for unit in "banana".encode_utf16() { original.extend_from_slice(&unit.to_le_bytes()); }
    tree.file("selected/kept.rs", &original);
    tree.file("selected/sibling.rs", b"banana outside selected scope");
    let budget = budget();
    let catalog = file_catalog(&tree, b"selected/kept.rs", WorkspaceLimits {
        max_files: 1, ..Default::default()
    }, &budget);
    let captures = fill(&tree, &catalog, &budget);
    tree.file("selected/kept.rs", b"new live contents");
    let inputs = captures.search_inputs(&budget, allocation(3)).unwrap();
    assert_eq!(inputs.manifest().unwrap().membership(), MembershipState::Closed);
    let index = inputs.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let result = index.search(&ParsedQuery::parse("ana").unwrap(),
        QueryOptions::new(QueryGeneration::new(owner(), 1).unwrap()),
        &budget, allocation(5), || false).unwrap();
    assert!(result.is_complete());
    assert_eq!(result.capture_results().matches.len(), 2);
    for hit in &result.capture_results().matches {
        assert_eq!(hit.file_id, catalog.file_id(0).unwrap());
        let capture = captures.capture(hit.file_id).unwrap();
        assert_eq!(capture.request().revision(), hit.revision);
        assert_eq!(capture.bytes(), original.as_slice());
        let (start, end) = hit.original_byte_range.as_usize_bounds().unwrap();
        assert_eq!(&capture.bytes()[start..end], &[b'a', 0, b'n', 0, b'a', 0]);
    }
}

#[test]
fn admitted_file_lost_before_observation_never_closes_empty_membership() {
    let tree = Tree::new(); tree.file("kept.rs", b"needle");
    let budget = budget();
    let mut catalog = WorkspaceCatalog::open_for_file(tree.grant(), path(b"kept.rs"),
        SearchManifestId::new(owner(), 1).unwrap(), FileId::new(owner(), 10).unwrap(),
        WorkspaceLimits::default(), true, &budget, allocation(1)).unwrap();
    fs::rename(tree.0.join("kept.rs"), tree.0.join("moved.rs")).unwrap();
    finish(&mut catalog);
    assert!(!catalog.discovery_complete());
    assert!(catalog.entries().is_empty());
    assert_eq!(catalog.file().unwrap().as_bytes(), b"kept.rs");
    assert_eq!(catalog.aggregate().unavailable, 1);
    let captures = fill(&tree, &catalog, &budget);
    let inputs = captures.search_inputs(&budget, allocation(3)).unwrap();
    assert_eq!(inputs.manifest().unwrap().membership(), MembershipState::Discovering);
    let index = inputs.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let result = index.search(&ParsedQuery::parse("needle").unwrap(),
        QueryOptions::new(QueryGeneration::new(owner(), 1).unwrap()),
        &budget, allocation(5), || false).unwrap();
    assert!(!result.is_complete());
    assert!(result.capture_results().matches.is_empty());
}

#[test]
fn file_scope_does_not_bypass_path_admission() {
    let tree = Tree::new(); tree.file("selected/kept.rs", b"needle");
    let budget = budget();
    let catalog = file_catalog(&tree, b"selected/kept.rs", WorkspaceLimits {
        max_files: 1, max_total_path_bytes: 0, ..Default::default()
    }, &budget);
    assert!(!catalog.discovery_complete());
    assert_eq!(catalog.stopped_by_limit(), Some(WorkspaceLimit::Paths));
    assert!(catalog.entries().is_empty());
    assert!(catalog.file().is_some());
    let captures = fill(&tree, &catalog, &budget);
    let inputs = captures.search_inputs(&budget, allocation(3)).unwrap();
    assert_eq!(inputs.manifest().unwrap().membership(), MembershipState::Discovering);
}

#[test]
fn known_member_exclusion_is_counted_independently_of_coverage() {
    let tree = Tree::new();
    tree.file(".gitignore", b"kept.rs\n");
    tree.file("kept.rs", b"needle");
    let budget = budget();
    let mut catalog = WorkspaceCatalog::open_rule_aware_for_file(tree.grant(), path(b"kept.rs"),
        SearchManifestId::new(owner(), 1).unwrap(), FileId::new(owner(), 1).unwrap(),
        WorkspaceLimits::default(), RuleLimits::default(), &budget,
        [allocation(1), allocation(2)]).unwrap();
    finish(&mut catalog);
    assert!(catalog.discovery_complete());
    assert!(catalog.entries().is_empty());
    assert_eq!(catalog.aggregate().files, 1);
    assert_eq!(catalog.aggregate().excluded, 1);
    assert_eq!(catalog.aggregate().unavailable, 0);
    assert!(!catalog.is_workspace());
    assert_eq!(catalog.file().unwrap().as_bytes(), b"kept.rs");
}

#[cfg(feature = "snapshot")]
#[test]
fn file_snapshot_keeps_exact_bytes_without_whole_root_absence_authority() {
    use std::os::unix::ffi::OsStrExt;
    use fcb::search::snapshot::{export_workspace, SnapshotData, SnapshotLimits, SnapshotView};
    let tree = Tree::new();
    let raw = b"selected/kept\\\xff.rs";
    let native = Path::new(std::ffi::OsStr::from_bytes(raw));
    let original = b"\xef\xbb\xbfretained\r\n";
    tree.file(native, original);
    tree.file("outside.rs", b"must not become a missing whole-root member");
    let budget = budget();
    let catalog = file_catalog(&tree, raw, WorkspaceLimits::default(), &budget);
    assert!(catalog.discovery_complete(), "the selected member itself is complete");
    let captures = fill(&tree, &catalog, &budget);
    tree.file(native, b"changed after capture");
    let exported = export_workspace(&captures, SnapshotLimits::default(), &budget,
        [allocation(3), allocation(4)], || false).unwrap();
    let view = SnapshotView::open(exported.bytes(), SnapshotLimits::default(), || false).unwrap();
    assert!(!view.discovery_complete(), "a file catalog cannot establish sibling absence");
    assert_eq!(view.len(), 1);
    let saved = view.entries().next().unwrap().unwrap();
    assert_eq!(saved.path, raw);
    assert_eq!(saved.observed_bytes, original.len() as u64);
    assert_eq!(saved.data, SnapshotData::Captured(original));
}

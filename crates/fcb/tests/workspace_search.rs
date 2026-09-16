#![forbid(unsafe_code)]
#![cfg(all(feature = "search", unix))]

use std::{fs, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicU64, Ordering}}};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::source::{CancelFlag, SourceError};
use fcb::search::{CaptureRequest, CompleteCapture, IndexLimits, ParsedQuery, QueryGeneration,
    QueryOptions, RawPath, ResourceAllocationId, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCaptureFailure, WorkspaceCaptures,
    WorkspaceCatalog, WorkspaceError, WorkspaceLimit, WorkspaceLimits, WorkspaceStage};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fcb-workspace-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, path: impl AsRef<Path>, bytes: &[u8]) {
        let path = self.0.join(path); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
}
impl Drop for Tree { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1419).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn catalog(tree: &Tree, limits: WorkspaceLimits, include: bool, budget: &ResourceBudget) -> WorkspaceCatalog {
    let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&tree.0));
    let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).unwrap(),
        FileId::new(owner(), 10).unwrap(), limits, include, budget, allocation(1)).unwrap();
    for _ in 0..=limits.max_discovery_pages + 1 {
        if catalog.stage() != WorkspaceStage::Discovering { break; }
        catalog.step(&CancelFlag::new()).unwrap();
    }
    assert_eq!(catalog.stage(), WorkspaceStage::Ready);
    catalog
}
fn read(tree: &Tree, request: CaptureRequest, path: &fcb::source::path::NormalizedPath,
    limit: usize) -> Result<CompleteCapture, SourceError> {
    let bytes = fs::read(tree.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable)?;
    if bytes.len() > limit { return Err(SourceError::PayloadTooLarge); }
    CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes))
}
fn fill<'a>(tree: &Tree, catalog: &'a WorkspaceCatalog, budget: &ResourceBudget) -> WorkspaceCaptures<'a> {
    let mut captures = WorkspaceCaptures::new(catalog, SourceRevision::new(owner(), 20).unwrap(), budget, allocation(2)).unwrap();
    while !captures.finished() { captures.step(&CancelFlag::new(), |r, p, n| read(tree, r, p, n)).unwrap(); }
    captures
}

#[test]
fn complete_discovery_is_sorted_and_default_exclusions_are_not_deletions() {
    let tree = Tree::new(); tree.file("z.rs", b"z"); tree.file("src/a.rs", b"a");
    tree.file("target/secret.rs", b"excluded"); tree.file(".git/objects/blob", b"excluded");
    tree.file(".gitignore", b"*.rs\n");
    let budget = budget(); let catalog = catalog(&tree, WorkspaceLimits::default(), false, &budget);
    assert!(catalog.discovery_complete());
    assert_eq!(catalog.entries().iter().map(|e| e.path().as_bytes()).collect::<Vec<_>>(),
        [b".gitignore".as_slice(), b"src/a.rs", b"z.rs"]);
    assert!(catalog.policy_name().contains("no-rule-files"));
    assert_eq!(catalog.aggregate().excluded, 2);
    assert_eq!(catalog.file_id(0).unwrap().get(), 10);
    assert_eq!(catalog.file_id(2).unwrap().get(), 12);
}

#[test]
fn workspace_index_matches_exact_utf8_and_utf16_captures_after_live_replacement() {
    let tree = Tree::new(); tree.file("a.rs", b"banana"); tree.file("b.rs", b"unrelated");
    let mut utf16 = vec![0xff, 0xfe];
    for unit in "banana".encode_utf16() { utf16.extend_from_slice(&unit.to_le_bytes()); }
    tree.file("nested/c.rs", &utf16);
    let budget = budget(); let catalog = catalog(&tree, WorkspaceLimits::default(), false, &budget);
    let captures = fill(&tree, &catalog, &budget);
    tree.file("a.rs", b"changed live source");
    let inputs = captures.search_inputs(&budget, allocation(3)).unwrap();
    let index = inputs.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let query = ParsedQuery::parse("ana").unwrap();
    let result = index.search(&query, QueryOptions::new(generation()), &budget, allocation(5), || false).unwrap();
    assert!(result.is_complete()); assert_eq!(result.capture_results().matches.len(), 4);
    assert_eq!(result.skipped_by_index(), 1);
    for hit in &result.capture_results().matches {
        let captured = captures.capture(hit.file_id).unwrap();
        assert_eq!(captured.request().revision(), hit.revision);
        let (start, end) = hit.original_byte_range.as_usize_bounds().unwrap();
        let expected = if catalog.entry(hit.file_id).unwrap().path().as_bytes() == b"a.rs" {
            b"ana".as_slice()
        } else { &[b'a', 0, b'n', 0, b'a', 0] };
        assert_eq!(&captured.bytes()[start..end], expected);
    }
}

#[test]
fn a_quota_refused_file_remains_unavailable_not_a_false_complete_negative() {
    let tree = Tree::new(); tree.file("a.rs", b"needle"); tree.file("b.rs", &vec![b'x'; 100]);
    let budget = budget();
    let limits = WorkspaceLimits { max_file_bytes: 10, ..Default::default() };
    let catalog = catalog(&tree, limits, false, &budget);
    let captures = fill(&tree, &catalog, &budget);
    assert_eq!(captures.failure(1), Some(WorkspaceCaptureFailure::FileLimit));
    assert_eq!(captures.captured_bytes(), 6);
    let inputs = captures.search_inputs(&budget, allocation(3)).unwrap();
    let index = inputs.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let result = index.search(&ParsedQuery::parse("absent").unwrap(), QueryOptions::new(generation()),
        &budget, allocation(5), || false).unwrap();
    assert!(result.capture_results().matches.is_empty()); assert!(!result.is_complete());
    assert_eq!(result.unavailable_files(), [catalog.file_id(1).unwrap()]);
}

#[test]
fn total_capture_budget_is_shared_and_callback_never_receives_unadmitted_large_files() {
    let tree = Tree::new(); for name in ["a", "b", "c"] { tree.file(name, b"abcd"); }
    let budget = budget();
    let catalog = catalog(&tree, WorkspaceLimits { max_source_bytes: 7, ..Default::default() }, false, &budget);
    let mut captures = WorkspaceCaptures::new(&catalog, SourceRevision::new(owner(), 1).unwrap(), &budget, allocation(2)).unwrap();
    let mut calls = 0;
    while !captures.finished() {
        captures.step(&CancelFlag::new(), |r, p, n| { calls += 1; read(&tree, r, p, n) }).unwrap();
    }
    assert_eq!(calls, 1); assert_eq!(captures.captured_bytes(), 4);
    assert_eq!(captures.failure(1), Some(WorkspaceCaptureFailure::SourceLimit));
    assert_eq!(captures.failure(2), Some(WorkspaceCaptureFailure::SourceLimit));
}

#[test]
fn failed_and_partial_captures_keep_eligible_membership_visible() {
    let tree = Tree::new(); tree.file("a", b"needle"); tree.file("b", b"needle");
    let budget = budget(); let catalog = catalog(&tree, WorkspaceLimits::default(), false, &budget);
    let mut captures = WorkspaceCaptures::new(&catalog, SourceRevision::new(owner(), 1).unwrap(), &budget, allocation(2)).unwrap();
    captures.step(&CancelFlag::new(), |r, p, n| read(&tree, r, p, n)).unwrap();
    {
        let inputs = captures.search_inputs(&budget, allocation(3)).unwrap();
        let manifest = inputs.manifest().unwrap();
        assert_eq!(manifest.membership(), fcb::search::MembershipState::Discovering);
        assert_eq!(manifest.documents().len(), 1); assert_eq!(manifest.unavailable().len(), 1);
    }
    captures.step(&CancelFlag::new(), |_, _, _| Err(SourceError::CaptureUnavailable)).unwrap();
    assert!(captures.finished()); assert!(captures.failure(1).is_some());
    let inputs = captures.search_inputs(&budget, allocation(3)).unwrap();
    assert_eq!(inputs.manifest().unwrap().unavailable().len(), 1);
}

#[test]
fn discovery_limits_cannot_promote_a_partial_catalog_to_closed_membership() {
    let tree = Tree::new(); for i in 0..200 { tree.file(format!("file-{i:03}"), b"x"); }
    for (limits, reason) in [
        (WorkspaceLimits { max_files: 1, ..Default::default() }, WorkspaceLimit::Files),
        (WorkspaceLimits { max_total_path_bytes: 0, ..Default::default() }, WorkspaceLimit::Paths),
        (WorkspaceLimits { max_discovery_pages: 1, ..Default::default() }, WorkspaceLimit::DiscoveryPages),
    ] {
        let budget = budget(); let catalog = catalog(&tree, limits, false, &budget);
        assert!(!catalog.discovery_complete()); assert_eq!(catalog.stopped_by_limit(), Some(reason));
        let captures = fill(&tree, &catalog, &budget);
        let inputs = captures.search_inputs(&budget, allocation(3)).unwrap();
        assert_eq!(inputs.manifest().unwrap().membership(), fcb::search::MembershipState::Discovering);
    }
}

#[test]
fn empty_workspace_and_exact_file_limit_can_be_complete() {
    let tree = Tree::new(); let budget = budget();
    {
        let catalog = catalog(&tree, WorkspaceLimits { max_files: 1, ..Default::default() }, false, &budget);
        assert!(catalog.discovery_complete()); assert!(catalog.entries().is_empty());
        let captures = fill(&tree, &catalog, &budget);
        let inputs = captures.search_inputs(&budget, allocation(3)).unwrap();
        let index = inputs.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
        assert!(index.search(&ParsedQuery::parse("missing").unwrap(), QueryOptions::new(generation()),
            &budget, allocation(5), || false).unwrap().is_complete());
    }
    tree.file("only", b"source");
    let catalog = catalog(&tree, WorkspaceLimits { max_files: 1, ..Default::default() }, false, &budget);
    assert!(catalog.discovery_complete()); assert_eq!(catalog.entries().len(), 1);
}

#[test]
fn raw_names_and_symlinks_do_not_acquire_other_source_authority() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let tree = Tree::new(); tree.file(Path::new(std::ffi::OsStr::from_bytes(b"bad-\xff.rs")), b"needle");
    tree.file("Case.rs", b"upper"); tree.file("case.rs", b"lower");
    let outside = Tree::new(); outside.file("private.rs", b"never-read");
    symlink(outside.0.join("private.rs"), tree.0.join("link.rs")).unwrap();
    symlink(&tree.0, tree.0.join("loop")).unwrap();
    let budget = budget(); let catalog = catalog(&tree, WorkspaceLimits::default(), false, &budget);
    assert_eq!(catalog.aggregate().symlinks, 2); assert!(catalog.discovery_complete());
    assert_eq!(catalog.entries().len(), 3);
    assert!(catalog.entries().iter().any(|e| e.path().as_bytes() == b"bad-\xff.rs"));
}

#[test]
fn revocation_refuses_new_captures_and_search_publication_without_destroying_old_bytes() {
    let tree = Tree::new(); tree.file("a", b"original");
    let budget = budget(); let catalog = catalog(&tree, WorkspaceLimits::default(), false, &budget);
    let captures = fill(&tree, &catalog, &budget);
    catalog.grant().revoke();
    assert!(captures.search_inputs(&budget, allocation(3)).is_err());
    assert_eq!(captures.capture(catalog.file_id(0).unwrap()).unwrap().bytes(), b"original");
}

#[test]
fn denied_identity_exhausted_and_canceled_work_does_not_leak_reservations() {
    let tree = Tree::new(); tree.file("a", b"x");
    let budget = budget();
    let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&tree.0));
    let id = SearchManifestId::new(owner(), 1).unwrap();
    assert!(matches!(WorkspaceCatalog::open(grant.clone(), id, FileId::new(owner(), u64::MAX).unwrap(),
        WorkspaceLimits::default(), false, &budget, allocation(1)), Err(WorkspaceError::IdentityExhausted)));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(WorkspaceCatalog::open(grant.clone(), id, FileId::new(owner(), 1).unwrap(),
        WorkspaceLimits::default(), false, &tiny, allocation(1)), Err(WorkspaceError::ResourceDenied)));
    let mut catalog = WorkspaceCatalog::open(grant, id, FileId::new(owner(), 1).unwrap(),
        WorkspaceLimits::default(), false, &budget, allocation(1)).unwrap();
    catalog.cancel(); assert_eq!(catalog.step(&CancelFlag::new()).unwrap(), WorkspaceStage::Canceled);
    drop(catalog); assert_eq!(budget.accounting().reserved().get(), 0);
}

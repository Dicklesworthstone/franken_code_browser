#![forbid(unsafe_code)]
#![cfg(all(feature = "search", unix))]

use std::{fs, path::PathBuf, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::source::{CancelFlag, SourceError};
use fcb::search::{CaptureRequest, CompleteCapture, RawPath, ResourceAllocationId,
    ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCaptureFailure, WorkspaceCaptures,
    WorkspaceCatalog, WorkspaceError, WorkspaceLimits, WorkspaceStage};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fcb-publish-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&path).unwrap(); fs::write(path.join("file"), b"data").unwrap(); Self(path)
    }
}
impl Drop for Tree { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1519).unwrap() }
fn alloc(value: u64) -> ResourceAllocationId { ResourceAllocationId::new(value).unwrap() }
fn file() -> FileId { FileId::new(owner(), 1).unwrap() }
fn revision() -> SourceRevision { SourceRevision::new(owner(), 1).unwrap() }
fn prepare(tree: &Tree, budget: &ResourceBudget) -> WorkspaceCatalog {
    let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&tree.0));
    let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).unwrap(), file(),
        WorkspaceLimits { max_files: 4, max_source_bytes: 16, ..Default::default() }, false, budget, alloc(1)).unwrap();
    // No file ordinal is assigned before the final raw-path ordering exists.
    assert!(catalog.file_id(0).is_none()); assert!(catalog.entry(file()).is_none());
    while catalog.stage() == WorkspaceStage::Discovering { catalog.step(&CancelFlag::new()).unwrap(); }
    assert!(catalog.discovery_complete()); catalog
}
fn captured(request: CaptureRequest) -> CompleteCapture {
    CompleteCapture::new(request, ByteLength::new(4), Arc::from(b"data".as_slice())).unwrap()
}

#[test]
fn callback_cancellation_and_revocation_do_not_publish_or_count_the_candidate() {
    let tree = Tree::new();
    for action in ["success", "cancel", "revoke"] {
        let budget = ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap();
        let catalog = prepare(&tree, &budget);
        let mut captures = WorkspaceCaptures::new(&catalog, revision(), &budget, alloc(2)).unwrap();
        let cancel = CancelFlag::new();
        let result = captures.step(&cancel, |request, _, _| {
            let candidate = captured(request);
            match action { "cancel" => cancel.cancel(), "revoke" => { catalog.grant().revoke(); }, _ => {} }
            Ok(candidate)
        });
        match action {
            "success" => {
                assert_eq!(result, Ok(true)); assert_eq!(captures.captured_bytes(), 4);
                assert_eq!(captures.capture(file()).unwrap().bytes(), b"data");
            }
            _ => {
                assert_eq!(result, Err(if action == "cancel" { WorkspaceError::Canceled }
                    else { WorkspaceError::Source(SourceError::GrantRevoked) }));
                assert_eq!(captures.captured_bytes(), 0); assert_eq!(captures.examined_files(), 0);
                assert!(captures.capture(file()).is_none());
                assert!(captures.search_inputs(&budget, alloc(3)).is_err());
            }
        }
        drop(captures); drop(catalog); assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn a_host_capture_with_the_wrong_revision_becomes_an_explicit_unavailable_file() {
    let tree = Tree::new();
    let budget = ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap();
    let catalog = prepare(&tree, &budget);
    let mut captures = WorkspaceCaptures::new(&catalog, revision(), &budget, alloc(2)).unwrap();
    captures.step(&CancelFlag::new(), |_, _, _| {
        Ok(captured(CaptureRequest::new(file(), SourceRevision::new(owner(), 99).unwrap()).unwrap()))
    }).unwrap();
    assert!(captures.finished()); assert_eq!(captures.captured_bytes(), 0);
    assert_eq!(captures.file_failure(file()), Some(WorkspaceCaptureFailure::InvalidCapture));
    let inputs = captures.search_inputs(&budget, alloc(3)).unwrap();
    let manifest = inputs.manifest().unwrap();
    assert!(manifest.documents().is_empty()); assert_eq!(manifest.unavailable(), [file()]);
}

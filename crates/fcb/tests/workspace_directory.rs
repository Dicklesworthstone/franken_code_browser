#![forbid(unsafe_code)]
#![cfg(all(feature = "search", unix))]

//! Public facade path: a bounded subtree remains under the caller's original
//! grant and keeps source paths relative to that root, independently of a
//! whole-project catalog which exhausted its file cap.

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteLength, FileId};
use fcb::search::{RawPath, ResourceAllocationId, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, RuleLimits, WorkspaceCatalog, WorkspaceLimits, WorkspaceStage};
use fcb::source::{CancelFlag, path::NormalizedPath};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(5139).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!("fcb-workspace-directory-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn finish(catalog: &mut WorkspaceCatalog) {
    for _ in 0..64 {
        if catalog.stage() != WorkspaceStage::Discovering { break; }
        catalog.step(&CancelFlag::new()).unwrap();
    }
    assert_eq!(catalog.stage(), WorkspaceStage::Ready);
}

#[test]
fn focused_membership_has_a_separate_budget_and_retains_original_root_authority() {
    let root = root();
    let candidates = ["first-focused", "second-focused"];
    for directory in candidates {
        fs::create_dir(root.join(directory)).unwrap();
        fs::write(root.join(directory).join("kept.rs"), b"\xff\x00binary").unwrap();
    }
    let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&root));
    let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
    let limits = WorkspaceLimits { max_files: 1, max_file_bytes: 0, max_source_bytes: 0,
        ..Default::default() };
    let mut project = WorkspaceCatalog::open_rule_aware(grant.clone(),
        SearchManifestId::new(owner(), 1).unwrap(), FileId::new(owner(), 1).unwrap(),
        limits, RuleLimits::default(), &budget, [allocation(1), allocation(2)]).unwrap();
    finish(&mut project);
    assert!(!project.discovery_complete());
    assert!(project.directory().is_none());
    assert_eq!(project.entries().len(), 1);
    // Native enumeration order is unspecified. Select a folder whose file is
    // actually absent from the accepted capped catalog, whichever arrived first.
    let selected = candidates.into_iter().find(|directory| {
        let path = format!("{directory}/kept.rs");
        project.entries().iter().all(|entry| entry.path().as_bytes() != path.as_bytes())
    }).expect("a one-file catalog must omit one of the two candidate folders");
    let selected_path = format!("{selected}/kept.rs");

    let directory = NormalizedPath::from_dirent_name(selected.as_bytes()).unwrap();
    let mut focused = WorkspaceCatalog::open_rule_aware_in_directory(grant.clone(), directory,
        SearchManifestId::new(owner(), 2).unwrap(), FileId::new(owner(), 100).unwrap(),
        limits, RuleLimits::default(), &budget, [allocation(3), allocation(4)]).unwrap();
    finish(&mut focused);
    assert!(focused.discovery_complete());
    assert_eq!(focused.directory().unwrap().as_bytes(), selected.as_bytes());
    assert_eq!(focused.grant().root_path(), grant.root_path());
    assert_eq!(focused.grant().root_id(), grant.root_id());
    assert!(std::sync::Arc::ptr_eq(focused.grant().token(), grant.token()));
    assert_eq!(focused.entries().len(), 1);
    assert_eq!(focused.entries()[0].path().as_bytes(), selected_path.as_bytes());
    assert_eq!(focused.file_id(0).unwrap().get(), 100);
    assert_eq!(focused.limits().max_files, 1);
    assert_eq!(focused.limits().max_source_bytes, 0);
    grant.revoke();
    assert!(focused.validate_active().is_err());
    assert!(project.validate_active().is_err());
}

#[cfg(feature = "snapshot")]
#[test]
fn scoped_snapshot_preserves_captured_bytes_without_claiming_complete_root_membership() {
    use std::sync::Arc;
    use fcb::SourceRevision;
    use fcb::search::CompleteCapture;
    use fcb::search::workspace::WorkspaceCaptures;
    use fcb::search::snapshot::{export_workspace, SnapshotData, SnapshotLimits, SnapshotView};
    use fcb::source::SourceError;

    let root = root();
    fs::write(root.join("outside.rs"), b"must not become a deleted member").unwrap();
    fs::create_dir(root.join("selected")).unwrap();
    let original = b"\xef\xbb\xbfretained\r\n";
    fs::write(root.join("selected/kept.rs"), original).unwrap();
    let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&root));
    let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
    let limits = WorkspaceLimits { max_files: 4, max_file_bytes: 1024, max_source_bytes: 1024,
        ..Default::default() };
    let mut catalog = WorkspaceCatalog::open_in_directory(grant,
        NormalizedPath::from_dirent_name(b"selected").unwrap(),
        SearchManifestId::new(owner(), 1).unwrap(), FileId::new(owner(), 1).unwrap(),
        limits, true, &budget, allocation(1)).unwrap();
    finish(&mut catalog);
    assert!(catalog.discovery_complete(), "the selected directory itself is complete");
    let mut captures = WorkspaceCaptures::new(&catalog, SourceRevision::new(owner(), 1).unwrap(),
        &budget, allocation(2)).unwrap();
    while !captures.finished() {
        captures.step(&CancelFlag::new(), |request, path, allowance| {
            assert_eq!(path.as_bytes(), b"selected/kept.rs");
            let bytes = fs::read(root.join(path.raw().to_path_buf()))
                .map_err(|_| SourceError::CaptureUnavailable)?;
            if bytes.len() > allowance { return Err(SourceError::PayloadTooLarge); }
            CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes))
        }).unwrap();
    }
    fs::write(root.join("selected/kept.rs"), b"changed after capture").unwrap();
    let exported = export_workspace(&captures, SnapshotLimits::default(), &budget,
        [allocation(3), allocation(4)], || false).unwrap();
    let view = SnapshotView::open(exported.bytes(), SnapshotLimits::default(), || false).unwrap();
    assert!(!view.discovery_complete(), "the archive has no whole-project membership evidence");
    assert_eq!(view.len(), 1);
    let saved = view.entries().next().unwrap().unwrap();
    assert_eq!(saved.path, b"selected/kept.rs");
    assert_eq!(saved.observed_bytes, original.len() as u64);
    assert_eq!(saved.data, SnapshotData::Captured(original));
}

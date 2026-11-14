#![forbid(unsafe_code)]
#![cfg(all(feature = "search", unix))]

//! Capture admission includes its working overlap. Once every host callback has
//! returned, unused quota and temporary copies must stop competing with the
//! exact captured bytes needed by a subsequent export or index build.

use std::{fs, path::PathBuf, sync::{Arc, atomic::{AtomicU64, Ordering}}};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::source::{CancelFlag, SourceError};
use fcb::search::{CompleteCapture, RawPath, ResourceAllocationId, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCaptureFailure, WorkspaceCaptures,
    WorkspaceCatalog, WorkspaceError, WorkspaceLimits, WorkspaceStage};

const SOURCE_QUOTA: usize = 64 * 1024 * 1024;
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1781).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn fixture(files: usize) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-capture-budget-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    for index in 0..files { fs::write(root.join(format!("file-{index}.txt")), b"data").unwrap(); }
    root // Retain fixtures for the independent qualification runner.
}
fn catalog(files: usize, budget: &ResourceBudget) -> WorkspaceCatalog {
    let root = fixture(files);
    let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&root));
    let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).unwrap(),
        FileId::new(owner(), 1).unwrap(), WorkspaceLimits { max_files: 8, max_source_bytes: SOURCE_QUOTA,
            ..WorkspaceLimits::default() }, false, budget, allocation(1)).unwrap();
    while catalog.stage() == WorkspaceStage::Discovering { catalog.step(&CancelFlag::new()).unwrap(); }
    assert!(catalog.discovery_complete());
    catalog
}

#[test]
fn pending_callbacks_keep_the_working_charge_and_completion_keeps_only_retained_capacity() {
    let budget = budget();
    let catalog = catalog(2, &budget);
    let metadata = budget.accounting().reserved().get();
    let mut captures = WorkspaceCaptures::new(&catalog, SourceRevision::new(owner(), 1).unwrap(),
        &budget, allocation(2)).unwrap();
    let working = budget.accounting().reserved().get();
    assert!(working - metadata >= 2 * SOURCE_QUOTA as u64);
    for step in 0..2 {
        let finished = captures.step(&CancelFlag::new(), |request, _, allowance| {
            assert!(allowance >= 4);
            assert_eq!(budget.accounting().reserved().get(), working,
                "a callback still owns its source handoff overlap");
            CompleteCapture::new(request, ByteLength::new(4), Arc::from(b"data".as_slice()))
        }).unwrap();
        assert_eq!(finished, step == 1);
        if !finished { assert_eq!(budget.accounting().reserved().get(), working); }
    }
    assert_eq!(captures.captured_bytes(), 8);
    let retained = budget.accounting().reserved().get() - metadata;
    assert!(retained >= 8);
    assert!(retained < 4096, "unused 64 MiB source quota and temporary copies must be returned");
    for ordinal in 0..2 {
        assert_eq!(captures.capture(catalog.file_id(ordinal).unwrap()).unwrap().bytes(), b"data");
    }
    drop(captures);
    assert_eq!(budget.accounting().reserved().get(), metadata);
    drop(catalog);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn empty_and_unavailable_finished_workspaces_return_unused_source_quota() {
    for files in [0, 1] {
        let budget = budget();
        let catalog = catalog(files, &budget);
        let metadata = budget.accounting().reserved().get();
        let mut captures = WorkspaceCaptures::new(&catalog, SourceRevision::new(owner(), 1).unwrap(),
            &budget, allocation(2)).unwrap();
        if files != 0 {
            assert!(captures.step(&CancelFlag::new(), |_, _, _| Err(SourceError::CaptureUnavailable)).unwrap());
            assert_eq!(captures.file_failure(catalog.file_id(0).unwrap()),
                Some(WorkspaceCaptureFailure::Source(SourceError::CaptureUnavailable)));
        }
        assert!(captures.finished()); assert_eq!(captures.captured_bytes(), 0);
        assert!(budget.accounting().reserved().get() - metadata < 4096);
        drop(captures); drop(catalog);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn cancellation_never_reconciles_an_unfinished_capture_as_complete() {
    let budget = budget();
    let catalog = catalog(1, &budget);
    let metadata = budget.accounting().reserved().get();
    let mut captures = WorkspaceCaptures::new(&catalog, SourceRevision::new(owner(), 1).unwrap(),
        &budget, allocation(2)).unwrap();
    let working = budget.accounting().reserved().get();
    let cancel = CancelFlag::new();
    assert_eq!(captures.step(&cancel, |request, _, _| {
        cancel.cancel();
        CompleteCapture::new(request, ByteLength::new(4), Arc::from(b"data".as_slice()))
    }), Err(WorkspaceError::Canceled));
    assert!(!captures.finished()); assert_eq!(captures.captured_bytes(), 0);
    assert_eq!(budget.accounting().reserved().get(), working);
    assert!(captures.capture(catalog.file_id(0).unwrap()).is_none());
    drop(captures);
    assert_eq!(budget.accounting().reserved().get(), metadata);
}

#[test]
fn capture_still_requires_its_full_working_admission_before_the_first_callback() {
    let budget = budget();
    let catalog = catalog(1, &budget);
    let pressure_bytes = budget.accounting().ordinary_available().get() - SOURCE_QUOTA as u64;
    let pressure = budget.try_reserve_managed(owner(), allocation(3), ByteLength::new(pressure_bytes)).unwrap();
    let before = budget.accounting().reserved().get();
    assert!(matches!(WorkspaceCaptures::new(&catalog, SourceRevision::new(owner(), 1).unwrap(),
        &budget, allocation(2)), Err(WorkspaceError::ResourceDenied)));
    assert_eq!(budget.accounting().reserved().get(), before);
    drop(pressure); drop(catalog);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

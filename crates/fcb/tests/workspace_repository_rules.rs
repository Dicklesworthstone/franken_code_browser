#![forbid(unsafe_code)]
#![cfg(all(feature = "search", unix))]

//! Real discovery -> explicit host captures -> production index. Ignored files
//! contain matching bytes, so dropping policy evaluation would fail the oracle.

use std::{fs, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicU64, Ordering}}};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::source::{CancelFlag, SourceError};
use fcb::search::{CaptureRequest, CompleteCapture, IndexLimits, ParsedQuery, QueryGeneration,
    QueryOptions, RawPath, ResourceAllocationId, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, RuleLimits, WorkspaceCaptures, WorkspaceCatalog, WorkspaceLimits, WorkspaceStage};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-rule-workspace-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, name: impl AsRef<Path>, bytes: &[u8]) {
        let path = self.0.join(name); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
    fn grant(&self) -> RootGrant { RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&self.0)) }
}
impl Drop for Tree { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(5130).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn initial(tree: &Tree, b: &ResourceBudget, limits: RuleLimits) -> WorkspaceCatalog {
    WorkspaceCatalog::open_rule_aware(tree.grant(), SearchManifestId::new(owner(), 1).unwrap(),
        FileId::new(owner(), 10).unwrap(), WorkspaceLimits::default(), limits, b, [id(1), id(2)]).unwrap()
}
fn finish(catalog: &mut WorkspaceCatalog) {
    for _ in 0..5000 {
        if catalog.stage() != WorkspaceStage::Discovering { break; }
        catalog.step(&CancelFlag::new()).unwrap();
    }
    assert_eq!(catalog.stage(), WorkspaceStage::Ready);
}
fn capture(tree: &Tree, request: CaptureRequest, path: &fcb::source::NormalizedPath, limit: usize) -> Result<CompleteCapture, SourceError> {
    assert!(!path.as_bytes().starts_with(b"private/"), "ignored source reached provider");
    assert!(!path.as_bytes().starts_with(b"unknown/"), "unevaluated policy reached provider");
    let bytes = fs::read(tree.0.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable)?;
    if bytes.len() > limit { return Err(SourceError::PayloadTooLarge); }
    CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes))
}
fn captures<'a>(tree: &Tree, catalog: &'a WorkspaceCatalog, b: &ResourceBudget) -> WorkspaceCaptures<'a> {
    let mut captures = WorkspaceCaptures::new(catalog, SourceRevision::new(owner(), 20).unwrap(), b, id(3)).unwrap();
    while !captures.finished() { captures.step(&CancelFlag::new(), |r, p, l| capture(tree, r, p, l)).unwrap(); }
    captures
}

#[test]
fn explicit_rule_construction_does_not_read_configuration_until_worker_steps() {
    let tree = Tree::new(); tree.file(".gitignore", b"private/\n"); tree.file("private/secret", b"needle");
    tree.file("safe.rs", b"needle");
    let b = budget(); let mut catalog = initial(&tree, &b, RuleLimits::default());
    assert!(catalog.reads_rule_files());
    assert_eq!(catalog.rule_policy().unwrap().stats().bytes_read, 0);
    finish(&mut catalog);
    assert!(catalog.discovery_complete());
    assert_eq!(catalog.rule_policy().unwrap().stats().bytes_read, 9);
    assert!(catalog.entries().iter().all(|e| !e.path().as_bytes().starts_with(b"private/")));
    assert_eq!(catalog.rule_policy().unwrap().observations()[0].bytes(), b"private/\n");
}

#[test]
fn rule_filtered_captures_use_exact_utf16_search_and_retain_old_policy_after_live_edit() {
    let tree = Tree::new(); tree.file(".gitignore", b"private/\n"); tree.file("private/secret", b"needle");
    let mut wide = vec![0xff, 0xfe];
    for unit in "head\r\nneedle".encode_utf16() { wide.extend_from_slice(&unit.to_le_bytes()); }
    tree.file("safe.rs", &wide);
    let b = budget(); let mut catalog = initial(&tree, &b, RuleLimits::default()); finish(&mut catalog);
    let policy = catalog.policy_name().to_owned();
    let retained = captures(&tree, &catalog, &b);
    tree.file(".gitignore", b"safe.rs\n"); tree.file("safe.rs", b"no longer the same bytes");
    assert_eq!(catalog.policy_name(), policy);
    assert_eq!(catalog.rule_policy().unwrap().observations()[0].bytes(), b"private/\n");
    let input = retained.search_inputs(&b, id(4)).unwrap();
    let index = input.index(IndexLimits::default(), &b, id(5), || false).unwrap();
    let report = index.search(&ParsedQuery::parse("needle").unwrap(), QueryOptions::new(QueryGeneration::new(owner(), 1).unwrap()),
        &b, id(6), || false).unwrap();
    assert!(report.is_complete()); assert_eq!(report.capture_results().matches.len(), 1);
    let hit = &report.capture_results().matches[0];
    assert_eq!(catalog.entry(hit.file_id).unwrap().path().as_bytes(), b"safe.rs");
    assert_eq!(hit.original_byte_range.start().get(), 14);
    assert_eq!(retained.capture(hit.file_id).unwrap().bytes(), wide);
}

#[test]
fn unknown_rule_subtree_is_not_a_complete_negative_even_when_all_admitted_files_are_captured() {
    let tree = Tree::new(); tree.file("unknown/.gitignore", b"[bad");
    tree.file("unknown/matching.rs", b"needle"); tree.file("safe.rs", b"no match");
    let b = budget(); let mut catalog = initial(&tree, &b, RuleLimits::default()); finish(&mut catalog);
    assert!(!catalog.discovery_complete());
    assert_eq!(catalog.rule_policy().unwrap().stats().failed_files, 1);
    let retained = captures(&tree, &catalog, &b);
    let input = retained.search_inputs(&b, id(4)).unwrap();
    let index = input.index(IndexLimits::default(), &b, id(5), || false).unwrap();
    let report = index.search(&ParsedQuery::parse("needle").unwrap(), QueryOptions::new(QueryGeneration::new(owner(), 1).unwrap()),
        &b, id(6), || false).unwrap();
    assert!(report.capture_results().matches.is_empty()); assert!(!report.is_complete());
    assert!(report.unavailable_files().is_empty(), "unknown membership is distinct from a known missing capture");
}

#[test]
fn bounded_match_refusal_survives_catalog_freeze_and_releases_owned_capacity() {
    let tree = Tree::new(); tree.file("a.rs", b"needle");
    let b = budget();
    let mut catalog = initial(&tree, &b, RuleLimits { max_total_match_steps: 0, ..Default::default() });
    finish(&mut catalog);
    assert!(catalog.entries().is_empty()); assert!(!catalog.discovery_complete());
    assert_eq!(catalog.rule_policy().unwrap().stats().unresolved_paths, 1);
    assert!(b.accounting().reserved().get() > 0);
    drop(catalog); assert_eq!(b.accounting().reserved().get(), 0);
}

#[cfg(feature = "snapshot")]
#[test]
fn snapshots_preserve_rule_scope_and_editing_rules_changes_saved_policy_identity() {
    use fcb::search::snapshot::{export_workspace, SnapshotLimits, SnapshotView};
    let tree = Tree::new(); tree.file(".gitignore", b"private/\n"); tree.file("private/secret", b"needle"); tree.file("safe.rs", b"needle");
    let b = budget();
    let (first_policy, encoded) = {
        let mut catalog = initial(&tree, &b, RuleLimits::default()); finish(&mut catalog);
        assert!(catalog.policy_name().starts_with("repository-rules-v1:"));
        let retained = captures(&tree, &catalog, &b);
        let encoded = export_workspace(&retained, SnapshotLimits::default(), &b, [id(7), id(8)], || false).unwrap();
        (catalog.policy_name().to_owned(), encoded)
    };
    let view = SnapshotView::open(encoded.bytes(), SnapshotLimits::default(), || false).unwrap();
    assert!(view.discovery_complete()); assert_eq!(view.policy(), first_policy);
    assert!(view.entries().all(|entry| !entry.unwrap().path.starts_with(b"private/")));
    tree.file(".gitignore", b"private/\nsafe.rs\n");
    let mut changed = initial(&tree, &b, RuleLimits::default()); finish(&mut changed);
    assert_ne!(changed.policy_name(), first_policy);
    assert!(changed.discovery_complete());
    assert!(changed.entries().iter().all(|e| e.path().as_bytes() != b"safe.rs"));
}

#[cfg(feature = "snapshot")]
#[test]
fn bad_configuration_remains_partial_after_export_and_exact_restoration() {
    use fcb::search::snapshot::{export_workspace, SavedWorkspace, SnapshotLimits, SnapshotView};
    let tree = Tree::new(); tree.file("unknown/.gitignore", b"[bad"); tree.file("unknown/secret", b"needle"); tree.file("safe.rs", b"x");
    let b = budget(); let mut catalog = initial(&tree, &b, RuleLimits::default()); finish(&mut catalog);
    let retained = captures(&tree, &catalog, &b);
    let encoded = export_workspace(&retained, SnapshotLimits::default(), &b, [id(7), id(8)], || false).unwrap();
    let view = SnapshotView::open(encoded.bytes(), SnapshotLimits::default(), || false).unwrap();
    assert!(!view.discovery_complete());
    let restored = SavedWorkspace::restore(view, SearchManifestId::new(owner(), 10).unwrap(), FileId::new(owner(), 100).unwrap(),
        SourceRevision::new(owner(), 200).unwrap(), &b, id(9), || false).unwrap();
    let inputs = restored.search_inputs(&b, id(10), || false).unwrap();
    let index = inputs.index(IndexLimits::default(), &b, id(11), || false).unwrap();
    let report = index.search(&ParsedQuery::parse("needle").unwrap(), QueryOptions::new(QueryGeneration::new(owner(), 2).unwrap()),
        &b, id(12), || false).unwrap();
    assert!(report.capture_results().matches.is_empty()); assert!(!report.is_complete());
}

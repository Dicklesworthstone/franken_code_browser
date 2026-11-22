#![forbid(unsafe_code)]
#![cfg(unix)]

//! Production subtree discovery: inherited policy, original path identity and
//! bounded direct descent. Fixtures remain available for failure diagnosis.

use std::{collections::BTreeMap, ffi::OsString, fs, os::unix::{ffi::OsStringExt, fs::symlink},
    path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, ResourceAllocationId, ResourceBudget, RootId, SourceRevision};
use fcb_source::{BoundedDiscovery, CancelFlag, DiscoveryEntry, DiscoveryKind, DiscoveryLimits,
    IgnoreMatcher, RawPath, RootGrant, ScanStatus, SourceError, SymlinkPolicy};
use fcb_source::path::NormalizedPath;
use fcb_source::ignore::repository::RuleLimits;
use fcb_source::reconciliation::{DirtyHintTracker, KnownEntry, ReconciliationIncompleteReason,
    ReconciliationPass};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-directory-discovery-{}-{}-{}",
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
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(5138).unwrap() }
fn budget() -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap()
}
fn limits() -> DiscoveryLimits { DiscoveryLimits::new(1, 32, 2048, 1, 4096, 1).unwrap() }
fn path(bytes: &[u8]) -> NormalizedPath {
    let mut names = bytes.split(|byte| *byte == b'/');
    let mut path = NormalizedPath::from_dirent_name(names.next().unwrap()).unwrap();
    for name in names { path = path.join_segment(name).unwrap(); }
    path
}
fn rules(tree: &Tree, directory: &[u8], budget: &ResourceBudget) -> BoundedDiscovery {
    BoundedDiscovery::open_rule_aware_in_directory(tree.grant(), path(directory),
        limits(), RuleLimits::default(), budget, ResourceAllocationId::new(1).unwrap()).unwrap()
}
fn drain(walker: &mut BoundedDiscovery) -> Vec<DiscoveryEntry> {
    let mut entries = Vec::new();
    for _ in 0..1024 {
        match walker.next_batch(&CancelFlag::new()).unwrap() {
            Some(page) => {
                entries.extend_from_slice(page.entries());
                if !page.more() { break; }
            }
            None => break,
        }
    }
    assert!(!matches!(walker.status(), ScanStatus::InProgress));
    entries
}
fn included(entries: &[DiscoveryEntry]) -> Vec<Vec<u8>> {
    entries.iter().filter(|entry| entry.kind() == DiscoveryKind::File && !entry.is_excluded())
        .map(|entry| entry.path().as_bytes().to_vec()).collect()
}
fn known_catalog(tree: &Tree) -> BTreeMap<NormalizedPath, KnownEntry> {
    let mut walker = BoundedDiscovery::open_metadata_only(tree.grant(),
        SymlinkPolicy::DisallowAll, DiscoveryLimits::modest(), IgnoreMatcher::include_all()).unwrap();
    assert_eq!(walker.directory(), None);
    let entries = drain(&mut walker);
    assert!(walker.is_complete());
    entries.into_iter().enumerate().map(|(ordinal, entry)| (entry.path().clone(),
        KnownEntry::new(FileId::new(owner(), ordinal as u64 + 1).unwrap(),
            SourceRevision::new(owner(), 1).unwrap(), entry.kind(), entry.observed_len(), None, None)))
        .collect()
}

#[test]
fn direct_descent_loads_inherited_rules_without_enumerating_siblings() {
    let tree = Tree::new();
    tree.file(".gitignore", b"*.tmp\n");
    tree.file("parent/.gitignore", b"*.rs\n!kept.rs\n");
    tree.file("parent/selected/.fcbignore", b"!restored.rs\n");
    for name in ["kept.rs", "restored.rs", "dropped.rs", "dropped.tmp", "notes.txt"] {
        tree.file(format!("parent/selected/{name}"), b"SOURCE_BYTES_MUST_NOT_BE_READ");
    }
    for ordinal in 0..128 {
        // Visiting any sibling would exhaust the one-directory queue and load
        // invalid policy. A focused walk must never encounter either defect.
        tree.file(format!("sibling-{ordinal}/.gitignore"), b"[broken");
        tree.file(format!("sibling-{ordinal}/outside.rs"), b"outside");
    }
    let budget = budget();
    let mut walker = rules(&tree, b"parent/selected", &budget);
    assert_eq!(walker.repository_rules().unwrap().stats().files_loaded, 0,
        "constructing a focused walker must not read rule configuration");
    let entries = drain(&mut walker);
    assert!(entries.iter().all(|entry| entry.path().as_bytes().starts_with(b"parent/selected/")));
    let kept = included(&entries);
    for expected in [b"parent/selected/kept.rs".as_slice(), b"parent/selected/restored.rs",
        b"parent/selected/notes.txt"] {
        assert!(kept.contains(&expected.to_vec()), "missing {expected:?}");
    }
    for excluded in [b"parent/selected/dropped.rs".as_slice(), b"parent/selected/dropped.tmp"] {
        assert!(!kept.contains(&excluded.to_vec()), "incorrectly included {excluded:?}");
    }
    assert!(walker.is_complete());
    assert_eq!(walker.repository_rules().unwrap().stats().files_loaded, 3);
    assert_eq!(walker.aggregate().queue_refused, 0);
    assert!(walker.peaks().open_descriptors <= 1);
    assert!(walker.peaks().queue_entries <= 1);
    assert!(walker.peaks().batch_entries <= 1);
}

#[test]
fn child_negation_cannot_reenter_an_excluded_ancestor() {
    let tree = Tree::new();
    tree.file(".gitignore", b"ignored/\n");
    tree.file("ignored/selected/.gitignore", b"!kept.rs\n");
    tree.file("ignored/selected/kept.rs", b"must stay withheld");
    let budget = budget();
    let mut walker = rules(&tree, b"ignored/selected", &budget);
    assert!(drain(&mut walker).is_empty());
    assert!(walker.root_is_excluded());
    assert!(!walker.is_complete(), "an excluded directory is not a complete empty directory");
    assert_eq!(walker.repository_rules().unwrap().stats().files_loaded, 1);
}

#[test]
fn unavailable_ancestor_policy_is_partial_without_reading_descendant_rules() {
    let tree = Tree::new();
    tree.file(".gitignore", b"[broken");
    tree.file("parent/selected/.gitignore", b"!kept.rs\n");
    tree.file("parent/selected/kept.rs", b"must stay withheld");
    let budget = budget();
    let mut walker = rules(&tree, b"parent/selected", &budget);
    assert!(drain(&mut walker).is_empty());
    assert!(!walker.is_complete());
    let policy = walker.repository_rules().unwrap();
    assert_eq!(policy.stats().files_read, 1);
    assert_eq!(policy.stats().failed_files, 1);
    assert_eq!(policy.diagnostics()[0].path(), b".gitignore");
}

#[test]
fn including_excluded_files_reads_no_rules_but_preserves_owned_artifact_exclusion() {
    let tree = Tree::new();
    tree.file(".gitignore", b"[broken");
    tree.file("target/selected/generated.rs", b"\xff\x00binary");
    tree.file(".fcb-cache/selected/owned.rs", b"owned");
    let mut walker = BoundedDiscovery::open_metadata_only_in_directory(tree.grant(),
        path(b"target/selected"), limits(), IgnoreMatcher::include_all()).unwrap();
    assert!(!walker.reads_rule_files());
    assert_eq!(included(&drain(&mut walker)), vec![b"target/selected/generated.rs".to_vec()]);
    assert!(walker.is_complete());
    let mut owned = BoundedDiscovery::open_metadata_only_in_directory(tree.grant(),
        path(b".fcb-cache/selected"), limits(), IgnoreMatcher::include_all()).unwrap();
    assert!(drain(&mut owned).is_empty());
    assert!(owned.root_is_excluded());
    assert!(!owned.is_complete());
}

#[test]
fn native_directory_bytes_are_never_utf8_or_separator_normalized() {
    let tree = Tree::new();
    let name = OsString::from_vec(b"C:\\raw\xff".to_vec());
    tree.file(Path::new(&name).join("kept.rs"), b"\xff\x00binary");
    let mut walker = BoundedDiscovery::open_metadata_only_in_directory(tree.grant(),
        NormalizedPath::from_dirent_name(b"C:\\raw\xff").unwrap(),
        limits(), IgnoreMatcher::include_all()).unwrap();
    assert_eq!(walker.directory().unwrap().as_bytes(), b"C:\\raw\xff");
    assert_eq!(included(&drain(&mut walker)), vec![b"C:\\raw\xff/kept.rs".to_vec()]);
    assert!(walker.is_complete());
    assert_eq!(walker.directory().unwrap().as_bytes(), b"C:\\raw\xff");
}

#[test]
fn directory_admission_refuses_symlinks_files_missing_paths_and_existing_bounds() {
    let tree = Tree::new(); tree.file("selected/kept.rs", b"x");
    let outside = Tree::new(); outside.file("outside.rs", b"x");
    symlink(tree.0.join("selected"), tree.0.join("alias")).unwrap();
    symlink(&outside.0, tree.0.join("escape")).unwrap();
    for name in [b"alias".as_slice(), b"escape"] {
        assert!(matches!(BoundedDiscovery::open_metadata_only_in_directory(tree.grant(),
            path(name), limits(), IgnoreMatcher::include_all()), Err(SourceError::SymlinkForbidden)));
    }
    for name in [b"missing".as_slice(), b"selected/kept.rs"] {
        assert!(BoundedDiscovery::open_metadata_only_in_directory(tree.grant(),
            path(name), limits(), IgnoreMatcher::include_all()).is_err());
    }
    let tiny = DiscoveryLimits::new(1, 1, 2048, 1, 4096, 1).unwrap();
    assert!(matches!(BoundedDiscovery::open_metadata_only_in_directory(tree.grant(),
        path(b"selected"), tiny, IgnoreMatcher::include_all()), Err(SourceError::PayloadTooLarge)));
    let tiny = DiscoveryLimits::new(1, 32, 3, 1, 4096, 1).unwrap();
    assert!(matches!(BoundedDiscovery::open_metadata_only_in_directory(tree.grant(),
        path(b"selected"), tiny, IgnoreMatcher::include_all()), Err(SourceError::PayloadTooLarge)));
}

#[test]
fn ancestor_descent_uses_bounded_empty_progress_pages() {
    let tree = Tree::new(); tree.file("a/b/c/d/e/kept.rs", b"x");
    let mut walker = BoundedDiscovery::open_metadata_only_in_directory(tree.grant(),
        path(b"a/b/c/d/e"), limits(), IgnoreMatcher::include_all()).unwrap();
    let first = walker.next_batch(&CancelFlag::new()).unwrap().unwrap();
    assert!(first.entries().is_empty());
    assert!(first.more());
    assert_eq!(included(&drain(&mut walker)), vec![b"a/b/c/d/e/kept.rs".to_vec()]);
    assert!(walker.is_complete());
}

#[test]
fn empty_directory_is_complete_but_a_directory_lost_after_admission_is_partial() {
    let tree = Tree::new();
    fs::create_dir(tree.0.join("empty")).unwrap();
    let mut empty = BoundedDiscovery::open_metadata_only_in_directory(tree.grant(),
        path(b"empty"), limits(), IgnoreMatcher::include_all()).unwrap();
    assert!(drain(&mut empty).is_empty());
    assert!(empty.is_complete());
    tree.file("selected/kept.rs", b"x");
    let mut replaced = BoundedDiscovery::open_metadata_only_in_directory(tree.grant(),
        path(b"selected"), limits(), IgnoreMatcher::include_all()).unwrap();
    fs::rename(tree.0.join("selected"), tree.0.join("moved")).unwrap();
    assert!(drain(&mut replaced).is_empty());
    assert!(!replaced.is_complete());
    assert!(replaced.aggregate().unavailable > 0);
}

#[test]
fn scoped_discovery_reconciliation_preserves_the_rest_of_the_root_catalog() {
    for rule_aware in [false, true] {
        let tree = Tree::new();
        let held = Tree::new();
        for name in ["parent/selected/keep.rs", "parent/selected/gone.rs",
            "parent/sibling/keep.rs", "parent/selectedish/keep.rs", "outside/keep.rs"] {
            tree.file(name, b"kept");
        }
        let known = known_catalog(&tree);
        fs::rename(tree.0.join("parent/selected/gone.rs"), held.0.join("gone.rs")).unwrap();
        tree.file("parent/selected/new.rs", b"new");
        let scope = path(b"parent/selected");
        let gone = path(b"parent/selected/gone.rs");
        let mut hints = DirtyHintTracker::new();
        for (name, directory) in [(b"parent".as_slice(), true), (b"parent/selected", true),
            (b"parent/selected/gone.rs", false), (b"parent/sibling", true),
            (b"parent/selectedish", true), (b"outside", true)] {
            hints.record_hint(path(name), directory);
        }
        let budget = budget();
        let mut walker = if rule_aware { rules(&tree, scope.as_bytes(), &budget) } else {
            BoundedDiscovery::open_metadata_only_in_directory(tree.grant(), scope.clone(),
                limits(), IgnoreMatcher::include_all()).unwrap()
        };
        assert_eq!(walker.directory(), Some(&scope));
        let report = ReconciliationPass::reconcile_from_discovery(
            &mut walker, &CancelFlag::new(), &mut hints, known.clone(),
        ).unwrap();

        assert!(report.status.is_completed());
        assert_eq!(report.directory, Some(scope.clone()));
        assert_eq!(report.added, [path(b"parent/selected/new.rs")]);
        assert_eq!(report.retained, [path(b"parent/selected/keep.rs")]);
        assert_eq!(report.tombstoned, [gone.clone()]);
        assert!(report.modified.is_empty());
        assert_eq!(report.preserved_unseen, [path(b"outside"), path(b"outside/keep.rs"),
            path(b"parent"), scope, path(b"parent/selectedish"), path(b"parent/selectedish/keep.rs"),
            path(b"parent/sibling"), path(b"parent/sibling/keep.rs")]);
        assert_eq!(hints.all_hints().iter().map(|hint| hint.path().as_bytes()).collect::<Vec<_>>(),
            [b"parent".as_slice(), b"parent/sibling", b"parent/selectedish", b"outside"]);
        assert!(report.is_honest_and_complete());

        // A later complete root pass has authority to consume the remaining
        // dirty work. Scoped protection must not disable genuine deletions.
        let mut root = BoundedDiscovery::open_metadata_only(tree.grant(),
            SymlinkPolicy::DisallowAll, DiscoveryLimits::modest(), IgnoreMatcher::include_all()).unwrap();
        let report = ReconciliationPass::reconcile_from_discovery(
            &mut root, &CancelFlag::new(), &mut hints, known,
        ).unwrap();
        assert!(report.status.is_completed());
        assert_eq!(report.directory, None);
        assert_eq!(report.tombstoned, [gone]);
        assert!(report.preserved_unseen.is_empty());
        assert_eq!(hints.pending_count(), 0);
    }
}

#[test]
fn interrupted_scoped_discovery_retains_scope_catalog_and_dirty_work() {
    for interruption in ["canceled", "revoked", "moved"] {
        let tree = Tree::new();
        let held = Tree::new();
        tree.file("parent/selected/keep.rs", b"kept");
        tree.file("sibling/keep.rs", b"outside");
        let known = known_catalog(&tree);
        let expected_paths = known.keys().cloned().collect::<Vec<_>>();
        let scope = path(b"parent/selected");
        let grant = tree.grant();
        let mut walker = BoundedDiscovery::open_metadata_only_in_directory(grant.clone(),
            scope.clone(), limits(), IgnoreMatcher::include_all()).unwrap();
        let mut hints = DirtyHintTracker::new();
        hints.record_hint(path(b"parent"), true);
        hints.record_hint(scope.clone(), true);
        hints.record_hint(path(b"sibling"), true);
        let expected_hints = hints.all_hints().to_vec();
        let cancel = CancelFlag::new();
        let reason = match interruption {
            "canceled" => { cancel.cancel(); ReconciliationIncompleteReason::Canceled }
            "revoked" => { grant.revoke(); ReconciliationIncompleteReason::GrantRevoked }
            _ => {
                fs::rename(tree.0.join("parent/selected"), held.0.join("moved")).unwrap();
                ReconciliationIncompleteReason::EnumerationUnavailable
            }
        };
        let report = ReconciliationPass::reconcile_from_discovery(
            &mut walker, &cancel, &mut hints, known,
        ).unwrap();
        assert_eq!(report.directory, Some(scope.clone()));
        assert_eq!(walker.directory(), Some(&scope));
        assert_eq!(report.status.incomplete_reason(), Some(reason));
        assert!(report.tombstoned.is_empty());
        assert_eq!(report.preserved_unseen, expected_paths);
        assert_eq!(hints.all_hints(), expected_hints.as_slice());
        assert!(report.is_honest_and_complete());
    }
}

#[test]
fn ancestor_policy_or_artifact_exclusion_never_erases_a_scoped_catalog() {
    for boundary in ["excluded", "invalid-policy", "owned-artifact"] {
        let tree = Tree::new();
        tree.file("parent/selected/keep.rs", b"kept");
        tree.file("sibling/keep.rs", b"outside");
        tree.file(".gitignore", match boundary {
            "excluded" => b"parent/\n".as_slice(),
            "invalid-policy" => b"[broken", _ => b"",
        });
        let known = known_catalog(&tree);
        let expected_paths = known.keys().cloned().collect::<Vec<_>>();
        let scope = path(b"parent/selected");
        let budget = budget();
        let mut walker = rules(&tree, scope.as_bytes(), &budget);
        if boundary == "owned-artifact" {
            walker.register_own_artifact_path(&tree.0.join("parent")).unwrap();
        }
        let mut hints = DirtyHintTracker::new();
        hints.record_hint(path(b"parent"), true);
        hints.record_hint(scope.clone(), true);
        hints.record_hint(path(b"sibling"), true);
        let expected_hints = hints.all_hints().to_vec();
        let report = ReconciliationPass::reconcile_from_discovery(
            &mut walker, &CancelFlag::new(), &mut hints, known,
        ).unwrap();
        assert_eq!(report.directory, Some(scope));
        assert!(!report.status.is_completed(), "{boundary}");
        assert!(report.tombstoned.is_empty());
        assert_eq!(report.preserved_unseen, expected_paths);
        assert_eq!(hints.all_hints(), expected_hints.as_slice());
        assert_eq!(walker.root_is_excluded(), boundary != "invalid-policy");
    }
}

#[test]
fn scoped_reconciliation_keeps_excluded_descendants_while_deleting_observed_absence() {
    let tree = Tree::new();
    let held = Tree::new();
    tree.file(".gitignore", b"parent/selected/build/\n");
    for name in ["parent/selected/build/hidden.rs", "parent/selected/keep.rs",
        "parent/selected/gone.rs", "sibling/keep.rs"] { tree.file(name, b"kept"); }
    let known = known_catalog(&tree);
    fs::rename(tree.0.join("parent/selected/gone.rs"), held.0.join("gone.rs")).unwrap();
    let scope = path(b"parent/selected");
    let hidden = path(b"parent/selected/build/hidden.rs");
    let gone = path(b"parent/selected/gone.rs");
    let sibling = path(b"sibling");
    let mut hints = DirtyHintTracker::new();
    hints.record_hint(scope.clone(), true);
    hints.record_hint(hidden.clone(), false);
    hints.record_hint(gone.clone(), false);
    hints.record_hint(sibling.clone(), true);
    let budget = budget();
    let mut walker = rules(&tree, scope.as_bytes(), &budget);
    let report = ReconciliationPass::reconcile_from_discovery(
        &mut walker, &CancelFlag::new(), &mut hints, known,
    ).unwrap();
    assert!(report.status.is_completed());
    assert!(!walker.root_is_excluded());
    assert_eq!(report.directory, Some(scope.clone()));
    assert_eq!(report.tombstoned, [gone]);
    assert!(report.preserved_unseen.contains(&hidden));
    assert!(report.preserved_unseen.contains(&path(b"sibling/keep.rs")));
    assert_eq!(hints.all_hints().iter().map(|hint| hint.path()).collect::<Vec<_>>(),
        [&scope, &hidden, &sibling]);
    assert!(report.is_honest_and_complete());
}

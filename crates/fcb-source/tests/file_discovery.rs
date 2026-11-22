#![forbid(unsafe_code)]
#![cfg(unix)]

//! Exact-file discovery under the original grant. Fixtures intentionally remain
//! available for failure diagnosis; no source payload is read by discovery.

use std::{collections::BTreeMap, ffi::OsString, fs, os::unix::{
    ffi::OsStringExt, fs::symlink, net::UnixListener,
}, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, ResourceAllocationId, ResourceBudget,
    RootId, SourceRevision};
use fcb_source::{BoundedDiscovery, CancelFlag, DiscoveryEntry, DiscoveryKind,
    DiscoveryLimits, DiscoveryScope, IgnoreMatcher, RawPath, RootGrant, ScanEpoch,
    ScanStatus, SourceError};
use fcb_source::ignore::repository::RuleLimits;
use fcb_source::path::NormalizedPath;
use fcb_source::reconciliation::{DirtyHintTracker, KnownEntry, ReconciliationPass,
    ReconciliationStatus};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("ff-{:x}-{:x}-{:x}",
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
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(5140).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
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
fn rules(tree: &Tree, file: &[u8], budget: &ResourceBudget, id: u64) -> BoundedDiscovery {
    BoundedDiscovery::open_rule_aware_for_file(tree.grant(), path(file), limits(),
        RuleLimits::default(), budget, allocation(id)).unwrap()
}
fn metadata(tree: &Tree, file: &[u8]) -> BoundedDiscovery {
    BoundedDiscovery::open_metadata_only_for_file(tree.grant(), path(file),
        limits(), IgnoreMatcher::include_all()).unwrap()
}
fn drain(walker: &mut BoundedDiscovery) -> Vec<DiscoveryEntry> {
    let mut entries = Vec::new();
    for _ in 0..64 {
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
fn known(names: &[&[u8]]) -> BTreeMap<NormalizedPath, KnownEntry> {
    names.iter().enumerate().map(|(index, name)| (path(name),
        KnownEntry::new(FileId::new(owner(), index as u64 + 1).unwrap(),
            SourceRevision::new(owner(), 1).unwrap(), DiscoveryKind::File,
            Some(1), None, None))).collect()
}

#[test]
fn only_the_exact_member_spends_the_catalog_budget_after_inherited_rules() {
    let tree = Tree::new();
    tree.file(".gitignore", b"*.tmp\n");
    tree.file("parent/.gitignore", b"*.rs\n");
    tree.file("parent/selected/.fcbignore", b"!kept.rs\n");
    tree.file("parent/selected/kept.rs", b"SOURCE_BYTES_MUST_NOT_BE_READ");
    for ordinal in 0..128 {
        tree.file(format!("parent/selected/sibling-{ordinal}.rs"), b"sibling");
        tree.file(format!("sibling-{ordinal}/.gitignore"), b"[broken");
        tree.file(format!("sibling-{ordinal}/outside.rs"), b"outside");
    }
    let budget = budget();
    let mut walker = rules(&tree, b"parent/selected/kept.rs", &budget, 1);
    assert_eq!(walker.repository_rules().unwrap().stats().files_loaded, 0);
    assert_eq!(walker.directory(), None);
    assert_eq!(walker.file().unwrap().as_bytes(), b"parent/selected/kept.rs");
    assert!(matches!(walker.scope(), DiscoveryScope::File(_)));
    let entries = drain(&mut walker);
    assert_eq!(entries.len(), 1, "siblings and ancestor rule files are not catalog members");
    assert_eq!(entries[0].path().as_bytes(), b"parent/selected/kept.rs");
    assert_eq!(entries[0].kind(), DiscoveryKind::File);
    assert!(!entries[0].is_excluded());
    assert!(walker.is_complete());
    assert_eq!(walker.repository_rules().unwrap().stats().files_loaded, 3);
    assert_eq!(walker.aggregate().files, 1);
    assert_eq!(walker.aggregate().queue_refused, 0);
    assert_eq!(walker.peaks().open_descriptors, 0, "no directory iterator is opened");
    assert!(walker.peaks().queue_entries <= 1);
    assert!(walker.peaks().batch_entries <= 1);
}

#[test]
fn excluded_member_is_reported_separately_from_an_excluded_ancestor() {
    let tree = Tree::new();
    tree.file(".gitignore", b"*.rs\nblocked/\n");
    tree.file("selected/omit.rs", b"withheld");
    tree.file("blocked/.gitignore", b"!kept.rs\n");
    tree.file("blocked/kept.rs", b"still withheld");
    let budget = budget();
    let mut member = rules(&tree, b"selected/omit.rs", &budget, 1);
    let entries = drain(&mut member);
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_excluded());
    assert_eq!(member.aggregate().excluded, 1);
    assert!(member.is_complete(), "an observed deliberate member exclusion is known policy");
    assert!(!member.root_is_excluded());

    let mut ancestor = rules(&tree, b"blocked/kept.rs", &budget, 2);
    assert!(drain(&mut ancestor).is_empty());
    assert!(ancestor.root_is_excluded());
    assert!(!ancestor.is_complete(), "an excluded ancestor was never enumerated");
    assert_eq!(ancestor.repository_rules().unwrap().stats().files_loaded, 1);
}

#[test]
fn unavailable_policy_never_becomes_a_complete_empty_member() {
    let tree = Tree::new();
    tree.file(".gitignore", b"[broken");
    tree.file("parent/.gitignore", b"!kept.rs\n");
    tree.file("parent/kept.rs", b"must stay withheld");
    let budget = budget();
    let mut walker = rules(&tree, b"parent/kept.rs", &budget, 1);
    assert!(drain(&mut walker).is_empty());
    assert!(!walker.is_complete());
    let policy = walker.repository_rules().unwrap();
    assert_eq!(policy.stats().files_read, 1);
    assert_eq!(policy.stats().failed_files, 1);
    assert_eq!(policy.diagnostics()[0].path(), b".gitignore");
}

#[test]
fn include_excluded_reads_no_rules_and_still_withholds_owned_artifacts() {
    let tree = Tree::new();
    tree.file(".gitignore", b"[broken");
    tree.file("target/generated.rs", b"\xff\x00binary");
    tree.file(".fcb.db", b"owned");
    tree.file(".fcb-cache/kept.rs", b"owned");
    let mut selected = metadata(&tree, b"target/generated.rs");
    assert!(!selected.reads_rule_files());
    let entries = drain(&mut selected);
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].is_excluded());
    assert!(selected.is_complete());

    let mut owned = metadata(&tree, b".fcb.db");
    assert!(drain(&mut owned)[0].is_excluded());
    assert_eq!(owned.aggregate().excluded, 1);
    let mut ancestor = metadata(&tree, b".fcb-cache/kept.rs");
    assert!(drain(&mut ancestor).is_empty());
    assert!(ancestor.root_is_excluded());
    assert!(!ancestor.is_complete());
}

#[test]
fn native_member_paths_preserve_non_utf8_backslashes_and_drive_looking_names() {
    let tree = Tree::new();
    let parent = OsString::from_vec(b"C:\\parent\xff".to_vec());
    let name = OsString::from_vec(b"kept\\name\xfe.rs".to_vec());
    tree.file(Path::new(&parent).join(&name), b"source");
    let raw = b"C:\\parent\xff/kept\\name\xfe.rs";
    let mut walker = metadata(&tree, raw);
    let entries = drain(&mut walker);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path().as_bytes(), raw);
    assert_eq!(walker.file().unwrap().as_bytes(), raw);
    assert!(walker.is_complete());
}

#[test]
fn admission_requires_a_regular_confined_member_and_enforces_path_and_page_bounds() {
    let tree = Tree::new();
    tree.file("selected/kept.rs", b"x");
    let outside = Tree::new(); outside.file("outside.rs", b"x");
    symlink(tree.0.join("selected/kept.rs"), tree.0.join("alias.rs")).unwrap();
    symlink(tree.0.join("selected"), tree.0.join("alias-dir")).unwrap();
    symlink(outside.0.join("outside.rs"), tree.0.join("escape.rs")).unwrap();
    for name in [b"alias.rs".as_slice(), b"alias-dir/kept.rs", b"escape.rs"] {
        assert!(matches!(BoundedDiscovery::open_metadata_only_for_file(tree.grant(),
            path(name), limits(), IgnoreMatcher::include_all()), Err(SourceError::SymlinkForbidden)));
    }
    for name in [b"missing.rs".as_slice(), b"selected"] {
        assert!(BoundedDiscovery::open_metadata_only_for_file(tree.grant(),
            path(name), limits(), IgnoreMatcher::include_all()).is_err());
    }
    let _socket = UnixListener::bind(tree.0.join("socket")).unwrap();
    assert!(matches!(BoundedDiscovery::open_metadata_only_for_file(tree.grant(),
        path(b"socket"), limits(), IgnoreMatcher::include_all()), Err(SourceError::SpecialObject)));

    for limit in [
        DiscoveryLimits::new(1, 1, 2048, 1, 4096, 1).unwrap(),
        DiscoveryLimits::new(1, 32, 3, 1, 4096, 1).unwrap(),
        DiscoveryLimits::new(1, 32, 2048, 1, 3, 1).unwrap(),
    ] {
        assert!(matches!(BoundedDiscovery::open_metadata_only_for_file(tree.grant(),
            path(b"selected/kept.rs"), limit, IgnoreMatcher::include_all()),
            Err(SourceError::PayloadTooLarge)));
    }
}

#[test]
fn file_at_maximum_depth_succeeds_and_the_next_component_is_refused() {
    let tree = Tree::new();
    tree.file("root.rs", b"x");
    let one = DiscoveryLimits::new(1, 1, 2048, 1, 4096, 1).unwrap();
    let mut root_file = BoundedDiscovery::open_metadata_only_for_file(tree.grant(),
        path(b"root.rs"), one, IgnoreMatcher::include_all()).unwrap();
    assert_eq!(drain(&mut root_file).len(), 1);
    assert!(root_file.is_complete());

    let selected = format!("{}kept.rs", "a/".repeat(31));
    tree.file(&selected, b"x");
    let mut deepest = metadata(&tree, selected.as_bytes());
    let first = deepest.next_batch(&CancelFlag::new()).unwrap().unwrap();
    assert!(first.entries().is_empty());
    assert!(first.more(), "ancestor visits are bounded empty progress pages");
    let entries = drain(&mut deepest);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].depth(), 32);
    assert!(deepest.is_complete());
    let too_deep = format!("{}kept.rs", "a/".repeat(32));
    assert!(matches!(BoundedDiscovery::open_metadata_only_for_file(tree.grant(),
        path(too_deep.as_bytes()), limits(), IgnoreMatcher::include_all()),
        Err(SourceError::PayloadTooLarge)));
}

#[test]
fn replacement_never_expands_a_file_scope_or_closes_an_empty_search_universe() {
    for replacement in ["missing", "directory", "symlink", "socket"] {
        let tree = Tree::new();
        tree.file("selected.rs", b"admitted");
        let mut walker = metadata(&tree, b"selected.rs");
        fs::rename(tree.0.join("selected.rs"), tree.0.join("held.rs")).unwrap();
        let _socket = match replacement {
            "directory" => { tree.file("selected.rs/child.rs", b"outside file scope"); None }
            "symlink" => {
                symlink(tree.0.join("held.rs"), tree.0.join("selected.rs")).unwrap(); None
            }
            "socket" => Some(UnixListener::bind(tree.0.join("selected.rs")).unwrap()),
            _ => None,
        };
        let entries = drain(&mut walker);
        assert_eq!(entries.len(), 1, "{replacement}");
        assert_eq!(entries[0].path().as_bytes(), b"selected.rs");
        assert!(!walker.is_complete(), "{replacement}");
        assert!(walker.aggregate().unavailable > 0, "{replacement}");
        assert_eq!(walker.peaks().open_descriptors, 0);
    }
}

#[test]
fn cancellation_and_revocation_keep_the_file_scope_and_stop_observation() {
    for revoke in [false, true] {
        let tree = Tree::new(); tree.file("parent/kept.rs", b"x");
        let grant = tree.grant();
        let mut walker = BoundedDiscovery::open_metadata_only_for_file(grant.clone(),
            path(b"parent/kept.rs"), limits(), IgnoreMatcher::include_all()).unwrap();
        let cancel = CancelFlag::new();
        if revoke { grant.revoke(); } else { cancel.cancel(); }
        let error = walker.next_batch(&cancel).unwrap_err();
        assert_eq!(error, if revoke { SourceError::GrantRevoked } else { SourceError::Canceled });
        assert!(!walker.is_complete());
        assert_eq!(walker.file().unwrap().as_bytes(), b"parent/kept.rs");
        assert_eq!(walker.directory(), None);
        assert_eq!(walker.aggregate().files, 0);
    }
}

#[test]
fn file_discovery_reconciliation_preserves_siblings_and_former_descendants() {
    let tree = Tree::new(); tree.file("parent/selected.rs", b"x");
    let mut catalog = known(&[b"parent/selected.rs", b"parent/selected.rs/former.rs",
        b"parent/selected.rs.more", b"parent/sibling.rs", b"outside.rs"]);
    let selected = path(b"parent/selected.rs");
    catalog.get_mut(&selected).unwrap().kind = DiscoveryKind::Directory;
    let mut hints = DirtyHintTracker::new();
    let ancestor = hints.record_hint(path(b"parent"), true);
    hints.record_hint(selected.clone(), false);
    let descendant = hints.record_hint(path(b"parent/selected.rs/former.rs"), false);
    let sibling = hints.record_hint(path(b"parent/sibling.rs"), false);
    let mut walker = metadata(&tree, b"parent/selected.rs");
    let report = ReconciliationPass::reconcile_from_discovery(&mut walker,
        &CancelFlag::new(), &mut hints, catalog).unwrap();
    assert!(report.status.is_completed());
    assert_eq!(report.directory, None);
    assert_eq!(report.file, Some(selected.clone()));
    assert_eq!(report.modified, vec![selected]);
    assert!(report.tombstoned.is_empty());
    assert_eq!(report.preserved_unseen, vec![path(b"outside.rs"),
        path(b"parent/selected.rs.more"), path(b"parent/selected.rs/former.rs"),
        path(b"parent/sibling.rs")]);
    assert_eq!(hints.all_hints().iter().map(|hint| hint.sequence()).collect::<Vec<_>>(),
        [ancestor, descendant, sibling]);
    assert!(report.is_honest_and_complete());
}

#[test]
fn exact_member_reconciliation_rejects_other_observations_and_outside_absence() {
    let selected = path(b"parent/selected.rs");
    let mut hints = DirtyHintTracker::new();
    let mut pass = ReconciliationPass::for_file(selected.clone(), ScanEpoch::new(2),
        &hints, known(&[b"parent/selected.rs", b"parent/sibling.rs", b"parent/selected.rs/old.rs"]));
    pass.record_observation(path(b"parent/sibling.rs"), DiscoveryKind::File, Some(99), None, None);
    pass.record_observation(path(b"parent/selected.rs/new.rs"), DiscoveryKind::File, Some(1), None, None);
    let report = pass.finish(ReconciliationStatus::Completed { epoch: ScanEpoch::new(2) }, &mut hints);
    assert!(report.added.is_empty());
    assert!(report.modified.is_empty());
    assert_eq!(report.tombstoned, [selected]);
    assert_eq!(report.preserved_unseen, [path(b"parent/selected.rs/old.rs"), path(b"parent/sibling.rs")]);
    assert!(report.is_honest_and_complete());
    for invalid in [b"parent".as_slice(), b"parent/selected.rs/old.rs", b"parent/sibling.rs"] {
        let mut defective = report.clone();
        defective.tombstoned.push(path(invalid));
        assert!(!defective.is_honest_and_complete(), "scope oracle accepted {invalid:?}");
    }
}

#[test]
fn a_late_ancestor_hint_prevents_file_absence_and_retains_all_pending_work() {
    let selected = path(b"parent/selected.rs");
    let mut hints = DirtyHintTracker::new();
    hints.record_hint(selected.clone(), false);
    let mut pass = ReconciliationPass::for_file(selected.clone(), ScanEpoch::new(2),
        &hints, known(&[b"parent/selected.rs", b"parent/sibling.rs"]));
    hints.record_hint(path(b"parent"), true);
    let before = hints.all_hints().to_vec();
    let report = pass.finish(ReconciliationStatus::Completed { epoch: ScanEpoch::new(2) }, &mut hints);
    assert!(report.tombstoned.is_empty());
    assert_eq!(report.preserved_unseen, [selected, path(b"parent/sibling.rs")]);
    assert!(report.intervening_hints_detected);
    assert_eq!(hints.all_hints(), before.as_slice());
}

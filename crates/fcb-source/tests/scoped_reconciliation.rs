#![forbid(unsafe_code)]

//! Directory-scope regression cases for FCB-083.A / fcb-tzet.1.
//! A completed subtree cannot establish absence or consume dirty work elsewhere.

use std::collections::BTreeMap;

use fcb_core::{ArenaOwnerId, FileId, SourceRevision};
use fcb_source::discovery::{DiscoveryKind, ScanEpoch};
use fcb_source::path::NormalizedPath;
use fcb_source::reconciliation::{
    DirtyHintTracker, KnownEntry, ReconciliationIncompleteReason, ReconciliationPass,
    ReconciliationStatus,
};

fn path(value: &str) -> NormalizedPath {
    NormalizedPath::new(value).unwrap()
}

fn known_entry(id: u64) -> KnownEntry {
    let owner = ArenaOwnerId::new(1).unwrap();
    KnownEntry::new(
        FileId::new(owner, id).unwrap(),
        SourceRevision::new(owner, 1).unwrap(),
        DiscoveryKind::File,
        Some(1),
        None,
        None,
    )
}

fn known_entries(paths: &[&str]) -> BTreeMap<NormalizedPath, KnownEntry> {
    paths.iter().enumerate()
        .map(|(index, value)| (path(value), known_entry(index as u64 + 1)))
        .collect()
}

fn completed() -> ReconciliationStatus {
    ReconciliationStatus::Completed { epoch: ScanEpoch::new(2) }
}

fn pending_sequences(hints: &DirtyHintTracker) -> Vec<u64> {
    hints.all_hints().iter().map(|hint| hint.sequence()).collect()
}

#[test]
fn completed_subtree_changes_only_its_enumerated_namespace() {
    let mut hints = DirtyHintTracker::new();
    hints.record_hint(path("src"), true);
    hints.record_hint(path("src/gone.rs"), false);
    let outside_hint = hints.record_hint(path("tests/keep.rs"), false);
    let mut known = known_entries(&[
        "src", "src/a.rs", "src/changed.rs", "src/gone.rs", "src2/keep.rs", "tests/keep.rs",
    ]);
    let directory = known.get_mut(&path("src")).unwrap();
    directory.kind = DiscoveryKind::Directory;
    directory.len = None;
    let mut pass = ReconciliationPass::new(Some(path("src")), ScanEpoch::new(2), &hints, known);

    pass.record_observation(path("src/a.rs"), DiscoveryKind::File, Some(1), None, None);
    pass.record_observation(path("src/changed.rs"), DiscoveryKind::File, Some(2), None, None);
    pass.record_observation(path("src/new.rs"), DiscoveryKind::File, Some(1), None, None);
    // A shared producer may deliver other directories' observations. They must
    // not create or modify entries under this pass's completion authority.
    pass.record_observation(path("tests/keep.rs"), DiscoveryKind::File, Some(99), None, None);
    pass.record_observation(path("tests/new.rs"), DiscoveryKind::File, Some(1), None, None);

    let report = pass.finish(completed(), &mut hints);
    assert_eq!(report.added, vec![path("src/new.rs")]);
    assert_eq!(report.modified, vec![path("src/changed.rs")]);
    assert_eq!(report.retained, vec![path("src/a.rs")]);
    assert_eq!(report.tombstoned, vec![path("src/gone.rs")]);
    assert_eq!(report.preserved_unseen, vec![
        path("src"), path("src2/keep.rs"), path("tests/keep.rs"),
    ]);
    assert_eq!(pending_sequences(&hints), vec![outside_hint]);
    assert!(!report.intervening_hints_detected);
    assert!(report.is_honest_and_complete());
}

#[test]
fn child_completion_preserves_ancestor_and_sibling_dirty_work() {
    let mut hints = DirtyHintTracker::new();
    let ancestor = hints.record_hint(path("src"), true);
    hints.record_hint(path("src/left"), true);
    hints.record_hint(path("src/left/gone.rs"), false);
    let sibling = hints.record_hint(path("src/right"), true);
    let similar = hints.record_hint(path("src/leftover"), true);
    let known = known_entries(&[
        "src/left/gone.rs", "src/right/keep.rs", "src/leftover/keep.rs",
    ]);
    let mut child = ReconciliationPass::new(Some(path("src/left")), ScanEpoch::new(2), &hints, known);

    let report = child.finish(completed(), &mut hints);
    assert_eq!(report.tombstoned, vec![path("src/left/gone.rs")]);
    assert_eq!(report.preserved_unseen, vec![
        path("src/leftover/keep.rs"), path("src/right/keep.rs"),
    ]);
    assert_eq!(pending_sequences(&hints), vec![ancestor, sibling, similar]);
    assert!(hints.is_dirty(&path("src/right/keep.rs")));
    assert!(hints.is_dirty(&path("src/left")), "the ancestor remains conservatively dirty");

    // Only a successful pass covering the whole ancestor can retire its hint.
    let mut parent = ReconciliationPass::new(
        Some(path("src")), ScanEpoch::new(2), &hints, BTreeMap::new(),
    );
    assert!(parent.finish(completed(), &mut hints).is_honest_and_complete());
    assert_eq!(hints.pending_count(), 0);
}

#[test]
fn root_completion_reconciles_dirty_work_left_by_a_subtree() {
    let mut hints = DirtyHintTracker::new();
    hints.record_hint(path("src"), true);
    let other_root_child = hints.record_hint(path("tests"), true);
    let mut child = ReconciliationPass::new(
        Some(path("src")), ScanEpoch::new(2), &hints,
        known_entries(&["src/gone.rs", "tests/gone.rs"]),
    );
    let child_report = child.finish(completed(), &mut hints);
    assert_eq!(child_report.tombstoned, vec![path("src/gone.rs")]);
    assert_eq!(child_report.preserved_unseen, vec![path("tests/gone.rs")]);
    assert_eq!(pending_sequences(&hints), vec![other_root_child]);

    let mut root = ReconciliationPass::new(
        None, ScanEpoch::new(2), &hints, known_entries(&["tests/gone.rs"]),
    );
    let root_report = root.finish(completed(), &mut hints);
    assert_eq!(root_report.tombstoned, vec![path("tests/gone.rs")]);
    assert!(root_report.preserved_unseen.is_empty());
    assert_eq!(hints.pending_count(), 0);
}

#[test]
fn explicit_hint_clearing_requires_containment_and_respects_sequence_cutoff() {
    let mut hints = DirtyHintTracker::new();
    let ancestor = hints.record_hint(path("src"), true);
    hints.record_hint(path("src/left"), true);
    hints.record_hint(path("src/left/file.rs"), false);
    let sibling = hints.record_hint(path("src/right"), true);
    let cutoff = hints.record_hint(path("src/leftover"), true);
    let newer = hints.record_hint(path("src/left/new.rs"), false);

    hints.clear_directory_hints(Some(&path("src/left")), cutoff);
    assert_eq!(pending_sequences(&hints), vec![ancestor, sibling, cutoff, newer]);
    hints.clear_directory_hints(Some(&path("src")), cutoff);
    assert_eq!(pending_sequences(&hints), vec![newer]);
    hints.clear_directory_hints(None, newer);
    assert_eq!(hints.pending_count(), 0);
}

#[test]
fn intervening_ancestor_exact_and_descendant_hints_prevent_scoped_tombstones() {
    for (hint_path, is_directory) in [
        ("src", true), ("src/left", true), ("src/left/new.rs", false),
    ] {
        let mut hints = DirtyHintTracker::new();
        let prior = hints.record_hint(path("src/left"), true);
        let mut pass = ReconciliationPass::new(
            Some(path("src/left")), ScanEpoch::new(2), &hints,
            known_entries(&["src/left/gone.rs", "src/right/keep.rs"]),
        );
        let intervening = hints.record_hint(path(hint_path), is_directory);
        let report = pass.finish(completed(), &mut hints);
        assert!(report.tombstoned.is_empty(), "{hint_path}");
        assert_eq!(report.preserved_unseen, vec![
            path("src/left/gone.rs"), path("src/right/keep.rs"),
        ]);
        assert!(report.intervening_hints_detected);
        assert_eq!(pending_sequences(&hints), vec![prior, intervening]);
        assert!(report.is_honest_and_complete());
    }
}

#[test]
fn intervening_sibling_hint_does_not_block_completed_subtree() {
    let mut hints = DirtyHintTracker::new();
    hints.record_hint(path("src/left"), true);
    let mut pass = ReconciliationPass::new(
        Some(path("src/left")), ScanEpoch::new(2), &hints,
        known_entries(&["src/left/gone.rs", "src/right/keep.rs"]),
    );
    let sibling = hints.record_hint(path("src/right"), true);
    let report = pass.finish(completed(), &mut hints);
    assert_eq!(report.tombstoned, vec![path("src/left/gone.rs")]);
    assert_eq!(report.preserved_unseen, vec![path("src/right/keep.rs")]);
    assert!(!report.intervening_hints_detected);
    assert_eq!(pending_sequences(&hints), vec![sibling]);
}

#[test]
fn every_incomplete_subtree_preserves_unseen_entries_and_all_dirty_hints() {
    use ReconciliationIncompleteReason::*;
    let statuses = std::iter::once(ReconciliationStatus::InProgress).chain([
        Canceled, GrantRevoked, QueueSaturated, BudgetExhausted, PermissionDenied,
        RootUnavailable, RulePolicyUnavailable, EnumerationUnavailable, TraversalLimited,
    ].map(|reason| ReconciliationStatus::Incomplete { reason }));
    for status in statuses {
        let mut hints = DirtyHintTracker::new();
        hints.record_hint(path("src"), true);
        hints.record_hint(path("src/left"), true);
        hints.record_hint(path("src/right"), true);
        let before = hints.all_hints().to_vec();
        let mut pass = ReconciliationPass::new(
            Some(path("src/left")), ScanEpoch::new(2), &hints,
            known_entries(&["src/left/gone.rs", "src/right/keep.rs"]),
        );
        let report = pass.finish(status, &mut hints);
        assert_eq!(report.status, status);
        assert!(report.tombstoned.is_empty(), "{status:?}");
        assert_eq!(report.preserved_unseen, vec![
            path("src/left/gone.rs"), path("src/right/keep.rs"),
        ]);
        assert_eq!(hints.all_hints(), before.as_slice());
        assert!(report.is_honest_and_complete());
    }
}

#[cfg(unix)]
#[test]
fn scoped_reconciliation_preserves_native_bytes_and_component_boundaries() {
    let directory = NormalizedPath::from_dirent_name(b"build\\\xff").unwrap();
    let similar = NormalizedPath::from_dirent_name(b"build\\\xffcache").unwrap();
    let nested = path("build/nested.rs");
    let gone = directory.join_segment(b"gone.rs").unwrap();
    let outside = similar.join_segment(b"keep.rs").unwrap();
    let mut hints = DirtyHintTracker::new();
    hints.record_hint(directory.clone(), true);
    let similar_hint = hints.record_hint(similar, true);
    let nested_hint = hints.record_hint(path("build"), true);
    let known = BTreeMap::from([
        (gone.clone(), known_entry(1)),
        (outside.clone(), known_entry(2)),
        (nested.clone(), known_entry(3)),
    ]);
    let mut pass = ReconciliationPass::new(Some(directory), ScanEpoch::new(2), &hints, known);
    let report = pass.finish(completed(), &mut hints);
    assert_eq!(report.tombstoned, vec![gone]);
    assert_eq!(report.preserved_unseen, vec![nested, outside]);
    assert_eq!(pending_sequences(&hints), vec![similar_hint, nested_hint]);
    assert!(report.is_honest_and_complete());
}

#[test]
fn scope_oracle_rejects_outside_tombstones_negative_control() {
    let mut hints = DirtyHintTracker::new();
    let mut pass = ReconciliationPass::new(
        Some(path("src")), ScanEpoch::new(2), &hints, known_entries(&["src/gone.rs"]),
    );
    let report = pass.finish(completed(), &mut hints);
    assert_eq!(report.tombstoned, vec![path("src/gone.rs")]);
    assert!(report.is_honest_and_complete());
    for invalid in ["src", "src2/gone.rs", "tests/gone.rs"] {
        let mut defective = report.clone();
        defective.tombstoned.push(path(invalid));
        assert!(!defective.is_honest_and_complete(), "oracle accepted {invalid}");
    }
}

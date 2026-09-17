#![forbid(unsafe_code)]

//! Integration and oracle tests for scan epochs, reconciliation, dirty-hint
//! tracking, atomic-save continuity, and orphaned annotations (FCB-083.A / fcb-tzet.1).
//!
//! Verifies (§8.6, §8.8):
//! 1. Successful scan epochs tombstone unseen entries only when enumeration completes
//!    without intervening dirty hints.
//! 2. Partial/incomplete scans (canceled, budget exhausted, queue saturated, permission failure)
//!    NEVER tombstone unseen entries.
//! 3. Intervening dirty hints prevent tombstones even if enumeration reached the end.
//! 4. Atomic-save replacements preserve logical file identity and increment revision.
//! 5. Recycled paths retire old identity and allocate a fresh FileId; old annotations
//!    are never attached to a new file simply because a path was recycled.
//! 6. Regular-to-FIFO object changes isolate annotations with `SpecialObject`.
//! 7. Filenames with newlines and bidi controls preserve raw bytes while escaping display.
//! 8. Symlink loops are classified honestly as cycles without unbounded recursion.
//! 9. Negative control oracles detect tombstone leakage and annotation leakage defects.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fcb_core::{ArenaOwnerId, FileId, RootId, SourceRevision};
use fcb_source::confined::SymlinkPolicy;
use fcb_source::discovery::{
    BoundedDiscovery, DiscoveryKind, DiscoveryLimits, ScanEpoch,
};
use fcb_source::old_anchor::{fnv1a, OldAnchor};
use fcb_source::path::{NormalizedPath, RawPath};
use fcb_source::reconciliation::{
    AnnotationId, AnnotationRegistry, ContinuityPolicy, DirtyHintTracker, FileContinuityRegistry,
    FileContinuityResult, KnownEntry, OrphanReason, ReconciliationIncompleteReason,
    ReconciliationPass, ReconciliationReport, ReconciliationStatus, SourceAnnotation,
};
use fcb_source::root::RootGrant;
use fcb_source::{CancelFlag, ObservationDigest};

struct TempTestDir {
    path: PathBuf,
}

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fcb_reconcile_{}_{}_{}",
            prefix,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&path).expect("temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn test_owner_id(val: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(val).unwrap()
}

fn test_root_id(owner: u64, root: u64) -> RootId {
    RootId::new(test_owner_id(owner), root).unwrap()
}

fn write_test_file(dir: &Path, rel: &str, bytes: &[u8]) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut file = File::create(&path).unwrap();
    file.write_all(bytes).unwrap();
}

// ---------------------------------------------------------------------------
// 1. Successful scan epoch tombstones unseen entries
// ---------------------------------------------------------------------------
#[test]
fn test_successful_scan_epoch_tombstones_unseen_entries() {
    let owner = test_owner_id(1);
    let mut dirty_tracker = DirtyHintTracker::new();
    let mut known = BTreeMap::new();

    let path_a = NormalizedPath::new("src/a.rs").unwrap();
    let path_b = NormalizedPath::new("src/b.rs").unwrap();
    let path_c = NormalizedPath::new("src/c.rs").unwrap();

    known.insert(
        path_a.clone(),
        KnownEntry::new(
            FileId::new(owner, 1).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(100),
            Some(1001),
            Some(ObservationDigest::observe(b"a")),
        ),
    );
    known.insert(
        path_b.clone(),
        KnownEntry::new(
            FileId::new(owner, 2).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(200),
            Some(1002),
            Some(ObservationDigest::observe(b"b")),
        ),
    );
    known.insert(
        path_c.clone(),
        KnownEntry::new(
            FileId::new(owner, 3).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(300),
            Some(1003),
            Some(ObservationDigest::observe(b"c")),
        ),
    );

    let mut pass = ReconciliationPass::new(
        Some(NormalizedPath::new("src").unwrap()),
        ScanEpoch::new(2),
        &dirty_tracker,
        known,
    );

    // Epoch 2 observes only a.rs and b.rs; c.rs was removed from disk
    pass.record_observation(
        path_a.clone(),
        DiscoveryKind::File,
        Some(100),
        Some(1001),
        Some(ObservationDigest::observe(b"a")),
    );
    pass.record_observation(
        path_b.clone(),
        DiscoveryKind::File,
        Some(200),
        Some(1002),
        Some(ObservationDigest::observe(b"b")),
    );

    let report = pass.finish(
        ReconciliationStatus::Completed {
            epoch: ScanEpoch::new(2),
        },
        &mut dirty_tracker,
    );

    assert_eq!(report.status, ReconciliationStatus::Completed { epoch: ScanEpoch::new(2) });
    assert_eq!(report.retained, vec![path_a, path_b]);
    assert_eq!(report.tombstoned, vec![path_c]);
    assert!(report.preserved_unseen.is_empty());
    assert!(report.is_honest_and_complete());
}

// ---------------------------------------------------------------------------
// 2. Canceled partial scan NEVER tombstones unseen entries
// ---------------------------------------------------------------------------
#[test]
fn test_canceled_partial_scan_never_tombstones_unseen_entries() {
    let owner = test_owner_id(1);
    let mut dirty_tracker = DirtyHintTracker::new();
    let mut known = BTreeMap::new();

    let path_a = NormalizedPath::new("src/a.rs").unwrap();
    let path_b = NormalizedPath::new("src/b.rs").unwrap();
    let path_c = NormalizedPath::new("src/c.rs").unwrap();

    known.insert(
        path_a.clone(),
        KnownEntry::new(
            FileId::new(owner, 1).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(100),
            None,
            None,
        ),
    );
    known.insert(
        path_b.clone(),
        KnownEntry::new(
            FileId::new(owner, 2).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(200),
            None,
            None,
        ),
    );
    known.insert(
        path_c.clone(),
        KnownEntry::new(
            FileId::new(owner, 3).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(300),
            None,
            None,
        ),
    );

    let mut pass = ReconciliationPass::new(
        Some(NormalizedPath::new("src").unwrap()),
        ScanEpoch::new(2),
        &dirty_tracker,
        known,
    );

    // Only a.rs observed before scan was canceled mid-flight
    pass.record_observation(path_a.clone(), DiscoveryKind::File, Some(100), None, None);

    let report = pass.finish(
        ReconciliationStatus::Incomplete {
            reason: ReconciliationIncompleteReason::Canceled,
        },
        &mut dirty_tracker,
    );

    // Invariant: ZERO tombstones on incomplete scan
    assert!(report.tombstoned.is_empty(), "Incomplete scan must NEVER tombstone unseen entries!");
    assert_eq!(report.retained, vec![path_a]);
    assert_eq!(report.preserved_unseen, vec![path_b, path_c]);
    assert!(report.is_honest_and_complete());
}

// ---------------------------------------------------------------------------
// 3. Queue saturated or budget exhausted NEVER tombstones unseen entries
// ---------------------------------------------------------------------------
#[test]
fn test_budget_exhausted_or_queue_saturated_never_tombstones() {
    let owner = test_owner_id(2);
    let mut dirty_tracker = DirtyHintTracker::new();
    let mut known = BTreeMap::new();

    let path_x = NormalizedPath::new("deep/x.txt").unwrap();
    let path_y = NormalizedPath::new("deep/y.txt").unwrap();

    known.insert(
        path_x.clone(),
        KnownEntry::new(
            FileId::new(owner, 10).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(10),
            None,
            None,
        ),
    );
    known.insert(
        path_y.clone(),
        KnownEntry::new(
            FileId::new(owner, 11).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(20),
            None,
            None,
        ),
    );

    let mut pass = ReconciliationPass::new(None, ScanEpoch::new(3), &dirty_tracker, known);

    // Queue saturated: stopped before visiting y.txt
    pass.record_observation(path_x.clone(), DiscoveryKind::File, Some(10), None, None);

    let report = pass.finish(
        ReconciliationStatus::Incomplete {
            reason: ReconciliationIncompleteReason::QueueSaturated,
        },
        &mut dirty_tracker,
    );

    assert!(report.tombstoned.is_empty());
    assert_eq!(report.preserved_unseen, vec![path_y]);
    assert!(report.is_honest_and_complete());
}

// ---------------------------------------------------------------------------
// 4. Permission failure half-scan NEVER tombstones unseen entries
// ---------------------------------------------------------------------------
#[test]
fn test_permission_failure_half_scan_never_tombstones() {
    let owner = test_owner_id(3);
    let mut dirty_tracker = DirtyHintTracker::new();
    let mut known = BTreeMap::new();

    let restricted_path = NormalizedPath::new("private/locked_archive.dat").unwrap();
    known.insert(
        restricted_path.clone(),
        KnownEntry::new(
            FileId::new(owner, 20).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(256),
            None,
            None,
        ),
    );

    let mut pass = ReconciliationPass::new(
        Some(NormalizedPath::new("private").unwrap()),
        ScanEpoch::new(4),
        &dirty_tracker,
        known,
    );

    // Permission error encountered on directory
    let report = pass.finish(
        ReconciliationStatus::Incomplete {
            reason: ReconciliationIncompleteReason::PermissionDenied,
        },
        &mut dirty_tracker,
    );

    assert!(report.tombstoned.is_empty());
    assert_eq!(report.preserved_unseen, vec![restricted_path]);
    assert!(report.is_honest_and_complete());
}

// ---------------------------------------------------------------------------
// 5. Intervening dirty hints prevent tombstones on completed scan
// ---------------------------------------------------------------------------
#[test]
fn test_intervening_dirty_hints_prevent_tombstones_on_completed_scan() {
    let owner = test_owner_id(4);
    let mut dirty_tracker = DirtyHintTracker::new();
    let mut known = BTreeMap::new();

    let path_1 = NormalizedPath::new("docs/intro.md").unwrap();
    let path_2 = NormalizedPath::new("docs/advanced.md").unwrap();

    known.insert(
        path_1.clone(),
        KnownEntry::new(
            FileId::new(owner, 30).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(500),
            None,
            None,
        ),
    );
    known.insert(
        path_2.clone(),
        KnownEntry::new(
            FileId::new(owner, 31).unwrap(),
            SourceRevision::new(owner, 1).unwrap(),
            DiscoveryKind::File,
            Some(900),
            None,
            None,
        ),
    );

    let mut pass = ReconciliationPass::new(
        Some(NormalizedPath::new("docs").unwrap()),
        ScanEpoch::new(5),
        &dirty_tracker,
        known,
    );

    // During enumeration, only intro.md is observed
    pass.record_observation(path_1.clone(), DiscoveryKind::File, Some(500), None, None);

    // BUT an intervening dirty hint arrived while enumeration was executing!
    let hint_seq = dirty_tracker.record_hint(path_2.clone(), false);
    assert!(hint_seq >= pass.start_hint_sequence());

    // Scan technically finished with Completed status
    let report = pass.finish(
        ReconciliationStatus::Completed {
            epoch: ScanEpoch::new(5),
        },
        &mut dirty_tracker,
    );

    // §8.6: Absence is deletion evidence ONLY when intervening dirty hints have been reconciled.
    assert!(
        report.intervening_hints_detected,
        "Report must flag intervening hints"
    );
    assert!(
        report.tombstoned.is_empty(),
        "Intervening dirty hints MUST prevent tombstones!"
    );
    assert_eq!(report.preserved_unseen, vec![path_2]);
    assert!(report.is_honest_and_complete());
}

// ---------------------------------------------------------------------------
// 6. Atomic-save preserves logical file identity and increments revision
// ---------------------------------------------------------------------------
#[test]
fn test_atomic_save_preserves_logical_identity_and_increments_revision() {
    let owner = test_owner_id(5);
    let mut registry = FileContinuityRegistry::new(owner);
    let path = NormalizedPath::new("src/main.rs").unwrap();

    // Initial observation: inode 5001
    let digest1 = ObservationDigest::observe(b"fn main() { println!(\"1\"); }");
    let res1 = registry
        .observe_file(&path, Some(5001), Some(digest1), ContinuityPolicy::AutomaticAtomicSave)
        .unwrap();

    let (initial_file_id, initial_rev) = match res1 {
        FileContinuityResult::NewFile { file_id, revision } => (file_id, revision),
        other => {
            assert_eq!(true, false, "Expected NewFile, got {other:?}");
            return;
        }
    };
    assert_eq!(initial_rev.get(), 1);

    // Atomic save: editor writes to temp file and renames, changing inode from 5001 to 9999
    let digest2 = ObservationDigest::observe(b"fn main() { println!(\"2\"); }");
    let res2 = registry
        .observe_file(&path, Some(9999), Some(digest2), ContinuityPolicy::AutomaticAtomicSave)
        .unwrap();

    match res2 {
        FileContinuityResult::PreservedAtomicSave {
            file_id,
            prior_revision,
            new_revision,
            prior_inode,
            new_inode,
        } => {
            assert_eq!(file_id, initial_file_id, "Logical FileId must be preserved across atomic save!");
            assert_eq!(prior_revision, initial_rev);
            assert_eq!(new_revision.get(), 2);
            assert_eq!(prior_inode, Some(5001));
            assert_eq!(new_inode, 9999);
        }
        other => {
            assert_eq!(true, false, "Expected PreservedAtomicSave, got {other:?}");
        }
    }

    assert_eq!(registry.active_files_count(), 1);
    assert_eq!(registry.tombstoned_files_count(), 0);
}

// ---------------------------------------------------------------------------
// 7. Recycled path retires old identity and allocates fresh FileId
// ---------------------------------------------------------------------------
#[test]
fn test_recycled_path_retires_identity_and_allocates_fresh_id() {
    let owner = test_owner_id(6);
    let mut registry = FileContinuityRegistry::new(owner);
    let path = NormalizedPath::new("scratch/temp.txt").unwrap();

    // Initial file creation
    let res1 = registry
        .observe_file(&path, Some(6001), None, ContinuityPolicy::AutomaticAtomicSave)
        .unwrap();
    let old_file_id = match res1 {
        FileContinuityResult::NewFile { file_id, .. } => file_id,
        other => {
            assert_eq!(true, false, "Expected NewFile, got {other:?}");
            return;
        }
    };

    // File is verified tombstoned
    let tombstoned_id = registry.mark_tombstoned(&path, ScanEpoch::new(10));
    assert_eq!(tombstoned_id, Some(old_file_id));
    assert_eq!(registry.active_files_count(), 0);
    assert_eq!(registry.tombstoned_files_count(), 1);

    // New file appears at the recycled path
    let res2 = registry
        .observe_file(&path, Some(7001), None, ContinuityPolicy::AutomaticAtomicSave)
        .unwrap();

    match res2 {
        FileContinuityResult::RecycledPathNewIdentity {
            old_file_id: old_id,
            new_file_id: new_id,
            new_revision,
        } => {
            assert_eq!(old_id, old_file_id);
            assert_ne!(new_id, old_file_id, "Recycled path must get a fresh FileId!");
            assert_eq!(new_revision.get(), 1, "Recycled path revision must start at 1");
        }
        other => {
            assert_eq!(true, false, "Expected RecycledPathNewIdentity, got {other:?}");
        }
    }

    assert_eq!(registry.active_files_count(), 1);
    assert_eq!(registry.tombstoned_files_count(), 0);
}

// ---------------------------------------------------------------------------
// 8. Orphaned annotations on content divergence
// ---------------------------------------------------------------------------
#[test]
fn test_orphaned_annotations_on_content_divergence() {
    let owner = test_owner_id(7);
    let file_id = FileId::new(owner, 100).unwrap();
    let rev1 = SourceRevision::new(owner, 1).unwrap();
    let rev2 = SourceRevision::new(owner, 2).unwrap();

    let mut annotations = AnnotationRegistry::new();
    let anchor_text = b"important logic";
    let anchor_digest = fnv1a(anchor_text);

    let ann = SourceAnnotation::new(
        AnnotationId(1),
        file_id,
        rev1,
        OldAnchor::new(10, rev1.get()),
        anchor_digest,
        "Review this critical section".to_string(),
    );
    annotations.add_annotation(ann);
    assert_eq!(annotations.active_count(), 1);
    assert_eq!(annotations.orphaned_count(), 0);

    // Case A: content at anchor offset is modified
    let modified_bytes = b"0123456789different words here instead!";
    let results = annotations.reattach_for_file_modification(file_id, rev2, modified_bytes, anchor_text.len());

    assert_eq!(results.len(), 1);
    assert_eq!(annotations.active_count(), 0);
    assert_eq!(annotations.orphaned_count(), 1);

    let orphaned = annotations.get_orphaned(AnnotationId(1)).unwrap();
    assert_eq!(orphaned.reason, OrphanReason::ContentDiverged);
    assert_eq!(orphaned.annotation.file_id, file_id);
    assert_eq!(orphaned.annotation.revision, rev1);
    assert_eq!(orphaned.annotation.anchor.offset, 10);
    assert_eq!(orphaned.orphaned_at_revision, Some(rev2));
}

// ---------------------------------------------------------------------------
// 9. Orphaned annotations on recycled path NEVER attach to new file (§8.8)
// ---------------------------------------------------------------------------
#[test]
fn test_orphaned_annotations_on_recycled_path_never_attach_to_new_file() {
    let owner = test_owner_id(8);
    let old_file_id = FileId::new(owner, 200).unwrap();
    let new_file_id = FileId::new(owner, 201).unwrap();
    let rev1 = SourceRevision::new(owner, 1).unwrap();

    let mut annotations = AnnotationRegistry::new();
    let ann = SourceAnnotation::new(
        AnnotationId(2),
        old_file_id,
        rev1,
        OldAnchor::new(5, rev1.get()),
        12345,
        "Old file note".to_string(),
    );
    annotations.add_annotation(ann);

    // Path is recycled
    let orphaned_list = annotations.handle_path_recycled(old_file_id, new_file_id);
    assert_eq!(orphaned_list.len(), 1);
    assert_eq!(orphaned_list[0].reason, OrphanReason::PathRecycledNewIdentity);

    // Invariant: no annotations active on new_file_id or old_file_id
    assert_eq!(annotations.active_count(), 0);
    assert_eq!(annotations.orphaned_count(), 1);

    let orphaned = annotations.get_orphaned(AnnotationId(2)).unwrap();
    assert_eq!(orphaned.annotation.file_id, old_file_id);
    assert_ne!(orphaned.annotation.file_id, new_file_id);
}

// ---------------------------------------------------------------------------
// 10. Regular to FIFO race protects annotations
// ---------------------------------------------------------------------------
#[test]
fn test_regular_to_fifo_race_protects_annotations() {
    let owner = test_owner_id(9);
    let file_id = FileId::new(owner, 300).unwrap();
    let rev = SourceRevision::new(owner, 1).unwrap();

    let mut annotations = AnnotationRegistry::new();
    let ann = SourceAnnotation::new(
        AnnotationId(3),
        file_id,
        rev,
        OldAnchor::new(0, rev.get()),
        999,
        "Note on file that becomes FIFO".to_string(),
    );
    annotations.add_annotation(ann);

    let orphaned = annotations.handle_file_became_special(file_id);
    assert_eq!(orphaned.len(), 1);
    assert_eq!(orphaned[0].reason, OrphanReason::SpecialObject);
    assert_eq!(annotations.active_count(), 0);
    assert_eq!(annotations.orphaned_count(), 1);
}

// ---------------------------------------------------------------------------
// 11. Symlink loop traversal cycle reporting
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn test_symlink_loop_traversal_cycle_reporting() {
    let dir = TempTestDir::new("symloop");
    write_test_file(dir.path(), "sub/file.txt", b"content");

    // Create a cycle: sub/loop -> dir.path()
    let loop_path = dir.path().join("sub/loop");
    let _ = std::os::unix::fs::symlink(dir.path(), &loop_path);

    let grant = RootGrant::new(test_root_id(10, 1), dir.path());
    let limits = DiscoveryLimits::new(2, 10, 256, 32, 4096, 64).unwrap();
    let mut discovery = BoundedDiscovery::open(grant, SymlinkPolicy::AllowWithinRoot, limits).unwrap();

    let cancel = CancelFlag::new();
    let mut entries = Vec::new();
    while let Some(batch) = discovery.next_batch(&cancel).unwrap() {
        for entry in batch.entries() {
            entries.push((entry.path().clone(), entry.kind()));
        }
        if !batch.more() {
            break;
        }
    }

    // Traversal must complete without infinite loop
    assert!(discovery.is_complete());
    // The cycle link must be reported as Cycle or Symlink without infinite descent
    let has_cycle = entries.iter().any(|(_, k)| *k == DiscoveryKind::Cycle)
        || discovery.aggregate().cycles > 0;
    assert!(has_cycle || entries.iter().any(|(p, k)| p.as_str() == Ok("sub/loop") && *k == DiscoveryKind::Symlink));
}

// ---------------------------------------------------------------------------
// 12. Raw path preserves bytes and escapes newline and bidi characters
// ---------------------------------------------------------------------------
#[test]
fn test_raw_path_preserves_bytes_and_escapes_newline_and_bidi() {
    // Filename containing newline: "file\nwith\nnewlines.txt"
    let newline_name = b"file\nwith\nnewlines.txt";
    let raw_newline = RawPath::from_bytes(newline_name.as_slice());
    assert_eq!(raw_newline.as_bytes(), newline_name);
    let escaped_newline = format!("{}", raw_newline.display_escaped());
    assert!(!escaped_newline.contains('\n'), "Escaped display must not contain raw newlines!");
    assert_eq!(escaped_newline, "file\\nwith\\nnewlines.txt");

    // Filename containing RTL override: "\u{202E}evil.rs"
    let bidi_name = "\u{202E}evil.rs".as_bytes();
    let raw_bidi = RawPath::from_bytes(bidi_name);
    assert_eq!(raw_bidi.as_bytes(), bidi_name);
    let escaped_bidi = format!("{}", raw_bidi.display_escaped());
    assert!(!escaped_bidi.contains('\u{202E}'), "Escaped display must not contain raw bidi overrides!");
    assert_eq!(escaped_bidi, "\\u{202e}evil.rs");
}

// ---------------------------------------------------------------------------
// 13. Negative control oracle: detects tombstone leak on incomplete scan
// ---------------------------------------------------------------------------
fn verify_tombstone_safety_oracle(report: &ReconciliationReport) -> Result<(), &'static str> {
    if !report.status.is_completed() && !report.tombstoned.is_empty() {
        return Err("DEFECT: Incomplete scan emitted tombstones for unseen files!");
    }
    if report.intervening_hints_detected && !report.tombstoned.is_empty() {
        return Err("DEFECT: Intervening dirty hints did not prevent tombstones!");
    }
    Ok(())
}

#[test]
fn test_negative_control_oracle_detects_tombstone_leak_on_incomplete_scan() {
    // Legitimate report passes oracle
    let safe_report = ReconciliationReport {
        epoch: ScanEpoch::new(1),
        status: ReconciliationStatus::Incomplete {
            reason: ReconciliationIncompleteReason::Canceled,
        },
        directory: None,
        added: Vec::new(),
        modified: Vec::new(),
        retained: Vec::new(),
        tombstoned: Vec::new(),
        preserved_unseen: vec![NormalizedPath::new("a.rs").unwrap()],
        intervening_hints_detected: false,
    };
    assert_eq!(verify_tombstone_safety_oracle(&safe_report), Ok(()));

    // Planted defective report: incomplete scan with leaked tombstone
    let defective_report = ReconciliationReport {
        epoch: ScanEpoch::new(1),
        status: ReconciliationStatus::Incomplete {
            reason: ReconciliationIncompleteReason::Canceled,
        },
        directory: None,
        added: Vec::new(),
        modified: Vec::new(),
        retained: Vec::new(),
        tombstoned: vec![NormalizedPath::new("a.rs").unwrap()], // LEAKED DEFECT
        preserved_unseen: Vec::new(),
        intervening_hints_detected: false,
    };
    let oracle_res = verify_tombstone_safety_oracle(&defective_report);
    assert!(
        oracle_res.is_err(),
        "Negative control oracle must detect tombstone leak on incomplete scan!"
    );
}

// ---------------------------------------------------------------------------
// 14. Negative control oracle: detects recycled path note leak
// ---------------------------------------------------------------------------
fn verify_annotation_isolation_oracle(
    old_file: FileId,
    new_file: FileId,
    registry: &AnnotationRegistry,
) -> Result<(), &'static str> {
    for ann in registry.all_active() {
        if ann.file_id == new_file && ann.text.contains("Old file note") {
            return Err("DEFECT: Old note was leaked and attached to recycled path with new FileId!");
        }
        if ann.file_id == old_file {
            return Err("DEFECT: Old note is still active on tombstoned file!");
        }
    }
    Ok(())
}

#[test]
fn test_negative_control_oracle_detects_recycled_path_note_leak() {
    let owner = test_owner_id(11);
    let old_id = FileId::new(owner, 500).unwrap();
    let new_id = FileId::new(owner, 501).unwrap();
    let rev = SourceRevision::new(owner, 1).unwrap();

    let mut registry = AnnotationRegistry::new();
    let ann = SourceAnnotation::new(
        AnnotationId(10),
        old_id,
        rev,
        OldAnchor::new(0, 1),
        100,
        "Old file note".to_string(),
    );
    registry.add_annotation(ann);

    // Correct handling
    registry.handle_path_recycled(old_id, new_id);
    assert_eq!(verify_annotation_isolation_oracle(old_id, new_id, &registry), Ok(()));

    // Planted defective registry: note reattached to new_id
    let mut defective_registry = AnnotationRegistry::new();
    let leaked_ann = SourceAnnotation::new(
        AnnotationId(11),
        new_id, // LEAKED DEFECT
        rev,
        OldAnchor::new(0, 1),
        100,
        "Old file note".to_string(),
    );
    defective_registry.add_annotation(leaked_ann);
    let oracle_res = verify_annotation_isolation_oracle(old_id, new_id, &defective_registry);
    assert!(
        oracle_res.is_err(),
        "Negative control oracle must detect note leaked to recycled path!"
    );
}

// ---------------------------------------------------------------------------
// 15. End-to-end discovery and reconciliation lifecycle
// ---------------------------------------------------------------------------
#[test]
fn test_end_to_end_discovery_reconciliation_workflow() {
    let dir = TempTestDir::new("e2e_reconcile");
    let owner = test_owner_id(12);

    // Initial files
    write_test_file(dir.path(), "file1.txt", b"hello");
    write_test_file(dir.path(), "file2.txt", b"world");

    let grant = RootGrant::new(test_root_id(12, 1), dir.path());
    let limits = DiscoveryLimits::new(4, 10, 256, 32, 4096, 64).unwrap();
    let mut dirty_tracker = DirtyHintTracker::new();
    let cancel = CancelFlag::new();

    // Pass 1: initial discovery
    let mut disc1 = BoundedDiscovery::open(grant.clone(), SymlinkPolicy::DisallowAll, limits).unwrap();
    let report1 = ReconciliationPass::reconcile_from_discovery(
        &mut disc1,
        &cancel,
        &mut dirty_tracker,
        BTreeMap::new(),
    )
    .unwrap();

    assert!(report1.status.is_completed());
    assert_eq!(report1.added.len(), 2);
    assert!(report1.tombstoned.is_empty());
    assert!(report1.preserved_unseen.is_empty());

    // Build known map from report1
    let mut known = BTreeMap::new();
    for (idx, path) in report1.added.iter().enumerate() {
        known.insert(
            path.clone(),
            KnownEntry::new(
                FileId::new(owner, idx as u64 + 1).unwrap(),
                SourceRevision::new(owner, 1).unwrap(),
                DiscoveryKind::File,
                Some(5),
                None,
                None,
            ),
        );
    }

    // Now modify disk: remove file2.txt, add file3.txt
    let _ = fs::remove_file(dir.path().join("file2.txt"));
    write_test_file(dir.path(), "file3.txt", b"franken");

    // Pass 2: subsequent discovery
    let mut disc2 = BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();
    let report2 = ReconciliationPass::reconcile_from_discovery(
        &mut disc2,
        &cancel,
        &mut dirty_tracker,
        known,
    )
    .unwrap();

    assert!(report2.status.is_completed());
    let added_strs: Vec<String> = report2.added.iter().map(|p| p.as_str().unwrap().to_string()).collect();
    let retained_strs: Vec<String> = report2.retained.iter().map(|p| p.as_str().unwrap().to_string()).collect();
    let tombstoned_strs: Vec<String> = report2.tombstoned.iter().map(|p| p.as_str().unwrap().to_string()).collect();

    assert_eq!(added_strs, vec!["file3.txt"]);
    assert_eq!(retained_strs, vec!["file1.txt"]);
    assert_eq!(tombstoned_strs, vec!["file2.txt"]);
    assert!(report2.preserved_unseen.is_empty());
    assert!(report2.is_honest_and_complete());
}

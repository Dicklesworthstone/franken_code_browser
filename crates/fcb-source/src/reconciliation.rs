#![forbid(unsafe_code)]

//! Reconciliation epochs, dirty-hint tracking, atomic-save continuity, and
//! orphaned annotations (FCB-083.A / fcb-tzet.1).
//!
//! §8.6, §8.8:
//! A reconciliation pass has a directory identity, scan epoch, start observation,
//! and explicit completion status. Absence becomes deletion evidence only after
//! that directory's relevant enumeration completes successfully and intervening
//! dirty hints have been reconciled. A partially read directory, budget exhaustion,
//! permission failure, or canceled scan cannot tombstone unseen entries.
//!
//! For atomic-save replacements, preserving a logical file identity is an explicit
//! namespace-continuity decision, not an assumption that its inode stayed the same.
//! Reattach annotations to new bytes only when the chosen exact or qualified
//! anchor-mapping rule succeeds. Otherwise keep an orphaned/stale annotation with
//! its original source evidence. Never attach an old note to an unrelated new file
//! simply because a path was recycled.

use std::collections::BTreeMap;

use fcb_core::{ArenaOwnerId, FileId, SourceRevision};

use crate::discovery::{
    BoundedDiscovery, DiscoveryEntry, DiscoveryKind, IncompleteReason, ScanEpoch, ScanStatus,
};
use crate::old_anchor::{fnv1a, OldAnchor};
use crate::path::NormalizedPath;
use crate::{CancelFlag, ObservationDigest, SourceError};

/// A dirty hint recorded for a path or directory from an external watcher or user action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirtyHint {
    path: NormalizedPath,
    sequence: u64,
    is_directory: bool,
}

impl DirtyHint {
    pub fn new(path: NormalizedPath, sequence: u64, is_directory: bool) -> Self {
        Self {
            path,
            sequence,
            is_directory,
        }
    }

    pub fn path(&self) -> &NormalizedPath {
        &self.path
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn is_directory(&self) -> bool {
        self.is_directory
    }

    /// Whether this dirty hint affects the given path or directory.
    pub fn affects(&self, target: &NormalizedPath) -> bool {
        if self.path == *target {
            return true;
        }
        let hint_bytes = self.path.as_bytes();
        let target_bytes = target.as_bytes();

        // If hint is a directory and target is within that directory
        if self.is_directory {
            if target_bytes.starts_with(hint_bytes)
                && target_bytes.get(hint_bytes.len()) == Some(&b'/')
            {
                return true;
            }
        } else if target_bytes.starts_with(hint_bytes)
            && target_bytes.get(hint_bytes.len()) == Some(&b'/')
        {
            return true;
        }

        // If target is a directory and hint is inside target
        if hint_bytes.starts_with(target_bytes) && hint_bytes.get(target_bytes.len()) == Some(&b'/')
        {
            return true;
        }

        false
    }
}

/// Tracks pending dirty hints to reconcile during directory scans.
#[derive(Clone, Debug, Default)]
pub struct DirtyHintTracker {
    next_sequence: u64,
    hints: Vec<DirtyHint>,
}

impl DirtyHintTracker {
    pub fn new() -> Self {
        Self {
            next_sequence: 1,
            hints: Vec::new(),
        }
    }

    pub fn current_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Record a dirty hint for a file or directory. Returns the assigned sequence number.
    pub fn record_hint(&mut self, path: NormalizedPath, is_directory: bool) -> u64 {
        let seq = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.hints.push(DirtyHint::new(path, seq, is_directory));
        seq
    }

    /// Check if a path has any pending dirty hints.
    pub fn is_dirty(&self, path: &NormalizedPath) -> bool {
        self.hints.iter().any(|h| h.affects(path))
    }

    /// Check if any hints arrived at or after `sequence` that affect `directory` (or root if None).
    pub fn has_hints_since(&self, sequence: u64, directory: Option<&NormalizedPath>) -> bool {
        self.hints.iter().any(|h| {
            if h.sequence < sequence {
                return false;
            }
            match directory {
                None => true,
                Some(dir) => h.affects(dir),
            }
        })
    }

    /// Clear dirty hints affecting `directory` up to and including `up_to_sequence`.
    pub fn clear_directory_hints(
        &mut self,
        directory: Option<&NormalizedPath>,
        up_to_sequence: u64,
    ) {
        self.hints.retain(|h| {
            if h.sequence > up_to_sequence {
                return true;
            }
            match directory {
                None => false,
                Some(dir) => !h.affects(dir),
            }
        });
    }

    pub fn pending_count(&self) -> usize {
        self.hints.len()
    }

    pub fn all_hints(&self) -> &[DirtyHint] {
        &self.hints
    }
}

/// Why a reconciliation pass stopped short of full enumeration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationIncompleteReason {
    Canceled,
    GrantRevoked,
    QueueSaturated,
    BudgetExhausted,
    PermissionDenied,
    RootUnavailable,
}

impl From<IncompleteReason> for ReconciliationIncompleteReason {
    fn from(reason: IncompleteReason) -> Self {
        match reason {
            IncompleteReason::Canceled => Self::Canceled,
            IncompleteReason::GrantRevoked => Self::GrantRevoked,
            IncompleteReason::QueueSaturated => Self::QueueSaturated,
            IncompleteReason::RootUnavailable => Self::RootUnavailable,
        }
    }
}

/// Status of a reconciliation pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationStatus {
    InProgress,
    Completed { epoch: ScanEpoch },
    Incomplete { reason: ReconciliationIncompleteReason },
}

impl ReconciliationStatus {
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }

    pub fn incomplete_reason(&self) -> Option<ReconciliationIncompleteReason> {
        match self {
            Self::Incomplete { reason } => Some(*reason),
            _ => None,
        }
    }
}

/// A known file entry before reconciliation begins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownEntry {
    pub file_id: FileId,
    pub revision: SourceRevision,
    pub kind: DiscoveryKind,
    pub len: Option<u64>,
    pub inode: Option<u64>,
    pub digest: Option<ObservationDigest>,
}

impl KnownEntry {
    pub fn new(
        file_id: FileId,
        revision: SourceRevision,
        kind: DiscoveryKind,
        len: Option<u64>,
        inode: Option<u64>,
        digest: Option<ObservationDigest>,
    ) -> Self {
        Self {
            file_id,
            revision,
            kind,
            len,
            inode,
            digest,
        }
    }
}

/// An entry observed during the current reconciliation pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedEntry {
    pub kind: DiscoveryKind,
    pub len: Option<u64>,
    pub inode: Option<u64>,
    pub digest: Option<ObservationDigest>,
}

impl ObservedEntry {
    pub fn new(
        kind: DiscoveryKind,
        len: Option<u64>,
        inode: Option<u64>,
        digest: Option<ObservationDigest>,
    ) -> Self {
        Self {
            kind,
            len,
            inode,
            digest,
        }
    }
}

/// Outcome report of a completed or partial reconciliation pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconciliationReport {
    pub epoch: ScanEpoch,
    pub status: ReconciliationStatus,
    pub directory: Option<NormalizedPath>,
    pub added: Vec<NormalizedPath>,
    pub modified: Vec<NormalizedPath>,
    pub retained: Vec<NormalizedPath>,
    pub tombstoned: Vec<NormalizedPath>,
    pub preserved_unseen: Vec<NormalizedPath>,
    pub intervening_hints_detected: bool,
}

impl ReconciliationReport {
    /// Invariant assertion: An incomplete scan or a scan with intervening dirty hints
    /// must NEVER tombstone any unseen entries!
    pub fn is_honest_and_complete(&self) -> bool {
        if !self.status.is_completed() || self.intervening_hints_detected {
            self.tombstoned.is_empty()
        } else {
            true
        }
    }
}

/// Manages a reconciliation pass over a directory.
pub struct ReconciliationPass {
    directory: Option<NormalizedPath>,
    epoch: ScanEpoch,
    start_hint_sequence: u64,
    known_entries: BTreeMap<NormalizedPath, KnownEntry>,
    observed_entries: BTreeMap<NormalizedPath, ObservedEntry>,
    status: ReconciliationStatus,
}

impl ReconciliationPass {
    pub fn new(
        directory: Option<NormalizedPath>,
        epoch: ScanEpoch,
        dirty_tracker: &DirtyHintTracker,
        known_entries: BTreeMap<NormalizedPath, KnownEntry>,
    ) -> Self {
        Self {
            directory,
            epoch,
            start_hint_sequence: dirty_tracker.current_sequence(),
            known_entries,
            observed_entries: BTreeMap::new(),
            status: ReconciliationStatus::InProgress,
        }
    }

    pub fn directory(&self) -> Option<&NormalizedPath> {
        self.directory.as_ref()
    }

    pub fn epoch(&self) -> ScanEpoch {
        self.epoch
    }

    pub fn start_hint_sequence(&self) -> u64 {
        self.start_hint_sequence
    }

    pub fn record_observation(
        &mut self,
        path: NormalizedPath,
        kind: DiscoveryKind,
        len: Option<u64>,
        inode: Option<u64>,
        digest: Option<ObservationDigest>,
    ) {
        self.observed_entries.insert(
            path,
            ObservedEntry::new(kind, len, inode, digest),
        );
    }

    /// Record observations from a `DiscoveryEntry`.
    pub fn record_discovery_entry(&mut self, entry: &DiscoveryEntry) {
        self.record_observation(
            entry.path().clone(),
            entry.kind(),
            entry.observed_len(),
            None,
            None,
        );
    }

    /// Finish the reconciliation pass with an explicit status.
    ///
    /// # Invariant (§8.6)
    /// Absence becomes deletion evidence only after enumeration completes
    /// successfully AND intervening dirty hints have been reconciled. A partially
    /// read directory, budget exhaustion, permission failure, or canceled scan
    /// cannot tombstone unseen entries.
    pub fn finish(
        &mut self,
        status: ReconciliationStatus,
        dirty_tracker: &mut DirtyHintTracker,
    ) -> ReconciliationReport {
        self.status = status;

        let mut report = ReconciliationReport {
            epoch: self.epoch,
            status,
            directory: self.directory.clone(),
            added: Vec::new(),
            modified: Vec::new(),
            retained: Vec::new(),
            tombstoned: Vec::new(),
            preserved_unseen: Vec::new(),
            intervening_hints_detected: false,
        };

        // Categorize observed entries
        for (path, observed) in &self.observed_entries {
            if let Some(known) = self.known_entries.get(path) {
                let is_modified = known.kind != observed.kind
                    || known.len != observed.len
                    || (known.inode.is_some()
                        && observed.inode.is_some()
                        && known.inode != observed.inode)
                    || (known.digest.is_some()
                        && observed.digest.is_some()
                        && known.digest != observed.digest);

                if is_modified {
                    report.modified.push(path.clone());
                } else {
                    report.retained.push(path.clone());
                }
            } else {
                report.added.push(path.clone());
            }
        }

        // Categorize known entries not observed in this pass
        let completed = matches!(status, ReconciliationStatus::Completed { .. });
        let intervening_hints = dirty_tracker
            .has_hints_since(self.start_hint_sequence, self.directory.as_ref());

        if intervening_hints {
            report.intervening_hints_detected = true;
        }

        for path in self.known_entries.keys() {
            if !self.observed_entries.contains_key(path) {
                // §8.6: Absence becomes deletion evidence ONLY when scan completed
                // successfully and NO intervening dirty hints arrived.
                if completed && !intervening_hints {
                    report.tombstoned.push(path.clone());
                } else {
                    report.preserved_unseen.push(path.clone());
                }
            }
        }

        // If completed without intervening hints, clear reconciled dirty hints
        if completed && !intervening_hints {
            dirty_tracker
                .clear_directory_hints(self.directory.as_ref(), self.start_hint_sequence);
        }

        report
    }

    /// Reconcile a directory pass directly using a `BoundedDiscovery` session.
    pub fn reconcile_from_discovery(
        discovery: &mut BoundedDiscovery,
        cancel: &CancelFlag,
        dirty_tracker: &mut DirtyHintTracker,
        known_entries: BTreeMap<NormalizedPath, KnownEntry>,
    ) -> Result<ReconciliationReport, SourceError> {
        let mut pass = Self::new(None, discovery.scan_epoch(), dirty_tracker, known_entries);

        loop {
            match discovery.next_batch(cancel) {
                Ok(Some(batch)) => {
                    for entry in batch.entries() {
                        pass.record_discovery_entry(entry);
                    }
                    if !batch.more() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(SourceError::Canceled) => {
                    return Ok(pass.finish(
                        ReconciliationStatus::Incomplete {
                            reason: ReconciliationIncompleteReason::Canceled,
                        },
                        dirty_tracker,
                    ));
                }
                Err(SourceError::GrantRevoked) => {
                    return Ok(pass.finish(
                        ReconciliationStatus::Incomplete {
                            reason: ReconciliationIncompleteReason::GrantRevoked,
                        },
                        dirty_tracker,
                    ));
                }
                Err(SourceError::RootUnavailable) => {
                    return Ok(pass.finish(
                        ReconciliationStatus::Incomplete {
                            reason: ReconciliationIncompleteReason::RootUnavailable,
                        },
                        dirty_tracker,
                    ));
                }
                Err(err) => return Err(err),
            }
        }

        let terminal_status = match discovery.status() {
            ScanStatus::Completed { epoch } => ReconciliationStatus::Completed { epoch },
            ScanStatus::Incomplete { reason } => ReconciliationStatus::Incomplete {
                reason: reason.into(),
            },
            ScanStatus::InProgress => ReconciliationStatus::Incomplete {
                reason: ReconciliationIncompleteReason::BudgetExhausted,
            },
        };

        Ok(pass.finish(terminal_status, dirty_tracker))
    }
}

/// Explicit decision on file identity continuity across an update or atomic save.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContinuityDecision {
    /// Explicitly preserve logical file identity across atomic save / rewrite.
    PreserveLogicalIdentity,
    /// Treat as a recycled path: retire old identity and allocate a new one.
    RecyclePathNewIdentity,
}

/// Policy governing identity continuity during file observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContinuityPolicy {
    /// Automatically preserve logical identity for continuous active files (atomic saves),
    /// but recycle identity with a new FileId if the path was tombstoned.
    AutomaticAtomicSave,
    /// Apply an explicit continuity decision.
    ExplicitPolicy(ContinuityDecision),
}

/// Outcome of reconciling a file identity observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileContinuityResult {
    /// Brand new file never seen before.
    NewFile {
        file_id: FileId,
        revision: SourceRevision,
    },
    /// In-place modification with unchanged inode.
    InPlaceModified {
        file_id: FileId,
        prior_revision: SourceRevision,
        new_revision: SourceRevision,
    },
    /// Atomic save replacement detected (inode changed while path remained active).
    /// Logical identity was preserved by explicit decision.
    PreservedAtomicSave {
        file_id: FileId,
        prior_revision: SourceRevision,
        new_revision: SourceRevision,
        prior_inode: Option<u64>,
        new_inode: u64,
    },
    /// Path was recycled (file was previously tombstoned or continuity was refused).
    /// Old identity retired; brand new logical identity allocated.
    RecycledPathNewIdentity {
        old_file_id: FileId,
        new_file_id: FileId,
        new_revision: SourceRevision,
    },
}

/// An entry in the file continuity registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileContinuityEntry {
    pub file_id: FileId,
    pub current_revision: SourceRevision,
    pub inode: Option<u64>,
    pub digest: Option<ObservationDigest>,
    pub is_tombstoned: bool,
    pub tombstone_epoch: Option<ScanEpoch>,
}

/// Registry that tracks file identity continuity across edits, atomic saves, and tombstone cycles.
pub struct FileContinuityRegistry {
    owner: ArenaOwnerId,
    next_file_id: u64,
    entries: BTreeMap<NormalizedPath, FileContinuityEntry>,
}

impl FileContinuityRegistry {
    pub fn new(owner: ArenaOwnerId) -> Self {
        Self {
            owner,
            next_file_id: 1,
            entries: BTreeMap::new(),
        }
    }

    /// Reconcile identity for a file observed at `path`.
    pub fn observe_file(
        &mut self,
        path: &NormalizedPath,
        inode: Option<u64>,
        digest: Option<ObservationDigest>,
        policy: ContinuityPolicy,
    ) -> Result<FileContinuityResult, SourceError> {
        if let Some(entry) = self.entries.get_mut(path) {
            if entry.is_tombstoned {
                // Path was previously tombstoned: path recycling!
                // §8.8: Never attach an old note to an unrelated new file simply because a path was recycled.
                let old_file_id = entry.file_id;
                let new_file_id = FileId::new(self.owner, self.next_file_id)?;
                self.next_file_id = self.next_file_id.saturating_add(1);
                let new_revision = SourceRevision::new(self.owner, 1)?;

                *entry = FileContinuityEntry {
                    file_id: new_file_id,
                    current_revision: new_revision,
                    inode,
                    digest,
                    is_tombstoned: false,
                    tombstone_epoch: None,
                };

                return Ok(FileContinuityResult::RecycledPathNewIdentity {
                    old_file_id,
                    new_file_id,
                    new_revision,
                });
            }

            // Path is active: check for atomic save or modification
            let prior_revision = entry.current_revision;
            let next_rev_val = prior_revision.get().saturating_add(1);
            let new_revision = SourceRevision::new(self.owner, next_rev_val)?;

            let is_atomic_save = match (entry.inode, inode) {
                (Some(old_in), Some(new_in)) => old_in != new_in,
                _ => false,
            };

            if is_atomic_save {
                let decision = match policy {
                    ContinuityPolicy::AutomaticAtomicSave => {
                        ContinuityDecision::PreserveLogicalIdentity
                    }
                    ContinuityPolicy::ExplicitPolicy(d) => d,
                };

                match decision {
                    ContinuityDecision::PreserveLogicalIdentity => {
                        let prior_inode = entry.inode;
                        let file_id = entry.file_id;
                        entry.current_revision = new_revision;
                        entry.inode = inode;
                        entry.digest = digest;

                        Ok(FileContinuityResult::PreservedAtomicSave {
                            file_id,
                            prior_revision,
                            new_revision,
                            prior_inode,
                            new_inode: inode.unwrap(),
                        })
                    }
                    ContinuityDecision::RecyclePathNewIdentity => {
                        let old_file_id = entry.file_id;
                        let new_file_id = FileId::new(self.owner, self.next_file_id)?;
                        self.next_file_id = self.next_file_id.saturating_add(1);
                        let fresh_rev = SourceRevision::new(self.owner, 1)?;

                        *entry = FileContinuityEntry {
                            file_id: new_file_id,
                            current_revision: fresh_rev,
                            inode,
                            digest,
                            is_tombstoned: false,
                            tombstone_epoch: None,
                        };

                        Ok(FileContinuityResult::RecycledPathNewIdentity {
                            old_file_id,
                            new_file_id,
                            new_revision: fresh_rev,
                        })
                    }
                }
            } else {
                let file_id = entry.file_id;
                entry.current_revision = new_revision;
                entry.inode = inode;
                entry.digest = digest;

                Ok(FileContinuityResult::InPlaceModified {
                    file_id,
                    prior_revision,
                    new_revision,
                })
            }
        } else {
            // Brand new file
            let file_id = FileId::new(self.owner, self.next_file_id)?;
            self.next_file_id = self.next_file_id.saturating_add(1);
            let revision = SourceRevision::new(self.owner, 1)?;

            self.entries.insert(
                path.clone(),
                FileContinuityEntry {
                    file_id,
                    current_revision: revision,
                    inode,
                    digest,
                    is_tombstoned: false,
                    tombstone_epoch: None,
                },
            );

            Ok(FileContinuityResult::NewFile { file_id, revision })
        }
    }

    /// Mark a file as tombstoned after verified reconciliation completion.
    pub fn mark_tombstoned(&mut self, path: &NormalizedPath, epoch: ScanEpoch) -> Option<FileId> {
        if let Some(entry) = self.entries.get_mut(path) {
            entry.is_tombstoned = true;
            entry.tombstone_epoch = Some(epoch);
            Some(entry.file_id)
        } else {
            None
        }
    }

    pub fn get_entry(&self, path: &NormalizedPath) -> Option<&FileContinuityEntry> {
        self.entries.get(path)
    }

    pub fn active_files_count(&self) -> usize {
        self.entries.values().filter(|e| !e.is_tombstoned).count()
    }

    pub fn tombstoned_files_count(&self) -> usize {
        self.entries.values().filter(|e| e.is_tombstoned).count()
    }
}

/// Stable identifier for a user annotation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AnnotationId(pub u64);

/// An annotation recorded against a specific file revision and byte anchor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAnnotation {
    pub id: AnnotationId,
    pub file_id: FileId,
    pub revision: SourceRevision,
    pub anchor: OldAnchor,
    pub expected_digest: u64,
    pub text: String,
}

impl SourceAnnotation {
    pub fn new(
        id: AnnotationId,
        file_id: FileId,
        revision: SourceRevision,
        anchor: OldAnchor,
        expected_digest: u64,
        text: String,
    ) -> Self {
        Self {
            id,
            file_id,
            revision,
            anchor,
            expected_digest,
            text,
        }
    }
}

/// Reason why an annotation could not reattach to the current file revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrphanReason {
    /// Content at the anchor byte offset diverged.
    ContentDiverged,
    /// Anchor offset exceeds the new file length.
    OffsetOutOfBounds,
    /// Path was recycled with a new identity; note cannot attach to an unrelated file (§8.8).
    PathRecycledNewIdentity,
    /// The owning file was tombstoned in a verified scan pass.
    FileTombstoned,
    /// Object became a FIFO, socket, device, or non-regular file.
    SpecialObject,
}

/// An orphaned annotation retained with its original source evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrphanedAnnotation {
    pub annotation: SourceAnnotation,
    pub reason: OrphanReason,
    pub orphaned_at_revision: Option<SourceRevision>,
}

/// Result of attempting to reattach an annotation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnnotationReattachment {
    Reattached {
        annotation_id: AnnotationId,
        file_id: FileId,
        new_revision: SourceRevision,
        new_anchor: OldAnchor,
    },
    Orphaned(OrphanedAnnotation),
}

/// Stores active and orphaned annotations, ensuring §8.8 continuity contracts.
#[derive(Clone, Debug, Default)]
pub struct AnnotationRegistry {
    active: BTreeMap<AnnotationId, SourceAnnotation>,
    orphaned: BTreeMap<AnnotationId, OrphanedAnnotation>,
}

impl AnnotationRegistry {
    pub fn new() -> Self {
        Self {
            active: BTreeMap::new(),
            orphaned: BTreeMap::new(),
        }
    }

    pub fn add_annotation(&mut self, annotation: SourceAnnotation) {
        self.active.insert(annotation.id, annotation);
    }

    pub fn get_active(&self, id: AnnotationId) -> Option<&SourceAnnotation> {
        self.active.get(&id)
    }

    pub fn get_orphaned(&self, id: AnnotationId) -> Option<&OrphanedAnnotation> {
        self.orphaned.get(&id)
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    pub fn orphaned_count(&self) -> usize {
        self.orphaned.len()
    }

    /// Reattach active annotations for a modified file.
    ///
    /// Validates the content at the anchor against the recorded digest.
    /// If content matches, reattaches to `new_revision`.
    /// Otherwise, preserves the annotation as orphaned with its original evidence intact.
    pub fn reattach_for_file_modification(
        &mut self,
        file_id: FileId,
        new_revision: SourceRevision,
        new_bytes: &[u8],
        anchor_len: usize,
    ) -> Vec<AnnotationReattachment> {
        let mut results = Vec::new();
        let target_ids: Vec<AnnotationId> = self
            .active
            .values()
            .filter(|a| a.file_id == file_id)
            .map(|a| a.id)
            .collect();

        for id in target_ids {
            let mut ann = self.active.remove(&id).expect("target id existed");
            let offset = ann.anchor.offset;

            let end = offset.saturating_add(anchor_len);
            let Some(slice) = new_bytes.get(offset..end) else {
                let orphaned = OrphanedAnnotation {
                    annotation: ann,
                    reason: OrphanReason::OffsetOutOfBounds,
                    orphaned_at_revision: Some(new_revision),
                };
                self.orphaned.insert(id, orphaned.clone());
                results.push(AnnotationReattachment::Orphaned(orphaned));
                continue;
            };

            let actual_digest = fnv1a(slice);

            if actual_digest == ann.expected_digest {
                let new_anchor = OldAnchor::new(offset, new_revision.get());
                ann.revision = new_revision;
                ann.anchor = new_anchor;
                self.active.insert(id, ann.clone());
                results.push(AnnotationReattachment::Reattached {
                    annotation_id: id,
                    file_id,
                    new_revision,
                    new_anchor,
                });
            } else {
                let orphaned = OrphanedAnnotation {
                    annotation: ann,
                    reason: OrphanReason::ContentDiverged,
                    orphaned_at_revision: Some(new_revision),
                };
                self.orphaned.insert(id, orphaned.clone());
                results.push(AnnotationReattachment::Orphaned(orphaned));
            }
        }

        results
    }

    /// Handle path recycling: when an old file is tombstoned and a new file appears at the same path.
    ///
    /// # Invariant (§8.8)
    /// Never attach an old note to an unrelated new file simply because a path was recycled.
    /// All notes on `old_file_id` are permanently orphaned with `PathRecycledNewIdentity`.
    pub fn handle_path_recycled(
        &mut self,
        old_file_id: FileId,
        _new_file_id: FileId,
    ) -> Vec<OrphanedAnnotation> {
        let mut orphaned_list = Vec::new();
        let target_ids: Vec<AnnotationId> = self
            .active
            .values()
            .filter(|a| a.file_id == old_file_id)
            .map(|a| a.id)
            .collect();

        for id in target_ids {
            let ann = self.active.remove(&id).expect("target id existed");
            let orphaned = OrphanedAnnotation {
                annotation: ann,
                reason: OrphanReason::PathRecycledNewIdentity,
                orphaned_at_revision: None,
            };
            self.orphaned.insert(id, orphaned.clone());
            orphaned_list.push(orphaned);
        }

        orphaned_list
    }

    /// Handle verified tombstone of a file.
    pub fn handle_file_tombstoned(&mut self, file_id: FileId) -> Vec<OrphanedAnnotation> {
        let mut orphaned_list = Vec::new();
        let target_ids: Vec<AnnotationId> = self
            .active
            .values()
            .filter(|a| a.file_id == file_id)
            .map(|a| a.id)
            .collect();

        for id in target_ids {
            let ann = self.active.remove(&id).expect("target id existed");
            let orphaned = OrphanedAnnotation {
                annotation: ann,
                reason: OrphanReason::FileTombstoned,
                orphaned_at_revision: None,
            };
            self.orphaned.insert(id, orphaned.clone());
            orphaned_list.push(orphaned);
        }

        orphaned_list
    }

    /// Handle a file becoming a FIFO, socket, or device.
    pub fn handle_file_became_special(&mut self, file_id: FileId) -> Vec<OrphanedAnnotation> {
        let mut orphaned_list = Vec::new();
        let target_ids: Vec<AnnotationId> = self
            .active
            .values()
            .filter(|a| a.file_id == file_id)
            .map(|a| a.id)
            .collect();

        for id in target_ids {
            let ann = self.active.remove(&id).expect("target id existed");
            let orphaned = OrphanedAnnotation {
                annotation: ann,
                reason: OrphanReason::SpecialObject,
                orphaned_at_revision: None,
            };
            self.orphaned.insert(id, orphaned.clone());
            orphaned_list.push(orphaned);
        }

        orphaned_list
    }

    pub fn all_active(&self) -> impl Iterator<Item = &SourceAnnotation> {
        self.active.values()
    }

    pub fn all_orphaned(&self) -> impl Iterator<Item = &OrphanedAnnotation> {
        self.orphaned.values()
    }
}

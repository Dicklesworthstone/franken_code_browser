#![forbid(unsafe_code)]

//! Bounded directory discovery and stable paged publication (FCB-010.A).
//!
//! Authorized iterative traversal with descriptor, depth, path, queue and page
//! caps. Metadata-only discovery performs no rule-file reads. The explicit
//! rule-aware route separately admits configuration I/O/state/work and loads
//! rules before enumerating each directory. Incomplete scans never tombstone
//! unseen entries. Path-based traversal is not a race-safe native sandbox.

use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, ReadDir};
use std::path::{Path, PathBuf};
use fcb_core::{ByteLength, ResourceAllocationId, ResourceBudget};
use crate::confined::{validate_not_special, ConfinedSourceReader, DirectoryId, SymlinkPolicy};
use crate::ignore::IgnoreMatcher;
use crate::ignore::repository::{RepositoryRules, RuleError, RuleLimits, RuleStats, read_rule_file};
use crate::path::NormalizedPath;
use crate::root::RootGrant;
use crate::{CancelFlag, SourceError};

const MAX_RULE_FILE_BYTES: usize = 64 * 1024;

/// Caps applied to one discovery session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryLimits {
    max_open_descriptors: u32,
    max_depth: u32,
    max_path_bytes: u32,
    max_batch_entries: u32,
    max_batch_bytes: u64,
    max_queue_entries: u32,
    max_alias_hops: u32,
}
impl DiscoveryLimits {
    pub fn new(max_open_descriptors: u32, max_depth: u32, max_path_bytes: u32,
        max_batch_entries: u32, max_batch_bytes: u64, max_queue_entries: u32) -> Result<Self, SourceError> {
        if max_open_descriptors == 0 || max_depth == 0 || max_path_bytes == 0
            || max_batch_entries == 0 || max_batch_bytes == 0 || max_queue_entries == 0 {
            return Err(SourceError::InvalidRange);
        }
        Ok(Self { max_open_descriptors, max_depth, max_path_bytes, max_batch_entries,
            max_batch_bytes, max_queue_entries, max_alias_hops: 16 })
    }
    pub fn modest() -> Self {
        Self { max_open_descriptors: 4, max_depth: 64, max_path_bytes: 4096,
            max_batch_entries: 256, max_batch_bytes: 64 * 1024, max_queue_entries: 4096, max_alias_hops: 16 }
    }
    pub fn max_open_descriptors(self) -> u32 { self.max_open_descriptors }
    pub fn max_depth(self) -> u32 { self.max_depth }
    pub fn max_path_bytes(self) -> u32 { self.max_path_bytes }
    pub fn max_batch_entries(self) -> u32 { self.max_batch_entries }
    pub fn max_batch_bytes(self) -> u64 { self.max_batch_bytes }
    pub fn max_queue_entries(self) -> u32 { self.max_queue_entries }
    pub fn max_alias_hops(self) -> u32 { self.max_alias_hops }
    pub fn with_max_alias_hops(mut self, hops: u32) -> Self { self.max_alias_hops = hops; self }
}

/// FCB-owned artifacts remain excluded independently of repository rules.
#[derive(Clone, Debug)]
pub struct OwnArtifactExclusion {
    excluded_dir_ids: BTreeSet<DirectoryId>,
    excluded_names: BTreeSet<String>,
}
impl Default for OwnArtifactExclusion { fn default() -> Self { Self::default_product() } }
impl OwnArtifactExclusion {
    pub fn default_product() -> Self {
        let mut me = Self::empty();
        for name in [".fcb-cache", ".fcb-store", ".fcb-scratch", ".fcb-recovery", ".fcb.db", ".fcb.db-wal", ".fcb.db-shm"] {
            me.add_excluded_name(name);
        }
        me
    }
    pub fn empty() -> Self { Self { excluded_dir_ids: BTreeSet::new(), excluded_names: BTreeSet::new() } }
    pub fn add_excluded_name(&mut self, name: &str) { self.excluded_names.insert(name.to_string()); }
    pub fn register_path(&mut self, path: &Path) -> Result<(), SourceError> {
        if let Ok(dir_id) = DirectoryId::from_path(path) { self.excluded_dir_ids.insert(dir_id); }
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) { self.excluded_names.insert(file_name.to_string()); }
        Ok(())
    }
    pub fn is_excluded(&self, path: &Path, dir_id: Option<DirectoryId>) -> bool {
        if dir_id.is_some_and(|id| self.excluded_dir_ids.contains(&id)) { return true; }
        path.file_name().and_then(|n| n.to_str()).is_some_and(|name| self.excluded_names.contains(name))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScanEpoch(u64);
impl ScanEpoch {
    pub const fn new(epoch: u64) -> Self { Self(epoch) }
    pub const fn get(self) -> u64 { self.0 }
    pub const fn next(self) -> Self { Self(self.0.saturating_add(1)) }
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChildOrderGeneration(u64);
impl ChildOrderGeneration { pub const fn get(self) -> u64 { self.0 } }
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DiscoveryKind { File, Directory, Symlink, Special, Unavailable, Cycle }

/// An observation is not a capture, deletion or persistent file identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryEntry {
    path: NormalizedPath, kind: DiscoveryKind, depth: u32,
    observed_len: Option<u64>, scan_epoch: ScanEpoch, excluded: bool,
}
impl DiscoveryEntry {
    pub fn path(&self) -> &NormalizedPath { &self.path }
    pub fn kind(&self) -> DiscoveryKind { self.kind }
    pub fn depth(&self) -> u32 { self.depth }
    pub fn observed_len(&self) -> Option<u64> { self.observed_len }
    pub fn scan_epoch(&self) -> ScanEpoch { self.scan_epoch }
    pub fn is_excluded(&self) -> bool { self.excluded }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationState { Provisional, Stable { generation: ChildOrderGeneration } }
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryAggregate {
    pub files: u64, pub directories: u64, pub symlinks: u64, pub special: u64,
    pub unavailable: u64, pub cycles: u64, pub depth_limited: u64,
    pub path_limited: u64, pub queue_refused: u64,
    /// Excluded/withheld namespace entries, never deletions. Consult rule stats
    /// separately for paths withheld because their policy could not be evaluated.
    pub excluded: u64,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryPeaks {
    pub open_descriptors: u32, pub queue_entries: u32, pub batch_entries: u32, pub batch_bytes: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncompleteReason { Canceled, GrantRevoked, QueueSaturated, RootUnavailable, RulePolicyUnavailable }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanStatus { InProgress, Completed { epoch: ScanEpoch }, Incomplete { reason: IncompleteReason } }
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryBatch {
    entries: Vec<DiscoveryEntry>, aggregate: DiscoveryAggregate,
    publication: PublicationState, more: bool, scan_epoch: ScanEpoch,
}
impl DiscoveryBatch {
    pub fn entries(&self) -> &[DiscoveryEntry] { &self.entries }
    pub fn aggregate(&self) -> DiscoveryAggregate { self.aggregate }
    pub fn publication(&self) -> PublicationState { self.publication }
    pub fn more(&self) -> bool { self.more }
    pub fn scan_epoch(&self) -> ScanEpoch { self.scan_epoch }
}
struct PendingDir { rel: Option<NormalizedPath>, depth: u32, alias_hops: u32, ancestry: Vec<DirectoryId> }
struct OpenDir {
    rel: Option<NormalizedPath>, depth: u32, alias_hops: u32, ancestry: Vec<DirectoryId>,
    resolved: PathBuf, iter: ReadDir, children_emitted: u64, split_across_batches: bool,
}

pub struct BoundedDiscovery {
    reader: ConfinedSourceReader,
    grant: RootGrant,
    limits: DiscoveryLimits,
    ignore: IgnoreMatcher,
    own_exclusions: OwnArtifactExclusion,
    read_rule_files: bool,
    repository_rules: Option<RepositoryRules>,
    pending: VecDeque<PendingDir>,
    open: VecDeque<OpenDir>,
    epoch: ScanEpoch,
    next_stable_generation: u64,
    aggregate: DiscoveryAggregate,
    peaks: DiscoveryPeaks,
    status: ScanStatus,
    queue_saturated: bool,
}
impl fmt::Debug for BoundedDiscovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundedDiscovery").field("epoch", &self.epoch).field("status", &self.status)
            .field("pending", &self.pending.len()).field("open", &self.open.len())
            .field("aggregate", &self.aggregate).field("peaks", &self.peaks).finish_non_exhaustive()
    }
}
impl BoundedDiscovery {
    pub fn open(grant: RootGrant, symlink_policy: SymlinkPolicy, limits: DiscoveryLimits) -> Result<Self, SourceError> {
        Self::open_with_ignore(grant, symlink_policy, limits, IgnoreMatcher::product_defaults())
    }
    /// Never reads rule files or payloads. The supplied matcher is the complete
    /// policy. The caller owns its admission and any precompiled host rules.
    pub fn open_metadata_only(grant: RootGrant, symlink_policy: SymlinkPolicy,
        limits: DiscoveryLimits, ignore: IgnoreMatcher) -> Result<Self, SourceError> {
        let mut discovery = Self::open_with_ignore(grant, symlink_policy, limits, ignore)?;
        discovery.read_rule_files = false;
        Ok(discovery)
    }
    /// Explicitly authorize nested rule-file reads under separate host capacity.
    /// No symlinks are followed. A bad/missing-evidence rule file withholds its
    /// containing subtree, but other directories can still produce useful pages.
    pub fn open_rule_aware(grant: RootGrant, limits: DiscoveryLimits, rules: RuleLimits,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, RuleError> {
        let policy = RepositoryRules::new(grant.owner(), rules, budget, allocation)?;
        let mut discovery = Self::open_metadata_only(grant, SymlinkPolicy::DisallowAll, limits, IgnoreMatcher::include_all())?;
        discovery.repository_rules = Some(policy);
        Ok(discovery)
    }
    pub fn reads_rule_files(&self) -> bool { self.read_rule_files || self.repository_rules.is_some() }
    pub fn repository_rules(&self) -> Option<&RepositoryRules> { self.repository_rules.as_ref() }
    /// End traversal and transfer its exact policy evidence without copying.
    /// Pending queues and directory descriptors are retired here on the worker;
    /// the policy's own admission lease follows the returned value. This method
    /// makes no completeness claim; preserve status/aggregate before consuming.
    pub fn into_repository_rules(self) -> Option<RepositoryRules> { self.repository_rules }
    /// Compatibility route. Individual reads are bounded; use open_rule_aware
    /// for session-wide configuration allocation, work, and failure reporting.
    pub fn open_with_ignore(grant: RootGrant, symlink_policy: SymlinkPolicy,
        limits: DiscoveryLimits, ignore: IgnoreMatcher) -> Result<Self, SourceError> {
        grant.validate_active()?;
        let reader = ConfinedSourceReader::new(grant.clone(), symlink_policy, ByteLength::new(u64::MAX));
        reader.canonical_root_path()?;
        let mut session = Self { reader, grant, limits, ignore,
            own_exclusions: OwnArtifactExclusion::default_product(), read_rule_files: true,
            repository_rules: None, pending: VecDeque::new(), open: VecDeque::new(), epoch: ScanEpoch(1),
            next_stable_generation: 1, aggregate: DiscoveryAggregate::default(), peaks: DiscoveryPeaks::default(),
            status: ScanStatus::InProgress, queue_saturated: false };
        session.pending.push_back(PendingDir { rel: None, depth: 0, alias_hops: 0, ancestry: Vec::new() });
        session.note_queue_peak();
        Ok(session)
    }
    pub fn own_exclusions(&self) -> &OwnArtifactExclusion { &self.own_exclusions }
    pub fn own_exclusions_mut(&mut self) -> &mut OwnArtifactExclusion { &mut self.own_exclusions }
    pub fn register_own_artifact_path(&mut self, path: &Path) -> Result<(), SourceError> { self.own_exclusions.register_path(path) }
    pub fn limits(&self) -> DiscoveryLimits { self.limits }
    pub fn scan_epoch(&self) -> ScanEpoch { self.epoch }
    pub fn status(&self) -> ScanStatus { self.status }
    pub fn aggregate(&self) -> DiscoveryAggregate { self.aggregate }
    pub fn peaks(&self) -> DiscoveryPeaks { self.peaks }
    /// This is the host/legacy matcher, not the private admitted repository policy.
    pub fn ignore(&self) -> &IgnoreMatcher { &self.ignore }
    pub fn ignore_mut(&mut self) -> &mut IgnoreMatcher { &mut self.ignore }
    pub fn is_complete(&self) -> bool { matches!(self.status, ScanStatus::Completed { .. }) }

    /// A page can be empty with more=true. Rejected names and empty directories
    /// consume traversal transitions independently of output; at most four
    /// transitions per configured batch-entry slot. Rule-aware opens add at most
    /// two bounded rule reads per newly opened directory; no UI-latency claim.
    pub fn next_batch(&mut self, cancel: &CancelFlag) -> Result<Option<DiscoveryBatch>, SourceError> {
        match self.ensure_runnable(cancel) {
            Ok(()) => {},
            Err(SourceError::Canceled | SourceError::GrantRevoked) => return Err(self.halt_error()),
            Err(error) => return Err(error),
        }
        if self.queue_empty() { self.finalize_status(); return Ok(None); }
        let mut entries = Vec::new();
        let mut batch_bytes = 0u64;
        let mut complete_unsplit_dirs = 0u32;
        let mut any_split = false;
        let mut transitions = 0u64;
        let max_transitions = u64::from(self.limits.max_batch_entries) * 4;
        loop {
            match self.ensure_runnable(cancel) {
                Ok(()) => {},
                Err(SourceError::Canceled | SourceError::GrantRevoked) => {
                    if entries.is_empty() { return Err(self.halt_error()); }
                    break;
                }
                Err(error) => return Err(error),
            }
            if self.batch_full(&entries, batch_bytes) || transitions == max_transitions {
                if self.open.front().is_some_and(|dir| dir.children_emitted > 0) {
                    if let Some(open) = self.open.front_mut() { open.split_across_batches = true; }
                    any_split = true;
                }
                if transitions == max_transitions { any_split = true; }
                break;
            }
            transitions += 1;
            if !self.open.is_empty() {
                match self.advance_open_dir(&mut entries, &mut batch_bytes, &mut any_split) {
                    Advance::FinishedUnsplit => complete_unsplit_dirs = complete_unsplit_dirs.saturating_add(1),
                    Advance::Continue => {},
                }
                continue;
            }
            if self.open.len() < self.limits.max_open_descriptors as usize
                && let Some(pending) = self.pending.pop_front() {
                match self.open_pending(pending, cancel) {
                    OpenResult::Opened => {},
                    OpenResult::SkippedCycle(entry) => {
                        batch_bytes = batch_bytes.saturating_add(entry.path.as_bytes().len() as u64);
                        entries.push(entry);
                    }
                    OpenResult::SkippedUnavailable => {},
                }
                continue;
            }
            break;
        }
        if entries.is_empty() && self.queue_empty() { self.finalize_status(); return Ok(None); }
        entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
        self.peaks.batch_entries = self.peaks.batch_entries.max(entries.len() as u32);
        self.peaks.batch_bytes = self.peaks.batch_bytes.max(batch_bytes);
        let publication = if !any_split && complete_unsplit_dirs > 0 && self.open.iter().all(|dir| dir.children_emitted == 0) {
            let generation = ChildOrderGeneration(self.next_stable_generation);
            self.next_stable_generation = self.next_stable_generation.saturating_add(1);
            PublicationState::Stable { generation }
        } else { PublicationState::Provisional };
        let more = !self.queue_empty();
        if !more { self.finalize_status(); }
        Ok(Some(DiscoveryBatch { entries, aggregate: self.aggregate, publication, more, scan_epoch: self.epoch }))
    }
    fn ensure_runnable(&mut self, cancel: &CancelFlag) -> Result<(), SourceError> {
        if cancel.is_canceled() {
            self.status = ScanStatus::Incomplete { reason: IncompleteReason::Canceled };
            return Err(SourceError::Canceled);
        }
        if let Err(error) = self.grant.validate_active() {
            self.status = ScanStatus::Incomplete { reason: IncompleteReason::GrantRevoked };
            return Err(error);
        }
        Ok(())
    }
    fn halt_error(&self) -> SourceError {
        match self.status {
            ScanStatus::Incomplete { reason: IncompleteReason::Canceled } => SourceError::Canceled,
            _ => SourceError::GrantRevoked,
        }
    }
    fn queue_empty(&self) -> bool { self.pending.is_empty() && self.open.is_empty() }
    fn batch_full(&self, entries: &[DiscoveryEntry], bytes: u64) -> bool {
        entries.len() >= self.limits.max_batch_entries as usize || bytes >= self.limits.max_batch_bytes
    }
    fn finalize_status(&mut self) {
        if matches!(self.status, ScanStatus::Incomplete { .. } | ScanStatus::Completed { .. }) { return; }
        if self.queue_saturated {
            self.status = ScanStatus::Incomplete { reason: IncompleteReason::QueueSaturated };
        } else if self.queue_empty() {
            self.status = if self.repository_rules.as_ref().is_some_and(|rules| !rules.stats().is_complete()) {
                ScanStatus::Incomplete { reason: IncompleteReason::RulePolicyUnavailable }
            } else { ScanStatus::Completed { epoch: self.epoch } };
        }
    }
    fn open_pending(&mut self, pending: PendingDir, cancel: &CancelFlag) -> OpenResult {
        let canonical_root = match self.reader.canonical_root_path() {
            Ok(path) => path,
            Err(_) => {
                self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
                self.status = ScanStatus::Incomplete { reason: IncompleteReason::RootUnavailable };
                return OpenResult::SkippedUnavailable;
            }
        };
        let resolved = match &pending.rel {
            None => canonical_root.clone(),
            Some(rel) => match self.reader.resolve_confined_path(&canonical_root, rel) {
                Ok(path) => path,
                Err(SourceError::TraversalCycle) => {
                    if let Some(rel) = pending.rel {
                        return OpenResult::SkippedCycle(self.push_kind(rel, DiscoveryKind::Cycle, pending.depth, None, false));
                    }
                    self.aggregate.cycles = self.aggregate.cycles.saturating_add(1);
                    return OpenResult::SkippedUnavailable;
                }
                Err(_) => { self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1); return OpenResult::SkippedUnavailable; }
            }
        };
        let meta = match fs::symlink_metadata(&resolved) {
            Ok(meta) => meta,
            Err(_) => { self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1); return OpenResult::SkippedUnavailable; }
        };
        if validate_not_special(&meta).is_err() {
            self.aggregate.special = self.aggregate.special.saturating_add(1); return OpenResult::SkippedUnavailable;
        }
        if !meta.is_dir() { self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1); return OpenResult::SkippedUnavailable; }
        let dir_id = match DirectoryId::from_path(&resolved) {
            Ok(id) => id,
            Err(_) => { self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1); return OpenResult::SkippedUnavailable; }
        };
        if self.own_exclusions.is_excluded(&resolved, Some(dir_id)) {
            self.aggregate.excluded = self.aggregate.excluded.saturating_add(1); return OpenResult::SkippedUnavailable;
        }
        if pending.ancestry.contains(&dir_id) {
            if let Some(rel) = pending.rel {
                return OpenResult::SkippedCycle(self.push_kind(rel, DiscoveryKind::Cycle, pending.depth, None, false));
            }
            self.aggregate.cycles = self.aggregate.cycles.saturating_add(1); return OpenResult::SkippedUnavailable;
        }
        // Load before opening the directory iterator: at most one rule descriptor
        // and no directory descriptor overlap in this explicit rule-aware route.
        if let Some(policy) = self.repository_rules.as_mut() {
            if policy.load_directory(pending.rel.as_ref(), &resolved, &self.grant, cancel).is_err() {
                self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
                return OpenResult::SkippedUnavailable;
            }
        } else if self.read_rule_files { self.load_rule_files(pending.rel.as_ref(), &resolved, cancel); }
        let iter = match fs::read_dir(&resolved) {
            Ok(iter) => iter,
            Err(_) => { self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1); return OpenResult::SkippedUnavailable; }
        };
        let mut ancestry = pending.ancestry; ancestry.push(dir_id);
        self.open.push_back(OpenDir { rel: pending.rel, depth: pending.depth, alias_hops: pending.alias_hops,
            ancestry, resolved, iter, children_emitted: 0, split_across_batches: false });
        self.note_descriptor_peak(); self.note_queue_peak(); OpenResult::Opened
    }
    fn advance_open_dir(&mut self, entries: &mut Vec<DiscoveryEntry>, batch_bytes: &mut u64, any_split: &mut bool) -> Advance {
        let next = self.open.front_mut().and_then(|open| open.iter.next());
        match next {
            None => {
                let finished = self.open.pop_front().expect("open dir existed"); self.note_descriptor_peak();
                if finished.children_emitted > 0 && !finished.split_across_batches { Advance::FinishedUnsplit }
                else { if finished.split_across_batches { *any_split = true; } Advance::Continue }
            }
            Some(Err(_)) => {
                self.open.pop_front(); self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
                *any_split = true; self.note_descriptor_peak(); Advance::Continue
            }
            Some(Ok(dirent)) => {
                let snapshot = {
                    let open = self.open.front().expect("open dir existed");
                    OpenSnapshot { rel: open.rel.clone(), depth: open.depth, alias_hops: open.alias_hops,
                        ancestry: open.ancestry.clone(), resolved: open.resolved.clone() }
                };
                let child_fs = snapshot.resolved.join(dirent.file_name());
                if let Some(entry) = self.observe_dirent(&snapshot, dirent.file_name().as_os_str(), &child_fs) {
                    *batch_bytes = batch_bytes.saturating_add(entry.path.as_bytes().len() as u64);
                    entries.push(entry);
                    if let Some(open) = self.open.front_mut() {
                        if open.children_emitted > 0 && (entries.len() >= self.limits.max_batch_entries as usize
                            || *batch_bytes >= self.limits.max_batch_bytes) {
                            open.split_across_batches = true; *any_split = true;
                        }
                        open.children_emitted = open.children_emitted.saturating_add(1);
                    }
                }
                Advance::Continue
            }
        }
    }
    fn observe_dirent(&mut self, open: &OpenSnapshot, name: &OsStr, child_fs_path: &Path) -> Option<DiscoveryEntry> {
        if name == OsStr::new(".") || name == OsStr::new("..") { return None; }
        let child_path = match child_relative(open.rel.as_ref(), os_name_bytes(name)) {
            Ok(path) => path,
            Err(_) => { self.aggregate.path_limited = self.aggregate.path_limited.saturating_add(1); return None; }
        };
        if child_path.as_bytes().len() as u32 > self.limits.max_path_bytes {
            self.aggregate.path_limited = self.aggregate.path_limited.saturating_add(1); return None;
        }
        let depth = open.depth.saturating_add(1);
        let meta = match fs::symlink_metadata(child_fs_path) {
            Ok(meta) => meta,
            Err(_) => return Some(self.push_kind(child_path, DiscoveryKind::Unavailable, depth, None, false)),
        };
        if validate_not_special(&meta).is_err() {
            return Some(self.push_kind(child_path, DiscoveryKind::Special, depth, None, false));
        }
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            let excluded = self.classify_exclusion(&child_path, false) || self.own_exclusions.is_excluded(child_fs_path, None);
            if !excluded { self.maybe_follow_symlink_dir(open, &child_path, depth, child_fs_path); }
            return Some(self.push_kind(child_path, DiscoveryKind::Symlink, depth, None, excluded));
        }
        if file_type.is_dir() {
            let excluded = self.classify_exclusion(&child_path, true) || self.own_exclusions.is_excluded(child_fs_path, None);
            if !excluded { self.maybe_enqueue_dir(open, child_path.clone(), depth); }
            return Some(self.push_kind(child_path, DiscoveryKind::Directory, depth, None, excluded));
        }
        if file_type.is_file() {
            let excluded = self.classify_exclusion(&child_path, false) || self.own_exclusions.is_excluded(child_fs_path, None);
            return Some(self.push_kind(child_path, DiscoveryKind::File, depth, Some(meta.len()), excluded));
        }
        Some(self.push_kind(child_path, DiscoveryKind::Special, depth, None, false))
    }
    fn maybe_follow_symlink_dir(&mut self, open: &OpenSnapshot, child_path: &NormalizedPath, depth: u32, child_fs_path: &Path) {
        if self.reader.symlink_policy() != SymlinkPolicy::AllowWithinRoot { return; }
        let target = match fs::metadata(child_fs_path) { Ok(meta) => meta, Err(_) => return };
        if target.is_dir() {
            let next_alias = open.alias_hops.saturating_add(1);
            if next_alias > self.limits.max_alias_hops { self.aggregate.cycles = self.aggregate.cycles.saturating_add(1); return; }
            self.maybe_enqueue_dir_with_alias(open, child_path.clone(), depth, next_alias);
        }
    }
    fn maybe_enqueue_dir(&mut self, open: &OpenSnapshot, child_path: NormalizedPath, depth: u32) {
        self.maybe_enqueue_dir_with_alias(open, child_path, depth, open.alias_hops);
    }
    fn maybe_enqueue_dir_with_alias(&mut self, open: &OpenSnapshot, child_path: NormalizedPath, depth: u32, alias_hops: u32) {
        if depth >= self.limits.max_depth { self.aggregate.depth_limited = self.aggregate.depth_limited.saturating_add(1); return; }
        let queued = (self.pending.len() + self.open.len()) as u32;
        if queued >= self.limits.max_queue_entries {
            self.queue_saturated = true; self.aggregate.queue_refused = self.aggregate.queue_refused.saturating_add(1); return;
        }
        self.pending.push_back(PendingDir { rel: Some(child_path), depth, alias_hops, ancestry: open.ancestry.clone() });
        self.note_queue_peak();
    }
    fn classify_exclusion(&mut self, path: &NormalizedPath, is_dir: bool) -> bool {
        if let Some(policy) = self.repository_rules.as_mut() {
            match policy.excluded(path, is_dir) {
                Ok(excluded) => excluded,
                Err(_) => { self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1); true }
            }
        } else { self.ignore.decide(path, is_dir).is_excluded() }
    }
    fn load_rule_files(&mut self, dir_rel: Option<&NormalizedPath>, resolved: &Path, cancel: &CancelFlag) {
        for name in [".gitignore", ".fcbignore"] {
            let mut stats = RuleStats::default();
            let Ok(Some(bytes)) = read_rule_file(&resolved.join(name), MAX_RULE_FILE_BYTES,
                MAX_RULE_FILE_BYTES as u64 + 1, 65_536, &self.grant, cancel, &mut stats) else { continue; };
            let Ok(text) = std::str::from_utf8(&bytes) else { continue; };
            self.ignore.add_rule_file(dir_rel, text);
        }
    }
    fn push_kind(&mut self, path: NormalizedPath, kind: DiscoveryKind, depth: u32,
        observed_len: Option<u64>, excluded: bool) -> DiscoveryEntry {
        match kind {
            DiscoveryKind::File => self.aggregate.files = self.aggregate.files.saturating_add(1),
            DiscoveryKind::Directory => self.aggregate.directories = self.aggregate.directories.saturating_add(1),
            DiscoveryKind::Symlink => self.aggregate.symlinks = self.aggregate.symlinks.saturating_add(1),
            DiscoveryKind::Special => self.aggregate.special = self.aggregate.special.saturating_add(1),
            DiscoveryKind::Unavailable => self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1),
            DiscoveryKind::Cycle => self.aggregate.cycles = self.aggregate.cycles.saturating_add(1),
        }
        if excluded { self.aggregate.excluded = self.aggregate.excluded.saturating_add(1); }
        DiscoveryEntry { path, kind, depth, observed_len, scan_epoch: self.epoch, excluded }
    }
    fn note_descriptor_peak(&mut self) {
        self.peaks.open_descriptors = self.peaks.open_descriptors.max(self.open.len() as u32); self.note_queue_peak();
    }
    fn note_queue_peak(&mut self) {
        self.peaks.queue_entries = self.peaks.queue_entries.max((self.pending.len() + self.open.len()) as u32);
    }
}
enum OpenResult { Opened, SkippedCycle(DiscoveryEntry), SkippedUnavailable }
enum Advance { FinishedUnsplit, Continue }
struct OpenSnapshot { rel: Option<NormalizedPath>, depth: u32, alias_hops: u32, ancestry: Vec<DirectoryId>, resolved: PathBuf }
fn child_relative(parent: Option<&NormalizedPath>, name: &[u8]) -> Result<NormalizedPath, SourceError> {
    match parent { None => NormalizedPath::from_dirent_name(name), Some(parent) => parent.join_segment(name) }
}
#[cfg(unix)]
fn os_name_bytes(name: &OsStr) -> &[u8] { use std::os::unix::ffi::OsStrExt; name.as_bytes() }
#[cfg(not(unix))]
fn os_name_bytes(name: &OsStr) -> &[u8] { name.to_str().unwrap_or("").as_bytes() }

#![forbid(unsafe_code)]

//! Bounded directory discovery and stable paged publication (FCB-010.A).
//!
//! Walks an authorized [`RootGrant`] with an explicit work queue rather than
//! recursive call-stack traversal. Each batch is descriptor-, depth-, path-,
//! entry-, and byte-capped. A page is sorted only after it is gathered;
//! oversized directories stay provisional rather than allocating a whole
//! million-file listing. Incomplete scans never tombstone unseen entries.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, ReadDir};
use std::path::PathBuf;

use fcb_core::ByteLength;

use crate::confined::{
    validate_not_special, ConfinedSourceReader, DirectoryId, SymlinkPolicy,
};
use crate::ignore::IgnoreMatcher;
use crate::path::NormalizedPath;
use crate::root::RootGrant;
use crate::{CancelFlag, SourceError};

/// Maximum bytes of one nested `.gitignore` / `.fcbignore` that discovery will
/// parse. Larger files are skipped rather than allocating an unbounded rule set.
const MAX_RULE_FILE_BYTES: u64 = 64 * 1024;

/// Caps applied to one discovery session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryLimits {
    max_open_descriptors: u32,
    max_depth: u32,
    max_path_bytes: u32,
    max_batch_entries: u32,
    max_batch_bytes: u64,
    max_queue_entries: u32,
}

impl DiscoveryLimits {
    pub fn new(
        max_open_descriptors: u32,
        max_depth: u32,
        max_path_bytes: u32,
        max_batch_entries: u32,
        max_batch_bytes: u64,
        max_queue_entries: u32,
    ) -> Result<Self, SourceError> {
        if max_open_descriptors == 0
            || max_depth == 0
            || max_path_bytes == 0
            || max_batch_entries == 0
            || max_batch_bytes == 0
            || max_queue_entries == 0
        {
            return Err(SourceError::InvalidRange);
        }
        Ok(Self {
            max_open_descriptors,
            max_depth,
            max_path_bytes,
            max_batch_entries,
            max_batch_bytes,
            max_queue_entries,
        })
    }

    pub fn modest() -> Self {
        Self {
            max_open_descriptors: 4,
            max_depth: 64,
            max_path_bytes: 4096,
            max_batch_entries: 256,
            max_batch_bytes: 64 * 1024,
            max_queue_entries: 4096,
        }
    }

    pub fn max_open_descriptors(self) -> u32 {
        self.max_open_descriptors
    }

    pub fn max_depth(self) -> u32 {
        self.max_depth
    }

    pub fn max_path_bytes(self) -> u32 {
        self.max_path_bytes
    }

    pub fn max_batch_entries(self) -> u32 {
        self.max_batch_entries
    }

    pub fn max_batch_bytes(self) -> u64 {
        self.max_batch_bytes
    }

    pub fn max_queue_entries(self) -> u32 {
        self.max_queue_entries
    }
}

/// Identity of one discovery pass over a root.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScanEpoch(u64);

impl ScanEpoch {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Generation assigned when a directory's complete child set is sorted in one page.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChildOrderGeneration(u64);

impl ChildOrderGeneration {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Kind of one published discovery observation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DiscoveryKind {
    File,
    Directory,
    Symlink,
    Special,
    Unavailable,
    Cycle,
}

/// One observed namespace entry. This is not a capture and not a deletion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryEntry {
    path: NormalizedPath,
    kind: DiscoveryKind,
    depth: u32,
    observed_len: Option<u64>,
    scan_epoch: ScanEpoch,
    excluded: bool,
}

impl DiscoveryEntry {
    pub fn path(&self) -> &NormalizedPath {
        &self.path
    }

    pub fn kind(&self) -> DiscoveryKind {
        self.kind
    }

    pub fn depth(&self) -> u32 {
        self.depth
    }

    pub fn observed_len(&self) -> Option<u64> {
        self.observed_len
    }

    pub fn scan_epoch(&self) -> ScanEpoch {
        self.scan_epoch
    }

    /// Whether ignore matching classified this observation as excluded.
    /// Exclusion is not a deletion or tombstone.
    pub fn is_excluded(&self) -> bool {
        self.excluded
    }
}

/// Whether a published page can be treated as a frozen child order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationState {
    /// Enumeration is still partial, split across pages, or budget-limited.
    Provisional,
    /// Every directory contributing to this page finished inside the page and
    /// its children were sorted as a complete set.
    Stable {
        generation: ChildOrderGeneration,
    },
}

/// Cumulative counters for one session. Unseen entries are not counted as deleted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryAggregate {
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
    pub special: u64,
    pub unavailable: u64,
    pub cycles: u64,
    pub depth_limited: u64,
    pub path_limited: u64,
    pub queue_refused: u64,
    /// Observed entries classified as excluded. Not a deletion count.
    pub excluded: u64,
}

/// Observed peaks; used to prove descriptor and queue caps held.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryPeaks {
    pub open_descriptors: u32,
    pub queue_entries: u32,
    pub batch_entries: u32,
    pub batch_bytes: u64,
}

/// Why a scan stopped short of visiting every admitted directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncompleteReason {
    Canceled,
    GrantRevoked,
    QueueSaturated,
    RootUnavailable,
}

/// Terminal or in-flight status of one session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanStatus {
    InProgress,
    Completed { epoch: ScanEpoch },
    Incomplete { reason: IncompleteReason },
}

/// One bounded publication of discovery observations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryBatch {
    entries: Vec<DiscoveryEntry>,
    aggregate: DiscoveryAggregate,
    publication: PublicationState,
    more: bool,
    scan_epoch: ScanEpoch,
}

impl DiscoveryBatch {
    pub fn entries(&self) -> &[DiscoveryEntry] {
        &self.entries
    }

    pub fn aggregate(&self) -> DiscoveryAggregate {
        self.aggregate
    }

    pub fn publication(&self) -> PublicationState {
        self.publication
    }

    pub fn more(&self) -> bool {
        self.more
    }

    pub fn scan_epoch(&self) -> ScanEpoch {
        self.scan_epoch
    }
}

struct PendingDir {
    rel: Option<NormalizedPath>,
    depth: u32,
    ancestry: Vec<DirectoryId>,
}

struct OpenDir {
    rel: Option<NormalizedPath>,
    depth: u32,
    ancestry: Vec<DirectoryId>,
    resolved: PathBuf,
    iter: ReadDir,
    children_emitted: u64,
    split_across_batches: bool,
}

/// Iterative, grant-scoped directory walker.
pub struct BoundedDiscovery {
    reader: ConfinedSourceReader,
    grant: RootGrant,
    limits: DiscoveryLimits,
    ignore: IgnoreMatcher,
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
        f.debug_struct("BoundedDiscovery")
            .field("epoch", &self.epoch)
            .field("status", &self.status)
            .field("pending", &self.pending.len())
            .field("open", &self.open.len())
            .field("aggregate", &self.aggregate)
            .field("peaks", &self.peaks)
            .finish_non_exhaustive()
    }
}

impl BoundedDiscovery {
    pub fn open(
        grant: RootGrant,
        symlink_policy: SymlinkPolicy,
        limits: DiscoveryLimits,
    ) -> Result<Self, SourceError> {
        Self::open_with_ignore(
            grant,
            symlink_policy,
            limits,
            IgnoreMatcher::product_defaults(),
        )
    }

    /// Same as [`Self::open`], with an explicit ignore matcher. Pass
    /// [`IgnoreMatcher::include_all`] to disable default exclusions.
    pub fn open_with_ignore(
        grant: RootGrant,
        symlink_policy: SymlinkPolicy,
        limits: DiscoveryLimits,
        ignore: IgnoreMatcher,
    ) -> Result<Self, SourceError> {
        grant.validate_active()?;
        let reader = ConfinedSourceReader::new(grant.clone(), symlink_policy, ByteLength::new(u64::MAX));
        reader.canonical_root_path()?;
        let mut session = Self {
            reader,
            grant,
            limits,
            ignore,
            pending: VecDeque::new(),
            open: VecDeque::new(),
            epoch: ScanEpoch(1),
            next_stable_generation: 1,
            aggregate: DiscoveryAggregate::default(),
            peaks: DiscoveryPeaks::default(),
            status: ScanStatus::InProgress,
            queue_saturated: false,
        };
        session.pending.push_back(PendingDir {
            rel: None,
            depth: 0,
            ancestry: Vec::new(),
        });
        session.note_queue_peak();
        Ok(session)
    }

    pub fn limits(&self) -> DiscoveryLimits {
        self.limits
    }

    pub fn scan_epoch(&self) -> ScanEpoch {
        self.epoch
    }

    pub fn status(&self) -> ScanStatus {
        self.status
    }

    pub fn aggregate(&self) -> DiscoveryAggregate {
        self.aggregate
    }

    pub fn peaks(&self) -> DiscoveryPeaks {
        self.peaks
    }

    pub fn ignore(&self) -> &IgnoreMatcher {
        &self.ignore
    }

    pub fn ignore_mut(&mut self) -> &mut IgnoreMatcher {
        &mut self.ignore
    }

    pub fn is_complete(&self) -> bool {
        matches!(self.status, ScanStatus::Completed { .. })
    }

    /// Drain the next bounded page. `Ok(None)` means the session has no further
    /// work; inspect [`Self::status`] for completion versus incomplete stop.
    pub fn next_batch(
        &mut self,
        cancel: &CancelFlag,
    ) -> Result<Option<DiscoveryBatch>, SourceError> {
        match self.ensure_runnable(cancel) {
            Ok(()) => {}
            Err(SourceError::Canceled) | Err(SourceError::GrantRevoked) => {
                return Err(self.halt_error());
            }
            Err(err) => return Err(err),
        }

        if self.queue_empty() {
            self.finalize_status();
            return Ok(None);
        }

        let mut entries: Vec<DiscoveryEntry> = Vec::new();
        let mut batch_bytes: u64 = 0;
        let mut complete_unsplit_dirs: u32 = 0;
        let mut any_split = false;

        loop {
            match self.ensure_runnable(cancel) {
                Ok(()) => {}
                Err(SourceError::Canceled) | Err(SourceError::GrantRevoked) => {
                    if entries.is_empty() {
                        return Err(self.halt_error());
                    }
                    break;
                }
                Err(err) => return Err(err),
            }

            if self.batch_full(&entries, batch_bytes) {
                if self.open.front().is_some_and(|dir| dir.children_emitted > 0) {
                    if let Some(open) = self.open.front_mut() {
                        open.split_across_batches = true;
                    }
                    any_split = true;
                }
                break;
            }

            if !self.open.is_empty() {
                match self.advance_open_dir(&mut entries, &mut batch_bytes, &mut any_split) {
                    Advance::FinishedUnsplit => {
                        complete_unsplit_dirs = complete_unsplit_dirs.saturating_add(1);
                    }
                    Advance::Continue => {}
                }
                continue;
            }

            if self.open.len() < self.limits.max_open_descriptors as usize
                && let Some(pending) = self.pending.pop_front()
            {
                match self.open_pending(pending) {
                    OpenResult::Opened => {}
                    OpenResult::SkippedCycle(entry) => {
                        batch_bytes = batch_bytes.saturating_add(entry.path.as_bytes().len() as u64);
                        entries.push(entry);
                    }
                    OpenResult::SkippedUnavailable => {}
                }
                continue;
            }

            break;
        }

        if entries.is_empty() {
            if self.queue_empty() {
                self.finalize_status();
                return Ok(None);
            }
            if self.open.is_empty() {
                self.finalize_status();
                return Ok(None);
            }
        }

        entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
        self.peaks.batch_entries = self.peaks.batch_entries.max(entries.len() as u32);
        self.peaks.batch_bytes = self.peaks.batch_bytes.max(batch_bytes);

        let publication = if !any_split
            && complete_unsplit_dirs > 0
            && self.open.iter().all(|dir| dir.children_emitted == 0)
        {
            let generation = ChildOrderGeneration(self.next_stable_generation);
            self.next_stable_generation = self.next_stable_generation.saturating_add(1);
            PublicationState::Stable { generation }
        } else {
            PublicationState::Provisional
        };

        let more = !self.queue_empty();
        if !more {
            self.finalize_status();
        }

        Ok(Some(DiscoveryBatch {
            entries,
            aggregate: self.aggregate,
            publication,
            more,
            scan_epoch: self.epoch,
        }))
    }

    fn ensure_runnable(&mut self, cancel: &CancelFlag) -> Result<(), SourceError> {
        if cancel.is_canceled() {
            self.status = ScanStatus::Incomplete {
                reason: IncompleteReason::Canceled,
            };
            return Err(SourceError::Canceled);
        }
        if let Err(err) = self.grant.validate_active() {
            self.status = ScanStatus::Incomplete {
                reason: IncompleteReason::GrantRevoked,
            };
            return Err(err);
        }
        Ok(())
    }

    fn halt_error(&self) -> SourceError {
        match self.status {
            ScanStatus::Incomplete {
                reason: IncompleteReason::Canceled,
            } => SourceError::Canceled,
            _ => SourceError::GrantRevoked,
        }
    }

    fn queue_empty(&self) -> bool {
        self.pending.is_empty() && self.open.is_empty()
    }

    fn batch_full(&self, entries: &[DiscoveryEntry], batch_bytes: u64) -> bool {
        entries.len() >= self.limits.max_batch_entries as usize
            || batch_bytes >= self.limits.max_batch_bytes
    }

    fn finalize_status(&mut self) {
        if matches!(
            self.status,
            ScanStatus::Incomplete { .. } | ScanStatus::Completed { .. }
        ) {
            return;
        }
        if self.queue_saturated {
            self.status = ScanStatus::Incomplete {
                reason: IncompleteReason::QueueSaturated,
            };
        } else if self.queue_empty() {
            self.status = ScanStatus::Completed { epoch: self.epoch };
        }
    }

    fn open_pending(&mut self, pending: PendingDir) -> OpenResult {
        let canonical_root = match self.reader.canonical_root_path() {
            Ok(path) => path,
            Err(_) => {
                self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
                self.status = ScanStatus::Incomplete {
                    reason: IncompleteReason::RootUnavailable,
                };
                return OpenResult::SkippedUnavailable;
            }
        };

        let resolved = match &pending.rel {
            None => canonical_root.clone(),
            Some(rel) => match self.reader.resolve_confined_path(&canonical_root, rel) {
                Ok(path) => path,
                Err(SourceError::TraversalCycle) => {
                    if let Some(rel) = pending.rel {
                        let entry = self.push_kind(rel, DiscoveryKind::Cycle, pending.depth, None, false);
                        return OpenResult::SkippedCycle(entry);
                    }
                    self.aggregate.cycles = self.aggregate.cycles.saturating_add(1);
                    return OpenResult::SkippedUnavailable;
                }
                Err(_) => {
                    self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
                    return OpenResult::SkippedUnavailable;
                }
            },
        };

        let meta = match fs::symlink_metadata(&resolved) {
            Ok(meta) => meta,
            Err(_) => {
                self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
                return OpenResult::SkippedUnavailable;
            }
        };
        if validate_not_special(&meta).is_err() {
            self.aggregate.special = self.aggregate.special.saturating_add(1);
            return OpenResult::SkippedUnavailable;
        }
        if !meta.is_dir() {
            self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
            return OpenResult::SkippedUnavailable;
        }

        let dir_id = match DirectoryId::from_path(&resolved) {
            Ok(id) => id,
            Err(_) => {
                self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
                return OpenResult::SkippedUnavailable;
            }
        };
        if pending.ancestry.contains(&dir_id) {
            if let Some(rel) = pending.rel {
                let entry =
                    self.push_kind(rel, DiscoveryKind::Cycle, pending.depth, None, false);
                return OpenResult::SkippedCycle(entry);
            }
            self.aggregate.cycles = self.aggregate.cycles.saturating_add(1);
            return OpenResult::SkippedUnavailable;
        }

        let iter = match fs::read_dir(&resolved) {
            Ok(iter) => iter,
            Err(_) => {
                self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
                return OpenResult::SkippedUnavailable;
            }
        };
        self.load_rule_files(pending.rel.as_ref(), &resolved);

        let mut ancestry = pending.ancestry;
        ancestry.push(dir_id);
        self.open.push_back(OpenDir {
            rel: pending.rel,
            depth: pending.depth,
            ancestry,
            resolved,
            iter,
            children_emitted: 0,
            split_across_batches: false,
        });
        self.note_descriptor_peak();
        self.note_queue_peak();
        OpenResult::Opened
    }

    fn advance_open_dir(
        &mut self,
        entries: &mut Vec<DiscoveryEntry>,
        batch_bytes: &mut u64,
        any_split: &mut bool,
    ) -> Advance {
        let next = self
            .open
            .front_mut()
            .and_then(|open| open.iter.next());
        match next {
            None => {
                let finished = self.open.pop_front().expect("open dir existed");
                self.note_descriptor_peak();
                if finished.children_emitted > 0 && !finished.split_across_batches {
                    Advance::FinishedUnsplit
                } else {
                    if finished.split_across_batches {
                        *any_split = true;
                    }
                    Advance::Continue
                }
            }
            Some(Err(_)) => {
                self.open.pop_front();
                self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
                *any_split = true;
                self.note_descriptor_peak();
                Advance::Continue
            }
            Some(Ok(dirent)) => {
                let snapshot = {
                    let open = self.open.front().expect("open dir existed");
                    OpenSnapshot {
                        rel: open.rel.clone(),
                        depth: open.depth,
                        ancestry: open.ancestry.clone(),
                        resolved: open.resolved.clone(),
                    }
                };
                let child_fs = snapshot.resolved.join(dirent.file_name());
                if let Some(entry) =
                    self.observe_dirent(&snapshot, dirent.file_name().as_os_str(), &child_fs)
                {
                    *batch_bytes = batch_bytes.saturating_add(entry.path.as_bytes().len() as u64);
                    entries.push(entry);
                    if let Some(open) = self.open.front_mut() {
                        if open.children_emitted > 0
                            && (entries.len() >= self.limits.max_batch_entries as usize
                                || *batch_bytes >= self.limits.max_batch_bytes)
                        {
                            open.split_across_batches = true;
                            *any_split = true;
                        }
                        open.children_emitted = open.children_emitted.saturating_add(1);
                    }
                }
                Advance::Continue
            }
        }
    }

    fn observe_dirent(
        &mut self,
        open: &OpenSnapshot,
        name: &OsStr,
        child_fs_path: &std::path::Path,
    ) -> Option<DiscoveryEntry> {
        if name == OsStr::new(".") || name == OsStr::new("..") {
            return None;
        }
        let name_bytes = os_name_bytes(name);
        let child_path = match child_relative(open.rel.as_ref(), name_bytes) {
            Ok(path) => path,
            Err(_) => {
                self.aggregate.path_limited = self.aggregate.path_limited.saturating_add(1);
                return None;
            }
        };
        if child_path.as_bytes().len() as u32 > self.limits.max_path_bytes {
            self.aggregate.path_limited = self.aggregate.path_limited.saturating_add(1);
            return None;
        }

        let child_depth = open.depth.saturating_add(1);
        let meta = match fs::symlink_metadata(child_fs_path) {
            Ok(meta) => meta,
            Err(_) => {
                return Some(self.push_kind(
                    child_path,
                    DiscoveryKind::Unavailable,
                    child_depth,
                    None,
                    false,
                ));
            }
        };

        if validate_not_special(&meta).is_err() {
            return Some(self.push_kind(
                child_path,
                DiscoveryKind::Special,
                child_depth,
                None,
                false,
            ));
        }

        let file_type = meta.file_type();
        if file_type.is_symlink() {
            let excluded = self.classify_exclusion(&child_path, false);
            if !excluded {
                self.maybe_follow_symlink_dir(open, &child_path, child_depth, child_fs_path);
            }
            return Some(self.push_kind(
                child_path,
                DiscoveryKind::Symlink,
                child_depth,
                None,
                excluded,
            ));
        }
        if file_type.is_dir() {
            let excluded = self.classify_exclusion(&child_path, true);
            if !excluded {
                self.maybe_enqueue_dir(open, child_path.clone(), child_depth);
            }
            return Some(self.push_kind(
                child_path,
                DiscoveryKind::Directory,
                child_depth,
                None,
                excluded,
            ));
        }
        if file_type.is_file() {
            let excluded = self.classify_exclusion(&child_path, false);
            return Some(self.push_kind(
                child_path,
                DiscoveryKind::File,
                child_depth,
                Some(meta.len()),
                excluded,
            ));
        }
        Some(self.push_kind(
            child_path,
            DiscoveryKind::Special,
            child_depth,
            None,
            false,
        ))
    }

    fn maybe_follow_symlink_dir(
        &mut self,
        open: &OpenSnapshot,
        child_path: &NormalizedPath,
        child_depth: u32,
        child_fs_path: &std::path::Path,
    ) {
        if self.reader.symlink_policy() != SymlinkPolicy::AllowWithinRoot {
            return;
        }
        let target_meta = match fs::metadata(child_fs_path) {
            Ok(meta) => meta,
            Err(_) => return,
        };
        if target_meta.is_dir() {
            self.maybe_enqueue_dir(open, child_path.clone(), child_depth);
        }
    }

    fn maybe_enqueue_dir(
        &mut self,
        open: &OpenSnapshot,
        child_path: NormalizedPath,
        child_depth: u32,
    ) {
        if child_depth >= self.limits.max_depth {
            self.aggregate.depth_limited = self.aggregate.depth_limited.saturating_add(1);
            return;
        }
        let queued = (self.pending.len() + self.open.len()) as u32;
        if queued >= self.limits.max_queue_entries {
            self.queue_saturated = true;
            self.aggregate.queue_refused = self.aggregate.queue_refused.saturating_add(1);
            return;
        }
        self.pending.push_back(PendingDir {
            rel: Some(child_path),
            depth: child_depth,
            ancestry: open.ancestry.clone(),
        });
        self.note_queue_peak();
    }

    fn classify_exclusion(&mut self, path: &NormalizedPath, is_dir: bool) -> bool {
        self.ignore.decide(path, is_dir).is_excluded()
    }

    fn load_rule_files(&mut self, dir_rel: Option<&NormalizedPath>, resolved: &std::path::Path) {
        for name in [".gitignore", ".fcbignore"] {
            let rule_path = resolved.join(name);
            let meta = match fs::symlink_metadata(&rule_path) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if !meta.file_type().is_file() || meta.len() > MAX_RULE_FILE_BYTES {
                continue;
            }
            let Ok(bytes) = fs::read(&rule_path) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            self.ignore.add_rule_file(dir_rel, text);
        }
    }

    fn push_kind(
        &mut self,
        path: NormalizedPath,
        kind: DiscoveryKind,
        depth: u32,
        observed_len: Option<u64>,
        excluded: bool,
    ) -> DiscoveryEntry {
        match kind {
            DiscoveryKind::File => self.aggregate.files = self.aggregate.files.saturating_add(1),
            DiscoveryKind::Directory => {
                self.aggregate.directories = self.aggregate.directories.saturating_add(1);
            }
            DiscoveryKind::Symlink => {
                self.aggregate.symlinks = self.aggregate.symlinks.saturating_add(1);
            }
            DiscoveryKind::Special => {
                self.aggregate.special = self.aggregate.special.saturating_add(1);
            }
            DiscoveryKind::Unavailable => {
                self.aggregate.unavailable = self.aggregate.unavailable.saturating_add(1);
            }
            DiscoveryKind::Cycle => {
                self.aggregate.cycles = self.aggregate.cycles.saturating_add(1);
            }
        }
        if excluded {
            self.aggregate.excluded = self.aggregate.excluded.saturating_add(1);
        }
        DiscoveryEntry {
            path,
            kind,
            depth,
            observed_len,
            scan_epoch: self.epoch,
            excluded,
        }
    }

    fn note_descriptor_peak(&mut self) {
        self.peaks.open_descriptors = self
            .peaks
            .open_descriptors
            .max(self.open.len() as u32);
        self.note_queue_peak();
    }

    fn note_queue_peak(&mut self) {
        let queued = (self.pending.len() + self.open.len()) as u32;
        self.peaks.queue_entries = self.peaks.queue_entries.max(queued);
    }
}

enum OpenResult {
    Opened,
    SkippedCycle(DiscoveryEntry),
    SkippedUnavailable,
}

enum Advance {
    FinishedUnsplit,
    Continue,
}

struct OpenSnapshot {
    rel: Option<NormalizedPath>,
    depth: u32,
    ancestry: Vec<DirectoryId>,
    resolved: PathBuf,
}

fn child_relative(
    parent: Option<&NormalizedPath>,
    name: &[u8],
) -> Result<NormalizedPath, SourceError> {
    match parent {
        None => NormalizedPath::from_dirent_name(name),
        Some(parent) => parent.join_segment(name),
    }
}

#[cfg(unix)]
fn os_name_bytes(name: &OsStr) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes()
}

#[cfg(not(unix))]
fn os_name_bytes(name: &OsStr) -> &[u8] {
    name.to_str().unwrap_or("").as_bytes()
}

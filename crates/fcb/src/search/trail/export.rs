#![forbid(unsafe_code)]

//! Bounded, ordered source-evidence selection for an explicitly authorized
//! saved archive. Formatting and destination effects belong to the caller.
//! One returned capture owns its existing source leases; the cursor retains no
//! payloads and never opens a live path or silently reattaches a changed note.

use std::io::{Read, Seek};
use fcb_core::{FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use super::{resolve_selection, PagedSearchError, PagedSnapshot, PagedSnapshotError,
    TrailCapture, TrailReadError, TrailTarget, TrailView, MAX_TRAIL_ITEMS};

pub const MAX_TRAIL_EXPORT_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrailExportLimits {
    /// Visit at most this many trail entries, in their explicit saved order.
    pub max_entries: usize,
    /// Whole-member verification admission, including repeated visits and
    /// failed loads. Archive-opening validation has its own reported limits.
    pub max_source_bytes: u64,
    /// Exact original selection bytes, not decoded characters or encoded
    /// output bytes. The formatter must independently reserve its metadata.
    pub max_selected_bytes: u64,
}
impl Default for TrailExportLimits {
    fn default() -> Self {
        Self { max_entries: 128, max_source_bytes: 32 * 1024 * 1024,
            max_selected_bytes: 1024 * 1024 }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrailExportStats {
    pub total_entries: usize,
    pub entries_visited: usize,
    pub verified_entries: usize,
    pub unavailable_entries: usize,
    pub omitted_entry_limit: usize,
    pub omitted_source_limit: usize,
    pub omitted_selection_limit: usize,
    pub verification_bytes_admitted: u64,
    /// Payload reads during this cursor's selected member loads. This does not
    /// include the archive's separate initial full-envelope validation pass.
    pub source_bytes_read: u64,
    pub selected_bytes: u64,
    pub interrupted: bool,
}
impl TrailExportStats {
    pub fn complete(self) -> bool {
        !self.interrupted && self.entries_visited == self.total_entries && self.unavailable_entries == 0
            && self.omitted_source_limit == 0 && self.omitted_selection_limit == 0
    }
}

pub enum TrailExportOutcome<'trail> {
    Verified(TrailCapture<'trail>),
    Unavailable(TrailReadError),
    SourceByteLimit,
    SelectionByteLimit,
}
pub struct TrailExportItem<'trail> {
    pub target: TrailTarget<'trail>,
    pub outcome: TrailExportOutcome<'trail>,
}

/// A single export generation with fresh caller-owned identity intervals. An
/// error terminates the cursor; cancellation cannot later be resumed as if the
/// failed attempt completed. Ordinary missing/changed sources produce explicit
/// unavailable rows, while other admitted entries can still succeed.
pub struct TrailExportCursor<'trail> {
    view: TrailView<'trail>,
    generation: QueryGeneration,
    first_file: FileId,
    first_revision: SourceRevision,
    limits: TrailExportLimits,
    stats: TrailExportStats,
    stopped: bool,
}
impl<'trail> TrailExportCursor<'trail> {
    pub fn new(view: TrailView<'trail>, generation: QueryGeneration,
        first_file: FileId, first_revision: SourceRevision, limits: TrailExportLimits)
        -> Result<Self, TrailReadError> {
        if generation.owner() != first_file.owner() || generation.owner() != first_revision.owner() {
            return Err(TrailReadError::OwnerMismatch);
        }
        if limits.max_entries > MAX_TRAIL_ITEMS || limits.max_source_bytes > MAX_TRAIL_EXPORT_SOURCE_BYTES
            || limits.max_selected_bytes > MAX_TRAIL_EXPORT_SOURCE_BYTES {
            return Err(super::TrailError::Limit.into());
        }
        let admitted = view.len().min(limits.max_entries);
        let last = admitted.saturating_sub(1) as u64;
        first_file.get().checked_add(last).ok_or(TrailReadError::GenerationExhausted)?;
        first_revision.get().checked_add(last).ok_or(TrailReadError::GenerationExhausted)?;
        Ok(Self { view, generation, first_file, first_revision, limits,
            stats: TrailExportStats { total_entries: view.len(),
                omitted_entry_limit: view.len() - admitted, ..TrailExportStats::default() }, stopped: false })
    }
    pub const fn stats(&self) -> TrailExportStats { self.stats }

    /// The two allocation IDs are for this item's verified member and retained
    /// capture metadata. Release the previous item before reusing them. Limits
    /// are checked before a member is loaded; oversized selections never force
    /// unnecessary source I/O or silently consume the remaining smaller items.
    pub fn next<R: Read + Seek>(&mut self, archive: &mut PagedSnapshot<R>, budget: &ResourceBudget,
        allocations: [ResourceAllocationId; 2], mut canceled: impl FnMut() -> bool)
        -> Result<Option<TrailExportItem<'trail>>, TrailReadError> {
        if self.stopped { return Err(TrailReadError::StaleDelivery); }
        if canceled() { self.stop(); return Err(PagedSnapshotError::Canceled.into()); }
        if archive.directory().owner() != self.generation.owner() {
            self.stop(); return Err(TrailReadError::OwnerMismatch);
        }
        if self.stats.entries_visited == self.view.len().min(self.limits.max_entries) { return Ok(None); }
        let ordinal = self.stats.entries_visited;
        let entry = self.view.entry(ordinal)?;
        let target = TrailTarget { entry, ordinal, trail: self.view.digest(), generation: self.generation };
        self.stats.entries_visited += 1;
        let outcome = if let Err(error) = resolve_selection(entry, archive.directory()) {
            self.stats.unavailable_entries += 1;
            TrailExportOutcome::Unavailable(error)
        } else if entry.range.len().get() > self.limits.max_selected_bytes - self.stats.selected_bytes {
            self.stats.omitted_selection_limit += 1;
            TrailExportOutcome::SelectionByteLimit
        } else if entry.source_length > self.limits.max_source_bytes - self.stats.verification_bytes_admitted {
            self.stats.omitted_source_limit += 1;
            TrailExportOutcome::SourceByteLimit
        } else {
            self.stats.verification_bytes_admitted += entry.source_length;
            let file = FileId::new(self.first_file.owner(), self.first_file.get() + ordinal as u64)
                .map_err(|_| TrailReadError::GenerationExhausted)?;
            let revision = SourceRevision::new(self.first_revision.owner(), self.first_revision.get() + ordinal as u64)
                .map_err(|_| TrailReadError::GenerationExhausted)?;
            let before = archive.load_stats().bytes_read;
            let loaded = target.open(archive, self.view.digest(), self.generation, file, revision,
                budget, allocations, &mut canceled);
            self.stats.source_bytes_read += archive.load_stats().bytes_read.saturating_sub(before);
            match loaded {
                Ok(capture) => {
                    self.stats.verified_entries += 1;
                    self.stats.selected_bytes += entry.range.len().get();
                    TrailExportOutcome::Verified(capture)
                }
                Err(error) if fatal(error) => { self.stop(); return Err(error); }
                Err(error) => {
                    self.stats.unavailable_entries += 1;
                    TrailExportOutcome::Unavailable(error)
                }
            }
        };
        if canceled() { self.stop(); return Err(PagedSnapshotError::Canceled.into()); }
        Ok(Some(TrailExportItem { target, outcome }))
    }
    fn stop(&mut self) { self.stopped = true; self.stats.interrupted = true; }
}

fn fatal(error: TrailReadError) -> bool {
    matches!(error, TrailReadError::OwnerMismatch | TrailReadError::GenerationExhausted
        | TrailReadError::Format(super::TrailError::Canceled | super::TrailError::ResourceDenied)
        | TrailReadError::Archive(PagedSnapshotError::Canceled | PagedSnapshotError::ResourceDenied)
        | TrailReadError::Capture(PagedSearchError::Canceled | PagedSearchError::ResourceDenied
            | PagedSearchError::OwnerMismatch | PagedSearchError::IdentityExhausted | PagedSearchError::InvalidLimits
            | PagedSearchError::Archive(PagedSnapshotError::Canceled | PagedSnapshotError::ResourceDenied)))
}

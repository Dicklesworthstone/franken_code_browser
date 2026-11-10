#![forbid(unsafe_code)]

//! Retained comparison of two explicitly selected FCBS observations. Both
//! verified directories share one analysis owner and keep their original open
//! descriptors. Membership classification reuses the public snapshot merge;
//! this service adds bounded scheduling, delivery and exact reader handoff.
//! No live root, Git command, archive pathname reopening or rename inference.

mod wire;

use std::{fs::File, mem::size_of, path::{Path, PathBuf}};
use fcb::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, SourceRevision};
use fcb::search::{RawPath, ResourceAllocationId, ResourceBudget};
use fcb::search::paged_snapshot::{PagedCapture, PagedSearchError, PagedSnapshot,
    PagedSnapshotError, PagedMemberData};
use fcb::search::snapshot_comparison::{SnapshotChange, SnapshotChangeKind,
    SnapshotComparison, SnapshotCompareError};
pub use fcb::search::snapshot::SnapshotLimits;
use fcb_core::ResourceLease;
use crate::{AppError, input, MANAGED_BYTES, EXIT_OK, EXIT_PARTIAL};
use crate::output::{Output, OutputError};
use super::{HostResponse, MAX_HOST_TEXT_BYTES, reader::{ReaderSession, ReaderSessionError}};

pub const MAX_SNAPSHOT_COMPARISON_PAGE: usize = 128;
pub const MAX_SNAPSHOT_COMPARISON_STEP: usize = 1024;
pub const MAX_SNAPSHOT_COMPARISON_ENTRIES: usize = 131_072;
pub const MAX_SNAPSHOT_COMPARISON_RESPONSE: usize = 4 * 1024 * 1024;
const MEMBERS_PER_ARCHIVE: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotComparisonSide { Before, After }
impl SnapshotComparisonSide {
    pub const fn name(self) -> &'static str {
        match self { Self::Before => "before", Self::After => "after" }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotChangeFilter {
    All, Differences, Unchanged, Changed, Added, Removed, OnlyBefore, OnlyAfter, Unavailable,
}
impl SnapshotChangeFilter {
    pub const fn name(self) -> &'static str {
        match self {
            Self::All => "all", Self::Differences => "differences", Self::Unchanged => "unchanged",
            Self::Changed => "changed", Self::Added => "added", Self::Removed => "removed",
            Self::OnlyBefore => "only-before", Self::OnlyAfter => "only-after",
            Self::Unavailable => "unavailable",
        }
    }
    fn includes(self, kind: SnapshotChangeKind) -> bool {
        match self {
            Self::All => true, Self::Differences => kind != SnapshotChangeKind::Unchanged,
            Self::Unchanged => kind == SnapshotChangeKind::Unchanged,
            Self::Changed => kind == SnapshotChangeKind::Changed,
            Self::Added => kind == SnapshotChangeKind::Added,
            Self::Removed => kind == SnapshotChangeKind::Removed,
            Self::OnlyBefore => kind == SnapshotChangeKind::OnlyBefore,
            Self::OnlyAfter => kind == SnapshotChangeKind::OnlyAfter,
            Self::Unavailable => kind == SnapshotChangeKind::Unavailable,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotComparisonError {
    App(AppError), Archive(PagedSnapshotError), Compare(SnapshotCompareError),
    Query(PagedSearchError), Reader(ReaderSessionError),
    InvalidLimits, MissingComparison, StaleGeneration, MissingEntry,
    UnavailableSide, OwnerMismatch, IdentityExhausted, Canceled,
}
impl SnapshotComparisonError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Archive(PagedSnapshotError::Canceled)
                | Self::Compare(SnapshotCompareError::Canceled)
                | Self::Compare(SnapshotCompareError::Archive(PagedSnapshotError::Canceled))
                | Self::Compare(SnapshotCompareError::Analysis(fcb::analysis::ComparisonError::Canceled))
                | Self::Query(PagedSearchError::Canceled)
                | Self::Query(PagedSearchError::Archive(PagedSnapshotError::Canceled))
                | Self::Reader(ReaderSessionError::Canceled) => true,
            Self::App(error) => error.is_canceled(),
            Self::Reader(ReaderSessionError::Host(super::HostError::App(error))
                | ReaderSessionError::App(error)) => error.is_canceled(),
            Self::Reader(ReaderSessionError::View(error)) => AppError::View(error).is_canceled(),
            _ => false,
        }
    }
}
impl std::fmt::Display for SnapshotComparisonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "{e}"), Self::Archive(e) => write!(f, "{e}"),
            Self::Compare(e) => write!(f, "{e}"), Self::Query(e) => write!(f, "{e}"),
            Self::Reader(e) => write!(f, "{e}"),
            Self::InvalidLimits => f.write_str("SNAPSHOT_COMPARISON_INVALID_LIMITS"),
            Self::MissingComparison => f.write_str("SNAPSHOT_COMPARISON_NOT_READY"),
            Self::StaleGeneration => f.write_str("SNAPSHOT_COMPARISON_STALE_GENERATION"),
            Self::MissingEntry => f.write_str("SNAPSHOT_COMPARISON_NO_ENTRY"),
            Self::UnavailableSide => f.write_str("SNAPSHOT_COMPARISON_SIDE_UNAVAILABLE"),
            Self::OwnerMismatch => f.write_str("SNAPSHOT_COMPARISON_OWNER_MISMATCH"),
            Self::IdentityExhausted => f.write_str("SNAPSHOT_COMPARISON_IDENTITY_EXHAUSTED"),
            Self::Canceled => f.write_str("SNAPSHOT_COMPARISON_CANCELED"),
        }
    }
}
impl std::error::Error for SnapshotComparisonError {}
impl From<AppError> for SnapshotComparisonError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<PagedSnapshotError> for SnapshotComparisonError { fn from(e: PagedSnapshotError) -> Self { Self::Archive(e) } }
impl From<SnapshotCompareError> for SnapshotComparisonError { fn from(e: SnapshotCompareError) -> Self { Self::Compare(e) } }
impl From<PagedSearchError> for SnapshotComparisonError { fn from(e: PagedSearchError) -> Self { Self::Query(e) } }
impl From<ReaderSessionError> for SnapshotComparisonError { fn from(e: ReaderSessionError) -> Self { Self::Reader(e) } }
impl From<OutputError> for SnapshotComparisonError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

struct RetainedComparison {
    generation: QueryGeneration,
    cursor: SnapshotComparison,
    entries: Vec<SnapshotChange>,
    capacity: usize,
    steps: u64,
    last_step_entries: usize,
    _lease: ResourceLease,
}
impl RetainedComparison {
    fn complete(&self) -> bool {
        self.cursor.finished() && self.cursor.membership_comparable()
            && self.cursor.stats().uncertain_paths() == 0
    }
}

/// Explicit worker-owned service. Opening validates both bounded archives in
/// full; subsequent comparison steps/pages read only retained metadata. Only
/// reader activation loads one member, verifying its original source digest.
/// Pending generations never replace accepted row authority before completion.
pub struct SnapshotComparisonSession {
    before: PagedSnapshot<File>,
    after: PagedSnapshot<File>,
    before_path: PathBuf,
    after_path: PathBuf,
    limits: SnapshotLimits,
    accepted: Option<RetainedComparison>,
    pending: Option<RetainedComparison>,
    last_attempt: u64,
    next_allocation: u64,
    budget: ResourceBudget,
    _lease: ResourceLease,
}
impl SnapshotComparisonSession {
    pub fn open(owner: ArenaOwnerId, before: &Path, after: &Path, limits: SnapshotLimits,
        mut canceled: impl FnMut() -> bool) -> Result<Self, SnapshotComparisonError> {
        check(&mut canceled)?;
        if limits.max_file_bytes > MAX_HOST_TEXT_BYTES || limits.max_files > MEMBERS_PER_ARCHIVE
            || limits.max_path_bytes > 16_384
            || [before, after].iter().any(|path| path.as_os_str().is_empty() || path.as_os_str().len() > 16_384) {
            return Err(SnapshotComparisonError::InvalidLimits);
        }
        let budget = ResourceBudget::new(owner, ByteLength::new(MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
        let lease = budget.try_reserve_managed(owner, allocation(1)?,
            ByteLength::new((512 * 1024 + size_of::<Self>()) as u64)).map_err(|_| AppError::Admission)?;
        let before_path = input::absolute(before)?;
        let after_path = input::absolute(after)?;
        let before = open_archive(owner, &before_path, limits, &budget, allocation(2)?, &mut canceled)?;
        let after = open_archive(owner, &after_path, limits, &budget, allocation(3)?, &mut canceled)?;
        check(&mut canceled)?;
        Ok(Self { before, after, before_path, after_path, limits, accepted: None, pending: None,
            last_attempt: 0, next_allocation: 4, budget, _lease: lease })
    }
    pub fn owner(&self) -> ArenaOwnerId { self.before.directory().owner() }
    pub fn accepted_generation(&self) -> Option<u64> { self.accepted.as_ref().map(|s| s.generation.get()) }
    pub fn pending_generation(&self) -> Option<u64> { self.pending.as_ref().map(|s| s.generation.get()) }
    pub const fn last_attempt(&self) -> u64 { self.last_attempt }
    pub fn cancel_pending(&mut self) -> bool { self.pending.take().is_some() }

    pub fn info(&mut self, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SnapshotComparisonError> {
        check(&mut canceled)?;
        let mut out = self.output("info", None)?;
        out.literal("}\n")?;
        self.finish(out, false, &mut canceled)
    }

    /// Admit the full known union upper bound before allocating descriptors.
    /// An admitted attempt consumes its identity even if allocation, output or
    /// cancellation fails; the previous accepted inventory remains usable.
    pub fn begin(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SnapshotComparisonError> {
        check(&mut canceled)?;
        if generation == 0 || generation <= self.last_attempt { return Err(SnapshotComparisonError::StaleGeneration); }
        self.last_attempt = generation;
        self.pending = None;
        let generation = QueryGeneration::new(self.owner(), generation).map_err(|_| SnapshotComparisonError::IdentityExhausted)?;
        let capacity = self.before.directory().len().checked_add(self.after.directory().len())
            .filter(|n| *n <= MAX_SNAPSHOT_COMPARISON_ENTRIES).ok_or(SnapshotComparisonError::InvalidLimits)?;
        let charge = capacity.checked_mul(size_of::<SnapshotChange>())
            .and_then(|n| n.checked_add(size_of::<RetainedComparison>())).ok_or(AppError::Admission)?;
        let [id] = self.allocations()?;
        let lease = self.budget.try_reserve_managed(self.owner(), id, ByteLength::new(charge as u64)).map_err(|_| AppError::Admission)?;
        let mut entries = Vec::new();
        entries.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
        if entries.capacity() > capacity { return Err(AppError::Admission.into()); }
        let state = RetainedComparison { generation,
            cursor: SnapshotComparison::new(self.before.directory(), self.after.directory(), generation)?,
            entries, capacity, steps: 0, last_step_entries: 0, _lease: lease };
        let response = self.state_response("begin", &state, &mut canceled)?;
        self.pending = Some(state);
        Ok(response)
    }

    /// Each call visits at most max_entries path pairs. A final zero-record
    /// iteration establishes cursor exhaustion without inventing completeness
    /// at an exact step boundary. No original member payload is loaded here.
    pub fn step(&mut self, generation: u64, max_entries: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SnapshotComparisonError> {
        if canceled() {
            if self.pending_generation() == Some(generation) { self.pending = None; }
            return Err(SnapshotComparisonError::Canceled);
        }
        if !(1..=MAX_SNAPSHOT_COMPARISON_STEP).contains(&max_entries) { return Err(SnapshotComparisonError::InvalidLimits); }
        if self.pending_generation() != Some(generation) {
            self.accepted(generation)?;
            return self.page_command("step", generation, SnapshotChangeFilter::Differences, 0, 64, &mut canceled);
        }
        let mut state = self.pending.take().ok_or(SnapshotComparisonError::MissingComparison)?;
        state.last_step_entries = 0;
        state.steps = state.steps.checked_add(1).ok_or(SnapshotComparisonError::IdentityExhausted)?;
        for _ in 0..max_entries {
            let Some(change) = state.cursor.next(self.before.directory(), self.after.directory(),
                state.generation, &mut canceled)? else { break; };
            if state.entries.len() == state.capacity { return Err(SnapshotComparisonError::InvalidLimits); }
            state.entries.push(change);
            state.last_step_entries += 1;
        }
        let response = self.state_response("step", &state, &mut canceled)?;
        if state.cursor.finished() { self.accepted = Some(state); } else { self.pending = Some(state); }
        Ok(response)
    }

    pub fn page(&mut self, generation: u64, filter: SnapshotChangeFilter, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SnapshotComparisonError> {
        self.page_command("page", generation, filter, start, limit, &mut canceled)
    }
    fn page_command(&mut self, command: &str, generation: u64, filter: SnapshotChangeFilter,
        start: usize, limit: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, SnapshotComparisonError> {
        check(canceled)?;
        self.accepted(generation)?;
        let mut out = self.output(command, None)?;
        let state = self.accepted(generation)?;
        self.encode_page(&mut out, state, filter, start, limit, canceled)?;
        let partial = !state.complete();
        out.literal("}\n")?;
        self.finish(out, partial, canceled)
    }

    pub fn open_reader(&mut self, reader_owner: ArenaOwnerId, generation: u64, entry_id: u64,
        side: SnapshotComparisonSide, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), SnapshotComparisonError> {
        check(&mut canceled)?;
        if reader_owner == self.owner() { return Err(SnapshotComparisonError::OwnerMismatch); }
        let index = entry_id.checked_sub(1).and_then(|n| usize::try_from(n).ok()).ok_or(SnapshotComparisonError::MissingEntry)?;
        let state = self.accepted(generation)?;
        let change = *state.entries.get(index).ok_or(SnapshotComparisonError::MissingEntry)?;
        change.validate(self.before.directory(), self.after.directory(), state.generation)?;
        let ordinal = match side { SnapshotComparisonSide::Before => change.before_ordinal(),
            SnapshotComparisonSide::After => change.after_ordinal() }.ok_or(SnapshotComparisonError::UnavailableSide)?;
        let file_number = member_identity(side, ordinal)?;
        let file = FileId::new(self.owner(), file_number).map_err(|_| SnapshotComparisonError::IdentityExhausted)?;
        let revision = SourceRevision::new(self.owner(), file_number).map_err(|_| SnapshotComparisonError::IdentityExhausted)?;
        let ids = self.allocations()?;
        let archive = match side { SnapshotComparisonSide::Before => &mut self.before, SnapshotComparisonSide::After => &mut self.after };
        let member = archive.directory().member(ordinal).ok_or(SnapshotComparisonError::MissingEntry)?;
        if !matches!(member.data, PagedMemberData::Captured { .. }) { return Err(SnapshotComparisonError::UnavailableSide); }
        let before_reads = archive.load_stats();
        let capture = PagedCapture::load(archive, ordinal, file, revision, &self.budget, ids, &mut canceled)?;
        let after_reads = archive.load_stats();
        let member = archive.directory().member(ordinal).ok_or(SnapshotComparisonError::MissingEntry)?;
        let label = RawPath::from_bytes(member.path).to_path_buf();
        let archive_digest = archive.directory().digest();
        let mut reader = ReaderSession::from_bytes(reader_owner, &label, capture.bytes(), &mut canceled)?;
        let info = reader.info(&mut canceled)?;
        let mut out = self.output("open-reader", None)?;
        out.literal(",\"comparison_generation\":")?; out.integer(generation)?;
        out.literal(",\"entry_id\":")?; out.integer(entry_id)?;
        out.literal(",\"side\":")?; out.quoted(side.name())?;
        out.literal(",\"member\":")?; out.integer(ordinal as u64)?;
        out.literal(",\"file_id\":")?; out.integer(file_number)?;
        out.literal(",\"source_revision\":")?; out.integer(file_number)?;
        out.literal(",\"path\":")?; out.path(&label)?;
        out.literal(",\"archive_digest\":")?; out.quoted(&archive_digest.to_hex())?;
        out.literal(",\"source_digest\":")?; out.quoted(&capture.source_digest().to_hex())?;
        out.literal(",\"captured_bytes\":")?; out.integer(capture.bytes().len() as u64)?;
        out.literal(",\"source_observation\":\"verified-saved-member\",\"reader_owner\":")?; out.integer(reader_owner.get())?;
        out.literal(",\"member_bytes_read\":")?; out.integer(after_reads.bytes_read - before_reads.bytes_read)?;
        out.literal(",\"member_read_calls\":")?; out.integer(after_reads.read_calls - before_reads.read_calls)?;
        out.literal(",\"reader\":")?; out.literal(info.as_str())?; out.literal("}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        Ok((reader, response))
    }

    fn accepted(&self, generation: u64) -> Result<&RetainedComparison, SnapshotComparisonError> {
        let state = self.accepted.as_ref().ok_or(SnapshotComparisonError::MissingComparison)?;
        if state.generation.get() != generation { return Err(SnapshotComparisonError::StaleGeneration); }
        Ok(state)
    }
    fn state_response(&mut self, command: &str, state: &RetainedComparison, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, SnapshotComparisonError> {
        let mut out = self.output(command, Some(state))?;
        self.encode_page(&mut out, state, SnapshotChangeFilter::Differences, 0, 64, canceled)?;
        out.literal("}\n")?;
        self.finish(out, !state.complete(), canceled)
    }
    fn allocations<const N: usize>(&mut self) -> Result<[ResourceAllocationId; N], SnapshotComparisonError> {
        let next = self.next_allocation.checked_add(N as u64).ok_or(SnapshotComparisonError::IdentityExhausted)?;
        let first = self.next_allocation;
        self.next_allocation = next;
        let mut ids = [allocation(first)?; N];
        for (index, id) in ids.iter_mut().enumerate() { *id = allocation(first + index as u64)?; }
        Ok(ids)
    }
    fn finish(&mut self, out: Output, partial: bool, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, SnapshotComparisonError> {
        check(canceled)?;
        let [id] = self.allocations()?;
        let charge = out.as_bytes().len().checked_mul(3).and_then(|n| n.checked_add(4096)).ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.owner(), id, ByteLength::new(charge as u64)).map_err(|_| AppError::Admission)?;
        let source = std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?;
        let mut text = String::new();
        text.try_reserve_exact(source.len()).map_err(|_| AppError::Admission)?;
        if text.capacity() > source.len() { return Err(AppError::Admission.into()); }
        text.push_str(source);
        check(canceled)?;
        Ok(HostResponse { text, exit_code: if partial { EXIT_PARTIAL } else { EXIT_OK }, _lease: lease })
    }
}
fn open_archive(owner: ArenaOwnerId, path: &Path, limits: SnapshotLimits, budget: &ResourceBudget,
    id: ResourceAllocationId, canceled: &mut impl FnMut() -> bool) -> Result<PagedSnapshot<File>, SnapshotComparisonError> {
    check(canceled)?;
    let (file, metadata) = input::open_regular(path)?;
    if metadata.len() > limits.max_document_bytes as u64 { return Err(SnapshotComparisonError::InvalidLimits); }
    Ok(PagedSnapshot::open(file, owner, limits, budget, id, canceled)?)
}
fn member_identity(side: SnapshotComparisonSide, ordinal: usize) -> Result<u64, SnapshotComparisonError> {
    if ordinal >= MEMBERS_PER_ARCHIVE { return Err(SnapshotComparisonError::MissingEntry); }
    Ok((ordinal + 1 + if side == SnapshotComparisonSide::After { MEMBERS_PER_ARCHIVE } else { 0 }) as u64)
}
fn allocation(value: u64) -> Result<ResourceAllocationId, SnapshotComparisonError> {
    ResourceAllocationId::new(value).map_err(|_| SnapshotComparisonError::IdentityExhausted)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), SnapshotComparisonError> {
    if canceled() { Err(SnapshotComparisonError::Canceled) } else { Ok(()) }
}

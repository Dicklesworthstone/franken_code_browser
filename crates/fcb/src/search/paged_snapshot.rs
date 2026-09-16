#![forbid(unsafe_code)]

//! Bounded-residency saved-source search and reading. Uses the existing exact
//! streaming matcher/decoder, not a second search engine. At most one source
//! member is loaded by a query. Results retain digest-qualified original ranges;
//! opening a hit verifies its member again and never consults the live source root.
//!
//! Archive validation and member loads are bounded, cancellable worker I/O.
//! A query step may load one admitted member, then runs one bounded matcher step.
//! It is not an interaction callback or a wall-clock latency guarantee.

use std::{io::{Cursor, Read, Seek}, mem::size_of, sync::Arc};
use crate::{SourceCapture, FramePlan};
use fcb_core::{ByteLength, ByteRange, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, ResourceLease, SourceRevision};
use super::{CaptureRequest, ReaderSearch, StreamingNeedle, StreamReadOptions, StreamReadState,
    StreamReadStep, StreamReadError, RawPath, SourceReader, ReaderLimits, ReaderError,
    ReaderIndexProgress, ReadingTarget, ReadingSeek, ReadingAnchor, ReadingWindow, ReadingWindowOptions};
pub use fcb_store::paged_snapshot::{PagedSnapshot, PagedSnapshotError, PagedMember, PagedMemberData,
    SnapshotDirectory, SnapshotIoStats, VerifiedMember};
pub use fcb_store::Sha256Digest;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PagedSearchError {
    Archive(PagedSnapshotError), Search(StreamReadError), OwnerMismatch, IdentityExhausted,
    InvalidLimits, ResourceDenied, Pending, Canceled, StaleQuery, InvalidHit, IncompleteInput,
}
impl std::fmt::Display for PagedSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Archive(error) => write!(f, "{error}"), Self::Search(error) => write!(f, "{error}"),
            Self::OwnerMismatch => f.write_str("SNAPSHOT_OWNER_MISMATCH"),
            Self::IdentityExhausted => f.write_str("SNAPSHOT_IDENTITY_EXHAUSTED"),
            Self::InvalidLimits => f.write_str("SNAPSHOT_QUERY_INVALID_LIMITS"),
            Self::ResourceDenied => f.write_str("SNAPSHOT_RESOURCE_DENIED"),
            Self::Pending => f.write_str("SNAPSHOT_QUERY_PENDING"),
            Self::Canceled => f.write_str("SNAPSHOT_CANCELED"),
            Self::StaleQuery => f.write_str("SNAPSHOT_STALE_QUERY"),
            Self::InvalidHit => f.write_str("SNAPSHOT_INVALID_HIT"),
            Self::IncompleteInput => f.write_str("SNAPSHOT_SEARCH_INCOMPLETE_INPUT"),
        }
    }
}
impl std::error::Error for PagedSearchError {}
impl From<PagedSnapshotError> for PagedSearchError { fn from(e: PagedSnapshotError) -> Self { Self::Archive(e) } }
impl From<StreamReadError> for PagedSearchError { fn from(e: StreamReadError) -> Self { Self::Search(e) } }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagedQueryOptions {
    pub generation: QueryGeneration,
    pub first_file: FileId,
    pub first_revision: SourceRevision,
    pub max_matches: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PagedQueryState { Pending, Complete, Truncated, Canceled, Failed(PagedSearchError) }
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PagedQueryStats {
    pub members_visited: usize,
    pub files_searched: usize,
    pub unsupported_files: usize,
    pub scanned_bytes: u64,
    pub peak_source_bytes: usize,
    pub last_step_loaded_bytes: usize,
    pub last_step_scanned_bytes: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagedHit {
    ordinal: usize, file: FileId, revision: SourceRevision, generation: QueryGeneration,
    range: ByteRange, archive: Sha256Digest, source: Sha256Digest,
}
impl PagedHit {
    pub const fn ordinal(self) -> usize { self.ordinal }
    pub const fn file(self) -> FileId { self.file }
    pub const fn revision(self) -> SourceRevision { self.revision }
    pub const fn generation(self) -> QueryGeneration { self.generation }
    pub const fn original_range(self) -> ByteRange { self.range }
    pub const fn archive_digest(self) -> Sha256Digest { self.archive }
    pub const fn source_digest(self) -> Sha256Digest { self.source }
}

pub struct PagedQuery<'archive, 'needle, R: Read + Seek> {
    archive: &'archive mut PagedSnapshot<R>,
    needle: &'needle StreamingNeedle,
    options: PagedQueryOptions,
    allocations: [ResourceAllocationId; 3],
    next: usize,
    active: Option<ReaderSearch<'needle, Cursor<VerifiedMember>>>,
    hits: Vec<PagedHit>,
    matches_seen: u64,
    state: PagedQueryState,
    stats: PagedQueryStats,
    _lease: ResourceLease,
}
impl<'archive, 'needle, R: Read + Seek> PagedQuery<'archive, 'needle, R> {
    /// Allocate fresh host identity intervals; archive ordinals are not trusted
    /// FileIds. allocations = [result table, one member, one matcher]. They must
    /// be distinct and must not collide with the retained archive/needle leases.
    pub fn new(archive: &'archive mut PagedSnapshot<R>, needle: &'needle StreamingNeedle,
        options: PagedQueryOptions, budget: &ResourceBudget, allocations: [ResourceAllocationId; 3])
        -> Result<Self, PagedSearchError> {
        let owner = archive.directory().owner();
        if options.generation.owner() != owner || options.first_file.owner() != owner || options.first_revision.owner() != owner {
            return Err(PagedSearchError::OwnerMismatch);
        }
        if options.max_matches > super::streaming::MAX_STREAM_RESULTS
            || allocations[0] == allocations[1] || allocations[0] == allocations[2] || allocations[1] == allocations[2] {
            return Err(PagedSearchError::InvalidLimits);
        }
        let last = archive.directory().len().saturating_sub(1) as u64;
        options.first_file.get().checked_add(last).ok_or(PagedSearchError::IdentityExhausted)?;
        options.first_revision.get().checked_add(last).ok_or(PagedSearchError::IdentityExhausted)?;
        let charge = options.max_matches.checked_mul(size_of::<PagedHit>()).and_then(|n| n.checked_add(size_of::<Self>()))
            .ok_or(PagedSearchError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(owner, allocations[0], ByteLength::new(charge as u64))
            .map_err(|_| PagedSearchError::ResourceDenied)?;
        let mut hits = Vec::new(); hits.try_reserve_exact(options.max_matches).map_err(|_| PagedSearchError::ResourceDenied)?;
        if hits.capacity() > options.max_matches { return Err(PagedSearchError::ResourceDenied); }
        Ok(Self { archive, needle, options, allocations, next: 0, active: None, hits, matches_seen: 0,
            state: PagedQueryState::Pending, stats: PagedQueryStats::default(), _lease: lease })
    }
    pub const fn state(&self) -> PagedQueryState { self.state }
    pub const fn generation(&self) -> QueryGeneration { self.options.generation }
    pub const fn stats(&self) -> PagedQueryStats { self.stats }
    pub fn cancel(&mut self) { self.state = PagedQueryState::Canceled; self.active = None; }
    pub fn step(&mut self, step: StreamReadStep, active_generation: QueryGeneration,
        budget: &ResourceBudget, mut canceled: impl FnMut() -> bool) -> Result<PagedQueryState, PagedSearchError> {
        self.stats.last_step_loaded_bytes = 0; self.stats.last_step_scanned_bytes = 0;
        if active_generation != self.options.generation { self.cancel(); return Err(PagedSearchError::StaleQuery); }
        if canceled() { self.cancel(); return Err(PagedSearchError::Canceled); }
        if self.state != PagedQueryState::Pending { return Ok(self.state); }
        if step.max_bytes == 0 || step.max_calls == 0 || step.max_hits == 0 { return Ok(self.state); }
        let result = self.advance(step, budget, &mut canceled);
        if let Err(error) = result {
            self.active = None;
            self.state = if matches!(error, PagedSearchError::Canceled | PagedSearchError::Archive(PagedSnapshotError::Canceled)) {
                PagedQueryState::Canceled
            } else { PagedQueryState::Failed(error) };
            return Err(error);
        }
        if canceled() { self.cancel(); return Err(PagedSearchError::Canceled); }
        Ok(self.state)
    }
    fn advance(&mut self, step: StreamReadStep, budget: &ResourceBudget,
        canceled: &mut impl FnMut() -> bool) -> Result<(), PagedSearchError> {
        if self.active.is_none() {
            if self.next == self.archive.directory().len() { self.state = PagedQueryState::Complete; return Ok(()); }
            let ordinal = self.next; self.next += 1; self.stats.members_visited += 1;
            let member = self.archive.directory().member(ordinal).ok_or(PagedSearchError::InvalidHit)?;
            if matches!(member.data, PagedMemberData::Unavailable(_)) { return Ok(()); }
            let file = FileId::new(self.options.first_file.owner(), self.options.first_file.get() + ordinal as u64)
                .map_err(|_| PagedSearchError::IdentityExhausted)?;
            let revision = SourceRevision::new(file.owner(), self.options.first_revision.get() + ordinal as u64)
                .map_err(|_| PagedSearchError::IdentityExhausted)?;
            let verified = self.archive.load(ordinal, budget, self.allocations[1], &mut *canceled)?;
            let length = verified.bytes().len();
            self.stats.peak_source_bytes = self.stats.peak_source_bytes.max(length);
            self.stats.last_step_loaded_bytes = length;
            let request = CaptureRequest::new(file, revision).map_err(|_| PagedSearchError::OwnerMismatch)?;
            let mut options = StreamReadOptions::new(self.options.generation);
            options.max_matches = self.options.max_matches - self.hits.len();
            options.max_bytes = length as u64;
            options.max_read_calls = fcb_store::paged_snapshot::MAX_SNAPSHOT_READ_CALLS;
            self.active = Some(ReaderSearch::new(Cursor::new(verified), request, ByteLength::new(length as u64),
                self.needle, options, budget, self.allocations[2])?);
        }
        let query = self.active.as_mut().ok_or(PagedSearchError::Pending)?;
        let before = query.stats().scanned_bytes;
        let state = query.step(step, self.options.generation, &mut *canceled)?;
        let scanned = query.stats().scanned_bytes - before;
        self.stats.scanned_bytes += scanned;
        self.stats.last_step_scanned_bytes = scanned as usize;
        if state == StreamReadState::Pending { return Ok(()); }
        let (cursor, report) = self.active.take().ok_or(PagedSearchError::Pending)?.finish()?;
        let verified = cursor.into_inner();
        match report.state() {
            StreamReadState::Complete | StreamReadState::Truncated => {
                self.stats.files_searched += 1;
                self.matches_seen += report.matches_seen();
                for hit in report.hits() {
                    self.hits.push(PagedHit { ordinal: verified.ordinal(), file: report.request().file(),
                        revision: report.request().revision(), generation: self.options.generation,
                        range: hit.original_range(), archive: verified.archive_digest(), source: verified.source_digest() });
                }
                if report.state() == StreamReadState::Truncated { self.state = PagedQueryState::Truncated; }
            }
            StreamReadState::UnsupportedText => {
                // All text results from an unsupported member are discarded,
                // matching whole-capture semantics. Raw-byte search remains valid.
                self.stats.unsupported_files += 1;
            }
            StreamReadState::Canceled => return Err(PagedSearchError::Canceled),
            StreamReadState::Failed(error) => return Err(error.into()),
            _ => return Err(PagedSearchError::IncompleteInput),
        }
        Ok(())
    }
    /// Only terminal nonfailed output is publishable. A failed digest check or
    /// stale request cannot become an apparently complete partial result table.
    pub fn finish(self) -> Result<PagedReport, PagedSearchError> {
        match self.state {
            PagedQueryState::Pending => return Err(PagedSearchError::Pending),
            PagedQueryState::Canceled => return Err(PagedSearchError::Canceled),
            PagedQueryState::Failed(error) => return Err(error),
            _ => {},
        }
        let directory = self.archive.directory();
        Ok(PagedReport { archive: directory.digest(), generation: self.options.generation,
            discovery_complete: directory.discovery_complete(),
            unavailable: directory.len() - directory.captured_files(),
            truncated: self.state == PagedQueryState::Truncated, hits: self.hits,
            matches_seen: self.matches_seen, stats: self.stats, _lease: self._lease })
    }
}

pub struct PagedReport {
    archive: Sha256Digest, generation: QueryGeneration, discovery_complete: bool,
    unavailable: usize, truncated: bool, hits: Vec<PagedHit>, matches_seen: u64,
    stats: PagedQueryStats, _lease: ResourceLease,
}
impl PagedReport {
    pub fn hits(&self) -> &[PagedHit] { &self.hits }
    pub const fn matches_seen(&self) -> u64 { self.matches_seen }
    pub const fn unavailable_files(&self) -> usize { self.unavailable }
    pub const fn truncated(&self) -> bool { self.truncated }
    pub const fn stats(&self) -> PagedQueryStats { self.stats }
    pub const fn archive_digest(&self) -> Sha256Digest { self.archive }
    pub const fn is_complete(&self) -> bool {
        self.discovery_complete && self.unavailable == 0 && !self.truncated && self.stats.unsupported_files == 0
    }
    pub fn validate_delivery(&self, archive: Sha256Digest, generation: QueryGeneration) -> Result<(), PagedSearchError> {
        if self.archive != archive || self.generation != generation { return Err(PagedSearchError::StaleQuery); }
        Ok(())
    }
}

/// A selected member's exact bytes and their independent lease. It remains
/// readable after the archive and its directory have been released. The facade
/// intentionally does not expose a cloneable uncharged SourceCapture.
pub struct PagedCapture {
    source: SourceCapture, ordinal: usize, archive: Sha256Digest, digest: Sha256Digest, _lease: ResourceLease,
}
impl PagedCapture {
    pub fn load<R: Read + Seek>(archive: &mut PagedSnapshot<R>, ordinal: usize,
        file: FileId, revision: SourceRevision, budget: &ResourceBudget,
        allocations: [ResourceAllocationId; 2], mut canceled: impl FnMut() -> bool) -> Result<Self, PagedSearchError> {
        if file.owner() != archive.directory().owner() || revision.owner() != file.owner() { return Err(PagedSearchError::OwnerMismatch); }
        if allocations[0] == allocations[1] { return Err(PagedSearchError::InvalidLimits); }
        let verified = archive.load(ordinal, budget, allocations[0], &mut canceled)?;
        let entry = archive.directory().member(ordinal).ok_or(PagedSearchError::InvalidHit)?;
        let charge = verified.bytes().len().checked_add(entry.path.len().checked_mul(16).ok_or(PagedSearchError::ResourceDenied)?)
            .and_then(|n| n.checked_add(size_of::<Self>() + 256)).ok_or(PagedSearchError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(file.owner(), allocations[1], ByteLength::new(charge as u64))
            .map_err(|_| PagedSearchError::ResourceDenied)?;
        let path = RawPath::from_bytes(entry.path);
        let source = SourceCapture { owner: file.owner(), file, revision,
            logical_path: path.display_escaped().to_string(), bytes: Arc::from(verified.bytes()) };
        if canceled() { return Err(PagedSearchError::Canceled); }
        Ok(Self { source, ordinal, archive: verified.archive_digest(), digest: verified.source_digest(), _lease: lease })
    }
    pub fn open_hit<R: Read + Seek>(archive: &mut PagedSnapshot<R>, hit: PagedHit,
        generation: QueryGeneration, budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
        canceled: impl FnMut() -> bool) -> Result<Self, PagedSearchError> {
        if hit.generation != generation || hit.archive != archive.directory().digest() { return Err(PagedSearchError::StaleQuery); }
        let capture = Self::load(archive, hit.ordinal, hit.file, hit.revision, budget, allocations, canceled)?;
        capture.hit_bytes(hit)?;
        Ok(capture)
    }
    pub fn bytes(&self) -> &[u8] { self.source.bytes() }
    pub const fn file(&self) -> FileId { self.source.file() }
    pub const fn revision(&self) -> SourceRevision { self.source.revision() }
    pub const fn source_digest(&self) -> Sha256Digest { self.digest }
    pub fn hit_bytes(&self, hit: PagedHit) -> Result<&[u8], PagedSearchError> {
        if hit.ordinal != self.ordinal || hit.file != self.file() || hit.revision != self.revision()
            || hit.archive != self.archive || hit.source != self.digest { return Err(PagedSearchError::InvalidHit); }
        let (start, end) = hit.range.as_usize_bounds().map_err(|_| PagedSearchError::InvalidHit)?;
        self.bytes().get(start..end).ok_or(PagedSearchError::InvalidHit)
    }
    pub fn frame_plan(&self) -> Result<FramePlan, PagedSearchError> {
        Ok(FramePlan::new(self.source.owner(), self.file(), self.revision(),
            self.source.byte_range().map_err(|_| PagedSearchError::InvalidHit)?))
    }
    pub fn reader(&self, limits: ReaderLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<PagedReader<'_>, ReaderError> {
        Ok(PagedReader { inner: SourceReader::new(&self.source, limits, budget, allocation)? })
    }
}
/// Restricts the ordinary reader to borrowed outputs, keeping the selected
/// capture's lease alive. There is no SourceCapture getter or Deref escape.
pub struct PagedReader<'a> { inner: SourceReader<'a> }
impl<'a> PagedReader<'a> {
    pub fn progress(&self) -> ReaderIndexProgress { self.inner.progress() }
    pub fn index_step(&mut self, max_bytes: usize, canceled: impl FnMut() -> bool) -> Result<ReaderIndexProgress, ReaderError> {
        self.inner.index_step(max_bytes, canceled)
    }
    pub fn seek(&self, target: ReadingTarget, generation: QueryGeneration) -> Result<ReadingSeek<'a>, ReaderError> {
        self.inner.seek(target, generation)
    }
    pub fn window(&self, anchor: ReadingAnchor, generation: QueryGeneration, options: ReadingWindowOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId, canceled: impl FnMut() -> bool) -> Result<ReadingWindow<'a>, ReaderError> {
        self.inner.window(anchor, generation, options, budget, allocation, canceled)
    }
}

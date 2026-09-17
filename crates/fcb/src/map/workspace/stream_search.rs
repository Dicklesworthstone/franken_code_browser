#![forbid(unsafe_code)]

//! Resumable whole-file text search over the SAME catalog as a retained atlas.
//! Hosts supply authorized open files; this module never resolves a pathname.
//! One step opens at most one file and drives at most one existing FileSearch
//! quantum. Source length is absent from retained-memory admission. Completed
//! files retain compact status/hit records and exact literal witnesses, never
//! decoder buffers, complete captures, or invented surrounding source.

mod report;
pub use report::{AtlasStreamFile, AtlasStreamFileState, AtlasStreamHit,
    AtlasStreamReport, AtlasStreamSelection, AtlasStreamStats};

use std::{fs::File, mem::size_of};
use fcb_core::{ByteLength, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use crate::search::{CaptureRequest, FileSearch, FileSearchError, FileSearchReport,
    StreamReadError, StreamReadOptions, StreamReadState, StreamReadStats, StreamReadStep, StreamingNeedle};
use crate::source::{SourceError, path::NormalizedPath};
use super::{WorkspaceAtlas, WorkspaceAtlasError};

pub const MAX_ATLAS_STREAM_MATCHES: usize = 4096;
pub const MAX_ATLAS_STREAM_NEEDLE_BYTES: usize = 1024;
pub const MAX_ATLAS_STREAM_BYTES: u64 = 1u64 << 40;
pub const MAX_ATLAS_STREAM_CALLS: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasStreamLimits {
    /// Global retained occurrences. A full buffer still allows one lookahead
    /// witness before declaring truncation, including across file boundaries.
    pub max_matches: usize,
    /// Actual source I/O, including bytes spent before a file failed.
    pub max_bytes: u64,
    /// Includes interrupted reads. Opening/metadata work is bounded by the
    /// already-admitted catalog and by one new file per nonzero step.
    pub max_read_calls: u64,
}
impl Default for AtlasStreamLimits {
    fn default() -> Self {
        Self { max_matches: 100, max_bytes: 256 * 1024 * 1024,
            max_read_calls: MAX_ATLAS_STREAM_CALLS }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasStreamState { Pending, Finished, Canceled, Failed }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasStreamStop { MatchLimit, ByteLimit, ReadCallLimit }
impl AtlasStreamStop {
    pub const fn code(self) -> &'static str {
        match self { Self::MatchLimit => "match-limit", Self::ByteLimit => "byte-limit",
            Self::ReadCallLimit => "read-call-limit" }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AtlasStreamError {
    Atlas(WorkspaceAtlasError), File(FileSearchError), InvalidLimits,
    ResourceDenied, AllocationFailed, Canceled, StaleQuery, WrongAtlas, Pending, InvalidHit,
}
impl std::fmt::Display for AtlasStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(e) => write!(f, "{e}"), Self::File(e) => write!(f, "{e}"),
            other => f.write_str(match other {
                Self::InvalidLimits => "ATLAS_STREAM_INVALID_LIMITS",
                Self::ResourceDenied => "ATLAS_STREAM_RESOURCE_DENIED",
                Self::AllocationFailed => "ATLAS_STREAM_ALLOCATION_FAILED",
                Self::Canceled => "ATLAS_STREAM_CANCELED", Self::StaleQuery => "ATLAS_STREAM_STALE_QUERY",
                Self::WrongAtlas => "ATLAS_STREAM_WRONG_ATLAS", Self::Pending => "ATLAS_STREAM_PENDING",
                Self::InvalidHit => "ATLAS_STREAM_INVALID_HIT", _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for AtlasStreamError {}
impl From<WorkspaceAtlasError> for AtlasStreamError { fn from(e: WorkspaceAtlasError) -> Self { Self::Atlas(e) } }
impl From<FileSearchError> for AtlasStreamError { fn from(e: FileSearchError) -> Self { Self::File(e) } }

/// The compiled literal and worker budget outlive this job, but not its finished
/// report. Allocate a fresh first_revision and query generation for each new
/// observation. A pathname/length match never reuses an old source observation.
pub struct AtlasStreamSearch<'atlas, 'catalog, 'worker> {
    report: AtlasStreamReport<'atlas, 'catalog>,
    needle: &'worker StreamingNeedle,
    budget: &'worker ResourceBudget,
    worker_allocation: ResourceAllocationId,
    first_revision: SourceRevision,
    active: Option<FileSearch<'worker>>,
}
impl<'atlas, 'catalog, 'worker> AtlasStreamSearch<'atlas, 'catalog, 'worker> {
    /// Allocations: [compact retained report, one reusable file-search worker].
    /// Build the StreamingNeedle separately; only exact text is admitted here.
    /// UTF-8 and BOM-marked UTF-16 reuse the shared engine's encoding contract.
    pub fn new(atlas: &'atlas WorkspaceAtlas<'catalog>, needle: &'worker StreamingNeedle,
        first_revision: SourceRevision, generation: QueryGeneration, limits: AtlasStreamLimits,
        budget: &'worker ResourceBudget, allocations: [ResourceAllocationId; 2]) -> Result<Self, AtlasStreamError> {
        atlas.validate_active()?;
        let text = needle.text_value().ok_or(AtlasStreamError::InvalidLimits)?;
        let count = atlas.catalog().entries().len();
        if text.is_empty() || text.len() > MAX_ATLAS_STREAM_NEEDLE_BYTES
            || limits.max_matches > MAX_ATLAS_STREAM_MATCHES || limits.max_bytes > MAX_ATLAS_STREAM_BYTES
            || limits.max_read_calls > MAX_ATLAS_STREAM_CALLS || allocations[0] == allocations[1]
            || generation.owner() != atlas.layout().owner() || first_revision.owner() != atlas.layout().owner()
            || first_revision.get().checked_add(count.saturating_sub(1) as u64).is_none() {
            return Err(AtlasStreamError::InvalidLimits);
        }
        let witness_files = count.min(limits.max_matches);
        // UTF-16 needs at most twice the UTF-8 literal's byte length. Store one
        // encoded witness per matched file, not a decoder lease for every file.
        let witness_bytes = witness_files.checked_mul(text.len()).and_then(|n| n.checked_mul(2))
            .ok_or(AtlasStreamError::InvalidLimits)?;
        let charge = count.checked_mul(size_of::<AtlasStreamFile>())
            .and_then(|n| limits.max_matches.checked_mul(size_of::<AtlasStreamHit>()).and_then(|h| n.checked_add(h)))
            .and_then(|n| witness_files.checked_mul(size_of::<usize>()).and_then(|h| n.checked_add(h)))
            .and_then(|n| n.checked_add(witness_bytes))
            .and_then(|n| n.checked_add(text.len() + size_of::<Self>() + size_of::<AtlasStreamReport<'_, '_>>()))
            .ok_or(AtlasStreamError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(atlas.layout().owner(), allocations[0], ByteLength::new(charge as u64))
            .map_err(|_| AtlasStreamError::ResourceDenied)?;
        let mut literal = String::new();
        literal.try_reserve_exact(text.len()).map_err(|_| AtlasStreamError::AllocationFailed)?;
        if literal.capacity() > text.len() { return Err(AtlasStreamError::ResourceDenied); }
        literal.push_str(text);
        let report = AtlasStreamReport { atlas, generation, limits, files: reserve(count)?,
            hits: reserve(limits.max_matches)?, matched_files: reserve(witness_files)?,
            witnesses: reserve(witness_bytes)?, literal, stats: AtlasStreamStats::default(),
            state: if count == 0 { AtlasStreamState::Finished } else { AtlasStreamState::Pending },
            stop: None, _lease: lease };
        Ok(Self { report, needle, budget, worker_allocation: allocations[1], first_revision, active: None })
    }
    pub fn state(&self) -> AtlasStreamState { self.report.state }
    /// Only finalized files contribute hit rows. Active-file I/O is already
    /// included in stats, so cancellation/failure cannot hide spent I/O.
    pub fn report(&self) -> &AtlasStreamReport<'atlas, 'catalog> { &self.report }
    pub fn cancel(&mut self) { self.active = None; self.report.state = AtlasStreamState::Canceled; }

    /// Supply files using the given exact catalog path/request; callback paths
    /// are data, never permission to widen scope. The host remains responsible
    /// for grant-aware opening and native confinement. No callback is retried.
    /// A zero byte/call/hit quantum performs no open, read, or matching work.
    pub fn step(&mut self, quantum: StreamReadStep, generation: QueryGeneration,
        mut open: impl FnMut(CaptureRequest, &NormalizedPath) -> Result<File, SourceError>,
        mut canceled: impl FnMut() -> bool) -> Result<AtlasStreamState, AtlasStreamError> {
        if generation != self.report.generation { self.cancel(); return Err(AtlasStreamError::StaleQuery); }
        if let Err(e) = self.report.atlas.validate_active() { self.cancel(); return Err(e.into()); }
        if canceled() { self.cancel(); return Err(AtlasStreamError::Canceled); }
        if self.state() != AtlasStreamState::Pending { return Ok(self.state()); }
        if quantum.max_bytes == 0 || quantum.max_calls == 0 || quantum.max_hits == 0 { return Ok(self.state()); }
        let result = self.advance(quantum, &mut open, &mut canceled);
        if result.is_err() && self.state() == AtlasStreamState::Pending {
            self.active = None; self.report.state = AtlasStreamState::Failed;
        }
        result
    }
    fn advance(&mut self, quantum: StreamReadStep,
        open: &mut impl FnMut(CaptureRequest, &NormalizedPath) -> Result<File, SourceError>,
        canceled: &mut impl FnMut() -> bool) -> Result<AtlasStreamState, AtlasStreamError> {
        let ordinal = self.report.files.len();
        if self.active.is_none() {
            if ordinal == self.report.atlas.file_count() { self.report.state = AtlasStreamState::Finished; return Ok(self.state()); }
            let stop = if self.report.stats.bytes_read >= self.report.limits.max_bytes { Some(AtlasStreamStop::ByteLimit) }
                else if self.report.stats.read_calls >= self.report.limits.max_read_calls { Some(AtlasStreamStop::ReadCallLimit) }
                else { None };
            if let Some(stop) = stop { self.stop(stop); return Ok(self.state()); }
            let catalog = self.report.atlas.catalog();
            let file = catalog.file_id(ordinal).ok_or(AtlasStreamError::InvalidLimits)?;
            let revision = SourceRevision::new(file.owner(), self.first_revision.get() + ordinal as u64)
                .map_err(|_| AtlasStreamError::InvalidLimits)?;
            let request = CaptureRequest::new(file, revision).map_err(|e| AtlasStreamError::File(e.into()))?;
            let opened = open(request, catalog.entries()[ordinal].path());
            if let Err(e) = self.report.atlas.validate_active() { self.cancel(); return Err(e.into()); }
            if canceled() { self.cancel(); return Err(AtlasStreamError::Canceled); }
            let options = StreamReadOptions { generation: self.report.generation, encoding: None,
                max_matches: self.report.limits.max_matches - self.report.hits.len(),
                max_bytes: self.report.limits.max_bytes - self.report.stats.bytes_read,
                max_read_calls: self.report.limits.max_read_calls - self.report.stats.read_calls };
            let started = match opened {
                Ok(file) => FileSearch::new(file, request, self.needle, options, self.budget, self.worker_allocation),
                Err(e) => Err(FileSearchError::Source(e)),
            };
            match started {
                Ok(search) => self.active = Some(search),
                Err(FileSearchError::Source(SourceError::Canceled)) => { self.cancel(); return Err(AtlasStreamError::Canceled); }
                Err(e @ FileSearchError::Stream(StreamReadError::OwnerMismatch)) => return Err(e.into()),
                Err(error) => {
                    self.record_unavailable(request, StreamReadStats::default(), error)?;
                    self.finish_if_done(); return Ok(self.state());
                }
            }
        }
        let atlas = self.report.atlas;
        let search = self.active.as_mut().ok_or(AtlasStreamError::InvalidLimits)?;
        let before = search.stats();
        let mut stop = || canceled() || atlas.validate_active().is_err();
        let result = search.step(quantum, self.report.generation, &mut stop);
        let after = search.stats();
        let state = search.state();
        self.report.stats.add_step(before, after)?;
        if let Err(e) = atlas.validate_active() { self.cancel(); return Err(e.into()); }
        if canceled() || state == StreamReadState::Canceled { self.cancel(); return Err(AtlasStreamError::Canceled); }
        if state == StreamReadState::Pending {
            // Never loop forever if an engine error failed to terminalize.
            result?; return Ok(self.state());
        }
        let search = self.active.take().ok_or(AtlasStreamError::InvalidLimits)?;
        // FileSearch retains I/O/decoder failure as a terminal report, so earlier
        // exact witnesses and spent bytes survive a failed source observation.
        let report = search.finish()?;
        self.record(&report)?;
        match state {
            StreamReadState::Truncated => self.stop(AtlasStreamStop::MatchLimit),
            StreamReadState::ByteLimit => self.stop(AtlasStreamStop::ByteLimit),
            StreamReadState::CallLimit => self.stop(AtlasStreamStop::ReadCallLimit),
            _ => self.finish_if_done(),
        }
        Ok(self.state())
    }
    fn stop(&mut self, reason: AtlasStreamStop) { self.report.stop = Some(reason); self.report.state = AtlasStreamState::Finished; }
    fn finish_if_done(&mut self) {
        if self.report.files.len() == self.report.atlas.file_count() { self.report.state = AtlasStreamState::Finished; }
    }
    fn record_unavailable(&mut self, request: CaptureRequest, stats: StreamReadStats,
        error: FileSearchError) -> Result<(), AtlasStreamError> {
        let node = self.report.atlas.node_for_file(request.file())?;
        self.report.files.push(AtlasStreamFile { request, node,
            state: AtlasStreamFileState::Unavailable(error), encoding: None, observed_length: None,
            final_length: None, consistency: None, unsupported_at: None, stats,
            matches_seen: 0, first_hit: self.report.hits.len(), hit_count: 0,
            witness_start: self.report.witnesses.len(), witness_len: 0 });
        self.report.stats.incomplete_files += 1;
        Ok(())
    }
    fn record(&mut self, file_report: &FileSearchReport<'_>) -> Result<(), AtlasStreamError> {
        let scan = file_report.search();
        let count = scan.hits().len();
        if count > self.report.limits.max_matches - self.report.hits.len() { return Err(AtlasStreamError::InvalidLimits); }
        let node = self.report.atlas.node_for_file(scan.request().file())?;
        let witness_start = self.report.witnesses.len();
        let bytes = if count > 0 { scan.witness_bytes(0).map_err(FileSearchError::from)? } else { &[] };
        if bytes.len() > self.report.witnesses.capacity() - witness_start { return Err(AtlasStreamError::ResourceDenied); }
        let file_record = self.report.files.len();
        let first_hit = self.report.hits.len();
        for hit in scan.hits() {
            if hit.original_range().len().get() != bytes.len() as u64 { return Err(AtlasStreamError::InvalidHit); }
            self.report.hits.push(AtlasStreamHit { node, request: scan.request(), file_record,
                occurrence_id: hit.occurrence_id(), original_range: hit.original_range() });
        }
        if count > 0 {
            self.report.witnesses.extend_from_slice(bytes);
            self.report.matched_files.push(file_record);
        }
        self.report.files.push(AtlasStreamFile { request: scan.request(), node,
            state: AtlasStreamFileState::Scanned(scan.state()), encoding: scan.encoding(),
            observed_length: Some(scan.observed_length()), final_length: file_report.final_length(),
            consistency: Some(file_report.consistency()), unsupported_at: scan.unsupported_at(), stats: scan.stats(),
            matches_seen: scan.matches_seen(), first_hit, hit_count: count, witness_start, witness_len: bytes.len() });
        self.report.stats.matches_seen = self.report.stats.matches_seen.checked_add(scan.matches_seen())
            .ok_or(AtlasStreamError::InvalidLimits)?;
        if !file_report.is_complete() { self.report.stats.incomplete_files += 1; }
        Ok(())
    }
    /// Only a terminal, noncanceled operation may publish a standalone report.
    /// Partial coverage is still terminal and remains explicit in is_complete().
    pub fn finish(self) -> Result<AtlasStreamReport<'atlas, 'catalog>, AtlasStreamError> {
        match self.report.state {
            AtlasStreamState::Finished => { self.report.atlas.validate_active()?; Ok(self.report) }
            AtlasStreamState::Canceled => Err(AtlasStreamError::Canceled),
            _ => Err(AtlasStreamError::Pending),
        }
    }
}
fn reserve<T>(capacity: usize) -> Result<Vec<T>, AtlasStreamError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| AtlasStreamError::AllocationFailed)?;
    if values.capacity() > capacity { return Err(AtlasStreamError::ResourceDenied); }
    Ok(values)
}

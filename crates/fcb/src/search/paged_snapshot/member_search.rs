#![forbid(unsafe_code)]

//! Shared member pipeline for saved literal and expression search. The existing
//! ReaderSearch owns all decoding/matching. A separate bounded integrity drain
//! completes the SHA-256 observation when matching stops before member EOF.

use super::PagedSearchError;
use crate::search::streaming::{MAX_STREAM_STEP_CALLS, StreamReadReport};
use crate::search::{
    CaptureRequest, ReaderSearch, StreamReadOptions, StreamReadState, StreamReadStep,
    StreamingNeedle,
};
use fcb_core::{ByteLength, QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_store::paged_snapshot::{
    MemberReadState, MemberReader, SNAPSHOT_READ_BYTES, SnapshotIoStats, SnapshotReadSession,
    VerifiedMemberIdentity,
};
use std::{
    io::{self, Read, Seek},
    mem::size_of,
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MemberSearchStats {
    pub io: SnapshotIoStats,
    pub scanned_bytes: u64,
    pub peak_source_bytes: usize,
    pub last_step_loaded_bytes: usize,
    pub last_step_read_calls: usize,
    pub last_step_scanned_bytes: u64,
}

/// Owns only fixed-capacity scratch, matcher state and bounded result witnesses.
/// The complete member remains in the borrowed archive. Reports cannot escape
/// until every original byte from the SAME stream has passed digest validation.
pub(crate) struct MemberSearch<'archive, 'needle, R: Read + Seek> {
    scan: Option<ReaderSearch<'needle, MemberReader<'archive, R>>>,
    reader: Option<MemberReader<'archive, R>>,
    report: Option<StreamReadReport<'needle>>,
    scratch: Vec<u8>,
    stats: MemberSearchStats,
    _lease: ResourceLease,
}
impl<'archive, 'needle, R: Read + Seek> MemberSearch<'archive, 'needle, R> {
    pub(crate) fn new(
        session: SnapshotReadSession<'archive, R>,
        ordinal: usize,
        request: CaptureRequest,
        needle: &'needle StreamingNeedle,
        options: StreamReadOptions,
        skip_matches: u64,
        count_all_matches: bool,
        verify_only: bool,
        budget: &ResourceBudget,
        allocations: [ResourceAllocationId; 2],
    ) -> Result<Self, PagedSearchError> {
        let owner = session.directory().owner();
        let charge = size_of::<Self>()
            .checked_add(SNAPSHOT_READ_BYTES)
            .ok_or(PagedSearchError::ResourceDenied)?;
        // Reserve the integrity-drain buffer before the first input read,
        // including overlap with the still-retained matcher report and output.
        let lease = budget
            .try_reserve_managed(owner, allocations[0], ByteLength::new(charge as u64))
            .map_err(|_| PagedSearchError::ResourceDenied)?;
        let reader = session.member(ordinal)?;
        let length = ByteLength::new(reader.byte_length() as u64);
        let (scan, reader) = if verify_only {
            (None, Some(reader))
        } else {
            (
                Some(ReaderSearch::new_page(
                    reader,
                    request,
                    length,
                    needle,
                    options,
                    skip_matches,
                    count_all_matches,
                    budget,
                    allocations[1],
                )?),
                None,
            )
        };
        Ok(Self {
            scan,
            reader,
            report: None,
            scratch: Vec::new(),
            stats: MemberSearchStats::default(),
            _lease: lease,
        })
    }

    pub(crate) const fn stats(&self) -> MemberSearchStats {
        self.stats
    }

    /// One step performs either one bounded matcher batch/read or one bounded
    /// integrity-drain read, never both. Interrupted calls consume max_calls;
    /// zero allowances do no I/O. Terminal calls never reread the member.
    pub(crate) fn step(
        &mut self,
        step: StreamReadStep,
        generation: QueryGeneration,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<bool, PagedSearchError> {
        self.stats.last_step_loaded_bytes = 0;
        self.stats.last_step_read_calls = 0;
        self.stats.last_step_scanned_bytes = 0;
        if canceled() {
            return Err(PagedSearchError::Canceled);
        }
        if step.max_bytes == 0 || step.max_calls == 0 || step.max_hits == 0 {
            return Ok(false);
        }
        let before = self.io_stats();
        let scanned_before = self.stats.scanned_bytes;
        let result = self.advance(step, generation, &mut canceled);
        let after = self.io_stats();
        self.stats.io = after;
        self.stats.last_step_loaded_bytes =
            after.bytes_read.saturating_sub(before.bytes_read) as usize;
        self.stats.last_step_read_calls =
            after.read_calls.saturating_sub(before.read_calls) as usize;
        self.stats.peak_source_bytes = self
            .stats
            .peak_source_bytes
            .max(self.stats.last_step_loaded_bytes);
        self.stats.last_step_scanned_bytes =
            self.stats.scanned_bytes.saturating_sub(scanned_before);
        if canceled() {
            return Err(PagedSearchError::Canceled);
        }
        result
    }

    fn io_stats(&self) -> SnapshotIoStats {
        if let Some(scan) = &self.scan {
            scan.reader().stats()
        } else {
            self.reader
                .as_ref()
                .map_or(SnapshotIoStats::default(), MemberReader::stats)
        }
    }

    fn advance(
        &mut self,
        step: StreamReadStep,
        generation: QueryGeneration,
        canceled: &mut impl FnMut() -> bool,
    ) -> Result<bool, PagedSearchError> {
        if let Some(scan) = self.scan.as_mut() {
            let result = scan.step(step, generation, &mut *canceled);
            self.stats.scanned_bytes = scan.stats().scanned_bytes;
            self.stats.peak_source_bytes = self
                .stats
                .peak_source_bytes
                .max(scan.stats().peak_buffer_bytes);
            // Preserve the archive's precise integrity/I/O failure instead of
            // collapsing the Read adapter's error into a generic matcher I/O.
            if let MemberReadState::Failed(error) = scan.reader().state() {
                return Err(error.into());
            }
            if result? == StreamReadState::Pending {
                return Ok(false);
            }
            let (reader, report) = self
                .scan
                .take()
                .ok_or(PagedSearchError::Pending)?
                .finish()?;
            let state = report.state();
            self.reader = Some(reader);
            self.report = Some(report);
            match state {
                StreamReadState::Canceled => return Err(PagedSearchError::Canceled),
                StreamReadState::Failed(error) => return Err(error.into()),
                _ => {}
            }
            return Ok(self
                .reader
                .as_ref()
                .is_some_and(|reader| reader.state() == MemberReadState::Verified));
        }
        let reader = self.reader.as_mut().ok_or(PagedSearchError::Pending)?;
        match reader.state() {
            MemberReadState::Verified => return Ok(true),
            MemberReadState::Failed(error) => return Err(error.into()),
            MemberReadState::Pending => {}
        }
        if self.scratch.is_empty() {
            let capacity = SNAPSHOT_READ_BYTES.min(reader.byte_length());
            self.scratch
                .try_reserve_exact(capacity)
                .map_err(|_| PagedSearchError::ResourceDenied)?;
            if self.scratch.capacity() > capacity {
                return Err(PagedSearchError::ResourceDenied);
            }
            self.scratch.resize(capacity, 0);
        }
        let count = step
            .max_bytes
            .min(self.scratch.len())
            .min(reader.remaining_bytes());
        for _ in 0..step.max_calls.min(MAX_STREAM_STEP_CALLS) {
            if canceled() {
                return Err(PagedSearchError::Canceled);
            }
            match reader.read(&mut self.scratch[..count]) {
                Ok(_) => break,
                Err(error)
                    if error.kind() == io::ErrorKind::Interrupted
                        && reader.state() == MemberReadState::Pending => {}
                Err(_) => match reader.state() {
                    MemberReadState::Failed(error) => return Err(error.into()),
                    _ => return Err(PagedSearchError::IncompleteInput),
                },
            }
        }
        match reader.state() {
            MemberReadState::Verified => Ok(true),
            MemberReadState::Failed(error) => Err(error.into()),
            MemberReadState::Pending => Ok(false),
        }
    }

    pub(crate) fn finish(
        mut self,
    ) -> Result<
        (
            SnapshotReadSession<'archive, R>,
            VerifiedMemberIdentity,
            Option<StreamReadReport<'needle>>,
        ),
        PagedSearchError,
    > {
        if self.scan.is_some() {
            return Err(PagedSearchError::Pending);
        }
        let reader = self.reader.take().ok_or(PagedSearchError::Pending)?;
        let (session, identity) = reader.finish()?;
        Ok((session, identity, self.report.take()))
    }
}

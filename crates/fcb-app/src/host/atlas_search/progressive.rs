#![forbid(unsafe_code)]

//! One-file scheduling slices over the existing bounded capture/text pipeline.
//! There is no self-referential reader, new matcher, executor, timer or thread.
//! A slice examines at most one catalog entry and captures at most the admitted
//! per-file byte limit (hard maximum 1 MiB). Cancellation is also checked inside
//! source read and exact-scan chunks. OS calls have no invented latency deadline.

use super::*;
use crate::workspace;
use fcb::search::{CaptureRequest, QueryGeneration, ReaderSearch, StreamReadOptions,
    StreamReadState, StreamReadStep, StreamingNeedle};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasSearchStop { AllFilesExamined, FileLimit, SourceByteLimit, MatchLimit, VerificationByteLimit }
impl AtlasSearchStop {
    pub const fn code(self) -> &'static str {
        match self {
            Self::AllFilesExamined => "all-files-examined",
            Self::FileLimit => "file-limit", Self::SourceByteLimit => "source-byte-limit",
            Self::MatchLimit => "match-limit", Self::VerificationByteLimit => "verification-byte-limit",
        }
    }
}

/// Running/finished is independent from completeness. A finished query may
/// still have unavailable files, undiscovered membership or a limiting quota.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasSearchProgress {
    pub generation: u64,
    pub stop_reason: Option<AtlasSearchStop>,
    pub complete: bool,
    pub truncated: bool,
    pub examined_files: usize,
    pub scanned_files: usize,
    pub unavailable_files: usize,
    pub pending_files: usize,
    /// Observed occurrences, including any unstored lookahead. This is not an
    /// exhaustive total when coverage is partial; retained_hits is separate.
    pub matches_seen: u64,
    pub retained_hits: usize,
    pub retained_source_bytes: usize,
    pub source_bytes_read: u64,
    pub read_calls: u64,
    pub step_count: u64,
    pub last_step_files: usize,
    pub last_step_source_bytes: u64,
    pub last_step_read_calls: u64,
}
impl AtlasSearchProgress {
    pub const fn is_running(self) -> bool { self.stop_reason.is_none() }
}

pub(super) struct SearchWork {
    pub(super) snapshot: Snapshot,
    pattern: StreamingNeedle,
    options: AtlasSearchOptions,
    scan_id: ResourceAllocationId,
    io: workspace::IoCounts,
}

impl RetainedAtlasSearch {
    pub fn pending_generation(&self) -> Option<u64> {
        self.pending.as_ref().map(|work| work.snapshot.generation)
    }
    /// No scan, source read, result allocation or source cloning occurs here.
    pub fn progress(&self, atlas: &AtlasSession, generation: u64)
        -> Result<AtlasSearchProgress, AtlasSearchError> {
        self.validate(atlas)?;
        let q = self.snapshot(generation)?;
        Ok(AtlasSearchProgress { generation: q.generation, stop_reason: q.stop_reason,
            complete: q.complete, truncated: q.truncated, examined_files: q.examined,
            scanned_files: q.scanned, unavailable_files: q.unavailable, pending_files: q.pending,
            matches_seen: q.matches_seen, retained_hits: q.hits.len(), retained_source_bytes: q.retained_bytes,
            source_bytes_read: q.source_bytes_read, read_calls: q.read_calls,
            step_count: q.step_count, last_step_files: q.last_step_files,
            last_step_source_bytes: q.last_step_bytes, last_step_read_calls: q.last_step_calls })
    }
    /// Retire only the running replacement. Finished-query rows/captures and
    /// independently opened readers survive. This may free source memory: call
    /// on a worker. Native cancellation can defer this until an operation drains.
    pub fn cancel_pending(&mut self) -> bool { self.pending.take().is_some() }

    /// Admit a new request without source I/O. Its generation supersedes any
    /// running request, including if admission fails. The last finished query
    /// remains available by its own generation until this replacement finishes.
    pub fn begin(&mut self, atlas: &AtlasSession, generation: u64, needle: &str,
        options: AtlasSearchOptions, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let work = self.prepare_work(atlas, generation, needle, options, &mut canceled)?;
        let response = self.work_response(atlas, &work, "begin", &mut canceled)?;
        self.pending = Some(work);
        Ok(response)
    }

    /// Advance at most ONE file, release control, and expose append-only exact
    /// progress. Hosts can page, open a hit or move the camera between calls.
    /// Stale steps never discard a newer job. A failed/canceled matching step
    /// retires its provisional progress and leaves the finished query untouched.
    /// After success, query progress/page reconciles a lost response; repeating
    /// a terminal step does no more source work.
    pub fn step(&mut self, atlas: &AtlasSession, generation: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?;
        if self.pending_generation() != Some(generation) {
            if self.pending.is_none() && self.last_attempt == generation
                && self.accepted_generation() == Some(generation) {
                return self.page(atlas, generation, 0, 64, canceled);
            }
            return Err(AtlasSearchError::StaleQuery);
        }
        // Taking the job makes every error path fail closed, without exposing
        // unencoded/half-built mutations. Only successful output reinstalls it.
        let mut work = self.pending.take().ok_or(AtlasSearchError::MissingQuery)?;
        self.advance_work(atlas, &mut work, &mut canceled)?;
        let response = self.work_response(atlas, &work, "step", &mut canceled)?;
        if work.snapshot.stop_reason.is_some() { self.accepted = Some(work.snapshot); }
        else { self.pending = Some(work); }
        Ok(response)
    }

    pub(super) fn prepare_work(&mut self, atlas: &AtlasSession, generation: u64,
        needle: &str, options: AtlasSearchOptions, canceled: &mut impl FnMut() -> bool)
        -> Result<SearchWork, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?;
        options.validate()?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES {
            return Err(AtlasSearchError::InvalidLimits);
        }
        check(canceled)?;
        let [state_id, pattern_id, scan_id] = [self.next_id()?, self.next_id()?, self.next_id()?];
        let capacity = options.max_matches.min(options.max_files);
        // Before any source allocation/read, cover old/new capture conversion
        // overlap, retained occurrence capacities and work state. The pattern,
        // range reader, exact scanner and encoded responses share this budget.
        let charge = 2 * options.max_source_bytes + capacity * (size_of::<RetainedFile>() + 64)
            + options.max_matches * size_of::<AtlasSearchHit>()
            + MAX_DIAGNOSTICS * size_of::<Unavailable>() + needle.len() + size_of::<SearchWork>();
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), state_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let snapshot = Snapshot { generation, needle: copy_text(needle)?, files: reserve(capacity)?,
            hits: reserve(options.max_matches)?, diagnostics: reserve(MAX_DIAGNOSTICS)?,
            examined: 0, scanned: 0, unavailable: 0, pending: atlas.atlas().catalog().entries().len(),
            matches_seen: 0, source_bytes_read: 0, read_calls: 0, retained_bytes: 0,
            truncated: false, complete: false, stop_reason: None, step_count: 0,
            last_step_files: 0, last_step_bytes: 0, last_step_calls: 0, index_usage: None, _lease: lease };
        let pattern = StreamingNeedle::text(self.manifest.owner(), needle, &self.budget, pattern_id)
            .map_err(AppError::from)?;
        self.validate(atlas)?; check(canceled)?;
        Ok(SearchWork { snapshot, pattern, options, scan_id, io: workspace::IoCounts::default() })
    }

    pub(super) fn work_response(&mut self, atlas: &AtlasSession, work: &SearchWork,
        command: &str, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        let mut out = self.output(command)?;
        encode_page(&mut out, atlas, &work.snapshot, 0, 64, canceled)?;
        self.finish(atlas, out, !work.snapshot.complete, canceled)
    }

    pub(super) fn advance_work(&self, atlas: &AtlasSession, work: &mut SearchWork,
        canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        self.validate(atlas)?; check(canceled)?;
        let before_files = work.snapshot.examined;
        let before_bytes = work.io.bytes;
        let before_calls = work.io.calls;
        update_coverage(atlas, work);
        if work.snapshot.stop_reason.is_none() {
            self.scan_one_file(atlas, work, canceled)?;
        }
        self.validate(atlas)?; check(canceled)?;
        work.snapshot.step_count += 1; // At most max_files + one terminal check.
        work.snapshot.last_step_files = work.snapshot.examined - before_files;
        work.snapshot.last_step_bytes = work.io.bytes - before_bytes;
        work.snapshot.last_step_calls = work.io.calls - before_calls;
        update_coverage(atlas, work);
        Ok(())
    }

    fn scan_one_file(&self, atlas: &AtlasSession, work: &mut SearchWork,
        canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        let catalog = atlas.atlas().catalog();
        let ordinal = work.snapshot.examined;
        let entry = catalog.entries().get(ordinal).ok_or(AtlasSearchError::WrongAtlas)?;
        let file = catalog.file_id(ordinal).ok_or(AtlasSearchError::WrongAtlas)?;
        let candidate = &mut work.snapshot;
        candidate.examined += 1;
        if entry.observed_bytes() > work.options.max_file_bytes as u64 {
            candidate.unavailable(file, "FILE_BYTE_LIMIT");
            return Ok(());
        }
        let qgen = QueryGeneration::new(self.manifest.owner(), candidate.generation)
            .map_err(|_| AtlasSearchError::IdentityExhausted)?;
        let revision = SourceRevision::new(self.manifest.owner(), candidate.generation)
            .map_err(|_| AtlasSearchError::IdentityExhausted)?;
        let request = CaptureRequest::new(file, revision).map_err(|_| AtlasSearchError::WrongAtlas)?;
        let root = catalog.grant().root_path().to_path_buf();
        let captured = workspace::read_capture(&root, request, entry.path(), work.options.max_file_bytes,
            work.options.max_source_bytes as u64, &mut work.io, &self.budget,
            &mut || canceled() || atlas.validate_active().is_err());
        self.validate(atlas)?; check(canceled)?;
        let capture = match captured {
            Ok(capture) => capture,
            Err(fcb::source::SourceError::Canceled) => return Err(AtlasSearchError::Canceled),
            Err(error) => { candidate.unavailable(file, error.code()); return Ok(()); }
        };
        let node = atlas.atlas().node_for_file(file).map_err(AtlasSessionError::from)?;
        let before_hits = candidate.hits.len();
        {
            let mut scan_options = StreamReadOptions::new(qgen);
            scan_options.max_matches = work.options.max_matches - before_hits;
            scan_options.max_bytes = capture.bytes().len() as u64;
            let mut scan = ReaderSearch::new(capture.bytes(), *capture.request(),
                ByteLength::new(capture.bytes().len() as u64), &work.pattern, scan_options,
                &self.budget, work.scan_id).map_err(AppError::from)?;
            while scan.state() == StreamReadState::Pending {
                scan.step(StreamReadStep::default(), qgen,
                    || canceled() || atlas.validate_active().is_err()).map_err(AppError::from)?;
            }
            let (_, report) = scan.finish().map_err(AppError::from)?;
            self.validate(atlas)?; check(canceled)?;
            if report.state() == StreamReadState::Canceled { return Err(AtlasSearchError::Canceled); }
            if let StreamReadState::Failed(error) = report.state() { return Err(AppError::from(error).into()); }
            candidate.scanned += 1;
            candidate.matches_seen += report.matches_seen();
            if report.state() == StreamReadState::Truncated { candidate.truncated = true; }
            else if !report.input_complete() { candidate.unavailable(file, report.state().code()); }
            for hit in report.hits() {
                candidate.hits.push(AtlasSearchHit { id: candidate.hits.len() as u64 + 1,
                    file, revision, node, original_range: hit.original_range(), source_slot: candidate.files.len() });
            }
        }
        let hits = candidate.hits.len() - before_hits;
        if hits != 0 {
            candidate.retained_bytes += capture.bytes().len();
            candidate.files.push(RetainedFile { capture, node, hits });
        }
        Ok(())
    }
}

fn update_coverage(atlas: &AtlasSession, work: &mut SearchWork) {
    let q = &mut work.snapshot;
    q.pending = atlas.atlas().catalog().entries().len() - q.examined;
    q.source_bytes_read = work.io.bytes;
    q.read_calls = work.io.calls;
    q.stop_reason = if q.pending == 0 {
        // A per-file lookahead can prove truncation even on the final file.
        Some(if q.truncated { AtlasSearchStop::MatchLimit } else { AtlasSearchStop::AllFilesExamined })
    } else if q.hits.len() == work.options.max_matches {
        q.truncated = true;
        Some(AtlasSearchStop::MatchLimit)
    } else if q.examined == work.options.max_files {
        Some(AtlasSearchStop::FileLimit)
    } else if work.io.bytes >= work.options.max_source_bytes as u64 {
        Some(AtlasSearchStop::SourceByteLimit)
    } else { None };
    q.complete = q.stop_reason.is_some() && atlas.atlas().discovery_complete()
        && q.pending == 0 && q.unavailable == 0 && !q.truncated;
}

#![forbid(unsafe_code)]

//! Resumable verification over the live-capture and retained-index pipelines.
//! Each call verifies at most 64 KiB from at most one source member. Capturing
//! a new member, hashing retained hits, and encoding excerpts retain their
//! separate bounds; this is not a fixed-duration UI or native latency claim.
//! A member's hits and counts publish together only after its scan finishes.

use super::*;
use crate::workspace;
use fcb::search::{CaptureQueryScan, CaptureRequest, ParsedQuery,
    QueryGeneration, QueryOptions, SearchCoverage, SearchDocument, SearchMode, UnicodeNormalization};
use super::indexed::IndexedWork;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasSearchStop { AllFilesExamined, FileLimit, SourceByteLimit, MatchLimit, VerificationByteLimit, CaptureScope }
impl AtlasSearchStop {
    pub const fn code(self) -> &'static str {
        match self {
            Self::AllFilesExamined => "all-files-examined",
            Self::FileLimit => "file-limit", Self::SourceByteLimit => "source-byte-limit",
            Self::MatchLimit => "match-limit", Self::VerificationByteLimit => "verification-byte-limit",
            Self::CaptureScope => "capture-scope",
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
    pub count_all_matches: bool,
    pub match_count_complete: bool,
    pub eligible_files: usize,
    pub excluded_files: usize,
    /// Frozen requested-region membership, independent of file-type filtering.
    /// None means this query did not request a visible-region manifest.
    pub scope_region_files: Option<usize>,
    pub scope_region_catalogued_files: Option<usize>,
    pub scope_region_missing_files: Option<usize>,
    pub examined_files: usize,
    pub scanned_files: usize,
    pub unavailable_files: usize,
    pub pending_files: usize,
    pub retained_hits: usize,
    /// Legacy queries include at most one unstored lookahead hit. Counted
    /// queries continue observing occurrences independently of retained rows.
    pub matches_seen: u64,
    /// Distinct files with observed primary occurrences, including omitted rows.
    pub matching_files: usize,
    pub retained_matching_files: usize,
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
    pub(super) execution: SearchExecution,
}
pub(super) enum SearchExecution {
    Live(LiveWork), Indexed(IndexedWork), Structure(super::structure::StructureWork),
    Expression(super::expression::ExpressionWork),
}
/// Raw source bytes offered to the shared verifier by one host step. File
/// capture and result presentation have independent, explicitly larger limits.
pub const MAX_ATLAS_VERIFICATION_STEP_BYTES: usize = 64 * 1024;

pub(super) struct LiveWork {
    options: AtlasSearchOptions,
    scan_id: ResourceAllocationId,
    io: workspace::IoCounts,
    current: Option<PendingCaptureScan>,
}

/// The scan and its exact source survive worker yields together. The cursor
/// shares the capture's byte allocation; its query/result scratch is admitted
/// separately and remains charged until final publication or cancellation.
pub(super) struct PendingCaptureScan {
    pub(super) capture: CompleteCapture,
    pub(super) node: AtlasNodeId,
    pub(super) scan: CaptureQueryScan,
    _lease: ResourceLease,
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
            count_all_matches: q.count_all_matches, match_count_complete: q.complete,
            eligible_files: q.eligible_files, excluded_files: atlas.atlas().file_count() - q.eligible_files,
            scope_region_files: q.scope.region_files(),
            scope_region_catalogued_files: q.scope_region.map(|region| region.catalogued),
            scope_region_missing_files: q.scope_region.map(|region| region.missing),
            scanned_files: q.scanned, unavailable_files: q.unavailable, pending_files: q.pending,
            retained_hits: q.hits.len(), matches_seen: q.matches_seen,
            matching_files: q.matching_files, retained_matching_files: q.files.len(),
            retained_source_bytes: q.retained_bytes,
            source_bytes_read: q.source_bytes_read, read_calls: q.read_calls,
            step_count: q.step_count, last_step_files: q.last_step_files,
            last_step_source_bytes: q.last_step_bytes, last_step_read_calls: q.last_step_calls })
    }
    /// Retire a running live OR indexed replacement on a worker. Accepted rows,
    /// the reusable index and independently opened reader captures survive.
    pub fn cancel_pending(&mut self) -> bool { self.pending.take().is_some() }

    pub fn begin(&mut self, atlas: &AtlasSession, generation: u64, needle: &str,
        options: AtlasSearchOptions, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.begin_scoped(atlas, generation, needle, AtlasSearchScope::all(), options, canceled)
    }

    /// Admit only selected catalog members, independently of display filtering.
    /// Explicit empty scope is valid and finishes on its first zero-I/O step.
    pub fn begin_scoped(&mut self, atlas: &AtlasSession, generation: u64, needle: &str,
        scope: AtlasSearchScope, options: AtlasSearchOptions, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let work = self.prepare_work(atlas, generation, needle, scope, options, &mut canceled)?;
        let response = self.work_response(atlas, &work, "begin", &mut canceled)?;
        self.pending = Some(work);
        Ok(response)
    }

    /// Admit decoded-text work whose count can continue beyond retained rows.
    pub fn begin_counted_scoped(&mut self, atlas: &AtlasSession, generation: u64, needle: &str,
        scope: AtlasSearchScope, options: AtlasSearchOptions, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let work = self.prepare_counted_work(atlas, generation, needle, scope, options, &mut canceled)?;
        let response = self.work_response(atlas, &work, "begin", &mut canceled)?;
        self.pending = Some(work);
        Ok(response)
    }

    /// Advance verification within at most ONE live or captured file. A large
    /// file can require several calls before its hits and counts are published.
    /// Stale steps never discard a newer request. Any failed/canceled step drops
    /// that request's provisional state, not the previously accepted query.
    /// Repeating a terminal step returns its page without repeating verification.
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
        let mut work = self.pending.take().ok_or(AtlasSearchError::MissingQuery)?;
        self.advance_work(atlas, &mut work, &mut canceled)?;
        let response = self.work_response(atlas, &work, "step", &mut canceled)?;
        if work.snapshot.stop_reason.is_some() { self.accepted = Some(work.snapshot); }
        else { self.pending = Some(work); }
        Ok(response)
    }

    pub(super) fn prepare_work(&mut self, atlas: &AtlasSession, generation: u64,
        needle: &str, scope: AtlasSearchScope, options: AtlasSearchOptions, canceled: &mut impl FnMut() -> bool)
        -> Result<SearchWork, AtlasSearchError> {
        self.prepare_work_inner(atlas, generation, needle, scope, options, false, canceled)
    }

    pub(super) fn prepare_counted_work(&mut self, atlas: &AtlasSession, generation: u64,
        needle: &str, scope: AtlasSearchScope, options: AtlasSearchOptions, canceled: &mut impl FnMut() -> bool)
        -> Result<SearchWork, AtlasSearchError> {
        self.prepare_work_inner(atlas, generation, needle, scope, options, true, canceled)
    }

    fn prepare_work_inner(&mut self, atlas: &AtlasSession, generation: u64,
        needle: &str, scope: AtlasSearchScope, options: AtlasSearchOptions, count_all_matches: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<SearchWork, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?;
        if count_all_matches { options.validate_counted()?; } else { options.validate()?; }
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES {
            return Err(AtlasSearchError::InvalidLimits);
        }
        check(canceled)?;
        let (eligible_files, scope_region) = scope.admission_counts(atlas, canceled)?;
        let scope_bytes = if scope.is_all() { 0 } else {
            eligible_files.checked_mul(size_of::<FileId>()).ok_or(AppError::Admission)?
        };
        let [state_id, scan_id] = [self.next_id()?, self.next_id()?];
        let capacity = options.max_matches.min(options.max_files);
        let text_capacity = matched_text_capacity(needle, &scope)?;
        let charge = 2 * options.max_source_bytes + capacity * (size_of::<RetainedFile>() + 64)
            + options.max_matches * size_of::<AtlasSearchHit>()
            + MAX_DIAGNOSTICS * size_of::<Unavailable>() + needle.len() + size_of::<SearchWork>();
        let charge = charge.checked_add(scope_bytes).ok_or(AppError::Admission)?;
        let charge = charge.checked_add(retained_text_bytes(options.max_matches, text_capacity)?)
            .ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), state_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let scope_files = scope.freeze(atlas, eligible_files, canceled)?;
        let case_sensitive = scope.case_sensitive();
        let snapshot = Snapshot { generation, needle: copy_text(needle)?, expression: None, structure: None,
            structure_rows: Vec::new(), files: reserve(capacity)?,
            scope, scope_files, scope_region, eligible_files,
            hits: reserve(options.max_matches)?, diagnostics: reserve(MAX_DIAGNOSTICS)?,
            matched_texts: reserve(if case_sensitive { 0 } else { options.max_matches })?,
            matched_text_capacity: text_capacity,
            examined: 0, scanned: 0, unavailable: 0, pending: eligible_files,
            matches_seen: 0, matching_files: 0, source_bytes_read: 0, read_calls: 0, retained_bytes: 0,
            count_all_matches, truncated: false, complete: false, stop_reason: None, step_count: 0,
            last_step_files: 0, last_step_bytes: 0, last_step_calls: 0, index_usage: None, _lease: lease };
        self.validate(atlas)?; check(canceled)?;
        Ok(SearchWork { snapshot, execution: SearchExecution::Live(LiveWork {
            options, scan_id, io: workspace::IoCounts::default(), current: None }) })
    }

    pub(super) fn work_response(&mut self, atlas: &AtlasSession, work: &SearchWork,
        command: &str, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        let mut out = self.output(atlas, command)?;
        let allocations = self.excerpt_ids(64)?;
        let mut excerpts = Excerpts::new(&self.budget, allocations);
        encode_page(&mut out, atlas, &work.snapshot, 0, 64, &mut excerpts, canceled)?;
        self.finish(atlas, out, !work.snapshot.complete || work.snapshot.truncated, canceled)
    }

    pub(super) fn advance_work(&self, atlas: &AtlasSession, work: &mut SearchWork,
        canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        self.validate(atlas)?; check(canceled)?;
        let snapshot = &mut work.snapshot;
        let before_files = snapshot.examined;
        let before_bytes = snapshot.source_bytes_read;
        let before_calls = snapshot.read_calls;
        let before_matches = snapshot.matches_seen;
        match &mut work.execution {
            SearchExecution::Live(live) => {
                update_coverage(atlas, snapshot, live);
                if snapshot.stop_reason.is_none() { self.scan_one_file(atlas, snapshot, live, canceled)?; }
                update_coverage(atlas, snapshot, live);
            }
            SearchExecution::Indexed(indexed) => self.advance_indexed_work(atlas, snapshot, indexed, canceled)?,
            SearchExecution::Structure(structure) => self.advance_structure_work(atlas, snapshot, structure, canceled)?,
            SearchExecution::Expression(expression) => self.advance_expression_work(atlas, snapshot, expression, canceled)?,
        }
        self.validate(atlas)?; check(canceled)?;
        // Every execution variant publishes at most one finalized member.
        // In-progress slices expose no tentative counts, so a matching file is
        // counted once even when verification spans many worker calls.
        if snapshot.matches_seen > before_matches { snapshot.matching_files += 1; }
        snapshot.step_count = snapshot.step_count.checked_add(1).ok_or(AtlasSearchError::IdentityExhausted)?;
        snapshot.last_step_files = snapshot.examined - before_files;
        snapshot.last_step_bytes = snapshot.source_bytes_read - before_bytes;
        snapshot.last_step_calls = snapshot.read_calls - before_calls;
        Ok(())
    }

    fn scan_one_file(&self, atlas: &AtlasSession, candidate: &mut Snapshot, work: &mut LiveWork,
        canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        if work.current.is_none() {
            let catalog = atlas.atlas().catalog();
            let file = match &candidate.scope_files {
                Some(files) => files.get(candidate.examined).copied(),
                None => catalog.file_id(candidate.examined),
            }.ok_or(AtlasSearchError::WrongAtlas)?;
            let entry = catalog.entry(file).ok_or(AtlasSearchError::WrongAtlas)?;
            if entry.observed_bytes() > work.options.max_file_bytes as u64 {
                candidate.examined += 1;
                candidate.unavailable(file, "FILE_BYTE_LIMIT"); return Ok(());
            }
            let generation = QueryGeneration::new(self.manifest.owner(), candidate.generation)
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
                Err(error) => {
                    candidate.examined += 1;
                    candidate.unavailable(file, error.code()); return Ok(());
                }
            };
            let node = atlas.atlas().node_for_file(file).map_err(AtlasSessionError::from)?;
            let remaining = work.options.max_matches - candidate.hits.len();
            let mut options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
                case_sensitive: candidate.scope.case_sensitive(), normalization: UnicodeNormalization::Exact,
            });
            // Non-counting probes retain one temporary row when global storage
            // is full; only the first unstored occurrence affects the result.
            options.max_matches = if candidate.count_all_matches { remaining } else { remaining.max(1) };
            options.max_bytes_scanned = Some(capture.bytes().len() as u64);
            // Literal entry points never parse spaces, signs or field syntax.
            let query = ParsedQuery { primary_needle: copy_text(&candidate.needle)?,
                is_phrase: true, conjunction_terms: Vec::new(), exclusion_terms: Vec::new(),
                path_filters: Vec::new(), lang_filters: Vec::new(), raw_query: copy_text(&candidate.needle)? };
            let text_capacity = candidate.matched_text_capacity.max(candidate.needle.len());
            // No path predicates occur in this literal AST; membership was
            // already frozen by the atlas scope at query admission.
            work.current = Some(self.prepare_capture_scan(capture, node, "", &query, &options,
                candidate.count_all_matches, text_capacity, work.scan_id)?);
        }
        let current = work.current.as_mut().ok_or(AtlasSearchError::WrongAtlas)?;
        current.scan.step(MAX_ATLAS_VERIFICATION_STEP_BYTES,
            || canceled() || atlas.validate_active().is_err()).map_err(IndexError::from)?;
        self.validate(atlas)?; check(canceled)?;
        if current.scan.is_terminal() {
            let current = work.current.take().ok_or(AtlasSearchError::WrongAtlas)?;
            self.publish_capture_scan(atlas, candidate, current, work.options.max_matches, canceled)?;
        }
        Ok(())
    }

    pub(super) fn prepare_capture_scan(&self, capture: CompleteCapture, node: AtlasNodeId,
        path: &str, query: &ParsedQuery, options: &QueryOptions, count_all: bool,
        text_capacity: usize, allocation: ResourceAllocationId) -> Result<PendingCaptureScan, AtlasSearchError> {
        let result_scratch = text_matching::scan_scratch_bytes(options.max_matches, text_capacity)?
            .checked_sub(text_matching::LOWERCASE_SCAN_SCRATCH_BYTES).ok_or(AppError::Admission)?;
        let scratch = CaptureQueryScan::scratch_bytes(query, options).map_err(IndexError::from)?
            .checked_add(result_scratch).and_then(|n| n.checked_add(size_of::<PendingCaptureScan>()))
            .ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), allocation,
            ByteLength::new(scratch as u64)).map_err(|_| AppError::Admission)?;
        let document = SearchDocument::new(capture.request().file(), path, &capture);
        let scan = CaptureQueryScan::new(&document, query, options, 0, count_all).map_err(IndexError::from)?;
        Ok(PendingCaptureScan { capture, node, scan, _lease: lease })
    }

    /// Transfer a finalized member once. Returning byte/coverage information
    /// lets expression queries apply their separate cumulative work allowance.
    pub(super) fn publish_capture_scan(&self, atlas: &AtlasSession, candidate: &mut Snapshot,
        current: PendingCaptureScan, max_matches: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<(u64, bool), AtlasSearchError> {
        let PendingCaptureScan { capture, node, scan, _lease } = current;
        let report = scan.into_result().map_err(IndexError::from)?;
        self.validate(atlas)?; check(canceled)?;
        if report.coverage == SearchCoverage::CanceledEarly { return Err(AtlasSearchError::Canceled); }
        let file = capture.request().file();
        let revision = capture.request().revision();
        let before_hits = candidate.hits.len();
        let remaining = max_matches.checked_sub(before_hits).ok_or(AtlasSearchError::WrongAtlas)?;
        let work_limited = matches!(report.coverage, SearchCoverage::BudgetExhausted { .. });
        if candidate.count_all_matches && matches!(report.coverage, SearchCoverage::TruncatedAtLimit { .. })
            || report.unsupported_files.iter().any(|&unsupported| unsupported != file) {
            return Err(AtlasSearchError::WrongAtlas);
        }
        candidate.examined += 1;
        candidate.scanned += 1;
        let counted = if candidate.count_all_matches { report.total_matches_counted }
            else { report.total_matches_counted.min(remaining + 1) };
        candidate.matches_seen = candidate.matches_seen.checked_add(counted as u64)
            .ok_or(AtlasSearchError::IdentityExhausted)?;
        candidate.truncated |= counted > remaining
            || matches!(report.coverage, SearchCoverage::TruncatedAtLimit { .. });
        if !report.unsupported_files.is_empty() { candidate.unavailable(file, "unsupported-text"); }
        else if work_limited { candidate.unavailable(file, "verification-byte-limit"); }
        for hit in report.matches.into_iter().take(remaining) {
            check(canceled)?;
            if hit.file_id != file || hit.revision != revision || hit.original_byte_range.is_empty()
                || hit.original_byte_range.end().get() > capture.bytes().len() as u64
                || hit.matched_text.is_empty() { return Err(AtlasSearchError::WrongAtlas); }
            if candidate.scope.case_sensitive() {
                if hit.matched_text.as_bytes() != candidate.needle.as_bytes() { return Err(AtlasSearchError::WrongAtlas); }
            } else {
                if hit.matched_text.capacity() > candidate.matched_text_capacity { return Err(AtlasSearchError::WrongAtlas); }
                candidate.matched_texts.push(hit.matched_text);
            }
            candidate.hits.push(AtlasSearchHit { id: candidate.hits.len() as u64 + 1,
                file, revision, node, original_range: hit.original_byte_range, source_slot: candidate.files.len() });
        }
        let hits = candidate.hits.len() - before_hits;
        if hits != 0 {
            candidate.retained_bytes += capture.bytes().len();
            let witness = CaptureWitness::new(capture.bytes(),
                &mut || canceled() || atlas.validate_active().is_err())?;
            candidate.files.push(RetainedFile { capture, node, hits, witness });
        }
        Ok((report.scanned_bytes, work_limited))
    }
}
fn update_coverage(atlas: &AtlasSession, q: &mut Snapshot, work: &LiveWork) {
    q.pending = q.eligible_files - q.examined;
    q.source_bytes_read = work.io.bytes;
    q.read_calls = work.io.calls;
    q.stop_reason = if q.pending == 0 {
        Some(if q.truncated && !q.count_all_matches { AtlasSearchStop::MatchLimit }
            else { AtlasSearchStop::AllFilesExamined })
    } else if q.truncated && !q.count_all_matches {
        // Legacy mode stops on omitted occurrences. Counted mode continues.
        Some(AtlasSearchStop::MatchLimit)
    } else if q.examined == work.options.max_files {
        Some(AtlasSearchStop::FileLimit)
    } else if work.current.is_none() && work.io.bytes >= work.options.max_source_bytes as u64 {
        // Already captured bytes remain searchable after I/O admission closes.
        Some(AtlasSearchStop::SourceByteLimit)
    } else { None };
    q.complete = q.stop_reason.is_some() && atlas.atlas().discovery_complete()
        && q.scope_region.is_none_or(|region| region.missing == 0) && q.pending == 0 && q.unavailable == 0
        && (!q.truncated || q.count_all_matches);
}

#![forbid(unsafe_code)]

//! Retained document-wide queries over an explicitly opened FCBS repository.
//! Uses the existing expression parser, predicate state machine and exact member
//! verifier. No source payload survives query completion. This synchronous host
//! adapter runs on a worker; it creates no runtime, process or live-root grant.

use std::mem::size_of;
use fcb_core::ResourceLease;
use fcb::{ArenaOwnerId, FileId, QueryGeneration, SourceRevision};
use fcb::search::{MAX_QUERY_LEN, MAX_QUERY_TOKENS, StreamReadStep};
use fcb::search::expression::{ExpressionPlan, ExpressionQuery, ExpressionOptions,
    ExpressionReport, ExpressionState, ExpressionStats, ExpressionHit};
pub use fcb::search::expression::ExpressionError;
use super::{SavedRepositorySession, SavedRepositoryError, SavedDeskError, SavedDeskOpen,
    DeskSession, HostResponse, ByteLength, RawPath, AppError, check};
use super::super::{ReaderSession, PagedCapture, PagedSearchError, Output};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SavedExpressionOptions {
    /// A zero cap probes existence without retaining a result row.
    pub max_hits: usize,
    /// Aggregate original bytes consumed by ALL predicate and primary scans,
    /// including scans over already loaded bytes. This is not an I/O budget.
    pub max_scan_bytes: u64,
}
impl Default for SavedExpressionOptions {
    fn default() -> Self { Self { max_hits: super::super::MAX_SAVED_HITS, max_scan_bytes: 64 * 1024 * 1024 } }
}

/// Owns its syntax/patterns and bounded witness metadata, not an archive handle
/// or matching source collection. Hosts supply fresh generations and publish a
/// candidate only after its first response succeeds. Literal queries and FCBD
/// indexes remain independent; this route does not use an incompatible index.
pub struct SavedExpression {
    owner: ArenaOwnerId,
    generation: u64,
    plan: ExpressionPlan,
    report: ExpressionReport,
    options: SavedExpressionOptions,
    member_bytes_read: u64,
    member_read_calls: u64,
    _lease: ResourceLease,
}
impl SavedExpression {
    pub fn prepare(saved: &mut SavedRepositorySession, generation: u64, query: &str,
        options: SavedExpressionOptions, mut canceled: impl FnMut() -> bool)
        -> Result<Self, SavedDeskError> {
        check(&mut canceled)?;
        if options.max_hits > super::super::MAX_SAVED_HITS || query.len() > MAX_QUERY_LEN {
            return Err(SavedRepositoryError::InvalidLimits.into());
        }
        let owner = saved.owner();
        let qgen = QueryGeneration::new(owner, generation).map_err(|_| ExpressionError::StaleQuery)?;
        let [object_id, plan_id] = saved.allocations()?;
        // Parser tokens bound matcher count. Reserve a disjoint ID interval,
        // rather than guessing how many IDs the parsed expression will consume.
        let pattern_ids = saved.allocations::<MAX_QUERY_TOKENS>()?;
        let allocations = saved.allocations()?;
        let lease = saved.budget.try_reserve_managed(owner, object_id, ByteLength::new(size_of::<Self>() as u64))
            .map_err(|_| AppError::Admission)?;
        let plan = ExpressionPlan::parse(owner, query, &saved.budget, plan_id, pattern_ids[0])?;
        let first_file = FileId::new(owner, 1).map_err(|_| ExpressionError::IdentityExhausted)?;
        let first_revision = SourceRevision::new(owner, 1).map_err(|_| ExpressionError::IdentityExhausted)?;
        let before = saved.archive.load_stats();
        let mut work = ExpressionQuery::new(&mut saved.archive, &plan, ExpressionOptions {
            generation: qgen, first_file, first_revision, max_matches: options.max_hits,
            max_scan_bytes: options.max_scan_bytes,
        }, &saved.budget, allocations)?;
        while work.state() == ExpressionState::Pending {
            work.step(StreamReadStep::default(), qgen, &saved.budget, &mut canceled)?;
        }
        let report = work.finish()?;
        let after = saved.archive.load_stats();
        check(&mut canceled)?;
        Ok(Self { owner, generation, plan, report, options,
            member_bytes_read: after.bytes_read - before.bytes_read,
            member_read_calls: after.read_calls - before.read_calls, _lease: lease })
    }
    pub const fn generation(&self) -> u64 { self.generation }
    pub fn is_complete(&self) -> bool { self.report.is_complete() }
    pub fn stats(&self) -> ExpressionStats { self.report.stats() }
    pub fn retained_hits(&self) -> usize { self.report.hits().len() }
    pub fn validate(&self, saved: &SavedRepositorySession, generation: u64) -> Result<(), SavedDeskError> {
        if saved.owner() != self.owner || generation != self.generation { return Err(ExpressionError::StaleQuery.into()); }
        let qgen = QueryGeneration::new(self.owner, generation).map_err(|_| ExpressionError::StaleQuery)?;
        self.report.validate_delivery(saved.archive_digest(), qgen)?;
        Ok(())
    }

    /// Pages do not rescan predicates, read member payloads, or renumber hits.
    /// Missing/unsupported members and unknown predicate outcomes stay partial.
    pub fn page(&self, saved: &mut SavedRepositorySession, generation: u64,
        start: usize, limit: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedDeskError> {
        self.validate(saved, generation)?; check(&mut canceled)?;
        super::super::page_limits(start, limit, self.report.hits().len())?;
        let mut out = saved.output("expression")?;
        self.encode_page(saved, start, limit, &mut out, &mut canceled)?;
        Ok(saved.finish(out, !self.is_complete(), &mut canceled)?)
    }

    fn encode_page(&self, saved: &SavedRepositorySession, start: usize, limit: usize,
        out: &mut Output, canceled: &mut impl FnMut() -> bool) -> Result<(), SavedDeskError> {
        out.literal(",\"selection_namespace\":\"saved-expression\",\"expression_generation\":")?;
        out.integer(self.generation)?;
        out.literal(",\"expression\":")?; out.quoted(&self.plan.syntax().raw_query)?;
        out.literal(",\"primary_needle\":")?; out.quoted(&self.plan.syntax().primary_needle)?;
        out.literal(",\"predicate_scope\":\"whole-captured-member\",\"case_sensitive\":true,\"index_used\":false,\"index_policy\":\"direct-verified-member-scan\",\"state\":")?;
        out.quoted(match self.report.state() {
            ExpressionState::Complete => "complete", ExpressionState::Truncated => "truncated",
            ExpressionState::WorkLimit => "work-limit", _ => return Err(ExpressionError::Pending.into()),
        })?;
        out.literal(",\"complete\":")?; out.boolean(self.is_complete())?;
        out.literal(",\"matches_seen_exact\":")?; out.boolean(self.is_complete())?;
        out.literal(",\"truncated\":")?; out.boolean(self.report.state() == ExpressionState::Truncated)?;
        let s = self.stats();
        for (key, value) in [
            ("required_terms", self.plan.syntax().conjunction_terms.len() as u64),
            ("excluded_terms", self.plan.syntax().exclusion_terms.len() as u64),
            ("max_hits", self.options.max_hits as u64), ("max_scan_bytes", self.options.max_scan_bytes),
            ("retained_hits", self.retained_hits() as u64), ("matches_seen", self.report.matches_seen()),
            ("metadata_examined", s.metadata_examined as u64), ("scope_files", s.scope_files as u64),
            ("metadata_excluded", s.metadata_excluded as u64), ("unavailable_files", s.unavailable_files as u64),
            ("unsupported_files", s.unsupported_files as u64), ("members_loaded", s.members_loaded as u64),
            ("member_bytes_read", self.member_bytes_read), ("member_read_calls", self.member_read_calls),
            ("loaded_bytes", s.loaded_bytes), ("peak_source_bytes", s.peak_member_bytes as u64),
            ("predicate_scans", s.predicate_scans as u64), ("predicate_rejections", s.predicate_rejections as u64),
            ("primary_scans", s.primary_scans as u64), ("scanned_bytes", s.scanned_bytes),
        ] { out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?; }
        out.literal(",\"retained_source_payload_bytes\":\"0\",\"hits\":[")?;
        let end = start.saturating_add(limit).min(self.retained_hits());
        for (i, hit) in self.report.hits()[start..end].iter().enumerate() {
            check(canceled)?;
            if i > 0 { out.literal(",")?; }
            let member = saved.archive.directory().member(hit.ordinal()).ok_or(SavedRepositoryError::MissingHit)?;
            out.literal("{\"hit_id\":")?; out.integer((start + i + 1) as u64)?;
            out.literal(",\"member\":")?; out.integer(hit.ordinal() as u64)?;
            out.literal(",\"file_id\":")?; out.integer(hit.file().get())?;
            out.literal(",\"source_revision\":")?; out.integer(hit.revision().get())?;
            out.literal(",\"source_digest\":")?; out.quoted(&hit.source_digest().to_hex())?;
            out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(member.path).to_path_buf())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?; out.literal("}")?;
        }
        out.literal("],\"next_offset\":")?;
        super::super::optional(out, (end < self.retained_hits()).then_some(end as u64))?;
        out.literal("}\n")?;
        Ok(())
    }

    /// Activate one retained expression witness as an independently owned
    /// reader. The complete archived member is verified again before any
    /// reader or response is returned; archived paths remain labels only.
    pub fn open_hit_reader(&self, saved: &mut SavedRepositorySession, reader_owner: ArenaOwnerId,
        generation: u64, hit_id: u64, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), SavedDeskError> {
        self.validate(saved, generation)?; check(&mut canceled)?;
        let hit = self.hit(hit_id)?;
        let prepared = saved.prepare_expression_reader(reader_owner, generation, hit, &mut canceled)?;
        let mut out = saved.output("expression-open-reader")?;
        self.encode_reader(&prepared, hit_id, &mut out, &mut canceled)?;
        let response = saved.finish(out, false, &mut canceled)?;
        Ok((prepared.reader, response))
    }

    fn hit(&self, hit_id: u64) -> Result<ExpressionHit, SavedDeskError> {
        let position = hit_id.checked_sub(1).and_then(|n| usize::try_from(n).ok())
            .ok_or(SavedRepositoryError::MissingHit)?;
        self.report.hits().get(position).copied().ok_or(SavedRepositoryError::MissingHit.into())
    }

    fn encode_reader(&self, prepared: &ExpressionReader, hit_id: u64, out: &mut Output,
        canceled: &mut impl FnMut() -> bool) -> Result<(), SavedDeskError> {
        check(canceled)?;
        let capture = &prepared.capture;
        let hit = prepared.hit;
        let (start, end) = hit.original_range().as_usize_bounds().map_err(|_| PagedSearchError::InvalidHit)?;
        let selected = capture.bytes().get(start..end).ok_or(PagedSearchError::InvalidHit)?;
        out.literal(",\"selection_namespace\":\"saved-expression\",\"expression_generation\":")?;
        out.integer(self.generation)?;
        out.literal(",\"primary_needle\":")?; out.quoted(&self.plan.syntax().primary_needle)?;
        out.literal(",\"member\":")?; out.integer(hit.ordinal() as u64)?;
        out.literal(",\"file_id\":")?; out.integer(capture.file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(capture.revision().get())?;
        out.literal(",\"source_digest\":")?; out.quoted(&capture.source_digest().to_hex())?;
        out.literal(",\"source_observation\":\"verified-saved-member\",\"live_source_reopened\":false,\"reader_owner\":")?;
        out.integer(prepared.reader_owner.get())?;
        out.literal(",\"selection\":{\"expression_generation\":")?; out.integer(self.generation)?;
        out.literal(",\"hit_id\":")?; out.integer(hit_id)?;
        out.literal(",\"domain\":\"original-source-bytes\",\"original_range\":")?; out.range(hit.original_range())?;
        out.literal(",\"original_hex\":")?; out.hex(selected)?; out.literal("}")?;
        out.literal(",\"reader\":")?; out.literal(prepared.info.as_str())?;
        out.literal(",\"window\":")?; out.literal(prepared.window.as_str())?; out.literal("}\n")?;
        check(canceled)?;
        Ok(())
    }

    /// The immutable hit certifies the primary occurrence AND whole-member
    /// predicates at search time. Reverify the entire member digest on import;
    /// a changed archive cannot keep an old predicate proof with new bytes.
    /// There is one desk transaction, not open-then-select partial publication.
    pub fn open_hit_desk(&self, saved: &mut SavedRepositorySession, desk: &mut DeskSession,
        expected: u64, attempt: u64, generation: u64, hit_id: u64,
        mut canceled: impl FnMut() -> bool) -> Result<SavedDeskOpen, SavedDeskError> {
        self.validate(saved, generation)?;
        let position = hit_id.checked_sub(1).and_then(|n| usize::try_from(n).ok())
            .ok_or(SavedRepositoryError::MissingHit)?;
        let hit = *self.report.hits().get(position).ok_or(SavedRepositoryError::MissingHit)?;
        saved.validate_desk_member(desk, expected, attempt, hit.ordinal(), &mut canceled)?;
        let allocations = saved.allocations()?;
        let qgen = QueryGeneration::new(self.owner, generation).map_err(|_| ExpressionError::StaleQuery)?;
        let before = saved.archive.load_stats();
        let capture = hit.open(&mut saved.archive, qgen, &saved.budget, allocations, &mut canceled)?;
        let after = saved.archive.load_stats();
        let mut opened = saved.import_desk_capture(desk, expected, attempt, hit.ordinal(), &capture,
            Some(hit.original_range()), None, (after.bytes_read - before.bytes_read, after.read_calls - before.read_calls),
            &mut canceled)?;
        // This is infallible receipt metadata, not a second navigation action.
        opened.expression = Some((generation, hit_id));
        Ok(opened)
    }
}

/// Preparation owns every fallible reader handoff component until the complete
/// response succeeds. No archive/session slot is temporarily removed to build
/// it, so cancellation and response failure leave the accepted query intact.
struct ExpressionReader {
    reader_owner: ArenaOwnerId,
    hit: ExpressionHit,
    capture: PagedCapture,
    reader: ReaderSession,
    info: HostResponse,
    window: HostResponse,
}

impl SavedRepositorySession {
    pub fn accepted_expression_generation(&self) -> Option<u64> {
        self.expression.as_ref().map(SavedExpression::generation)
    }
    pub const fn last_expression_attempt(&self) -> u64 { self.last_expression_attempt }

    /// Expression attempts have an independent monotonic namespace. A failed
    /// or canceled replacement retires its attempt while retaining the prior
    /// accepted report. The candidate's first complete response is prepared
    /// before replacing that report, including work-limit/partial responses.
    pub fn expression_search(&mut self, generation: u64, query: &str, max_hits: usize,
        max_scan_bytes: u64, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedDeskError> {
        super::super::query_attempt(&mut self.last_expression_attempt, generation)?;
        check(&mut canceled)?;
        let candidate = SavedExpression::prepare(self, generation, query,
            SavedExpressionOptions { max_hits, max_scan_bytes }, &mut canceled)?;
        let response = candidate.page(self, generation, 0, 64, &mut canceled)?;
        self.expression = Some(candidate);
        Ok(response)
    }

    /// Reusing retained metadata performs no predicate scans or member reads.
    pub fn expression_results(&mut self, generation: u64, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedDeskError> {
        check(&mut canceled)?;
        let accepted = self.accepted_expression(generation)?;
        super::super::page_limits(start, limit, accepted.retained_hits())?;
        let partial = !accepted.is_complete();
        let mut out = self.output("expression")?;
        self.accepted_expression(generation)?.encode_page(self, start, limit, &mut out, &mut canceled)?;
        Ok(self.finish(out, partial, &mut canceled)?)
    }

    /// Clear only expression results; literal queries, indexes, relationships
    /// and already delivered readers retain their own identities and lifetime.
    pub fn clear_expression(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedDeskError> {
        super::super::query_attempt(&mut self.last_expression_attempt, generation)?;
        check(&mut canceled)?;
        let mut out = self.output("expression-clear")?;
        out.literal(",\"selection_namespace\":\"saved-expression\",\"expression_generation\":")?;
        out.integer(generation)?;
        out.literal(",\"accepted_expression_generation\":null,\"retained_hits\":\"0\"}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        self.expression = None;
        Ok(response)
    }

    pub fn open_expression_hit_reader(&mut self, reader_owner: ArenaOwnerId, generation: u64,
        hit_id: u64, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), SavedDeskError> {
        check(&mut canceled)?;
        let hit = self.accepted_expression(generation)?.hit(hit_id)?;
        let prepared = self.prepare_expression_reader(reader_owner, generation, hit, &mut canceled)?;
        let mut out = self.output("expression-open-reader")?;
        self.accepted_expression(generation)?.encode_reader(&prepared, hit_id, &mut out, &mut canceled)?;
        let response = self.finish(out, false, &mut canceled)?;
        Ok((prepared.reader, response))
    }

    fn accepted_expression(&self, generation: u64) -> Result<&SavedExpression, SavedDeskError> {
        let query = self.expression.as_ref().ok_or(SavedRepositoryError::MissingQuery)?;
        query.validate(self, generation)?;
        Ok(query)
    }

    fn prepare_expression_reader(&mut self, reader_owner: ArenaOwnerId, generation: u64,
        hit: ExpressionHit, canceled: &mut impl FnMut() -> bool) -> Result<ExpressionReader, SavedDeskError> {
        check(canceled)?;
        if reader_owner == self.owner() { return Err(PagedSearchError::OwnerMismatch.into()); }
        let qgen = QueryGeneration::new(self.owner(), generation).map_err(|_| ExpressionError::StaleQuery)?;
        let allocations = self.allocations()?;
        // Digest verification covers every predicate's captured source, not
        // just the visible primary occurrence or a newly reopened live path.
        let capture = hit.open(&mut self.archive, qgen, &self.budget, allocations, &mut *canceled)?;
        let member = self.archive.directory().member(hit.ordinal()).ok_or(SavedRepositoryError::MissingHit)?;
        let label = RawPath::from_bytes(member.path).to_path_buf();
        let mut reader = ReaderSession::from_bytes(reader_owner, &label, capture.bytes(), &mut *canceled)
            .map_err(SavedRepositoryError::from)?;
        let info = reader.info(&mut *canceled).map_err(SavedRepositoryError::from)?;
        let range = hit.original_range();
        let start = range.start().get().saturating_sub(132);
        let end = range.end().get().saturating_add(132).min(capture.bytes().len() as u64);
        let window = reader.read_window(start, ((end - start) as usize).max(4), &mut *canceled)
            .map_err(SavedRepositoryError::from)?;
        check(canceled)?;
        Ok(ExpressionReader { reader_owner, hit, capture, reader, info, window })
    }
}

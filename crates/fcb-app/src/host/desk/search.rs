#![forbid(unsafe_code)]

//! Bounded pane Find over one retained immutable capture. Shared text engines
//! supply decoding, matching, source ranges and complete-expression predicates.
//! Every page replays retained bytes with a new query generation. No source path
//! is reopened; a pane's display label cannot authorize path/language filters.

use std::mem::size_of;
use fcb::{ArenaOwnerId, ByteLength, QueryGeneration};
use fcb::search::{CaptureRequest, ParsedQuery, QueryError, ReaderSearch,
    ResourceAllocationId, ResourceBudget, StreamReadOptions, StreamReadState,
    StreamReadStep, StreamingHit, StreamingNeedle, MAX_QUERY_LEN, MAX_QUERY_TOKENS};
use fcb_core::ResourceLease;
use super::{AcceptedQuery, AppError, DeskError, DeskPaneId, DeskSession,
    DeskSessionError, HostResponse, MAX_DESK_MATCHES, MAX_DESK_NEEDLE_BYTES, check};
use crate::{EXIT_OK, EXIT_PARTIAL};

/// Text semantics for one pane's immutable capture.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeskFindMode {
    /// Decoded UTF-8/BOM UTF-16 literal text, including overlapping occurrences.
    #[default]
    Literal,
    /// Bounded phrases, required terms and exclusions within the same capture.
    /// Path/language filters require repository metadata and are refused here.
    Expression,
    /// Exact whole-token textual candidates, including comments and strings.
    /// This is not compiler name resolution; case must remain sensitive.
    Identifier,
}

/// Additive options; the defaults preserve the established literal Find API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeskFindOptions {
    pub mode: DeskFindMode,
    /// False uses Unicode lowercase, not full case folding or normalization.
    pub case_sensitive: bool,
    /// Finish the scan after filling the retained page to count every match.
    /// Expressions always scan the full capture to establish their predicates.
    pub count_all_matches: bool,
    /// Skip this many matching occurrences, not source bytes or result indices.
    /// The skipped prefix still consumes the caller's scan-byte allowance.
    pub start_after: u64,
}
impl Default for DeskFindOptions {
    fn default() -> Self {
        Self { mode: DeskFindMode::Literal, case_sensitive: true,
            count_all_matches: false, start_after: 0 }
    }
}

struct AdmittedExpression {
    parsed: ParsedQuery,
    _lease: ResourceLease,
}
impl AdmittedExpression {
    fn new(raw: &str, owner: ArenaOwnerId, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<Self, DeskSessionError> {
        // Reserve parser/token growth before parsing. This bound depends only
        // on the admitted query, never on the source or number of occurrences.
        let charge = size_of::<Self>() + MAX_QUERY_LEN * 32
            + MAX_QUERY_TOKENS * 8 * size_of::<String>();
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let parsed = ParsedQuery::parse(raw)?;
        if !parsed.path_filters.is_empty() || !parsed.lang_filters.is_empty() {
            return Err(DeskSessionError::ExpressionScopeUnsupported);
        }
        Ok(Self { parsed, _lease: lease })
    }
}

impl DeskSession {
    /// Search a bounded occurrence window on a pane's retained capture.
    ///
    /// Generations are strictly increasing across all pane Find modes. Old
    /// rows remain activatable when the operation returns an error or is
    /// canceled. An accepted response replaces those rows, including a partial
    /// or unsupported-text response with no hits. Acceptance does not change
    /// the desk revision, reading history, notes or selection.
    /// Activate its page-local index with activate_hit and its query generation.
    ///
    /// Complete scanning, complete counts and retained rows are separate facts.
    /// A count-only request uses max_matches = 0 and count_all_matches = true.
    /// Every page starts at byte zero, so max_scan_bytes includes skipped work.
    pub fn search_with_options(&mut self, expected: u64, pane: DeskPaneId,
        generation: u64, needle: &str, max_matches: usize, max_scan_bytes: u64,
        options: DeskFindOptions, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskSessionError> {
        self.desk.location(pane, expected)?;
        if generation == 0 || generation <= self.last_query_attempt {
            return Err(DeskSessionError::StaleQuery);
        }
        if needle.is_empty() || needle.len() > MAX_DESK_NEEDLE_BYTES
            || max_matches > MAX_DESK_MATCHES
            || (options.mode == DeskFindMode::Identifier && !options.case_sensitive) {
            return Err(DeskError::InvalidLimits.into());
        }
        self.last_query_attempt = generation;
        check(&mut canceled)?;
        let [view_id, pattern_id, worker_id, result_id, expression_id] = self.ids()?;
        let view = self.desk.view(pane, expected, view_id)?;
        let source = view.source();
        let expression = if options.mode == DeskFindMode::Expression {
            Some(AdmittedExpression::new(needle, self.desk.owner(), &self.budget, expression_id)?)
        } else { None };
        let pattern = match options.mode {
            DeskFindMode::Literal => StreamingNeedle::text_matching(self.desk.owner(),
                needle, options.case_sensitive, &self.budget, pattern_id)?,
            DeskFindMode::Expression => StreamingNeedle::expression(self.desk.owner(),
                &expression.as_ref().ok_or(QueryError::SyntaxError)?.parsed,
                options.case_sensitive, &self.budget, pattern_id)?,
            DeskFindMode::Identifier => StreamingNeedle::identifier(self.desk.owner(),
                needle, &self.budget, pattern_id)?,
        };
        // The old accepted page and this private replacement can overlap.
        // Admit replacement storage before doing any scan or discarding rows.
        let charge = max_matches.checked_mul(size_of::<StreamingHit>())
            .and_then(|n| n.checked_add(size_of::<AcceptedQuery>() + 128))
            .ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.desk.owner(), result_id,
            ByteLength::new(charge as u64)).map_err(|_| AppError::Admission)?;
        let mut hits = Vec::new();
        hits.try_reserve_exact(max_matches).map_err(|_| AppError::Admission)?;
        if hits.capacity() > max_matches { return Err(AppError::Admission.into()); }

        let request = CaptureRequest::new(source.file(), source.revision())
            .map_err(|_| DeskError::OwnerMismatch)?;
        let query_generation = QueryGeneration::new(self.desk.owner(), generation)
            .map_err(|_| DeskError::IdentityExhausted)?;
        let mut scan_options = StreamReadOptions::new(query_generation);
        scan_options.max_matches = max_matches;
        scan_options.max_bytes = max_scan_bytes;
        let mut scan = ReaderSearch::new_page(source.bytes(), request,
            ByteLength::new(source.bytes().len() as u64), &pattern, scan_options,
            options.start_after, options.count_all_matches, &self.budget, worker_id)?;
        while scan.state() == StreamReadState::Pending {
            scan.step(StreamReadStep::default(), query_generation, &mut canceled)?;
        }
        let (_, report) = scan.finish()?;
        check(&mut canceled)?;
        if report.state() == StreamReadState::Canceled { return Err(DeskError::Canceled.into()); }
        if let StreamReadState::Failed(error) = report.state() { return Err(error.into()); }
        if report.hits().len() > max_matches { return Err(AppError::Admission.into()); }
        hits.extend_from_slice(report.hits());

        let mut out = self.output("find")?;
        out.literal(",\"pane\":")?; out.integer(pane.get())?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"file_id\":")?; out.integer(source.file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(source.revision().get())?;
        out.literal(",\"mode\":")?; out.quoted(match options.mode {
            DeskFindMode::Literal if options.case_sensitive => "exact-decoded-literal",
            DeskFindMode::Literal => "unicode-lowercase-literal",
            DeskFindMode::Expression => "decoded-text-expression",
            DeskFindMode::Identifier => "whole-token-text-candidate",
        })?;
        out.literal(",\"needle\":")?; out.quoted(needle)?;
        out.literal(",\"case_sensitive\":")?; out.boolean(options.case_sensitive)?;
        out.literal(",\"count_all_matches\":")?; out.boolean(options.count_all_matches)?;
        out.literal(",\"retained_limit\":")?; out.integer(max_matches as u64)?;
        out.literal(",\"max_scan_bytes\":")?; out.integer(max_scan_bytes)?;
        out.literal(",\"page_start_after\":")?; out.integer(report.page_start_after())?;
        out.literal(",\"skipped_matches\":")?; out.integer(report.skipped_matches())?;
        out.literal(",\"has_more_matches\":")?; out.boolean(report.has_more_matches())?;
        out.literal(",\"state\":")?; out.quoted(report.state().code())?;
        out.literal(",\"search_complete\":")?; out.boolean(report.input_complete())?;
        out.literal(",\"match_count_complete\":")?; out.boolean(report.input_complete())?;
        out.literal(",\"rows_truncated\":")?; out.boolean(report.hits_truncated())?;
        out.literal(",\"scanned_bytes\":")?; out.integer(report.stats().scanned_bytes)?;
        out.literal(",\"matches_seen\":")?; out.integer(report.matches_seen())?;
        out.literal(",\"retained_hits\":")?; out.integer(hits.len() as u64)?;
        out.literal(",\"unsupported_at\":")?;
        match report.unsupported_at() {
            Some(offset) => out.integer(offset.get())?,
            None => out.literal("null")?,
        }
        if let Some(expression) = &expression {
            out.literal(",\"expression\":")?; out.quoted(&expression.parsed.raw_query)?;
            out.literal(",\"primary_needle\":")?; out.quoted(&expression.parsed.primary_needle)?;
            out.literal(",\"query_syntax\":\"fcb-bounded-expression/1\",\"predicate_scope\":\"retained-source-capture\",\"expression_qualified\":")?;
            match report.expression_qualified() {
                Some(qualified) => out.boolean(qualified)?,
                None => out.literal("null")?,
            }
        }
        out.literal(",\"hits\":[")?;
        for (index, hit) in hits.iter().enumerate() {
            if index > 0 { out.literal(",")?; }
            out.literal("{\"hit\":")?; out.integer(index as u64)?;
            out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
            out.literal("}")?;
        }
        out.literal("]}\n")?;
        let exit = if report.input_complete() && !report.hits_truncated() {
            EXIT_OK
        } else { EXIT_PARTIAL };
        let response = self.finish(out, exit, &mut canceled)?;
        check(&mut canceled)?;
        drop(report);
        self.query = Some(AcceptedQuery { generation, source: view, hits, _lease: lease });
        Ok(response)
    }
}

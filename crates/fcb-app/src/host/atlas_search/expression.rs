#![forbid(unsafe_code)]

//! Explicit bounded expressions over the existing parser and capture verifier.
//! Metadata predicates narrow frozen membership before source/file quotas;
//! required/excluded content terms all inspect the same complete capture.
//! Source I/O and repeated predicate-verification work have separate budgets.

use std::borrow::Cow;
use fcb::search::{CaptureRequest, ParsedQuery, QueryGeneration, QueryOptions,
    ReferenceScanOracle, SearchCoverage, SearchDocument, SearchMode, UnicodeNormalization};
use crate::workspace;
use super::*;
use super::progressive::SearchExecution;

/// Aggregate original bytes consumed by all required/excluded/primary probes.
/// Capturing source remains independently limited to 32 MiB per query/index.
pub const MAX_ATLAS_EXPRESSION_SCAN_BYTES: u64 = 256 * 1024 * 1024;

pub(super) struct ExpressionSummary {
    raw: String,
    pub(super) max_scan_bytes: u64,
    pub(super) scanned_input_bytes: u64,
    pub(super) excluded: usize,
    scope_files: usize,
    work_limited: bool,
    // Covers parser/AST scratch, retained syntax and the selected-ID vector.
    // It survives publication with the receipt even after execution retires.
    _lease: ResourceLease,
}
impl ExpressionSummary {
    pub(super) fn encode(&self, out: &mut Output, primary: &str) -> Result<(), AtlasSearchError> {
        out.literal(",\"mode\":\"decoded-text-expression\",\"expression\":")?; out.quoted(&self.raw)?;
        out.literal(",\"primary_needle\":")?; out.quoted(primary)?;
        out.literal(",\"query_syntax\":\"fcb-bounded-expression/1\",\"predicate_scope\":\"same-complete-capture\",\"path_filter_domain\":\"utf8-or-escaped-search-key\"")?;
        for (key, value) in [("max_scan_bytes", self.max_scan_bytes),
            ("scanned_input_bytes", self.scanned_input_bytes),
            ("expression_excluded_files", self.excluded as u64),
            ("expression_scope_files", self.scope_files as u64)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        Ok(())
    }
}

pub(super) struct AdmittedExpression {
    pub(super) parsed: ParsedQuery,
    pub(super) summary: ExpressionSummary,
    pub(super) files: Vec<FileId>,
    pub(super) eligible: usize,
    pub(super) region: Option<scope::RegionCoverage>,
}

pub(super) struct ExpressionWork {
    parsed: ParsedQuery,
    options: AtlasSearchOptions,
    scan_id: ResourceAllocationId,
    io: workspace::IoCounts,
}

impl RetainedAtlasSearch {
    /// Admit an explicit expression without source I/O. Literal requests keep
    /// their literal meaning; only this method invokes the shared query parser.
    /// The ordinary step/page/overlay/reader paths use this same query identity.
    pub fn begin_expression_scoped(&mut self, atlas: &AtlasSession, generation: u64,
        expression: &str, scope: AtlasSearchScope, options: AtlasSearchOptions,
        max_scan_bytes: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?; options.validate()?;
        check(&mut canceled)?;
        let admitted = self.admit_expression(atlas, expression, &scope, max_scan_bytes, &mut canceled)?;
        let [state_id, scan_id] = [self.next_id()?, self.next_id()?];
        let capacity = options.max_matches.min(options.max_files);
        let text_capacity = matched_text_capacity(&admitted.parsed.primary_needle, &scope)?;
        let retained_text = retained_text_bytes(options.max_matches, text_capacity)?;
        let charge = (2 * options.max_source_bytes)
            .checked_add(capacity * (size_of::<RetainedFile>() + 64))
            .and_then(|n| n.checked_add(options.max_matches * size_of::<AtlasSearchHit>()))
            .and_then(|n| n.checked_add(MAX_DIAGNOSTICS * size_of::<Unavailable>()
                + admitted.parsed.primary_needle.len() + size_of::<SearchWork>()))
            .and_then(|n| n.checked_add(retained_text))
            .ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), state_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let excluded = admitted.summary.excluded;
        let case_sensitive = scope.case_sensitive();
        let snapshot = Snapshot { generation, needle: copy_text(&admitted.parsed.primary_needle)?,
            expression: Some(admitted.summary), structure: None, structure_rows: Vec::new(),
            scope, scope_files: Some(admitted.files), scope_region: admitted.region,
            eligible_files: admitted.eligible, files: reserve(capacity)?,
            hits: reserve(options.max_matches)?, diagnostics: reserve(MAX_DIAGNOSTICS)?,
            matched_texts: reserve(if case_sensitive { 0 } else { options.max_matches })?,
            matched_text_capacity: text_capacity, examined: excluded, scanned: 0, unavailable: 0,
            pending: admitted.eligible - excluded, matches_seen: 0, source_bytes_read: 0,
            read_calls: 0, retained_bytes: 0, truncated: false, complete: false, stop_reason: None,
            step_count: 0, last_step_files: 0, last_step_bytes: 0, last_step_calls: 0,
            index_usage: None, _lease: lease };
        let work = SearchWork { snapshot, execution: SearchExecution::Expression(ExpressionWork {
            parsed: admitted.parsed, options, scan_id, io: workspace::IoCounts::default() }) };
        let response = self.work_response(atlas, &work, "begin-expression", &mut canceled)?;
        self.validate(atlas)?; check(&mut canceled)?;
        self.pending = Some(work);
        Ok(response)
    }

    /// Reuse an explicitly prepared index's exact captures, with the same AST
    /// and full-capture predicate verifier as the live route. No source is read.
    pub fn begin_indexed_expression_scoped(&mut self, atlas: &AtlasSession, generation: u64,
        index_generation: u64, expression: &str, scope: AtlasSearchScope,
        max_matches: usize, max_scan_bytes: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let work = self.prepare_indexed_work(atlas, generation, index_generation, expression,
            scope, max_matches, max_scan_bytes, true, &mut canceled)?;
        let response = self.work_response(atlas, &work, "begin-indexed-expression", &mut canceled)?;
        self.validate(atlas)?; check(&mut canceled)?;
        self.pending = Some(work);
        Ok(response)
    }

    /// Reserve before parser allocation, then freeze the intersection without
    /// opening files. A non-UTF-8 path uses the existing escaped search-key
    /// convention solely for predicates; activation retains its original bytes.
    pub(super) fn admit_expression(&mut self, atlas: &AtlasSession, raw: &str,
        scope: &AtlasSearchScope, max_scan_bytes: u64, canceled: &mut impl FnMut() -> bool)
        -> Result<AdmittedExpression, AtlasSearchError> {
        if max_scan_bytes > MAX_ATLAS_EXPRESSION_SCAN_BYTES { return Err(AtlasSearchError::InvalidLimits); }
        if raw.len() > fcb::search::MAX_QUERY_LEN {
            return Err(IndexError::from(fcb::search::QueryError::NeedleTooLong).into());
        }
        let (eligible, region) = scope.admission_counts(atlas, canceled)?;
        let charge = eligible.checked_mul(size_of::<FileId>())
            .and_then(|n| n.checked_add(128 * 1024)).ok_or(AppError::Admission)?;
        let allocation = self.next_id()?;
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let parsed = ParsedQuery::parse(raw).map_err(IndexError::from)?;
        let catalog = atlas.atlas().catalog();
        let mut files = reserve(eligible)?;
        for (ordinal, entry) in catalog.entries().iter().enumerate() {
            check(canceled)?;
            if !scope.includes_path(entry.path().raw().as_bytes()) { continue; }
            let key = match entry.path().as_str() {
                Ok(path) => Cow::Borrowed(path),
                Err(_) => Cow::Owned(entry.path().display_escaped().to_string()),
            };
            if ReferenceScanOracle::matches_path_filters(&key, &parsed.path_filters)
                && ReferenceScanOracle::matches_lang_filters(&key, &parsed.lang_filters) {
                if files.len() == eligible { return Err(AtlasSearchError::WrongAtlas); }
                files.push(catalog.file_id(ordinal).ok_or(AtlasSearchError::WrongAtlas)?);
            }
        }
        files.sort_unstable();
        let scope_files = files.len();
        let excluded = eligible.checked_sub(scope_files).ok_or(AtlasSearchError::WrongAtlas)?;
        let summary = ExpressionSummary { raw: copy_text(raw)?, max_scan_bytes,
            scanned_input_bytes: 0, excluded, scope_files, work_limited: false, _lease: lease };
        self.validate(atlas)?; check(canceled)?;
        Ok(AdmittedExpression { parsed, summary, files, eligible, region })
    }

    pub(super) fn advance_expression_work(&self, atlas: &AtlasSession, candidate: &mut Snapshot,
        work: &mut ExpressionWork, canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        update_coverage(atlas, candidate, work)?;
        if candidate.stop_reason.is_none() { self.expression_one_file(atlas, candidate, work, canceled)?; }
        update_coverage(atlas, candidate, work)
    }

    fn expression_one_file(&self, atlas: &AtlasSession, candidate: &mut Snapshot,
        work: &mut ExpressionWork, canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        let expression = candidate.expression.as_ref().ok_or(AtlasSearchError::WrongAtlas)?;
        let ordinal = candidate.examined.checked_sub(expression.excluded).ok_or(AtlasSearchError::WrongAtlas)?;
        let file = candidate.scope_files.as_ref().and_then(|files| files.get(ordinal)).copied()
            .ok_or(AtlasSearchError::WrongAtlas)?;
        let remaining_work = expression.max_scan_bytes.checked_sub(expression.scanned_input_bytes)
            .ok_or(AtlasSearchError::WrongAtlas)?;
        let catalog = atlas.atlas().catalog();
        let entry = catalog.entry(file).ok_or(AtlasSearchError::WrongAtlas)?;
        candidate.examined += 1;
        if entry.observed_bytes() > work.options.max_file_bytes as u64 {
            candidate.unavailable(file, "FILE_BYTE_LIMIT"); return Ok(());
        }
        let owner = self.manifest.owner();
        let generation = QueryGeneration::new(owner, candidate.generation)
            .map_err(|_| AtlasSearchError::IdentityExhausted)?;
        let revision = SourceRevision::new(owner, candidate.generation)
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
        let key = match entry.path().as_str() {
            Ok(path) => Cow::Borrowed(path),
            Err(_) => Cow::Owned(entry.path().display_escaped().to_string()),
        };
        let document = SearchDocument::new(file, &key, &capture);
        let before_hits = candidate.hits.len();
        let remaining = work.options.max_matches - before_hits;
        let mut options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
            case_sensitive: candidate.scope.case_sensitive(), normalization: UnicodeNormalization::Exact,
        });
        // One-hit lookahead proves an omitted primary occurrence after all
        // predicates. A full output buffer alone cannot establish truncation.
        options.max_matches = remaining.max(1);
        options.max_bytes_scanned = Some(remaining_work);
        let text_capacity = candidate.matched_text_capacity.max(candidate.needle.len());
        let scratch = text_matching::scan_scratch_bytes(options.max_matches, text_capacity)?;
        let _scan_lease = self.budget.try_reserve_managed(owner, work.scan_id, ByteLength::new(scratch as u64))
            .map_err(|_| AppError::Admission)?;
        let report = ReferenceScanOracle::scan_document_with_cancel(&document, &work.parsed, &options,
            || canceled() || atlas.validate_active().is_err()).map_err(IndexError::from)?;
        self.validate(atlas)?; check(canceled)?;
        if report.coverage == SearchCoverage::CanceledEarly { return Err(AtlasSearchError::Canceled); }
        let expression = candidate.expression.as_mut().ok_or(AtlasSearchError::WrongAtlas)?;
        expression.scanned_input_bytes = expression.scanned_input_bytes.checked_add(report.scanned_bytes)
            .filter(|&bytes| bytes <= expression.max_scan_bytes).ok_or(AtlasSearchError::WrongAtlas)?;
        expression.work_limited = matches!(report.coverage, SearchCoverage::BudgetExhausted { .. });
        let work_limited = expression.work_limited;
        candidate.scanned += 1;
        let counted = report.total_matches_counted.min(remaining + 1);
        candidate.matches_seen = candidate.matches_seen.checked_add(counted as u64)
            .ok_or(AtlasSearchError::IdentityExhausted)?;
        candidate.truncated = counted > remaining || matches!(report.coverage, SearchCoverage::TruncatedAtLimit { .. });
        if !report.unsupported_files.is_empty() { candidate.unavailable(file, "unsupported-text"); }
        else if work_limited { candidate.unavailable(file, "verification-byte-limit"); }
        let node = atlas.atlas().node_for_file(file).map_err(AtlasSessionError::from)?;
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
            let witness = CaptureWitness::new(capture.bytes(), &mut || canceled() || atlas.validate_active().is_err())?;
            candidate.files.push(RetainedFile { capture, node, hits, witness });
        }
        Ok(())
    }
}

fn update_coverage(atlas: &AtlasSession, q: &mut Snapshot, work: &ExpressionWork) -> Result<(), AtlasSearchError> {
    let expression = q.expression.as_ref().ok_or(AtlasSearchError::WrongAtlas)?;
    let selected_examined = q.examined.checked_sub(expression.excluded).ok_or(AtlasSearchError::WrongAtlas)?;
    q.pending = q.eligible_files.checked_sub(q.examined).ok_or(AtlasSearchError::WrongAtlas)?;
    q.source_bytes_read = work.io.bytes; q.read_calls = work.io.calls;
    q.stop_reason = if q.truncated { Some(AtlasSearchStop::MatchLimit) }
        else if expression.work_limited { Some(AtlasSearchStop::VerificationByteLimit) }
        else if q.pending == 0 { Some(AtlasSearchStop::AllFilesExamined) }
        else if selected_examined == work.options.max_files { Some(AtlasSearchStop::FileLimit) }
        else if work.io.bytes >= work.options.max_source_bytes as u64 { Some(AtlasSearchStop::SourceByteLimit) }
        else if expression.scanned_input_bytes == expression.max_scan_bytes { Some(AtlasSearchStop::VerificationByteLimit) }
        else { None };
    q.complete = q.stop_reason == Some(AtlasSearchStop::AllFilesExamined) && atlas.atlas().discovery_complete()
        && q.scope_region.is_none_or(|region| region.missing == 0) && q.pending == 0 && q.unavailable == 0;
    Ok(())
}

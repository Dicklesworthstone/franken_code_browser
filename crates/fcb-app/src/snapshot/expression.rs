#![forbid(unsafe_code)]

//! Read-only snapshot expression command. This is a consumer of the public
//! resumable query; it never evaluates repository text as commands/configuration.

use std::io::{Read, Seek};
use fcb::search::StreamReadStep;
use fcb::search::snapshot_scope::SnapshotScope;
use fcb::search::expression::{ExpressionError, ExpressionOptions, ExpressionPlan, ExpressionQuery, ExpressionState, ExpressionReport};
use fcb::search::paged_snapshot::{PagedSearchError, PagedSnapshotError, PagedSnapshot};
use crate::{generation, allocation, file_id, owner, revision, EXIT_NO_MATCH, EXIT_OK, EXIT_PARTIAL};
use super::{Settings, Failure, ResourceBudget, Output, RawPath, begin, catalog, matching};

impl From<ExpressionError> for Failure {
    fn from(error: ExpressionError) -> Self {
        let canceled = matches!(error, ExpressionError::Canceled
            | ExpressionError::Syntax(fcb::search::QueryError::Canceled)
            | ExpressionError::Search(PagedSearchError::Canceled)
            | ExpressionError::Search(PagedSearchError::Archive(PagedSnapshotError::Canceled)));
        Self { code: error.to_string(), canceled }
    }
}

pub(super) fn execute(settings: &Settings, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    let raw = settings.expression.as_deref().ok_or_else(|| Failure::new("CLI_INVALID_NEEDLE"))?;
    // Parse and reserve every pattern BEFORE opening any input archive/catalog.
    let plan = ExpressionPlan::parse_with_matching(owner(), raw, !settings.ignore_case,
        budget, allocation(150), allocation(1000))?;
    let path = settings.source.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?;
    let mut archive = catalog::open(path, settings.catalog.as_deref(), settings.catalog_pin,
        budget, [allocation(104), allocation(110)], canceled)?;
    let options = ExpressionOptions { generation: generation(), first_file: file_id(), first_revision: revision(),
        max_matches: settings.limit, max_scan_bytes: settings.max_scan_bytes };
    let allocations = [allocation(151), allocation(152), allocation(153)];
    let scope = settings.search_scope();
    let query = ExpressionQuery::new_scoped_page(&mut archive, &plan, options,
        scope, settings.skip_matches, settings.count_all_matches, budget, allocations)?;
    let report = finish(query, budget, canceled)?;
    render(Presentation { command: "snapshot-search", scope, max_matches: settings.limit,
        max_scan_bytes: settings.max_scan_bytes, json: settings.json, index_available: false },
        &archive, &plan, &report, out, canceled, |_| Ok(()))
}

/// Direct and indexed queries share the exact same continuation and partial-work
/// contract. A report can be complete even when its retained result window is capped.
pub(super) fn finish<R: Read + Seek>(mut query: ExpressionQuery<'_, '_, R>,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<ExpressionReport, Failure> {
    while query.state() == ExpressionState::Pending {
        query.step(StreamReadStep::default(), generation(), budget, &mut *canceled)?;
    }
    let report = query.finish()?;
    if canceled() { return Err(Failure::canceled()); }
    Ok(report)
}

pub(super) struct Presentation<'a> {
    pub command: &'static str,
    pub scope: SnapshotScope<'a>,
    pub max_matches: usize,
    pub max_scan_bytes: u64,
    pub json: bool,
    pub index_available: bool,
}

/// Keep source witnesses, scope closure and result-window semantics identical
/// across direct scanning and all saved index layouts. Extra fields describe
/// the separately pinned index and its physical I/O, never substitute for hits.
pub(super) fn render<R: Read + Seek>(settings: Presentation<'_>, archive: &PagedSnapshot<R>,
    plan: &ExpressionPlan, report: &ExpressionReport, out: &mut Output,
    canceled: &mut impl FnMut() -> bool,
    index_fields: impl FnOnce(&mut Output) -> Result<(), Failure>) -> Result<u8, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    let stats = report.stats();
    let raw = &plan.syntax().raw_query;
    let scope = settings.scope;
    if settings.json {
        begin(out, settings.command)?;
        index_fields(out)?;
        out.literal(",\"mode\":")?; out.quoted(plan.realized_mode())?;
        out.literal(",\"case_sensitive\":")?; out.boolean(plan.case_sensitive())?;
        out.literal(",\"normalization\":\"exact\",\"expression\":")?; out.quoted(raw)?;
        out.literal(",\"primary_needle\":")?; out.quoted(&plan.syntax().primary_needle)?;
        out.literal(",\"index_used\":")?; out.boolean(settings.index_available)?;
        out.literal(",\"index_policy\":")?;
        out.quoted(if !settings.index_available { "direct-verified-member-scan" }
            else if plan.case_sensitive() { "exact-compatible-index" }
            else { "ascii-case-variants-with-source-fallback" })?;
        out.literal(",\"index_prefilter\":")?;
        out.quoted(if settings.index_available { "mandatory-primary-literal-only" } else { "none" })?;
        for (name, count) in [
            ("index_eliminated_files", stats.index_eliminated as u64),
            ("index_candidates", stats.index_candidates as u64),
            ("index_fallback_files", stats.index_fallbacks as u64),
            ("posting_list_lookups", stats.posting_list_lookups as u64),
            ("posting_entries_visited", stats.posting_entries_visited as u64),
            ("posting_membership_lookups", stats.posting_membership_lookups as u64),
        ] {
            out.literal(",")?; out.quoted(name)?; out.literal(":")?; out.integer(count)?;
        }
        out.literal(",\"posting_cursor_complete\":")?; out.boolean(stats.posting_cursor_complete)?;
        out.literal(",\"source_scope\":\"saved-observations-only\",\"live_roots_accessed\":false,\"identity_scope\":\"response-local\",\"predicate_scope\":\"same-complete-capture\",\"path_filter_domain\":\"utf8-or-escaped-search-key\",\"strategy\":\"one-verified-member-expression\",\"snapshot_digest\":")?;
        out.quoted(&report.archive_digest().to_hex())?;
        out.literal(",\"policy\":")?; out.quoted(archive.directory().policy())?;
        out.literal(",\"discovery_complete\":")?; out.boolean(archive.directory().discovery_complete())?;
        matching::scope_fields(out, scope, report.selected_members(), report.captured_members(),
            report.membership_complete(), report.is_complete())?;
        out.literal(",\"workspace_complete\":")?;
        out.boolean(scope == SnapshotScope::Archive && report.is_complete())?;
        out.literal(",\"truncated\":")?; out.boolean(report.hits_truncated())?;
        out.literal(",\"count_all_matches\":")?; out.boolean(report.count_all_matches())?;
        matching::window_fields(out, report.skip_matches(), report.skipped_matches(),
            report.hits().len(), report.has_more_matches(), report.next_match_offset())?;
        out.literal(",\"matches_seen_exact\":")?; out.boolean(report.is_complete())?;
        out.literal(",\"match_count_complete\":")?; out.boolean(report.is_complete())?;
        out.literal(",\"work_limited\":")?; out.boolean(report.state() == ExpressionState::WorkLimit)?;
        out.literal(",\"max_scan_bytes\":")?; out.integer(settings.max_scan_bytes)?;
        out.literal(",\"archive_validation_bytes\":")?; out.integer(archive.directory().validation_stats().bytes_read)?;
        catalog::validation_fields(out, archive.directory())?;
        for (name, count) in [
            ("known_files", archive.directory().len() as u64),
            ("scope_files", stats.scope_files as u64),
            ("metadata_examined", stats.metadata_examined as u64),
            ("metadata_excluded_files", stats.metadata_excluded as u64),
            ("unavailable_files_count", stats.unavailable_files as u64),
            ("unsupported_text_files_count", stats.unsupported_files as u64),
            ("members_loaded", stats.members_loaded as u64),
            ("loaded_source_bytes", stats.loaded_bytes),
            ("peak_member_bytes", stats.peak_member_bytes as u64),
            ("scanned_input_bytes", stats.scanned_bytes),
            ("predicate_scans", stats.predicate_scans as u64),
            ("predicate_rejections", stats.predicate_rejections as u64),
            ("primary_scans", stats.primary_scans as u64),
            ("matches_seen", report.matches_seen()),
            ("stored_hits", report.hits().len() as u64),
            ("max_matches", settings.max_matches as u64),
            ("matching_files", report.matching_files() as u64),
            ("retained_matching_files", report.retained_matching_files() as u64),
        ] {
            out.literal(",")?; out.quoted(name)?; out.literal(":")?; out.integer(count)?;
        }
        out.literal(",\"hits\":[")?;
    } else {
        out.literal(if report.is_complete() { "Complete saved-scope expression search; no live root accessed.\n" }
            else { "PARTIAL saved-scope expression search; no live root accessed.\n" })?;
        matching::scope_human(out, scope, report.selected_members(), report.captured_members(), report.membership_complete())?;
        out.literal("Query: ")?; out.human_text(raw)?; out.literal("\n")?;
        if settings.index_available {
            out.literal("Pinned index filters only the mandatory primary literal; whole-member predicates and fallback members are source-verified.\n")?;
        }
        matching::window_human(out, report.skip_matches(), report.skipped_matches(),
            report.hits().len(), report.has_more_matches(), report.next_match_offset())?;
        if !plan.case_sensitive() { out.literal("Text case: Unicode lowercase comparison for every positive and excluded term.\n")?; }
        if report.count_all_matches() {
            out.literal("Primary matches seen: ")?; out.literal(&report.matches_seen().to_string())?;
            out.literal(if report.is_complete() { " (exact)" } else { " (lower bound; saved scope incomplete)" })?;
            out.literal("; retained hits: ")?; out.literal(&report.hits().len().to_string())?;
            out.literal("; matching files: ")?; out.literal(&report.matching_files().to_string())?;
            out.literal(". Whole-member conjunctions and exclusions filter qualifying files.\n")?;
        }
    }
    for (i, hit) in report.hits().iter().enumerate() {
        if canceled() { return Err(Failure::canceled()); }
        let member = archive.directory().member(hit.ordinal()).ok_or_else(|| Failure::new("SNAPSHOT_MEMBER_MISSING"))?;
        let path = RawPath::from_bytes(member.path).to_path_buf();
        if settings.json {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(hit.file().get())?;
            out.literal(",\"revision\":")?; out.integer(hit.revision().get())?;
            out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
            out.literal(",\"path\":")?; out.path(&path)?;
            out.literal(",\"source_digest\":")?; out.quoted(&hit.source_digest().to_hex())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
            out.literal(",\"matched_text\":")?;
            // The report retains exact range/digest witnesses, not payloads.
            // Lowercase matches can have a different spelling and width from
            // the needle; never label the query as the original source text.
            if plan.case_sensitive() { out.quoted(&plan.syntax().primary_needle)?; }
            else { out.literal("null")?; }
            out.literal("}")?;
        } else {
            out.path(&path)?; out.literal(" bytes ")?; out.literal(&hit.original_range().start().get().to_string())?;
            out.literal("..")?; out.literal(&hit.original_range().end().get().to_string())?; out.literal("\n")?;
        }
    }
    if settings.json { out.literal("]}\n")?; }
    else {
        out.literal("Matching work bytes (including predicates): ")?; out.literal(&stats.scanned_bytes.to_string())?;
        out.literal("; member bytes loaded: ")?; out.literal(&stats.loaded_bytes.to_string())?; out.literal("\n")?;
        if settings.index_available {
            out.literal("Index-pruned members: ")?; out.literal(&stats.index_eliminated.to_string())?;
            out.literal("; candidate members: ")?; out.literal(&stats.index_candidates.to_string())?;
            out.literal("; fallback members: ")?; out.literal(&stats.index_fallbacks.to_string())?; out.literal("\n")?;
        }
        if report.state() == ExpressionState::WorkLimit { out.literal("Matching-work allowance exhausted; no exhaustive-negative claim.\n")?; }
    }
    Ok(if !report.is_complete() || (!report.count_all_matches() && report.skipped_matches() > 0) {
        EXIT_PARTIAL
    } else if report.matches_seen() == 0 { EXIT_NO_MATCH } else { EXIT_OK })
}

#![forbid(unsafe_code)]

//! Read-only snapshot expression command. This is a consumer of the public
//! resumable query; it never evaluates repository text as commands/configuration.

use fcb::search::{StreamReadStep};
use fcb::search::expression::{ExpressionError, ExpressionOptions, ExpressionPlan, ExpressionQuery, ExpressionState};
use fcb::search::paged_snapshot::{PagedSearchError, PagedSnapshotError};
use crate::{generation, allocation, file_id, owner, revision, EXIT_NO_MATCH, EXIT_OK, EXIT_PARTIAL};
use super::{Settings, Failure, ResourceBudget, Output, RawPath, begin, catalog};

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
    let plan = ExpressionPlan::parse(owner(), raw, budget, allocation(150), allocation(1000))?;
    let path = settings.source.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?;
    let mut archive = catalog::open(path, settings.catalog.as_deref(), settings.catalog_pin,
        budget, [allocation(104), allocation(110)], canceled)?;
    let options = ExpressionOptions { generation: generation(), first_file: file_id(), first_revision: revision(),
        max_matches: settings.limit, max_scan_bytes: settings.max_scan_bytes };
    let mut query = ExpressionQuery::new(&mut archive, &plan, options, budget,
        [allocation(151), allocation(152), allocation(153)])?;
    while query.state() == ExpressionState::Pending {
        query.step(StreamReadStep::default(), generation(), budget, &mut *canceled)?;
    }
    let report = query.finish()?;
    if canceled() { return Err(Failure::canceled()); }
    let stats = report.stats();
    if settings.json {
        begin(out, "snapshot-search")?;
        out.literal(",\"mode\":\"decoded-text-expression\",\"expression\":")?; out.quoted(raw)?;
        out.literal(",\"source_scope\":\"saved-observations-only\",\"live_roots_accessed\":false,\"identity_scope\":\"response-local\",\"predicate_scope\":\"same-complete-capture\",\"path_filter_domain\":\"utf8-or-escaped-search-key\",\"strategy\":\"one-verified-member-expression\",\"snapshot_digest\":")?;
        out.quoted(&report.archive_digest().to_hex())?;
        out.literal(",\"policy\":")?; out.quoted(archive.directory().policy())?;
        out.literal(",\"discovery_complete\":")?; out.boolean(archive.directory().discovery_complete())?;
        out.literal(",\"workspace_complete\":")?; out.boolean(report.is_complete())?;
        out.literal(",\"truncated\":")?; out.boolean(report.state() == ExpressionState::Truncated)?;
        out.literal(",\"work_limited\":")?; out.boolean(report.state() == ExpressionState::WorkLimit)?;
        out.literal(",\"max_scan_bytes\":")?; out.integer(settings.max_scan_bytes)?;
        out.literal(",\"archive_validation_bytes\":")?; out.integer(archive.directory().validation_stats().bytes_read)?;
        catalog::validation_fields(out, archive.directory())?;
        for (name, count) in [
            ("known_files", archive.directory().len() as u64),
            ("scope_files", stats.scope_files as u64),
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
        ] {
            out.literal(",")?; out.quoted(name)?; out.literal(":")?; out.integer(count)?;
        }
        out.literal(",\"hits\":[")?;
    } else {
        out.literal(if report.is_complete() { "Complete saved-scope expression search; no live root accessed.\n" }
            else { "PARTIAL saved-scope expression search; no live root accessed.\n" })?;
        out.literal("Query: ")?; out.human_text(raw)?; out.literal("\n")?;
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
            out.literal(",\"matched_text\":")?; out.quoted(&plan.syntax().primary_needle)?;
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
        if report.state() == ExpressionState::WorkLimit { out.literal("Matching-work allowance exhausted; no exhaustive-negative claim.\n")?; }
    }
    Ok(if !report.is_complete() { EXIT_PARTIAL } else if report.matches_seen() == 0 { EXIT_NO_MATCH } else { EXIT_OK })
}

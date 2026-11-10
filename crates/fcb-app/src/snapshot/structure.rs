#![forbid(unsafe_code)]

//! Saved structural navigation through the public one-member query. This CLI
//! performs no extraction of its own and never converts member names into live
//! source access. Trusted catalogs retain their explicit integrity boundary.

use fcb::search::SymbolNameMode;
use fcb::search::snapshot_structure::{StructureError, StructureMode, StructureOptions,
    StructureQuery, StructureState};
use crate::{allocation, file_id, generation, revision, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL};
use super::{Settings, Failure, ResourceBudget, Output, RawPath, begin, catalog};

impl From<StructureError> for Failure {
    fn from(error: StructureError) -> Self {
        Self { code: error.to_string(), canceled: error.is_canceled() }
    }
}

pub(super) fn execute(settings: &Settings, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    let mode = settings.structure_mode.ok_or_else(|| Failure::new("CLI_INVALID_VALUE"))?;
    let name = settings.structure_name.as_deref().ok_or_else(|| Failure::new("CLI_INVALID_NEEDLE"))?;
    let path = settings.source.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?;
    let mut archive = catalog::open(path, settings.catalog.as_deref(), settings.catalog_pin,
        budget, [allocation(104), allocation(110)], canceled)?;
    let options = StructureOptions { generation: generation(), first_file: file_id(),
        first_revision: revision(), mode, name_mode: settings.name_mode,
        max_matches: settings.limit, max_source_bytes: settings.max_structure_source_bytes };
    let mut query = StructureQuery::new(&mut archive, name, options, budget,
        [allocation(160), allocation(161), allocation(162), allocation(163), allocation(164)])?;
    while query.state() == StructureState::Pending {
        query.step(generation(), budget, &mut *canceled)?;
    }
    let report = query.finish()?;
    if canceled() { return Err(Failure::canceled()); }
    let stats = report.stats();
    if settings.json {
        begin(out, "snapshot-structure")?;
        out.literal(",\"source_scope\":\"saved-observations-only\",\"scope\":\"whole-saved-archive\",\"live_roots_accessed\":false,\"identity_scope\":\"response-local\",\"payload_residency\":\"one-verified-member\",\"retained_source_payload_bytes\":\"0\",\"snapshot_digest\":")?;
        out.quoted(&report.archive_digest().to_hex())?;
        out.literal(",\"policy\":")?; out.quoted(archive.directory().policy())?;
        out.literal(",\"requested_mode\":")?; out.quoted(mode.requested())?;
        out.literal(",\"realized_mode\":")?; out.quoted(mode.realized())?;
        out.literal(",\"evidence_level\":")?; out.quoted(mode.realized())?;
        out.literal(",\"semantic_complete\":false,\"completeness_basis\":\"admitted-extraction-processing\",\"name_case\":\"sensitive\",\"name\":")?;
        out.quoted(name)?;
        out.literal(",\"name_mode\":")?; out.quoted(match settings.name_mode {
            SymbolNameMode::Exact => "exact", SymbolNameMode::Prefix => "prefix", SymbolNameMode::Contains => "contains",
        })?;
        if mode == StructureMode::Occurrences {
            out.literal(",\"includes_comments_and_literals\":true,\"compiler_resolved\":false,\"token_policy\":\"ascii-alphanumeric-underscore-dollar-plus-nonascii-nonwhitespace-noncontrol-v1\",\"name_syntax\":\"single-token-no-leading-ascii-digit\",\"unicode_normalization\":\"none\"")?;
        }
        out.literal(",\"discovery_complete\":")?; out.boolean(archive.directory().discovery_complete())?;
        out.literal(",\"workspace_complete\":")?; out.boolean(report.is_complete())?;
        out.literal(",\"matches_seen_exact\":")?; out.boolean(report.is_complete())?;
        out.literal(",\"truncated\":")?; out.boolean(report.state() == StructureState::Truncated)?;
        out.literal(",\"work_limited\":")?; out.boolean(report.state() == StructureState::WorkLimit)?;
        out.literal(",\"state\":")?; out.quoted(match report.state() {
            StructureState::Complete => "complete", StructureState::Truncated => "truncated",
            StructureState::WorkLimit => "work-limit", _ => return Err(StructureError::Pending.into()),
        })?;
        for (key, value) in [
            ("known_files", archive.directory().len() as u64),
            ("captured_files", archive.directory().captured_files() as u64),
            ("max_hits", settings.limit as u64), ("max_source_bytes", settings.max_structure_source_bytes),
            ("per_file_source_limit", mode.source_limit() as u64),
            ("retained_hits", report.hits().len() as u64), ("matches_seen", report.matches_seen()),
            ("members_visited", stats.members_visited as u64), ("members_loaded", stats.members_loaded as u64),
            ("files_searched", stats.files_searched as u64), ("unsupported_files", stats.unsupported_files as u64),
            ("unavailable_files", stats.unavailable_files as u64), ("source_limited_files", stats.source_limited_files as u64),
            ("extraction_limited_files", stats.extraction_limited_files as u64), ("metadata_omitted", stats.metadata_omitted as u64),
            ("loaded_source_bytes", stats.loaded_bytes), ("peak_source_bytes", stats.peak_source_bytes as u64),
            ("metadata_bytes", stats.metadata_bytes as u64),
            ("archive_validation_bytes", archive.directory().validation_stats().bytes_read),
            ("archive_validation_read_calls", archive.directory().validation_stats().read_calls),
            ("member_payload_bytes_loaded", archive.load_stats().bytes_read),
            ("member_read_calls", archive.load_stats().read_calls),
        ] { out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?; }
        catalog::validation_fields(out, archive.directory())?;
        out.literal(",\"hits\":[")?;
    } else {
        if !archive.directory().fully_verified_on_open() {
            out.literal("Trusted saved catalog; archive boundaries checked. Unread body integrity is unchecked; loaded members are digest-verified.\n")?;
        }
        out.literal(if report.is_complete() { "Complete saved-scope structural search; no live roots accessed.\n" }
            else { "PARTIAL saved-scope structural search; no live roots accessed.\n" })?;
        out.literal("Evidence: ")?; out.literal(mode.realized())?; out.literal(". Name: ")?; out.human_text(name)?; out.literal("\n")?;
        if mode == StructureMode::Occurrences {
            out.literal("Whole-token text includes comments and literals; compiler bindings are not resolved.\n")?;
        } else if mode == StructureMode::Symbols {
            out.literal("Heuristic declaration candidates; absence does not prove no compiler definition exists.\n")?;
        }
    }
    for (i, hit) in report.hits().iter().enumerate() {
        if canceled() { return Err(Failure::canceled()); }
        let member = archive.directory().member(hit.ordinal()).ok_or_else(|| Failure::new("SNAPSHOT_MEMBER_MISSING"))?;
        let path = RawPath::from_bytes(member.path).to_path_buf();
        if settings.json {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"hit_id\":")?; out.integer(i as u64 + 1)?;
            out.literal(",\"member\":")?; out.integer(hit.ordinal() as u64)?;
            out.literal(",\"file_id\":")?; out.integer(hit.file().get())?;
            out.literal(",\"revision\":")?; out.integer(hit.revision().get())?;
            out.literal(",\"path\":")?; out.path(&path)?;
            out.literal(",\"source_digest\":")?; out.quoted(&hit.source_digest().to_hex())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
            out.literal(",\"evidence_range\":")?; out.range(hit.evidence_range())?;
            out.literal(",\"name\":")?; out.quoted(hit.name())?;
            out.literal(",\"kind\":")?; out.quoted(hit.kind())?;
            out.literal(",\"language\":")?; out.quoted(hit.language())?;
            out.literal(",\"candidate_id\":")?; out.integer(hit.candidate_id())?;
            out.literal(",\"heading_slug\":")?;
            if let Some(slug) = hit.heading_slug() { out.quoted(slug)?; } else { out.literal("null")?; }
            out.literal(",\"evidence_level\":")?; out.quoted(mode.realized())?; out.literal("}")?;
        } else {
            out.path(&path)?; out.literal(" bytes ")?; out.literal(&hit.original_range().start().get().to_string())?;
            out.literal("..")?; out.literal(&hit.original_range().end().get().to_string())?;
            out.literal(" ")?; out.literal(hit.kind())?; out.literal(" ")?; out.human_text(hit.name())?; out.literal("\n")?;
        }
    }
    if settings.json { out.literal("]}\n")?; }
    else {
        out.literal("Matching candidates seen: ")?; out.literal(&report.matches_seen().to_string())?;
        out.literal("; source bytes loaded: ")?; out.literal(&stats.loaded_bytes.to_string())?;
        out.literal("; retained source bytes: 0.\n")?;
        out.literal("Coverage: unsupported=")?; out.literal(&stats.unsupported_files.to_string())?;
        out.literal(", unavailable=")?; out.literal(&stats.unavailable_files.to_string())?;
        out.literal(", source-limited=")?; out.literal(&stats.source_limited_files.to_string())?;
        out.literal(", extraction-limited=")?; out.literal(&stats.extraction_limited_files.to_string())?;
        out.literal(", metadata-omitted=")?; out.literal(&stats.metadata_omitted.to_string())?; out.literal(".\n")?;
        if report.state() == StructureState::WorkLimit {
            out.literal("Aggregate source allowance exhausted; no exhaustive-negative claim.\n")?;
        }
    }
    Ok(if !report.is_complete() { EXIT_PARTIAL } else if report.matches_seen() == 0 { EXIT_NO_MATCH } else { EXIT_OK })
}

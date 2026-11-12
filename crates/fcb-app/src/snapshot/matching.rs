#![forbid(unsafe_code)]

//! Shared CLI composition for saved literal matching, counts and source witnesses.
//! Matching stays in the public streaming/paged engines. A result witness is
//! decoded from one digest-verified saved member, never copied from the query.

use std::io::{Read, Seek};
use fcb::ByteLength;
use fcb::source::{CaptureEncodingMap, SpanKind, detect_encoding};
use fcb::search::{RawPath, ResourceBudget, StreamingNeedle, StreamReadStep};
use fcb::search::paged_snapshot::{PagedSnapshot, PagedQuery, PagedQueryOptions, PagedQueryState,
    PagedReport, VerifiedMember, PagedSearchError};
use fcb_core::ResourceAllocationId;
use crate::{allocation, file_id, generation, owner, revision, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL};
use crate::output::Output;
use super::Failure;

// A 1024-byte input needle can expand during scalar lowercasing. Preserve every
// contributing original UTF-8/UTF-16 unit within the same bound as saved-reader
// activation; decoding and response storage retain their separate admissions.
const MAX_WITNESS_BYTES: usize = 12 * 1024;

pub(super) fn direct<R: Read + Seek>(archive: &mut PagedSnapshot<R>,
    text: Option<&str>, raw: Option<&[u8]>, ignore_case: bool, count_all_matches: bool,
    max_matches: usize, budget: &ResourceBudget, ids: [ResourceAllocationId; 4],
    canceled: &mut impl FnMut() -> bool) -> Result<PagedReport, Failure> {
    let needle = match (text, raw) {
        (Some(text), None) => StreamingNeedle::text_matching(owner(), text, !ignore_case, budget, ids[0]),
        (None, Some(bytes)) if !ignore_case => StreamingNeedle::raw(owner(), bytes, budget, ids[0]),
        _ => return Err(Failure::new("CLI_INVALID_NEEDLE")),
    }.map_err(crate::AppError::from)?;
    let options = PagedQueryOptions { generation: generation(), first_file: file_id(),
        first_revision: revision(), max_matches };
    let allocations = [ids[1], ids[2], ids[3]];
    let query = if count_all_matches {
        PagedQuery::new_counting(archive, &needle, options, budget, allocations)?
    } else {
        PagedQuery::new(archive, &needle, options, budget, allocations)?
    };
    finish(query, budget, canceled)
}
pub(super) fn finish<R: Read + Seek>(mut query: PagedQuery<'_, '_, R>,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<PagedReport, Failure> {
    while query.state() == PagedQueryState::Pending {
        query.step(StreamReadStep::default(), generation(), budget, &mut *canceled)?;
    }
    let report = query.finish()?;
    if canceled() { return Err(Failure::canceled()); }
    Ok(report)
}
pub(super) fn fields(out: &mut Output, report: &PagedReport, raw: bool,
    ignore_case: bool, index_available: bool) -> Result<(), Failure> {
    out.literal(",\"mode\":")?;
    out.quoted(if raw { "original-bytes" } else if ignore_case { "unicode-lowercase-literal" } else { "exact-text" })?;
    out.literal(",\"case_sensitive\":")?; out.boolean(!ignore_case)?;
    out.literal(",\"normalization\":")?;
    out.quoted(if ignore_case { "unicode-scalar-lowercase" } else { "none" })?;
    out.literal(",\"full_case_folding\":false,\"canonical_normalization\":false,\"index_used\":")?;
    out.boolean(index_available && !ignore_case)?;
    out.literal(",\"index_policy\":")?;
    out.quoted(if index_available && !ignore_case { "exact-compatible-index" } else { "direct-verified-member-scan" })?;
    out.literal(",\"index_bypass_reason\":")?;
    if index_available && ignore_case { out.quoted("incompatible-normalization")?; } else { out.literal("null")?; }
    out.literal(",\"workspace_complete\":")?; out.boolean(report.is_complete())?;
    out.literal(",\"truncated\":")?; out.boolean(report.truncated())?;
    out.literal(",\"count_all_matches\":")?; out.boolean(report.count_all_matches())?;
    out.literal(",\"matches_seen_exact\":")?; out.boolean(report.is_complete())?;
    for (name, value) in [("matches_seen", report.matches_seen()),
        ("stored_hits", report.hits().len() as u64), ("max_matches", report.max_matches() as u64),
        ("matching_files", report.matching_files() as u64),
        ("retained_matching_files", report.retained_matching_files() as u64)] {
        out.literal(",")?; out.quoted(name)?; out.literal(":")?; out.integer(value)?;
    }
    // Legacy member-load counters cover query work. Witness re-reads below are
    // separately visible, rather than being hidden or relabeled as query scans.
    out.literal(",\"member_payload_io_scope\":\"query\",\"match_witness_policy\":\"digest-verified-saved-source\"")?;
    Ok(())
}
pub(super) fn human(out: &mut Output, report: &PagedReport, ignore_case: bool,
    index_available: bool) -> Result<(), Failure> {
    if ignore_case {
        out.literal("Unicode scalar-lowercase literal matching; no full case folding or canonical normalization.\n")?;
        if index_available { out.literal("The exact index is incompatible with this mode; all saved members are verified and scanned directly.\n")?; }
    }
    if report.count_all_matches() {
        out.literal("Matches seen: ")?; out.literal(&report.matches_seen().to_string())?;
        out.literal(if report.is_complete() { " (exact)" } else { " (lower bound; saved scope incomplete)" })?;
        out.literal("; retained hits: ")?; out.literal(&report.hits().len().to_string())?;
        out.literal("; matching files: ")?; out.literal(&report.matching_files().to_string())?; out.literal(".\n")?;
    }
    Ok(())
}

/// Render hits in engine order, retaining at most one independently verified
/// source member. Multiple hits in one file share this load. Source offsets are
/// never treated as UTF-8 indexes; decoding starts at the original offset so an
/// interior byte-order mark remains source text.
pub(super) fn hits<R: Read + Seek>(archive: &mut PagedSnapshot<R>, report: &PagedReport,
    raw: bool, json: bool, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<(), Failure> {
    report.validate_delivery(archive.directory().digest(), generation())?;
    let before = archive.load_stats();
    let mut current: Option<(usize, VerifiedMember)> = None;
    if json { out.literal(",\"hits\":[")?; }
    for (position, hit) in report.hits().iter().enumerate() {
        if canceled() { return Err(Failure::canceled()); }
        let member = archive.directory().member(hit.ordinal()).ok_or_else(|| Failure::new("SNAPSHOT_MEMBER_MISSING"))?;
        let path = RawPath::from_bytes(member.path).to_path_buf();
        if !json {
            out.path(&path)?; out.literal(" bytes ")?; out.literal(&hit.original_range().start().get().to_string())?;
            out.literal("..")?; out.literal(&hit.original_range().end().get().to_string())?; out.literal("\n")?;
            continue;
        }
        if current.as_ref().is_none_or(|(ordinal, _)| *ordinal != hit.ordinal()) {
            drop(current.take());
            current = Some((hit.ordinal(), archive.load(hit.ordinal(), budget, allocation(210), &mut *canceled)?));
        }
        let source = &current.as_ref().ok_or_else(|| Failure::new("SNAPSHOT_INVALID_HIT"))?.1;
        if source.archive_digest() != hit.archive_digest() || source.source_digest() != hit.source_digest() {
            return Err(PagedSearchError::InvalidHit.into());
        }
        let (start, end) = hit.original_range().as_usize_bounds().map_err(|_| Failure::new("SNAPSHOT_INVALID_HIT"))?;
        let original = source.bytes().get(start..end).ok_or_else(|| Failure::new("SNAPSHOT_INVALID_HIT"))?;
        if original.is_empty() || original.len() > MAX_WITNESS_BYTES { return Err(PagedSearchError::InvalidHit.into()); }
        if position > 0 { out.literal(",")?; }
        out.literal("{\"hit_id\":")?; out.integer(position as u64 + 1)?;
        out.literal(",\"file_id\":")?; out.integer(hit.file().get())?;
        out.literal(",\"revision\":")?; out.integer(hit.revision().get())?;
        out.literal(",\"path\":")?; out.path(&path)?;
        out.literal(",\"source_digest\":")?; out.quoted(&hit.source_digest().to_hex())?;
        out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
        out.literal(",\"matched_text\":")?;
        if raw { out.literal("null")?; } else {
            let lease = budget.try_reserve_managed(owner(), allocation(211),
                ByteLength::new((original.len().max(4) * 256 + 4096) as u64))
                .map_err(|_| Failure::new("SNAPSHOT_RESOURCE_DENIED"))?;
            let text = CaptureEncodingMap::build_with_base_offset(original,
                detect_encoding(source.bytes()), hit.original_range().start().get(), 0, 0, 0)
                .map_err(|_| Failure::new("SNAPSHOT_INVALID_HIT"))?;
            if text.decoded_text().is_empty() || text.decoded_text().len() > MAX_WITNESS_BYTES
                || text.spans().iter().any(|span| matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
                return Err(PagedSearchError::InvalidHit.into());
            }
            if canceled() { return Err(Failure::canceled()); }
            out.quoted(text.decoded_text())?;
            drop(text); drop(lease);
        }
        out.literal(",\"matched_raw_hex\":")?;
        if raw { out.hex(original)?; } else { out.literal("null")?; }
        out.literal(",\"original_hex\":")?; out.hex(original)?; out.literal("}")?;
    }
    if json {
        out.literal("],\"match_witness_payload_bytes_loaded\":")?;
        out.integer(archive.load_stats().bytes_read - before.bytes_read)?;
        out.literal(",\"match_witness_members_loaded\":")?;
        out.integer(archive.load_stats().loaded_members - before.loaded_members)?;
        out.literal(",\"match_witness_read_calls\":")?;
        out.integer(archive.load_stats().read_calls - before.read_calls)?;
    }
    Ok(())
}
// An explicit count-all request succeeds when the entire saved scope was
// searched, even if its optional retained rows are capped or zero. Ordinary
// bounded queries stop scanning at their cap and remain incomplete (exit 3).
pub(super) fn exit(report: &PagedReport) -> u8 {
    if !report.is_complete() { EXIT_PARTIAL } else if report.matches_seen() == 0 { EXIT_NO_MATCH } else { EXIT_OK }
}

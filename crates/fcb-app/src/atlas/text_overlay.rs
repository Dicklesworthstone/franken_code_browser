#![forbid(unsafe_code)]

//! CLI adapters for the public captured-text atlas service. Native reads use
//! the same admitted capture route as workspace search. Geometry is untouched.
use fcb::map::workspace::text_search::{AtlasTextLimits, AtlasTextOverlay, WorkspaceTextSource};
use fcb::search::{IndexLimits, SearchCoverage};
use fcb::search::workspace::WorkspaceCaptures;
use crate::workspace::IoCounts;
use super::*;

pub(super) fn capture<'catalog>(catalog: &'catalog WorkspaceCatalog, root: &Path,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
    -> Result<(WorkspaceCaptures<'catalog>, IoCounts), Error> {
    let mut captures = WorkspaceCaptures::new(catalog, crate::revision(), budget, allocation(67))?;
    let mut io = IoCounts::default();
    let cancel = CancelFlag::new();
    while !captures.finished() {
        catalog.validate_active()?;
        if canceled() { return Err(AppError::Canceled.into()); }
        let mut stop = || canceled() || catalog.validate_active().is_err();
        captures.step(&cancel, |request, path, allowance| {
            workspace::read_capture(root, request, path, allowance,
                catalog.limits().max_source_bytes as u64, &mut io, budget, &mut stop)
        })?;
    }
    catalog.validate_active()?;
    if canceled() { return Err(AppError::Canceled.into()); }
    Ok((captures, io))
}

pub(super) fn search<'index, 'source, 'catalog>(source: &'index WorkspaceTextSource<'source, 'catalog>,
    needle: &str, options: &Options, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
    -> Result<AtlasTextOverlay<'index, 'source, 'catalog>, Error> {
    let index = source.index(IndexLimits::default(), budget, allocation(69), &mut *canceled)?;
    Ok(index.search(needle, generation(), AtlasTextLimits { max_matches: options.match_limit,
        max_bytes_scanned: options.text_scan_bytes }, budget,
        [allocation(70), allocation(71), allocation(72)], &mut *canceled)?)
}

pub(super) fn counts(out: &mut Output, overlay: Option<&AtlasTextOverlay<'_, '_, '_>>,
    parcel: fcb::map::VisibleParcel) -> Result<(), Error> {
    out.literal(",\"retained_text_matches\":")?;
    let Some(overlay) = overlay.filter(|_| parcel.detail() != AtlasDetail::SiblingGroup) else {
        out.literal("null")?; return Ok(());
    };
    let counts = overlay.retained_matches_in(parcel.node())?;
    out.literal("{\"occurrences\":")?; out.integer(counts.occurrences as u64)?;
    out.literal(",\"files\":")?; out.integer(counts.files as u64)?; out.literal("}")?;
    Ok(())
}

pub(super) fn json(out: &mut Output, overlay: Option<&AtlasTextOverlay<'_, '_, '_>>,
    io: &IoCounts, index: &AtlasIndex<'_>, focus: AtlasNodeId, camera: Camera2D,
    options: &Options, canceled: &mut impl FnMut() -> bool) -> Result<(), Error> {
    out.literal(",\"text_search\":")?;
    let Some(overlay) = overlay else { out.literal("null")?; return Ok(()); };
    overlay.validate_delivery(overlay.source(), generation())?;
    let report = overlay.report(); let result = report.capture_results();
    let source = overlay.source(); let atlas = source.atlas();
    let catalog = atlas.catalog();
    out.literal("{\"mode\":\"exact-decoded-text-literal\",\"scope\":\"catalogued-workspace-captures-not-camera-focus\",\"generation\":")?;
    out.integer(overlay.generation().get())?;
    out.literal(",\"workspace_complete\":")?; out.boolean(overlay.is_complete())?;
    out.literal(",\"counts_complete\":")?; out.boolean(overlay.is_complete())?;
    out.literal(",\"truncated\":")?; out.boolean(matches!(result.coverage, SearchCoverage::TruncatedAtLimit { .. }))?;
    out.literal(",\"byte_limited\":")?; out.boolean(matches!(result.coverage, SearchCoverage::BudgetExhausted { .. }))?;
    out.literal(",\"coverage\":")?; out.quoted(match result.coverage {
        SearchCoverage::Exhaustive => "exhaustive-captured-scan",
        SearchCoverage::TruncatedAtLimit { .. } => "match-limit",
        SearchCoverage::BudgetExhausted { .. } => "verification-byte-limit",
        SearchCoverage::CanceledEarly => "canceled",
    })?;
    out.literal(",\"known_files\":")?; out.integer(report.known_files() as u64)?;
    out.literal(",\"examined_captured_files\":")?; out.integer(report.examined_files() as u64)?;
    out.literal(",\"unexamined_captured_files\":")?;
    out.integer(report.known_files().saturating_sub(report.unavailable_files().len()).saturating_sub(report.examined_files()) as u64)?;
    out.literal(",\"payload_bytes_read\":")?; out.integer(io.bytes)?;
    out.literal(",\"read_calls\":")?; out.integer(io.calls)?;
    out.literal(",\"captured_bytes\":")?; out.integer(source.captures().captured_bytes() as u64)?;
    out.literal(",\"verification_bytes\":")?; out.integer(result.scanned_bytes)?;
    out.literal(",\"max_verification_bytes\":")?; out.integer(options.text_scan_bytes)?;
    out.literal(",\"max_matches\":")?; out.integer(options.match_limit as u64)?;
    out.literal(",\"index_skipped_files\":")?; out.integer(report.skipped_by_index() as u64)?;
    out.literal(",\"fallback_scans\":")?; out.integer(report.fallback_attempts() as u64)?;
    out.literal(",\"matches_counted\":")?; out.integer(result.total_matches_counted as u64)?;
    out.literal(",\"retained_matches\":")?; out.integer(overlay.hits().len() as u64)?;
    out.literal(",\"counts_describe\":\"retained-occurrences-and-distinct-files\",\"hits\":[")?;
    for (i, hit) in overlay.hits().iter().enumerate() {
        if canceled() { return Err(AppError::Canceled.into()); }
        let selection = overlay.select_hit(source, i, generation())?;
        if i > 0 { out.literal(",")?; }
        out.literal("{\"hit_index\":")?; out.integer(i as u64)?;
        out.literal(",\"node\":")?; node_record(out, index, hit.node())?;
        out.literal(",\"file_id\":")?; out.integer(hit.file().get())?;
        out.literal(",\"revision\":")?; out.integer(hit.revision().get())?;
        out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
        out.literal(",\"matched_text\":")?; out.quoted(selection.matched_text())?;
        out.literal(",\"original_hex\":")?; out.hex(selection.original_bytes())?;
        out.literal(",\"logical_rect\":")?;
        if let Some(rect) = projected_selection(index, hit.node(), focus, camera)? { rectangle(out, rect)?; }
        else { out.literal("null")?; }
        out.literal("}")?;
    }
    out.literal("],\"unavailable_files\":[")?;
    for (i, &file) in report.unavailable_files().iter().enumerate() {
        if canceled() { return Err(AppError::Canceled.into()); }
        if i > 0 { out.literal(",")?; }
        out.literal("{\"file_id\":")?; out.integer(file.get())?;
        out.literal(",\"path\":")?;
        out.path(&catalog.entry(file).ok_or(AppError::InvalidRange)?.path().raw().to_path_buf())?;
        out.literal(",\"reason\":")?;
        out.quoted(source.captures().file_failure(file).ok_or(AppError::InvalidRange)?.code())?;
        out.literal("}")?;
    }
    out.literal("],\"unsupported_text_files\":[")?;
    for (i, &file) in result.unsupported_files.iter().enumerate() {
        if canceled() { return Err(AppError::Canceled.into()); }
        if i > 0 { out.literal(",")?; }
        out.literal("{\"file_id\":")?; out.integer(file.get())?;
        out.literal(",\"path\":")?;
        out.path(&catalog.entry(file).ok_or(AppError::InvalidRange)?.path().raw().to_path_buf())?;
        out.literal(",\"reason\":\"UNSUPPORTED_EXACT_TEXT_DECODING\"}")?;
    }
    out.literal("]}")?;
    Ok(())
}

pub(super) fn human(out: &mut Output, overlay: Option<&AtlasTextOverlay<'_, '_, '_>>,
    canceled: &mut impl FnMut() -> bool) -> Result<(), Error> {
    let Some(overlay) = overlay else { return Ok(()); };
    overlay.validate_delivery(overlay.source(), generation())?;
    out.literal("Exact captured-text matches (not presented hit regions):\n")?;
    for (i, hit) in overlay.hits().iter().enumerate() {
        if canceled() { return Err(AppError::Canceled.into()); }
        out.path(&overlay.source().atlas().entry(hit.node())?.path().raw().to_path_buf())?;
        out.literal(" bytes ")?; out.literal(&hit.original_range().start().get().to_string())?;
        out.literal("..")?; out.literal(&hit.original_range().end().get().to_string())?;
        out.literal(": ")?;
        out.human_text(overlay.select_hit(overlay.source(), i, generation())?.matched_text())?; out.literal("\n")?;
    }
    if !overlay.is_complete() { out.literal("PARTIAL text search: inspect capture, decoding and query coverage\n")?; }
    Ok(())
}

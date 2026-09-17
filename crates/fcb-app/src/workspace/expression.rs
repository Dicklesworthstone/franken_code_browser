#![forbid(unsafe_code)]

//! Workspace expression composition, reusing the existing host reader, capture
//! admission, metadata predicates, substring index, and exact query verifier.
//! Files excluded by path/type filters spend no source-capture byte allowance.

use super::*;
use fcb::search::{EphemeralIndex, ManifestLimits, ReferenceScanOracle, SearchDocument, SearchManifest};

fn admits(query: &ParsedQuery, path: &NormalizedPath) -> bool {
    let test = |key: &str| ReferenceScanOracle::matches_path_filters(key, &query.path_filters)
        && ReferenceScanOracle::matches_lang_filters(key, &query.lang_filters);
    match path.as_str() { Ok(key) => test(key), Err(_) => test(&path.display_escaped().to_string()) }
}

pub(super) fn execute(args: &Arguments, query: &ParsedQuery, catalog: &WorkspaceCatalog,
    root: &Path, out: &mut Output, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
    -> Result<u8, AppError> {
    let count = catalog.entries().len();
    let charge = count.checked_mul(std::mem::size_of::<SearchDocument<'_>>() + std::mem::size_of::<fcb::FileId>())
        .and_then(|n| n.checked_add(512 * 1024)).ok_or(AppError::Admission)?;
    let _scratch = budget.try_reserve_managed(owner(), allocation(80), ByteLength::new(charge as u64))
        .map_err(|_| AppError::Admission)?;
    let cancel = CancelFlag::new();
    let mut captures = WorkspaceCaptures::new(catalog, revision(), budget, allocation(34))?;
    let mut io = IoCounts::default();
    while !captures.finished() {
        if canceled() { return Err(AppError::Canceled); }
        captures.step(&cancel, |request, path, limit| {
            // This private capture table may contain deliberately uncaptured
            // out-of-scope entries. They are REMOVED from the query manifest
            // below, never counted as missing selected sources or fake captures.
            if !admits(query, path) { return Err(SourceError::CaptureUnavailable); }
            read_capture(root, request, path, limit, args.max_total_bytes as u64, &mut io, budget, canceled)
        })?;
    }
    let inputs = captures.search_inputs(budget, allocation(35))?;
    let original = inputs.manifest()?;
    let mut documents = Vec::new(); let mut unavailable = Vec::new();
    documents.try_reserve_exact(original.documents().len()).map_err(|_| AppError::Admission)?;
    unavailable.try_reserve_exact(original.unavailable().len()).map_err(|_| AppError::Admission)?;
    if documents.capacity() > original.documents().len() || unavailable.capacity() > original.unavailable().len() {
        return Err(AppError::Admission);
    }
    for document in original.documents() {
        if canceled() { return Err(AppError::Canceled); }
        if ReferenceScanOracle::matches_path_filters(document.path, &query.path_filters)
            && ReferenceScanOracle::matches_lang_filters(document.path, &query.lang_filters) {
            documents.push(document.clone());
        }
    }
    for &file in original.unavailable() {
        if canceled() { return Err(AppError::Canceled); }
        let entry = catalog.entry(file).ok_or(AppError::InvalidRange)?;
        if admits(query, entry.path()) { unavailable.push(file); }
    }
    let scope_files = documents.len() + unavailable.len();
    let manifest = SearchManifest::new(original.id(), &documents, &unavailable, original.membership(),
        ManifestLimits { max_files: count, max_path_bytes: 16_384 })?;
    let index = EphemeralIndex::build(manifest, IndexLimits::default(), budget, allocation(36), &mut *canceled)?;
    let mut options = QueryOptions::new(generation()).with_max_matches(args.limit.max(1));
    options.max_bytes_scanned = Some(args.max_scan_bytes);
    let report = index.search(query, options, budget, allocation(37), &mut *canceled)?;
    if canceled() { return Err(AppError::Canceled); }
    catalog.validate_active()?;
    let result = report.capture_results();
    // The compatibility scanner's zero-cap mode does no scan. Perform one-hit
    // lookahead instead, so limit=0 can still distinguish a complete negative.
    let zero_overflow = args.limit == 0 && result.total_matches_counted > 0;
    let truncated = zero_overflow || matches!(result.coverage, SearchCoverage::TruncatedAtLimit { .. });
    let complete = report.is_complete() && !truncated;
    let matches_seen = if args.limit == 0 { result.total_matches_counted.min(1) } else { result.total_matches_counted };
    if args.json {
        common(out, "search", catalog, root)?;
        out.literal(",\"mode\":\"decoded-text-expression\",\"expression\":")?; out.quoted(&query.raw_query)?;
        out.literal(",\"predicate_scope\":\"same-complete-capture\",\"path_filter_domain\":\"utf8-or-escaped-search-key\",\"scope_files\":")?;
        out.integer(scope_files as u64)?;
        out.literal(",\"metadata_excluded_files\":")?; out.integer((count - scope_files) as u64)?;
        out.literal(",\"workspace_complete\":")?; out.boolean(complete)?;
        out.literal(",\"truncated\":")?; out.boolean(truncated)?;
        out.literal(",\"work_limited\":")?; out.boolean(matches!(result.coverage, SearchCoverage::BudgetExhausted { .. }))?;
        out.literal(",\"max_scan_bytes\":")?; out.integer(args.max_scan_bytes)?;
        out.literal(",\"scanned_input_bytes\":")?; out.integer(result.scanned_bytes)?;
        out.literal(",\"payload_bytes_read\":")?; out.integer(io.bytes)?;
        out.literal(",\"read_calls\":")?; out.integer(io.calls)?;
        out.literal(",\"captured_bytes\":")?; out.integer(captures.captured_bytes() as u64)?;
        out.literal(",\"index_skipped_files\":")?; out.integer(report.skipped_by_index() as u64)?;
        out.literal(",\"fallback_scans\":")?; out.integer(report.fallback_attempts() as u64)?;
        out.literal(",\"matches_seen\":")?; out.integer(matches_seen as u64)?;
        out.literal(",\"hits\":[")?;
        for (i, hit) in result.matches.iter().take(args.limit).enumerate() {
            if canceled() { return Err(AppError::Canceled); }
            if i > 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(hit.file_id.get())?;
            out.literal(",\"revision\":")?; out.integer(hit.revision.get())?;
            out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id)?;
            out.literal(",\"path\":")?; out.path(&catalog.entry(hit.file_id).ok_or(AppError::InvalidRange)?.path().raw().to_path_buf())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_byte_range)?;
            out.literal(",\"matched_text\":")?; out.quoted(&hit.matched_text)?; out.literal("}")?;
        }
        out.literal("],\"unavailable_files\":[")?;
        for (i, file) in report.unavailable_files().iter().enumerate() {
            if canceled() { return Err(AppError::Canceled); }
            if i > 0 { out.literal(",")?; }
            file_record(out, catalog, *file)?; out.literal(",\"reason\":")?;
            out.quoted(captures.file_failure(*file).ok_or(AppError::InvalidRange)?.code())?; out.literal("}")?;
        }
        out.literal("],\"unsupported_text_files\":[")?;
        for (i, file) in result.unsupported_files.iter().enumerate() {
            if canceled() { return Err(AppError::Canceled); }
            if i > 0 { out.literal(",")?; }
            file_record(out, catalog, *file)?;
            out.literal(",\"reason\":\"UNSUPPORTED_EXACT_TEXT_DECODING\"}")?;
        }
        out.literal("]}\n")?;
    } else {
        out.literal(if complete { "Complete expression search over selected workspace files\n" }
            else { "PARTIAL expression search: inspect scope, source, work and result limits\n" })?;
        out.literal("Query: ")?; out.human_text(&query.raw_query)?; out.literal("\n")?;
        for hit in result.matches.iter().take(args.limit) {
            if canceled() { return Err(AppError::Canceled); }
            out.path(&catalog.entry(hit.file_id).ok_or(AppError::InvalidRange)?.path().raw().to_path_buf())?;
            out.literal(" bytes ")?; out.literal(&hit.original_byte_range.start().get().to_string())?;
            out.literal("..")?; out.literal(&hit.original_byte_range.end().get().to_string())?; out.literal("\n")?;
        }
        out.literal("Matching work bytes (including predicates): ")?; out.literal(&result.scanned_bytes.to_string())?;
        out.literal("; source payload bytes read: ")?; out.literal(&io.bytes.to_string())?; out.literal("\n")?;
    }
    catalog.validate_active()?;
    Ok(if !complete { EXIT_PARTIAL } else if matches_seen == 0 { EXIT_NO_MATCH } else { EXIT_OK })
}

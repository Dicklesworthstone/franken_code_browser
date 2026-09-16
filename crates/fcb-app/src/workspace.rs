#![forbid(unsafe_code)]

//! Explicit --workspace composition. Enumeration/capture/query policies live in
//! the public facade; this module supplies CLI-authorized filesystem handles and
//! the bounded wire response. No store, shell, watcher or background runtime.

use std::{fs, path::{Path, PathBuf}, sync::Arc};
use fcb::{ByteLength, ByteOffset, ByteRange};
use fcb::source::{CancelFlag, SourceError};
use fcb::source::path::NormalizedPath;
use fcb::search::{CaptureRequest, CompleteCapture, ExtentConsistency, ExtentReadState,
    ExtentStepBudget, FileRangeReader, IndexLimits, ParsedQuery, PathEntry, PathIndex,
    PathIndexLimits, PathSearch, PathSearchOptions, RawPath, ResourceBudget, RootId,
    QueryOptions, SearchCoverage, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceCaptures,
    WorkspaceLimits, WorkspaceStage};
use crate::{AppError, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, SCHEMA, allocation,
    file_id, generation, owner, revision};
use crate::args::{Arguments, Command, Needle};
use crate::input;
use crate::output::Output;

const MAX_READ_CALLS: u64 = 131_072;

pub(crate) fn execute(args: &Arguments, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform); }
    let requested = input::absolute(args.file.as_deref().ok_or(AppError::InvalidRange)?)?;
    let meta = fs::symlink_metadata(&requested).map_err(|_| AppError::Io)?;
    if meta.file_type().is_symlink() { return Err(AppError::Symlink); }
    if !meta.is_dir() { return Err(AppError::InvalidRange); }
    let root = fs::canonicalize(&requested).map_err(|_| AppError::Io)?;
    let root_id = RootId::new(owner(), 1).map_err(|_| AppError::InvalidRange)?;
    let grant = RootGrant::new(root_id, RawPath::from_path(&root));
    let limits = WorkspaceLimits { max_files: args.max_files, max_file_bytes: args.max_file_bytes,
        max_source_bytes: args.max_total_bytes, ..WorkspaceLimits::default() };
    let id = SearchManifestId::new(owner(), 1)?;
    let mut catalog = WorkspaceCatalog::open(grant, id, file_id(), limits,
        args.include_excluded, budget, allocation(30))?;
    let cancel = CancelFlag::new();
    while catalog.stage() == WorkspaceStage::Discovering {
        if canceled() { return Err(AppError::Canceled); }
        catalog.step(&cancel)?;
    }
    if canceled() { return Err(AppError::Canceled); }
    if args.command == Command::Inspect { return inspect(args, &catalog, &root, out, canceled); }
    match args.needle.as_ref() {
        Some(Needle::Path(needle)) => paths(args, &catalog, &root, needle, out, budget, canceled),
        Some(Needle::Text(needle)) => text(args, &catalog, &root, needle, out, budget, canceled),
        _ => Err(AppError::InvalidRange),
    }
}

fn common(out: &mut Output, command: &str, catalog: &WorkspaceCatalog, root: &Path) -> Result<(), AppError> {
    out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
    out.literal(",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
    out.literal(",\"scope\":\"workspace\",\"identity_scope\":\"response-local\",\"owner\":\"1\",\"root_id\":\"1\",\"manifest\":\"1\",\"query_generation\":\"1\",\"root\":")?;
    out.path(root)?;
    out.literal(",\"policy\":")?; out.quoted(catalog.policy_name())?;
    out.literal(",\"snapshot_guarantee\":\"per-file-observations-not-an-atomic-repository\",\"confinement\":\"path-checked-not-race-safe\",\"discovery_complete\":")?;
    out.boolean(catalog.discovery_complete())?;
    out.literal(",\"known_files\":")?; out.integer(catalog.entries().len() as u64)?;
    out.literal(",\"discovery_pages\":")?; out.integer(catalog.discovery_pages() as u64)?;
    out.literal(",\"discovery_limit\":")?;
    match catalog.stopped_by_limit() { Some(limit) => out.quoted(&format!("{limit:?}"))?, None => out.literal("null")? }
    let a = catalog.aggregate();
    out.literal(",\"discovery\":{")?;
    for (i, (name, count)) in [("files", a.files), ("directories", a.directories),
        ("excluded_entries", a.excluded), ("symlinks_not_followed", a.symlinks),
        ("special_objects", a.special), ("unavailable", a.unavailable),
        ("path_limited", a.path_limited), ("depth_limited", a.depth_limited),
        ("queue_refused", a.queue_refused)].into_iter().enumerate() {
        if i > 0 { out.literal(",")?; }
        out.quoted(name)?; out.literal(":")?; out.integer(count)?;
    }
    out.literal("}")?;
    Ok(())
}

fn inspect(args: &Arguments, catalog: &WorkspaceCatalog, root: &Path, out: &mut Output,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    let truncated = catalog.entries().len() > args.limit;
    if args.json {
        common(out, "inspect", catalog, root)?;
        out.literal(",\"payload_bytes_read\":\"0\",\"listing_truncated\":")?; out.boolean(truncated)?;
        out.literal(",\"files\":[")?;
        for (ordinal, entry) in catalog.entries().iter().take(args.limit).enumerate() {
            if canceled() { return Err(AppError::Canceled); }
            if ordinal > 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(catalog.file_id(ordinal).ok_or(AppError::InvalidRange)?.get())?;
            out.literal(",\"path\":")?; out.path(&entry.path().raw().to_path_buf())?;
            out.literal(",\"observed_bytes\":")?; out.integer(entry.observed_bytes())?; out.literal("}")?;
        }
        out.literal("]}\n")?;
    } else {
        out.literal("Workspace metadata; policy: ")?; out.literal(catalog.policy_name())?; out.literal("\n")?;
        for entry in catalog.entries().iter().take(args.limit) {
            if canceled() { return Err(AppError::Canceled); }
            out.path(&entry.path().raw().to_path_buf())?; out.literal("\n")?;
        }
        out.literal("Source payload bytes read: 0\n")?;
        if !catalog.discovery_complete() || truncated { out.literal("PARTIAL metadata or bounded listing\n")?; }
    }
    catalog.validate_active()?;
    Ok(if catalog.discovery_complete() && !truncated { EXIT_OK } else { EXIT_PARTIAL })
}

fn paths(args: &Arguments, catalog: &WorkspaceCatalog, root: &Path, needle: &str,
    out: &mut Output, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    let count = catalog.entries().len();
    let _scratch = budget.try_reserve_managed(owner(), allocation(31),
        ByteLength::new((count * std::mem::size_of::<PathEntry<'_>>()) as u64))
        .map_err(|_| AppError::Admission)?;
    let mut entries = Vec::new();
    entries.try_reserve_exact(count).map_err(|_| AppError::Admission)?;
    if entries.capacity() > count { return Err(AppError::Admission); }
    for (ordinal, entry) in catalog.entries().iter().enumerate() {
        entries.push(PathEntry::new(catalog.file_id(ordinal).ok_or(AppError::InvalidRange)?,
            catalog.grant().root_id(), entry.path().raw()));
    }
    let membership = if catalog.discovery_complete() { fcb::search::MembershipState::Closed }
        else { fcb::search::MembershipState::Discovering };
    let index = PathIndex::build(catalog.id(), membership, &entries, PathIndexLimits::default(),
        budget, allocation(32), &mut *canceled)?;
    let mut options = PathSearchOptions::new(generation()); options.max_results = args.limit;
    let mut query = PathSearch::new(&index, needle.as_bytes(), options, budget, allocation(33))?;
    query.run_to_completion(&mut *canceled)?;
    if canceled() { return Err(AppError::Canceled); }
    let complete = query.is_complete();
    if args.json {
        common(out, "search", catalog, root)?;
        out.literal(",\"mode\":\"native-path-fuzzy\",\"payload_bytes_read\":\"0\",\"workspace_complete\":")?; out.boolean(complete)?;
        out.literal(",\"truncated\":")?; out.boolean(query.truncated())?;
        out.literal(",\"matches_seen\":")?; out.integer(query.matches_seen() as u64)?;
        out.literal(",\"hits\":[")?;
        for (i, hit) in query.ranked_matches().iter().enumerate() {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(hit.file_id().get())?;
            out.literal(",\"path\":")?; out.path(&hit.path().raw_path().to_path_buf())?;
            out.literal(",\"rank_kind\":")?; out.quoted(&format!("{:?}", hit.rank().kind))?; out.literal("}")?;
        }
        out.literal("]}\n")?;
    } else {
        out.literal("Workspace path search (no source payload read)\n")?;
        for hit in query.ranked_matches() { out.path(&hit.path().raw_path().to_path_buf())?; out.literal("\n")?; }
        if !complete || query.truncated() { out.literal("PARTIAL membership or limited result rows\n")?; }
    }
    catalog.validate_active()?;
    Ok(if !complete || query.truncated() { EXIT_PARTIAL } else if query.matches_seen() == 0 { EXIT_NO_MATCH } else { EXIT_OK })
}

fn text(args: &Arguments, catalog: &WorkspaceCatalog, root: &Path, needle: &str,
    out: &mut Output, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    let cancel = CancelFlag::new();
    let mut captures = WorkspaceCaptures::new(catalog, revision(), budget, allocation(34))?;
    let mut io = IoCounts::default();
    while !captures.finished() {
        if canceled() { return Err(AppError::Canceled); }
        captures.step(&cancel, |request, path, limit| {
            read_capture(root, request, path, limit, args.max_total_bytes as u64, &mut io, budget, canceled)
        })?;
    }
    let inputs = captures.search_inputs(budget, allocation(35))?;
    let index = inputs.index(IndexLimits::default(), budget, allocation(36), &mut *canceled)?;
    // --text is a literal, including whitespace and strings like "path:foo".
    // The existing advanced AST remains available to library callers separately.
    let query = ParsedQuery { primary_needle: needle.to_owned(), is_phrase: true,
        conjunction_terms: Vec::new(), exclusion_terms: Vec::new(), path_filters: Vec::new(),
        lang_filters: Vec::new(), raw_query: needle.to_owned() };
    let options = QueryOptions::new(generation()).with_max_matches(args.limit);
    let report = index.search(&query, options, budget, allocation(37), &mut *canceled)?;
    if canceled() { return Err(AppError::Canceled); }
    let result = report.capture_results();
    let truncated = matches!(result.coverage, SearchCoverage::TruncatedAtLimit { .. });
    if args.json {
        common(out, "search", catalog, root)?;
        out.literal(",\"mode\":\"decoded-text-literal\",\"workspace_complete\":")?; out.boolean(report.is_complete())?;
        out.literal(",\"truncated\":")?; out.boolean(truncated)?;
        out.literal(",\"payload_bytes_read\":")?; out.integer(io.bytes)?;
        out.literal(",\"read_calls\":")?; out.integer(io.calls)?;
        out.literal(",\"captured_bytes\":")?; out.integer(captures.captured_bytes() as u64)?;
        out.literal(",\"index_skipped_files\":")?; out.integer(report.skipped_by_index() as u64)?;
        out.literal(",\"fallback_scans\":")?; out.integer(report.fallback_attempts() as u64)?;
        out.literal(",\"matches_seen\":")?; out.integer(result.total_matches_counted as u64)?;
        out.literal(",\"hits\":[")?;
        for (i, hit) in result.matches.iter().enumerate() {
            if canceled() { return Err(AppError::Canceled); }
            if i > 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(hit.file_id.get())?;
            out.literal(",\"revision\":")?; out.integer(hit.revision.get())?;
            out.literal(",\"path\":")?;
            out.path(&catalog.entry(hit.file_id).ok_or(AppError::InvalidRange)?.path().raw().to_path_buf())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_byte_range)?;
            out.literal(",\"matched_text\":")?; out.quoted(&hit.matched_text)?; out.literal("}")?;
        }
        out.literal("],\"unavailable_files\":[")?;
        for (i, file) in report.unavailable_files().iter().enumerate() {
            if i > 0 { out.literal(",")?; }
            out.integer(file.get())?;
        }
        out.literal("],\"unsupported_text_files\":[")?;
        for (i, file) in result.unsupported_files.iter().enumerate() {
            if i > 0 { out.literal(",")?; } out.integer(file.get())?;
        }
        out.literal("]}\n")?;
    } else {
        out.literal(if report.is_complete() { "Complete captured-workspace search\n" } else { "PARTIAL workspace search\n" })?;
        for hit in &result.matches {
            out.path(&catalog.entry(hit.file_id).ok_or(AppError::InvalidRange)?.path().raw().to_path_buf())?;
            out.literal(" bytes ")?; out.literal(&hit.original_byte_range.start().get().to_string())?;
            out.literal("..")?; out.literal(&hit.original_byte_range.end().get().to_string())?; out.literal("\n")?;
        }
        out.literal("Policy: ")?; out.literal(catalog.policy_name())?; out.literal("\n")?;
        out.literal("Unavailable captures: ")?; out.literal(&report.unavailable_files().len().to_string())?;
        out.literal("; unsupported text files: ")?; out.literal(&result.unsupported_files.len().to_string())?; out.literal("\n")?;
    }
    catalog.validate_active()?;
    Ok(if !report.is_complete() { EXIT_PARTIAL } else if result.matches.is_empty() { EXIT_NO_MATCH } else { EXIT_OK })
}

#[derive(Default)]
struct IoCounts { bytes: u64, calls: u64 }

fn read_capture(root: &Path, request: CaptureRequest, path: &NormalizedPath, limit: usize,
    total_limit: u64, io: &mut IoCounts, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
    -> Result<CompleteCapture, SourceError> {
    if canceled() { return Err(SourceError::Canceled); }
    let mut native: PathBuf = root.to_path_buf();
    // Recheck each observed component. The final open additionally uses the
    // existing no-follow/nonblocking flags; ancestor replacement is still an
    // explicit native-confinement limitation, not a sandbox claim.
    for segment in path.segments() {
        native.push(segment.to_path_buf());
        let metadata = fs::symlink_metadata(&native).map_err(|_| SourceError::CaptureUnavailable)?;
        if metadata.file_type().is_symlink() { return Err(SourceError::SymlinkForbidden); }
    }
    let (file, metadata) = input::open_regular(&native).map_err(|_| SourceError::CaptureUnavailable)?;
    if metadata.len() > limit as u64 || metadata.len() > total_limit.saturating_sub(io.bytes) {
        return Err(SourceError::PayloadTooLarge);
    }
    let mut reader = FileRangeReader::new(request.file(), file).map_err(|_| SourceError::CaptureUnavailable)?;
    let length = ByteLength::new(metadata.len());
    let mut read_request = request;
    if metadata.len() > 0 {
        let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(metadata.len())).map_err(|_| SourceError::InvalidRange)?;
        read_request = read_request.with_range(range)?;
    }
    let mut read = reader.begin(read_request, budget, allocation(38)).map_err(|_| SourceError::CaptureUnavailable)?;
    while read.state() == ExtentReadState::Pending {
        if canceled() { return Err(SourceError::Canceled); }
        if io.calls >= MAX_READ_CALLS { return Err(SourceError::CaptureUnavailable); }
        let before = read.stats();
        let status = read.step(ExtentStepBudget { max_bytes: 64 * 1024,
            max_calls: (MAX_READ_CALLS - io.calls).min(32) as usize }, &mut *canceled);
        let after = read.stats();
        io.bytes += after.bytes_read - before.bytes_read;
        io.calls += after.read_calls - before.read_calls;
        status.map_err(|_| if canceled() { SourceError::Canceled } else { SourceError::CaptureUnavailable })?;
    }
    let extent = read.finish(&mut *canceled).map_err(|_| if canceled() { SourceError::Canceled } else { SourceError::CaptureUnavailable })?;
    if !extent.request_filled() || !extent.covers_whole_observation()
        || extent.final_length() != Some(length) || extent.consistency() != ExtentConsistency::UnchangedMetadata {
        return Err(SourceError::MetadataMismatch);
    }
    CompleteCapture::new(request, length, Arc::from(extent.bytes()))
}

#![forbid(unsafe_code)]

//! CLI composition for continuous whole-file scans. No full-source capture,
//! persistent index, spool file or new matcher. The byte/call/result budgets are
//! GLOBAL for a workspace, including work spent before a source failure.

use std::{fs::File, path::Path};
use fcb::source::path::NormalizedPath;
use fcb::search::{CaptureRequest, DetectedEncoding, ExtentConsistency, FileSearch, FileSearchReport,
    ResourceBudget, StreamReadOptions, StreamReadState, StreamReadStep, StreamingNeedle};
use fcb::search::workspace::WorkspaceCatalog;
use crate::{AppError, SCHEMA, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, allocation, owner, generation, revision, file_id};
use crate::args::{Arguments, Encoding, Needle};
use crate::output::Output;
use crate::{input, workspace};

// A second explicit bound even when a user admits a very large source size.
const MAX_READ_CALLS: u64 = 16 * 1024 * 1024;

fn needle(args: &Arguments, budget: &ResourceBudget) -> Result<StreamingNeedle, AppError> {
    match args.needle.as_ref() {
        Some(Needle::Text(text)) => Ok(StreamingNeedle::text(owner(), text, budget, allocation(40))?),
        Some(Needle::Raw(bytes)) => Ok(StreamingNeedle::raw(owner(), bytes, budget, allocation(40))?),
        _ => Err(AppError::InvalidRange),
    }
}
fn options(args: &Arguments, bytes: u64, calls: u64, matches: usize) -> StreamReadOptions {
    StreamReadOptions { generation: generation(), max_matches: matches, max_bytes: bytes, max_read_calls: calls,
        encoding: match args.encoding {
            Encoding::Auto => None,
            Encoding::Utf8 => Some(DetectedEncoding::Utf8 { has_bom: true }),
            Encoding::Utf16Le => Some(DetectedEncoding::Utf16Le),
            Encoding::Utf16Be => Some(DetectedEncoding::Utf16Be),
        }}
}
fn scan<'needle>(file: File, request: CaptureRequest, needle: &'needle StreamingNeedle,
    options: StreamReadOptions, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
    -> Result<FileSearchReport<'needle>, AppError> {
    let mut scan = FileSearch::new(file, request, needle, options, budget, allocation(41))?;
    while scan.state() == StreamReadState::Pending {
        // Read/decoder failures are retained terminal report states. No retry
        // restarts at byte zero and silently double-counts a partial observation.
        let _ = scan.step(StreamReadStep::default(), generation(), &mut *canceled);
    }
    if canceled() || scan.state() == StreamReadState::Canceled { return Err(AppError::Canceled); }
    Ok(scan.finish()?)
}

pub(crate) fn single(args: &Arguments, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    let pattern = needle(args, budget)?;
    if canceled() { return Err(AppError::Canceled); }
    let path = input::absolute(args.file.as_deref().ok_or(AppError::InvalidRange)?)?;
    let (file, _) = input::open_regular(&path)?;
    let request = CaptureRequest::new(file_id(), revision()).map_err(|_| AppError::InvalidRange)?;
    let report = scan(file, request, &pattern, options(args, args.max_scan_bytes, MAX_READ_CALLS, args.limit), budget, canceled)?;
    if args.json {
        out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
        out.literal(",\"status\":\"ok\",\"command\":\"search\",\"scope\":\"named-file\",\"identity_scope\":\"response-local\",\"path\":")?;
        out.path(&path)?;
        out.literal(",\"strategy\":\"streaming-whole-file\",\"max_scan_bytes\":")?; out.integer(args.max_scan_bytes)?;
        out.literal(",\"max_read_calls\":")?; out.integer(MAX_READ_CALLS)?;
        fields(&report, out, canceled)?;
        out.literal("}\n")?;
    } else {
        out.literal("Continuous observed-file search: ")?; out.path(&path)?; out.literal("\n")?;
        human(&report, out)?;
    }
    Ok(exit(&report))
}

pub(crate) fn workspace(args: &Arguments, catalog: &WorkspaceCatalog, root: &Path,
    out: &mut Output, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    let pattern = needle(args, budget)?;
    let (mut bytes, mut calls, mut stored, mut seen, mut examined, mut failed) = (0u64, 0u64, 0usize, 0u64, 0usize, 0usize);
    let mut complete = catalog.discovery_complete();
    let mut truncated = false;
    let mut stopped = None;
    if args.json {
        workspace::common(out, "search", catalog, root)?;
        out.literal(",\"strategy\":\"streaming-whole-file\",\"capture_limits_applied\":false,\"max_scan_bytes\":")?;
        out.integer(args.max_scan_bytes)?;
        out.literal(",\"max_read_calls\":")?; out.integer(MAX_READ_CALLS)?;
        out.literal(",\"files\":[")?;
    } else { out.literal("Streaming workspace search; only literal witnesses are retained\n")?; }
    for (ordinal, entry) in catalog.entries().iter().enumerate() {
        if canceled() { return Err(AppError::Canceled); }
        catalog.validate_active()?;
        let file_id = catalog.file_id(ordinal).ok_or(AppError::InvalidRange)?;
        let request = CaptureRequest::new(file_id,
            fcb::SourceRevision::new(owner(), ordinal as u64 + 1).map_err(|_| AppError::InvalidRange)?)
            .map_err(|_| AppError::InvalidRange)?;
        let opened = open_workspace_source(root, entry.path());
        let result = match opened {
            Ok(file) => {
                let mut stop = || canceled() || catalog.validate_active().is_err();
                scan(file, request, &pattern, options(args, args.max_scan_bytes.saturating_sub(bytes),
                    MAX_READ_CALLS.saturating_sub(calls), args.limit.saturating_sub(stored)), budget, &mut stop)
            }
            Err(error) => Err(error),
        };
        catalog.validate_active()?;
        if canceled() { return Err(AppError::Canceled); }
        if args.json {
            if examined != 0 { out.literal(",")?; }
            out.literal("{\"path\":")?; out.path(&entry.path().raw().to_path_buf())?;
        } else { out.path(&entry.path().raw().to_path_buf())?; out.literal("\n")?; }
        examined += 1;
        match result {
            Ok(report) => {
                let search = report.search();
                bytes += search.stats().bytes_read; calls += search.stats().read_calls;
                stored += search.hits().len(); seen += search.matches_seen();
                complete &= report.is_complete();
                if !report.is_complete() { failed += 1; }
                if args.json { fields(&report, out, canceled)?; } else { human(&report, out)?; }
                match search.state() {
                    StreamReadState::Truncated => { truncated = true; stopped = Some("match-limit"); }
                    StreamReadState::ByteLimit => stopped = Some("byte-limit"),
                    StreamReadState::CallLimit => stopped = Some("read-call-limit"),
                    _ => {},
                }
            }
            Err(error) => {
                if error.is_canceled() { return Err(error); }
                complete = false; failed += 1;
                if args.json {
                    out.literal(",\"file_id\":")?; out.integer(file_id.get())?;
                    out.literal(",\"state\":\"unavailable\",\"whole_file_complete\":false,\"error_code\":")?;
                    out.quoted(&error.code())?; out.literal(",\"hits\":[]")?;
                } else { out.literal("Unavailable: ")?; out.literal(&error.code())?; out.literal("\n")?; }
            }
        }
        if args.json { out.literal("}")?; }
        if stopped.is_some() { break; }
    }
    complete &= examined == catalog.entries().len();
    catalog.validate_active()?;
    if args.json {
        out.literal("],\"workspace_complete\":")?; out.boolean(complete)?;
        out.literal(",\"truncated\":")?; out.boolean(truncated)?;
        out.literal(",\"files_examined\":")?; out.integer(examined as u64)?;
        out.literal(",\"unexamined_files\":")?; out.integer((catalog.entries().len() - examined) as u64)?;
        out.literal(",\"incomplete_files\":")?; out.integer(failed as u64)?;
        out.literal(",\"payload_bytes_read\":")?; out.integer(bytes)?;
        out.literal(",\"read_calls\":")?; out.integer(calls)?;
        out.literal(",\"matches_seen\":")?; out.integer(seen)?;
        out.literal(",\"stored_hits\":")?; out.integer(stored as u64)?;
        out.literal(",\"stop_reason\":")?;
        if let Some(reason) = stopped { out.quoted(reason)?; } else { out.literal("null")?; }
        out.literal("}\n")?;
    } else {
        out.literal(if complete { "Complete observed-workspace scan\n" } else { "PARTIAL observed-workspace scan\n" })?;
        out.literal("Files examined: ")?; out.literal(&examined.to_string())?;
        out.literal("; source bytes read: ")?; out.literal(&bytes.to_string())?; out.literal("\n")?;
    }
    Ok(if !complete { EXIT_PARTIAL } else if seen == 0 { EXIT_NO_MATCH } else { EXIT_OK })
}

fn open_workspace_source(root: &Path, relative: &NormalizedPath) -> Result<File, AppError> {
    let path = workspace::checked_source_path(root, relative).map_err(|_| AppError::SourceChanged)?;
    Ok(input::open_regular(&path)?.0)
}
fn exit(report: &FileSearchReport<'_>) -> u8 {
    if !report.is_complete() { EXIT_PARTIAL } else if report.search().matches_seen() == 0 { EXIT_NO_MATCH } else { EXIT_OK }
}
fn fields(report: &FileSearchReport<'_>, out: &mut Output, canceled: &mut impl FnMut() -> bool) -> Result<(), AppError> {
    let search = report.search();
    out.literal(",\"file_id\":")?; out.integer(search.request().file().get())?;
    out.literal(",\"revision\":")?; out.integer(search.request().revision().get())?;
    out.literal(",\"query_generation\":")?; out.integer(search.generation().get())?;
    out.literal(",\"mode\":")?;
    out.quoted(if search.text_literal().is_some() { "exact-decoded-text" } else { "original-bytes" })?;
    out.literal(",\"encoding\":")?;
    out.quoted(match search.encoding() { Some(DetectedEncoding::Utf16Le) => "utf16le",
        Some(DetectedEncoding::Utf16Be) => "utf16be", Some(DetectedEncoding::Utf8 { .. }) => "utf8", _ => "not-selected" })?;
    out.literal(",\"state\":")?; out.quoted(search.state().code())?;
    out.literal(",\"input_complete\":")?; out.boolean(search.input_complete())?;
    out.literal(",\"whole_file_complete\":")?; out.boolean(report.is_complete())?;
    out.literal(",\"truncated\":")?; out.boolean(search.state() == StreamReadState::Truncated)?;
    out.literal(",\"observed_length\":")?; out.integer(search.observed_length().get())?;
    out.literal(",\"final_length\":")?;
    if let Some(length) = report.final_length() { out.integer(length.get())?; } else { out.literal("null")?; }
    out.literal(",\"payload_bytes_read\":")?; out.integer(search.stats().bytes_read)?;
    out.literal(",\"scanned_input_bytes\":")?; out.integer(search.stats().scanned_bytes)?;
    out.literal(",\"read_calls\":")?; out.integer(search.stats().read_calls)?;
    out.literal(",\"peak_input_buffer_bytes\":")?; out.integer(search.stats().peak_buffer_bytes as u64)?;
    out.literal(",\"matches_seen\":")?; out.integer(search.matches_seen())?;
    out.literal(",\"stored_hits\":")?; out.integer(search.hits().len() as u64)?;
    out.literal(",\"unsupported_at\":")?;
    if let Some(offset) = search.unsupported_at() { out.integer(offset.get())?; } else { out.literal("null")?; }
    out.literal(",\"error_code\":")?;
    if let StreamReadState::Failed(error) = search.state() { out.quoted(&error.to_string())?; } else { out.literal("null")?; }
    out.literal(",\"consistency\":")?;
    out.quoted(match report.consistency() { ExtentConsistency::UnchangedMetadata => "unchanged-metadata-not-atomic",
        ExtentConsistency::ChangedDuringRead => "changed-during-read", ExtentConsistency::ShortRead => "short-read",
        _ => "metadata-unavailable" })?;
    out.literal(",\"retention\":\"literal-witnesses-only\",\"literal_text\":")?;
    if let Some(text) = search.text_literal() { out.quoted(text)?; } else { out.literal("null")?; }
    out.literal(",\"literal_original_hex\":")?;
    if search.hits().is_empty() { out.literal("null")?; } else { out.hex(search.witness_bytes(0)?)?; }
    out.literal(",\"hits\":[")?;
    for (index, hit) in search.hits().iter().enumerate() {
        if canceled() { return Err(AppError::Canceled); }
        if index > 0 { out.literal(",")?; }
        out.literal("{\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
        out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
        out.literal(",\"decoded_range\":null}")?;
    }
    out.literal("]")?;
    Ok(())
}
fn human(report: &FileSearchReport<'_>, out: &mut Output) -> Result<(), AppError> {
    let search = report.search();
    for hit in search.hits() {
        out.literal("  bytes ")?; out.literal(&hit.original_range().start().get().to_string())?;
        out.literal("..")?; out.literal(&hit.original_range().end().get().to_string())?; out.literal("\n")?;
    }
    out.literal(if report.is_complete() { "  Complete observed sequence; " } else { "  PARTIAL: " })?;
    out.literal(search.state().code())?;
    out.literal("; matches seen ")?; out.literal(&search.matches_seen().to_string())?;
    out.literal("; bytes read ")?; out.literal(&search.stats().bytes_read.to_string())?; out.literal("\n")?;
    Ok(())
}

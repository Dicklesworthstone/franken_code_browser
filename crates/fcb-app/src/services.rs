#![forbid(unsafe_code)]

//! Thin CLI consumers of the public facade. No second decoder or matcher.
//! All source payloads are explicit user-selected output, not diagnostic logs.

use fcb::{BrowserSession, ByteRange};
use fcb::search::{DetectedEncoding, ExtentConsistency, ExtentQuery, ExtentQueryOptions,
    ExtentQueryState, ResourceBudget};
use crate::{AppError, SCHEMA, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, allocation, generation, owner};
use crate::args::{Arguments, Command, Encoding, Needle};
use crate::input::{self, Loaded};
use crate::output::Output;

const HELP: &str = "FrankenCodeBrowser — bounded headless source tools\n\n\
  fcb capabilities [--json]\n\
  fcb doctor [--json]                 Static capability report; no scans/repairs\n\
  fcb inspect FILE [--json]           Metadata only; no source payload read\n\
  fcb read FILE [--json]              Decode a bounded source window\n\
  fcb open FILE --json                Same headless reading operation\n\
  fcb search FILE --text TEXT [--json]\n\
  fcb search FILE --raw-hex HEX [--json]\n\
  fcb read --stdin [--json]           Bounded input; EOF required\n\
  fcb search --stdin --text TEXT [--json]\n\n\
Explicit workspace scope:\n\
  fcb inspect ROOT --workspace [--json]\n\
  fcb search ROOT --workspace --text TEXT [--json]\n\
  fcb search ROOT --workspace --path QUERY [--json]\n\
  --max-files N --max-file-bytes N --max-total-bytes N --include-excluded\n\
Workspace defaults: 4096 files, 1 MiB/file, 32 MiB captured source.\n\
Static product exclusions apply; nested .gitignore/.fcbignore are NOT loaded.\n\
Path search and inspection read no source payload. Files refused by capture\n\
quotas stay unavailable, not false no-match results. No atomic-repository claim.\n\n\
File-window options: --offset DECIMAL --bytes DECIMAL --limit DECIMAL\n\
                     --encoding auto|utf8|utf16le|utf16be\n\
--limit also bounds workspace result/listing rows; -- ends options.\n\
File defaults: offset 0, 65536 visible source bytes, 100 stored matches.\n\
Limits: 262144 visible bytes, 4096 stored matches, 8 MiB response.\n\
Far-offset text requires --encoding; --raw-hex searches original bytes.\n\
No directory is scanned without --workspace. Workspace mode does not accept\n\
stdin, window offsets, raw-byte queries or an encoding override.\n\
IDs are response-local. Decoded offsets are window-local UTF-8, not file bytes.\n\
Human source output escapes terminal controls; JSON retains logical text.\n\
Exit: 0 complete, 1 complete no-match, 2 error, 3 partial/truncated, 130 canceled.\n\
Bare fcb and human open reserve the native GUI route, currently unavailable.\n\
All new CLI routes are implemented-unqualified; native product gates remain open.\n";

pub(crate) fn help(json: bool, out: &mut Output) -> Result<u8, AppError> {
    if json { begin(out, "help")?; out.literal(",\"text\":")?; out.quoted(HELP)?; out.literal("}\n")?; }
    else { out.literal(HELP)?; }
    Ok(EXIT_OK)
}

pub(crate) fn capabilities(args: &Arguments, out: &mut Output) -> Result<u8, AppError> {
    let diagnostic = args.command == Command::Doctor;
    let command = if diagnostic { "doctor" } else { "capabilities" };
    let implemented = "implemented-unqualified";
    let file_state = if input::NATIVE_FILE_SUPPORTED { implemented } else { "unavailable" };
    let rows = [
        ("bounded-stdin-reading", implemented, "Explicit input only; EOF required within byte cap."),
        ("named-file-range-reading", file_state, "One regular file; no full-file capture prerequisite."),
        ("file-inspection", file_state, "Metadata only; recursive catalog requires explicit --workspace."),
        ("exact-window-text-search", implemented, "Existing decoded UTF-8/UTF-16 range search; scope explicit."),
        ("exact-window-byte-search", implemented, "Existing overlapping byte matcher; scope explicit."),
        ("lossless-json-output", implemented, "One bounded document; decimal-string integers and Unix path hex."),
        ("native-gui", "unavailable", "AppKit/Metal composition is not implemented in this binary."),
        ("workspace-cli-search", file_state, "Explicit bounded discovery, native-path lookup and indexed literal search over retained captures; static exclusions, no rule files."),
        ("persistent-cli-index", "unavailable", "No store is opened or implicitly created."),
        ("markdown-preview", "unavailable", "No integrated upstream document renderer in this lane."),
        ("regex-search", "unavailable", "No qualified regex engine selected."),
        ("trail-export", "unavailable", "No source packs or destinations are written."),
    ];
    if args.json {
        begin(out, command)?;
        out.literal(",\"qualification\":\"pending-independent-RCH\",\"native_ready\":false,\"mode\":")?;
        out.quoted(if diagnostic { "static-capabilities-only" } else { "headless-source-tools" })?;
        out.literal(",\"source_scanned\":false,\"features\":[")?;
        for (index, (name, state, reason)) in rows.iter().enumerate() {
            if index != 0 { out.literal(",")?; }
            out.literal("{\"name\":")?; out.quoted(name)?;
            out.literal(",\"state\":")?; out.quoted(state)?;
            out.literal(",\"reason\":")?; out.quoted(reason)?; out.literal("}")?;
        }
        out.literal("]}\n")?;
    } else {
        out.literal("FrankenCodeBrowser: headless source tools; independent verification pending.\n")?;
        if diagnostic { out.literal("Static diagnostic only: no source scans, benchmarks, database opens, or repairs.\n")?; }
        for (name, state, reason) in rows { out.literal(name)?; out.literal(": ")?;
            out.literal(state)?; out.literal(" — ")?; out.literal(reason)?; out.literal("\n")?; }
    }
    Ok(EXIT_OK)
}

pub(crate) fn inspect(args: &Arguments, out: &mut Output,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    let path = input::absolute(args.file.as_deref().ok_or(AppError::InvalidRange)?)?;
    let (_file, metadata) = input::open_regular(&path)?;
    if canceled() { return Err(AppError::Canceled); }
    if args.json {
        begin(out, "inspect")?; out.literal(",\"path\":")?; out.path(&path)?;
        out.literal(",\"selected_parent\":")?;
        if let Some(parent) = path.parent() { out.path(parent)?; } else { out.literal("null")?; }
        out.literal(",\"scope\":\"named-file-only\",\"regular_file\":true,\"observed_length\":")?;
        out.integer(metadata.len())?;
        out.literal(",\"payload_bytes_read\":\"0\",\"snapshot_guarantee\":\"metadata-observation-only\"}\n")?;
    } else {
        out.literal("Source ")?; out.path(&path)?;
        out.literal("\nObserved bytes: ")?; out.literal(&metadata.len().to_string())?;
        out.literal("\nRegular file; payload bytes read: 0. No directory scan or atomic snapshot claim.\n")?;
    }
    Ok(EXIT_OK)
}

pub(crate) fn source(args: &Arguments, loaded: &Loaded, out: &mut Output,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    let view = BrowserSession::new(owner()).open_extent(loaded.extent.clone())?;
    let options = ExtentQueryOptions { generation: generation(), max_matches: args.limit };
    if let Some(Needle::Raw(needle)) = args.needle.as_ref() {
        let mut query = ExtentQuery::raw(&view, loaded.visible, needle, options, budget, allocation(21))?;
        return search_result(args, loaded, &mut query, 0, "original-bytes", out, canceled);
    }
    let encoding = match args.encoding {
        Encoding::Auto => fcb::source::detect_encoding(loaded.extent.bytes()),
        Encoding::Utf8 => DetectedEncoding::Utf8 { has_bom: loaded.extent.range().start().get() == 0
            && loaded.extent.bytes().starts_with(&[0xef, 0xbb, 0xbf]) },
        Encoding::Utf16Le => DetectedEncoding::Utf16Le,
        Encoding::Utf16Be => DetectedEncoding::Utf16Be,
    };
    let header = if loaded.extent.range().start().get() == 0 {
        let bytes = loaded.extent.bytes();
        match encoding {
            DetectedEncoding::Utf8 { has_bom: true } if bytes.starts_with(&[0xef, 0xbb, 0xbf]) => 3,
            DetectedEncoding::Utf16Le if bytes.starts_with(&[0xff, 0xfe]) => 2,
            DetectedEncoding::Utf16Be if bytes.starts_with(&[0xfe, 0xff]) => 2,
            _ => 0,
        }
    } else { 0 };
    let text = view.decode(loaded.visible, encoding, generation(), budget, allocation(20), &mut *canceled)?;
    if let Some(Needle::Text(needle)) = args.needle.as_ref() {
        let mut query = ExtentQuery::text(&text, needle, options, budget, allocation(21))?;
        return search_result(args, loaded, &mut query, header, encoding_name(encoding), out, canceled);
    }
    let complete = whole_scope(loaded, text.range(), header);
    if args.json {
        begin(out, "read")?; describe_source(loaded, out)?;
        out.literal(",\"encoding\":")?; out.quoted(encoding_name(encoding))?;
        out.literal(",\"visible_range\":")?; out.range(text.range())?;
        out.literal(",\"whole_file_complete\":")?; out.boolean(complete)?;
        out.literal(",\"first_line\":")?;
        match text.first_line_number() { Some(line) => out.integer(line)?, None => out.literal("null")? }
        out.literal(",\"has_replacements\":")?; out.boolean(text.has_replacements())?;
        out.literal(",\"text\":")?; out.quoted(text.text())?;
        out.literal(",\"original_hex\":")?; out.hex(view.raw_selection(text.range())?)?;
        out.literal(",\"next_offset\":")?;
        match text.next_offset() { Some(offset) => out.integer(offset.get())?, None => out.literal("null")? }
        out.literal("}\n")?;
    } else {
        out.literal("[source bytes ")?; out.literal(&text.range().start().get().to_string())?;
        out.literal("..")?; out.literal(&text.range().end().get().to_string())?;
        out.literal(if complete { "; complete observed text; escaped controls]\n" }
            else { "; PARTIAL source window; escaped controls]\n" })?;
        out.human_text(text.text())?;
        if !text.text().ends_with('\n') { out.literal("\n")?; }
        if text.has_replacements() { out.literal("[malformed input displayed with replacements; original bytes preserved]\n")?; }
    }
    Ok(if complete { EXIT_OK } else { EXIT_PARTIAL })
}

fn search_result(args: &Arguments, loaded: &Loaded, query: &mut ExtentQuery<'_, '_>,
    header: u64, mode: &str, out: &mut Output, canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    while query.state() == ExtentQueryState::Pending { query.step(64 * 1024, generation(), &mut *canceled)?; }
    let scope_complete = query.scope_complete();
    let whole = scope_complete && whole_scope(loaded, query.scope(), header);
    let truncated = query.state() == ExtentQueryState::Truncated;
    if args.json {
        begin(out, "search")?; describe_source(loaded, out)?;
        out.literal(",\"mode\":")?; out.quoted(mode)?;
        out.literal(",\"scope\":")?; out.range(query.scope())?;
        out.literal(",\"scope_complete\":")?; out.boolean(scope_complete)?;
        out.literal(",\"whole_file_complete\":")?; out.boolean(whole)?;
        out.literal(",\"truncated\":")?; out.boolean(truncated)?;
        out.literal(",\"matches_seen\":")?; out.integer(query.matches_seen())?;
        out.literal(",\"stored_hits\":")?; out.integer(query.hits().len() as u64)?;
        out.literal(",\"scanned_input_bytes\":")?; out.integer(query.scanned_input_bytes())?;
        out.literal(",\"input_domain\":")?;
        out.quoted(if matches!(args.needle.as_ref(), Some(Needle::Raw(_))) { "original-bytes" } else { "window-utf8" })?;
        out.literal(",\"hits\":[")?;
        for (index, hit) in query.hits().iter().enumerate() {
            if canceled() { return Err(AppError::Canceled); }
            if index != 0 { out.literal(",")?; }
            out.literal("{\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
            out.literal(",\"window_utf8_range\":")?;
            if let Some(range) = hit.window_text_range() {
                out.literal("{\"start\":")?; out.integer(range.start().get())?;
                out.literal(",\"end\":")?; out.integer(range.end().get())?; out.literal("}")?;
            } else { out.literal("null")?; }
            out.literal("}")?;
        }
        out.literal("]}\n")?;
    } else {
        out.literal(if whole { "Complete observed-file search\n" } else { "PARTIAL search (unseen source or result limit)\n" })?;
        for hit in query.hits() {
            out.literal("bytes ")?; out.literal(&hit.original_range().start().get().to_string())?;
            out.literal("..")?; out.literal(&hit.original_range().end().get().to_string())?; out.literal("\n")?;
        }
        out.literal("Matches seen: ")?; out.literal(&query.matches_seen().to_string())?;
        out.literal(if truncated { " (lookahead count, not an exhaustive total)\n" } else { "\n" })?;
    }
    Ok(if !whole { EXIT_PARTIAL } else if query.matches_seen() == 0 { EXIT_NO_MATCH } else { EXIT_OK })
}

fn whole_scope(loaded: &Loaded, scope: ByteRange, header: u64) -> bool {
    loaded.extent.request_filled() && scope.start().get() == header
        && scope.end().get() == loaded.extent.observed_length().get()
        && matches!(loaded.extent.consistency(), ExtentConsistency::HostSupplied | ExtentConsistency::UnchangedMetadata)
}
fn encoding_name(encoding: DetectedEncoding) -> &'static str {
    match encoding {
        DetectedEncoding::Utf8 { .. } => "utf8", DetectedEncoding::Utf16Le => "utf16le",
        DetectedEncoding::Utf16Be => "utf16be", _ => "unsupported",
    }
}
fn begin(out: &mut Output, command: &str) -> Result<(), AppError> {
    out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
    out.literal(",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
    Ok(())
}
fn describe_source(loaded: &Loaded, out: &mut Output) -> Result<(), AppError> {
    out.literal(",\"identity_scope\":\"response-local\",\"owner\":\"1\",\"file_id\":\"1\",\"observation\":\"1\",\"query_generation\":\"1\",\"path\":")?;
    if let Some(path) = &loaded.path { out.path(path)?; } else { out.literal("null")?; }
    out.literal(",\"selected_parent\":")?;
    if let Some(parent) = loaded.path.as_deref().and_then(|p| p.parent()) { out.path(parent)?; } else { out.literal("null")?; }
    out.literal(",\"source_kind\":")?; out.quoted(if loaded.path.is_some() { "named-file" } else { "stdin" })?;
    out.literal(",\"observed_length\":")?; out.integer(loaded.extent.observed_length().get())?;
    out.literal(",\"captured_range\":")?; out.range(loaded.extent.range())?;
    out.literal(",\"requested_visible_range\":")?; out.range(loaded.visible)?;
    out.literal(",\"payload_bytes_read\":")?; out.integer(loaded.extent.bytes().len() as u64)?;
    out.literal(",\"read_calls\":")?; out.integer(loaded.read_calls)?;
    out.literal(",\"consistency\":")?;
    out.quoted(match loaded.extent.consistency() {
        ExtentConsistency::HostSupplied => "host-supplied-sequence", ExtentConsistency::UnchangedMetadata => "unchanged-metadata-not-atomic",
        ExtentConsistency::ChangedDuringRead => "changed-during-read", ExtentConsistency::ShortRead => "short-read",
        ExtentConsistency::MetadataUnavailable => "metadata-unavailable",
    })?;
    Ok(())
}

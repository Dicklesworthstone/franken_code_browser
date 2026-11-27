#![forbid(unsafe_code)]

//! Explicit CLI consumer of the native progressive catalog search service.
//! Discovery, scope predicates, matching, witnesses and coverage have one owner.
//! Only invocation framing, output backpressure and exit statuses live here.

use std::io::Write;
use fcb::search::ResourceBudget;
use crate::{AppError, EXIT_CANCELED, EXIT_ERROR, EXIT_NO_MATCH, EXIT_OK, EXIT_PARTIAL,
    allocation, owner};
use crate::args::{ArgumentError, Arguments, Needle};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};
use crate::host::atlas_search::AtlasSearchScope;
use crate::host::atlas_stream_search::{AtlasStreamSearchOptions, MAX_ATLAS_STREAM_SEARCH_READ_CALLS};
use crate::host::catalog_session::{CatalogContentSearchError, CatalogContentSearchMode,
    CatalogContentSearchOptions, CatalogContentSearchSession, CatalogDiscoveryPolicy, CatalogSearchProgress};
use super::ndjson::{self, Sink};

const SCHEMA: &str = "fcb.progressive-search/1";
const STREAM_SCHEMA: &str = "fcb.progressive-search-stream/1";
const PROGRESS_STEPS: u64 = 256;

#[derive(Clone, Copy)]
enum Error { App(AppError), Search(CatalogContentSearchError) }
impl From<AppError> for Error { fn from(error: AppError) -> Self { Self::App(error) } }
impl From<OutputError> for Error { fn from(error: OutputError) -> Self { Self::App(error.into()) } }
impl From<CatalogContentSearchError> for Error { fn from(error: CatalogContentSearchError) -> Self { Self::Search(error) } }
impl Error {
    fn canceled(self) -> bool { match self { Self::App(e) => e.is_canceled(), Self::Search(e) => e.is_canceled() } }
    fn output_limit(self) -> bool {
        matches!(self, Self::App(AppError::Output(OutputError::Limit))
            | Self::Search(CatalogContentSearchError::App(AppError::Output(OutputError::Limit))))
    }
    fn code(self) -> String { match self { Self::App(e) => e.code(), Self::Search(e) => e.to_string() } }
    fn app(self) -> Option<AppError> {
        match self { Self::App(error) | Self::Search(CatalogContentSearchError::App(error)) => Some(error), _ => None }
    }
}
type Failure = ndjson::Failure<Error>;
impl From<CatalogContentSearchError> for Failure {
    fn from(error: CatalogContentSearchError) -> Self { Self::Service(error.into()) }
}

pub(crate) fn run(parsed: Result<Arguments, ArgumentError>, json: bool, stream: bool,
    stdout: &mut impl Write, stderr: &mut impl Write, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> u8 {
    if stream { stream_response(parsed, stdout, stderr, budget, canceled) }
    else { document_response(parsed, json, stdout, stderr, budget, canceled) }
}

fn discovery_policy(args: &Arguments) -> CatalogDiscoveryPolicy {
    if args.include_excluded { CatalogDiscoveryPolicy::IncludeExcluded }
    else { CatalogDiscoveryPolicy::RepositoryRules }
}

fn open(args: &Arguments, canceled: &mut impl FnMut() -> bool)
    -> Result<CatalogContentSearchSession, CatalogContentSearchError> {
    let (mode, query) = match args.needle.as_ref() {
        Some(Needle::Text(text)) => (CatalogContentSearchMode::Text, text.as_str()),
        Some(Needle::Identifier(text)) => (CatalogContentSearchMode::Occurrences, text.as_str()),
        Some(Needle::Query(query)) => (CatalogContentSearchMode::Expression, query.raw_query.as_str()),
        _ => return Err(CatalogContentSearchError::InvalidLimits),
    };
    let mut scope = AtlasSearchScope::all().with_case_sensitive(!args.ignore_case);
    if let Some((kind, path)) = args.progressive_target_bytes() {
        scope = scope.with_raw_path(kind, path)?;
    }
    CatalogContentSearchSession::open_with_policy(owner(), args.file.as_deref().ok_or(AppError::InvalidRange)?, 1, query,
        CatalogContentSearchOptions { mode, count_all_matches: args.count_all, start_after: args.skip_matches,
            limits: AtlasStreamSearchOptions { max_matches: args.limit, max_scan_bytes: args.max_scan_bytes,
                max_read_calls: MAX_ATLAS_STREAM_SEARCH_READ_CALLS },
            scope }, discovery_policy(args), canceled)
}

/// Poll cancellation before every bounded service step. An accepted file is
/// never undone; stop drops only unfinished work. The observer can stop this
/// driver on backpressure before another source or discovery operation occurs.
fn drive<C: FnMut() -> bool>(args: &Arguments, session: &mut Option<CatalogContentSearchSession>,
    canceled: &mut C, mut observe: impl FnMut(&CatalogContentSearchSession, bool, &mut C) -> Result<(), Failure>)
    -> Result<(), Failure> {
    *session = Some(open(args, canceled)?);
    let session = session.as_mut().expect("installed progressive search");
    observe(session, true, canceled)?;
    while session.cli_progress().more {
        if canceled() {
            let _ = session.stop_cli();
            return Err(AppError::Canceled.into());
        }
        session.advance_cli_step()?;
        let progress = session.cli_progress();
        observe(session, !progress.more || progress.revision % PROGRESS_STEPS == 0, canceled)?;
    }
    if canceled() { return Err(AppError::Canceled.into()); }
    Ok(())
}

fn exit(progress: CatalogSearchProgress, error: Option<Error>) -> u8 {
    match error {
        Some(error) if error.canceled() => EXIT_CANCELED,
        Some(error) if error.output_limit() => EXIT_PARTIAL,
        Some(_) => EXIT_ERROR,
        None if !progress.complete || progress.truncated() => EXIT_PARTIAL,
        None if progress.matches_seen == 0 => EXIT_NO_MATCH,
        None => EXIT_OK,
    }
}
fn status(exit: u8) -> &'static str {
    match exit { EXIT_OK => "ok", EXIT_NO_MATCH => "no-match", EXIT_PARTIAL => "partial",
        EXIT_CANCELED => "canceled", _ => "error" }
}
fn error_field(out: &mut Output, error: Option<Error>) -> Result<(), OutputError> {
    out.literal(",\"error\":")?;
    if let Some(error) = error {
        out.literal("{\"code\":")?; out.quoted(&error.code())?;
        out.literal(",\"subsystem\":")?;
        out.quoted(error.app().map_or("progressive-workspace-search", AppError::subsystem))?;
        out.literal(",\"retryable\":")?; out.boolean(error.app().is_some_and(AppError::retryable))?;
        out.literal(",\"message\":")?;
        out.quoted(error.app().map_or("The progressive workspace observation could not continue.", AppError::message))?;
        out.literal(",\"next_action\":")?;
        out.quoted(error.app().map_or("Check the selected root and reported limits, then start a new live observation.", AppError::next_action))?;
        out.literal("}")?;
    } else { out.literal("null")?; }
    Ok(())
}
fn integer(out: &mut Output, key: &str, value: u64) -> Result<(), OutputError> {
    out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)
}

/// Compact terminal state contains only bounded counters and stable labels.
/// Full rule observations and discovery omissions are emitted as metadata;
/// terminal framing fits the shared sink's protected 16 KiB reservation.
fn summary(out: &mut Output, p: CatalogSearchProgress, delivered: usize, complete: bool)
    -> Result<(), OutputError> {
    for (key, value) in [("revision", p.revision), ("catalogued_files", p.catalogued_files),
        ("discovery_pages", p.discovery_pages), ("scope_files", p.scope_files),
        ("metadata_excluded_files", p.excluded_files), ("pending_catalogued_files", p.pending_catalogued_files),
        ("files_examined", p.examined), ("files_unexamined", p.scope_files.saturating_sub(p.examined)),
        ("incomplete_files", p.incomplete), ("unavailable_files", p.unavailable),
        ("unsupported_files", p.unsupported), ("failed_files", p.failed),
        ("matching_files", p.matching_files), ("retained_hits", p.retained_hits as u64),
        ("delivered_hits", delivered as u64), ("matches_seen", p.matches_seen),
        ("page_start_after", p.skip_matches), ("skipped_matches", p.skip_matches.min(p.matches_seen)),
        ("payload_bytes_read", p.bytes_read), ("scanned_input_bytes", p.scanned_bytes),
        ("read_calls", p.read_calls), ("retained_catalog_pages", p.retained_pages as u64),
        ("retained_catalog_files", p.retained_files as u64),
        ("failed_rule_files", p.failed_rule_files as u64), ("unresolved_rule_paths", p.unresolved_rule_paths as u64)] { integer(out, key, value)?; }
    for (key, value) in [("more", p.more), ("discovery_more", p.discovery_more),
        ("discovery_complete", p.discovery_complete), ("rule_policy_complete", p.rule_policy_complete), ("scan_complete", complete),
        ("match_count_complete", complete), ("count_all_matches", p.count_all),
        ("truncated", p.truncated()),
        ("results_complete", complete && !p.truncated() && delivered == p.retained_hits)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.boolean(value)?;
    }
    out.literal(",\"next_match_offset\":")?;
    let skipped = p.skip_matches.min(p.matches_seen);
    if p.matches_seen - skipped > p.retained_hits as u64 && delivered == p.retained_hits && delivered != 0 {
        out.integer(skipped + delivered as u64)?;
    } else { out.literal("null")?; }
    out.literal(",\"continuation_observation\":\"new-invocation-is-a-new-live-walk\"")?;
    out.literal(",\"discovery\":{\"files\":")?; out.integer(p.discovery.files)?;
    for (key, value) in [("directories", p.discovery.directories), ("excluded_entries", p.discovery.excluded),
        ("symlinks_not_followed", p.discovery.symlinks), ("special_objects", p.discovery.special),
        ("unavailable", p.discovery.unavailable), ("cycles", p.discovery.cycles),
        ("depth_limited", p.discovery.depth_limited), ("path_limited", p.discovery.path_limited),
        ("queue_refused", p.discovery.queue_refused)] { integer(out, key, value)?; }
    out.literal("}")?;
    Ok(())
}

fn stream_response<C: FnMut() -> bool>(parsed: Result<Arguments, ArgumentError>, stdout: &mut impl Write,
    stderr: &mut impl Write, budget: &ResourceBudget, canceled: &mut C) -> u8 {
    let mut sink = match Sink::with_schema(stdout, budget, STREAM_SCHEMA) {
        Ok(sink) => sink,
        Err(_) => { let _ = stderr.write(b"CLI_OUTPUT_ADMISSION\n"); return EXIT_ERROR; }
    };
    let mut session = None;
    let mut delivered = 0usize;
    let result: Result<(), Failure> = (|| {
        let args = parsed.map_err(AppError::from)?;
        sink.emit("start", canceled, |out, _| {
            out.literal(",\"command\":\"search\",\"identity_scope\":\"invocation-local\",\"scope\":\"progressively-discovered-whole-files\",\"requested_path\":")?;
            out.path(args.file.as_deref().ok_or(AppError::InvalidRange)?)?;
            out.literal(",\"membership\":\"progressive-observation\",\"ordering\":\"provisional-discovery-order\",\"respect_ignores\":")?;
            out.boolean(args.respect_ignores)?;
            if let Some((kind, path)) = args.progressive_target_bytes() {
                out.literal(",\"requested_scope_kind\":")?; out.quoted(kind.name())?;
                out.literal(",\"requested_scope_path\":")?; out.utf8_or_null(path)?;
                out.literal(",\"requested_scope_path_hex\":")?; out.hex(path)?;
            }
            out.literal(",\"include_excluded\":")?; out.boolean(args.include_excluded)?;
            out.literal(",\"discovery_policy\":")?; out.quoted(discovery_policy(&args).name())?;
            out.literal(",\"terminal_required\":true,\"capture_limits_applied\":false,\"aggregate_file_limit\":null,\"aggregate_path_byte_limit\":null")?;
            integer(out, "max_record_bytes", MAX_RESPONSE_BYTES as u64)?;
            integer(out, "max_stream_output_bytes", ndjson::MAX_STREAM_OUTPUT_BYTES)?;
            integer(out, "max_write_calls_per_record", ndjson::MAX_WRITE_CALLS as u64)?;
            integer(out, "max_scan_bytes", args.max_scan_bytes)?;
            integer(out, "max_retained_hits", args.limit as u64)?;
            integer(out, "page_start_after", args.skip_matches)?;
            out.literal(",\"progress_counts\":\"finalized-file-matches-with-current-io\"")?;
            Ok::<(), Error>(())
        })?;
        drive(&args, &mut session, canceled, |search, progress, canceled| {
            let current = search.cli_progress();
            while delivered < current.retained_hits {
                sink.emit("match", canceled, |out, _| {
                    integer(out, "revision", current.revision)?;
                    out.literal(",\"match\":")?; search.encode_cli_match(delivered, out)?;
                    Ok::<(), Error>(())
                })?;
                delivered += 1;
            }
            if progress {
                sink.emit("progress", canceled, |out, _| {
                    // This is metadata, not a second native page/result stream.
                    out.literal(",\"search\":{\"command\":\"progress\"")?;
                    search.encode_cli_metadata(out)?;
                    out.literal("}")?;
                    integer(out, "delivered_hits", delivered as u64)?;
                    Ok::<(), Error>(())
                })?;
            }
            Ok(())
        })
    })();
    let error = match result {
        Err(Failure::Delivery { canceled }) => return interrupted(stderr, canceled),
        Err(Failure::Service(error)) => Some(error),
        Ok(()) => None,
    };
    if error.is_some() {
        if let Some(search) = &mut session { let _ = search.stop_cli(); }
    }
    let progress = session.as_ref().map_or_else(CatalogSearchProgress::default, CatalogContentSearchSession::cli_progress);
    let code = exit(progress, error);
    // A clean record boundary can report cancellation. A failed write/flush
    // above already returned and can never be repaired with another record.
    let result = if code == EXIT_CANCELED {
        sink.emit("terminal", &mut || false, |out, _| terminal_fields(out, progress, delivered, code, error))
    } else { sink.emit("terminal", canceled, |out, _| terminal_fields(out, progress, delivered, code, error)) };
    match result {
        Ok(()) => code,
        Err(Failure::Service(error)) if error.canceled() => {
            match sink.emit("terminal", &mut || false, |out, _| {
                terminal_fields(out, progress, delivered, EXIT_CANCELED, Some(error))
            }) {
                Ok(()) => EXIT_CANCELED,
                Err(Failure::Delivery { canceled }) => interrupted(stderr, canceled),
                Err(_) => interrupted(stderr, true),
            }
        }
        Err(Failure::Delivery { canceled }) => interrupted(stderr, canceled),
        Err(_) => interrupted(stderr, false),
    }
}
fn terminal_fields(out: &mut Output, progress: CatalogSearchProgress, delivered: usize,
    exit: u8, error: Option<Error>) -> Result<(), Error> {
    out.literal(",\"status\":")?; out.quoted(status(exit))?;
    integer(out, "exit_code", exit as u64)?;
    summary(out, progress, delivered, error.is_none() && progress.complete)?;
    out.literal(",\"stop_reason\":")?;
    if exit == EXIT_CANCELED { out.quoted("canceled")?; }
    else if error.is_some_and(Error::output_limit) { out.quoted("output-limit")?; }
    else if let Some(reason) = progress.stop_reason { out.quoted(reason)?; }
    else { out.literal("null")?; }
    error_field(out, error)?;
    Ok(())
}

fn document_response(parsed: Result<Arguments, ArgumentError>, json: bool, stdout: &mut impl Write,
    stderr: &mut impl Write, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> u8 {
    let mut out = match Output::new(owner(), MAX_RESPONSE_BYTES, budget, allocation(1)) {
        Ok(out) => out,
        Err(_) => { let _ = stderr.write(b"CLI_OUTPUT_ADMISSION\n"); return EXIT_ERROR; }
    };
    let mut session = None;
    let policy = parsed.as_ref().ok().map(discovery_policy);
    let result = match parsed {
        Ok(args) => drive(&args, &mut session, canceled, |_, _, _| Ok(())),
        Err(error) => Err(Failure::Service(AppError::from(error).into())),
    };
    let mut error = match result { Ok(()) => None, Err(Failure::Service(e)) => Some(e),
        Err(Failure::Delivery { canceled }) => return interrupted(stderr, canceled) };
    if error.is_some() {
        if let Some(search) = &mut session { let _ = search.stop_cli(); }
    }
    let progress = session.as_ref().map_or_else(CatalogSearchProgress::default, CatalogContentSearchSession::cli_progress);
    let mut code = exit(progress, error);
    let encoded = encode_document(&mut out, session.as_ref(), policy, progress, json, code, error);
    if let Err(failure) = encoded {
        error = Some(failure); code = if failure.canceled() { EXIT_CANCELED } else { EXIT_ERROR };
        out.clear();
        if encode_document(&mut out, None, policy, progress, json, code, error).is_err() {
            let _ = stderr.write(b"CLI_ERROR_ENCODING_FAILED\n"); return EXIT_ERROR;
        }
    }
    if code != EXIT_CANCELED && canceled() {
        code = EXIT_CANCELED; error = Some(AppError::Canceled.into());
        out.clear();
        if encode_document(&mut out, session.as_ref(), policy, progress, json, code, error).is_err() {
            out.clear();
            if encode_document(&mut out, None, policy, progress, json, code, error).is_err() {
                let _ = stderr.write(b"CLI_ERROR_ENCODING_FAILED\n"); return EXIT_ERROR;
            }
        }
    }
    let mut delivery_canceled = false;
    let mut stop = || {
        if code != EXIT_CANCELED && canceled() { delivery_canceled = true; true } else { false }
    };
    let delivered = if json || session.is_some() || matches!(code, EXIT_OK | EXIT_NO_MATCH | EXIT_PARTIAL) {
        out.deliver(stdout, ndjson::MAX_WRITE_CALLS, &mut stop)
    } else { out.deliver(stderr, ndjson::MAX_WRITE_CALLS, &mut stop) };
    if delivered.is_err() { return interrupted(stderr, delivery_canceled); }
    code
}
fn encode_document(out: &mut Output, session: Option<&CatalogContentSearchSession>, policy: Option<CatalogDiscoveryPolicy>, p: CatalogSearchProgress,
    json: bool, exit: u8, error: Option<Error>) -> Result<(), Error> {
    if json {
        out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
        out.literal(",\"command\":\"search\",\"identity_scope\":\"invocation-local\",\"status\":")?; out.quoted(status(exit))?;
        integer(out, "exit_code", exit as u64)?;
        out.literal(",\"aggregate_file_limit\":null,\"aggregate_path_byte_limit\":null,\"capture_limits_applied\":false")?;
        if let Some(search) = session { search.encode_cli_metadata(out)?; }
        out.literal(",\"hits\":[")?;
        let delivered = if let Some(search) = session {
            for index in 0..p.retained_hits {
                if index != 0 { out.literal(",")?; }
                search.encode_cli_match(index, out)?;
            }
            p.retained_hits
        } else { 0 };
        out.literal("],\"summary\":{\"scope\":\"progressively-discovered-whole-files\"")?;
        summary(out, p, delivered, error.is_none() && p.complete)?;
        out.literal("}")?; error_field(out, error)?; out.literal("}\n")?;
    } else {
        out.literal("Progressive workspace search; ")?;
        out.literal(policy.map_or("discovery policy not admitted", CatalogDiscoveryPolicy::name))?;
        out.literal("; observed discovery order\n")?;
        if let Some(search) = session { search.encode_cli_human_matches(out)?; }
        out.literal(status(exit))?; out.literal(": ")?; out.literal(&p.matches_seen.to_string())?;
        out.literal(" matches observed, ")?; out.literal(&p.retained_hits.to_string())?;
        out.literal(" retained; ")?; out.literal(&p.examined.to_string())?;
        out.literal(" files examined from ")?; out.literal(&p.catalogued_files.to_string())?;
        out.literal(" discovered files.\n")?;
        if !p.discovery_complete { out.literal("Discovery is incomplete; this is not an exhaustive workspace result.\n")?; }
        if p.incomplete != 0 { out.literal("Some files were unavailable, unsupported, or incompletely read.\n")?; }
        if let Some(reason) = p.stop_reason { out.literal("Search stopped: ")?; out.literal(reason)?; out.literal("\n")?; }
        if p.truncated() { out.literal("The retained result window omits observed matches; totals and stored rows are distinct.\n")?; }
        if let Some(error) = error { out.literal("Error: ")?; out.literal(&error.code())?; out.literal("\n")?; }
    }
    Ok(())
}
fn interrupted(stderr: &mut impl Write, canceled: bool) -> u8 {
    let _ = stderr.write(b"CLI_OUTPUT_INTERRUPTED: progressive search delivery incomplete\n");
    if canceled { EXIT_CANCELED } else { EXIT_ERROR }
}

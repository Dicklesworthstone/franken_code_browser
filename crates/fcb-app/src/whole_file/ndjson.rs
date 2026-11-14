#![forbid(unsafe_code)]

//! Explicit progressive output for the existing continuous-file scanner.
//! Every record is privately staged, bounded, then delivered and flushed. A
//! failed transport is never repaired by appending JSON to an unknown prefix.
//! IDs are invocation-local; sequence numbers order records within that stream.

use std::{fs::File, io::{self, Write}, path::Path};
use fcb::{ByteLength, SourceRevision};
use fcb::search::{CaptureRequest, ResourceBudget, StreamReadState, StreamReadStats, StreamingNeedle};
use fcb::search::workspace::WorkspaceStage;
use fcb::source::{CancelFlag, RawPath};
use crate::{AppError, EXIT_CANCELED, EXIT_ERROR, EXIT_NO_MATCH, EXIT_OK, EXIT_PARTIAL,
    allocation, file_id, owner, revision};
use crate::args::{ArgumentError, Arguments, Needle};
use crate::output::{MAX_RESPONSE_BYTES, Output, OutputError};
use crate::{input, workspace};

pub(crate) const SCHEMA: &str = "fcb.search-stream/1";
pub(crate) const MAX_STREAM_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;
const TERMINAL_RESERVE: u64 = 16 * 1024;
const MAX_WRITE_CALLS: usize = 4096;
const PROGRESS_STEPS: u64 = 256;

enum Failure {
    Service(AppError),
    Delivery { canceled: bool },
}
impl From<AppError> for Failure { fn from(error: AppError) -> Self { Self::Service(error) } }
impl From<OutputError> for Failure { fn from(error: OutputError) -> Self { Self::Service(error.into()) } }

struct CountingWriter<'a, W> { writer: &'a mut W, written: usize }
impl<W: Write> Write for CountingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.writer.write(bytes)?;
        self.written = self.written.saturating_add(count);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> { self.writer.flush() }
}

struct Sink<'a, W> {
    writer: &'a mut W,
    output: Output,
    sequence: u64,
    emitted_bytes: u64,
    max_bytes: u64,
}
impl<'a, W: Write> Sink<'a, W> {
    fn new(writer: &'a mut W, budget: &ResourceBudget) -> Result<Self, AppError> {
        Ok(Self { writer, output: Output::new(owner(), MAX_RESPONSE_BYTES, budget, allocation(1))?,
            sequence: 0, emitted_bytes: 0, max_bytes: MAX_STREAM_OUTPUT_BYTES })
    }

    fn emit<C: FnMut() -> bool>(&mut self, kind: &str, canceled: &mut C,
        encode: impl FnOnce(&mut Output, &mut C) -> Result<(), AppError>) -> Result<(), Failure> {
        if canceled() { return Err(AppError::Canceled.into()); }
        self.output.clear();
        self.output.literal("{\"schema\":")?; self.output.quoted(SCHEMA)?;
        self.output.literal(",\"type\":")?; self.output.quoted(kind)?;
        self.output.literal(",\"sequence\":")?; self.output.integer(self.sequence)?;
        self.output.literal(",\"owner\":\"1\",\"query_generation\":\"1\"")?;
        encode(&mut self.output, canceled)?;
        self.output.literal("}\n")?;
        let bytes = self.output.as_bytes().len() as u64;
        let allowance = if kind == "terminal" { self.max_bytes }
            else { self.max_bytes.saturating_sub(TERMINAL_RESERVE) };
        if bytes > allowance.saturating_sub(self.emitted_bytes) { return Err(OutputError::Limit.into()); }
        let mut writer = CountingWriter { writer: &mut *self.writer, written: 0 };
        let mut delivery_canceled = false;
        let delivered = self.output.deliver(&mut writer, MAX_WRITE_CALLS, || {
            let stop = canceled();
            delivery_canceled |= stop;
            stop
        });
        if delivered.is_err() {
            // A cancellation before the first byte leaves a clean boundary.
            // All actual write errors, including zero-byte errors, poison it.
            if delivery_canceled && writer.written == 0 { return Err(AppError::Canceled.into()); }
            return Err(Failure::Delivery { canceled: delivery_canceled });
        }
        // A failed flush poisons even a record whose newline reached Write.
        // One foreign write/flush may block; this is not a wall-clock deadline.
        writer.flush().map_err(|_| Failure::Delivery { canceled: false })?;
        self.sequence += 1;
        self.emitted_bytes += bytes;
        Ok(())
    }
}

#[derive(Default)]
struct Summary {
    known_files: Option<u64>,
    scope_files: Option<u64>,
    excluded_files: Option<u64>,
    discovery_complete: bool,
    examined: u64,
    finished: u64,
    incomplete: u64,
    in_flight: bool,
    bytes: u64,
    scanned: u64,
    calls: u64,
    seen: u64,
    stored: usize,
    matching_files: u64,
    delivered_files: u64,
    delivered_hits: u64,
    truncated: bool,
    stopped: Option<&'static str>,
}
impl Summary {
    fn work(&mut self, baseline: (u64, u64, u64), stats: StreamReadStats) {
        self.bytes = baseline.0 + stats.bytes_read;
        self.calls = baseline.1 + stats.read_calls;
        self.scanned = baseline.2 + stats.scanned_bytes;
    }
    fn complete(&self) -> bool {
        self.discovery_complete && self.scope_files == Some(self.finished)
            && self.incomplete == 0 && !self.in_flight
    }
    fn fields(&self, out: &mut Output) -> Result<(), AppError> {
        for (name, value) in [("known_files", self.known_files), ("scope_files", self.scope_files),
            ("metadata_excluded_files", self.excluded_files),
            ("unexamined_files", self.scope_files.map(|count| count.saturating_sub(self.examined)))] {
            out.literal(",")?; out.quoted(name)?; out.literal(":")?;
            if let Some(value) = value { out.integer(value)?; } else { out.literal("null")?; }
        }
        out.literal(",\"discovery_complete\":")?; out.boolean(self.discovery_complete)?;
        for (name, value) in [("files_examined", self.examined), ("files_finished", self.finished),
            ("incomplete_files", self.incomplete + u64::from(self.in_flight)),
            ("payload_bytes_read", self.bytes), ("scanned_input_bytes", self.scanned),
            ("read_calls", self.calls), ("matches_seen", self.seen), ("stored_hits", self.stored as u64),
            ("matching_files", self.matching_files), ("file_records", self.delivered_files),
            ("delivered_hits", self.delivered_hits)] {
            out.literal(",")?; out.quoted(name)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"truncated\":")?; out.boolean(self.truncated)?;
        Ok(())
    }
}

pub(crate) fn run(parsed: Result<Arguments, ArgumentError>, stdout: &mut impl Write,
    stderr: &mut impl Write, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> u8 {
    let sink = match Sink::new(stdout, budget) {
        Ok(sink) => sink,
        Err(_) => { let _ = stderr.write(b"CLI_OUTPUT_ADMISSION\n"); return EXIT_ERROR; }
    };
    drive(parsed, sink, stderr, budget, canceled)
}

fn drive<W: Write, C: FnMut() -> bool>(parsed: Result<Arguments, ArgumentError>,
    mut sink: Sink<'_, W>, stderr: &mut impl Write, budget: &ResourceBudget, canceled: &mut C) -> u8 {
    let mut summary = Summary::default();
    let result = match parsed {
        Ok(args) => search(&args, budget, &mut sink, &mut summary, canceled),
        Err(error) => Err(Failure::Service(error.into())),
    };
    // Do not invoke cancellation or visit another source after transport failure.
    let error = match result {
        Err(Failure::Delivery { canceled }) => return interrupted(stderr, canceled),
        Err(Failure::Service(error)) => Some(error),
        Ok(()) if canceled() => Some(AppError::Canceled),
        Ok(()) => None,
    };
    let exit = match error {
        Some(error) if error.is_canceled() => EXIT_CANCELED,
        Some(AppError::Output(OutputError::Limit)) if sink.sequence != 0 => {
            summary.stopped = Some("output-limit"); EXIT_PARTIAL
        }
        Some(_) => EXIT_ERROR,
        None if !summary.complete() || summary.truncated => EXIT_PARTIAL,
        None if summary.seen == 0 => EXIT_NO_MATCH,
        None => EXIT_OK,
    };
    let delivered = if exit == EXIT_CANCELED {
        terminal(&mut sink, &summary, exit, error, &mut || false)
    } else {
        terminal(&mut sink, &summary, exit, error, canceled)
    };
    match delivered {
        Ok(()) => exit,
        // Cancellation during private encoding or before any terminal bytes
        // can still publish one truthful canceled terminal at the same sequence.
        Err(Failure::Service(error)) if error.is_canceled() => {
            match terminal(&mut sink, &summary, EXIT_CANCELED, Some(error), &mut || false) {
                Ok(()) => EXIT_CANCELED,
                Err(Failure::Delivery { canceled }) => interrupted(stderr, canceled),
                Err(_) => interrupted(stderr, true),
            }
        }
        Err(Failure::Delivery { canceled }) => interrupted(stderr, canceled),
        Err(_) => interrupted(stderr, false),
    }
}

fn interrupted(stderr: &mut impl Write, canceled: bool) -> u8 {
    let _ = stderr.write(b"CLI_OUTPUT_INTERRUPTED: stream delivery incomplete\n");
    if canceled { EXIT_CANCELED } else { EXIT_ERROR }
}

fn terminal<W: Write, C: FnMut() -> bool>(sink: &mut Sink<'_, W>, summary: &Summary,
    exit: u8, error: Option<AppError>, canceled: &mut C) -> Result<(), Failure> {
    sink.emit("terminal", canceled, |out, _| {
        out.literal(",\"status\":")?;
        out.quoted(match exit { EXIT_OK => "ok", EXIT_NO_MATCH => "no-match", EXIT_PARTIAL => "partial",
            EXIT_CANCELED => "canceled", _ => "error" })?;
        out.literal(",\"exit_code\":")?; out.integer(u64::from(exit))?;
        let complete = error.is_none() && summary.complete();
        out.literal(",\"scan_complete\":")?; out.boolean(complete)?;
        out.literal(",\"match_count_complete\":")?; out.boolean(complete)?;
        out.literal(",\"results_complete\":")?; out.boolean(complete && !summary.truncated)?;
        summary.fields(out)?;
        out.literal(",\"stop_reason\":")?;
        if exit == EXIT_CANCELED { out.quoted("canceled")?; }
        else if let Some(reason) = summary.stopped { out.quoted(reason)?; }
        else { out.literal("null")?; }
        out.literal(",\"error\":")?;
        if let Some(error) = error {
            out.literal("{\"code\":")?; out.quoted(&error.code())?;
            out.literal(",\"subsystem\":")?; out.quoted(error.subsystem())?;
            out.literal(",\"retryable\":")?; out.boolean(error.retryable())?;
            out.literal(",\"message\":")?; out.quoted(error.message())?; out.literal("}")?;
        } else { out.literal("null")?; }
        Ok(())
    })
}

fn search<W: Write, C: FnMut() -> bool>(args: &Arguments, budget: &ResourceBudget,
    sink: &mut Sink<'_, W>, summary: &mut Summary, canceled: &mut C) -> Result<(), Failure> {
    if canceled() { return Err(AppError::Canceled.into()); }
    let pattern = super::needle(args, budget)?;
    let _paths = budget.try_reserve_managed(owner(), allocation(42), ByteLength::new(128 * 1024))
        .map_err(|_| AppError::Admission)?;
    sink.emit("start", canceled, |out, _| {
        out.literal(",\"command\":\"search\",\"identity_scope\":\"invocation-local\",\"scope\":")?;
        out.quoted(if args.workspace { "workspace" } else { "named-file" })?;
        out.literal(",\"requested_path\":")?;
        out.path(args.file.as_deref().ok_or(AppError::InvalidRange)?)?;
        out.literal(",\"strategy\":\"streaming-whole-file\",\"capture_limits_applied\":false")?;
        super::matching_fields(pattern.mode(), super::expression(args).is_some(), out)?;
        super::expression_fields(args, if args.workspace { "root-relative-utf8-or-escaped-search-key" }
            else { "absolute-named-path-utf8-or-escaped-search-key" }, out)?;
        out.literal(",\"query\":{")?;
        match args.needle.as_ref() {
            Some(Needle::Text(text) | Needle::Identifier(text)) => {
                out.literal("\"text\":")?; out.quoted(text)?;
            }
            Some(Needle::Raw(bytes)) => { out.literal("\"raw_hex\":")?; out.hex(bytes)?; }
            Some(Needle::Query(query)) => { out.literal("\"expression\":")?; out.quoted(&query.raw_query)?; }
            _ => return Err(AppError::InvalidRange),
        }
        out.literal("},\"count_all_matches\":")?; out.boolean(args.count_all)?;
        for (name, value) in [("max_scan_bytes", args.max_scan_bytes), ("max_read_calls", super::MAX_READ_CALLS),
            ("max_retained_hits", args.limit as u64), ("max_record_bytes", MAX_RESPONSE_BYTES as u64),
            ("max_stream_output_bytes", MAX_STREAM_OUTPUT_BYTES), ("max_write_calls_per_record", MAX_WRITE_CALLS as u64)] {
            out.literal(",")?; out.quoted(name)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"terminal_required\":true,\"progress_counts\":\"finalized-files-only\"")?;
        Ok(())
    })?;
    if args.workspace {
        let (root, mut catalog) = workspace::open_catalog(args, budget)?;
        let cancel = CancelFlag::new();
        while catalog.stage() == WorkspaceStage::Discovering {
            if canceled() { return Err(AppError::Canceled.into()); }
            catalog.step(&cancel).map_err(AppError::from)?;
            summary.known_files = Some(catalog.entries().len() as u64);
            let pages = catalog.discovery_pages() as u64;
            if pages == 1 || pages % 16 == 0 {
                sink.emit("progress", canceled, |out, _| {
                    out.literal(",\"phase\":\"discovery\",\"discovery_pages\":")?; out.integer(pages)?;
                    summary.fields(out)
                })?;
            }
        }
        summary.known_files = Some(catalog.entries().len() as u64);
        summary.discovery_complete = catalog.discovery_complete();
        let mut scoped = 0u64;
        for entry in catalog.entries() {
            if canceled() { return Err(AppError::Canceled.into()); }
            if super::matches_metadata(args, entry.path().raw()) { scoped += 1; }
        }
        summary.scope_files = Some(scoped);
        summary.excluded_files = Some(catalog.entries().len() as u64 - scoped);
        catalog.validate_active().map_err(AppError::from)?;
        sink.emit("progress", canceled, |out, _| {
            out.literal(",\"phase\":\"catalog-ready\",\"catalog\":")?;
            workspace::common(out, "search", &catalog, &root)?;
            out.literal("}")?;
            summary.fields(out)
        })?;
        for (ordinal, entry) in catalog.entries().iter().enumerate() {
            if canceled() { return Err(AppError::Canceled.into()); }
            catalog.validate_active().map_err(AppError::from)?;
            if !super::matches_metadata(args, entry.path().raw()) { continue; }
            let id = catalog.file_id(ordinal).ok_or(AppError::InvalidRange)?;
            let revision = SourceRevision::new(owner(), ordinal as u64 + 1).map_err(|_| AppError::InvalidRange)?;
            let request = CaptureRequest::new(id, revision).map_err(|_| AppError::InvalidRange)?;
            opening(sink, summary, request, &entry.path().raw().to_path_buf(), canceled)?;
            if canceled() { return Err(AppError::Canceled.into()); }
            catalog.validate_active().map_err(AppError::from)?;
            let opened = super::open_workspace_source(&root, entry.path());
            let mut stop = || canceled() || catalog.validate_active().is_err();
            observe(args, opened, request, &entry.path().raw().to_path_buf(), &pattern,
                budget, sink, summary, &mut stop)?;
            catalog.validate_active().map_err(AppError::from)?;
            if summary.stopped.is_some() { break; }
        }
        catalog.validate_active().map_err(AppError::from)?;
    } else {
        let path = input::absolute(args.file.as_deref().ok_or(AppError::InvalidRange)?)?;
        summary.known_files = Some(1); summary.discovery_complete = true;
        let included = super::matches_metadata(args, &RawPath::from_path(&path));
        summary.scope_files = Some(u64::from(included));
        summary.excluded_files = Some(u64::from(!included));
        if included {
            if canceled() { return Err(AppError::Canceled.into()); }
            let request = CaptureRequest::new(file_id(), revision()).map_err(|_| AppError::InvalidRange)?;
            opening(sink, summary, request, &path, canceled)?;
            if canceled() { return Err(AppError::Canceled.into()); }
            let opened = input::open_regular(&path).map(|(file, _)| file);
            observe(args, opened, request, &path, &pattern, budget, sink, summary, canceled)?;
        }
    }
    Ok(())
}

fn opening<W: Write, C: FnMut() -> bool>(sink: &mut Sink<'_, W>, summary: &Summary,
    request: CaptureRequest, path: &Path, canceled: &mut C) -> Result<(), Failure> {
    // A consumer that closes after the preceding file is detected BEFORE
    // opening or scanning the next source. No observation revision is claimed.
    sink.emit("progress", canceled, |out, _| {
        out.literal(",\"phase\":\"opening\",\"current_file\":{\"file_id\":")?;
        out.integer(request.file().get())?;
        out.literal(",\"revision\":null,\"path\":")?; out.path(path)?; out.literal("}")?;
        summary.fields(out)
    })
}

fn observe<W: Write, C: FnMut() -> bool>(args: &Arguments, opened: Result<File, AppError>,
    request: CaptureRequest, path: &Path, pattern: &StreamingNeedle, budget: &ResourceBudget,
    sink: &mut Sink<'_, W>, summary: &mut Summary, canceled: &mut C) -> Result<(), Failure> {
    summary.examined += 1;
    summary.in_flight = true;
    let baseline = (summary.bytes, summary.calls, summary.scanned);
    let options = super::options(args, args.max_scan_bytes.saturating_sub(summary.bytes),
        super::MAX_READ_CALLS.saturating_sub(summary.calls), args.limit.saturating_sub(summary.stored));
    let mut steps = 0u64;
    let result = match opened {
        Ok(file) => super::scan_with_progress(file, request, pattern, options, args.count_all, budget, canceled,
            |stats, canceled| {
                // Work is charged even when delivery fails on this very step.
                summary.work(baseline, stats);
                steps += 1;
                if steps == 1 || steps % PROGRESS_STEPS == 0 {
                    sink.emit("progress", canceled, |out, _| {
                        out.literal(",\"phase\":\"scanning\",\"current_file\":{\"file_id\":")?;
                        out.integer(request.file().get())?;
                        out.literal(",\"revision\":")?; out.integer(request.revision().get())?;
                        out.literal(",\"path\":")?; out.path(path)?; out.literal("}")?;
                        summary.fields(out)
                    })?;
                }
                Ok::<(), Failure>(())
            }),
        Err(error) => Err(Failure::Service(error)),
    };
    let report = match result {
        Ok(report) => report,
        Err(Failure::Service(error)) if !error.is_canceled() && !matches!(error, AppError::Output(_)) => {
            summary.in_flight = false; summary.finished += 1; summary.incomplete += 1;
            sink.emit("file", canceled, |out, _| {
                out.literal(",\"file\":{\"path\":")?; out.path(path)?;
                out.literal(",\"file_id\":")?; out.integer(request.file().get())?;
                out.literal(",\"revision\":null,\"state\":\"unavailable\",\"whole_file_complete\":false,\"error_code\":")?;
                out.quoted(&error.code())?; out.literal(",\"hits\":[]}")?; Ok(())
            })?;
            summary.delivered_files += 1;
            return if args.workspace { Ok(()) } else { Err(error.into()) };
        }
        Err(error) => return Err(error),
    };
    summary.work(baseline, report.search().stats());
    summary.in_flight = false; summary.finished += 1;
    if !report.is_complete() { summary.incomplete += 1; }
    let search = report.search();
    summary.seen += search.matches_seen();
    summary.stored += search.hits().len();
    if search.matches_seen() != 0 { summary.matching_files += 1; }
    summary.truncated |= search.hits_truncated();
    summary.stopped = match search.state() {
        StreamReadState::Truncated => Some("match-limit"),
        StreamReadState::ByteLimit => Some("byte-limit"),
        StreamReadState::CallLimit => Some("read-call-limit"),
        _ => None,
    };
    sink.emit("file", canceled, |out, canceled| {
        out.literal(",\"file\":{\"path\":")?; out.path(path)?;
        super::fields(&report, out, canceled)?;
        out.literal("}")?; Ok(())
    })?;
    summary.delivered_files += 1;
    summary.delivered_hits += search.hits().len() as u64;
    Ok(())
}


#[cfg(all(test, any(target_os = "macos",
    all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))))]
mod tests {
    use super::*;
    use std::{ffi::OsString, fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};

    struct Temp(PathBuf);
    impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

    #[test]
    fn aggregate_output_limit_reserves_a_terminal_and_does_not_read_the_next_file() {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-ndjson-cap-{}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        let temp = Temp(path);
        fs::write(temp.0.join("a.rs"), b"needle").unwrap();
        fs::write(temp.0.join("b.rs"), b"needle needle").unwrap();
        fs::write(temp.0.join("c.rs"), b"needle needle needle").unwrap();
        let values: Vec<OsString> = vec!["search".into(), temp.0.as_os_str().to_owned(),
            "--workspace".into(), "--whole-file".into(), "--text".into(), "needle".into(), "--stream".into()];
        let parsed = crate::args::parse(&values).unwrap();
        let budget = ResourceBudget::new(owner(), ByteLength::new(crate::MANAGED_BYTES)).unwrap();
        let (mut full, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(run(Ok(parsed.clone()), &mut full, &mut stderr, &budget, &mut || false), EXIT_OK);
        assert!(stderr.is_empty());
        let text = std::str::from_utf8(&full).unwrap();
        let mut first_file_end = 0usize;
        for line in text.split_inclusive('\n') {
            first_file_end += line.len();
            if line.contains("\"type\":\"file\"") { break; }
        }
        assert!(first_file_end < full.len());
        let maximum = first_file_end as u64 + TERMINAL_RESERVE;
        let mut partial = Vec::new();
        let mut sink = Sink::new(&mut partial, &budget).unwrap();
        sink.max_bytes = maximum;
        assert_eq!(drive(Ok(parsed), sink, &mut stderr, &budget, &mut || false), EXIT_PARTIAL);
        assert!(stderr.is_empty());
        assert!(partial.len() as u64 <= maximum);
        assert_eq!(&partial[..first_file_end], &full[..first_file_end]);
        let terminal = std::str::from_utf8(&partial[first_file_end..]).unwrap();
        assert_eq!(terminal.lines().count(), 1);
        for expected in ["\"type\":\"terminal\"", "\"status\":\"partial\"",
            "\"stop_reason\":\"output-limit\"", "\"code\":\"CLI_OUTPUT_LIMIT\"",
            "\"payload_bytes_read\":\"6\"", "\"files_examined\":\"1\"",
            "\"unexamined_files\":\"2\"", "\"file_records\":\"1\"",
            "\"scan_complete\":false", "\"match_count_complete\":false"] {
            assert!(terminal.contains(expected), "missing {expected}: {terminal}");
        }
    }
}

#![forbid(unsafe_code)]

//! Read-only CLI composition of the retained first-party Markdown reader. No
//! command execution, link following, image loading or implicit stdin occurs.
use std::{ffi::OsString, io::Write, path::{Path, PathBuf}, sync::Arc};
use fcb::{ByteLength, ByteOffset, ByteRange};
use fcb_core::{DocumentGeneration, DocumentId};
use fcb::document::{TextSelectionRange};
use fcb::document::reader::{DocumentReader, DocumentReadError, DocumentReadOptions,
    MAX_DOCUMENT_SOURCE_BYTES, MAX_DOCUMENT_WINDOW_LINES};
use fcb::search::{CaptureRequest, CompleteCapture, ExtentConsistency, ExtentReadState,
    ExtentStepBudget, FileRangeReader, ResourceBudget};
use crate::{AppError, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED, MANAGED_BYTES,
    SCHEMA, allocation, file_id, owner, revision, input};
use crate::args::{self, ArgumentError};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};

const HELP: &str = "Captured Markdown reading\n\n\
  fcb markdown FILE [--json] [--width N] [--line N] [--lines N]\n\
  fcb markdown FILE --heading SLUG [--lines N] [--json]\n\
  --max-source-bytes N        Complete source admission (default 65536, max 262144)\n\
  --copy-start N --copy-end N  Rendered UTF-8 byte selection; end exclusive\n\
Defaults: 100 logical columns, first rendered line 1, 40 rendered lines.\n\
Rendered line numbers are NOT original source line numbers. Width uses logical\n\
character cells, not native shaping. UTF-8, including an initial BOM, is supported.\n\
UTF-16 and malformed UTF-8 are refused; ordinary fcb read remains available.\n\
Headings use upstream canonical slugs without a leading #. --heading and --line\n\
are alternatives. Copy returns rendered text and separately labeled enclosing\n\
original Markdown, not an invented source concatenation. -- ends options.\n\
No links, images, includes, network, scripts, repository scan or stdin reads.\n\
Exit: 0 entire document visible, 3 bounded window, 2 error, 130 canceled.\n\
Logical headless output only; native rendering and independent verification pending.\n";

#[derive(Clone, Debug)]
pub(crate) struct ViewOptions {
    pub read: DocumentReadOptions,
    pub first: usize,
    pub lines: usize,
    pub heading: Option<String>,
    pub copy: Option<(usize, usize)>,
}
impl Default for ViewOptions {
    fn default() -> Self { Self { read: Default::default(), first: 0, lines: 40, heading: None, copy: None } }
}
struct Options { path: PathBuf, json: bool, view: ViewOptions }
#[derive(Clone, Copy, Debug)]
pub(crate) enum Error { App(AppError), Document(DocumentReadError) }
impl From<AppError> for Error { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<ArgumentError> for Error { fn from(e: ArgumentError) -> Self { Self::App(e.into()) } }
impl From<OutputError> for Error { fn from(e: OutputError) -> Self { Self::App(e.into()) } }
impl From<DocumentReadError> for Error { fn from(e: DocumentReadError) -> Self { Self::Document(e) } }
impl Error {
    pub(crate) fn code(self) -> String { match self { Self::App(e) => e.code(), Self::Document(e) => e.code().to_owned() } }
    pub(crate) fn canceled(self) -> bool {
        matches!(self, Self::Document(DocumentReadError::Canceled)) || matches!(self, Self::App(e) if e.is_canceled())
    }
}
fn takes_value(arg: &str) -> bool {
    matches!(arg, "--width" | "--line" | "--lines" | "--heading" | "--max-source-bytes" | "--copy-start" | "--copy-end")
}
fn json_requested(arguments: &[OsString]) -> bool {
    let mut cursor = 0;
    while cursor < arguments.len().min(args::MAX_ARGUMENTS + 1) {
        let arg = &arguments[cursor]; cursor += 1;
        if arg == "--" { break; }
        if arg == "--json" { return true; }
        if arg.to_str().is_some_and(takes_value) { cursor += 1; }
    }
    false
}
fn parse(arguments: &[OsString]) -> Result<Options, Error> {
    if arguments.len() > args::MAX_ARGUMENTS { return Err(ArgumentError::Limit.into()); }
    let mut total = 0usize;
    for arg in arguments {
        total = total.checked_add(arg.len()).ok_or(ArgumentError::Limit)?;
        if total > args::MAX_ARGUMENT_BYTES || arg.len() > args::MAX_SINGLE_ARGUMENT { return Err(ArgumentError::Limit.into()); }
    }
    let mut view = ViewOptions::default(); let mut path = None; let mut json = false;
    let (mut start, mut end) = (None, None);
    let (mut cursor, mut seen, mut positional) = (0usize, 0u32, false);
    while cursor < arguments.len() {
        let arg = &arguments[cursor]; cursor += 1;
        if !positional && arg == "--" { positional = true; continue; }
        let option = if positional { None } else { arg.to_str().filter(|s| s.starts_with('-')) };
        let Some(option) = option else {
            if path.is_some() { return Err(ArgumentError::MultipleSources.into()); }
            if arg.is_empty() { return Err(ArgumentError::MissingSource.into()); }
            path = Some(PathBuf::from(arg)); continue;
        };
        let bit = match option { "--json" => 1, "--width" => 2, "--line" => 4, "--lines" => 8,
            "--heading" => 16, "--max-source-bytes" => 32, "--copy-start" => 64, "--copy-end" => 128,
            _ => return Err(ArgumentError::UnknownOption.into()) };
        if seen & bit != 0 { return Err(ArgumentError::DuplicateOption.into()); } seen |= bit;
        if option == "--json" { json = true; continue; }
        let value = arguments.get(cursor).ok_or(ArgumentError::MissingValue)?; cursor += 1;
        let value = value.to_str().ok_or(ArgumentError::MissingValue)?;
        if option == "--heading" {
            if value.is_empty() || value.len() > 4096 { return Err(ArgumentError::Limit.into()); }
            view.heading = Some(value.to_owned()); continue;
        }
        let value = usize::try_from(args::decimal(value)?).map_err(|_| ArgumentError::Limit)?;
        match option {
            "--width" => view.read.width_columns = u32::try_from(value).map_err(|_| ArgumentError::Limit)?,
            "--line" => view.first = value.checked_sub(1).ok_or(ArgumentError::InvalidNumber)?,
            "--lines" => view.lines = value,
            "--max-source-bytes" => view.read.max_source_bytes = value,
            "--copy-start" => start = Some(value), "--copy-end" => end = Some(value),
            _ => unreachable!(),
        }
    }
    if seen & 4 != 0 && view.heading.is_some() { return Err(ArgumentError::IncompatibleOptions.into()); }
    view.copy = match (start, end) {
        (None, None) => None, (Some(a), Some(b)) if a < b => Some((a, b)),
        _ => return Err(ArgumentError::IncompatibleOptions.into()),
    };
    view.read.validate()?;
    if !(1..=MAX_DOCUMENT_WINDOW_LINES).contains(&view.lines) { return Err(ArgumentError::Limit.into()); }
    Ok(Options { path: path.ok_or(ArgumentError::MissingSource)?, json, view })
}

pub(crate) fn run(arguments: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write,
    mut canceled: impl FnMut() -> bool) -> u8 {
    let json = json_requested(arguments);
    let budget = match ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)) {
        Ok(b) => b, Err(_) => { let _ = stderr.write(b"CLI_RESOURCE_DENIED\n"); return EXIT_ERROR; }
    };
    let mut out = match Output::new(owner(), MAX_RESPONSE_BYTES, &budget, allocation(1)) {
        Ok(o) => o, Err(_) => { let _ = stderr.write(b"CLI_OUTPUT_ADMISSION\n"); return EXIT_ERROR; }
    };
    let help = !arguments.is_empty() && arguments.len() <= 2
        && arguments.iter().any(|arg| arg == "--help" || arg == "-h")
        && arguments.iter().all(|arg| arg == "--help" || arg == "-h" || arg == "--json");
    let result = if help {
        if json { out.literal("{\"schema\":\"fcb.cli/1\",\"text\":")
            .and_then(|_| out.quoted(HELP)).and_then(|_| out.literal("}\n")).map(|_| EXIT_OK).map_err(Error::from) }
        else { out.literal(HELP).map(|_| EXIT_OK).map_err(Error::from) }
    } else { parse(arguments).and_then(|options| execute(options, &mut out, &budget, &mut canceled)) };
    let result = if canceled() && result.is_ok() { Err(AppError::Canceled.into()) } else { result };
    let exit = match result {
        Ok(exit) => exit,
        Err(error) => {
            out.clear();
            let encoded = if json {
                out.literal("{\"schema\":\"fcb.cli/1\",\"command\":\"markdown\",\"status\":\"error\",\"complete\":false,\"error\":{\"code\":")
                    .and_then(|_| out.quoted(&error.code()))
                    .and_then(|_| out.literal(",\"next_action\":\"Run fcb markdown --help; use fcb read for raw or unsupported source.\"}}\n"))
            } else { out.literal(&error.code()).and_then(|_| out.literal("; run fcb markdown --help\n")) };
            if encoded.is_err() { let _ = stderr.write(b"CLI_ERROR_ENCODING_FAILED\n"); return EXIT_ERROR; }
            if error.canceled() { EXIT_CANCELED } else { EXIT_ERROR }
        }
    };
    let mut delivery_canceled = false;
    let mut stop = || { if exit != EXIT_CANCELED && canceled() { delivery_canceled = true; true } else { false } };
    let delivery = if json || matches!(exit, EXIT_OK | EXIT_PARTIAL) { out.deliver(stdout, 4096, &mut stop) }
        else { out.deliver(stderr, 4096, &mut stop) };
    if delivery.is_err() {
        let _ = stderr.write(b"CLI_OUTPUT_INTERRUPTED: Markdown delivery incomplete\n");
        return if delivery_canceled { EXIT_CANCELED } else { EXIT_ERROR };
    }
    exit
}

fn execute(options: Options, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Error> {
    if canceled() { return Err(AppError::Canceled.into()); }
    let _source_admission = budget.try_reserve_managed(owner(), allocation(79),
        ByteLength::new((3 * options.view.read.max_source_bytes + 128 * 1024) as u64)).map_err(|_| AppError::Admission)?;
    let path = input::absolute(&options.path)?;
    let (capture, calls) = load(&path, options.view.read.max_source_bytes, budget, canceled)?;
    if options.json {
        out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
        out.literal(",\"command\":\"markdown\",\"status\":\"ok\",\"identity_scope\":\"response-local\",\"path\":")?;
        out.path(&path)?;
        out.literal(",\"payload_bytes_read\":")?; out.integer(capture.bytes().len() as u64)?;
        out.literal(",\"read_calls\":")?; out.integer(calls)?;
        out.literal(",\"source_consistency\":\"unchanged-metadata-not-atomic\",\"document\":")?;
    } else { out.literal("Captured Markdown: ")?; out.path(&path)?; out.literal("\n")?; }
    let entire = write_capture(&capture, &options.view, options.json, out, budget, canceled)?;
    if options.json { out.literal("}\n")?; }
    Ok(if entire { EXIT_OK } else { EXIT_PARTIAL })
}

fn load(path: &Path, limit: usize, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
    -> Result<(CompleteCapture, u64), Error> {
    if limit > MAX_DOCUMENT_SOURCE_BYTES { return Err(DocumentReadError::InvalidLimits.into()); }
    let (file, metadata) = input::open_regular(path)?;
    if metadata.len() > limit as u64 { return Err(DocumentReadError::SourceLimit.into()); }
    let length = ByteLength::new(metadata.len());
    let request = CaptureRequest::new(file_id(), revision()).map_err(|_| AppError::InvalidRange)?;
    let mut ranged = request;
    if metadata.len() != 0 {
        ranged = request.with_range(ByteRange::new(ByteOffset::new(0), ByteOffset::new(metadata.len()))
            .map_err(|_| AppError::InvalidRange)?).map_err(|_| AppError::InvalidRange)?;
    }
    let mut reader = FileRangeReader::new(file_id(), file).map_err(AppError::from)?;
    let mut read = reader.begin(ranged, budget, allocation(80)).map_err(AppError::from)?;
    while read.state() == ExtentReadState::Pending {
        if canceled() { return Err(AppError::Canceled.into()); }
        let calls = read.stats().read_calls;
        if calls >= 4096 { return Err(AppError::IoCallLimit.into()); }
        read.step(ExtentStepBudget { max_bytes: 16 * 1024, max_calls: (4096 - calls).min(32) as usize }, &mut *canceled)
            .map_err(AppError::from)?;
    }
    let calls = read.stats().read_calls;
    let extent = read.finish(&mut *canceled).map_err(AppError::from)?;
    if !extent.request_filled() || !extent.covers_whole_observation()
        || extent.final_length() != Some(length) || extent.consistency() != ExtentConsistency::UnchangedMetadata {
        return Err(AppError::SourceChanged.into());
    }
    let capture = CompleteCapture::new(request, length, Arc::from(extent.bytes())).map_err(|_| AppError::InvalidRange)?;
    Ok((capture, calls))
}

/// Shared by standalone and captured-workspace document routes. Never opens a
/// path; the caller retains the admitted capture and validates its own grant.
pub(crate) fn write_capture(capture: &CompleteCapture, options: &ViewOptions, json: bool,
    out: &mut Output, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<bool, Error> {
    let owner = capture.request().file().owner();
    let generation = DocumentGeneration::new(owner, 1).map_err(|_| AppError::InvalidRange)?;
    let reader = DocumentReader::prepare(capture, DocumentId::new(owner, 1).map_err(|_| AppError::InvalidRange)?,
        generation, options.read, budget, allocation(81), &mut *canceled)?;
    let window = match options.heading.as_deref() {
        Some(slug) => reader.window_at_heading(slug, options.lines)?, None => reader.window(options.first, options.lines)?,
    };
    if json {
        out.literal("{\"document_schema\":\"fcb.document/1\",\"qualification\":\"implemented-unqualified\",\"native_presented\":false,\"layout_mode\":\"logical-character-cells-not-native-shaping\",\"document_complete\":true,\"asset_policy\":\"no-external-access\",\"additional_source_bytes_read\":\"0\",\"file_id\":")?;
        out.integer(capture.request().file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(capture.request().revision().get())?;
        out.literal(",\"document_generation\":")?; out.integer(generation.get())?;
        out.literal(",\"source_bytes\":")?; out.integer(capture.bytes().len() as u64)?;
        out.literal(",\"bom_bytes\":")?; out.integer(reader.source_base() as u64)?;
        out.literal(",\"width_columns\":")?; out.integer(u64::from(options.read.width_columns))?;
        out.literal(",\"total_rendered_lines\":")?; out.integer(reader.total_lines() as u64)?;
        out.literal(",\"whole_document_visible\":")?; out.boolean(window.whole_document_visible())?;
        out.literal(",\"first_rendered_line\":")?; out.integer(window.first_index() as u64 + 1)?;
        out.literal(",\"next_rendered_line\":")?;
        if let Some(next) = window.next_index() { out.integer(next as u64 + 1)?; } else { out.literal("null")?; }
        out.literal(",\"headings\":[")?;
        for (i, heading) in reader.headings().iter().enumerate() {
            if canceled() { return Err(AppError::Canceled.into()); }
            if i != 0 { out.literal(",")?; }
            out.literal("{\"slug\":")?; out.quoted(&heading.slug)?;
            out.literal(",\"title\":")?; out.quoted(&heading.title)?;
            out.literal(",\"level\":")?; out.integer(u64::from(heading.level))?;
            out.literal(",\"original_range\":")?; out.range(reader.original_span(heading.source_span)?)?;
            out.literal(",\"rendered_utf8_offset\":")?; out.integer(heading.rendered_offset as u64)?; out.literal("}")?;
        }
        out.literal("],\"lines\":[")?;
        for (i, line) in window.lines().iter().enumerate() {
            if canceled() { return Err(AppError::Canceled.into()); }
            if i != 0 { out.literal(",")?; }
            out.literal("{\"rendered_line\":")?; out.integer(line.line_index as u64 + 1)?;
            out.literal(",\"text\":")?; out.quoted(&line.rendered_text)?;
            out.literal(",\"rendered_utf8_range\":")?; rendered_range(out, line.rendered_range)?;
            out.literal(",\"enclosing_original_range\":")?;
            if line.source_span.start == line.source_span.end { out.literal("null")?; }
            else { out.range(reader.original_span(line.source_span)?)?; }
            out.literal("}")?;
        }
        out.literal("],\"selection\":")?;
        if let Some((start, end)) = options.copy {
            let selected = reader.selection(TextSelectionRange::new(start, end))?;
            out.literal("{\"rendered_text\":")?; out.quoted(selected.rendered_text)?;
            out.literal(",\"rendered_utf8_range\":")?; rendered_range(out, selected.rendered_range)?;
            out.literal(",\"source_copy_kind\":\"enclosing-markdown-block-not-literal-rendered-selection\",\"enclosing_original_range\":")?;
            out.range(selected.enclosing_original_range)?;
            out.literal(",\"enclosing_original_hex\":")?; out.hex(selected.enclosing_original_bytes)?; out.literal("}")?;
        } else { out.literal("null")?; }
        out.literal("}")?;
    } else {
        out.literal("Logical Markdown reading; no asset or link access; not native shaped text.\n")?;
        for line in window.lines() {
            if canceled() { return Err(AppError::Canceled.into()); }
            out.human_text(&line.rendered_text)?; out.literal("\n")?;
        }
        if let Some((start, end)) = options.copy {
            let selected = reader.selection(TextSelectionRange::new(start, end))?;
            out.literal("[rendered text selection]\n")?; out.human_text(selected.rendered_text)?;
            out.literal("\n[enclosing original Markdown; not literal selection]\n")?;
            out.human_text(std::str::from_utf8(selected.enclosing_original_bytes).map_err(|_| DocumentReadError::InvalidUtf8)?)?;
            out.literal("\n")?;
        }
        if !window.whole_document_visible() { out.literal("[bounded document window; use --line/--heading to navigate]\n")?; }
    }
    if canceled() { return Err(AppError::Canceled.into()); }
    reader.validate_delivery(capture, generation)?;
    Ok(window.whole_document_visible())
}
fn rendered_range(out: &mut Output, range: TextSelectionRange) -> Result<(), Error> {
    out.literal("{\"start\":")?; out.integer(range.start as u64)?;
    out.literal(",\"end\":")?; out.integer(range.end as u64)?; out.literal("}")?; Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn argv(args: &[&str]) -> Vec<OsString> { args.iter().map(OsString::from).collect() }
    #[test]
    fn values_and_delimiters_do_not_change_permissions_or_output_mode() {
        assert!(!json_requested(&argv(&["file", "--heading", "--json"])));
        assert!(!json_requested(&argv(&["--", "--json"])));
        let options = parse(&argv(&["--heading", "--json", "--", "--stdin"])).unwrap();
        assert_eq!(options.path, PathBuf::from("--stdin")); assert!(!options.json);
    }
    #[test]
    fn incompatible_oversized_or_unrelated_options_are_rejected_before_io() {
        for input in [vec!["file", "--line", "0"], vec!["file", "--lines", "0"],
            vec!["file", "--lines", "1025"], vec!["file", "--width", "513"],
            vec!["file", "--max-source-bytes", "262145"], vec!["file", "--line", "2", "--heading", "x"],
            vec!["file", "--copy-start", "1"], vec!["file", "--copy-start", "2", "--copy-end", "1"],
            vec!["file", "--stdin"], vec!["file", "--allow-network"], vec!["file", "--json", "--json"]] {
            assert!(parse(&argv(&input)).is_err(), "{input:?}");
        }
    }
}

#![forbid(unsafe_code)]

//! Application composition, distinct from the inert public fcb engine.
//! `run` is explicit: it receives arguments and I/O handles, performs only the
//! selected service, and returns an exit code without exiting the host process.
//! There is no global runtime, logger, signal handler, cache or worker pool.

pub mod args;
pub mod output;
mod input;
mod services;
mod workspace;
mod whole_file;
mod snapshot;
mod snapshot_diff;
mod trail;
mod symbols;
mod lines;
mod atlas;

use std::{ffi::OsString, io::{Read, Write}};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{ExtentError, ExtentViewError, ExtentQueryError, QueryGeneration,
    ResourceAllocationId, ResourceBudget, IndexError, PathSearchError, StreamReadError, FileSearchError};
use fcb::search::workspace::WorkspaceError;
use args::{Arguments, ArgumentError, Command};
use output::{Output, OutputError, MAX_RESPONSE_BYTES};

pub const SCHEMA: &str = "fcb.cli/1";
pub const EXIT_OK: u8 = 0;
pub const EXIT_NO_MATCH: u8 = 1;
pub const EXIT_ERROR: u8 = 2;
pub const EXIT_PARTIAL: u8 = 3;
pub const EXIT_CANCELED: u8 = 130;
pub const MANAGED_BYTES: u64 = 256 * 1024 * 1024;

/// IDs identify this response's observation, not a durable cross-process
/// namespace. Wire clients must not merge two invocations by these values.
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1).expect("nonzero application owner") }
fn file_id() -> FileId { FileId::new(owner(), 1).expect("nonzero response-local file") }
fn revision() -> SourceRevision { SourceRevision::new(owner(), 1).expect("nonzero observation") }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).expect("nonzero query") }
fn allocation(value: u64) -> ResourceAllocationId { ResourceAllocationId::new(value).expect("nonzero internal allocation") }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppError {
    Argument(ArgumentError), Output(OutputError), Extent(ExtentError),
    View(ExtentViewError), Query(ExtentQueryError), Io, UnsupportedPlatform,
    Symlink, Special, Directory, InputLimit, IoCallLimit, Canceled,
    GuiUnavailable, InvalidRange, Admission, SourceChanged,
    Workspace(WorkspaceError), Index(IndexError), Path(PathSearchError),
    Stream(StreamReadError), FileSearch(FileSearchError),
}
impl AppError {
    pub fn code(self) -> String {
        let code = match self {
            Self::Argument(error) => return error.code().to_owned(),
            Self::Output(error) => return error.code().to_owned(),
            Self::Extent(error) => return error.code().to_owned(),
            Self::View(error) => return error.to_string(), Self::Query(error) => return error.to_string(),
            Self::Workspace(error) => return error.to_string(), Self::Index(error) => return error.to_string(),
            Self::Path(error) => return error.to_string(),
            Self::Stream(error) => return error.to_string(), Self::FileSearch(error) => return error.to_string(),
            Self::Io => "CLI_SOURCE_IO", Self::UnsupportedPlatform => "CLI_NATIVE_FILE_UNSUPPORTED",
            Self::Symlink => "CLI_SYMLINK_REFUSED", Self::Special => "CLI_SPECIAL_OBJECT_REFUSED",
            Self::Directory => "CLI_DIRECTORY_SCOPE_UNAVAILABLE", Self::InputLimit => "CLI_INPUT_LIMIT",
            Self::IoCallLimit => "CLI_IO_CALL_LIMIT", Self::Canceled => "CLI_CANCELED",
            Self::GuiUnavailable => "CLI_NATIVE_GUI_UNAVAILABLE", Self::InvalidRange => "CLI_INVALID_RANGE",
            Self::Admission => "CLI_RESOURCE_DENIED", Self::SourceChanged => "CLI_SOURCE_CHANGED",
        };
        code.to_owned()
    }
    pub const fn subsystem(self) -> &'static str {
        match self {
            Self::Argument(_) => "arguments", Self::Output(_) => "output",
            Self::View(_) => "decoder", Self::Query(_) | Self::Index(_) | Self::Path(_)
                | Self::Stream(_) | Self::FileSearch(_) => "search",
            Self::Workspace(_) => "workspace", Self::GuiUnavailable => "native", _ => "source",
        }
    }
    pub const fn message(self) -> &'static str {
        match self {
            Self::Argument(_) => "The command or its options are not supported in the selected scope.",
            Self::Directory => "This file-scoped command does not authorize directory enumeration; use an explicit --workspace search or inspection.",
            Self::GuiUnavailable => "The native GUI launcher is not implemented in this build.",
            Self::UnsupportedPlatform => "Named-file opening is not implemented for this target ABI; bounded stdin remains available.",
            Self::Symlink => "The selected final path component is a symbolic link.",
            Self::Special => "The selected source is not a regular file.",
            Self::InputLimit => "Input exceeded its admitted size; no truncated input is reported as a whole observation.",
            Self::IoCallLimit => "The admitted read-call budget was exhausted.",
            Self::Canceled => "The operation was canceled before response delivery.",
            Self::SourceChanged => "The source identity changed during admission.",
            Self::Output(_) => "The complete response did not fit its output admission budget.",
            Self::View(_) => "The requested text could not be decoded with the available exact source context.",
            Self::Query(_) | Self::Index(_) | Self::Path(_) | Self::Stream(_) | Self::FileSearch(_) =>
                "The exact query could not complete under its declared source semantics.",
            Self::Workspace(_) => "The workspace operation could not publish its bounded observation.",
            Self::Admission => "Managed resource capacity was refused before publication.",
            Self::InvalidRange => "The requested range or source kind is not valid for this operation.",
            _ => "The explicitly selected source could not be read.",
        }
    }
    pub const fn next_action(self) -> &'static str {
        match self {
            Self::GuiUnavailable => "Use fcb read FILE or fcb open FILE --json for headless reading.",
            Self::View(_) | Self::Query(_) => "Check the declared encoding or select original bytes with search --raw-hex.",
            Self::Directory => "Use fcb inspect ROOT --workspace or fcb search ROOT --workspace --text TEXT.",
            Self::Argument(_) => "Run fcb --help for supported commands and limits.",
            _ => "Check the selected source and limits, then retry explicitly.",
        }
    }
    pub fn is_canceled(self) -> bool {
        matches!(self, Self::Canceled | Self::Extent(ExtentError::Canceled)
            | Self::View(ExtentViewError::Canceled) | Self::Query(ExtentQueryError::Canceled)
            | Self::Workspace(WorkspaceError::Canceled)
            | Self::Workspace(WorkspaceError::Index(IndexError::Canceled))
            | Self::Workspace(WorkspaceError::Source(fcb::source::SourceError::Canceled))
            | Self::Index(IndexError::Canceled) | Self::Path(PathSearchError::Canceled))
    }
    pub const fn retryable(self) -> bool {
        matches!(self, Self::Io | Self::SourceChanged | Self::Admission)
    }
}
impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.code()) }
}
impl std::error::Error for AppError {}
impl From<ArgumentError> for AppError { fn from(error: ArgumentError) -> Self { Self::Argument(error) } }
impl From<OutputError> for AppError { fn from(error: OutputError) -> Self { Self::Output(error) } }
impl From<ExtentError> for AppError { fn from(error: ExtentError) -> Self { Self::Extent(error) } }
impl From<ExtentViewError> for AppError { fn from(error: ExtentViewError) -> Self { Self::View(error) } }
impl From<ExtentQueryError> for AppError { fn from(error: ExtentQueryError) -> Self { Self::Query(error) } }
impl From<WorkspaceError> for AppError { fn from(error: WorkspaceError) -> Self { Self::Workspace(error) } }
impl From<IndexError> for AppError { fn from(error: IndexError) -> Self { Self::Index(error) } }
impl From<PathSearchError> for AppError { fn from(error: PathSearchError) -> Self { Self::Path(error) } }
impl From<StreamReadError> for AppError { fn from(error: StreamReadError) -> Self { Self::Stream(error) } }
impl From<FileSearchError> for AppError { fn from(error: FileSearchError) -> Self { Self::FileSearch(error) } }

/// Ordinary --json writes ONE complete bounded document or one error document.
/// Service failures discard the private partial encoder before writing errors.
/// A broken output pipe cannot be repaired by appending a second JSON document; only a
/// redacted stderr diagnostic follows. No caller process exit or signal occurs.
pub fn run(arguments: &[OsString], stdin: &mut impl Read, stdout: &mut impl Write,
    stderr: &mut impl Write, mut canceled: impl FnMut() -> bool) -> u8 {
    // Atlas plans consume explicit metadata discovery, never implicit stdin.
    if arguments.first().is_some_and(|argument| argument == "atlas") {
        return atlas::run(&arguments[1..], stdout, stderr, canceled);
    }
    // Line navigation uses a forward scan and never reads implicit stdin.
    if lines::requested(arguments) {
        return lines::run(arguments, stdout, stderr, canceled);
    }
    // Candidate navigation remains read-only and never reads implicit stdin.
    if arguments.first().is_some_and(|argument| argument == "symbols") {
        return symbols::run(&arguments[1..], stdout, stderr, canceled);
    }
    // User-state exports have explicit write-aware terminal receipts.
    if arguments.first().is_some_and(|argument| argument == "trail") {
        return trail::run(&arguments[1..], stdout, stderr, canceled);
    }
    // Snapshot save is the explicit source-export route with write-aware
    // terminal receipts. Diff remains a separate read-only operation.
    if arguments.first().is_some_and(|argument| argument == "snapshot") {
        if arguments.get(1).is_some_and(|argument| argument == "diff") {
            return snapshot_diff::run(&arguments[2..], stdout, stderr, canceled);
        }
        return snapshot::run(&arguments[1..], stdout, stderr, canceled);
    }
    let wants_json = args::json_requested(arguments);
    let parsed = args::parse(arguments);
    let budget = match ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)) {
        Ok(budget) => budget,
        Err(_) => { let _ = stderr.write(b"CLI_RESOURCE_DENIED\n"); return EXIT_ERROR; }
    };
    let size = match parsed.as_ref() {
        Ok(args) if args.workspace || matches!(args.command, Command::Read | Command::Open | Command::Search) => MAX_RESPONSE_BYTES,
        _ => 16 * 1024,
    };
    let mut output = match Output::new(owner(), size, &budget, allocation(1)) {
        Ok(output) => output,
        Err(_) => { let _ = stderr.write(b"CLI_OUTPUT_ADMISSION\n"); return EXIT_ERROR; }
    };
    let result = match parsed {
        Err(error) => Err(error.into()),
        Ok(args) => execute(&args, stdin, &mut output, &budget, &mut canceled),
    };
    let result = if canceled() && result.is_ok() { Err(AppError::Canceled) } else { result };
    let exit = match result {
        Ok(exit) => exit,
        Err(error) => {
            output.clear();
            if encode_error(&mut output, wants_json, error).is_err() {
                let _ = stderr.write(b"CLI_ERROR_ENCODING_FAILED\n"); return EXIT_ERROR;
            }
            if error.is_canceled() { EXIT_CANCELED } else { EXIT_ERROR }
        }
    };
    let mut delivery_canceled = false;
    let mut stop = || {
        if exit != EXIT_CANCELED && canceled() { delivery_canceled = true; true } else { false }
    };
    let delivered = if wants_json || matches!(exit, EXIT_OK | EXIT_NO_MATCH | EXIT_PARTIAL) {
        output.deliver(stdout, 4096, &mut stop)
    } else { output.deliver(stderr, 4096, &mut stop) };
    if delivered.is_err() {
        let _ = stderr.write(b"CLI_OUTPUT_INTERRUPTED: response delivery incomplete\n");
        return if delivery_canceled { EXIT_CANCELED } else { EXIT_ERROR };
    }
    exit
}

fn execute(args: &Arguments, stdin: &mut impl Read, output: &mut Output,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    if canceled() { return Err(AppError::Canceled); }
    if args.workspace { return workspace::execute(args, output, budget, canceled); }
    if args.whole_file { return whole_file::single(args, output, budget, canceled); }
    match args.command {
        Command::Help => services::help(args.json, output),
        Command::Capabilities | Command::Doctor => services::capabilities(args, output),
        Command::Launch => Err(AppError::GuiUnavailable),
        Command::Open if !args.json => Err(AppError::GuiUnavailable),
        Command::Inspect => services::inspect(args, output, canceled),
        Command::Open | Command::Read | Command::Search => {
            let loaded = input::load(args, stdin, budget, canceled)?;
            services::source(args, &loaded, output, budget, canceled)
        }
    }
}

fn encode_error(output: &mut Output, json: bool, error: AppError) -> Result<(), OutputError> {
    if !json {
        output.literal(&error.code())?; output.literal(": ")?;
        output.literal(error.message())?; output.literal("\n")?;
        output.literal(error.next_action())?; return output.literal("\n");
    }
    output.literal("{\"schema\":")?; output.quoted(SCHEMA)?;
    output.literal(",\"status\":\"error\",\"complete\":false,\"error\":{\"code\":")?;
    output.quoted(&error.code())?; output.literal(",\"subsystem\":")?; output.quoted(error.subsystem())?;
    output.literal(",\"message\":")?; output.quoted(error.message())?;
    output.literal(",\"retryable\":")?; output.boolean(error.retryable())?;
    output.literal(",\"next_action\":")?; output.quoted(error.next_action())?;
    output.literal("}}\n")
}

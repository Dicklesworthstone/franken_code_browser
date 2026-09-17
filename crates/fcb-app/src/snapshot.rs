#![forbid(unsafe_code)]

//! Explicit source-containing snapshot commands. Archives are never auto-opened,
//! extracted into directories, or interpreted as root grants/configuration.
//! Save creates ONE new destination and never overwrites/removes an existing
//! file. An interrupted destination remains on disk and is reported as such;
//! checksum validation refuses incomplete bytes. This is not atomic rename/DB
//! publication or a qualified power-loss-durability claim.

mod paged;
mod index;

use std::{ffi::OsString, fs::{self, File, OpenOptions}, io::{self, Write}, path::{Path, PathBuf}, sync::Arc};
use fcb::{ByteLength, ByteOffset, ByteRange};
use fcb::source::{CancelFlag, SourceError, NormalizedPath};
use fcb::search::{CaptureRequest, CompleteCapture, ExtentConsistency, ExtentReadState, ExtentStepBudget,
    FileRangeReader, RawPath, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceCaptures, WorkspaceLimits, WorkspaceStage};
use fcb::search::snapshot::{SavedSourceError, SnapshotError, SnapshotLimits, SnapshotView,
    export_workspace, MAX_SNAPSHOT_BYTES};
use crate::{AppError, MANAGED_BYTES, SCHEMA, EXIT_OK, EXIT_NO_MATCH, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED,
    allocation, file_id, owner, revision};
use crate::args::{decimal, MAX_ARGUMENTS, MAX_ARGUMENT_BYTES, MAX_SINGLE_ARGUMENT};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};
use crate::{input, workspace};

const MAX_CALLS: u64 = 131_072;
const HELP: &str = "fcb snapshot save ROOT --output NEW_FILE [--json] [--include-excluded]\n\
fcb snapshot inspect FILE [--json] [--limit N]\n\
fcb snapshot search FILE (--text LITERAL | --raw-hex HEX) [--json] [--limit N]\n\
fcb snapshot read FILE (--member NAME | --member-hex HEX) [--json]\n\
fcb snapshot index help  # Build/reopen pinned substring indexes\n\
Read options: --line N OR --offset N; --bytes N --lines N; --raw for original bytes\n\
Save options: --max-files N --max-file-bytes N --max-total-bytes N\n\
Explicit plaintext source export; no overwrite, no extraction, no restored root grants.\n\
Offline reads/search use one verified member at a time after full archive validation.\n\
Offline completeness describes saved observations, never the current filesystem.\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode { Help, Save, Inspect, Search, Read }
#[derive(Debug)]
struct Settings {
    mode: Mode, source: Option<PathBuf>, output: Option<PathBuf>, text: Option<String>,
    raw_needle: Option<Vec<u8>>, member: Option<Vec<u8>>, offset: u64, line: Option<u64>,
    window_bytes: usize, lines: usize, raw: bool,
    json: bool, limit: usize, limits: WorkspaceLimits, include_excluded: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Effect { None, Incomplete, Written, Synced }
impl Effect {
    fn name(self) -> &'static str {
        match self { Self::None => "none", Self::Incomplete => "destination-created-incomplete",
            Self::Written => "complete-file-sync-unconfirmed", Self::Synced => "complete-file-sync-requested" }
    }
}
#[derive(Debug)]
struct Failure { code: String, canceled: bool }
impl Failure {
    fn new(code: &str) -> Self { Self { code: code.to_owned(), canceled: false } }
    fn canceled() -> Self { Self { code: "SNAPSHOT_CANCELED".to_owned(), canceled: true } }
}
impl From<AppError> for Failure {
    fn from(error: AppError) -> Self { Self { code: error.code(), canceled: error.is_canceled() } }
}
impl From<OutputError> for Failure { fn from(error: OutputError) -> Self { Self::new(error.code()) } }
impl From<SnapshotError> for Failure {
    fn from(error: SnapshotError) -> Self { Self { code: error.code().to_owned(), canceled: error == SnapshotError::Canceled } }
}
impl From<SavedSourceError> for Failure {
    fn from(error: SavedSourceError) -> Self {
        Self { code: error.to_string(), canceled: matches!(error, SavedSourceError::Canceled | SavedSourceError::Format(SnapshotError::Canceled)) }
    }
}

fn takes_value(text: &str) -> bool {
    matches!(text, "--output" | "--text" | "--raw-hex" | "--limit" | "--max-files" | "--max-file-bytes" | "--max-total-bytes"
        | "--member" | "--member-hex" | "--offset" | "--line" | "--bytes" | "--lines")
}
fn wants_json(args: &[OsString]) -> bool {
    let mut cursor = 0;
    while cursor < args.len().min(MAX_ARGUMENTS + 1) {
        let arg = &args[cursor]; cursor += 1;
        if arg == "--" { break; }
        if arg == "--json" { return true; }
        if arg.to_str().is_some_and(takes_value) { cursor += 1; }
    }
    false
}
fn parse(args: &[OsString]) -> Result<Settings, Failure> {
    if args.len() > MAX_ARGUMENTS { return Err(Failure::new("CLI_ARGUMENT_LIMIT")); }
    let mut total = 0usize;
    for arg in args {
        total = total.checked_add(arg.len()).ok_or_else(|| Failure::new("CLI_ARGUMENT_LIMIT"))?;
        if arg.len() > MAX_SINGLE_ARGUMENT || total > MAX_ARGUMENT_BYTES { return Err(Failure::new("CLI_ARGUMENT_LIMIT")); }
    }
    let mode = match args.first().and_then(|arg| arg.to_str()) {
        None | Some("help" | "--help" | "-h") => Mode::Help,
        Some("save") => Mode::Save, Some("inspect") => Mode::Inspect, Some("search") => Mode::Search,
        Some("read") => Mode::Read, _ => return Err(Failure::new("SNAPSHOT_UNKNOWN_COMMAND")),
    };
    let mut settings = Settings { mode, source: None, output: None, text: None, raw_needle: None,
        member: None, offset: 0, line: None, window_bytes: 64 * 1024, lines: 100, raw: false, json: false,
        limit: 100, limits: WorkspaceLimits::default(), include_excluded: false };
    let mut seen = 0u32;
    let mut cursor = usize::from(!args.is_empty());
    let mut positional = false;
    while cursor < args.len() {
        let arg = &args[cursor]; cursor += 1;
        if !positional && arg == "--" { positional = true; continue; }
        let option = if positional { None } else { arg.to_str().filter(|arg| arg.starts_with('-')) };
        if let Some(option) = option {
            let bit = match option { "--json" => 1, "--output" => 2, "--text" => 4,
                "--limit" => 8, "--include-excluded" => 16, "--max-files" => 32,
                "--max-file-bytes" => 64, "--max-total-bytes" => 128,
                "--member" => 256, "--member-hex" => 512, "--offset" => 1024, "--line" => 2048,
                "--bytes" => 4096, "--lines" => 8192, "--raw" => 16384, "--raw-hex" => 32768,
                _ => return Err(Failure::new("CLI_UNKNOWN_OPTION")), };
            if seen & bit != 0 { return Err(Failure::new("CLI_DUPLICATE_OPTION")); }
            seen |= bit;
            if option == "--json" { settings.json = true; continue; }
            if option == "--include-excluded" { settings.include_excluded = true; continue; }
            if option == "--raw" { settings.raw = true; continue; }
            let value = args.get(cursor).ok_or_else(|| Failure::new("CLI_MISSING_VALUE"))?; cursor += 1;
            if option == "--output" {
                if value.is_empty() { return Err(Failure::new("CLI_MISSING_VALUE")); }
                settings.output = Some(PathBuf::from(value)); continue;
            }
            if option == "--member" {
                if value.is_empty() || settings.member.is_some() { return Err(Failure::new("CLI_INVALID_MEMBER")); }
                settings.member = Some(RawPath::from_path(&PathBuf::from(value)).as_bytes().to_vec()); continue;
            }
            let text = value.to_str().ok_or_else(|| Failure::new("CLI_INVALID_VALUE"))?;
            if option == "--text" {
                if text.is_empty() || text.len() > 1024 { return Err(Failure::new("CLI_INVALID_NEEDLE")); }
                settings.text = Some(text.to_owned()); continue;
            }
            if option == "--member-hex" {
                if settings.member.is_some() { return Err(Failure::new("CLI_INVALID_MEMBER")); }
                settings.member = Some(hex(text, 16_384)?); continue;
            }
            if option == "--raw-hex" { settings.raw_needle = Some(hex(text, 1024)?); continue; }
            let number = decimal(text).map_err(|error| Failure::new(error.code()))?;
            match option {
                "--limit" if number <= 4096 => settings.limit = number as usize,
                "--max-files" if (1..=65_536).contains(&number) => settings.limits.max_files = number as usize,
                "--max-file-bytes" if number <= 1024 * 1024 => settings.limits.max_file_bytes = number as usize,
                "--max-total-bytes" if number <= 64 * 1024 * 1024 => settings.limits.max_source_bytes = number as usize,
                "--offset" => settings.offset = number,
                "--line" if number > 0 => settings.line = Some(number),
                "--bytes" if (4..=256 * 1024).contains(&number) => settings.window_bytes = number as usize,
                "--lines" if (1..=4096).contains(&number) => settings.lines = number as usize,
                _ => return Err(Failure::new("CLI_ARGUMENT_LIMIT")),
            }
        } else {
            if settings.source.is_some() || arg.is_empty() { return Err(Failure::new("CLI_MULTIPLE_SOURCES")); }
            settings.source = Some(PathBuf::from(arg));
        }
    }
    let valid = match mode {
        Mode::Help => settings.source.is_none() && seen & !1 == 0,
        Mode::Save => settings.source.is_some() && settings.output.is_some() && seen & !(1 | 2 | 16 | 32 | 64 | 128) == 0,
        Mode::Inspect => settings.source.is_some() && seen & !(1 | 8) == 0,
        Mode::Search => settings.source.is_some() && (settings.text.is_some() != settings.raw_needle.is_some())
            && seen & !(1 | 4 | 8 | 32768) == 0,
        Mode::Read => settings.source.is_some() && settings.member.is_some()
            && seen & !(1 | 256 | 512 | 1024 | 2048 | 4096 | 8192 | 16384) == 0
            && seen & (1024 | 2048) != (1024 | 2048)
            && (!settings.raw || (settings.line.is_none() && seen & 8192 == 0)),
    };
    if !valid { return Err(Failure::new("CLI_INCOMPATIBLE_OPTIONS")); }
    Ok(settings)
}
fn hex(text: &str, maximum: usize) -> Result<Vec<u8>, Failure> {
    if text.is_empty() || text.len() % 2 != 0 || text.len() / 2 > maximum { return Err(Failure::new("CLI_INVALID_HEX")); }
    let nibble = |b: u8| match b { b'0'..=b'9' => Some(b - b'0'), b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10), _ => None };
    text.as_bytes().chunks_exact(2).map(|pair| {
        let a = nibble(pair[0]).ok_or_else(|| Failure::new("CLI_INVALID_HEX"))?;
        let b = nibble(pair[1]).ok_or_else(|| Failure::new("CLI_INVALID_HEX"))?; Ok((a << 4) | b)
    }).collect()
}

/// Separate write-aware delivery: cancellation after a complete synchronized
/// save never changes its outcome into "nothing happened". Error receipts keep
/// the effect state; a broken stdout leaves a redacted effect marker on stderr.
pub(crate) fn run(args: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write,
    mut canceled: impl FnMut() -> bool) -> u8 {
    if args.first().is_some_and(|arg| arg == "index") {
        return index::run(&args[1..], stdout, stderr, canceled);
    }
    let json = wants_json(args);
    let budget = match ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)) {
        Ok(budget) => budget, Err(_) => { let _ = stderr.write(b"SNAPSHOT_RESOURCE_DENIED\n"); return EXIT_ERROR; }
    };
    let mut out = match Output::new(owner(), MAX_RESPONSE_BYTES, &budget, allocation(100)) {
        Ok(out) => out, Err(_) => { let _ = stderr.write(b"SNAPSHOT_OUTPUT_DENIED\n"); return EXIT_ERROR; }
    };
    let mut effect = Effect::None;
    let result = parse(args).and_then(|settings| execute(&settings, &mut out, &budget, &mut effect, &mut canceled));
    let result = if result.is_ok() && effect == Effect::None && canceled() { Err(Failure::canceled()) } else { result };
    let exit = match result {
        Ok(exit) => exit,
        Err(error) => {
            out.clear();
            if failure_output(&mut out, json, &error, effect).is_err() {
                let _ = stderr.write(b"SNAPSHOT_ERROR_ENCODING_FAILED\n"); return EXIT_ERROR;
            }
            if error.canceled { EXIT_CANCELED } else { EXIT_ERROR }
        }
    };
    let mut interrupted = false;
    let mut stop = || {
        let stop = effect == Effect::None && exit != EXIT_CANCELED && canceled();
        interrupted |= stop; stop
    };
    let delivered = if json || matches!(exit, EXIT_OK | EXIT_NO_MATCH | EXIT_PARTIAL) {
        out.deliver(stdout, 4096, &mut stop)
    } else { out.deliver(stderr, 4096, &mut stop) };
    if delivered.is_err() {
        let _ = writeln!(stderr, "SNAPSHOT_RESPONSE_INCOMPLETE effect={}", effect.name());
        return if interrupted { EXIT_CANCELED } else { EXIT_ERROR };
    }
    exit
}
fn failure_output(out: &mut Output, json: bool, error: &Failure, effect: Effect) -> Result<(), OutputError> {
    if !json {
        out.literal(&error.code)?; out.literal("; effect=")?; out.literal(effect.name())?;
        return out.literal("\nUse fcb snapshot help. Existing destinations are never overwritten or removed.\n");
    }
    out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
    out.literal(",\"status\":\"error\",\"complete\":false,\"effect\":")?; out.quoted(effect.name())?;
    out.literal(",\"error\":{\"code\":")?; out.quoted(&error.code)?;
    out.literal(",\"subsystem\":\"snapshot\",\"retryable\":false,\"next_action\":\"Inspect any reported destination before an explicit retry; do not overwrite source.\"}}\n")
}
fn execute(settings: &Settings, out: &mut Output, budget: &ResourceBudget, effect: &mut Effect,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    if settings.mode == Mode::Help {
        if settings.json { begin(out, "snapshot-help")?; out.literal(",\"text\":")?; out.quoted(HELP)?; out.literal("}\n")?; }
        else { out.literal(HELP)?; }
        return Ok(EXIT_OK);
    }
    if settings.mode == Mode::Save { return save(settings, out, budget, effect, canceled); }
    paged::execute(settings, out, budget, canceled)
}
fn begin(out: &mut Output, command: &str) -> Result<(), OutputError> {
    out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
    out.literal(",\"status\":\"ok\",\"command\":")?; out.quoted(command)
}

fn save(settings: &Settings, out: &mut Output, budget: &ResourceBudget, effect: &mut Effect,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
    let requested = input::absolute(settings.source.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?)?;
    let metadata = fs::symlink_metadata(&requested).map_err(|_| Failure::new("SNAPSHOT_ROOT_UNAVAILABLE"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() { return Err(Failure::new("SNAPSHOT_ROOT_NOT_DIRECTORY")); }
    let root = fs::canonicalize(requested).map_err(|_| Failure::new("SNAPSHOT_ROOT_UNAVAILABLE"))?;
    let destination = input::absolute(settings.output.as_deref().ok_or_else(|| Failure::new("SNAPSHOT_OUTPUT_REQUIRED"))?)?;
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(Failure::new("SNAPSHOT_DESTINATION_EXISTS")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        Err(_) => return Err(Failure::new("SNAPSHOT_DESTINATION_UNAVAILABLE")),
    }
    let grant = RootGrant::new(RootId::new(owner(), 1).map_err(|_| Failure::new("SNAPSHOT_OWNER"))?, RawPath::from_path(&root));
    let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).map_err(AppError::from)?,
        file_id(), settings.limits, settings.include_excluded, budget, allocation(101)).map_err(AppError::from)?;
    let cancel = CancelFlag::new();
    while catalog.stage() == WorkspaceStage::Discovering {
        if canceled() { return Err(Failure::canceled()); }
        catalog.step(&cancel).map_err(AppError::from)?;
    }
    let mut captures = WorkspaceCaptures::new(&catalog, revision(), budget, allocation(102)).map_err(AppError::from)?;
    let mut io = IoCounts::default();
    while !captures.finished() {
        if canceled() { return Err(Failure::canceled()); }
        captures.step(&cancel, |request, path, limit| capture(&root, request, path, limit,
            settings.limits.max_source_bytes as u64, &mut io, budget, canceled)).map_err(AppError::from)?;
    }
    let encoded = export_workspace(&captures, SnapshotLimits::default(), budget,
        [allocation(103), allocation(104)], &mut *canceled)?;
    let view = SnapshotView::open(encoded.bytes(), SnapshotLimits::default(), &mut *canceled)?;
    let complete = view.discovery_complete() && view.captured_files() == view.len();
    catalog.validate_active().map_err(AppError::from)?;
    write_new(&destination, encoded.bytes(), effect, canceled)?;
    if settings.json {
        begin(out, "snapshot-save")?; out.literal(",\"effect\":")?; out.quoted(effect.name())?;
        out.literal(",\"destination\":")?; out.path(&destination)?;
        summary(out, view, true)?;
        out.literal(",\"payload_bytes_read\":")?; out.integer(io.bytes)?;
        out.literal(",\"read_calls\":")?; out.integer(io.calls)?;
        out.literal(",\"archive_bytes\":")?; out.integer(encoded.bytes().len() as u64)?;
        out.literal(",\"source_contains_plaintext\":true,\"power_loss_qualified\":false}\n")?;
    } else {
        out.literal("Saved source-containing snapshot ")?; out.path(&destination)?;
        out.literal(if complete { "\nComplete observed membership and captures.\n" } else { "\nPARTIAL saved scope: discovery or captures are incomplete.\n" })?;
        out.literal("No overwrite; sync requested; no power-loss durability qualification.\n")?;
    }
    Ok(if complete { EXIT_OK } else { EXIT_PARTIAL })
}
#[derive(Default)]
struct IoCounts { bytes: u64, calls: u64 }
fn capture(root: &Path, request: CaptureRequest, path: &NormalizedPath, limit: usize,
    total_limit: u64, io: &mut IoCounts, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
    -> Result<CompleteCapture, SourceError> {
    if canceled() { return Err(SourceError::Canceled); }
    let native = workspace::checked_source_path(root, path)?;
    let (file, metadata) = input::open_regular(&native).map_err(|_| SourceError::CaptureUnavailable)?;
    if metadata.len() > limit as u64 || metadata.len() > total_limit.saturating_sub(io.bytes) { return Err(SourceError::PayloadTooLarge); }
    let length = ByteLength::new(metadata.len());
    let mut selected = request;
    if metadata.len() > 0 { selected = selected.with_range(ByteRange::new(ByteOffset::new(0), ByteOffset::new(metadata.len()))
        .map_err(|_| SourceError::InvalidRange)?)?; }
    let mut reader = FileRangeReader::new(request.file(), file).map_err(|_| SourceError::CaptureUnavailable)?;
    let mut read = reader.begin(selected, budget, allocation(109)).map_err(|_| SourceError::CaptureUnavailable)?;
    while read.state() == ExtentReadState::Pending {
        if canceled() { return Err(SourceError::Canceled); }
        if io.calls >= MAX_CALLS { return Err(SourceError::CaptureUnavailable); }
        let before = read.stats();
        let status = read.step(ExtentStepBudget { max_bytes: 64 * 1024, max_calls: (MAX_CALLS - io.calls).min(32) as usize }, &mut *canceled);
        let after = read.stats(); io.bytes += after.bytes_read - before.bytes_read; io.calls += after.read_calls - before.read_calls;
        status.map_err(|_| if canceled() { SourceError::Canceled } else { SourceError::CaptureUnavailable })?;
    }
    let extent = read.finish(&mut *canceled).map_err(|_| if canceled() { SourceError::Canceled } else { SourceError::CaptureUnavailable })?;
    if !extent.request_filled() || !extent.covers_whole_observation() || extent.final_length() != Some(length)
        || extent.consistency() != ExtentConsistency::UnchangedMetadata { return Err(SourceError::MetadataMismatch); }
    CompleteCapture::new(request, length, Arc::from(extent.bytes()))
}
fn write_new(path: &Path, bytes: &[u8], effect: &mut Effect, canceled: &mut impl FnMut() -> bool) -> Result<(), Failure> {
    if canceled() { return Err(Failure::canceled()); }
    if bytes.len() > MAX_SNAPSHOT_BYTES { return Err(Failure::new("SNAPSHOT_LIMIT")); }
    let mut options = OpenOptions::new(); options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut file = options.open(path).map_err(|error| Failure::new(if error.kind() == io::ErrorKind::AlreadyExists {
        "SNAPSHOT_DESTINATION_EXISTS" } else { "SNAPSHOT_CREATE_FAILED" }))?;
    *effect = Effect::Incomplete;
    let mut offset = 0;
    let mut calls = 0;
    while offset < bytes.len() {
        if canceled() { return Err(Failure::canceled()); }
        if calls == MAX_CALLS { return Err(Failure::new("SNAPSHOT_WRITE_CALL_LIMIT")); }
        calls += 1;
        let end = bytes.len().min(offset + 64 * 1024);
        match file.write(&bytes[offset..end]) {
            Ok(0) => return Err(Failure::new("SNAPSHOT_WRITE_FAILED")),
            Ok(count) if count <= end - offset => offset += count,
            Ok(_) => return Err(Failure::new("SNAPSHOT_WRITE_FAILED")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => return Err(Failure::new("SNAPSHOT_WRITE_FAILED")),
        }
    }
    *effect = Effect::Written;
    if canceled() { return Err(Failure::canceled()); }
    file.sync_all().map_err(|_| Failure::new("SNAPSHOT_SYNC_UNCERTAIN"))?;
    let parent = path.parent().ok_or_else(|| Failure::new("SNAPSHOT_SYNC_UNCERTAIN"))?;
    File::open(parent).and_then(|parent| parent.sync_all()).map_err(|_| Failure::new("SNAPSHOT_SYNC_UNCERTAIN"))?;
    *effect = Effect::Synced;
    Ok(())
}
fn summary(out: &mut Output, view: SnapshotView<'_>, live_roots_accessed: bool) -> Result<(), OutputError> {
    out.literal(",\"source_scope\":\"saved-observations-only\",\"live_roots_accessed\":")?;
    out.boolean(live_roots_accessed)?;
    out.literal(",\"snapshot_digest\":")?; out.quoted(&view.digest().to_hex())?;
    out.literal(",\"policy\":")?; out.quoted(view.policy())?;
    out.literal(",\"discovery_complete\":")?; out.boolean(view.discovery_complete())?;
    out.literal(",\"known_files\":")?; out.integer(view.len() as u64)?;
    out.literal(",\"captured_files\":")?; out.integer(view.captured_files() as u64)?;
    out.literal(",\"unavailable_files_count\":")?; out.integer((view.len() - view.captured_files()) as u64)?;
    out.literal(",\"captured_bytes\":")?; out.integer(view.source_bytes() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
    #[test]
    fn save_needs_explicit_destination_and_only_accepts_capture_options() {
        assert!(parse(&args(&["save", "root"])).is_err());
        assert!(parse(&args(&["save", "root", "--output", "new", "--text", "secret"])).is_err());
        let parsed = parse(&args(&["save", "root", "--output", "new", "--max-total-bytes", "0"])).unwrap();
        assert_eq!(parsed.limits.max_source_bytes, 0);
    }
    #[test]
    fn option_values_and_delimited_native_paths_cannot_change_output_mode() {
        assert!(!wants_json(&args(&["save", "root", "--output", "--json"])));
        assert!(!wants_json(&args(&["search", "saved", "--text", "--json"])));
        assert!(wants_json(&args(&["search", "saved", "--text", "--json", "--json"])));
        assert!(!wants_json(&args(&["read", "saved", "--member", "--json"])));
        assert_eq!(parse(&args(&["inspect", "--", "--json"])).unwrap().source.unwrap(), PathBuf::from("--json"));
    }
    #[test]
    fn offline_routes_reject_live_scope_flags_and_oversized_limits() {
        for values in [vec!["search", "saved", "--text", "x", "--include-excluded"],
            vec!["inspect", "saved", "--output", "other"], vec!["save", "root", "--output", "x", "--max-files", "65537"],
            vec!["inspect", "saved", "--limit", "4097"], vec!["search", "saved", "--text", "x", "--text", "y"]] {
            assert!(parse(&args(&values)).is_err());
        }
    }
    #[test]
    fn reading_options_preserve_raw_identity_and_reject_ambiguous_coordinates() {
        let options = parse(&args(&["read", "saved", "--member-hex", "61ff", "--line", "2", "--bytes", "16"])).unwrap();
        assert_eq!(options.member, Some(vec![b'a', 0xff])); assert_eq!(options.line, Some(2));
        for values in [vec!["read", "saved"], vec!["read", "saved", "--member", "a", "--line", "0"],
            vec!["read", "saved", "--member", "a", "--line", "2", "--offset", "0"],
            vec!["read", "saved", "--member", "a", "--member-hex", "61"],
            vec!["read", "saved", "--member", "a", "--raw", "--line", "2"],
            vec!["read", "saved", "--member", "a", "--bytes", "3"],
            vec!["search", "saved", "--text", "x", "--raw-hex", "78"]] {
            assert!(parse(&args(&values)).is_err());
        }
    }
}

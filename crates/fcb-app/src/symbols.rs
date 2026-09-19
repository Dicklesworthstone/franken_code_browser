#![forbid(unsafe_code)]

//! Explicit source-symbol candidate navigation, not compiler name resolution.
//! Reuses the analysis, bounded discovery, extent capture and paged archive
//! engines. One file and one candidate collection reside at a time; no parser, runtime, shell,
//! Git, language server, source execution or remote lookup is introduced.

use std::{ffi::OsString, fs, io::Write, mem::size_of, path::{Path, PathBuf}};
use fcb::{ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb::source::CancelFlag;
use fcb::search::{CaptureRequest, DetectedEncoding, ExtentConsistency, ExtentReadState,
    ExtentStepBudget, FileRangeReader, ObservedExtent, RawPath, ResourceBudget, RootId, SearchManifestId};
use fcb::search::symbols::{CapturedSymbols, SymbolError, SymbolLanguage, SymbolNameMode,
    MAX_SYMBOL_ITEMS, MAX_SYMBOL_SOURCE_BYTES, MAX_REFERENCE_SOURCE_BYTES, validate_reference_name};
use fcb::search::paged_snapshot::{PagedMemberData, PagedSnapshot};
use fcb::search::snapshot::SnapshotLimits;
use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceLimits, WorkspaceStage};
use fcb_core::ResourceLease;
use crate::{AppError, SCHEMA, MANAGED_BYTES, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_ERROR,
    EXIT_CANCELED, allocation, file_id, generation, owner};
use crate::args::{decimal, MAX_ARGUMENTS, MAX_ARGUMENT_BYTES, MAX_SINGLE_ARGUMENT};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};
use crate::{input, workspace};

mod references;

const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_READ_CALLS: u64 = 131_072;
const MAX_NOTICES: usize = 64;
const HELP: &str = "fcb symbols FILE [--name TEXT] [--match exact|prefix|contains] [--json]\n\
fcb symbols ROOT --workspace [--name TEXT] [--json]\n\
fcb symbols SAVED.fcbs --snapshot [--member PATH | --member-hex HEX] [--name TEXT] [--json]\n\
Reference candidates: add --references --name IDENTIFIER to any scope above.\n\
Options: --language rust|python|javascript|typescript|go|cpp\n\
         --encoding utf8|utf16le|utf16be --limit N --max-files N --max-bytes N\n\
         --include-excluded (workspace only)\n\
Lists declaration candidates from bounded outline recognition, NOT compiler definitions.\n\
Default name matching is exact and case-sensitive. Omit --name to list the outline.\n\
--references searches exact whole tokens in all admitted text files (512 KiB/file).\n\
Comments and string literals participate; these are NOT compiler-resolved references.\n\
ASCII letters/digits/_/$ and conservative non-ASCII runs form reference tokens.\n\
--match prefix/contains is incompatible with --references; --language only labels it.\n\
No source execution, language server, network, live-root lookup for archives, or writes.\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope { File, Workspace, Snapshot }
impl Scope { fn name(self) -> &'static str { match self { Self::File => "file", Self::Workspace => "workspace", Self::Snapshot => "saved-observations" } } }
#[derive(Debug)]
struct Settings {
    source: Option<PathBuf>, scope: Scope, name: Option<String>, mode: SymbolNameMode,
    language: Option<SymbolLanguage>, encoding: Option<DetectedEncoding>, member: Option<RawPath>,
    limit: usize, max_files: usize, max_bytes: u64, include_excluded: bool, json: bool, help: bool,
    references: bool,
}
#[derive(Debug)]
struct Failure { code: String, canceled: bool }
impl Failure {
    fn new(code: &str) -> Self { Self { code: code.to_owned(), canceled: false } }
    fn canceled() -> Self { Self { code: "SYMBOL_CANCELED".to_owned(), canceled: true } }
}
impl From<AppError> for Failure {
    fn from(error: AppError) -> Self { Self { code: error.code(), canceled: error.is_canceled() } }
}
impl From<OutputError> for Failure { fn from(error: OutputError) -> Self { Self::new(error.code()) } }
fn takes_value(flag: &str) -> bool {
    matches!(flag, "--name" | "--match" | "--language" | "--encoding" | "--member" | "--member-hex" | "--limit" | "--max-files" | "--max-bytes")
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
    let mut settings = Settings { source: None, scope: Scope::File, name: None, mode: SymbolNameMode::Exact,
        language: None, encoding: None, member: None, limit: 100, max_files: 4096, max_bytes: 8 * 1024 * 1024,
        include_excluded: false, json: false, help: args.is_empty(), references: false };
    let mut seen = 0u32;
    let mut cursor = 0;
    let mut positional = false;
    while cursor < args.len() {
        let arg = &args[cursor]; cursor += 1;
        if !positional && arg == "--" { positional = true; continue; }
        let flag = if positional { None } else { arg.to_str().filter(|text| text.starts_with('-')) };
        if let Some(flag) = flag {
            let bit = match flag {
                "--json" => 1, "--workspace" | "--snapshot" => 2, "--name" => 4, "--match" => 8,
                "--language" => 16, "--encoding" => 32, "--member" | "--member-hex" => 64,
                "--limit" => 128, "--max-files" => 256, "--max-bytes" => 512,
                "--include-excluded" => 1024, "--help" | "-h" => 2048, "--references" => 4096,
                _ => return Err(Failure::new("CLI_UNKNOWN_OPTION")),
            };
            if seen & bit != 0 { return Err(Failure::new("CLI_DUPLICATE_OPTION")); }
            seen |= bit;
            match flag {
                "--json" => { settings.json = true; continue; }
                "--references" => { settings.references = true; continue; }
                "--workspace" => { settings.scope = Scope::Workspace; continue; }
                "--snapshot" => { settings.scope = Scope::Snapshot; continue; }
                "--include-excluded" => { settings.include_excluded = true; continue; }
                "--help" | "-h" => { settings.help = true; continue; }
                _ => {}
            }
            let value = args.get(cursor).ok_or_else(|| Failure::new("CLI_MISSING_VALUE"))?; cursor += 1;
            if flag == "--member" {
                if value.is_empty() { return Err(Failure::new("CLI_INVALID_VALUE")); }
                settings.member = Some(RawPath::from_path(Path::new(value))); continue;
            }
            let text = value.to_str().ok_or_else(|| Failure::new("CLI_INVALID_VALUE"))?;
            match flag {
                "--name" => {
                    if text.is_empty() || text.len() > 1024 { return Err(Failure::new("CLI_INVALID_NEEDLE")); }
                    settings.name = Some(text.to_owned());
                }
                "--match" => settings.mode = match text { "exact" => SymbolNameMode::Exact, "prefix" => SymbolNameMode::Prefix,
                    "contains" => SymbolNameMode::Contains, _ => return Err(Failure::new("CLI_INVALID_VALUE")) },
                "--language" => settings.language = Some(SymbolLanguage::from_name(text).ok_or_else(|| Failure::new("SYMBOL_UNSUPPORTED_LANGUAGE"))?),
                "--encoding" => settings.encoding = Some(match text { "utf8" => DetectedEncoding::Utf8 { has_bom: true },
                    "utf16le" => DetectedEncoding::Utf16Le, "utf16be" => DetectedEncoding::Utf16Be,
                    _ => return Err(Failure::new("CLI_INVALID_ENCODING")) }),
                "--member-hex" => settings.member = Some(member_hex(text)?),
                _ => {
                    let n = decimal(text).map_err(|error| Failure::new(error.code()))?;
                    match flag {
                        "--limit" if n <= MAX_SYMBOL_ITEMS as u64 => settings.limit = n as usize,
                        "--max-files" if (1..=65_536).contains(&n) => settings.max_files = n as usize,
                        "--max-bytes" if n <= MAX_TOTAL_BYTES => settings.max_bytes = n,
                        _ => return Err(Failure::new("CLI_ARGUMENT_LIMIT")),
                    }
                }
            }
        } else {
            if settings.source.is_some() || arg.is_empty() { return Err(Failure::new("CLI_MULTIPLE_SOURCES")); }
            settings.source = Some(PathBuf::from(arg));
        }
    }
    if settings.help {
        if settings.source.is_some() || seen & !(1 | 2048) != 0 { return Err(Failure::new("CLI_INCOMPATIBLE_OPTIONS")); }
    } else if settings.source.is_none() || (settings.member.is_some() && settings.scope != Scope::Snapshot)
        || (settings.include_excluded && settings.scope != Scope::Workspace)
        || (seen & 8 != 0 && settings.name.is_none()) {
        return Err(Failure::new("CLI_INCOMPATIBLE_OPTIONS"));
    }
    if settings.references {
        if settings.mode != SymbolNameMode::Exact { return Err(Failure::new("CLI_INCOMPATIBLE_OPTIONS")); }
        let name = settings.name.as_deref().ok_or_else(|| Failure::new("REFERENCE_INVALID_NAME"))?;
        validate_reference_name(name).map_err(|error| Failure::new(error.code()))?;
    }
    Ok(settings)
}
fn member_hex(text: &str) -> Result<RawPath, Failure> {
    if text.is_empty() || text.len() % 2 != 0 || text.len() > 32_768 { return Err(Failure::new("CLI_INVALID_HEX")); }
    let mut bytes = Vec::new(); bytes.try_reserve_exact(text.len() / 2).map_err(|_| Failure::new("SYMBOL_RESOURCE_DENIED"))?;
    let nibble = |b: u8| match b { b'0'..=b'9' => Some(b - b'0'), b'a'..=b'f' => Some(b - b'a' + 10), b'A'..=b'F' => Some(b - b'A' + 10), _ => None };
    for pair in text.as_bytes().chunks_exact(2) {
        let high = nibble(pair[0]).ok_or_else(|| Failure::new("CLI_INVALID_HEX"))?;
        let low = nibble(pair[1]).ok_or_else(|| Failure::new("CLI_INVALID_HEX"))?;
        bytes.push(high * 16 + low);
    }
    Ok(RawPath::from_bytes(bytes))
}

pub(crate) fn run(args: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write,
    mut canceled: impl FnMut() -> bool) -> u8 {
    let json = wants_json(args);
    let budget = match ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)) {
        Ok(budget) => budget, Err(_) => { let _ = stderr.write(b"SYMBOL_RESOURCE_DENIED\n"); return EXIT_ERROR; }
    };
    let mut out = match Output::new(owner(), MAX_RESPONSE_BYTES, &budget, allocation(200)) {
        Ok(out) => out, Err(_) => { let _ = stderr.write(b"CLI_OUTPUT_ADMISSION\n"); return EXIT_ERROR; }
    };
    let result = parse(args).and_then(|settings| execute(&settings, &mut out, &budget, &mut canceled));
    let result = if canceled() && result.is_ok() { Err(Failure::canceled()) } else { result };
    let exit = match result {
        Ok(exit) => exit,
        Err(error) => {
            out.clear();
            let encoded = if json { (|| {
                out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
                out.literal(",\"status\":\"error\",\"complete\":false,\"error\":{\"code\":")?; out.quoted(&error.code)?;
                out.literal(",\"subsystem\":\"symbols\",\"retryable\":false,\"next_action\":\"Use fcb symbols --help; ordinary source reading remains available.\"}}\n")
            })() } else { out.literal(&format!("{}\nUse fcb symbols --help.\n", error.code)) };
            if encoded.is_err() { let _ = stderr.write(b"CLI_ERROR_ENCODING_FAILED\n"); return EXIT_ERROR; }
            if error.canceled { EXIT_CANCELED } else { EXIT_ERROR }
        }
    };
    let mut stopped = false;
    let mut stop = || { let value = exit != EXIT_CANCELED && canceled(); stopped |= value; value };
    let delivered = if json || matches!(exit, EXIT_OK | EXIT_NO_MATCH | EXIT_PARTIAL) {
        out.deliver(stdout, 4096, &mut stop)
    } else { out.deliver(stderr, 4096, &mut stop) };
    if delivered.is_err() {
        let _ = stderr.write(b"CLI_OUTPUT_INTERRUPTED: symbol response incomplete\n");
        return if stopped { EXIT_CANCELED } else { EXIT_ERROR };
    }
    exit
}

#[derive(Clone, Copy, Debug)]
enum AnalysisRoute { Outline(SymbolLanguage), References(Option<SymbolLanguage>) }

struct Notice { file: FileId, path: RawPath, code: &'static str }
#[derive(Default)]
struct Stats {
    known: usize, visited: usize, analyzed: usize, unsupported: usize, unavailable: usize,
    refused: usize, limited: usize, unrecognized: usize, matches: usize, emitted: usize,
    analysis_bytes: u64, read_bytes: u64, read_calls: u64, validation_bytes: u64,
    peak_source: usize, notices_omitted: usize,
}
struct RunState { stats: Stats, notices: Vec<Notice>, _lease: ResourceLease }
impl RunState {
    fn new(budget: &ResourceBudget) -> Result<Self, Failure> {
        let charge = MAX_NOTICES * (16_384 + size_of::<Notice>() + 32);
        let lease = budget.try_reserve_managed(owner(), allocation(201), ByteLength::new(charge as u64))
            .map_err(|_| Failure::new("SYMBOL_RESOURCE_DENIED"))?;
        let mut notices = Vec::new(); notices.try_reserve_exact(MAX_NOTICES).map_err(|_| Failure::new("SYMBOL_RESOURCE_DENIED"))?;
        if notices.capacity() > MAX_NOTICES { return Err(Failure::new("SYMBOL_RESOURCE_DENIED")); }
        Ok(Self { stats: Stats::default(), notices, _lease: lease })
    }
    fn notice(&mut self, file: FileId, path: &[u8], code: &'static str) {
        if self.notices.len() == MAX_NOTICES || path.len() > 16_384 { self.stats.notices_omitted += 1; return; }
        self.notices.push(Notice { file, path: RawPath::from_bytes(path), code });
    }
    fn admit(&mut self, settings: &Settings, file: FileId, path: &[u8], size: u64) -> Option<AnalysisRoute> {
        let language = settings.language.or_else(|| SymbolLanguage::from_path(path));
        let route = if settings.references { AnalysisRoute::References(language) }
            else if let Some(language) = language { AnalysisRoute::Outline(language) }
            else { self.stats.unsupported += 1; self.notice(file, path, "SYMBOL_UNSUPPORTED_LANGUAGE"); return None; };
        let (max_source, limit_code) = if settings.references { (MAX_REFERENCE_SOURCE_BYTES, "REFERENCE_SOURCE_LIMIT") }
            else { (MAX_SYMBOL_SOURCE_BYTES, "SYMBOL_SOURCE_LIMIT") };
        let reason = if size > max_source as u64 { Some(limit_code) }
            else if size > settings.max_bytes.saturating_sub(self.stats.read_bytes) { Some("SYMBOL_TOTAL_SOURCE_LIMIT") }
            else if self.stats.read_calls >= MAX_READ_CALLS { Some("SYMBOL_READ_CALL_LIMIT") } else { None };
        if let Some(reason) = reason { self.stats.refused += 1; self.notice(file, path, reason); return None; }
        Some(route)
    }
}
fn execute(settings: &Settings, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    if settings.help {
        if settings.json { out.literal("{\"schema\":")?; out.quoted(SCHEMA)?; out.literal(",\"status\":\"ok\",\"command\":\"symbols-help\",\"text\":")?; out.quoted(HELP)?; out.literal("}\n")?; }
        else { out.literal(HELP)?; }
        return Ok(EXIT_OK);
    }
    let mut state = RunState::new(budget)?;
    if settings.json {
        out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
        out.literal(",\"status\":\"ok\",\"command\":\"symbols\",\"scope\":")?; out.quoted(settings.scope.name())?;
        if settings.references {
            out.literal(",\"operation\":\"references\",\"identity_scope\":\"response-local-file-and-occurrence\",\"evidence\":\"whole-token-text-candidate\",\"compiler_resolved\":false,\"reference_inventory_complete\":false,\"token_policy\":\"conservative-unicode-whole-token-v1\",\"comments_and_literals_included\":true,\"language_policy\":\"all-admitted-text-files\",\"line_coordinates\":\"lf-delimited-decoded-lines\",\"live_roots_accessed\":")?;
        } else {
            out.literal(",\"identity_scope\":\"response-local-file-and-outline\",\"evidence\":\"heuristic-outline-candidate\",\"compiler_resolved\":false,\"symbol_inventory_complete\":false,\"line_coordinates\":\"lf-delimited-decoded-lines\",\"live_roots_accessed\":")?;
        }
        out.boolean(settings.scope != Scope::Snapshot)?;
        out.literal(",\"name_query\":")?;
        match &settings.name { Some(name) => out.quoted(name)?, None => out.literal("null")? }
        out.literal(",\"name_match\":")?;
        out.quoted(match settings.mode { SymbolNameMode::Exact => "exact", SymbolNameMode::Prefix => "prefix", SymbolNameMode::Contains => "contains" })?;
        out.literal(",\"candidates\":[")?;
    } else {
        out.literal(if settings.references {
            "Whole-token text candidates, including comments/literals; NOT compiler-resolved references.\n"
        } else { "Declaration candidates only; not compiler definitions or a complete symbol inventory.\n" })?;
    }
    let source = settings.source.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?;
    let discovery_complete = match settings.scope {
        Scope::File => {
            let native = input::absolute(source)?;
            state.stats.known = 1; state.stats.visited = 1;
            let raw = RawPath::from_path(&native);
            let (file, meta) = input::open_regular(&native)?;
            if let Some(language) = state.admit(settings, file_id(), raw.as_bytes(), meta.len()) {
                let request = request(0)?;
                let extent = read_file(file, meta.len(), request, &mut state, budget, canceled)?;
                analyze(settings, request, raw.as_bytes(), language, extent.bytes(), &mut state, out, budget, canceled)?;
            }
            true
        }
        Scope::Workspace => live_workspace(settings, source, &mut state, out, budget, canceled)?,
        Scope::Snapshot => saved_workspace(settings, source, &mut state, out, budget, canceled)?,
    };
    finish(settings, discovery_complete, state, out)
}
fn request(ordinal: usize) -> Result<CaptureRequest, Failure> {
    let id = u64::try_from(ordinal).ok().and_then(|id| id.checked_add(1)).ok_or_else(|| Failure::new("SYMBOL_IDENTITY_EXHAUSTED"))?;
    CaptureRequest::new(FileId::new(owner(), id).map_err(|_| Failure::new("SYMBOL_IDENTITY_EXHAUSTED"))?,
        SourceRevision::new(owner(), id).map_err(|_| Failure::new("SYMBOL_IDENTITY_EXHAUSTED"))?)
        .map_err(|_| Failure::new("SYMBOL_OWNER_MISMATCH"))
}
fn read_file(file: fs::File, size: u64, request: CaptureRequest, state: &mut RunState,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<ObservedExtent, Failure> {
    let mut reader = FileRangeReader::new(request.file(), file).map_err(AppError::from)?;
    let mut range_request = request;
    if size > 0 { range_request = request.with_range(ByteRange::new(ByteOffset::new(0), ByteOffset::new(size))
        .map_err(|_| Failure::new("SYMBOL_INVALID_RANGE"))?).map_err(|_| Failure::new("SYMBOL_INVALID_RANGE"))?; }
    let mut read = reader.begin(range_request, budget, allocation(203)).map_err(AppError::from)?;
    while read.state() == ExtentReadState::Pending {
        if canceled() { return Err(Failure::canceled()); }
        if state.stats.read_calls >= MAX_READ_CALLS { return Err(Failure::new("SYMBOL_READ_CALL_LIMIT")); }
        let before = read.stats();
        let result = read.step(ExtentStepBudget { max_bytes: 64 * 1024,
            max_calls: (MAX_READ_CALLS - state.stats.read_calls).min(32) as usize }, &mut *canceled);
        let after = read.stats();
        state.stats.read_bytes += after.bytes_read - before.bytes_read;
        state.stats.read_calls += after.read_calls - before.read_calls;
        result.map_err(AppError::from)?;
    }
    let extent = read.finish(&mut *canceled).map_err(AppError::from)?;
    if !extent.request_filled() || !extent.covers_whole_observation() || extent.final_length() != Some(ByteLength::new(size))
        || extent.consistency() != ExtentConsistency::UnchangedMetadata { return Err(Failure::new("SYMBOL_SOURCE_CHANGED")); }
    state.stats.peak_source = state.stats.peak_source.max(extent.bytes().len());
    Ok(extent)
}
fn live_workspace(settings: &Settings, source: &Path, state: &mut RunState, out: &mut Output,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<bool, Failure> {
    if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
    let requested = input::absolute(source)?;
    let meta = fs::symlink_metadata(&requested).map_err(|_| Failure::new("CLI_SOURCE_IO"))?;
    if meta.file_type().is_symlink() || !meta.is_dir() { return Err(Failure::new("SYMBOL_ROOT_NOT_DIRECTORY")); }
    let root = fs::canonicalize(requested).map_err(|_| Failure::new("CLI_SOURCE_IO"))?;
    let grant = RootGrant::new(RootId::new(owner(), 1).map_err(|_| Failure::new("SYMBOL_OWNER_MISMATCH"))?, RawPath::from_path(&root));
    let limits = WorkspaceLimits { max_files: settings.max_files, ..WorkspaceLimits::default() };
    let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).map_err(AppError::from)?,
        file_id(), limits, settings.include_excluded, budget, allocation(202)).map_err(AppError::from)?;
    let cancel = CancelFlag::new();
    while catalog.stage() == WorkspaceStage::Discovering {
        if canceled() { return Err(Failure::canceled()); }
        catalog.step(&cancel).map_err(AppError::from)?;
    }
    state.stats.known = catalog.entries().len();
    for (ordinal, entry) in catalog.entries().iter().enumerate() {
        if canceled() { return Err(Failure::canceled()); }
        catalog.validate_active().map_err(AppError::from)?;
        state.stats.visited += 1;
        let request = request(ordinal)?;
        let path = entry.path().as_bytes();
        let Some(language) = state.admit(settings, request.file(), path, entry.observed_bytes()) else { continue; };
        let loaded = (|| {
            let native = workspace::checked_source_path(&root, entry.path()).map_err(|_| Failure::new("SYMBOL_SOURCE_UNAVAILABLE"))?;
            let (file, metadata) = input::open_regular(&native)?;
            if metadata.len() != entry.observed_bytes() { return Err(Failure::new("SYMBOL_SOURCE_CHANGED")); }
            read_file(file, metadata.len(), request, state, budget, canceled)
        })();
        match loaded {
            Ok(extent) => analyze(settings, request, path, language, extent.bytes(), state, out, budget, canceled)?,
            Err(error) if error.canceled => return Err(error),
            Err(_) => { state.stats.unavailable += 1; state.notice(request.file(), path, "SYMBOL_SOURCE_UNAVAILABLE"); }
        }
    }
    catalog.validate_active().map_err(AppError::from)?;
    if settings.json { out.literal("],\"policy\":")?; out.quoted(catalog.policy_name())?; out.literal(",\"confinement\":\"path-checked-not-race-safe\",\"candidate_array_closed\":true")?; }
    Ok(catalog.discovery_complete())
}
fn saved_workspace(settings: &Settings, source: &Path, state: &mut RunState, out: &mut Output,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<bool, Failure> {
    let native = input::absolute(source)?;
    let (file, _) = input::open_regular(&native)?;
    let mut archive = PagedSnapshot::open(file, owner(), SnapshotLimits::default(), budget, allocation(202), &mut *canceled)
        .map_err(|error| if canceled() { Failure::canceled() } else { Failure::new(error.code()) })?;
    state.stats.validation_bytes = archive.directory().validation_stats().bytes_read;
    let chosen = settings.member.as_ref().map(|path| archive.directory().find_path(path.as_bytes())
        .ok_or_else(|| Failure::new("SYMBOL_MEMBER_NOT_FOUND"))).transpose()?;
    state.stats.known = if chosen.is_some() { 1 } else { archive.directory().len() };
    let (start, end) = match chosen { Some(ordinal) => (ordinal, ordinal + 1), None => (0, archive.directory().len()) };
    for ordinal in (start..end).take(settings.max_files) {
        if canceled() { return Err(Failure::canceled()); }
        state.stats.visited += 1;
        let request = request(ordinal)?;
        let entry = archive.directory().member(ordinal).ok_or_else(|| Failure::new("SYMBOL_MEMBER_NOT_FOUND"))?;
        // Borrowing metadata cannot span a mutable load; RawPath is one bounded
        // temporary path covered by Output's already-admitted path scratch.
        let path = RawPath::from_bytes(entry.path);
        let Some(language) = state.admit(settings, request.file(), path.as_bytes(), entry.observed_bytes) else { continue; };
        if matches!(entry.data, PagedMemberData::Unavailable(_)) {
            state.stats.unavailable += 1; state.notice(request.file(), path.as_bytes(), "SNAPSHOT_MEMBER_UNAVAILABLE"); continue;
        }
        let before = archive.load_stats();
        let loaded = archive.load(ordinal, budget, allocation(203), &mut *canceled);
        let after = archive.load_stats();
        state.stats.read_bytes += after.bytes_read - before.bytes_read;
        state.stats.read_calls += after.read_calls - before.read_calls;
        match loaded {
            Ok(member) => {
                state.stats.peak_source = state.stats.peak_source.max(member.bytes().len());
                analyze(settings, request, path.as_bytes(), language, member.bytes(), state, out, budget, canceled)?;
            }
            Err(error) => return Err(if canceled() { Failure::canceled() } else { Failure::new(error.code()) }),
        }
    }
    if settings.json {
        out.literal("],\"snapshot_digest\":")?; out.quoted(&archive.directory().digest().to_hex())?;
        out.literal(",\"policy\":")?; out.quoted(archive.directory().policy())?;
        out.literal(",\"candidate_array_closed\":true")?;
    }
    Ok(archive.directory().discovery_complete())
}
#[allow(clippy::too_many_arguments)]
fn analyze(settings: &Settings, request: CaptureRequest, path: &[u8], route: AnalysisRoute,
    bytes: &[u8], state: &mut RunState, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<(), Failure> {
    let language = match route {
        AnalysisRoute::Outline(language) => language,
        AnalysisRoute::References(language) => return references::analyze(settings, request, path,
            language, bytes, state, out, budget, canceled),
    };
    let symbols = match CapturedSymbols::build(bytes, request, generation(), language, settings.encoding,
        MAX_SYMBOL_ITEMS, budget, allocation(204), &mut *canceled) {
        Ok(symbols) => symbols,
        Err(SymbolError::Canceled) => return Err(Failure::canceled()),
        Err(error) => { state.stats.refused += 1; state.notice(request.file(), path, error.code()); return Ok(()); }
    };
    state.stats.analyzed += 1; state.stats.analysis_bytes += bytes.len() as u64;
    if symbols.no_recognized_declarations() { state.stats.unrecognized += 1; }
    if symbols.output_limited() { state.stats.limited += 1; state.notice(request.file(), path, "SYMBOL_ITEM_LIMIT"); }
    for candidate in symbols.candidates() {
        if canceled() { return Err(Failure::canceled()); }
        if settings.name.as_ref().is_some_and(|name| !candidate.matches_name(name, settings.mode)) { continue; }
        state.stats.matches += 1;
        if state.stats.emitted == settings.limit { continue; }
        if settings.json {
            if state.stats.emitted > 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(request.file().get())?;
            out.literal(",\"source_revision\":")?; out.integer(request.revision().get())?;
            out.literal(",\"candidate_id\":")?; out.integer(candidate.id())?;
            out.literal(",\"parent_candidate_id\":")?;
            match candidate.parent_id() { Some(id) => out.integer(id)?, None => out.literal("null")? }
            out.literal(",\"depth\":")?; out.integer(candidate.depth() as u64)?;
            out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(path).to_path_buf())?;
            out.literal(",\"name\":")?; out.quoted(candidate.name())?;
            out.literal(",\"kind\":")?; out.quoted(candidate.kind().label())?;
            out.literal(",\"language\":")?; out.quoted(language.name())?;
            out.literal(",\"line\":")?; out.integer(candidate.line())?;
            out.literal(",\"original_range\":")?; out.range(candidate.original_range())?;
            out.literal(",\"name_range\":")?;
            match candidate.name_range() { Some(range) => out.range(range)?, None => out.literal("null")? }
            out.literal("}")?;
        } else {
            out.path(&RawPath::from_bytes(path).to_path_buf())?; out.literal(" ")?;
            out.literal(candidate.kind().label())?; out.literal(" ")?; out.human_text(candidate.name())?;
            out.literal(" at line ")?; out.literal(&candidate.line().to_string())?;
            out.literal(" bytes ")?; out.literal(&candidate.original_range().start().get().to_string())?;
            out.literal("..")?; out.literal(&candidate.original_range().end().get().to_string())?; out.literal(" [candidate]\n")?;
        }
        state.stats.emitted += 1;
    }
    Ok(())
}
fn finish(settings: &Settings, discovery_complete: bool, state: RunState, out: &mut Output) -> Result<u8, Failure> {
    let stats = &state.stats;
    let truncated = stats.matches > stats.emitted;
    let processing_complete = discovery_complete && stats.visited == stats.known && stats.unsupported == 0
        && stats.unavailable == 0 && stats.refused == 0 && stats.limited == 0;
    if settings.json {
        if settings.scope == Scope::File { out.literal("]")?; }
        out.literal(",\"discovery_complete\":")?; out.boolean(discovery_complete)?;
        out.literal(",\"processing_complete\":")?; out.boolean(processing_complete)?;
        out.literal(",\"listing_truncated\":")?; out.boolean(truncated)?;
        if settings.references {
            out.literal(",\"text_candidates_complete\":")?; out.boolean(processing_complete && !truncated)?;
            out.literal(",\"candidate_count_complete\":")?; out.boolean(processing_complete)?;
        }
        out.literal(",\"stats\":{")?;
        for (i, (name, value)) in [("known_files", stats.known as u64), ("visited_files", stats.visited as u64),
            ("analyzed_files", stats.analyzed as u64), ("unsupported_language_files", stats.unsupported as u64),
            ("unavailable_files", stats.unavailable as u64), ("refused_files", stats.refused as u64),
            ("item_limited_files", stats.limited as u64), ("files_without_recognized_declarations", stats.unrecognized as u64),
            ("matching_candidates_seen", stats.matches as u64), ("emitted_candidates", stats.emitted as u64),
            ("analysis_source_bytes", stats.analysis_bytes), ("member_payload_bytes_read", stats.read_bytes),
            ("member_read_calls", stats.read_calls), ("archive_validation_bytes", stats.validation_bytes),
            ("peak_retained_source_bytes", stats.peak_source as u64), ("notices_omitted", stats.notices_omitted as u64)].into_iter().enumerate() {
            if i != 0 { out.literal(",")?; } out.quoted(name)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal("},\"notices\":[")?;
        for (i, notice) in state.notices.iter().enumerate() {
            if i > 0 { out.literal(",")?; } out.literal("{\"file_id\":")?; out.integer(notice.file.get())?;
            out.literal(",\"path\":")?; out.path(&notice.path.to_path_buf())?;
            out.literal(",\"reason\":")?; out.quoted(notice.code)?; out.literal("}")?;
        }
        out.literal("]}\n")?;
    } else {
        for notice in &state.notices {
            out.path(&notice.path.to_path_buf())?; out.literal(" ")?; out.literal(notice.code)?; out.literal("\n")?;
        }
        out.literal(if processing_complete && !truncated { "Candidate processing finished.\n" } else { "PARTIAL candidate processing or listing.\n" })?;
        out.literal("Matching candidates seen: ")?; out.literal(&stats.matches.to_string())?;
        out.literal(if settings.references { ". These are text candidates, not compiler bindings.\n" }
            else { ". Absence is not proof that no compiler definition exists.\n" })?;
    }
    Ok(if !processing_complete || truncated { EXIT_PARTIAL } else if stats.matches == 0 { EXIT_NO_MATCH } else { EXIT_OK })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
    #[test]
    fn literal_values_cannot_change_scope_or_wire_mode() {
        assert!(!wants_json(&args(&["file.rs", "--name", "--json"])));
        assert!(wants_json(&args(&["file.rs", "--name", "--json", "--json"])));
        assert!(!wants_json(&args(&["--", "--json"])));
        let settings = parse(&args(&["file.rs", "--name", "--workspace"])).unwrap();
        assert_eq!(settings.scope, Scope::File); assert_eq!(settings.name.as_deref(), Some("--workspace"));
    }
    #[test]
    fn incompatible_scopes_languages_and_budgets_are_rejected() {
        for values in [vec!["root", "--snapshot", "--workspace"], vec!["file", "--member", "a"],
            vec!["file", "--language", "markdown"], vec!["file", "--match", "prefix"],
            vec!["file", "--max-files", "65537"], vec!["file", "--limit", "4097"],
            vec!["file", "--snapshot", "--include-excluded"]] { assert!(parse(&args(&values)).is_err()); }
    }
    #[test]
    fn raw_member_names_and_zero_display_capacity_are_explicit() {
        let settings = parse(&args(&["saved", "--snapshot", "--member-hex", "61ff", "--limit", "0"])).unwrap();
        assert_eq!(settings.member.unwrap().as_bytes(), &[b'a', 0xff]); assert_eq!(settings.limit, 0);
        assert!(member_hex("0").is_err()); assert!(member_hex("zz").is_err());
    }
}

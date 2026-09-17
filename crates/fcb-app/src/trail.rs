#![forbid(unsafe_code)]

//! User-owned reference/annotation export. Every write is explicitly selected,
//! exclusive, bounded, and reported separately from response delivery. Existing
//! files are never removed or overwritten. Offline reads need an explicit archive.

use std::{ffi::OsString, fs::{File, OpenOptions}, io::{self, Read, Write}, path::{Path, PathBuf}};
use fcb::{ByteLength, ByteOffset, ByteRange};
use fcb::search::{RawPath, ReaderLimits, ReadingTarget, ReadingSeekState, ReadingWindowOptions, ResourceBudget};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::snapshot::SnapshotLimits;
use fcb::search::trail::{TrailView, TrailError, TrailNavigator, TrailEntry, MAX_TRAIL_BYTES,
    pin_selection, append_selection};
use fcb_core::ResourceLease;
use crate::{AppError, SCHEMA, MANAGED_BYTES, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED,
    owner, allocation, generation, file_id, revision, input};
use crate::args::{decimal, MAX_ARGUMENTS, MAX_ARGUMENT_BYTES, MAX_SINGLE_ARGUMENT};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};

const HELP: &str = "fcb trail add ARCHIVE --member NAME --start N --end N --output NEW_TRAIL [--note TEXT] [--json]\n\
  Append without overwriting: add ... --from EXISTING_TRAIL --output NEW_TRAIL\n\
  Native names: --member-hex HEX instead of --member NAME; optional --title for a new trail.\n\
fcb trail inspect TRAIL [--limit N] [--json]\n\
fcb trail read TRAIL --archive ARCHIVE [--item N] [--bytes N] [--lines N] [--raw] [--json]\n\
Items are one-based. Ranges are half-open original bytes, never text columns.\n\
Trails contain user notes and source references, not source payloads or access grants.\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode { Help, Add, Inspect, Read }
struct Settings {
    mode: Mode, source: Option<PathBuf>, archive: Option<PathBuf>, previous: Option<PathBuf>, output: Option<PathBuf>,
    member: Option<Vec<u8>>, start: Option<u64>, end: Option<u64>, note: String, title: String,
    item: usize, limit: usize, bytes: usize, lines: usize, raw: bool, json: bool,
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum Effect { None, Created, Written, Synced }
impl Effect {
    fn name(self) -> &'static str {
        match self { Self::None => "none", Self::Created => "destination-created-incomplete",
            Self::Written => "complete-file-sync-unconfirmed", Self::Synced => "complete-file-sync-requested" }
    }
}
struct Failure { code: String, canceled: bool }
impl Failure {
    fn new(code: &str) -> Self { Self { code: code.to_owned(), canceled: code.ends_with("CANCELED") } }
    fn canceled() -> Self { Self::new("TRAIL_CANCELED") }
}
fn fail(error: impl std::fmt::Display) -> Failure { Failure::new(&error.to_string()) }
impl From<OutputError> for Failure { fn from(e: OutputError) -> Self { fail(e) } }
impl From<TrailError> for Failure { fn from(e: TrailError) -> Self { fail(e) } }
impl From<AppError> for Failure { fn from(e: AppError) -> Self { fail(e) } }

fn takes_value(arg: &str) -> bool {
    matches!(arg, "--member" | "--member-hex" | "--start" | "--end" | "--note" | "--title" | "--from"
        | "--output" | "--archive" | "--item" | "--limit" | "--bytes" | "--lines")
}
fn wants_json(args: &[OsString]) -> bool {
    let mut i = 0;
    while i < args.len().min(MAX_ARGUMENTS + 1) {
        let arg = &args[i]; i += 1;
        if arg == "--" { break; }
        if arg == "--json" { return true; }
        if arg.to_str().is_some_and(takes_value) { i += 1; }
    }
    false
}
fn parse(args: &[OsString]) -> Result<Settings, Failure> {
    if args.len() > MAX_ARGUMENTS { return Err(Failure::new("CLI_ARGUMENT_LIMIT")); }
    let mut total = 0usize;
    for arg in args {
        total = total.checked_add(arg.len()).ok_or_else(|| Failure::new("CLI_ARGUMENT_LIMIT"))?;
        if total > MAX_ARGUMENT_BYTES || arg.len() > MAX_SINGLE_ARGUMENT { return Err(Failure::new("CLI_ARGUMENT_LIMIT")); }
    }
    let mode = match args.first().and_then(|arg| arg.to_str()) {
        None | Some("help" | "--help" | "-h") => Mode::Help,
        Some("add") => Mode::Add, Some("inspect") => Mode::Inspect, Some("read") => Mode::Read,
        _ => return Err(Failure::new("TRAIL_UNKNOWN_COMMAND")),
    };
    let mut s = Settings { mode, source: None, archive: None, previous: None, output: None, member: None,
        start: None, end: None, note: String::new(), title: "Reading trail".to_owned(),
        item: 1, limit: 100, bytes: 64 * 1024, lines: 100, raw: false, json: false };
    let mut seen = 0u32; let mut positional = false; let mut i = usize::from(!args.is_empty());
    while i < args.len() {
        let arg = &args[i]; i += 1;
        if !positional && arg == "--" { positional = true; continue; }
        if let Some(option) = (!positional).then(|| arg.to_str()).flatten().filter(|s| s.starts_with('-')) {
            let bit = match option {
                "--json" => 1, "--member" | "--member-hex" => 2, "--start" => 4, "--end" => 8,
                "--note" => 16, "--title" => 32, "--from" => 64, "--output" => 128,
                "--archive" => 256, "--item" => 512, "--limit" => 1024, "--bytes" => 2048,
                "--lines" => 4096, "--raw" => 8192, _ => return Err(Failure::new("CLI_UNKNOWN_OPTION")),
            };
            if seen & bit != 0 { return Err(Failure::new("CLI_DUPLICATE_OPTION")); }
            seen |= bit;
            if option == "--json" { s.json = true; continue; }
            if option == "--raw" { s.raw = true; continue; }
            let value = args.get(i).ok_or_else(|| Failure::new("CLI_MISSING_VALUE"))?; i += 1;
            if matches!(option, "--archive" | "--from" | "--output") {
                if value.is_empty() { return Err(Failure::new("CLI_MISSING_VALUE")); }
                let path = PathBuf::from(value);
                match option { "--archive" => s.archive = Some(path), "--from" => s.previous = Some(path), _ => s.output = Some(path) }
                continue;
            }
            if option == "--member" {
                s.member = Some(RawPath::from_path(&PathBuf::from(value)).as_bytes().to_vec()); continue;
            }
            let value = value.to_str().ok_or_else(|| Failure::new("CLI_INVALID_VALUE"))?;
            if option == "--member-hex" { s.member = Some(hex(value)?); continue; }
            if option == "--note" {
                if value.len() > 4096 { return Err(Failure::new("TRAIL_NOTE_LIMIT")); }
                s.note = value.to_owned(); continue;
            }
            if option == "--title" {
                if value.len() > 256 { return Err(Failure::new("TRAIL_TITLE_LIMIT")); }
                s.title = value.to_owned(); continue;
            }
            let n = decimal(value).map_err(fail)?;
            match option {
                "--start" => s.start = Some(n), "--end" => s.end = Some(n),
                "--item" if (1..=1024).contains(&n) => s.item = n as usize,
                "--limit" if n <= 1024 => s.limit = n as usize,
                "--bytes" if (4..=256 * 1024).contains(&n) => s.bytes = n as usize,
                "--lines" if (1..=4096).contains(&n) => s.lines = n as usize,
                _ => return Err(Failure::new("CLI_ARGUMENT_LIMIT")),
            }
        } else {
            if s.source.is_some() || arg.is_empty() { return Err(Failure::new("CLI_MULTIPLE_SOURCES")); }
            s.source = Some(PathBuf::from(arg));
        }
    }
    let valid = match s.mode {
        Mode::Help => s.source.is_none() && seen & !1 == 0,
        Mode::Add => s.source.is_some() && s.member.is_some() && s.start.is_some() && s.end.is_some()
            && s.output.is_some() && seen & !(1 | 2 | 4 | 8 | 16 | 32 | 64 | 128) == 0
            && !(s.previous.is_some() && seen & 32 != 0),
        Mode::Inspect => s.source.is_some() && seen & !(1 | 1024) == 0,
        Mode::Read => s.source.is_some() && s.archive.is_some() && seen & !(1 | 256 | 512 | 2048 | 4096 | 8192) == 0
            && !(s.raw && seen & 4096 != 0),
    };
    if !valid { return Err(Failure::new("CLI_INCOMPATIBLE_OPTIONS")); }
    Ok(s)
}
fn hex(text: &str) -> Result<Vec<u8>, Failure> {
    if text.is_empty() || text.len() % 2 != 0 || text.len() > 32_768 { return Err(Failure::new("CLI_INVALID_HEX")); }
    fn digit(b: u8) -> Option<u8> { match b { b'0'..=b'9' => Some(b - b'0'), b'a'..=b'f' => Some(b - b'a' + 10), b'A'..=b'F' => Some(b - b'A' + 10), _ => None } }
    text.as_bytes().chunks_exact(2).map(|p| Ok(digit(p[0]).ok_or_else(|| Failure::new("CLI_INVALID_HEX"))? * 16
        + digit(p[1]).ok_or_else(|| Failure::new("CLI_INVALID_HEX"))?)).collect()
}

pub(crate) fn run(args: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write, mut canceled: impl FnMut() -> bool) -> u8 {
    let json = wants_json(args);
    let Ok(budget) = ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)) else { let _ = stderr.write(b"TRAIL_RESOURCE_DENIED\n"); return EXIT_ERROR; };
    let Ok(mut out) = Output::new(owner(), MAX_RESPONSE_BYTES, &budget, allocation(200)) else { let _ = stderr.write(b"TRAIL_OUTPUT_DENIED\n"); return EXIT_ERROR; };
    let mut effect = Effect::None;
    let result = parse(args).and_then(|s| execute(&s, &mut out, &budget, &mut effect, &mut canceled));
    let result = if effect == Effect::None && result.is_ok() && canceled() { Err(Failure::canceled()) } else { result };
    let exit = match result {
        Ok(exit) => exit,
        Err(error) => {
            out.clear();
            if error_output(&mut out, json, &error, effect).is_err() { let _ = stderr.write(b"TRAIL_ERROR_ENCODING\n"); return EXIT_ERROR; }
            if error.canceled { EXIT_CANCELED } else { EXIT_ERROR }
        }
    };
    let mut stopped = false;
    let mut stop = || { let stop = effect == Effect::None && exit != EXIT_CANCELED && canceled(); stopped |= stop; stop };
    let sent = if json || matches!(exit, EXIT_OK | EXIT_PARTIAL) { out.deliver(stdout, 4096, &mut stop) } else { out.deliver(stderr, 4096, &mut stop) };
    if sent.is_err() {
        let _ = writeln!(stderr, "TRAIL_RESPONSE_INCOMPLETE effect={}", effect.name());
        return if stopped { EXIT_CANCELED } else { EXIT_ERROR };
    }
    exit
}
fn error_output(out: &mut Output, json: bool, e: &Failure, effect: Effect) -> Result<(), OutputError> {
    if !json { out.literal(&e.code)?; out.literal("; effect=")?; out.literal(effect.name())?; return out.literal("\nRun fcb trail help. Previous trails are never overwritten.\n"); }
    out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
    out.literal(",\"status\":\"error\",\"complete\":false,\"effect\":")?; out.quoted(effect.name())?;
    out.literal(",\"error\":{\"code\":")?; out.quoted(&e.code)?;
    out.literal(",\"subsystem\":\"trail\",\"retryable\":false,\"next_action\":\"Supply the exact saved archive; inspect any created destination before retrying.\"}}\n")
}
fn begin(out: &mut Output, command: &str) -> Result<(), OutputError> {
    out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
    out.literal(",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
    out.literal(",\"live_roots_accessed\":false")
}
fn metadata(out: &mut Output, view: TrailView<'_>) -> Result<(), OutputError> {
    out.literal(",\"trail_digest\":")?; out.quoted(&view.digest().to_hex())?;
    out.literal(",\"parent_digest\":")?;
    match view.parent_digest() { Some(d) => out.quoted(&d.to_hex())?, None => out.literal("null")? }
    out.literal(",\"title\":")?; out.quoted(view.title())?;
    out.literal(",\"items\":")?; out.integer(view.len() as u64)?;
    out.literal(",\"referenced_bytes_including_repeats\":")?; out.integer(view.reference_bytes())?;
    out.literal(",\"contains_source_payloads\":false,\"contains_user_text\":true")
}
fn entry_output(out: &mut Output, entry: TrailEntry<'_>, ordinal: usize) -> Result<(), OutputError> {
    out.literal("{\"item\":")?; out.integer(ordinal as u64 + 1)?;
    out.literal(",\"evidence\":\"user-selection\",\"archive_digest\":")?; out.quoted(&entry.archive.to_hex())?;
    out.literal(",\"source_digest\":")?; out.quoted(&entry.source.to_hex())?;
    out.literal(",\"source_length\":")?; out.integer(entry.source_length)?;
    out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(entry.path).to_path_buf())?;
    out.literal(",\"original_range\":")?; out.range(entry.range)?;
    out.literal(",\"rationale\":")?; out.quoted(entry.rationale)?; out.literal("}")
}
fn execute(s: &Settings, out: &mut Output, budget: &ResourceBudget, effect: &mut Effect, canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    if s.mode == Mode::Help {
        if s.json { begin(out, "trail-help")?; out.literal(",\"text\":")?; out.quoted(HELP)?; out.literal("}\n")?; }
        else { out.literal(HELP)?; }
        return Ok(EXIT_OK);
    }
    let source = s.source.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?;
    if s.mode == Mode::Add {
        let prior = s.previous.as_deref().map(|path| load_trail(path, budget, canceled)).transpose()?;
        let previous = prior.as_ref().map(|v| TrailView::open(&v.bytes, &mut *canceled)).transpose()?;
        let mut archive = open_archive(source, budget, canceled)?;
        let start = s.start.ok_or_else(|| Failure::new("TRAIL_INVALID_RANGE"))?;
        let end = s.end.ok_or_else(|| Failure::new("TRAIL_INVALID_RANGE"))?;
        let range = ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| Failure::new("TRAIL_INVALID_RANGE"))?;
        let selection = pin_selection(&mut archive, s.member.as_deref().ok_or_else(|| Failure::new("CLI_INVALID_MEMBER"))?,
            range, &s.note, budget, allocation(202), &mut *canceled).map_err(fail)?;
        let encoded = append_selection(owner(), previous, &s.title, selection, budget, [allocation(203), allocation(204)], &mut *canceled)?;
        let view = TrailView::open(encoded.bytes(), &mut *canceled)?;
        let destination = input::absolute(s.output.as_deref().ok_or_else(|| Failure::new("TRAIL_OUTPUT_REQUIRED"))?)?;
        // Prepare the complete success receipt before any filesystem effect.
        if s.json {
            begin(out, "trail-add")?; metadata(out, view)?;
            out.literal(",\"effect\":\"complete-file-sync-requested\",\"destination\":")?; out.path(&destination)?;
            out.literal(",\"power_loss_qualified\":false,\"complete\":true}\n")?;
        } else {
            out.literal("Saved reading trail to ")?; out.path(&destination)?;
            out.literal("\nReference-only user state; retain the selected archives. Previous files remain unchanged.\n")?;
        }
        write_new(&destination, encoded.bytes(), effect, canceled)?;
        return Ok(EXIT_OK);
    }
    let loaded = load_trail(source, budget, canceled)?;
    let view = TrailView::open(&loaded.bytes, &mut *canceled)?;
    if s.mode == Mode::Inspect {
        if s.json {
            begin(out, "trail-inspect")?; metadata(out, view)?;
            out.literal(",\"source_readiness\":\"not-checked\",\"listing_truncated\":")?; out.boolean(view.len() > s.limit)?;
            out.literal(",\"entries\":[")?;
        } else { out.human_text(view.title())?; out.literal("\nReferences only; source readiness has not been checked.\n")?; }
        for (i, entry) in view.entries().take(s.limit).enumerate() {
            if canceled() { return Err(Failure::canceled()); }
            let entry = entry?;
            if s.json { if i > 0 { out.literal(",")?; } entry_output(out, entry, i)?; }
            else {
                out.literal(&format!("{}: ", i + 1))?; out.path(&RawPath::from_bytes(entry.path).to_path_buf())?;
                out.literal(&format!(" bytes {}..{}\n", entry.range.start().get(), entry.range.end().get()))?;
                out.human_text(entry.rationale)?; out.literal("\n")?;
            }
        }
        if s.json { out.literal("]}\n")?; }
        return Ok(if view.len() > s.limit { EXIT_PARTIAL } else { EXIT_OK });
    }
    let mut archive = open_archive(s.archive.as_deref().ok_or_else(|| Failure::new("TRAIL_ARCHIVE_REQUIRED"))?, budget, canceled)?;
    let mut navigator = TrailNavigator::new(view, generation()); navigator.select(s.item - 1).map_err(fail)?;
    let target = navigator.target().map_err(fail)?.ok_or_else(|| Failure::new("TRAIL_ITEM_NOT_FOUND"))?;
    let capture = target.open(&mut archive, view.digest(), navigator.generation(), file_id(), revision(), budget,
        [allocation(202), allocation(205)], &mut *canceled).map_err(fail)?;
    let selected = capture.selected_bytes();
    let shown = selected.len().min(s.bytes); let mut partial = shown < selected.len();
    if s.json {
        begin(out, "trail-read")?; out.literal(",\"trail_digest\":")?; out.quoted(&view.digest().to_hex())?;
        out.literal(",\"selection\":")?; entry_output(out, target.entry(), target.ordinal())?;
        out.literal(",\"source_readiness\":\"digest-verified\",\"selected_original_hex\":")?; out.hex(&selected[..shown])?;
        out.literal(",\"selected_bytes_truncated\":")?; out.boolean(partial)?;
    } else {
        out.human_text(target.entry().rationale)?; out.literal("\n")?;
        if s.raw { out.hex(&selected[..shown])?; out.literal("\n")?; }
    }
    if !s.raw {
        let reader = capture.reader(ReaderLimits::default(), budget, allocation(206)).map_err(fail)?;
        let mut seek = reader.seek(ReadingTarget::Range(target.entry().range), navigator.generation()).map_err(fail)?;
        while seek.state() == ReadingSeekState::Pending { seek.step(64 * 1024, navigator.generation(), &mut *canceled).map_err(fail)?; }
        let ReadingSeekState::Ready(at) = seek.state() else { return Err(Failure::new("TRAIL_READING_UNAVAILABLE")); };
        let window = reader.window(at, navigator.generation(), ReadingWindowOptions { max_bytes: s.bytes, max_lines: s.lines },
            budget, allocation(207), &mut *canceled).map_err(fail)?;
        let mapped = window.source_to_text(target.entry().range).ok();
        partial |= window.next_anchor().is_some() || mapped.is_none();
        if s.json {
            out.literal(",\"window_original_range\":")?; out.range(window.raw_range())?;
            out.literal(",\"text\":")?; out.quoted(window.text())?;
            out.literal(",\"first_line\":")?; out.integer(at.line_number())?;
            out.literal(",\"has_replacements\":")?; out.boolean(window.has_replacements())?;
            out.literal(",\"selection_window_utf8_range\":")?;
            if let Some(mapped) = mapped {
                out.literal("{\"start\":")?; out.integer(mapped.start().get())?;
                out.literal(",\"end\":")?; out.integer(mapped.end().get())?; out.literal("}")?;
            } else { out.literal("null")?; }
            out.literal(",\"next_source_offset\":")?;
            if let Some(next) = window.next_anchor() { out.integer(next.offset().get())?; } else { out.literal("null")?; }
        } else { out.human_text(window.text())?; out.literal("\n")?; }
    }
    if canceled() { return Err(Failure::canceled()); }
    if s.json { out.literal(",\"complete\":")?; out.boolean(!partial)?; out.literal("}\n")?; }
    Ok(if partial { EXIT_PARTIAL } else { EXIT_OK })
}

fn open_archive(path: &Path, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<PagedSnapshot<File>, Failure> {
    let path = input::absolute(path)?; let (file, _) = input::open_regular(&path)?;
    PagedSnapshot::open(file, owner(), SnapshotLimits::default(), budget, allocation(201), canceled).map_err(fail)
}
struct Loaded { bytes: Vec<u8>, _lease: ResourceLease }
fn load_trail(path: &Path, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<Loaded, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    let path = input::absolute(path)?; let (mut file, before) = input::open_regular(&path)?;
    let size = usize::try_from(before.len()).map_err(|_| Failure::new("TRAIL_LIMIT"))?;
    if size > MAX_TRAIL_BYTES { return Err(Failure::new("TRAIL_LIMIT")); }
    let lease = budget.try_reserve_managed(owner(), allocation(208), ByteLength::new(size as u64 + 256)).map_err(|_| Failure::new("TRAIL_RESOURCE_DENIED"))?;
    let mut bytes = Vec::new(); bytes.try_reserve_exact(size).map_err(|_| Failure::new("TRAIL_RESOURCE_DENIED"))?;
    if bytes.capacity() > size { return Err(Failure::new("TRAIL_RESOURCE_DENIED")); }
    bytes.resize(size, 0); let mut offset = 0; let mut eof = false;
    for _ in 0..131_072 {
        if canceled() { return Err(Failure::canceled()); }
        let mut extra = [0u8]; let end = size.min(offset + 64 * 1024);
        let buffer = if offset == size { &mut extra[..] } else { &mut bytes[offset..end] };
        match file.read(buffer) {
            Ok(0) if offset == size => { eof = true; break; },
            Ok(n) if n > 0 && offset < size && n <= end - offset => offset += n,
            Ok(_) => return Err(Failure::new("TRAIL_FILE_CHANGED")),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => return Err(Failure::new("TRAIL_READ_FAILED")),
        }
    }
    if !eof { return Err(Failure::new("TRAIL_READ_CALL_LIMIT")); }
    let after = file.metadata().map_err(|_| Failure::new("TRAIL_READ_FAILED"))?;
    if before.len() != after.len() || matches!((before.modified().ok(), after.modified().ok()), (Some(a), Some(b)) if a != b) {
        return Err(Failure::new("TRAIL_FILE_CHANGED"));
    }
    Ok(Loaded { bytes, _lease: lease })
}
fn write_new(path: &Path, bytes: &[u8], effect: &mut Effect, canceled: &mut impl FnMut() -> bool) -> Result<(), Failure> {
    if canceled() { return Err(Failure::canceled()); }
    let mut options = OpenOptions::new(); options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut file = options.open(path).map_err(|e| Failure::new(if e.kind() == io::ErrorKind::AlreadyExists { "TRAIL_DESTINATION_EXISTS" } else { "TRAIL_CREATE_FAILED" }))?;
    *effect = Effect::Created; let mut offset = 0;
    for _ in 0..131_072 {
        if canceled() { return Err(Failure::canceled()); }
        if offset == bytes.len() { break; }
        let end = bytes.len().min(offset + 64 * 1024);
        match file.write(&bytes[offset..end]) {
            Ok(n) if n > 0 && n <= end - offset => offset += n,
            Ok(_) => return Err(Failure::new("TRAIL_WRITE_FAILED")),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => return Err(Failure::new("TRAIL_WRITE_FAILED")),
        }
        if offset == bytes.len() { *effect = Effect::Written; break; }
    }
    if offset != bytes.len() { return Err(Failure::new("TRAIL_WRITE_CALL_LIMIT")); }
    if canceled() { return Err(Failure::canceled()); }
    file.sync_all().map_err(|_| Failure::new("TRAIL_SYNC_UNCERTAIN"))?;
    File::open(path.parent().ok_or_else(|| Failure::new("TRAIL_SYNC_UNCERTAIN"))?)
        .and_then(|parent| parent.sync_all()).map_err(|_| Failure::new("TRAIL_SYNC_UNCERTAIN"))?;
    *effect = Effect::Synced; Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
    #[test]
    fn user_notes_and_member_values_do_not_change_json_mode() {
        assert!(!wants_json(&args(&["add", "a", "--note", "--json"])));
        assert!(!wants_json(&args(&["add", "a", "--member", "--json"])));
        assert!(wants_json(&args(&["add", "a", "--note", "--json", "--json"])));
        assert!(!wants_json(&args(&["inspect", "--", "--json"])));
    }
    #[test]
    fn explicit_ranges_and_destinations_required_and_no_silent_option_ignoring() {
        assert!(parse(&args(&["add", "a", "--member", "x", "--start", "0", "--output", "new"])).is_err());
        assert!(parse(&args(&["inspect", "a", "--note", "x"])).is_err());
        assert!(parse(&args(&["read", "a", "--archive", "b", "--raw", "--lines", "10"])).is_err());
        assert!(parse(&args(&["read", "a", "--archive", "b", "--item", "0"])).is_err());
        let valid = parse(&args(&["add", "a", "--member-hex", "ff", "--start", "0", "--end", "0", "--output", "new"])).unwrap();
        assert_eq!(valid.member, Some(vec![255]));
    }
}

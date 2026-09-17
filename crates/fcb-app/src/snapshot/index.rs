#![forbid(unsafe_code)]

//! Explicit CLI publication/reuse of a saved substring index. Reuses the source
//! snapshot's no-overwrite writer and effect-aware receipt contract. The index
//! digest must come from the prior trusted build receipt, never the input file.

mod refresh;
mod inverted;
mod paging;

use std::{ffi::OsString, fs, io::{self, Read, Seek, Write}, path::{Path, PathBuf}};
use fcb::ByteLength;
use fcb::search::{IndexLimits, RawPath, ResourceBudget, StreamReadStep};
use fcb::search::paged_snapshot::{PagedQuery, PagedQueryOptions, PagedQueryState,
    IndexedNeedle, SnapshotDirectory, PagedSnapshotError};
use fcb::search::snapshot::Sha256Digest;
use fcb::search::snapshot_index::{SnapshotIndex, SnapshotIndexError, SavedIndexStats, MAX_POSTINGS_BYTES, MAX_INDEX_GRAMS};
use fcb_core::ResourceLease;
use crate::{allocation, file_id, generation, owner, revision, input};
use crate::output::{Output, MAX_RESPONSE_BYTES};
use super::{Failure, Effect, begin, failure_output, write_new, hex, decimal, catalog,
    MAX_ARGUMENTS, MAX_ARGUMENT_BYTES, MAX_SINGLE_ARGUMENT, MANAGED_BYTES,
    EXIT_OK, EXIT_NO_MATCH, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};

const HELP: &str = "fcb snapshot index build SNAPSHOT --output NEW_INDEX [--inverted | --paged] [--json]\n\
fcb snapshot index refresh NEW_SNAPSHOT --base OLD_SNAPSHOT --index OLD_INDEX\n\
    --index-digest TRUSTED_OLD_SHA256 --output NEW_INDEX [--inverted] [--json]\n\
fcb snapshot index inspect SNAPSHOT --index INDEX --index-digest TRUSTED_SHA256 [--paged] [--json]\n\
fcb snapshot index search SNAPSHOT --index INDEX --index-digest TRUSTED_SHA256\n\
    (--text LITERAL | --raw-hex HEX) [--paged] [--limit N] [--json]\n\
Build/refresh options: --max-grams N --max-file-bytes N --max-source-bytes N\n\
--inverted persists resident global postings for rarest-list intersection.\n\
--paged builds/opens FCBD verified pages with a four-page 64 KiB cache.\n\
Its index_digest pins the metadata envelope and page hashes, not the full file.\n\
Use --paged explicitly on build, inspect and search; refresh remains FCBI/FCBO.\n\
Without --paged, search and inspect accept FCBI/FCBO under the full-artifact pin.\n\
Optional target cold-open: --catalog FILE --catalog-digest TRUSTED_CATALOG_SHA256\n\
Refresh base cold-open: --base-catalog FILE --base-catalog-digest TRUSTED_SHA256\n\
Refresh copies only digest-identical complete segments; changed files rebuild or\n\
remain uncovered for direct scanning. It creates a NEW index, never overwrites.\n\
A trusted catalog avoids the archive body scan, NOT member digest verification.\n\
Unread body integrity remains unchecked. Digests must be retained separately\n\
from original trusted builds, never inferred from untrusted input files.\n\
Ordinary snapshot search needs no index or pin and remains the full-scan fallback.\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action { Help, Build, Refresh, Inspect, Search }
struct Options {
    action: Action, archive: Option<PathBuf>, output: Option<PathBuf>, index: Option<PathBuf>,
    pin: Option<Sha256Digest>, text: Option<String>, raw: Option<Vec<u8>>, json: bool,
    limit: usize, build: IndexLimits, catalog: Option<PathBuf>, catalog_pin: Option<Sha256Digest>,
    base: Option<PathBuf>, base_catalog: Option<PathBuf>, base_catalog_pin: Option<Sha256Digest>,
    inverted: bool, paged: bool,
}
impl From<SnapshotIndexError> for Failure {
    fn from(error: SnapshotIndexError) -> Self {
        Self { code: error.to_string(), canceled: matches!(error, SnapshotIndexError::Canceled
            | SnapshotIndexError::Archive(PagedSnapshotError::Canceled)
            | SnapshotIndexError::Build(fcb::search::IndexError::Canceled)) }
    }
}
fn takes_value(arg: &str) -> bool {
    matches!(arg, "--output" | "--index" | "--index-digest" | "--text" | "--raw-hex" | "--limit"
        | "--max-grams" | "--max-file-bytes" | "--max-source-bytes" | "--catalog" | "--catalog-digest"
        | "--base" | "--base-catalog" | "--base-catalog-digest")
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
fn parse(args: &[OsString]) -> Result<Options, Failure> {
    if args.len() > MAX_ARGUMENTS { return Err(Failure::new("CLI_ARGUMENT_LIMIT")); }
    let mut total = 0usize;
    for arg in args {
        total = total.checked_add(arg.len()).ok_or_else(|| Failure::new("CLI_ARGUMENT_LIMIT"))?;
        if arg.len() > MAX_SINGLE_ARGUMENT || total > MAX_ARGUMENT_BYTES { return Err(Failure::new("CLI_ARGUMENT_LIMIT")); }
    }
    let action = match args.first().and_then(|arg| arg.to_str()) {
        None | Some("help" | "--help" | "-h") => Action::Help,
        Some("build") => Action::Build, Some("refresh") => Action::Refresh,
        Some("inspect") => Action::Inspect, Some("search") => Action::Search,
        _ => return Err(Failure::new("SAVED_INDEX_UNKNOWN_COMMAND")),
    };
    let mut out = Options { action, archive: None, output: None, index: None, pin: None, text: None,
        raw: None, json: false, limit: 100, build: IndexLimits::default(), catalog: None, catalog_pin: None,
        base: None, base_catalog: None, base_catalog_pin: None, inverted: false, paged: false };
    let mut seen = 0u32;
    let mut cursor = usize::from(!args.is_empty());
    let mut positional = false;
    while cursor < args.len() {
        let arg = &args[cursor]; cursor += 1;
        if !positional && arg == "--" { positional = true; continue; }
        let option = if positional { None } else { arg.to_str().filter(|text| text.starts_with('-')) };
        let Some(option) = option else {
            if arg.is_empty() || out.archive.is_some() { return Err(Failure::new("CLI_MULTIPLE_SOURCES")); }
            out.archive = Some(PathBuf::from(arg)); continue;
        };
        let bit = match option { "--json" => 1, "--output" => 2, "--index" => 4, "--index-digest" => 8,
            "--text" => 16, "--raw-hex" => 32, "--limit" => 64, "--max-grams" => 128,
            "--max-file-bytes" => 256, "--max-source-bytes" => 512,
            "--catalog" => 1024, "--catalog-digest" => 2048,
            "--base" => 4096, "--base-catalog" => 8192, "--base-catalog-digest" => 16384,
            "--inverted" => 32768, "--paged" => 65536,
            _ => return Err(Failure::new("CLI_UNKNOWN_OPTION")),
        };
        if seen & bit != 0 { return Err(Failure::new("CLI_DUPLICATE_OPTION")); }
        seen |= bit;
        if option == "--json" { out.json = true; continue; }
        if option == "--inverted" { out.inverted = true; continue; }
        if option == "--paged" { out.paged = true; continue; }
        let value = args.get(cursor).ok_or_else(|| Failure::new("CLI_MISSING_VALUE"))?; cursor += 1;
        if value.is_empty() { return Err(Failure::new("CLI_MISSING_VALUE")); }
        match option {
            "--output" => { out.output = Some(PathBuf::from(value)); continue; }
            "--index" => { out.index = Some(PathBuf::from(value)); continue; }
            "--catalog" => { out.catalog = Some(PathBuf::from(value)); continue; }
            "--base" => { out.base = Some(PathBuf::from(value)); continue; }
            "--base-catalog" => { out.base_catalog = Some(PathBuf::from(value)); continue; }
            _ => {},
        }
        let text = value.to_str().ok_or_else(|| Failure::new("CLI_INVALID_VALUE"))?;
        if option == "--catalog-digest" { out.catalog_pin = Some(catalog::parse_pin(text)?); continue; }
        if option == "--base-catalog-digest" { out.base_catalog_pin = Some(catalog::parse_pin(text)?); continue; }
        if option == "--index-digest" {
            if text.len() != 64 { return Err(Failure::new("SAVED_INDEX_INVALID_PIN")); }
            out.pin = Some(Sha256Digest::new(hex(text, 32)?.try_into().map_err(|_| Failure::new("SAVED_INDEX_INVALID_PIN"))?));
            continue;
        }
        if option == "--text" {
            if text.len() > 1024 { return Err(Failure::new("CLI_INVALID_NEEDLE")); }
            out.text = Some(text.to_owned()); continue;
        }
        if option == "--raw-hex" { out.raw = Some(hex(text, 1024)?); continue; }
        let number = decimal(text).map_err(|error| Failure::new(error.code()))?;
        match option {
            "--limit" if number <= 4096 => out.limit = number as usize,
            "--max-grams" if number <= MAX_INDEX_GRAMS as u64 => out.build.max_total_grams = number as usize,
            "--max-file-bytes" if number <= 256 * 1024 => out.build.max_source_bytes_per_file = number as usize,
            "--max-source-bytes" if number <= 64 * 1024 * 1024 => out.build.max_source_bytes_total = number,
            _ => return Err(Failure::new("CLI_ARGUMENT_LIMIT")),
        }
    }
    if out.catalog.is_some() != out.catalog_pin.is_some() || out.base_catalog.is_some() != out.base_catalog_pin.is_some() {
        return Err(Failure::new("CATALOG_PATH_AND_PIN_REQUIRED"));
    }
    let catalog_options = 1024 | 2048;
    let valid = match action {
        Action::Help => out.archive.is_none() && seen & !1 == 0,
        Action::Build => out.archive.is_some() && out.output.is_some() && seen & !(1 | 2 | 128 | 256 | 512 | catalog_options | 32768 | 65536) == 0,
        Action::Refresh => out.archive.is_some() && out.base.is_some() && out.output.is_some() && out.index.is_some() && out.pin.is_some()
            && seen & !(1 | 2 | 4 | 8 | 128 | 256 | 512 | catalog_options | 4096 | 8192 | 16384 | 32768) == 0,
        Action::Inspect => out.archive.is_some() && out.index.is_some() && out.pin.is_some() && seen & !(1 | 4 | 8 | catalog_options | 65536) == 0,
        Action::Search => out.archive.is_some() && out.index.is_some() && out.pin.is_some()
            && (out.text.is_some() != out.raw.is_some()) && seen & !(1 | 4 | 8 | 16 | 32 | 64 | catalog_options | 65536) == 0,
    };
    if !valid || (out.inverted && out.paged) { return Err(Failure::new("CLI_INCOMPATIBLE_OPTIONS")); }
    Ok(out)
}

pub(super) fn run(args: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write,
    mut canceled: impl FnMut() -> bool) -> u8 {
    let json = wants_json(args);
    let budget = match ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)) {
        Ok(budget) => budget, Err(_) => { let _ = stderr.write(b"SAVED_INDEX_RESOURCE_DENIED\n"); return EXIT_ERROR; }
    };
    let mut out = match Output::new(owner(), MAX_RESPONSE_BYTES, &budget, allocation(150)) {
        Ok(out) => out, Err(_) => { let _ = stderr.write(b"SAVED_INDEX_OUTPUT_DENIED\n"); return EXIT_ERROR; }
    };
    let mut effect = Effect::None;
    let result = parse(args).and_then(|options| execute(&options, &mut out, &budget, &mut effect, &mut canceled));
    let result = if result.is_ok() && effect == Effect::None && canceled() { Err(Failure::canceled()) } else { result };
    let exit = match result {
        Ok(exit) => exit,
        Err(error) => {
            out.clear();
            if failure_output(&mut out, json, &error, effect).is_err() {
                let _ = stderr.write(b"SAVED_INDEX_ERROR_ENCODING_FAILED\n"); return EXIT_ERROR;
            }
            if error.canceled { EXIT_CANCELED } else { EXIT_ERROR }
        }
    };
    let mut interrupted = false;
    let mut stop = || {
        let stop = effect == Effect::None && exit != EXIT_CANCELED && canceled();
        interrupted |= stop; stop
    };
    let result = if json || matches!(exit, EXIT_OK | EXIT_NO_MATCH | EXIT_PARTIAL) {
        out.deliver(stdout, 4096, &mut stop)
    } else { out.deliver(stderr, 4096, &mut stop) };
    if result.is_err() {
        let _ = writeln!(stderr, "SAVED_INDEX_RESPONSE_INCOMPLETE effect={}", effect.name());
        return if interrupted { EXIT_CANCELED } else { EXIT_ERROR };
    }
    exit
}
fn execute(options: &Options, out: &mut Output, budget: &ResourceBudget, effect: &mut Effect,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    if options.action == Action::Help {
        if options.json { begin(out, "snapshot-index-help")?; out.literal(",\"text\":")?; out.quoted(HELP)?; out.literal("}\n")?; }
        else { out.literal(HELP)?; }
        return Ok(EXIT_OK);
    }
    let source = input::absolute(options.archive.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?)?;
    // Refuse an existing output before paying source/index build work.
    let destination = options.output.as_ref().map(|path| input::absolute(path)).transpose()?;
    if let Some(path) = &destination {
        match fs::symlink_metadata(path) {
            Ok(_) => return Err(Failure::new("SNAPSHOT_DESTINATION_EXISTS")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(_) => return Err(Failure::new("SNAPSHOT_DESTINATION_UNAVAILABLE")),
        }
    }
    if options.action == Action::Refresh { return refresh::execute(options, out, budget, effect, canceled); }
    if options.paged { return paging::execute(options, out, budget, effect, canceled); }
    let mut archive = catalog::open(&source, options.catalog.as_deref(), options.catalog_pin,
        budget, [allocation(151), allocation(161)], canceled)?;
    if !options.json && !archive.directory().fully_verified_on_open() {
        out.literal("Trusted catalog; archive boundaries checked. Unread body integrity is unchecked; loaded members are digest-verified.\n")?;
    }
    if options.action == Action::Build {
        let index = SnapshotIndex::build(&mut archive, options.build, budget,
            [allocation(152), allocation(153), allocation(154), allocation(155)], &mut *canceled)?;
        let artifact = inverted::encode(&index, options.inverted, budget, canceled)?;
        let destination = destination.ok_or_else(|| Failure::new("SNAPSHOT_OUTPUT_REQUIRED"))?;
        write_new(&destination, artifact.bytes(), effect, canceled)?;
        if options.json {
            begin(out, "snapshot-index-build")?; summary(out, archive.directory(), index.stats(), artifact.digest())?;
            inverted::fields(out, options.inverted)?;
            out.literal(",\"effect\":")?; out.quoted(effect.name())?;
            out.literal(",\"destination\":")?; out.path(&destination)?;
            out.literal(",\"index_bytes\":")?; out.integer(artifact.bytes().len() as u64)?;
            out.literal(",\"member_payload_bytes_loaded\":")?; out.integer(archive.load_stats().bytes_read)?;
            out.literal(",\"source_derived_sensitive\":true,\"power_loss_qualified\":false}\n")?;
        } else {
            out.literal("Saved substring index. Retain this trusted index digest separately:\n")?;
            out.literal(&artifact.digest().to_hex())?; out.literal("\n")?;
            out.literal("Source-derived data may expose source fragments. Existing destinations are never overwritten.\n")?;
            out.literal(if index.stats().uncovered_files > 0 { "Some files remain uncovered and will be scanned directly.\n" } else { "All captured members have segments.\n" })?;
        }
        return Ok(if !archive.directory().discovery_complete() || index.stats().unavailable_files > 0
            || index.stats().uncovered_files > 0 { EXIT_PARTIAL } else { EXIT_OK });
    }
    let pin = options.pin.ok_or_else(|| Failure::new("SAVED_INDEX_PIN_REQUIRED"))?;
    let loaded = load_index(options.index.as_deref().ok_or_else(|| Failure::new("SAVED_INDEX_REQUIRED"))?, budget, canceled)?;
    let index = inverted::QueryIndex::decode(&loaded.bytes, pin, archive.directory(), budget, canceled)?;
    let index_bytes = loaded.bytes.len();
    drop(loaded);
    if options.action == Action::Inspect {
        if options.json {
            begin(out, "snapshot-index-inspect")?; summary(out, archive.directory(), index.stats(), pin)?;
            inverted::fields(out, index.inverted())?;
            out.literal(",\"index_bytes\":")?; out.integer(index_bytes as u64)?;
            out.literal(",\"member_payload_bytes_loaded\":\"0\"}\n")?;
        } else { out.literal("Pinned index matches the selected saved scope.\n")?;
            out.literal(if index.inverted() { "Layout: global posting lists.\n" } else { "Layout: per-member grams.\n" })?;
            out.literal("Indexed files: ")?; out.literal(&index.stats().indexed_files.to_string())?;
            out.literal("; uncovered files: ")?; out.literal(&index.stats().uncovered_files.to_string())?; out.literal("\n")?; }
        return Ok(if index.stats().uncovered_files > 0 || index.stats().unavailable_files > 0
            || !archive.directory().discovery_complete() { EXIT_PARTIAL } else { EXIT_OK });
    }
    let needle = match (&options.text, &options.raw) {
        (Some(text), None) => IndexedNeedle::text(owner(), text, budget, allocation(157)),
        (None, Some(bytes)) => IndexedNeedle::raw(owner(), bytes, budget, allocation(157)),
        _ => return Err(Failure::new("CLI_INVALID_NEEDLE")),
    }.map_err(|error| Failure::new(&error.to_string()))?;
    let opts = PagedQueryOptions { generation: generation(), first_file: file_id(), first_revision: revision(), max_matches: options.limit };
    let mut query = index.query(&mut archive, &needle, opts, budget)?;
    while query.state() == PagedQueryState::Pending {
        query.step(StreamReadStep::default(), generation(), budget, &mut *canceled)?;
    }
    let report = query.finish()?;
    if canceled() { return Err(Failure::canceled()); }
    if options.json {
        begin(out, "snapshot-index-search")?; summary(out, archive.directory(), index.stats(), pin)?;
        inverted::fields(out, index.inverted())?;
        out.literal(",\"identity_scope\":\"response-local\",\"mode\":")?;
        out.quoted(if options.text.is_some() { "exact-text" } else { "original-bytes" })?;
        out.literal(",\"workspace_complete\":")?; out.boolean(report.is_complete())?;
        out.literal(",\"truncated\":")?; out.boolean(report.truncated())?;
        out.literal(",\"matches_seen\":")?; out.integer(report.matches_seen())?;
        out.literal(",\"unsupported_text_files_count\":")?; out.integer(report.stats().unsupported_files as u64)?;
        out.literal(",\"index_eliminated_files\":")?; out.integer(report.stats().index_eliminated_files as u64)?;
        out.literal(",\"index_candidates\":")?; out.integer(report.stats().index_candidates as u64)?;
        out.literal(",\"index_fallback_files\":")?; out.integer(report.stats().index_fallback_files as u64)?;
        out.literal(",\"metadata_members_visited\":")?; out.integer(report.stats().members_visited as u64)?;
        out.literal(",\"posting_list_lookups\":")?; out.integer(report.stats().posting_list_lookups as u64)?;
        out.literal(",\"posting_entries_visited\":")?; out.integer(report.stats().posting_entries_visited as u64)?;
        out.literal(",\"posting_membership_lookups\":")?; out.integer(report.stats().posting_membership_lookups as u64)?;
        out.literal(",\"posting_cursor_complete\":")?; out.boolean(report.stats().posting_cursor_complete)?;
        out.literal(",\"index_bytes\":")?; out.integer(index_bytes as u64)?;
        out.literal(",\"member_payload_bytes_loaded\":")?; out.integer(archive.load_stats().bytes_read)?;
        out.literal(",\"loaded_members\":")?; out.integer(archive.load_stats().loaded_members)?;
        out.literal(",\"scanned_bytes\":")?; out.integer(report.stats().scanned_bytes)?;
        out.literal(",\"peak_retained_source_bytes\":")?; out.integer(report.stats().peak_source_bytes as u64)?;
        out.literal(",\"hits\":[")?;
    } else {
        out.literal(if report.is_complete() { "Complete indexed saved-scope search.\n" } else { "PARTIAL indexed saved-scope search.\n" })?;
    }
    for (position, hit) in report.hits().iter().enumerate() {
        if canceled() { return Err(Failure::canceled()); }
        let member = archive.directory().member(hit.ordinal()).ok_or_else(|| Failure::new("SNAPSHOT_MEMBER_MISSING"))?;
        let path = RawPath::from_bytes(member.path).to_path_buf();
        if options.json {
            if position > 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(hit.file().get())?;
            out.literal(",\"revision\":")?; out.integer(hit.revision().get())?;
            out.literal(",\"path\":")?; out.path(&path)?;
            out.literal(",\"source_digest\":")?; out.quoted(&hit.source_digest().to_hex())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?; out.literal("}")?;
        } else {
            out.path(&path)?; out.literal(" bytes ")?; out.literal(&hit.original_range().start().get().to_string())?;
            out.literal("..")?; out.literal(&hit.original_range().end().get().to_string())?; out.literal("\n")?;
        }
    }
    if options.json { out.literal("]}\n")?; }
    Ok(if !report.is_complete() { EXIT_PARTIAL } else if report.hits().is_empty() { EXIT_NO_MATCH } else { EXIT_OK })
}
fn summary(out: &mut Output, directory: &SnapshotDirectory, stats: SavedIndexStats, pin: Sha256Digest) -> Result<(), Failure> {
    out.literal(",\"source_scope\":\"saved-observations-only\",\"live_roots_accessed\":false,\"snapshot_digest\":")?;
    out.quoted(&directory.digest().to_hex())?;
    out.literal(",\"index_digest\":")?; out.quoted(&pin.to_hex())?;
    out.literal(",\"pin_policy\":\"separately-retained-trusted-build-digest\",\"discovery_complete\":")?;
    out.boolean(directory.discovery_complete())?;
    out.literal(",\"known_files\":")?; out.integer(stats.members as u64)?;
    out.literal(",\"indexed_files\":")?; out.integer(stats.indexed_files as u64)?;
    out.literal(",\"uncovered_files\":")?; out.integer(stats.uncovered_files as u64)?;
    out.literal(",\"unavailable_files_count\":")?; out.integer(stats.unavailable_files as u64)?;
    out.literal(",\"unique_grams\":")?; out.integer(stats.unique_grams as u64)?;
    out.literal(",\"index_build_source_bytes\":")?; out.integer(stats.build_source_bytes)?;
    out.literal(",\"archive_validation_bytes\":")?; out.integer(directory.validation_stats().bytes_read)?;
    catalog::validation_fields(out, directory)?;
    Ok(())
}
struct LoadedIndex { bytes: Vec<u8>, _lease: ResourceLease }
fn load_index(path: &Path, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<LoadedIndex, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    let path = input::absolute(path)?;
    let (mut file, metadata) = input::open_regular(&path)?;
    let length = usize::try_from(metadata.len()).map_err(|_| Failure::new("SAVED_INDEX_LIMIT"))?;
    if length > MAX_POSTINGS_BYTES { return Err(Failure::new("SAVED_INDEX_LIMIT")); }
    let lease = budget.try_reserve_managed(owner(), allocation(156), ByteLength::new(length as u64 + 256))
        .map_err(|_| Failure::new("SAVED_INDEX_RESOURCE_DENIED"))?;
    let mut bytes = Vec::new(); bytes.try_reserve_exact(length).map_err(|_| Failure::new("SAVED_INDEX_RESOURCE_DENIED"))?;
    if bytes.capacity() > length { return Err(Failure::new("SAVED_INDEX_RESOURCE_DENIED")); }
    bytes.resize(length, 0);
    let mut offset = 0;
    for _ in 0..131_072 {
        if canceled() { return Err(Failure::canceled()); }
        let mut extra = [0u8; 1];
        let end = length.min(offset + 64 * 1024);
        let target = if offset == length { &mut extra[..] } else { &mut bytes[offset..end] };
        match file.read(target) {
            Ok(0) if offset == length => return Ok(LoadedIndex { bytes, _lease: lease }),
            Ok(n) if offset < length && n > 0 && n <= end - offset => offset += n,
            Ok(_) => return Err(Failure::new("SAVED_INDEX_FILE_CHANGED")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => return Err(Failure::new("SAVED_INDEX_READ_FAILED")),
        }
    }
    Err(Failure::new("SAVED_INDEX_READ_CALL_LIMIT"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
    #[test]
    fn pins_are_required_and_never_inferred_from_untrusted_files() {
        assert!(parse(&args(&["search", "saved", "--index", "index", "--text", "needle"])).is_err());
        assert!(parse(&args(&["inspect", "saved", "--index", "index"])).is_err());
        assert!(parse(&args(&["build", "saved"])).is_err());
    }
    #[test]
    fn query_values_and_native_names_do_not_enable_json() {
        assert!(!wants_json(&args(&["build", "saved", "--output", "--json"])));
        assert!(!wants_json(&args(&["search", "saved", "--text", "--json"])));
        assert!(wants_json(&args(&["search", "saved", "--text", "--json", "--json"])));
        assert!(!wants_json(&args(&["inspect", "--", "--json"])));
    }
    #[test]
    fn duplicate_incompatible_and_excessive_options_are_refused() {
        for values in [vec!["build", "saved", "--output", "new", "--max-grams", "2097153"],
            vec!["build", "saved", "--output", "new", "--text", "x"],
            vec!["build", "saved", "--output", "new", "--output", "other"]] {
            assert!(parse(&args(&values)).is_err());
        }
        assert_eq!(parse(&args(&["build", "saved", "--output", "new", "--max-grams", "0"])).unwrap().build.max_total_grams, 0);
    }
    #[test]
    fn optional_catalog_pin_is_paired_and_independent_of_required_index_pin() {
        let pin = "ab".repeat(32);
        assert!(parse(&args(&["inspect", "saved", "--index", "index", "--index-digest", &pin, "--catalog", "meta"])).is_err());
        assert!(parse(&args(&["inspect", "saved", "--index", "index", "--catalog", "meta", "--catalog-digest", &pin])).is_err());
        assert!(parse(&args(&["inspect", "saved", "--index", "index", "--index-digest", &pin,
            "--catalog", "meta", "--catalog-digest", &pin])).is_ok());
        assert!(!wants_json(&args(&["inspect", "saved", "--catalog", "--json"])));
        assert!(!wants_json(&args(&["inspect", "saved", "--catalog-digest", "--json"])));
    }
    #[test]
    fn refresh_requires_separate_base_target_output_and_trusted_index_pin() {
        let pin = "ab".repeat(32);
        let valid = args(&["refresh", "new", "--base", "old", "--index", "prior", "--index-digest", &pin, "--output", "fresh"]);
        assert_eq!(parse(&valid).unwrap().action, Action::Refresh);
        for range in [2..4, 4..6, 6..8, 8..10] {
            let mut missing = valid.clone(); missing.drain(range); assert!(parse(&missing).is_err());
        }
        let mut invalid = valid.clone(); invalid.extend(args(&["--text", "needle"])); assert!(parse(&invalid).is_err());
        let mut unpaired = valid.clone(); unpaired.extend(args(&["--base-catalog", "base.fcbc"])); assert!(parse(&unpaired).is_err());
    }
    #[test]
    fn base_paths_and_pins_cannot_enable_json_or_leak_into_other_commands() {
        for option in ["--base", "--base-catalog", "--base-catalog-digest"] {
            assert!(!wants_json(&args(&["refresh", "new", option, "--json"])));
            assert!(parse(&args(&["build", "new", "--output", "fresh", option, "old"])).is_err());
        }
    }
    #[test]
    fn inverted_layout_is_explicit_for_publication_and_never_changes_a_query_value() {
        let parsed = parse(&args(&["build", "saved", "--output", "new", "--inverted"])).unwrap();
        assert!(parsed.inverted);
        assert!(parse(&args(&["build", "saved", "--output", "new", "--inverted", "--inverted"])).is_err());
        assert!(!parse(&args(&["build", "saved", "--output", "--inverted"])).unwrap().inverted);
        let pin = "ab".repeat(32);
        let parsed = parse(&args(&["search", "saved", "--index", "i", "--index-digest", &pin, "--text", "--inverted"])).unwrap();
        assert_eq!(parsed.text.as_deref(), Some("--inverted"));
        assert!(!parsed.inverted);
        assert!(parse(&args(&["inspect", "saved", "--index", "i", "--index-digest", &pin, "--inverted"])).is_err());
    }
    #[test]
    fn paging_requires_explicit_mode_and_does_not_reinterpret_literal_values() {
        let pin = "ab".repeat(32);
        assert!(parse(&args(&["build", "saved", "--output", "new", "--paged"])).unwrap().paged);
        assert!(parse(&args(&["build", "saved", "--output", "new", "--paged", "--inverted"])).is_err());
        assert!(parse(&args(&["build", "saved", "--output", "new", "--paged", "--paged"])).is_err());
        let parsed = parse(&args(&["search", "saved", "--index", "i", "--index-digest", &pin, "--text", "--paged"])).unwrap();
        assert_eq!(parsed.text.as_deref(), Some("--paged")); assert!(!parsed.paged);
        assert!(parse(&args(&["refresh", "new", "--base", "old", "--index", "i", "--index-digest", &pin, "--output", "next", "--paged"])).is_err());
    }
}

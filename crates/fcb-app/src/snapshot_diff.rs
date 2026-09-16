#![forbid(unsafe_code)]

//! Read-only comparison of two explicitly selected saved observations. Uses the
//! public saved-membership merge, digest-verified pair loader and analysis diff.
//! Neither Git nor any original source root is accessed. All output is privately
//! encoded before delivery; cancellation never publishes a partial JSON object.

use std::{ffi::OsString, fs::File, io::Write, path::PathBuf};
use fcb::{ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb::search::{CaptureRequest, RawPath, ResourceBudget};
use fcb::search::snapshot::SnapshotLimits;
use fcb::search::paged_snapshot::{PagedSnapshot, PagedSnapshotError, PagedMember, PagedMemberData};
use fcb::search::snapshot_comparison::{SnapshotComparison, SnapshotPair, SnapshotChangeKind,
    SnapshotCompareError, ComparisonLimits, ComparisonQuality, CorrespondenceKind, ComparisonSide};
use crate::{AppError, SCHEMA, MANAGED_BYTES, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED,
    owner, generation, allocation, input};
use crate::args::{decimal, MAX_ARGUMENTS, MAX_ARGUMENT_BYTES, MAX_SINGLE_ARGUMENT};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};

const HELP: &str = "fcb snapshot diff BEFORE.fcbs AFTER.fcbs [--json] [--limit N] [--hunks N]\n\
    [--max-work N] [--max-edits N]\n\
Compares saved native-path membership and original bytes, never live source or Git history.\n\
Defaults: 100 changed/unavailable paths, 64 spans per pair, 8388608 total diff work, 256 edits.\n\
Exit: 0 equal comparable snapshots; 1 differences; 3 partial evidence/detail/output; 2 error; 130 canceled.\n";
#[derive(Debug)]
struct Failure { code: String, canceled: bool }
impl Failure {
    fn new(code: &str) -> Self { Self { code: code.to_owned(), canceled: false } }
    fn canceled() -> Self { Self { code: "SNAPSHOT_DIFF_CANCELED".to_owned(), canceled: true } }
}
impl From<OutputError> for Failure { fn from(e: OutputError) -> Self { Self::new(e.code()) } }
impl From<AppError> for Failure { fn from(e: AppError) -> Self { Self { code: e.code(), canceled: e.is_canceled() } } }
impl From<PagedSnapshotError> for Failure {
    fn from(e: PagedSnapshotError) -> Self { Self { code: e.to_string(), canceled: e == PagedSnapshotError::Canceled } }
}
impl From<SnapshotCompareError> for Failure {
    fn from(e: SnapshotCompareError) -> Self {
        let canceled = matches!(e, SnapshotCompareError::Canceled | SnapshotCompareError::Archive(PagedSnapshotError::Canceled)
            | SnapshotCompareError::Analysis(fcb::search::snapshot_comparison::ComparisonError::Canceled));
        Self { code: e.to_string(), canceled }
    }
}
struct Options { paths: Vec<PathBuf>, json: bool, help: bool, limit: usize, hunks: usize, work: u64, edits: usize }
fn takes_value(arg: &str) -> bool { matches!(arg, "--limit" | "--hunks" | "--max-work" | "--max-edits") }
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
    let mut out = Options { paths: Vec::with_capacity(2), json: false, help: false,
        limit: 100, hunks: 64, work: 8 * 1024 * 1024, edits: 256 };
    let (mut i, mut seen, mut positional) = (0, 0u8, false);
    while i < args.len() {
        let arg = &args[i]; i += 1;
        if !positional && arg == "--" { positional = true; continue; }
        if !positional && matches!(arg.to_str(), Some("--help" | "help" | "-h")) {
            if out.help { return Err(Failure::new("CLI_DUPLICATE_OPTION")); } out.help = true; continue;
        }
        if !positional && arg == "--json" {
            if out.json { return Err(Failure::new("CLI_DUPLICATE_OPTION")); } out.json = true; continue;
        }
        if !positional && arg.to_str().is_some_and(|a| a.starts_with('-')) {
            let option = arg.to_str().ok_or_else(|| Failure::new("CLI_UNKNOWN_OPTION"))?;
            let bit = match option { "--limit" => 1, "--hunks" => 2, "--max-work" => 4, "--max-edits" => 8,
                _ => return Err(Failure::new("CLI_UNKNOWN_OPTION")) };
            if seen & bit != 0 { return Err(Failure::new("CLI_DUPLICATE_OPTION")); } seen |= bit;
            let value = args.get(i).and_then(|a| a.to_str()).ok_or_else(|| Failure::new("CLI_MISSING_VALUE"))?; i += 1;
            let value = decimal(value).map_err(|e| Failure::new(e.code()))?;
            match option {
                "--limit" if value <= 4096 => out.limit = value as usize,
                "--hunks" if (1..=1024).contains(&value) => out.hunks = value as usize,
                "--max-work" if value <= 64 * 1024 * 1024 => out.work = value,
                "--max-edits" if value <= 512 => out.edits = value as usize,
                _ => return Err(Failure::new("CLI_ARGUMENT_LIMIT")),
            }
        } else {
            if out.paths.len() == 2 || arg.is_empty() { return Err(Failure::new("SNAPSHOT_DIFF_TWO_INPUTS_REQUIRED")); }
            out.paths.push(PathBuf::from(arg));
        }
    }
    if (out.help && (!out.paths.is_empty() || seen != 0)) || (!out.help && out.paths.len() != 2) {
        return Err(Failure::new("SNAPSHOT_DIFF_TWO_INPUTS_REQUIRED"));
    }
    Ok(out)
}

pub(crate) fn run(args: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write,
    mut canceled: impl FnMut() -> bool) -> u8 {
    let json = wants_json(args);
    let budget = match ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)) {
        Ok(b) => b, Err(_) => { let _ = stderr.write(b"SNAPSHOT_DIFF_RESOURCE_DENIED\n"); return EXIT_ERROR; }
    };
    let mut out = match Output::new(owner(), MAX_RESPONSE_BYTES, &budget, allocation(200)) {
        Ok(out) => out, Err(_) => { let _ = stderr.write(b"SNAPSHOT_DIFF_OUTPUT_DENIED\n"); return EXIT_ERROR; }
    };
    let result = parse(args).and_then(|options| execute(&options, &mut out, &budget, &mut canceled));
    let result = if result.is_ok() && canceled() { Err(Failure::canceled()) } else { result };
    let exit = match result {
        Ok(code) => code,
        Err(error) => {
            out.clear();
            if error_output(&mut out, json, &error.code).is_err() { let _ = stderr.write(b"SNAPSHOT_DIFF_ERROR_ENCODING\n"); return EXIT_ERROR; }
            if error.canceled { EXIT_CANCELED } else { EXIT_ERROR }
        }
    };
    let mut stopped = false;
    let mut stop = || { let value = exit != EXIT_CANCELED && canceled(); stopped |= value; value };
    let delivered = if json || matches!(exit, 0 | 1 | 3) { out.deliver(stdout, 4096, &mut stop) }
        else { out.deliver(stderr, 4096, &mut stop) };
    if delivered.is_err() {
        let _ = stderr.write(b"SNAPSHOT_DIFF_RESPONSE_INCOMPLETE effect=none\n");
        return if stopped { EXIT_CANCELED } else { EXIT_ERROR };
    }
    exit
}
fn error_output(out: &mut Output, json: bool, code: &str) -> Result<(), OutputError> {
    if !json { out.literal(code)?; return out.literal("\nUse fcb snapshot diff --help. No input was modified.\n"); }
    out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
    out.literal(",\"status\":\"error\",\"complete\":false,\"effect\":\"none\",\"error\":{\"code\":")?;
    out.quoted(code)?; out.literal(",\"subsystem\":\"snapshot-comparison\",\"retryable\":false}}\n")
}
fn open(path: &PathBuf, budget: &ResourceBudget, id: u64, canceled: &mut impl FnMut() -> bool) -> Result<PagedSnapshot<File>, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    let path = input::absolute(path)?;
    let (file, _) = input::open_regular(&path)?;
    Ok(PagedSnapshot::open(file, owner(), SnapshotLimits::default(), budget, allocation(id), canceled)?)
}
fn execute(options: &Options, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    if options.help {
        if options.json { out.literal("{\"schema\":")?; out.quoted(SCHEMA)?; out.literal(",\"status\":\"ok\",\"text\":")?; out.quoted(HELP)?; out.literal("}\n")?; }
        else { out.literal(HELP)?; }
        return Ok(EXIT_OK);
    }
    let mut before = open(&options.paths[0], budget, 201, canceled)?;
    let mut after = open(&options.paths[1], budget, 202, canceled)?;
    let mut cursor = SnapshotComparison::new(before.directory(), after.directory(), generation())?;
    let (mut shown, mut omitted, mut work, mut peak_pair) = (0usize, 0usize, 0u64, 0usize);
    let mut detail_complete = true;
    if options.json {
        out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
        out.literal(",\"status\":\"ok\",\"command\":\"snapshot-diff\",\"source_scope\":\"saved-observations-only\",\"live_roots_accessed\":false,\"history_inferred\":false,\"identity_scope\":\"response-local\",\"alignment\":\"native-path-pair-not-logical-file-continuity\",\"granularity\":\"original-bytes\",\"algorithm\":\"bounded-myers\",\"before_digest\":")?;
        out.quoted(&before.directory().digest().to_hex())?; out.literal(",\"after_digest\":")?; out.quoted(&after.directory().digest().to_hex())?;
        out.literal(",\"before_policy\":")?; out.quoted(before.directory().policy())?;
        out.literal(",\"after_policy\":")?; out.quoted(after.directory().policy())?;
        out.literal(",\"changes\":[")?;
    } else { out.literal("Saved-source byte comparison; no live root or Git history accessed.\n")?; }
    while let Some(change) = cursor.next(before.directory(), after.directory(), generation(), &mut *canceled)? {
        if change.kind() == SnapshotChangeKind::Unchanged { continue; }
        if shown == options.limit { omitted += 1; continue; }
        let path = RawPath::from_bytes(change.path(before.directory(), after.directory())?).to_path_buf();
        if options.json {
            if shown > 0 { out.literal(",")?; }
            out.literal("{\"path\":")?; out.path(&path)?; out.literal(",\"kind\":")?; out.quoted(change.kind().name())?;
            out.literal(",\"before\":")?; member(out, change.before_ordinal().and_then(|i| before.directory().member(i)))?;
            out.literal(",\"after\":")?; member(out, change.after_ordinal().and_then(|i| after.directory().member(i)))?;
        } else { out.literal(change.kind().name())?; out.literal(" ")?; out.path(&path)?; out.literal("\n")?; }
        shown += 1;
        if change.kind() == SnapshotChangeKind::Changed {
            if work == options.work {
                detail_complete = false;
                if options.json { out.literal(",\"refinement\":\"work-limit\",\"spans\":[{\"kind\":\"unresolved\",\"before\":")?;
                    full_range(out, change.before_ordinal().and_then(|i| before.directory().member(i)))?;
                    out.literal(",\"after\":")?; full_range(out, change.after_ordinal().and_then(|i| after.directory().member(i)))?;
                    out.literal("}],\"previews_omitted\":true")?;
                } else { out.literal("  Detail pending: global diff-work budget exhausted.\n")?; }
            } else {
                let old_ordinal = change.before_ordinal().ok_or_else(|| Failure::new("SNAPSHOT_DIFF_MEMBER"))?;
                let new_ordinal = change.after_ordinal().ok_or_else(|| Failure::new("SNAPSHOT_DIFF_MEMBER"))?;
                let requests = [request(old_ordinal as u64 + 1, 1)?, request(new_ordinal as u64 + 65_537, 2)?];
                let pair = SnapshotPair::load(&mut before, &mut after, change, requests, generation(), budget,
                    [allocation(203), allocation(204)], &mut *canceled)?;
                peak_pair = peak_pair.max(pair.bytes(ComparisonSide::Before).len() + pair.bytes(ComparisonSide::After).len());
                let diff = pair.compare(generation(), ComparisonLimits { max_work: options.work - work,
                    max_edit_distance: options.edits, ..Default::default() }, budget, allocation(205), &mut *canceled)?;
                work += diff.stats().work_units;
                let truncated = diff.spans().len() > options.hunks;
                detail_complete &= diff.quality() == ComparisonQuality::Exact && !truncated;
                if options.json {
                    out.literal(",\"refinement\":")?; out.quoted(match diff.quality() { ComparisonQuality::Exact => "exact", ComparisonQuality::WorkLimit => "work-limit", ComparisonQuality::EditLimit => "edit-limit" })?;
                    out.literal(",\"before_file_id\":")?; out.integer(requests[0].file().get())?;
                    out.literal(",\"after_file_id\":")?; out.integer(requests[1].file().get())?;
                    out.literal(",\"edit_distance\":")?;
                    if let Some(distance) = diff.stats().edit_distance { out.integer(distance as u64)?; } else { out.literal("null")?; }
                    out.literal(",\"span_count\":")?; out.integer(diff.spans().len() as u64)?;
                    out.literal(",\"spans_truncated\":")?; out.boolean(truncated)?;
                    out.literal(",\"spans\":[")?;
                }
                for (i, span) in diff.spans().iter().take(options.hunks).enumerate() {
                    if canceled() { return Err(Failure::canceled()); }
                    let kind = match span.kind() { CorrespondenceKind::Equal => "equal", CorrespondenceKind::Changed => "changed", CorrespondenceKind::Unresolved => "unresolved" };
                    if options.json {
                        if i > 0 { out.literal(",")?; }
                        out.literal("{\"kind\":")?; out.quoted(kind)?;
                        out.literal(",\"before\":")?; out.range(span.before())?;
                        out.literal(",\"after\":")?; out.range(span.after())?;
                        if span.kind() != CorrespondenceKind::Equal {
                            let old = diff.before_bytes(i).ok_or_else(|| Failure::new("SNAPSHOT_DIFF_RANGE"))?;
                            let new = diff.after_bytes(i).ok_or_else(|| Failure::new("SNAPSHOT_DIFF_RANGE"))?;
                            out.literal(",\"before_preview_hex\":")?; out.hex(&old[..old.len().min(64)])?;
                            out.literal(",\"after_preview_hex\":")?; out.hex(&new[..new.len().min(64)])?;
                            out.literal(",\"preview_truncated\":")?; out.boolean(old.len() > 64 || new.len() > 64)?;
                        }
                        out.literal("}")?;
                    } else if span.kind() != CorrespondenceKind::Equal {
                        out.literal("  ")?; out.literal(kind)?; out.literal(" before bytes ")?; out.range(span.before())?;
                        out.literal(" after bytes ")?; out.range(span.after())?; out.literal("\n")?;
                    }
                }
                if options.json { out.literal("]")?; }
            }
        }
        if options.json { out.literal("}")?; }
    }
    if canceled() { return Err(Failure::canceled()); }
    let stats = cursor.stats();
    let evidence_complete = cursor.membership_comparable() && stats.uncertain_paths() == 0;
    let complete = evidence_complete && detail_complete && omitted == 0;
    if options.json {
        out.literal("],\"membership_comparable\":")?; out.boolean(cursor.membership_comparable())?;
        out.literal(",\"content_evidence_complete\":")?; out.boolean(evidence_complete)?;
        out.literal(",\"detail_complete\":")?; out.boolean(detail_complete && omitted == 0)?;
        out.literal(",\"complete\":")?; out.boolean(complete)?;
        out.literal(",\"listing_truncated\":")?; out.boolean(omitted > 0)?;
        for (key, number) in [("compared_paths", stats.compared_paths as u64), ("unchanged", stats.unchanged as u64),
            ("changed", stats.changed as u64), ("added", stats.added as u64), ("removed", stats.removed as u64),
            ("only_before", stats.only_before as u64), ("only_after", stats.only_after as u64), ("unavailable", stats.unavailable as u64),
            ("omitted_changes", omitted as u64), ("diff_work_units", work), ("peak_retained_pair_bytes", peak_pair as u64),
            ("archive_validation_bytes", before.directory().validation_stats().bytes_read + after.directory().validation_stats().bytes_read),
            ("member_payload_bytes_loaded", before.load_stats().bytes_read + after.load_stats().bytes_read)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(number)?;
        }
        out.literal("}\n")?;
    } else {
        out.literal(if complete { "Comparison complete within saved scope.\n" } else { "PARTIAL comparison: consult evidence, detail and listing limits.\n" })?;
        out.literal("Known differences: ")?; out.literal(&stats.known_differences().to_string())?;
        out.literal("; uncertain paths: ")?; out.literal(&stats.uncertain_paths().to_string())?; out.literal("\n")?;
    }
    Ok(if !complete { EXIT_PARTIAL } else if stats.known_differences() > 0 { 1 } else { EXIT_OK })
}
fn request(file: u64, revision: u64) -> Result<CaptureRequest, Failure> {
    let file = FileId::new(owner(), file).map_err(|_| Failure::new("SNAPSHOT_DIFF_IDENTITY"))?;
    let revision = SourceRevision::new(owner(), revision).map_err(|_| Failure::new("SNAPSHOT_DIFF_IDENTITY"))?;
    CaptureRequest::new(file, revision).map_err(|_| Failure::new("SNAPSHOT_DIFF_IDENTITY"))
}
fn full_range(out: &mut Output, member: Option<PagedMember<'_>>) -> Result<(), Failure> {
    let member = member.ok_or_else(|| Failure::new("SNAPSHOT_DIFF_MEMBER"))?;
    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(member.observed_bytes))
        .map_err(|_| Failure::new("SNAPSHOT_DIFF_RANGE"))?;
    out.range(range)?; Ok(())
}
fn member(out: &mut Output, member: Option<PagedMember<'_>>) -> Result<(), OutputError> {
    let Some(member) = member else { return out.literal("null"); };
    out.literal("{\"ordinal\":")?; out.integer(member.ordinal as u64)?;
    out.literal(",\"observed_bytes\":")?; out.integer(member.observed_bytes)?;
    match member.data {
        PagedMemberData::Captured { digest, .. } => {
            out.literal(",\"captured\":true,\"source_digest\":")?; out.quoted(&digest.to_hex())?;
        }
        PagedMemberData::Unavailable(reason) => {
            out.literal(",\"captured\":false,\"unavailable_reason\":")?; out.quoted(reason)?;
        }
    }
    out.literal("}")
}

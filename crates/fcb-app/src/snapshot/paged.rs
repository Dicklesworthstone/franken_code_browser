#![forbid(unsafe_code)]

//! Read-only CLI routes over the public paged archive APIs. The sole open is
//! the archive named by the caller; member names never become filesystem paths.

use std::fs::File;
use fcb::{ByteOffset, ByteRange, FileId, SourceRevision};
use fcb::search::{RawPath, ResourceBudget, StreamingNeedle, StreamReadStep,
    ReaderLimits, ReadingTarget, ReadingSeekState, ReadingWindowOptions};
use fcb::search::reader::LineNumber;
use fcb::search::paged_snapshot::{PagedSnapshot, PagedSnapshotError, PagedMemberData,
    PagedQuery, PagedQueryOptions, PagedQueryState, PagedSearchError, PagedCapture, SnapshotDirectory};
use crate::{allocation, file_id, generation, owner, revision, input, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL};
use crate::output::{Output, OutputError};
use super::{Settings, Mode, Failure, SnapshotLimits, MAX_SNAPSHOT_BYTES, begin};

impl From<PagedSnapshotError> for Failure {
    fn from(error: PagedSnapshotError) -> Self { Self { code: error.to_string(), canceled: error == PagedSnapshotError::Canceled } }
}
impl From<PagedSearchError> for Failure {
    fn from(error: PagedSearchError) -> Self {
        Self { code: error.to_string(), canceled: matches!(error, PagedSearchError::Canceled
            | PagedSearchError::Archive(PagedSnapshotError::Canceled)) }
    }
}
fn reader_error(error: fcb::search::ReaderError) -> Failure {
    Failure { code: error.to_string(), canceled: error == fcb::search::ReaderError::Canceled }
}

pub(super) fn execute(settings: &Settings, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    let path = input::absolute(settings.source.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?)?;
    let (file, metadata) = input::open_regular(&path)?;
    if metadata.len() > MAX_SNAPSHOT_BYTES as u64 { return Err(Failure::new("SNAPSHOT_LIMIT")); }
    let mut archive = PagedSnapshot::open(file, owner(), SnapshotLimits::default(), budget, allocation(104), &mut *canceled)?;
    match settings.mode {
        Mode::Inspect => inspect(settings, archive.directory(), out, canceled),
        Mode::Search => search(settings, &mut archive, out, budget, canceled),
        Mode::Read => read(settings, &mut archive, out, budget, canceled),
        _ => Err(Failure::new("SNAPSHOT_UNKNOWN_COMMAND")),
    }
}
fn summary(out: &mut Output, directory: &SnapshotDirectory) -> Result<(), OutputError> {
    out.literal(",\"source_scope\":\"saved-observations-only\",\"live_roots_accessed\":false,\"snapshot_digest\":")?;
    out.quoted(&directory.digest().to_hex())?;
    out.literal(",\"policy\":")?; out.quoted(directory.policy())?;
    out.literal(",\"discovery_complete\":")?; out.boolean(directory.discovery_complete())?;
    out.literal(",\"known_files\":")?; out.integer(directory.len() as u64)?;
    out.literal(",\"captured_files\":")?; out.integer(directory.captured_files() as u64)?;
    out.literal(",\"unavailable_files_count\":")?; out.integer((directory.len() - directory.captured_files()) as u64)?;
    out.literal(",\"captured_bytes\":")?; out.integer(directory.source_bytes())?;
    out.literal(",\"archive_validation_bytes\":")?; out.integer(directory.validation_stats().bytes_read)?;
    out.literal(",\"archive_validation_read_calls\":")?; out.integer(directory.validation_stats().read_calls)?;
    out.literal(",\"catalog_reserved_bytes\":")?; out.integer(directory.retained_charge() as u64)
}
fn inspect(settings: &Settings, directory: &SnapshotDirectory, out: &mut Output,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if settings.json {
        begin(out, "snapshot-inspect")?; summary(out, directory)?;
        out.literal(",\"listing_truncated\":")?; out.boolean(directory.len() > settings.limit)?;
        out.literal(",\"member_payload_bytes_loaded\":\"0\",\"files\":[")?;
    } else { out.literal("Verified saved observations; metadata retained, no live roots accessed.\n")?; }
    for entry in directory.members().take(settings.limit) {
        if canceled() { return Err(Failure::canceled()); }
        let path = RawPath::from_bytes(entry.path).to_path_buf();
        let captured = matches!(entry.data, PagedMemberData::Captured { .. });
        if settings.json {
            if entry.ordinal != 0 { out.literal(",")?; }
            out.literal("{\"path\":")?; out.path(&path)?;
            out.literal(",\"observed_bytes\":")?; out.integer(entry.observed_bytes)?;
            out.literal(",\"captured\":")?; out.boolean(captured)?;
            out.literal(",\"unavailable_reason\":")?;
            match entry.data { PagedMemberData::Unavailable(reason) => out.quoted(reason)?, _ => out.literal("null")? }
            out.literal("}")?;
        } else { out.path(&path)?; out.literal(if captured { " captured\n" } else { " unavailable\n" })?; }
    }
    if settings.json { out.literal("]}\n")?; }
    Ok(if directory.discovery_complete() && directory.captured_files() == directory.len() && directory.len() <= settings.limit {
        EXIT_OK
    } else { EXIT_PARTIAL })
}
fn search(settings: &Settings, archive: &mut PagedSnapshot<File>, out: &mut Output,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    let needle = match (&settings.text, &settings.raw_needle) {
        (Some(text), None) => StreamingNeedle::text(owner(), text, budget, allocation(105)),
        (None, Some(bytes)) => StreamingNeedle::raw(owner(), bytes, budget, allocation(105)),
        _ => return Err(Failure::new("CLI_INVALID_NEEDLE")),
    }.map_err(crate::AppError::from)?;
    let options = PagedQueryOptions { generation: generation(), first_file: file_id(),
        first_revision: revision(), max_matches: settings.limit };
    let mut query = PagedQuery::new(archive, &needle, options, budget, [allocation(106), allocation(107), allocation(108)])?;
    while query.state() == PagedQueryState::Pending {
        query.step(StreamReadStep::default(), generation(), budget, &mut *canceled)?;
    }
    let report = query.finish()?;
    if canceled() { return Err(Failure::canceled()); }
    if settings.json {
        begin(out, "snapshot-search")?; summary(out, archive.directory())?;
        out.literal(",\"identity_scope\":\"response-local\",\"engine\":\"bounded-stream-exact\",\"payload_residency\":\"one-verified-member\",\"workspace_complete\":")?;
        out.boolean(report.is_complete())?;
        out.literal(",\"truncated\":")?; out.boolean(report.truncated())?;
        out.literal(",\"unsupported_text_files_count\":")?; out.integer(report.stats().unsupported_files as u64)?;
        out.literal(",\"matches_seen\":")?; out.integer(report.matches_seen())?;
        out.literal(",\"files_searched\":")?; out.integer(report.stats().files_searched as u64)?;
        out.literal(",\"peak_source_bytes\":")?; out.integer(report.stats().peak_source_bytes as u64)?;
        out.literal(",\"member_payload_bytes_loaded\":")?; out.integer(archive.load_stats().bytes_read)?;
        out.literal(",\"member_read_calls\":")?; out.integer(archive.load_stats().read_calls)?;
        out.literal(",\"scanned_bytes\":")?; out.integer(report.stats().scanned_bytes)?;
        out.literal(",\"hits\":[")?;
    } else { out.literal(if report.is_complete() { "Complete saved-scope search; no live roots accessed.\n" } else { "PARTIAL saved-scope search; no live roots accessed.\n" })?; }
    for (ordinal, hit) in report.hits().iter().enumerate() {
        if canceled() { return Err(Failure::canceled()); }
        let member = archive.directory().member(hit.ordinal()).ok_or_else(|| Failure::new("SNAPSHOT_MEMBER_MISSING"))?;
        let path = RawPath::from_bytes(member.path).to_path_buf();
        if settings.json {
            if ordinal != 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(hit.file().get())?;
            out.literal(",\"revision\":")?; out.integer(hit.revision().get())?;
            out.literal(",\"path\":")?; out.path(&path)?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
            out.literal(",\"matched_text\":")?;
            match settings.text.as_deref() { Some(text) => out.quoted(text)?, None => out.literal("null")? }
            out.literal(",\"matched_raw_hex\":")?;
            match settings.raw_needle.as_deref() { Some(bytes) => out.hex(bytes)?, None => out.literal("null")? }
            out.literal("}")?;
        } else {
            out.path(&path)?; out.literal(" bytes ")?; out.literal(&hit.original_range().start().get().to_string())?;
            out.literal("..")?; out.literal(&hit.original_range().end().get().to_string())?; out.literal("\n")?;
        }
    }
    if settings.json { out.literal("]}\n")?; }
    Ok(if !report.is_complete() { EXIT_PARTIAL } else if report.hits().is_empty() { EXIT_NO_MATCH } else { EXIT_OK })
}
fn read(settings: &Settings, archive: &mut PagedSnapshot<File>, out: &mut Output,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    let native = settings.member.as_deref().ok_or_else(|| Failure::new("SNAPSHOT_MEMBER_REQUIRED"))?;
    let ordinal = archive.directory().find_path(native).ok_or_else(|| Failure::new("SNAPSHOT_MEMBER_NOT_FOUND"))?;
    let file = FileId::new(owner(), 1 + ordinal as u64).map_err(|_| Failure::new("SNAPSHOT_IDENTITY_EXHAUSTED"))?;
    let revision = SourceRevision::new(owner(), 1 + ordinal as u64).map_err(|_| Failure::new("SNAPSHOT_IDENTITY_EXHAUSTED"))?;
    let capture = PagedCapture::load(archive, ordinal, file, revision, budget, [allocation(105), allocation(106)], &mut *canceled)?;
    if settings.json {
        begin(out, "snapshot-read")?; summary(out, archive.directory())?;
        out.literal(",\"identity_scope\":\"response-local\",\"file_id\":")?; out.integer(file.get())?;
        out.literal(",\"revision\":")?; out.integer(revision.get())?;
        out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(native).to_path_buf())?;
        out.literal(",\"member_payload_bytes_loaded\":")?; out.integer(archive.load_stats().bytes_read)?;
        out.literal(",\"source_digest\":")?; out.quoted(&capture.source_digest().to_hex())?;
    }
    if settings.raw {
        let start = usize::try_from(settings.offset).map_err(|_| Failure::new("CLI_INVALID_RANGE"))?;
        if start > capture.bytes().len() { return Err(Failure::new("CLI_INVALID_RANGE")); }
        let end = start.saturating_add(settings.window_bytes).min(capture.bytes().len());
        let range = ByteRange::new(ByteOffset::new(start as u64), ByteOffset::new(end as u64)).map_err(|_| Failure::new("CLI_INVALID_RANGE"))?;
        let eof = end == capture.bytes().len();
        if settings.json {
            out.literal(",\"original_range\":")?; out.range(range)?;
            out.literal(",\"original_hex\":")?; out.hex(&capture.bytes()[start..end])?;
            out.literal(",\"reaches_eof\":")?; out.boolean(eof)?;
            out.literal(",\"next_offset\":")?;
            if eof { out.literal("null")?; } else { out.integer(end as u64)?; }
            out.literal("}\n")?;
        } else { out.hex(&capture.bytes()[start..end])?; out.literal("\n")?; }
        return Ok(if eof { EXIT_OK } else { EXIT_PARTIAL });
    }
    let reader = capture.reader(ReaderLimits::default(), budget, allocation(107)).map_err(reader_error)?;
    let target = match settings.line {
        Some(line) => ReadingTarget::Line(LineNumber::new(line).map_err(|_| Failure::new("CLI_INVALID_LINE"))?),
        None => ReadingTarget::Byte(ByteOffset::new(settings.offset)),
    };
    let mut seek = reader.seek(target, generation()).map_err(reader_error)?;
    while seek.state() == ReadingSeekState::Pending {
        seek.step(64 * 1024, generation(), &mut *canceled).map_err(reader_error)?;
    }
    let at = match seek.state() {
        ReadingSeekState::Ready(at) => at,
        ReadingSeekState::Canceled => return Err(Failure::canceled()),
        _ => return Err(Failure::new("SNAPSHOT_LINE_OUT_OF_BOUNDS")),
    };
    let window = reader.window(at, generation(), ReadingWindowOptions { max_bytes: settings.window_bytes, max_lines: settings.lines },
        budget, allocation(108), &mut *canceled).map_err(reader_error)?;
    if settings.json {
        out.literal(",\"original_range\":")?; out.range(window.raw_range())?;
        let (first, last) = window.raw_range().as_usize_bounds().map_err(|_| Failure::new("CLI_INVALID_RANGE"))?;
        out.literal(",\"original_hex\":")?; out.hex(&capture.bytes()[first..last])?;
        out.literal(",\"text\":")?; out.quoted(window.text())?;
        out.literal(",\"contains_replacements\":")?; out.boolean(window.has_replacements())?;
        out.literal(",\"reaches_eof\":")?; out.boolean(window.reaches_eof())?;
        out.literal(",\"next_offset\":")?;
        match window.next_anchor() { Some(next) => out.integer(next.offset().get())?, None => out.literal("null")? }
        out.literal(",\"lines\":[")?;
        for (i, row) in window.lines().iter().enumerate() {
            if canceled() { return Err(Failure::canceled()); }
            if i != 0 { out.literal(",")?; }
            out.literal("{\"number\":")?; out.integer(row.number)?;
            out.literal(",\"original_range\":")?; out.range(row.raw_range)?;
            out.literal(",\"text\":")?; out.quoted(window.line_text(i).ok_or_else(|| Failure::new("SNAPSHOT_INVALID_ROW"))?)?;
            out.literal(",\"continued_before\":")?; out.boolean(row.continued_before)?;
            out.literal(",\"continued_after\":")?; out.boolean(row.continued_after)?; out.literal("}")?;
        }
        out.literal("]}\n")?;
    } else { out.human_text(window.text())?; if !window.text().ends_with('\n') { out.literal("\n")?; } }
    Ok(if window.reaches_eof() { EXIT_OK } else { EXIT_PARTIAL })
}

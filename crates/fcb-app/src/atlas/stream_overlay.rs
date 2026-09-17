#![forbid(unsafe_code)]

//! CLI wiring for the public resumable atlas stream. No second filesystem walk,
//! capture/index pass, matcher, decoder or source-sized retained allocation.
use fcb::map::workspace::stream_search::{AtlasStreamError, AtlasStreamFileState,
    AtlasStreamLimits, AtlasStreamReport, AtlasStreamSearch, AtlasStreamState, AtlasStreamStop};
use fcb::map::VisibleParcel;
use fcb::search::{DetectedEncoding, ExtentConsistency, StreamReadState, StreamReadStep, StreamingNeedle};
use fcb::source::SourceError;
use super::*;

impl From<AtlasStreamError> for Error { fn from(e: AtlasStreamError) -> Self { Self::Stream(e) } }

pub(super) fn build<'a, 'c>(atlas: &'a WorkspaceAtlas<'c>, root: &Path, text: &str,
    options: &Options, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
    -> Result<AtlasStreamReport<'a, 'c>, Error> {
    if canceled() { return Err(AppError::Canceled.into()); }
    let needle = StreamingNeedle::text(owner(), text, budget, allocation(80)).map_err(AppError::from)?;
    let limits = AtlasStreamLimits { max_matches: options.match_limit,
        max_bytes: options.text_scan_bytes, max_read_calls: options.stream_max_calls };
    let mut search = AtlasStreamSearch::new(atlas, &needle, crate::revision(), generation(), limits,
        budget, [allocation(81), allocation(82)])?;
    while search.state() == AtlasStreamState::Pending {
        search.step(StreamReadStep::default(), generation(), |_, relative| {
            let path = workspace::checked_source_path(root, relative)?;
            input::open_regular(&path).map(|(file, _)| file).map_err(|_| SourceError::CaptureUnavailable)
        }, &mut *canceled)?;
    }
    Ok(search.finish()?)
}

pub(super) fn counts(out: &mut Output, report: &AtlasStreamReport<'_, '_>, parcel: VisibleParcel)
    -> Result<(), Error> {
    out.literal(",\"retained_text_matches\":")?;
    if parcel.detail() == AtlasDetail::SiblingGroup { out.literal("null")?; }
    else {
        let (occurrences, files) = report.retained_matches_in(parcel.node())?;
        out.literal("{\"occurrences\":")?; out.integer(occurrences as u64)?;
        out.literal(",\"files\":")?; out.integer(files as u64)?; out.literal("}")?;
    }
    Ok(())
}

pub(super) fn json(out: &mut Output, report: &AtlasStreamReport<'_, '_>, index: &AtlasIndex<'_>,
    focus: AtlasNodeId, camera: Camera2D, canceled: &mut impl FnMut() -> bool) -> Result<(), Error> {
    report.validate_delivery(report.atlas(), generation())?;
    let stats = report.stats();
    out.literal(",\"text_search\":{\"strategy\":\"streaming-whole-file\",\"mode\":\"exact-decoded-text\",\"scope\":\"catalogued-whole-files\",\"retention\":\"literal-witnesses-only\",\"capture_limits_applied\":false,\"captured_bytes\":\"0\",\"generation\":")?;
    out.integer(report.generation().get())?;
    out.literal(",\"workspace_complete\":")?; out.boolean(report.is_complete())?;
    out.literal(",\"counts_complete\":")?; out.boolean(report.is_complete())?;
    out.literal(",\"truncated\":")?; out.boolean(report.stop_reason() == Some(AtlasStreamStop::MatchLimit))?;
    out.literal(",\"matches_counted\":")?; out.integer(stats.matches_seen)?;
    out.literal(",\"retained_matches\":")?; out.integer(report.hits().len() as u64)?;
    out.literal(",\"files_examined\":")?; out.integer(report.files_examined() as u64)?;
    out.literal(",\"unexamined_files\":")?; out.integer(report.unexamined_files() as u64)?;
    out.literal(",\"incomplete_files\":")?; out.integer(stats.incomplete_files as u64)?;
    out.literal(",\"payload_bytes_read\":")?; out.integer(stats.bytes_read)?;
    out.literal(",\"scanned_input_bytes\":")?; out.integer(stats.scanned_bytes)?;
    out.literal(",\"read_calls\":")?; out.integer(stats.read_calls)?;
    out.literal(",\"interrupted_calls\":")?; out.integer(stats.interrupted_calls)?;
    out.literal(",\"peak_input_buffer_bytes\":")?; out.integer(stats.peak_input_buffer_bytes as u64)?;
    out.literal(",\"retained_witness_bytes\":")?; out.integer(report.retained_witness_bytes() as u64)?;
    out.literal(",\"max_scan_bytes\":")?; out.integer(report.limits().max_bytes)?;
    out.literal(",\"max_read_calls\":")?; out.integer(report.limits().max_read_calls)?;
    out.literal(",\"stop_reason\":")?;
    if let Some(stop) = report.stop_reason() { out.quoted(stop.code())?; } else { out.literal("null")?; }
    out.literal(",\"literal_text\":")?; out.quoted(report.literal())?;
    out.literal(",\"hits\":[")?;
    for (i, hit) in report.hits().iter().enumerate() {
        if canceled() { return Err(AppError::Canceled.into()); }
        if i > 0 { out.literal(",")?; }
        let selected = report.select_hit(i, report.atlas(), generation())?;
        out.literal("{\"hit_index\":")?; out.integer(i as u64)?;
        out.literal(",\"node\":")?; node_record(out, index, hit.node())?;
        out.literal(",\"file_id\":")?; out.integer(hit.file().get())?;
        out.literal(",\"revision\":")?; out.integer(hit.revision().get())?;
        out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
        out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
        out.literal(",\"matched_text\":")?; out.quoted(selected.matched_text())?;
        out.literal(",\"original_hex\":")?; out.hex(selected.original_bytes())?;
        out.literal(",\"decoded_range\":null,\"logical_rect\":")?;
        if let Some(rect) = projected_selection(index, hit.node(), focus, camera)? { rectangle(out, rect)?; }
        else { out.literal("null")?; }
        out.literal("}")?;
    }
    out.literal("],\"files\":[")?;
    for (i, file) in report.files().iter().enumerate() {
        if canceled() { return Err(AppError::Canceled.into()); }
        if i > 0 { out.literal(",")?; }
        out.literal("{\"node\":")?; node_record(out, index, file.node())?;
        out.literal(",\"file_id\":")?; out.integer(file.file().get())?;
        out.literal(",\"revision\":")?; out.integer(file.revision().get())?;
        out.literal(",\"state\":")?;
        out.quoted(match file.state() { AtlasStreamFileState::Unavailable(_) => "unavailable",
            AtlasStreamFileState::Scanned(state) => state.code() })?;
        out.literal(",\"whole_file_complete\":")?; out.boolean(file.is_complete())?;
        out.literal(",\"error_code\":")?;
        match file.state() {
            AtlasStreamFileState::Unavailable(e) => out.quoted(&e.to_string())?,
            AtlasStreamFileState::Scanned(StreamReadState::Failed(e)) => out.quoted(&e.to_string())?,
            _ => out.literal("null")?,
        }
        out.literal(",\"encoding\":")?;
        match file.encoding() { Some(DetectedEncoding::Utf8 { .. }) => out.quoted("utf8")?,
            Some(DetectedEncoding::Utf16Le) => out.quoted("utf16le")?, Some(DetectedEncoding::Utf16Be) => out.quoted("utf16be")?,
            _ => out.literal("null")? }
        out.literal(",\"observed_length\":")?;
        if let Some(n) = file.observed_length() { out.integer(n.get())?; } else { out.literal("null")?; }
        out.literal(",\"final_length\":")?;
        if let Some(n) = file.final_length() { out.integer(n.get())?; } else { out.literal("null")?; }
        out.literal(",\"consistency\":")?;
        match file.consistency() {
            Some(ExtentConsistency::UnchangedMetadata) => out.quoted("unchanged-metadata-not-atomic")?,
            Some(ExtentConsistency::ChangedDuringRead) => out.quoted("changed-during-read")?,
            Some(ExtentConsistency::ShortRead) => out.quoted("short-read")?,
            Some(_) => out.quoted("metadata-unavailable")?, None => out.literal("null")?,
        }
        out.literal(",\"unsupported_at\":")?;
        if let Some(offset) = file.unsupported_at() { out.integer(offset.get())?; } else { out.literal("null")?; }
        out.literal(",\"payload_bytes_read\":")?; out.integer(file.stats().bytes_read)?;
        out.literal(",\"scanned_input_bytes\":")?; out.integer(file.stats().scanned_bytes)?;
        out.literal(",\"read_calls\":")?; out.integer(file.stats().read_calls)?;
        out.literal(",\"matches_counted\":")?; out.integer(file.matches_seen())?;
        out.literal(",\"first_hit\":")?; out.integer(file.first_hit() as u64)?;
        out.literal(",\"stored_hits\":")?; out.integer(file.hit_count() as u64)?;
        out.literal("}")?;
    }
    out.literal("]}")?;
    Ok(())
}

pub(super) fn human(out: &mut Output, report: &AtlasStreamReport<'_, '_>, canceled: &mut impl FnMut() -> bool)
    -> Result<(), Error> {
    report.validate_delivery(report.atlas(), generation())?;
    out.literal("Whole-file atlas text search; exact literal witnesses only, no complete captures\n")?;
    for hit in report.hits() {
        if canceled() { return Err(AppError::Canceled.into()); }
        out.path(&report.atlas().entry(hit.node())?.path().raw().to_path_buf())?;
        out.literal(" bytes ")?; out.literal(&hit.original_range().start().get().to_string())?;
        out.literal("..")?; out.literal(&hit.original_range().end().get().to_string())?; out.literal("\n")?;
    }
    out.literal("Literal text: ")?; out.human_text(report.literal())?; out.literal("\n")?;
    out.literal("Files examined: ")?; out.literal(&report.files_examined().to_string())?;
    out.literal("; unexamined: ")?; out.literal(&report.unexamined_files().to_string())?;
    out.literal("; incomplete: ")?; out.literal(&report.stats().incomplete_files.to_string())?; out.literal("\n")?;
    if !report.is_complete() {
        out.literal("PARTIAL search: ")?;
        out.literal(report.stop_reason().map_or("incomplete discovery or source observation", AtlasStreamStop::code))?;
        out.literal("\n")?;
    }
    Ok(())
}

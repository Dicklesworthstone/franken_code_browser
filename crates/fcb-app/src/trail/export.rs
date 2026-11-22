#![forbid(unsafe_code)]

//! Source-specific JSON evidence packs, not a Markdown exporter or source
//! decoder. Exact original selections remain byte hex, including malformed
//! text and ranges inside a scalar. Scalar budgets count the serialized JSON
//! representation, including escaped metadata, not characters in decoded code.

use super::*;
use fcb::search::trail::{Sha256Digest, TrailExportCursor, TrailExportLimits, TrailExportOutcome, TrailExportStats};

pub(crate) const PACK_SCHEMA: &str = "fcb.trail-source-pack/1";
const ENTRY_METADATA_BYTES: usize = 512 * 1024;
const ENTRY_OVERHEAD_RESERVE: usize = 256;
const TAIL_RESERVE: usize = 2048;
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;

/// Archive envelopes are validated once and only their bounded directories stay
/// resident. A parent reservation backs the entire child accounting domain, so
/// several valid directories cannot bypass the command's managed-byte ceiling.
/// Field order releases archive leases before their accounting/reservation.
struct Archives {
    handles: [Option<PagedSnapshot<File>>; MAX_EXPORT_ARCHIVES],
    count: usize,
    supplied: usize,
    validation_bytes_admitted: u64,
    validation_bytes_read: u64,
    validation_read_calls: u64,
    directory_budget: ResourceBudget,
    _reservation: ResourceLease,
}
impl Archives {
    fn open(s: &Settings, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, Failure> {
        if s.archives.is_empty() { return Err(Failure::new("TRAIL_ARCHIVE_REQUIRED")); }
        if s.archives.len() > MAX_EXPORT_ARCHIVES { return Err(Failure::new("TRAIL_EXPORT_ARCHIVE_LIMIT")); }
        let reservation = budget.try_reserve_managed(owner(), allocation(214), ByteLength::new(MAX_DIRECTORY_BYTES))
            .map_err(|_| Failure::new("TRAIL_EXPORT_DIRECTORY_ADMISSION"))?;
        let directory_budget = ResourceBudget::new(owner(), ByteLength::new(MAX_DIRECTORY_BYTES))
            .map_err(|_| Failure::new("TRAIL_EXPORT_DIRECTORY_ADMISSION"))?;
        let mut archives = Self { handles: std::array::from_fn(|_| None), count: 0,
            supplied: s.archives.len(), validation_bytes_admitted: 0, validation_bytes_read: 0,
            validation_read_calls: 0, directory_budget, _reservation: reservation };
        for (ordinal, path) in s.archives.iter().enumerate() {
            if canceled() { return Err(Failure::canceled()); }
            let path = input::absolute(path)?;
            let (file, observed) = input::open_regular(&path)?;
            let mut limits = SnapshotLimits::default();
            if observed.len() > limits.max_document_bytes as u64 { return Err(Failure::new("SNAPSHOT_LIMIT")); }
            // PagedSnapshot checks the declared envelope against this limit
            // before body reads, then reads at most one byte to establish EOF.
            // Charge that probe even when it returns zero. Descriptor growth or
            // a forged header therefore cannot exceed admission on a failure.
            let admission = observed.len().checked_add(1)
                .ok_or_else(|| Failure::new("TRAIL_EXPORT_VALIDATION_BYTE_LIMIT"))?;
            if admission > s.export_validation_bytes.saturating_sub(archives.validation_bytes_admitted) {
                return Err(Failure::new("TRAIL_EXPORT_VALIDATION_BYTE_LIMIT"));
            }
            archives.validation_bytes_admitted += admission;
            limits.max_document_bytes = observed.len() as usize;
            let candidate = PagedSnapshot::open(file, owner(), limits, &archives.directory_budget,
                allocation(220 + ordinal as u64), &mut *canceled).map_err(fail)?;
            let stats = candidate.directory().validation_stats();
            archives.validation_bytes_read += stats.bytes_read;
            archives.validation_read_calls += stats.read_calls;
            if archives.validation_bytes_read > archives.validation_bytes_admitted {
                return Err(Failure::new("TRAIL_EXPORT_VALIDATION_BYTE_LIMIT"));
            }
            // Aliases/copies are equivalent only after each supplied file has
            // passed full validation. Retain the first verified handle; count
            // all validation work and never deduplicate explicit trail visits.
            if !archives.handles.iter().flatten().any(|prior| prior.directory().digest() == candidate.directory().digest()) {
                archives.handles[archives.count] = Some(candidate);
                archives.count += 1;
            }
        }
        Ok(archives)
    }

    fn for_digest(&mut self, digest: Sha256Digest) -> Result<&mut PagedSnapshot<File>, Failure> {
        let position = self.handles.iter().position(|archive| archive.as_ref()
            .is_some_and(|archive| archive.directory().digest() == digest)).unwrap_or(0);
        // If the user omitted this digest, the existing cursor produces its
        // explicit OtherArchive row before any member read or source admission.
        self.handles[position].as_mut().ok_or_else(|| Failure::new("TRAIL_ARCHIVE_REQUIRED"))
    }

    fn encode_validation(&self, out: &mut Output, maximum: u64) -> Result<(), OutputError> {
        let directory = self.directory_budget.accounting();
        out.literal("{\"routing\":\"verified-archive-digest\",\"duplicate_policy\":\"first-verified-handle\"")?;
        for (key, value) in [("supplied_archives", self.supplied as u64), ("distinct_archives", self.count as u64),
            ("duplicate_archives", (self.supplied - self.count) as u64),
            ("validation_bytes_admitted", self.validation_bytes_admitted),
            ("validation_bytes_read", self.validation_bytes_read), ("validation_read_calls", self.validation_read_calls),
            ("retained_directory_bytes", directory.reserved().get()), ("peak_directory_bytes", directory.peak_reserved().get()),
            ("max_archives", MAX_EXPORT_ARCHIVES as u64), ("max_validation_bytes", maximum),
            ("max_directory_bytes", MAX_DIRECTORY_BYTES)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"validation_admission\":\"observed-file-length-plus-one-eof-probe\"}")
    }
}

pub(super) fn run(s: &Settings, view: TrailView<'_>, receipt: &mut Output,
    budget: &ResourceBudget, effect: &mut Effect, canceled: &mut impl FnMut() -> bool)
    -> Result<u8, Failure> {
    let destination = input::absolute(s.output.as_deref().ok_or_else(|| Failure::new("TRAIL_OUTPUT_REQUIRED"))?)?;
    let mut archives = Archives::open(s, budget, canceled)?;
    let mut pack = Output::new(owner(), s.export_bytes, budget, allocation(210))?;
    header(&mut pack, s, view, &archives)?;
    let mut metadata = Output::new(owner(), ENTRY_METADATA_BYTES, budget, allocation(211))?;
    let mut reserved_bytes = pack.as_bytes().len() + TAIL_RESERVE;
    let mut reserved_characters = scalars(pack.as_bytes())? + TAIL_RESERVE;
    // All admitted notes and references remain visible even if source payloads
    // do not fit. Reserve their actual escaped encoding before payload loads.
    // If metadata itself cannot fit, refuse before creating any destination.
    for (ordinal, entry) in view.entries().take(s.limit).enumerate() {
        if canceled() { return Err(Failure::canceled()); }
        metadata.clear(); entry_output(&mut metadata, entry?, ordinal)?;
        reserved_bytes = reserved_bytes.checked_add(metadata.as_bytes().len() + ENTRY_OVERHEAD_RESERVE)
            .ok_or_else(|| Failure::new("TRAIL_EXPORT_METADATA_BYTE_LIMIT"))?;
        reserved_characters = reserved_characters.checked_add(scalars(metadata.as_bytes())? + ENTRY_OVERHEAD_RESERVE)
            .ok_or_else(|| Failure::new("TRAIL_EXPORT_METADATA_CHARACTER_LIMIT"))?;
    }
    let remaining_bytes = s.export_bytes.checked_sub(reserved_bytes)
        .ok_or_else(|| Failure::new("TRAIL_EXPORT_METADATA_BYTE_LIMIT"))?;
    let remaining_characters = s.export_characters.checked_sub(reserved_characters)
        .ok_or_else(|| Failure::new("TRAIL_EXPORT_METADATA_CHARACTER_LIMIT"))?;
    // One original byte always needs exactly two ASCII hex scalars. JSON quotes
    // and every other byte are already covered by the provenance reservations.
    let selection_limit = (remaining_bytes.min(remaining_characters) / 2) as u64;
    let output_reason = if remaining_bytes <= remaining_characters {
        "TRAIL_EXPORT_OUTPUT_BYTE_LIMIT"
    } else { "TRAIL_EXPORT_OUTPUT_CHARACTER_LIMIT" };
    let mut cursor = TrailExportCursor::new(view, generation(), file_id(), revision(),
        TrailExportLimits { max_entries: s.limit, max_source_bytes: s.export_source_bytes,
            max_selected_bytes: selection_limit }).map_err(fail)?;
    while cursor.stats().entries_visited < view.len().min(s.limit) {
        if canceled() { return Err(Failure::canceled()); }
        let entry = view.entry(cursor.stats().entries_visited)?;
        let archive = archives.for_digest(entry.archive)?;
        let item = cursor.next(archive, budget, [allocation(212), allocation(213)], &mut *canceled).map_err(fail)?
            .ok_or_else(|| Failure::new("TRAIL_EXPORT_STALE_CURSOR"))?;
        if item.target.ordinal() > 0 { pack.literal(",")?; }
        pack.literal("{\"selection\":")?; entry_output(&mut pack, item.target.entry(), item.target.ordinal())?;
        let (readiness, reason, selected) = match &item.outcome {
            TrailExportOutcome::Verified(capture) => ("digest-verified", None, Some(capture.selected_bytes())),
            TrailExportOutcome::Unavailable(error) => ("unavailable", Some(error.to_string()), None),
            TrailExportOutcome::SourceByteLimit => ("not-checked", Some("TRAIL_EXPORT_SOURCE_BYTE_LIMIT".to_owned()), None),
            TrailExportOutcome::SelectionByteLimit => ("not-checked", Some(output_reason.to_owned()), None),
        };
        pack.literal(",\"readiness\":")?; pack.quoted(readiness)?;
        pack.literal(",\"reason\":")?;
        match reason { Some(reason) => pack.quoted(&reason)?, None => pack.literal("null")? }
        pack.literal(",\"selected_original_hex\":")?;
        match selected { Some(bytes) => source_hex(&mut pack, bytes, canceled)?, None => pack.literal("null")? }
        pack.literal("}")?;
        // item drops here: the next repeated visit verifies anew with the same
        // bounded allocation slots, rather than retaining a trail-sized heap.
    }
    let stats = cursor.stats();
    tail(&mut pack, stats, output_reason)?;
    if canceled() { return Err(Failure::canceled()); }
    let bytes = pack.as_bytes().len(); let characters = scalars(pack.as_bytes())?;
    if characters > s.export_characters { return Err(Failure::new("TRAIL_EXPORT_OUTPUT_CHARACTER_LIMIT")); }
    // Prepare the complete receipt first; a failed output reservation or JSON
    // encoding cannot leave a newly created file with a false success receipt.
    if s.json {
        begin(receipt, "trail-export")?;
        receipt.literal(",\"format\":\"json\",\"pack_schema\":")?; receipt.quoted(PACK_SCHEMA)?;
        receipt.literal(",\"trail_digest\":")?; receipt.quoted(&view.digest().to_hex())?;
        receipt.literal(",\"destination\":")?; receipt.path(&destination)?;
        receipt.literal(",\"contains_source_payloads\":")?; receipt.boolean(stats.selected_bytes > 0)?;
        receipt.literal(",\"contains_user_text\":true,\"redacted\":false")?;
        receipt.literal(",\"export_bytes\":")?; receipt.integer(bytes as u64)?;
        receipt.literal(",\"export_characters\":")?; receipt.integer(characters as u64)?;
        receipt.literal(",\"archive_validation\":")?; archives.encode_validation(receipt, s.export_validation_bytes)?;
        receipt.literal(",\"character_representation\":\"unicode-scalars-in-serialized-utf8-json\",\"coverage\":")?;
        coverage(receipt, stats)?;
        receipt.literal(",\"effect\":\"complete-file-sync-requested\",\"power_loss_qualified\":false,\"complete\":")?;
        receipt.boolean(stats.complete())?; receipt.literal("}\n")?;
    } else {
        receipt.literal("Exported reading-trail source evidence to ")?; receipt.path(&destination)?;
        receipt.literal(&format!("\n{} of {} selections verified; {bytes} bytes, {characters} Unicode scalars in JSON.\n",
            stats.verified_entries, stats.total_entries))?;
        receipt.literal(if stats.complete() { "All saved selections are included.\n" }
            else { "Partial source evidence; the pack records unavailable selections and every budget omission.\n" })?;
    }
    write_new(&destination, pack.as_bytes(), effect, canceled)?;
    Ok(if stats.complete() { EXIT_OK } else { EXIT_PARTIAL })
}

fn header(out: &mut Output, s: &Settings, view: TrailView<'_>, archives: &Archives) -> Result<(), OutputError> {
    header_prefix(out, view)?;
    // Preserve the established single-archive field. A multi-archive export has
    // no single source universe; the full explicit inventory is always below.
    match archives.handles[0].as_ref().filter(|_| archives.count == 1) {
        Some(archive) => archive_output(out, archive)?, None => out.literal("null")?,
    }
    out.literal(",\"archives\":[")?;
    for (ordinal, archive) in archives.handles.iter().flatten().enumerate() {
        if ordinal > 0 { out.literal(",")?; }
        archive_output(out, archive)?;
    }
    out.literal("],\"archive_validation\":")?; archives.encode_validation(out, s.export_validation_bytes)?;
    out.literal(",\"budgets\":{\"entries\":")?; out.integer(s.limit as u64)?;
    out.literal(",\"verification_source_bytes\":")?; out.integer(s.export_source_bytes)?;
    out.literal(",\"export_bytes\":")?; out.integer(s.export_bytes as u64)?;
    out.literal(",\"export_characters\":")?; out.integer(s.export_characters as u64)?;
    out.literal(",\"admission\":\"whole-selections-with-reserved-provenance\",\"per_entry_overhead_reserved\":")?;
    out.integer(ENTRY_OVERHEAD_RESERVE as u64)?;
    out.literal(",\"footer_reserved\":")?; out.integer(TAIL_RESERVE as u64)?;
    out.literal("},\"entries\":[")
}

/// Shared source-pack vocabulary. The caller appends its authorized archive
/// metadata after this prefix, then its own budgets, entries and terminal data.
pub(crate) fn header_prefix(out: &mut Output, view: TrailView<'_>) -> Result<(), OutputError> {
    out.literal("{\"schema\":")?; out.quoted(PACK_SCHEMA)?;
    out.literal(",\"trail_digest\":")?; out.quoted(&view.digest().to_hex())?;
    out.literal(",\"parent_digest\":")?;
    match view.parent_digest() { Some(digest) => out.quoted(&digest.to_hex())?, None => out.literal("null")? }
    out.literal(",\"title\":")?; out.quoted(view.title())?;
    out.literal(",\"total_items\":")?; out.integer(view.len() as u64)?;
    out.literal(",\"referenced_bytes_including_repeats\":")?; out.integer(view.reference_bytes())?;
    out.literal(",\"completeness_scope\":\"all-saved-trail-selections\",\"deduplication\":\"none-preserve-explicit-visits\"")?;
    out.literal(",\"source_representation\":\"original-bytes-lowercase-hex\",\"character_representation\":\"unicode-scalars-in-serialized-utf8-json\"")?;
    out.literal(",\"live_roots_accessed\":false,\"redacted\":false,\"payload_truncated\":false,\"archive\":")
}

fn archive_output(out: &mut Output, archive: &PagedSnapshot<File>) -> Result<(), OutputError> {
    out.literal("{\"digest\":")?; out.quoted(&archive.directory().digest().to_hex())?;
    out.literal(",\"discovery_complete\":")?; out.boolean(archive.directory().discovery_complete())?;
    out.literal(",\"validation_bytes_read\":")?; out.integer(archive.directory().validation_stats().bytes_read)?;
    out.literal(",\"validation_read_calls\":")?; out.integer(archive.directory().validation_stats().read_calls)?;
    out.literal(",\"max_archive_bytes\":")?; out.integer(SnapshotLimits::default().max_document_bytes as u64)?;
    out.literal("}")
}

pub(crate) fn coverage(out: &mut Output, stats: TrailExportStats) -> Result<(), OutputError> {
    out.literal("{\"total_entries\":")?; out.integer(stats.total_entries as u64)?;
    for (name, value) in [("entries_visited", stats.entries_visited as u64),
        ("verified_entries", stats.verified_entries as u64), ("unavailable_entries", stats.unavailable_entries as u64),
        ("omitted_entry_limit", stats.omitted_entry_limit as u64), ("omitted_source_limit", stats.omitted_source_limit as u64),
        ("omitted_output_limit", stats.omitted_selection_limit as u64),
        ("verification_bytes_admitted", stats.verification_bytes_admitted),
        ("source_bytes_read", stats.source_bytes_read), ("selected_original_bytes", stats.selected_bytes)] {
        out.literal(",")?; out.quoted(name)?; out.literal(":")?; out.integer(value)?;
    }
    out.literal("}")
}
fn tail(out: &mut Output, stats: TrailExportStats, output_reason: &str) -> Result<(), OutputError> {
    out.literal("],\"coverage\":")?; coverage(out, stats)?;
    out.literal(",\"omitted_tail\":{\"reason\":\"TRAIL_EXPORT_ENTRY_LIMIT\",\"first_item\":")?;
    if stats.omitted_entry_limit > 0 { out.integer(stats.entries_visited as u64 + 1)?; } else { out.literal("null")?; }
    out.literal(",\"count\":")?; out.integer(stats.omitted_entry_limit as u64)?;
    out.literal("},\"output_omission_reason\":")?; out.quoted(output_reason)?;
    out.literal(",\"complete\":")?; out.boolean(stats.complete())?;
    out.literal("}\n")
}
fn scalars(bytes: &[u8]) -> Result<usize, Failure> {
    Ok(std::str::from_utf8(bytes).map_err(|_| Failure::new("TRAIL_EXPORT_INVALID_JSON_TEXT"))?.chars().count())
}
fn source_hex(out: &mut Output, bytes: &[u8], canceled: &mut impl FnMut() -> bool) -> Result<(), Failure> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.literal("\"")?;
    let mut encoded = [0u8; 4096];
    for chunk in bytes.chunks(encoded.len() / 2) {
        if canceled() { return Err(Failure::canceled()); }
        for (i, &byte) in chunk.iter().enumerate() {
            encoded[i * 2] = HEX[(byte >> 4) as usize]; encoded[i * 2 + 1] = HEX[(byte & 15) as usize];
        }
        out.literal(std::str::from_utf8(&encoded[..chunk.len() * 2]).map_err(|_| Failure::new("TRAIL_EXPORT_INVALID_JSON_TEXT"))?)?;
    }
    out.literal("\"")?; Ok(())
}

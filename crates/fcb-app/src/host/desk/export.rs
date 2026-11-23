#![forbid(unsafe_code)]

//! Source-specific JSON evidence packs from bookmarks and observed passages.
//! This is not a document formatter, a decoder or a checkpoint. Only explicit
//! saved byte ranges authorize source disclosure. Labels confer no path access;
//! missing selections and budget omissions stay visible alongside every note.

use std::path::Path;
use fcb::{ArenaOwnerId, ByteLength, ByteRange, SourceCapture,
    ui::reading_panes::desk::{DeskBookmark, DeskObservation, DeskObservedPassage}};
use fcb::store::envelope::{Sha256, Sha256Digest};
use super::{DeskSession, DeskSessionError, DeskError, Output, OutputError, HostResponse,
    check, location_fields, persistence::{self, CheckpointIoError, CheckpointSave, CheckpointSaveEffect}};
use crate::{EXIT_PARTIAL, output::MAX_ENCODED_BYTES};
use crate::source_pack_text::{self, TextSource, TextStatus, TextError};

const COMPLETE_SCHEMA: &str = "fcb.desk-source-pack/1";
const OBSERVED_SCHEMA: &str = "fcb.desk-source-pack/2";
const METADATA_SCRATCH: usize = 256 * 1024;
const ENTRY_RESERVE: usize = 512 + source_pack_text::FIELDS_RESERVE;
const TAIL_RESERVE: usize = 2048;

/// Source bytes count repeated selections separately. Byte/scalar ceilings
/// cover the entire serialized JSON, including escaped notes and provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeskSourcePackLimits {
    pub source_bytes: u64,
    pub output_bytes: usize,
    pub output_scalars: usize,
}
impl Default for DeskSourcePackLimits {
    fn default() -> Self {
        Self { source_bytes: 1024 * 1024, output_bytes: MAX_ENCODED_BYTES, output_scalars: MAX_ENCODED_BYTES }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeskSourcePackCoverage {
    pub total_bookmarks: u64,
    pub total_observed_passages: u64,
    /// Both bookmark selections and observed passages, counted separately even
    /// when two personal records retain the same original bytes.
    pub exported_selections: u64,
    pub omitted_no_selection: u64,
    pub omitted_source_limit: u64,
    pub omitted_output_limit: u64,
    pub selected_original_bytes: u64,
}
impl DeskSourcePackCoverage {
    pub const fn complete(self) -> bool {
        match self.total_bookmarks.checked_add(self.total_observed_passages) {
            Some(total) => self.exported_selections == total,
            None => false,
        }
    }
}

/// Publication success and source coverage are independent. A successfully
/// written pack can contain deliberate omissions; interrupted output is never
/// a successful pack, even if some or all of its bytes reached the destination.
#[derive(Clone, Copy, Debug)]
pub struct DeskSourcePackExport {
    owner: ArenaOwnerId,
    revision: u64,
    last_attempt: u64,
    write: CheckpointSave,
    limits: DeskSourcePackLimits,
    coverage: DeskSourcePackCoverage,
    scalars: u64,
}
impl DeskSourcePackExport {
    pub const fn effect(&self) -> CheckpointSaveEffect { self.write.effect() }
    pub const fn bytes_written(&self) -> u64 { self.write.bytes_written() }
    pub const fn document_bytes(&self) -> u64 { self.write.document_bytes() }
    pub const fn document_scalars(&self) -> u64 { self.scalars }
    pub const fn digest(&self) -> Option<Sha256Digest> { self.write.digest() }
    pub const fn error(&self) -> Option<CheckpointIoError> { self.write.error() }
    pub const fn coverage(&self) -> DeskSourcePackCoverage { self.coverage }
    pub fn exit_code(&self) -> u8 {
        if self.error().is_none() && !self.coverage.complete() { EXIT_PARTIAL } else { self.write.exit_code() }
    }
}

impl DeskSession {
    /// Export all personal metadata and whole explicit selections: bookmarks in
    /// saved order, then observed passages in saved order. Complete-only desks
    /// retain schema 1; schema 2 distinguishes observed extents and their absolute
    /// original coordinates. No selection is inferred from a caret; no source path
    /// is read. Metadata that cannot fit refuses the entire operation before
    /// destination creation. Sources that cannot fit are individually omitted.
    pub fn export_source_pack(&mut self, expected: u64, destination: &Path,
        limits: DeskSourcePackLimits, mut canceled: impl FnMut() -> bool) -> DeskSourcePackExport {
        let mut outcome = DeskSourcePackExport {
            owner: self.desk.owner(), revision: self.desk.revision(), last_attempt: self.desk.last_attempt(),
            write: CheckpointSave {
                owner: self.desk.owner(), revision: self.desk.revision(), last_attempt: self.desk.last_attempt(),
                effect: CheckpointSaveEffect::None, bytes_written: 0,
                document_bytes: 0, source_bytes: 0, digest: None, error: None },
            limits, coverage: DeskSourcePackCoverage {
                total_bookmarks: self.desk.bookmarks().len() as u64,
                total_observed_passages: self.desk.observed_passages().len() as u64, ..Default::default()
            }, scalars: 0,
        };
        let result = (|| -> Result<(), CheckpointIoError> {
            if expected != self.desk.revision() { return Err(DeskSessionError::from(DeskError::StaleRevision).into()); }
            check(&mut canceled)?;
            let destination = persistence::checked_path(destination)?;
            let (pack, coverage) = self.source_pack(expected, limits, &mut canceled)?;
            let mut digest = Sha256::new();
            for chunk in pack.as_bytes().chunks(64 * 1024) {
                check(&mut canceled)?; digest.update(chunk);
            }
            outcome.write.document_bytes = pack.as_bytes().len() as u64;
            outcome.write.source_bytes = coverage.selected_original_bytes;
            outcome.write.digest = Some(digest.finalize());
            outcome.scalars = scalar_count(pack.as_bytes()).map_err(DeskSessionError::from)? as u64;
            outcome.coverage = coverage;
            persistence::publish_new_payload(&destination, pack.as_bytes(), &mut outcome.write, &mut canceled)
        })();
        outcome.write.error = result.err();
        outcome
    }

    fn source_pack(&mut self, expected: u64, limits: DeskSourcePackLimits,
        canceled: &mut impl FnMut() -> bool) -> Result<(Output, DeskSourcePackCoverage), DeskSessionError> {
        if limits.output_bytes == 0 || limits.output_bytes > MAX_ENCODED_BYTES
            || limits.output_scalars == 0 || limits.output_scalars > MAX_ENCODED_BYTES {
            return Err(DeskError::InvalidLimits.into());
        }
        let [pack_id, metadata_id, decode_id] = self.ids()?;
        let mut pack = Output::new(self.desk.owner(), limits.output_bytes, &self.budget, pack_id)?;
        let _decode_scratch = self.budget.try_reserve_managed(self.desk.owner(), decode_id,
            ByteLength::new(source_pack_text::DECODE_SCRATCH_BYTES))
            .map_err(|_| DeskError::ResourceDenied)?;
        header(&mut pack, self, limits)?;
        let with_observations = !self.desk.observed_passages().is_empty();
        let mut metadata = Output::new(self.desk.owner(), METADATA_SCRATCH, &self.budget, metadata_id)?;
        let mut reserved_bytes = pack.as_bytes().len() + TAIL_RESERVE;
        let mut reserved_scalars = scalar_count(pack.as_bytes())? + TAIL_RESERVE;
        let mut requested_source_bytes = 0u64;
        // Charge the exact escaped representation of every note first. Omitting
        // payload must never silently drop the evidence explaining that payload.
        for (ordinal, bookmark) in self.desk.bookmarks().iter().enumerate() {
            check(canceled)?;
            let at = bookmark.location();
            if let Some(range) = at.selection {
                requested_source_bytes = requested_source_bytes.saturating_add(range.len().get());
            }
            let source = self.desk.retained_capture(expected, at.file, at.revision)?;
            metadata.clear(); entry_metadata(&mut metadata, bookmark, source, ordinal, with_observations)?;
            reserved_bytes = reserved_bytes.checked_add(metadata.as_bytes().len() + ENTRY_RESERVE)
                .ok_or(OutputError::Limit)?;
            reserved_scalars = reserved_scalars.checked_add(scalar_count(metadata.as_bytes())? + ENTRY_RESERVE)
                .ok_or(OutputError::Limit)?;
        }
        for (position, passage) in self.desk.observed_passages().iter().enumerate() {
            check(canceled)?;
            requested_source_bytes = requested_source_bytes.saturating_add(passage.selection().len().get());
            let observation = self.desk.observation(expected, passage.observation())?;
            metadata.clear();
            observed_entry_metadata(&mut metadata, passage, observation, self.desk.bookmarks().len() + position)?;
            reserved_bytes = reserved_bytes.checked_add(metadata.as_bytes().len() + ENTRY_RESERVE)
                .ok_or(OutputError::Limit)?;
            reserved_scalars = reserved_scalars.checked_add(scalar_count(metadata.as_bytes())? + ENTRY_RESERVE)
                .ok_or(OutputError::Limit)?;
        }
        let remaining_bytes = limits.output_bytes.checked_sub(reserved_bytes).ok_or(OutputError::Limit)?;
        let remaining_scalars = limits.output_scalars.checked_sub(reserved_scalars).ok_or(OutputError::Limit)?;
        let mut payload_room = (remaining_bytes.min(remaining_scalars) / 2) as u64;
        // Reserve every original payload that could be admitted before offering
        // spare output capacity to readable text. Text never displaces a later
        // bookmark's bytes or notes, including repeated selections.
        let hex_reserve = requested_source_bytes.min(limits.source_bytes).min(payload_room) as usize * 2;
        let mut text_room = TextRoom {
            bytes: remaining_bytes - hex_reserve, scalars: remaining_scalars - hex_reserve,
        };
        let output_reason = if remaining_bytes <= remaining_scalars {
            "DESK_EXPORT_OUTPUT_BYTE_LIMIT"
        } else { "DESK_EXPORT_OUTPUT_SCALAR_LIMIT" };
        let mut coverage = DeskSourcePackCoverage {
            total_bookmarks: self.desk.bookmarks().len() as u64,
            total_observed_passages: self.desk.observed_passages().len() as u64, ..Default::default()
        };
        for (ordinal, bookmark) in self.desk.bookmarks().iter().enumerate() {
            check(canceled)?;
            let at = bookmark.location();
            let source = self.desk.retained_capture(expected, at.file, at.revision)?;
            if ordinal > 0 { pack.literal(",")?; }
            entry_metadata(&mut pack, bookmark, source, ordinal, with_observations)?;
            let selected = match at.selection {
                None => None,
                Some(range) => {
                    let (start, end) = range.as_usize_bounds().map_err(|_| DeskError::InvalidLocation)?;
                    Some(PackSelection { bytes: source.bytes().get(start..end).ok_or(DeskError::InvalidLocation)?,
                        source: TextSource::complete(source.bytes()), range })
                }
            };
            selection_output(&mut pack, selected, limits, &mut payload_room, &mut text_room,
                output_reason, &mut coverage, canceled)?;
        }
        for (position, passage) in self.desk.observed_passages().iter().enumerate() {
            check(canceled)?;
            let observation = self.desk.observation(expected, passage.observation())?;
            let ordinal = self.desk.bookmarks().len() + position;
            if ordinal > 0 { pack.literal(",")?; }
            observed_entry_metadata(&mut pack, passage, observation, ordinal)?;
            let selected = observation.extent().range_bytes(passage.selection())
                .map_err(super::ObservedReaderError::from)?;
            let extent = observation.extent();
            let selected = PackSelection { bytes: selected,
                source: TextSource { bytes: extent.bytes(), original_start: extent.range().start().get(),
                    encoding: observation.encoding() }, range: passage.selection() };
            selection_output(&mut pack, Some(selected), limits, &mut payload_room, &mut text_room,
                output_reason, &mut coverage, canceled)?;
        }
        pack.literal("],\"coverage\":")?; coverage_output(&mut pack, coverage)?;
        pack.literal(",\"complete\":")?; pack.boolean(coverage.complete())?;
        pack.literal("}\n")?;
        check(canceled)?;
        if scalar_count(pack.as_bytes())? > limits.output_scalars { return Err(OutputError::Limit.into()); }
        Ok((pack, coverage))
    }

    /// Return the terminal effect without a post-write cancellation gate. A
    /// missing transport response means inspect, never automatically replay.
    pub fn source_pack_response(&mut self, saved: &DeskSourcePackExport) -> Result<HostResponse, DeskSessionError> {
        if saved.owner != self.desk.owner() { return Err(DeskError::OwnerMismatch.into()); }
        let id = self.next_id()?;
        let mut out = Output::new(self.desk.owner(), 16 * 1024, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.reading-desk/1\",\"command\":\"export\",\"status\":")?;
        out.quoted(if saved.error().is_none() { "ok" } else { "error" })?;
        out.literal(",\"owner\":")?; out.integer(saved.owner.get())?;
        out.literal(",\"model_revision\":")?; out.integer(saved.revision)?;
        out.literal(",\"last_attempt\":")?; out.integer(saved.last_attempt)?;
        out.literal(",\"pack_schema\":")?; out.quoted(pack_schema(saved.coverage.total_observed_passages))?;
        out.literal(",\"effect\":")?; out.quoted(saved.effect().code())?;
        out.literal(",\"bytes_written\":")?; out.integer(saved.bytes_written())?;
        out.literal(",\"export_bytes\":")?; out.integer(saved.document_bytes())?;
        out.literal(",\"export_scalars\":")?; out.integer(saved.document_scalars())?;
        out.literal(",\"export_digest\":")?;
        match saved.digest() { Some(digest) => out.quoted(&digest.to_hex())?, None => out.literal("null")? }
        out.literal(",\"budgets\":")?; limits_output(&mut out, saved.limits)?;
        out.literal(",\"coverage\":")?; coverage_output(&mut out, saved.coverage)?;
        out.literal(",\"completeness_scope\":")?; out.quoted(completeness_scope(saved.coverage.total_observed_passages))?;
        out.literal(",\"observed_passages_included\":")?; out.boolean(saved.coverage.total_observed_passages > 0)?;
        out.literal(",\"excluded_observed_passages\":\"0\"")?;
        out.literal(",\"complete\":")?; out.boolean(saved.document_bytes() > 0 && saved.coverage.complete())?;
        out.literal(",\"contains_source_payloads\":")?; out.boolean(saved.coverage.selected_original_bytes > 0)?;
        out.literal(",\"contains_personal_labels\":true,\"contains_user_text\":true,\"redacted\":false,\"encrypted\":false,\"overwritten\":false,\"live_roots_accessed\":false,\"native_presented\":false,\"power_loss_qualified\":false,\"error\":")?;
        match saved.error() {
            Some(error) => { out.literal("{\"code\":")?; out.quoted(&error.code())?;
                out.literal(",\"next_action\":\"Inspect any created destination before retrying; choose a new filename.\"}")?; }
            None => out.literal("null")?,
        }
        out.literal("}\n")?;
        self.finish(out, saved.exit_code(), &mut || false)
    }
}

fn pack_schema(observed: u64) -> &'static str {
    if observed == 0 { COMPLETE_SCHEMA } else { OBSERVED_SCHEMA }
}
fn completeness_scope(observed: u64) -> &'static str {
    if observed == 0 { "all-bookmarked-selections" } else { "all-bookmarked-selections-and-observed-passages" }
}
fn header(out: &mut Output, session: &DeskSession, limits: DeskSourcePackLimits) -> Result<(), OutputError> {
    let observed = session.desk.observed_passages().len() as u64;
    out.literal("{\"schema\":")?; out.quoted(pack_schema(observed))?;
    out.literal(",\"owner\":")?; out.integer(session.desk.owner().get())?;
    out.literal(",\"model_revision\":")?; out.integer(session.desk.revision())?;
    out.literal(",\"last_attempt\":")?; out.integer(session.desk.last_attempt())?;
    out.literal(",\"observed_passages_included\":")?; out.boolean(observed > 0)?;
    out.literal(",\"excluded_observed_passages\":\"0\"")?;
    out.literal(",\"source_representation\":\"original-bytes-lowercase-hex\",\"selected_text_representation\":\"exact-unicode-when-decodable-and-budgeted\",\"character_representation\":\"unicode-scalars-in-serialized-utf8-json\",\"completeness_scope\":")?;
    out.quoted(completeness_scope(observed))?;
    out.literal(",\"deduplication\":")?;
    out.quoted(if observed == 0 { "none-preserve-bookmark-order" } else { "none-preserve-bookmarks-then-observed-passages" })?;
    out.literal(",\"live_roots_accessed\":false,\"redacted\":false,\"encrypted\":false,\"payload_truncated\":false,\"source_labels_are_path_authority\":false,\"budgets\":")?;
    limits_output(out, limits)?;
    out.literal(",\"admission\":{\"policy\":\"whole-selections-with-reserved-provenance\",\"per_entry_overhead_reserved\":")?;
    out.integer(ENTRY_RESERVE as u64)?; out.literal(",\"footer_reserved\":")?; out.integer(TAIL_RESERVE as u64)?;
    out.literal("},\"entries\":[")
}
fn limits_output(out: &mut Output, limits: DeskSourcePackLimits) -> Result<(), OutputError> {
    out.literal("{\"source_bytes\":")?; out.integer(limits.source_bytes)?;
    out.literal(",\"output_bytes\":")?; out.integer(limits.output_bytes as u64)?;
    out.literal(",\"output_scalars\":")?; out.integer(limits.output_scalars as u64)?; out.literal("}")
}
fn entry_metadata(out: &mut Output, mark: &DeskBookmark, source: &SourceCapture,
    ordinal: usize, typed_capture: bool) -> Result<(), OutputError> {
    out.literal("{\"ordinal\":")?; out.integer(ordinal as u64)?;
    if typed_capture { out.literal(",\"capture_kind\":\"complete-capture\"")?; }
    out.literal(",\"bookmark_id\":")?; out.integer(mark.id())?;
    out.literal(",\"bookmark_update_sequence\":")?; out.integer(mark.update_sequence())?;
    out.literal(",\"label\":")?; out.quoted(mark.label())?;
    out.literal(",\"body\":")?; out.quoted(mark.body())?;
    out.literal(",\"source_owner\":")?; out.integer(source.owner().get())?;
    location_fields(out, mark.location())?;
    out.literal(",\"source_label\":")?; out.quoted(source.logical_path())?;
    out.literal(",\"capture_bytes\":")?; out.integer(source.bytes().len() as u64)
}
fn observed_entry_metadata(out: &mut Output, passage: &DeskObservedPassage,
    observation: &DeskObservation, ordinal: usize) -> Result<(), OutputError> {
    out.literal("{\"ordinal\":")?; out.integer(ordinal as u64)?;
    out.literal(",\"capture_kind\":\"observed-extent\",\"payload_scope\":\"selected-range-only\",\"passage_id\":")?;
    out.integer(passage.id())?;
    out.literal(",\"passage_update_sequence\":")?; out.integer(passage.update_sequence())?;
    out.literal(",\"label\":")?; out.quoted(passage.label())?;
    out.literal(",\"body\":")?; out.quoted(passage.body())?;
    out.literal(",\"selection\":")?; out.range(passage.selection())?;
    out.literal(",\"observation\":")?;
    super::observations::encode_observation(out, observation)
}
struct PackSelection<'a> { bytes: &'a [u8], source: TextSource<'a>, range: ByteRange }
struct TextRoom { bytes: usize, scalars: usize }

fn selection_output(out: &mut Output, selection: Option<PackSelection<'_>>, limits: DeskSourcePackLimits,
    payload_room: &mut u64, text_room: &mut TextRoom, output_reason: &'static str, coverage: &mut DeskSourcePackCoverage,
    canceled: &mut impl FnMut() -> bool) -> Result<(), DeskSessionError> {
    let mut selected = None;
    let reason = match selection {
        None => { coverage.omitted_no_selection += 1; Some("DESK_EXPORT_NO_SAVED_SELECTION") }
        Some(selection) => {
            let size = selection.bytes.len() as u64;
            if size > limits.source_bytes - coverage.selected_original_bytes {
                coverage.omitted_source_limit += 1; Some("DESK_EXPORT_SOURCE_BYTE_LIMIT")
            } else if size > *payload_room {
                coverage.omitted_output_limit += 1; Some(output_reason)
            } else {
                *payload_room -= size; coverage.selected_original_bytes += size;
                coverage.exported_selections += 1; selected = Some(selection); None
            }
        }
    };
    out.literal(",\"evidence\":\"explicit-user-selection\",\"outcome\":")?;
    out.quoted(if selected.is_some() { "exported" } else { "omitted" })?;
    out.literal(",\"reason\":")?;
    match reason { Some(code) => out.quoted(code)?, None => out.literal("null")? }
    out.literal(",\"selected_original_hex\":")?;
    match selected {
        Some(selection) => {
            source_hex(out, selection.bytes, canceled)?;
            let text = source_pack_text::prepare(selection.source, selection.range,
                text_room.bytes.saturating_add(4), text_room.scalars.saturating_add(4), canceled).map_err(text_error)?;
            source_pack_text::write_fields(out, &text, canceled).map_err(text_error)?;
            if text.status() == TextStatus::Exact {
                text_room.bytes -= text.json_bytes().saturating_sub(4);
                text_room.scalars -= text.json_scalars().saturating_sub(4);
            }
        }
        None => {
            out.literal("null")?;
            source_pack_text::write_unavailable_fields(out,
                if reason == Some("DESK_EXPORT_NO_SAVED_SELECTION") { TextStatus::NoSelection }
                else { TextStatus::SourceUnavailable })?;
        }
    }
    out.literal("}")?; Ok(())
}

fn text_error(error: TextError) -> DeskSessionError {
    match error {
        TextError::Canceled => DeskError::Canceled.into(),
        TextError::Decode => DeskError::InvalidLocation.into(),
        TextError::Output(error) => error.into(),
    }
}

fn coverage_output(out: &mut Output, c: DeskSourcePackCoverage) -> Result<(), OutputError> {
    out.literal("{\"total_bookmarks\":")?; out.integer(c.total_bookmarks)?;
    for (key, value) in [("total_observed_passages", c.total_observed_passages), ("exported_selections", c.exported_selections),
        ("omitted_no_selection", c.omitted_no_selection), ("omitted_source_limit", c.omitted_source_limit),
        ("omitted_output_limit", c.omitted_output_limit), ("selected_original_bytes", c.selected_original_bytes)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    out.literal("}")
}
fn scalar_count(bytes: &[u8]) -> Result<usize, OutputError> {
    Ok(std::str::from_utf8(bytes).map_err(|_| OutputError::UnsupportedPath)?.chars().count())
}
fn source_hex(out: &mut Output, bytes: &[u8], canceled: &mut impl FnMut() -> bool) -> Result<(), DeskSessionError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.literal("\"")?;
    let mut encoded = [0u8; 4096];
    for chunk in bytes.chunks(encoded.len() / 2) {
        check(canceled)?;
        for (i, &byte) in chunk.iter().enumerate() {
            encoded[i * 2] = HEX[(byte >> 4) as usize]; encoded[i * 2 + 1] = HEX[(byte & 15) as usize];
        }
        out.literal(std::str::from_utf8(&encoded[..chunk.len() * 2]).map_err(|_| OutputError::UnsupportedPath)?)?;
    }
    out.literal("\"")?; Ok(())
}

#![forbid(unsafe_code)]

//! Source-specific, self-contained JSON evidence packs from retained bookmarks.
//! This is not a document formatter, a decoder or a checkpoint. Only explicit
//! saved byte ranges authorize source disclosure. Labels confer no path access;
//! missing selections and budget omissions stay visible alongside every note.

use std::path::Path;
use fcb::{ArenaOwnerId, SourceCapture, ui::reading_panes::desk::DeskBookmark};
use fcb::store::envelope::{Sha256, Sha256Digest};
use super::{DeskSession, DeskSessionError, DeskError, Output, OutputError, HostResponse,
    check, location_fields, persistence::{self, CheckpointIoError, CheckpointSave, CheckpointSaveEffect}};
use crate::{EXIT_PARTIAL, output::MAX_ENCODED_BYTES};

const SCHEMA: &str = "fcb.desk-source-pack/1";
const METADATA_SCRATCH: usize = 256 * 1024;
const ENTRY_RESERVE: usize = 512;
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
    pub exported_selections: u64,
    pub omitted_no_selection: u64,
    pub omitted_source_limit: u64,
    pub omitted_output_limit: u64,
    pub selected_original_bytes: u64,
}
impl DeskSourcePackCoverage {
    pub const fn complete(self) -> bool { self.exported_selections == self.total_bookmarks }
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
    /// Export all bookmark metadata and whole explicit selections in stable
    /// bookmark order. No selection is inferred from a caret; no source path
    /// is read. Metadata that cannot fit refuses the entire operation before
    /// destination creation. Sources that cannot fit are individually omitted.
    pub fn export_source_pack(&mut self, expected: u64, destination: &Path,
        limits: DeskSourcePackLimits, mut canceled: impl FnMut() -> bool) -> DeskSourcePackExport {
        let mut outcome = DeskSourcePackExport {
            owner: self.desk.owner(), revision: self.desk.revision(), last_attempt: self.desk.last_attempt(),
            write: CheckpointSave { effect: CheckpointSaveEffect::None, bytes_written: 0,
                document_bytes: 0, source_bytes: 0, digest: None, error: None },
            limits, coverage: DeskSourcePackCoverage {
                total_bookmarks: self.desk.bookmarks().len() as u64, ..Default::default()
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
        let [pack_id, metadata_id] = self.ids()?;
        let mut pack = Output::new(self.desk.owner(), limits.output_bytes, &self.budget, pack_id)?;
        header(&mut pack, self, limits)?;
        let mut metadata = Output::new(self.desk.owner(), METADATA_SCRATCH, &self.budget, metadata_id)?;
        let mut reserved_bytes = pack.as_bytes().len() + TAIL_RESERVE;
        let mut reserved_scalars = scalar_count(pack.as_bytes())? + TAIL_RESERVE;
        // Charge the exact escaped representation of every note first. Omitting
        // payload must never silently drop the evidence explaining that payload.
        for (ordinal, bookmark) in self.desk.bookmarks().iter().enumerate() {
            check(canceled)?;
            let at = bookmark.location();
            let source = self.desk.retained_capture(expected, at.file, at.revision)?;
            metadata.clear(); entry_metadata(&mut metadata, bookmark, source, ordinal)?;
            reserved_bytes = reserved_bytes.checked_add(metadata.as_bytes().len() + ENTRY_RESERVE)
                .ok_or(OutputError::Limit)?;
            reserved_scalars = reserved_scalars.checked_add(scalar_count(metadata.as_bytes())? + ENTRY_RESERVE)
                .ok_or(OutputError::Limit)?;
        }
        let remaining_bytes = limits.output_bytes.checked_sub(reserved_bytes).ok_or(OutputError::Limit)?;
        let remaining_scalars = limits.output_scalars.checked_sub(reserved_scalars).ok_or(OutputError::Limit)?;
        let mut payload_room = (remaining_bytes.min(remaining_scalars) / 2) as u64;
        let output_reason = if remaining_bytes <= remaining_scalars {
            "DESK_EXPORT_OUTPUT_BYTE_LIMIT"
        } else { "DESK_EXPORT_OUTPUT_SCALAR_LIMIT" };
        let mut coverage = DeskSourcePackCoverage {
            total_bookmarks: self.desk.bookmarks().len() as u64, ..Default::default()
        };
        for (ordinal, bookmark) in self.desk.bookmarks().iter().enumerate() {
            check(canceled)?;
            let at = bookmark.location();
            let source = self.desk.retained_capture(expected, at.file, at.revision)?;
            if ordinal > 0 { pack.literal(",")?; }
            entry_metadata(&mut pack, bookmark, source, ordinal)?;
            let mut selected = None;
            let reason = match at.selection {
                None => { coverage.omitted_no_selection += 1; Some("DESK_EXPORT_NO_SAVED_SELECTION") }
                Some(range) => {
                    let (start, end) = range.as_usize_bounds().map_err(|_| DeskError::InvalidLocation)?;
                    let bytes = source.bytes().get(start..end).ok_or(DeskError::InvalidLocation)?;
                    let size = bytes.len() as u64;
                    if size > limits.source_bytes - coverage.selected_original_bytes {
                        coverage.omitted_source_limit += 1; Some("DESK_EXPORT_SOURCE_BYTE_LIMIT")
                    } else if size > payload_room {
                        coverage.omitted_output_limit += 1; Some(output_reason)
                    } else {
                        payload_room -= size; coverage.selected_original_bytes += size;
                        coverage.exported_selections += 1; selected = Some(bytes); None
                    }
                }
            };
            pack.literal(",\"evidence\":\"explicit-user-selection\",\"outcome\":")?;
            pack.quoted(if selected.is_some() { "exported" } else { "omitted" })?;
            pack.literal(",\"reason\":")?;
            match reason { Some(code) => pack.quoted(code)?, None => pack.literal("null")? }
            pack.literal(",\"selected_original_hex\":")?;
            match selected { Some(bytes) => source_hex(&mut pack, bytes, canceled)?, None => pack.literal("null")? }
            pack.literal("}")?;
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
        out.literal(",\"pack_schema\":")?; out.quoted(SCHEMA)?;
        out.literal(",\"effect\":")?; out.quoted(saved.effect().code())?;
        out.literal(",\"bytes_written\":")?; out.integer(saved.bytes_written())?;
        out.literal(",\"export_bytes\":")?; out.integer(saved.document_bytes())?;
        out.literal(",\"export_scalars\":")?; out.integer(saved.document_scalars())?;
        out.literal(",\"export_digest\":")?;
        match saved.digest() { Some(digest) => out.quoted(&digest.to_hex())?, None => out.literal("null")? }
        out.literal(",\"budgets\":")?; limits_output(&mut out, saved.limits)?;
        out.literal(",\"coverage\":")?; coverage_output(&mut out, saved.coverage)?;
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

fn header(out: &mut Output, session: &DeskSession, limits: DeskSourcePackLimits) -> Result<(), OutputError> {
    out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
    out.literal(",\"owner\":")?; out.integer(session.desk.owner().get())?;
    out.literal(",\"model_revision\":")?; out.integer(session.desk.revision())?;
    out.literal(",\"last_attempt\":")?; out.integer(session.desk.last_attempt())?;
    out.literal(",\"source_representation\":\"original-bytes-lowercase-hex\",\"character_representation\":\"unicode-scalars-in-serialized-utf8-json\",\"completeness_scope\":\"all-bookmarked-selections\",\"deduplication\":\"none-preserve-bookmark-order\",\"live_roots_accessed\":false,\"redacted\":false,\"encrypted\":false,\"payload_truncated\":false,\"source_labels_are_path_authority\":false,\"budgets\":")?;
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
fn entry_metadata(out: &mut Output, mark: &DeskBookmark, source: &SourceCapture, ordinal: usize) -> Result<(), OutputError> {
    out.literal("{\"ordinal\":")?; out.integer(ordinal as u64)?;
    out.literal(",\"bookmark_id\":")?; out.integer(mark.id())?;
    out.literal(",\"bookmark_update_sequence\":")?; out.integer(mark.update_sequence())?;
    out.literal(",\"label\":")?; out.quoted(mark.label())?;
    out.literal(",\"body\":")?; out.quoted(mark.body())?;
    out.literal(",\"source_owner\":")?; out.integer(source.owner().get())?;
    location_fields(out, mark.location())?;
    out.literal(",\"source_label\":")?; out.quoted(source.logical_path())?;
    out.literal(",\"capture_bytes\":")?; out.integer(source.bytes().len() as u64)
}
fn coverage_output(out: &mut Output, c: DeskSourcePackCoverage) -> Result<(), OutputError> {
    out.literal("{\"total_bookmarks\":")?; out.integer(c.total_bookmarks)?;
    for (key, value) in [("exported_selections", c.exported_selections),
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

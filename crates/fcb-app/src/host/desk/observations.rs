#![forbid(unsafe_code)]

//! Durable passages from bounded observations. Complete-source panes and their
//! navigation remain separate. Every displayed byte comes from retained data;
//! original offsets and observation evidence survive live changes and restore.

use fcb::search::{DetectedEncoding, ExtentConsistency};
use fcb::ui::reading_panes::desk::{DeskObservation, DeskObservedPassage, DeskObservationChange};
use super::{DeskSession, DeskSessionError, DeskError, DeskChange, ReadingDesk, ByteRange,
    HostResponse, Output, OutputError, RawPath, check, optional};
use crate::EXIT_OK;
use crate::host::observed_reader::ObservedReaderSession;

impl DeskSession {
    /// Import the exact selection admitted by a retained observed reader. The
    /// provider owns the path, encoding and immutable bytes; a display string or
    /// a pathname cannot substitute for that authority. No source I/O occurs.
    pub fn import_observation(&mut self, expected: u64, attempt: u64,
        reader: &mut ObservedReaderSession, observation: u64, selection: ByteRange,
        mut canceled: impl FnMut() -> bool) -> Result<DeskObservationChange, DeskSessionError> {
        self.validate_mutation(expected, attempt)?;
        check(&mut canceled)?;
        let allocation = self.next_id()?;
        let first_source_identity = self.claim_source_identity()?;
        let retained = reader.retained_selection(observation, selection, &mut canceled)?;
        #[cfg(unix)] {
            use std::os::unix::ffi::OsStrExt;
            let changed = self.desk.import_observation(expected, attempt, &retained.extent,
                retained.path.as_os_str().as_bytes(), retained.encoding, retained.selection,
                first_source_identity, allocation, canceled)?;
            self.next_source = self.next_source.max(changed.next_source_identity);
            Ok(changed)
        }
        #[cfg(not(unix))] {
            let _ = (allocation, first_source_identity, retained);
            Err(crate::AppError::UnsupportedPlatform.into())
        }
    }

    /// Read a saved passage, including after its source reader has closed or
    /// its checkpoint has been restored. There is no live-path fallback.
    pub fn observed_passage(&mut self, expected: u64, passage: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskSessionError> {
        let allocation = self.next_id()?;
        let text = self.desk.read_observed_passage(expected, passage, allocation, &mut canceled)?;
        let mut out = self.output("observed-passage")?;
        let saved = self.desk.observed_passage(expected, passage)?;
        if text.range() != saved.selection() { return Err(DeskError::InvalidLocation.into()); }
        let observation = self.desk.observation(expected, saved.observation())?;
        out.literal(",\"capture_kind\":\"observed-extent\",\"whole_file_immutable\":false,\"source_reopened\":false,\"observation\":")?;
        encode_observation(&mut out, observation)?;
        out.literal(",\"passage\":")?; encode_passage(&mut out, saved)?;
        out.literal(",\"text\":")?; out.quoted(text.text())?;
        out.literal(",\"original_hex\":")?;
        let bytes = observation.extent().range_bytes(saved.selection())
            .map_err(super::ObservedReaderError::from)?;
        out.hex(bytes)?;
        out.literal(",\"contains_replacements\":")?; out.boolean(text.has_replacements())?;
        out.literal("}\n")?;
        self.finish(out, EXIT_OK, &mut canceled)
    }

    pub fn edit_observed_passage(&mut self, expected: u64, attempt: u64, passage: u64,
        sequence: u64, label: &str, body: &str, canceled: impl FnMut() -> bool)
        -> Result<DeskChange, DeskSessionError> {
        Ok(self.desk.edit_observed_passage(expected, attempt, passage, sequence, label, body, canceled)?)
    }
    pub fn forget_observed_passage(&mut self, expected: u64, attempt: u64, passage: u64,
        sequence: u64, canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskSessionError> {
        Ok(self.desk.forget_observed_passage(expected, attempt, passage, sequence, canceled)?)
    }
}

pub(super) fn encode_state(out: &mut Output, desk: &ReadingDesk,
    canceled: &mut impl FnMut() -> bool) -> Result<(), DeskSessionError> {
    out.literal(",\"retained_observations\":")?; out.integer(desk.observations().len() as u64)?;
    out.literal(",\"observations\":[")?;
    for (position, observation) in desk.observations().enumerate() {
        check(canceled)?;
        if position > 0 { out.literal(",")?; }
        encode_observation(out, observation)?;
    }
    out.literal("],\"observed_passages\":[")?;
    for (position, passage) in desk.observed_passages().iter().enumerate() {
        check(canceled)?;
        if position > 0 { out.literal(",")?; }
        encode_passage(out, passage)?;
    }
    out.literal("]")?;
    Ok(())
}

fn encode_observation(out: &mut Output, observation: &DeskObservation) -> Result<(), OutputError> {
    let extent = observation.extent();
    let path = RawPath::from_bytes(observation.path_bytes());
    out.literal("{\"observation\":")?; out.integer(observation.id())?;
    out.literal(",\"origin_owner\":")?; out.integer(observation.origin_owner().get())?;
    out.literal(",\"origin_file\":")?; out.integer(observation.origin_file())?;
    out.literal(",\"origin_observation\":")?; out.integer(observation.origin_observation())?;
    out.literal(",\"path\":")?; out.quoted(&path.display_escaped().to_string())?;
    out.literal(",\"path_hex\":")?; out.hex(path.as_bytes())?;
    out.literal(",\"encoding\":")?;
    out.quoted(match observation.encoding() {
        DetectedEncoding::Utf8 { .. } => "utf8", DetectedEncoding::Utf16Le => "utf16le",
        DetectedEncoding::Utf16Be => "utf16be", _ => return Err(OutputError::UnsupportedPath),
    })?;
    out.literal(",\"captured_range\":")?; out.range(extent.range())?;
    out.literal(",\"observed_length\":")?; out.integer(extent.observed_length().get())?;
    out.literal(",\"final_observed_length\":")?; optional(out, extent.final_length().map(|length| length.get()))?;
    out.literal(",\"consistency\":")?;
    out.quoted(match extent.consistency() {
        ExtentConsistency::HostSupplied => "host-supplied-sequence",
        ExtentConsistency::UnchangedMetadata => "unchanged-metadata-not-atomic",
        ExtentConsistency::ChangedDuringRead => "changed-during-read",
        ExtentConsistency::ShortRead => "short-read",
        ExtentConsistency::MetadataUnavailable => "metadata-unavailable",
    })?;
    out.literal(",\"extent_sha256\":")?; out.quoted(&observation.extent_sha256().to_hex())?;
    out.literal(",\"requested_capture_range\":")?;
    match extent.request().range() { Some(range) => out.range(range)?, None => out.literal("null")? }
    out.literal(",\"requested_capture_filled\":")?; out.boolean(extent.request_filled())?;
    out.literal(",\"capture_kind\":\"observed-extent\",\"whole_file_immutable\":false,\"source_reopened\":false}")
}
fn encode_passage(out: &mut Output, passage: &DeskObservedPassage) -> Result<(), OutputError> {
    out.literal("{\"passage\":")?; out.integer(passage.id())?;
    out.literal(",\"observation\":")?; out.integer(passage.observation())?;
    out.literal(",\"update_sequence\":")?; out.integer(passage.update_sequence())?;
    out.literal(",\"label\":")?; out.quoted(passage.label())?;
    out.literal(",\"body\":")?; out.quoted(passage.body())?;
    out.literal(",\"selection\":")?; out.range(passage.selection())?;
    out.literal("}")
}

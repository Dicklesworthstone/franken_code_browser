#![forbid(unsafe_code)]

//! Explicit self-contained source packs from an accepted trail and the already
//! open archive plus explicitly selected supplemental archives. The cursor
//! verifies whole captured members and
//! chooses whole selections; shared trail encoders preserve notes/provenance.
//! Labels and other-archive references never open paths. The create-only writer
//! retains a terminal effect receipt before any fallible delivery can intervene.

use std::{borrow::Cow, fs::File, path::Path};
use fcb::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, SourceRevision};
use fcb::search::{ResourceAllocationId, ResourceBudget};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::trail::{TrailExportCursor, TrailExportLimits, TrailExportOutcome,
    TrailExportStats, TrailView, MAX_TRAIL_ITEMS};
use fcb::store::{Sha256, Sha256Digest};
use fcb_core::ResourceLease;
use crate::{AppError, EXIT_CANCELED, EXIT_ERROR, EXIT_OK, EXIT_PARTIAL, input};
use crate::host::HostResponse;
use crate::host::desk::persistence::{publish_new_bytes, CheckpointIoError, CheckpointSaveEffect};
use crate::output::{Output, OutputError, MAX_ENCODED_BYTES};
use crate::trail::{entry_output, export::{archive_output, coverage, header_prefix, PACK_SCHEMA,
    archives::{Archives, ArchiveSetError, MAX_ARCHIVES, MAX_VALIDATION_BYTES}}};
use super::{SavedRepositorySession, SavedTrailError, SavedTrailWriteParent, check};

const RECEIPT_BYTES: usize = 256 * 1024;
const MIXED_RECEIPT_BYTES: usize = 2 * 1024 * 1024;
const METADATA_BYTES: usize = 512 * 1024;
const ENTRY_RESERVE: usize = 512;
const TAIL_RESERVE: usize = 4096;

/// Independent ceilings for visits, whole-member verification, selected bytes,
/// and the complete serialized UTF-8 JSON including provenance and escaping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SavedTrailExportLimits {
    pub entries: usize,
    pub verification_bytes: u64,
    pub selected_bytes: u64,
    pub output_bytes: usize,
    pub output_scalars: usize,
}
impl Default for SavedTrailExportLimits {
    fn default() -> Self {
        Self { entries: MAX_TRAIL_ITEMS, verification_bytes: 64 * 1024 * 1024,
            selected_bytes: 1024 * 1024, output_bytes: MAX_ENCODED_BYTES, output_scalars: MAX_ENCODED_BYTES }
    }
}
impl SavedTrailExportLimits {
    fn valid(self) -> bool {
        self.entries <= MAX_TRAIL_ITEMS && self.verification_bytes <= 64 * 1024 * 1024
            && self.selected_bytes <= 64 * 1024 * 1024
            && (1..=MAX_ENCODED_BYTES).contains(&self.output_bytes)
            && (1..=MAX_ENCODED_BYTES).contains(&self.output_scalars)
    }
}
#[derive(Clone, Copy)]
pub struct SavedTrailExportRequest<'a> {
    pub operation: u64,
    pub trail: SavedTrailWriteParent,
    pub destination: &'a Path,
    pub limits: SavedTrailExportLimits,
}

#[derive(Clone, Copy)]
enum ExportFailure { Trail(SavedTrailError), Write(CheckpointIoError) }
impl ExportFailure {
    fn canceled(self) -> bool {
        match self { Self::Trail(e) => e.is_canceled(), Self::Write(e) => e.is_canceled() }
    }
    fn code(self) -> Cow<'static, str> {
        match self {
            Self::Trail(e) => Cow::Owned(e.to_string()),
            Self::Write(e) => Cow::Borrowed(match e {
                CheckpointIoError::DestinationExists => "SAVED_TRAIL_EXPORT_DESTINATION_EXISTS",
                CheckpointIoError::InvalidPath => "SAVED_TRAIL_EXPORT_DESTINATION_PATH",
                CheckpointIoError::IoCallLimit => "SAVED_TRAIL_EXPORT_WRITE_CALL_LIMIT",
                CheckpointIoError::Canceled => "SAVED_TRAIL_EXPORT_CANCELED",
                _ => "SAVED_TRAIL_EXPORT_WRITE_IO",
            }),
        }
    }
}
pub(in crate::host::saved_repository) struct RetainedTrailExport {
    operation: u64, receipt: Output, receipt_bytes: usize, tail: usize, encoded_tail: bool,
    effect: CheckpointSaveEffect, written: u64, artifact_bytes: u64,
    coverage: TrailExportStats, failure: Option<ExportFailure>,
}
impl RetainedTrailExport {
    fn exit_code(&self) -> u8 {
        match self.failure {
            Some(e) if e.canceled() => EXIT_CANCELED, Some(_) => EXIT_ERROR,
            None if !self.coverage.complete() => EXIT_PARTIAL, None => EXIT_OK,
        }
    }
    fn encode_tail(&mut self) -> Result<(), SavedTrailError> {
        if self.encoded_tail { return Ok(()); }
        self.receipt.restore(self.tail)?;
        self.receipt.literal(",\"status\":")?;
        self.receipt.quoted(if self.failure.is_some() { "error" } else { "ok" })?;
        self.receipt.literal(",\"effect\":")?; self.receipt.quoted(self.effect.code())?;
        self.receipt.literal(",\"bytes_written\":")?; self.receipt.integer(self.written)?;
        let complete = self.artifact_bytes > 0 && matches!(self.effect,
            CheckpointSaveEffect::Written | CheckpointSaveEffect::FileSynced | CheckpointSaveEffect::DirectorySynced);
        self.receipt.literal(",\"complete_artifact\":")?; self.receipt.boolean(complete)?;
        self.receipt.literal(",\"error\":")?;
        if let Some(error) = self.failure {
            self.receipt.literal("{\"code\":")?; self.receipt.quoted(&error.code())?;
            self.receipt.literal("}")?;
        } else { self.receipt.literal("null")?; }
        self.receipt.literal("}\n")?; self.encoded_tail = true; Ok(())
    }
}

impl SavedRepositorySession {
    pub const fn last_trail_export_attempt(&self) -> u64 { self.last_trail_export_attempt }
    pub fn last_trail_export_operation(&self) -> Option<u64> { self.trail_export.as_ref().map(|r| r.operation) }

    /// Export the frozen accepted trail using this retained archive only. A
    /// partial evidence pack remains a complete JSON artifact with explicit
    /// omitted/unavailable rows. No prior result, trail or reader is replaced.
    pub fn export_trail(&mut self, request: SavedTrailExportRequest<'_>,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedTrailError> {
        self.export_trail_with_archives(request, &[], 0, canceled)
    }

    /// Supplement the retained primary with up to seven explicitly granted
    /// archive paths. Full envelope validation shares one independent byte and
    /// directory budget; no member is read and no output is created until all
    /// supplied inputs validate. A missing digest remains an unavailable visit.
    /// Explicit paths are borrowed only through worker drain and never retained
    /// as future filesystem authority by the effect-recovery receipt.
    pub fn export_trail_with_archives(&mut self, request: SavedTrailExportRequest<'_>,
        additional_archives: &[&Path], validation_bytes: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedTrailError> {
        if request.operation == 0 || request.operation <= self.last_trail_export_attempt {
            return Err(SavedTrailError::StaleExport);
        }
        self.last_trail_export_attempt = request.operation;
        check(&mut canceled)?;
        if !request.limits.valid() || request.destination.as_os_str().is_empty()
            || request.destination.as_os_str().len() > 16_384
            || additional_archives.len() >= MAX_ARCHIVES || validation_bytes > MAX_VALIDATION_BYTES
            || additional_archives.iter().any(|path| !path.is_absolute() || path.as_os_str().is_empty()
                || path.as_os_str().len() > 16_384) { return Err(SavedTrailError::InvalidLimits); }
        if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
        let owner = self.owner();
        let archive_digest = self.archive_digest();
        let [out_id, reply_id, path_id, pack_id, metadata_id, source_id, capture_id, archives_id] = self.allocations()?;
        let _path = self.budget.try_reserve_managed(owner, path_id, ByteLength::new(64 * 1024))
            .map_err(|_| AppError::Admission)?;
        let destination = input::absolute(request.destination)?;
        if destination.as_os_str().len() > 16_384 { return Err(SavedTrailError::InvalidLimits); }
        let accepted = self.trail.as_ref().ok_or(SavedTrailError::MissingTrail)?;
        accepted.validate(owner, archive_digest, request.trail.generation)?;
        if accepted.digest != request.trail.digest { return Err(SavedTrailError::ParentMismatch); }
        let view = TrailView::open(&accepted.bytes, &mut canceled)?;
        let mixed = !additional_archives.is_empty();
        let receipt_bytes = if mixed { MIXED_RECEIPT_BYTES } else { RECEIPT_BYTES };
        let mut receipt = Output::new(owner, receipt_bytes, &self.budget, out_id)?;
        let (reply, reply_lease) = reply_storage(owner, &self.budget, reply_id, receipt_bytes)?;
        encode_prefix(self, &mut receipt, request, &destination, view, mixed)?;
        let mut archives = Archives::new(Some(archive_digest), additional_archives.len(), validation_bytes);
        let mut stats = TrailExportStats { total_entries: view.len(), ..TrailExportStats::default() };
        let before = self.archive.load_stats();
        let built = (|| -> Result<(Output, usize, Sha256Digest), SavedTrailError> {
            if mixed { archives.open_paths(additional_archives, owner, &self.budget, archives_id, &mut canceled)?; }
            let mut pack = Output::new(owner, request.limits.output_bytes, &self.budget, pack_id)?;
            encode_header(&mut pack, view, &self.archive, self.archive_limits.max_document_bytes,
                request.limits, if mixed { Some(&archives) } else { None })?;
            let mut metadata = Output::new(owner, METADATA_BYTES, &self.budget, metadata_id)?;
            let mut reserved_bytes = pack.as_bytes().len() + TAIL_RESERVE;
            let mut reserved_scalars = scalars(pack.as_bytes())? + TAIL_RESERVE;
            for (ordinal, entry) in view.entries().take(request.limits.entries).enumerate() {
                check(&mut canceled)?;
                metadata.clear(); entry_output(&mut metadata, entry?, ordinal)?;
                reserved_bytes = reserved_bytes.checked_add(metadata.as_bytes().len() + ENTRY_RESERVE)
                    .ok_or(SavedTrailError::InvalidLimits)?;
                reserved_scalars = reserved_scalars.checked_add(scalars(metadata.as_bytes())? + ENTRY_RESERVE)
                    .ok_or(SavedTrailError::InvalidLimits)?;
            }
            let remaining_bytes = request.limits.output_bytes.checked_sub(reserved_bytes)
                .ok_or(SavedTrailError::InvalidLimits)?;
            let remaining_scalars = request.limits.output_scalars.checked_sub(reserved_scalars)
                .ok_or(SavedTrailError::InvalidLimits)?;
            let output_selection_limit = (remaining_bytes.min(remaining_scalars) / 2) as u64;
            let selected_limit = request.limits.selected_bytes.min(output_selection_limit);
            let reason = if request.limits.selected_bytes <= output_selection_limit { "TRAIL_EXPORT_SELECTED_BYTE_LIMIT" }
                else if remaining_bytes <= remaining_scalars { "TRAIL_EXPORT_OUTPUT_BYTE_LIMIT" }
                else { "TRAIL_EXPORT_OUTPUT_CHARACTER_LIMIT" };
            let generation = QueryGeneration::new(owner, request.operation).map_err(|_| SavedTrailError::InvalidLimits)?;
            let file = FileId::new(owner, 1).map_err(|_| SavedTrailError::InvalidLimits)?;
            let revision = SourceRevision::new(owner, 1).map_err(|_| SavedTrailError::InvalidLimits)?;
            let mut cursor = TrailExportCursor::new(view, generation, file, revision, TrailExportLimits {
                max_entries: request.limits.entries, max_source_bytes: request.limits.verification_bytes,
                max_selected_bytes: selected_limit })?;
            stats = cursor.stats();
            while cursor.stats().entries_visited < view.len().min(request.limits.entries) {
                let entry = view.entry(cursor.stats().entries_visited)?;
                let next = if let Some(archive) = archives.for_digest(entry.archive) {
                    cursor.next(archive, &self.budget, [source_id, capture_id], &mut canceled)
                } else {
                    // The retained primary handles its own digest. For omitted
                    // digests, the cursor emits OtherArchive before any reads.
                    cursor.next(&mut self.archive, &self.budget, [source_id, capture_id], &mut canceled)
                };
                stats = cursor.stats();
                let Some(item) = next? else { break };
                if item.target.ordinal() > 0 { pack.literal(",")?; }
                pack.literal("{\"selection\":")?;
                entry_output(&mut pack, item.target.entry(), item.target.ordinal())?;
                let (readiness, omission, bytes) = match &item.outcome {
                    TrailExportOutcome::Verified(capture) => ("digest-verified", None, Some(capture.selected_bytes())),
                    TrailExportOutcome::Unavailable(error) => ("unavailable", Some(error.to_string()), None),
                    TrailExportOutcome::SourceByteLimit => ("not-checked", Some("TRAIL_EXPORT_SOURCE_BYTE_LIMIT".to_owned()), None),
                    TrailExportOutcome::SelectionByteLimit => ("not-checked", Some(reason.to_owned()), None),
                };
                pack.literal(",\"readiness\":")?; pack.quoted(readiness)?;
                pack.literal(",\"reason\":")?;
                if let Some(reason) = omission { pack.quoted(&reason)?; } else { pack.literal("null")?; }
                pack.literal(",\"selected_original_hex\":")?;
                if let Some(bytes) = bytes { source_hex(&mut pack, bytes, &mut canceled)?; } else { pack.literal("null")?; }
                pack.literal("}")?;
            }
            pack.literal("],\"coverage\":")?; coverage(&mut pack, stats)?;
            pack.literal(",\"omitted_tail\":{\"reason\":\"TRAIL_EXPORT_ENTRY_LIMIT\",\"first_item\":")?;
            if stats.omitted_entry_limit > 0 { pack.integer(stats.entries_visited as u64 + 1)?; } else { pack.literal("null")?; }
            pack.literal(",\"count\":")?; pack.integer(stats.omitted_entry_limit as u64)?;
            pack.literal("},\"output_omission_reason\":")?; pack.quoted(reason)?;
            pack.literal(",\"complete\":")?; pack.boolean(stats.complete())?; pack.literal("}\n")?;
            let count = scalars(pack.as_bytes())?;
            if count > request.limits.output_scalars { return Err(SavedTrailError::InvalidLimits); }
            let mut digest = Sha256::new();
            for chunk in pack.as_bytes().chunks(16 * 1024) { check(&mut canceled)?; digest.update(chunk); }
            Ok((pack, count, digest.finalize()))
        })();
        let after = self.archive.load_stats();
        if mixed {
            receipt.literal(",\"additional_archives\":[")?;
            for (ordinal, path) in additional_archives.iter().enumerate() {
                if ordinal > 0 { receipt.literal(",")?; }
                receipt.literal("{\"ordinal\":")?; receipt.integer(ordinal as u64)?;
                receipt.literal(",\"path\":")?; receipt.path(path)?;
                receipt.literal(",\"opened\":")?; receipt.boolean(archives.inputs[ordinal].opened)?;
                receipt.literal(",\"digest\":")?;
                match archives.inputs[ordinal].digest {
                    Some(digest) => receipt.quoted(&digest.to_hex())?, None => receipt.literal("null")?,
                }
                receipt.literal("}")?;
            }
            receipt.literal("],\"archive_validation\":")?; archives.encode_validation(&mut receipt)?;
        }
        receipt.literal(",\"coverage\":")?; coverage(&mut receipt, stats)?;
        receipt.literal(",\"coverage_complete\":")?; receipt.boolean(built.is_ok() && stats.complete())?;
        receipt.literal(",\"member_bytes_read\":")?; receipt.integer(stats.source_bytes_read)?;
        receipt.literal(",\"member_read_calls\":")?; receipt.integer(after.read_calls.saturating_sub(before.read_calls) + archives.member_read_calls())?;
        let artifact_bytes = built.as_ref().map_or(0, |(p, _, _)| p.as_bytes().len() as u64);
        receipt.literal(",\"export_bytes\":")?; receipt.integer(artifact_bytes)?;
        receipt.literal(",\"export_characters\":")?; receipt.integer(built.as_ref().map_or(0, |(_, n, _)| *n as u64))?;
        receipt.literal(",\"export_digest\":")?;
        if let Ok((_, _, digest)) = &built { receipt.quoted(&digest.to_hex())?; } else { receipt.literal("null")?; }
        if receipt.remaining_bytes() < 4096 { return Err(AppError::Admission.into()); }
        let tail = receipt.checkpoint();
        let mut retained = RetainedTrailExport { operation: request.operation, receipt, receipt_bytes, tail, encoded_tail: false,
            effect: CheckpointSaveEffect::None, written: 0, artifact_bytes, coverage: stats, failure: None };
        match built {
            Ok((pack, _, _)) => retained.failure = publish_new_bytes(&destination, pack.as_bytes(),
                &mut retained.effect, &mut retained.written, &mut canceled).err().map(ExportFailure::Write),
            Err(error) => retained.failure = Some(ExportFailure::Trail(error)),
        }
        self.trail_export = Some(retained);
        self.trail_export_response(reply, reply_lease)
    }

    /// Exact effect recovery performs no archive, trail or destination I/O and
    /// never replays publication. The latest admitted receipt survives detach.
    pub fn trail_export_receipt(&mut self, operation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedTrailError> {
        check(&mut canceled)?;
        let retained = self.trail_export.as_ref().ok_or(SavedTrailError::MissingExport)?;
        if operation != retained.operation { return Err(SavedTrailError::StaleExport); }
        let receipt_bytes = retained.receipt_bytes;
        let [id] = self.allocations()?;
        let (reply, lease) = reply_storage(self.owner(), &self.budget, id, receipt_bytes)?;
        check(&mut canceled)?;
        self.trail_export_response(reply, lease)
    }
    fn trail_export_response(&mut self, mut text: String, lease: ResourceLease)
        -> Result<HostResponse, SavedTrailError> {
        let retained = self.trail_export.as_mut().ok_or(SavedTrailError::MissingExport)?;
        retained.encode_tail()?;
        if retained.receipt.as_bytes().len() > text.capacity() { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(retained.receipt.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        Ok(HostResponse { text, exit_code: retained.exit_code(), _lease: lease })
    }
}

fn reply_storage(owner: ArenaOwnerId, budget: &ResourceBudget, id: ResourceAllocationId, capacity: usize)
    -> Result<(String, ResourceLease), SavedTrailError> {
    let lease = budget.try_reserve_managed(owner, id, ByteLength::new((capacity * 3 + 4096) as u64))
        .map_err(|_| AppError::Admission)?;
    let mut text = String::new(); text.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
    if text.capacity() > capacity { return Err(AppError::Admission.into()); }
    Ok((text, lease))
}
fn limits(out: &mut Output, limits: SavedTrailExportLimits) -> Result<(), OutputError> {
    out.literal("{\"entries\":")?; out.integer(limits.entries as u64)?;
    out.literal(",\"verification_source_bytes\":")?; out.integer(limits.verification_bytes)?;
    out.literal(",\"selected_original_bytes\":")?; out.integer(limits.selected_bytes)?;
    out.literal(",\"export_bytes\":")?; out.integer(limits.output_bytes as u64)?;
    out.literal(",\"export_characters\":")?; out.integer(limits.output_scalars as u64)?;
    out.literal(",\"admission\":\"whole-selections-with-reserved-provenance\",\"per_entry_overhead_reserved\":")?;
    out.integer(ENTRY_RESERVE as u64)?;
    out.literal(",\"footer_reserved\":")?; out.integer(TAIL_RESERVE as u64)?;
    out.literal("}")
}
fn encode_prefix(session: &SavedRepositorySession, out: &mut Output, request: SavedTrailExportRequest<'_>,
    destination: &Path, view: TrailView<'_>, mixed: bool) -> Result<(), OutputError> {
    out.literal("{\"schema\":\"fcb.saved-repository/1\",\"command\":\"trail-export\",\"owner\":")?;
    out.integer(session.owner().get())?;
    out.literal(",\"archive_digest\":")?; out.quoted(&session.archive_digest().to_hex())?;
    out.literal(",\"members_total\":")?; out.integer(session.archive.directory().len() as u64)?;
    out.literal(",\"captured_members\":")?; out.integer(session.archive.directory().captured_files() as u64)?;
    out.literal(",\"discovery_complete\":")?; out.boolean(session.archive.directory().discovery_complete())?;
    out.literal(",\"operation\":")?; out.integer(request.operation)?;
    out.literal(",\"last_trail_export_attempt\":")?; out.integer(request.operation)?;
    out.literal(",\"trail_generation\":")?; out.integer(request.trail.generation)?;
    out.literal(",\"trail_digest\":")?; out.quoted(&view.digest().to_hex())?;
    out.literal(",\"destination\":")?; out.path(destination)?;
    out.literal(",\"pack_schema\":")?; out.quoted(PACK_SCHEMA)?;
    out.literal(",\"budgets\":")?; limits(out, request.limits)?;
    out.literal(",\"character_representation\":\"unicode-scalars-in-serialized-utf8-json\",\"archive_scope\":")?;
    out.quoted(archive_scope(mixed))?;
    out.literal(",\"archive_path_reopened\":false,\"live_source_reopened\":false,\"accepted_trail_unchanged\":true,\"accepted_query_unchanged\":true,\"source_derived_sensitive\":true,\"redacted\":false,\"encrypted\":false,\"overwritten\":false,\"power_loss_qualified\":false,\"live_root_authority\":false,\"native_presented\":false")
}
fn archive_scope(mixed: bool) -> &'static str {
    if mixed { "retained-primary-and-explicit-supplemental-archives" } else { "retained-single-archive" }
}
fn encode_header(out: &mut Output, view: TrailView<'_>, archive: &PagedSnapshot<File>,
    max_archive_bytes: usize, options: SavedTrailExportLimits, supplemental: Option<&Archives>) -> Result<(), OutputError> {
    header_prefix(out, view)?;
    if supplemental.is_some_and(|archives| archives.count > 0) { out.literal("null")?; }
    else { primary_archive_output(out, archive, max_archive_bytes)?; }
    if let Some(archives) = supplemental {
        out.literal(",\"archives\":[")?; primary_archive_output(out, archive, max_archive_bytes)?;
        for additional in archives.handles.iter().flatten() {
            out.literal(",")?; archive_output(out, additional)?;
        }
        out.literal("],\"archive_validation\":")?; archives.encode_validation(out)?;
    }
    out.literal(",\"archive_scope\":")?; out.quoted(archive_scope(supplemental.is_some()))?;
    out.literal(",\"budgets\":")?; limits(out, options)?;
    out.literal(",\"entries\":[")
}
fn primary_archive_output(out: &mut Output, archive: &PagedSnapshot<File>, max_archive_bytes: usize)
    -> Result<(), OutputError> {
    out.literal("{\"digest\":")?; out.quoted(&archive.directory().digest().to_hex())?;
    out.literal(",\"discovery_complete\":")?; out.boolean(archive.directory().discovery_complete())?;
    out.literal(",\"validation_bytes_read\":")?; out.integer(archive.directory().validation_stats().bytes_read)?;
    out.literal(",\"validation_read_calls\":")?; out.integer(archive.directory().validation_stats().read_calls)?;
    out.literal(",\"max_archive_bytes\":")?; out.integer(max_archive_bytes as u64)?;
    out.literal("}")
}
impl From<ArchiveSetError> for SavedTrailError {
    fn from(error: ArchiveSetError) -> Self {
        match error {
            ArchiveSetError::Input(error) => error.into(),
            ArchiveSetError::Archive(error) => fcb::search::trail::TrailReadError::Archive(error).into(),
            ArchiveSetError::ArchiveLimit => Self::InvalidLimits,
            ArchiveSetError::ValidationByteLimit => Self::ExportValidationByteLimit,
            ArchiveSetError::DirectoryAdmission => Self::ExportDirectoryAdmission,
        }
    }
}
fn scalars(bytes: &[u8]) -> Result<usize, SavedTrailError> {
    Ok(std::str::from_utf8(bytes).map_err(|_| SavedTrailError::InvalidLimits)?.chars().count())
}
fn source_hex(out: &mut Output, bytes: &[u8], canceled: &mut impl FnMut() -> bool)
    -> Result<(), SavedTrailError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = [0u8; 4096]; out.literal("\"")?;
    for chunk in bytes.chunks(2048) {
        check(canceled)?;
        for (index, &byte) in chunk.iter().enumerate() {
            encoded[index * 2] = HEX[(byte >> 4) as usize]; encoded[index * 2 + 1] = HEX[(byte & 15) as usize];
        }
        out.literal(std::str::from_utf8(&encoded[..chunk.len() * 2]).map_err(|_| SavedTrailError::InvalidLimits)?)?;
    }
    out.literal("\"")?; Ok(())
}

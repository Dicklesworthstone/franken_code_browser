#![forbid(unsafe_code)]

//! Create or edit a NEW immutable FCBT reading trail. New source selections
//! verify the retained archive descriptor; edits preserve existing exact source
//! anchors without loading any source payload. The canonical trail engine
//! encodes every retained visit and the shared create-only writer publishes. No
//! accepted trail, reader or search is replaced, and no pathname is reopened to
//! acquire source authority. One retained terminal receipt reconciles lost
//! replies without replaying a write, including partial and canceled effects.

use std::{borrow::Cow, path::Path};
use fcb::{ArenaOwnerId, ByteLength, ByteRange};
use fcb::search::{RawPath, ResourceAllocationId, ResourceBudget};
use fcb_core::ResourceLease;
use fcb::search::paged_snapshot::PagedMemberData;
use fcb::search::trail::{append_selection, edit_trail as edit_reference_trail, pin_selection, TrailBytes, TrailEdit, TrailError,
    TrailReadError, TrailView, MAX_TRAIL_BYTES, MAX_TRAIL_ITEMS};
use fcb::store::{Sha256Digest, trail::{MAX_RATIONALE_BYTES, MAX_TRAIL_PATH_BYTES, MAX_TRAIL_TITLE_BYTES}};
use crate::{AppError, EXIT_CANCELED, EXIT_ERROR, EXIT_OK, input};
use crate::host::HostResponse;
use crate::host::desk::persistence::{publish_new_bytes, CheckpointIoError, CheckpointSaveEffect};
use crate::output::{Output, OutputError};
use super::{SavedRepositorySession, SavedTrailError, check, optional};

const MAX_RECEIPT_BYTES: usize = 256 * 1024;

/// The exact already accepted attachment reviewed by the author. A pathname is
/// not a parent identity. Mixed-archive and unavailable visits are preserved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SavedTrailWriteParent { pub generation: u64, pub digest: Sha256Digest }

/// A frozen user selection. `parent: None` creates a new trail; appending keeps
/// the parent's exact title and requires this supplied title to agree with it.
/// Empty selections are point bookmarks. Text is bounded UTF-8, including NUL.
#[derive(Clone, Copy)]
pub struct SavedTrailWriteRequest<'a> {
    pub operation: u64,
    pub parent: Option<SavedTrailWriteParent>,
    pub member: usize,
    pub selection: ByteRange,
    pub title: &'a str,
    pub rationale: &'a str,
    pub destination: &'a Path,
}

/// Edit only the exact currently attached immutable parent. All remaining
/// visits retain their original source anchors, including unavailable and other-
/// archive visits. Publication creates a new file; attachment is a separate act.
/// Create, append and edit operations share one monotonic write namespace and
/// the same retained terminal receipt.
#[derive(Clone, Copy)]
pub struct SavedTrailEditRequest<'a> {
    pub operation: u64,
    pub parent: SavedTrailWriteParent,
    pub edit: TrailEdit<'a>,
    pub destination: &'a Path,
}

#[derive(Clone, Copy)]
enum WriteFailure { Trail(SavedTrailError), Write(CheckpointIoError) }
impl WriteFailure {
    fn is_canceled(self) -> bool {
        match self { Self::Trail(e) => e.is_canceled(), Self::Write(e) => e.is_canceled() }
    }
    fn code(self) -> Cow<'static, str> {
        match self {
            // Engine failures precede every filesystem effect.
            Self::Trail(e) => Cow::Owned(e.to_string()),
            // After create, producing the bounded outcome tail allocates nothing.
            Self::Write(e) => Cow::Borrowed(match e {
                CheckpointIoError::DestinationExists => "SAVED_TRAIL_DESTINATION_EXISTS",
                CheckpointIoError::InvalidPath => "SAVED_TRAIL_DESTINATION_PATH",
                CheckpointIoError::IoCallLimit => "SAVED_TRAIL_WRITE_CALL_LIMIT",
                CheckpointIoError::Canceled => "SAVED_TRAIL_WRITE_CANCELED",
                _ => "SAVED_TRAIL_WRITE_IO",
            }),
        }
    }
}

pub(in crate::host::saved_repository) struct RetainedTrailWrite {
    operation: u64, receipt: Output, tail_start: usize, tail_complete: bool,
    effect: CheckpointSaveEffect, bytes_written: u64, encoded: bool,
    failure: Option<WriteFailure>,
}
impl RetainedTrailWrite {
    fn exit_code(&self) -> u8 {
        match self.failure { Some(e) if e.is_canceled() => EXIT_CANCELED,
            Some(_) => EXIT_ERROR, None => EXIT_OK }
    }
    fn encode_tail(&mut self) -> Result<(), SavedTrailError> {
        if self.tail_complete { return Ok(()); }
        self.receipt.restore(self.tail_start)?;
        self.receipt.literal(",\"status\":")?;
        self.receipt.quoted(if self.failure.is_some() { "error" } else { "ok" })?;
        self.receipt.literal(",\"effect\":")?; self.receipt.quoted(self.effect.code())?;
        self.receipt.literal(",\"bytes_written\":")?; self.receipt.integer(self.bytes_written)?;
        let complete = self.encoded && matches!(self.effect, CheckpointSaveEffect::Written
            | CheckpointSaveEffect::FileSynced | CheckpointSaveEffect::DirectorySynced);
        self.receipt.literal(",\"complete_artifact\":")?; self.receipt.boolean(complete)?;
        self.receipt.literal(",\"attach_eligible\":")?; self.receipt.boolean(complete)?;
        self.receipt.literal(",\"error\":")?;
        if let Some(error) = self.failure {
            self.receipt.literal("{\"code\":")?; self.receipt.quoted(&error.code())?;
            self.receipt.literal(",\"next_action\":\"Inspect this retained effect and any created destination before an explicit new operation; never replay a lost write reply.\"}")?;
        } else { self.receipt.literal("null")?; }
        self.receipt.literal("}\n")?; self.tail_complete = true;
        Ok(())
    }
}

impl SavedRepositorySession {
    pub const fn last_trail_write_attempt(&self) -> u64 { self.last_trail_write_attempt }
    pub fn last_trail_write_operation(&self) -> Option<u64> { self.trail_write.as_ref().map(|r| r.operation) }

    /// Verify one original-byte selection and create a NEW reference-only
    /// trail. Appends preserve every parent visit and rationale, including
    /// repeated selections and other archives. Never overwrites or attaches.
    ///
    /// Operation IDs increase independently of attachment/query generations.
    /// Invalid or pre-admission requests preserve the preceding receipt; an
    /// admitted engine or write outcome retains a complete terminal receipt.
    /// Output capacity is admitted before source verification and publication.
    pub fn write_trail(&mut self, request: SavedTrailWriteRequest<'_>,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedTrailError> {
        if request.operation == 0 || request.operation <= self.last_trail_write_attempt {
            return Err(SavedTrailError::StaleWrite);
        }
        self.last_trail_write_attempt = request.operation;
        check(&mut canceled)?;
        if request.title.len() > MAX_TRAIL_TITLE_BYTES || request.rationale.len() > MAX_RATIONALE_BYTES
            || request.destination.as_os_str().is_empty() || request.destination.as_os_str().len() > 16_384 {
            return Err(SavedTrailError::InvalidLimits);
        }
        if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
        let owner = self.owner();
        let archive_digest = self.archive_digest();
        let [output_id, reply_id, path_id, source_id, entries_id, artifact_id] = self.allocations()?;
        // Covers the explicit destination, exact native member path copy and
        // temporary label before any allocation or pathname normalization.
        let _paths = self.budget.try_reserve_managed(owner, path_id, ByteLength::new(64 * 1024))
            .map_err(|_| AppError::Admission)?;
        let destination = input::absolute(request.destination)?;
        if destination.as_os_str().len() > 16_384 { return Err(SavedTrailError::InvalidLimits); }
        let parent = match request.parent {
            Some(expected) => {
                let trail = self.trail.as_ref().ok_or(SavedTrailError::MissingTrail)?;
                trail.validate(owner, archive_digest, expected.generation)?;
                if trail.digest != expected.digest { return Err(SavedTrailError::ParentMismatch); }
                let view = TrailView::open(&trail.bytes, &mut canceled)?;
                if request.title != view.title() { return Err(SavedTrailError::ParentMismatch); }
                if view.len() == MAX_TRAIL_ITEMS { return Err(TrailError::Limit.into()); }
                Some(view)
            }
            None => None,
        };
        let result_items = parent.map_or(0, |view| view.len() as u64) + 1;
        let result_reference_bytes = parent.map_or(0, |view| view.reference_bytes())
            .checked_add(request.selection.len().get()).ok_or(TrailError::Limit)?;
        let member = self.archive.directory().member(request.member).ok_or(TrailReadError::MissingMember)?;
        let (source_bytes, source_digest) = match member.data {
            PagedMemberData::Captured { byte_length, digest, .. } => (byte_length as u64, digest),
            PagedMemberData::Unavailable(_) => return Err(TrailReadError::UnavailableMember.into()),
        };
        if request.selection.end().get() > source_bytes { return Err(TrailReadError::InvalidRange.into()); }
        if member.path.len() > MAX_TRAIL_PATH_BYTES { return Err(TrailError::Path.into()); }
        let mut path = Vec::new(); path.try_reserve_exact(member.path.len()).map_err(|_| AppError::Admission)?;
        if path.capacity() > member.path.len() { return Err(AppError::Admission.into()); }
        path.extend_from_slice(member.path);
        let mut out = Output::new(owner, MAX_RECEIPT_BYTES, &self.budget, output_id)?;
        let (reply, reply_lease) = reply_storage(owner, &self.budget, reply_id)?;
        encode_prefix(self, &mut out, request, &destination, parent, &path, source_bytes, source_digest)?;
        let before = self.archive.load_stats();
        let mut source_verified = false;
        // Field borrows remain disjoint: the parent references retained FCBT
        // bytes while only the original open archive descriptor performs I/O.
        let built = (|| -> Result<TrailBytes, SavedTrailError> {
            let selection = pin_selection(&mut self.archive, &path, request.selection,
                request.rationale, &self.budget, source_id, &mut canceled)?;
            source_verified = true;
            Ok(append_selection(owner, parent, request.title, selection, &self.budget,
                [entries_id, artifact_id], &mut canceled)?)
        })();
        let after = self.archive.load_stats();
        encode_artifact(&mut out, built.as_ref().ok(), result_items, result_reference_bytes, source_verified,
            after.bytes_read.saturating_sub(before.bytes_read),
            after.read_calls.saturating_sub(before.read_calls))?;
        self.publish_trail_write(request.operation, &destination, built, out, reply, reply_lease, &mut canceled)
    }

    /// Curate a retained trail without reading any saved source payload or
    /// reopening the parent path. Ordinals identify visits, not unique sources;
    /// repeated selections and their distinct notes remain distinct. The parent
    /// generation and digest must both match the currently accepted attachment.
    ///
    /// A rename, rationale change, move or removal creates one new immutable
    /// generation. Empty trails are valid. Attachment, query and readers remain
    /// unchanged until the caller explicitly attaches the newly saved artifact.
    pub fn edit_trail(&mut self, request: SavedTrailEditRequest<'_>,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedTrailError> {
        if request.operation == 0 || request.operation <= self.last_trail_write_attempt {
            return Err(SavedTrailError::StaleWrite);
        }
        self.last_trail_write_attempt = request.operation;
        check(&mut canceled)?;
        if request.destination.as_os_str().is_empty() || request.destination.as_os_str().len() > 16_384
            || matches!(request.edit, TrailEdit::Rename { title } if title.len() > MAX_TRAIL_TITLE_BYTES)
            || matches!(request.edit, TrailEdit::SetRationale { rationale, .. } if rationale.len() > MAX_RATIONALE_BYTES) {
            return Err(SavedTrailError::InvalidLimits);
        }
        if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
        let owner = self.owner();
        let archive_digest = self.archive_digest();
        let [output_id, reply_id, path_id, entries_id, artifact_id] = self.allocations()?;
        let _paths = self.budget.try_reserve_managed(owner, path_id, ByteLength::new(64 * 1024))
            .map_err(|_| AppError::Admission)?;
        let destination = input::absolute(request.destination)?;
        if destination.as_os_str().len() > 16_384 { return Err(SavedTrailError::InvalidLimits); }
        let trail = self.trail.as_ref().ok_or(SavedTrailError::MissingTrail)?;
        trail.validate(owner, archive_digest, request.parent.generation)?;
        if trail.digest != request.parent.digest { return Err(SavedTrailError::ParentMismatch); }
        let parent = TrailView::open(&trail.bytes, &mut canceled)?;
        let (title, result_items, result_reference_bytes) = edit_result(parent, request.edit)?;
        let mut out = Output::new(owner, MAX_RECEIPT_BYTES, &self.budget, output_id)?;
        let (reply, reply_lease) = reply_storage(owner, &self.budget, reply_id)?;
        encode_edit_prefix(self, &mut out, request, &destination, parent, title)?;
        let built = edit_reference_trail(owner, parent, request.edit, &self.budget,
            [entries_id, artifact_id], &mut canceled).map_err(SavedTrailError::from);
        encode_artifact(&mut out, built.as_ref().ok(), result_items, result_reference_bytes, false, 0, 0)?;
        self.publish_trail_write(request.operation, &destination, built, out, reply, reply_lease, &mut canceled)
    }

    /// Every authoring route uses the same create-only publication boundary.
    /// All receipt/reply capacity is admitted before this function can create a
    /// destination, and its effect is retained before any fallible delivery.
    #[allow(clippy::too_many_arguments)]
    fn publish_trail_write(&mut self, operation: u64, destination: &Path,
        built: Result<TrailBytes, SavedTrailError>, out: Output, reply: String,
        reply_lease: ResourceLease, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, SavedTrailError> {
        if out.remaining_bytes() < 4096 { return Err(AppError::Admission.into()); }
        let tail_start = out.checkpoint();
        let mut retained = RetainedTrailWrite { operation, receipt: out, tail_start,
            tail_complete: false, effect: CheckpointSaveEffect::None, bytes_written: 0,
            encoded: built.is_ok(), failure: None };
        match built {
            Ok(artifact) => retained.failure = publish_new_bytes(destination, artifact.bytes(),
                &mut retained.effect, &mut retained.bytes_written, canceled).err().map(WriteFailure::Write),
            Err(error) => retained.failure = Some(WriteFailure::Trail(error)),
        }
        self.trail_write = Some(retained);
        self.trail_write_response(reply, reply_lease)
    }

    /// Return the exact terminal receipt. No source/destination I/O, attachment,
    /// reconstruction or writer replay occurs during recovery.
    pub fn trail_write_receipt(&mut self, operation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedTrailError> {
        check(&mut canceled)?;
        let retained = self.trail_write.as_ref().ok_or(SavedTrailError::MissingWrite)?;
        if retained.operation != operation { return Err(SavedTrailError::StaleWrite); }
        let [id] = self.allocations()?;
        let (reply, lease) = reply_storage(self.owner(), &self.budget, id)?;
        check(&mut canceled)?;
        self.trail_write_response(reply, lease)
    }

    fn trail_write_response(&mut self, mut text: String, lease: ResourceLease)
        -> Result<HostResponse, SavedTrailError> {
        let retained = self.trail_write.as_mut().ok_or(SavedTrailError::MissingWrite)?;
        retained.encode_tail()?;
        let bytes = retained.receipt.as_bytes();
        if bytes.len() > text.capacity() { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(bytes).map_err(|_| AppError::InvalidRange)?);
        Ok(HostResponse { text, exit_code: retained.exit_code(), _lease: lease })
    }
}

fn reply_storage(owner: ArenaOwnerId, budget: &ResourceBudget, id: ResourceAllocationId)
    -> Result<(String, ResourceLease), SavedTrailError> {
    let lease = budget.try_reserve_managed(owner, id, ByteLength::new((MAX_RECEIPT_BYTES * 3 + 4096) as u64))
        .map_err(|_| AppError::Admission)?;
    let mut text = String::new(); text.try_reserve_exact(MAX_RECEIPT_BYTES).map_err(|_| AppError::Admission)?;
    if text.capacity() > MAX_RECEIPT_BYTES { return Err(AppError::Admission.into()); }
    Ok((text, lease))
}

#[allow(clippy::too_many_arguments)]
fn encode_prefix(session: &SavedRepositorySession, out: &mut Output, request: SavedTrailWriteRequest<'_>,
    destination: &Path, parent: Option<TrailView<'_>>, path: &[u8], source_bytes: u64,
    source_digest: Sha256Digest) -> Result<(), OutputError> {
    encode_receipt_identity(session, out, "trail-write", request.operation, destination)?;
    out.literal(",\"mode\":")?; out.quoted(if parent.is_some() { "append" } else { "new" })?;
    encode_parent(out, request.parent.map(|p| p.generation), parent)?;
    out.literal(",\"member\":")?; out.integer(request.member as u64)?;
    out.literal(",\"file_id\":")?; out.integer(request.member as u64 + 1)?;
    out.literal(",\"source_revision\":")?; out.integer(request.member as u64 + 1)?;
    out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(path).to_path_buf())?;
    out.literal(",\"source_digest\":")?; out.quoted(&source_digest.to_hex())?;
    out.literal(",\"source_bytes\":")?; out.integer(source_bytes)?;
    out.literal(",\"selection\":")?; out.range(request.selection)?;
    out.literal(",\"title\":")?; out.quoted(request.title)?;
    out.literal(",\"rationale\":")?; out.quoted(request.rationale)?;
    encode_policy(out)
}

fn encode_receipt_identity(session: &SavedRepositorySession, out: &mut Output, command: &str,
    operation: u64, destination: &Path) -> Result<(), OutputError> {
    out.literal("{\"schema\":\"fcb.saved-repository/1\",\"command\":")?; out.quoted(command)?;
    out.literal(",\"owner\":")?; out.integer(session.owner().get())?;
    out.literal(",\"archive_digest\":")?; out.quoted(&session.archive_digest().to_hex())?;
    out.literal(",\"members_total\":")?; out.integer(session.archive.directory().len() as u64)?;
    out.literal(",\"captured_members\":")?; out.integer(session.archive.directory().captured_files() as u64)?;
    out.literal(",\"discovery_complete\":")?; out.boolean(session.archive.directory().discovery_complete())?;
    out.literal(",\"live_root_authority\":false,\"native_presented\":false,\"operation\":")?;
    out.integer(operation)?;
    out.literal(",\"last_trail_write_attempt\":")?; out.integer(operation)?;
    out.literal(",\"destination\":")?; out.path(destination)?;
    Ok(())
}

fn encode_parent(out: &mut Output, generation: Option<u64>, parent: Option<TrailView<'_>>)
    -> Result<(), OutputError> {
    out.literal(",\"parent_generation\":")?; optional(out, generation)?;
    out.literal(",\"parent_digest\":")?;
    if let Some(view) = parent { out.quoted(&view.digest().to_hex())?; } else { out.literal("null")?; }
    out.literal(",\"parent_items\":")?; out.integer(parent.map_or(0, |view| view.len() as u64))?;
    Ok(())
}

fn encode_edit_prefix(session: &SavedRepositorySession, out: &mut Output,
    request: SavedTrailEditRequest<'_>, destination: &Path, parent: TrailView<'_>, title: &str)
    -> Result<(), OutputError> {
    encode_receipt_identity(session, out, "trail-edit", request.operation, destination)?;
    out.literal(",\"mode\":\"edit\"")?;
    encode_parent(out, Some(request.parent.generation), Some(parent))?;
    out.literal(",\"title\":")?; out.quoted(title)?;
    let (kind, ordinal, destination_ordinal, edited_title, rationale) = match request.edit {
        TrailEdit::Rename { title } => ("rename", None, None, Some(title), None),
        TrailEdit::SetRationale { ordinal, rationale } => ("set-rationale", Some(ordinal), None, None, Some(rationale)),
        TrailEdit::Move { ordinal, destination } => ("move", Some(ordinal), Some(destination), None, None),
        TrailEdit::Remove { ordinal } => ("remove", Some(ordinal), None, None, None),
    };
    out.literal(",\"edit\":{\"kind\":")?; out.quoted(kind)?;
    out.literal(",\"ordinal\":")?; optional(out, ordinal.map(|n| n as u64))?;
    out.literal(",\"destination_ordinal\":")?; optional(out, destination_ordinal.map(|n| n as u64))?;
    out.literal(",\"title\":")?;
    if let Some(value) = edited_title { out.quoted(value)?; } else { out.literal("null")?; }
    out.literal(",\"rationale\":")?;
    if let Some(value) = rationale { out.quoted(value)?; } else { out.literal("null")?; }
    out.literal("},\"source_anchors_unchanged\":true")?;
    encode_policy(out)
}

/// Validate ordinal identity before admitting a replacement receipt. No source
/// lookup participates: removal changes only the explicit visit count/byte sum.
fn edit_result<'a>(parent: TrailView<'a>, edit: TrailEdit<'a>) -> Result<(&'a str, u64, u64), SavedTrailError> {
    let count = parent.len() as u64;
    let bytes = parent.reference_bytes();
    match edit {
        TrailEdit::Rename { title } => Ok((title, count, bytes)),
        TrailEdit::SetRationale { ordinal, .. } => {
            parent.entry(ordinal)?;
            Ok((parent.title(), count, bytes))
        }
        TrailEdit::Move { ordinal, destination } => {
            parent.entry(ordinal)?;
            if destination >= parent.len() { return Err(TrailError::MissingItem.into()); }
            Ok((parent.title(), count, bytes))
        }
        TrailEdit::Remove { ordinal } => {
            let removed = parent.entry(ordinal)?;
            let bytes = bytes.checked_sub(removed.range.len().get()).ok_or(TrailError::Limit)?;
            Ok((parent.title(), count - 1, bytes))
        }
    }
}

fn encode_policy(out: &mut Output) -> Result<(), OutputError> {
    for (key, value) in [("max_trail_bytes", MAX_TRAIL_BYTES as u64), ("max_trail_items", MAX_TRAIL_ITEMS as u64),
        ("max_title_bytes", MAX_TRAIL_TITLE_BYTES as u64), ("max_rationale_bytes", MAX_RATIONALE_BYTES as u64)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    out.literal(",\"archive_path_reopened\":false,\"live_source_reopened\":false,\"source_payload_embedded\":false,\"trail_attached\":false,\"accepted_trail_unchanged\":true,\"accepted_query_unchanged\":true,\"source_derived_sensitive\":true,\"encrypted\":false,\"overwritten\":false,\"power_loss_qualified\":false,\"truncated\":false")
}

fn encode_artifact(out: &mut Output, artifact: Option<&TrailBytes>, items: u64, reference_bytes: u64, verified: bool,
    member_bytes_read: u64, member_read_calls: u64) -> Result<(), SavedTrailError> {
    out.literal(",\"source_verified\":")?; out.boolean(verified)?;
    out.literal(",\"member_bytes_read\":")?; out.integer(member_bytes_read)?;
    out.literal(",\"member_read_calls\":")?; out.integer(member_read_calls)?;
    out.literal(",\"trail_bytes\":")?; out.integer(artifact.map_or(0, |a| a.bytes().len() as u64))?;
    out.literal(",\"trail_digest\":")?;
    if let Some(artifact) = artifact { out.quoted(&artifact.digest().to_hex())?; } else { out.literal("null")?; }
    out.literal(",\"total_items\":")?; optional(out, artifact.map(|_| items))?;
    out.literal(",\"reference_bytes\":")?; optional(out, artifact.map(|_| reference_bytes))?;
    Ok(())
}

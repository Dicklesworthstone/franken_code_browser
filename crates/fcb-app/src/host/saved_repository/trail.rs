#![forbid(unsafe_code)]

//! Explicitly attached FCBT reading trails. The existing trail engine owns
//! validation, readiness, sequence targets and saved-payload verification.
//! Attachments retain bounded reference/note bytes, never source payloads or a
//! live root grant. Mixed-archive and unavailable visits remain visible.

use std::{io::{self, Read}, mem::size_of, path::{Path, PathBuf}};
use fcb::search::trail::{resolve_selection, TrailEntry, TrailError, TrailNavigator,
    TrailReadError, TrailView, MAX_TRAIL_BYTES, MAX_TRAIL_ITEMS};
use super::{ArenaOwnerId, AppError, ByteLength, FileId, HostResponse, Output, OutputError,
    PagedSearchError, PagedSnapshotError, QueryGeneration, RawPath, ReaderSession,
    ReaderSessionError, ResourceLease, SavedRepositoryError, SavedRepositorySession,
    Sha256Digest, SourceRevision, check, input, optional};

/// Even maximum-length raw names and rationale fit the bounded response.
pub const MAX_SAVED_TRAIL_PAGE: usize = 32;
const MAX_READ_CALLS: usize = 131_072;

/// Explicit immutable trail authoring and retained create-only write receipts.
pub mod write;
pub use write::{SavedTrailWriteParent, SavedTrailWriteRequest};
/// Self-contained evidence export from the explicitly retained archive.
pub mod export;
pub use export::{SavedTrailExportLimits, SavedTrailExportRequest};
/// Exact individual visits from an explicitly selected supplemental archive.
mod archive_reader;
pub use archive_reader::SavedTrailArchiveRequest;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavedTrailError {
    Saved(SavedRepositoryError), Trail(TrailReadError), InvalidLimits, MissingTrail,
    StaleGeneration, ReadFailed, FileChanged, ReadCallLimit,
    MissingWrite, StaleWrite, ParentMismatch, MissingExport, StaleExport,
    ExportValidationByteLimit, ExportDirectoryAdmission,
}
impl std::fmt::Display for SavedTrailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Saved(error) => write!(f, "{error}"), Self::Trail(error) => write!(f, "{error}"),
            Self::InvalidLimits => f.write_str("SAVED_TRAIL_INVALID_LIMITS"),
            Self::MissingTrail => f.write_str("SAVED_TRAIL_NOT_ATTACHED"),
            Self::StaleGeneration => f.write_str("SAVED_TRAIL_STALE_GENERATION"),
            Self::ReadFailed => f.write_str("SAVED_TRAIL_READ_FAILED"),
            Self::FileChanged => f.write_str("SAVED_TRAIL_FILE_CHANGED"),
            Self::ReadCallLimit => f.write_str("SAVED_TRAIL_READ_CALL_LIMIT"),
            Self::MissingWrite => f.write_str("SAVED_TRAIL_NO_WRITE"),
            Self::StaleWrite => f.write_str("SAVED_TRAIL_STALE_WRITE"),
            Self::ParentMismatch => f.write_str("SAVED_TRAIL_PARENT_MISMATCH"),
            Self::MissingExport => f.write_str("SAVED_TRAIL_NO_EXPORT"),
            Self::StaleExport => f.write_str("SAVED_TRAIL_STALE_EXPORT"),
            Self::ExportValidationByteLimit => f.write_str("TRAIL_EXPORT_VALIDATION_BYTE_LIMIT"),
            Self::ExportDirectoryAdmission => f.write_str("TRAIL_EXPORT_DIRECTORY_ADMISSION"),
        }
    }
}
impl std::error::Error for SavedTrailError {}
impl From<SavedRepositoryError> for SavedTrailError { fn from(error: SavedRepositoryError) -> Self { Self::Saved(error) } }
impl From<TrailReadError> for SavedTrailError { fn from(error: TrailReadError) -> Self { Self::Trail(error) } }
impl From<TrailError> for SavedTrailError { fn from(error: TrailError) -> Self { Self::Trail(error.into()) } }
impl From<AppError> for SavedTrailError { fn from(error: AppError) -> Self { Self::Saved(error.into()) } }
impl From<OutputError> for SavedTrailError { fn from(error: OutputError) -> Self { Self::Saved(error.into()) } }
impl From<ReaderSessionError> for SavedTrailError { fn from(error: ReaderSessionError) -> Self { Self::Saved(error.into()) } }
impl SavedTrailError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Saved(error) => error.is_canceled(),
            Self::Trail(TrailReadError::Format(TrailError::Canceled)
                | TrailReadError::Archive(PagedSnapshotError::Canceled)
                | TrailReadError::Capture(PagedSearchError::Canceled)
                | TrailReadError::Capture(PagedSearchError::Archive(PagedSnapshotError::Canceled))) => true,
            _ => false,
        }
    }
}

pub(super) struct AcceptedTrail {
    generation: u64, owner: ArenaOwnerId, archive: Sha256Digest, digest: Sha256Digest,
    bytes: Vec<u8>, path: PathBuf, ready: usize, read_calls: u64,
    _bytes_lease: ResourceLease, _state_lease: ResourceLease,
}
impl AcceptedTrail {
    fn validate(&self, owner: ArenaOwnerId, archive: Sha256Digest, generation: u64)
        -> Result<(), SavedTrailError> {
        if self.owner != owner || self.archive != archive || self.generation != generation {
            return Err(SavedTrailError::StaleGeneration);
        }
        Ok(())
    }
}

impl SavedRepositorySession {
    pub fn trail_generation(&self) -> Option<u64> { self.trail.as_ref().map(|trail| trail.generation) }
    pub const fn last_trail_attempt(&self) -> u64 { self.last_trail_attempt }

    /// Read and validate the one explicitly selected FCBT file. No archive
    /// named by an entry is opened. A candidate and its first page are fully
    /// prepared before replacing the previous accepted attachment.
    pub fn attach_trail(&mut self, generation: u64, path: &Path,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedTrailError> {
        trail_attempt(&mut self.last_trail_attempt, generation)?;
        check(&mut canceled)?;
        if path.as_os_str().is_empty() || path.as_os_str().len() > 16_384 {
            return Err(SavedTrailError::InvalidLimits);
        }
        let [state_id, bytes_id] = self.allocations()?;
        let state_lease = self.budget.try_reserve_managed(self.owner(), state_id,
            ByteLength::new((size_of::<AcceptedTrail>() + 64 * 1024) as u64)).map_err(|_| AppError::Admission)?;
        let path = input::absolute(path)?;
        let (mut file, before) = input::open_regular(&path)?;
        let size = usize::try_from(before.len()).map_err(|_| TrailError::Limit)?;
        if size > MAX_TRAIL_BYTES { return Err(TrailError::Limit.into()); }
        let bytes_lease = self.budget.try_reserve_managed(self.owner(), bytes_id,
            ByteLength::new(size as u64 + 256)).map_err(|_| AppError::Admission)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(|_| AppError::Admission)?;
        if bytes.capacity() > size { return Err(AppError::Admission.into()); }
        bytes.resize(size, 0);
        let mut offset = 0usize;
        let mut read_calls = 0u64;
        let mut eof = false;
        for _ in 0..MAX_READ_CALLS {
            check(&mut canceled)?;
            let mut extra = [0u8];
            let end = size.min(offset + 64 * 1024);
            let buffer = if offset == size { &mut extra[..] } else { &mut bytes[offset..end] };
            read_calls += 1;
            match file.read(buffer) {
                Ok(0) if offset == size => { eof = true; break; },
                Ok(count) if count > 0 && offset < size && count <= end - offset => offset += count,
                Ok(_) => return Err(SavedTrailError::FileChanged),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
                Err(_) => return Err(SavedTrailError::ReadFailed),
            }
        }
        if !eof { return Err(SavedTrailError::ReadCallLimit); }
        let after = file.metadata().map_err(|_| SavedTrailError::ReadFailed)?;
        if before.len() != after.len()
            || matches!((before.modified().ok(), after.modified().ok()), (Some(a), Some(b)) if a != b) {
            return Err(SavedTrailError::FileChanged);
        }
        check(&mut canceled)?;
        let view = TrailView::open(&bytes, &mut canceled)?;
        let mut ready = 0usize;
        for entry in view.entries() {
            check(&mut canceled)?;
            if readiness(entry?, self.archive.directory())?.member().is_some() { ready += 1; }
        }
        let digest = view.digest();
        let candidate = AcceptedTrail { generation, owner: self.owner(), archive: self.archive_digest(),
            digest, bytes, path, ready, read_calls, _bytes_lease: bytes_lease, _state_lease: state_lease };
        let mut out = self.output("trail-attach")?;
        self.encode_trail_page(&mut out, &candidate, 0, MAX_SAVED_TRAIL_PAGE, &mut canceled)?;
        let response = self.finish(out, false, &mut canceled)?;
        self.trail = Some(candidate);
        Ok(response)
    }

    /// Metadata only. No source payload is read or retained by a page. The
    /// attachment generation binds every ordinal even when notes are repeated.
    pub fn trail_page(&mut self, generation: u64, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedTrailError> {
        check(&mut canceled)?;
        let mut out = self.output("trail-page")?;
        let trail = self.accepted_trail(generation)?;
        self.encode_trail_page(&mut out, trail, start, limit, &mut canceled)?;
        Ok(self.finish(out, false, &mut canceled)?)
    }

    /// Detach reference state only. Independent readers keep their exact bytes.
    pub fn detach_trail(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedTrailError> {
        trail_attempt(&mut self.last_trail_attempt, generation)?;
        check(&mut canceled)?;
        let mut out = self.output("trail-detach")?;
        out.literal(",\"accepted_trail_generation\":null,\"last_trail_attempt\":")?;
        out.integer(generation)?;
        out.literal("}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        self.trail = None;
        Ok(response)
    }

    /// Reconstruct the shared sequence navigator and open its exact retained
    /// reference. The immutable attachment generation owns request staleness;
    /// the navigator's local generation is used only within this worker call.
    /// Full saved-member verification precedes independent reader publication.
    pub fn open_trail_reader(&mut self, reader_owner: ArenaOwnerId, generation: u64,
        item: usize, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), SavedTrailError> {
        check(&mut canceled)?;
        let owner = self.owner();
        let archive = self.archive_digest();
        if reader_owner == owner { return Err(TrailReadError::OwnerMismatch.into()); }
        let ids = self.allocations()?;
        let mut out = self.output("trail-open-reader")?;
        let before = self.archive.load_stats();
        let trail = self.trail.as_ref().ok_or(SavedTrailError::MissingTrail)?;
        trail.validate(owner, archive, generation)?;
        let view = TrailView::open(&trail.bytes, &mut canceled)?;
        let local = QueryGeneration::new(owner, 1).map_err(|_| TrailReadError::GenerationExhausted)?;
        let mut navigator = TrailNavigator::new(view, local);
        navigator.select(item)?;
        let target = navigator.target()?.ok_or(TrailError::MissingItem)?;
        let entry = target.entry();
        let member = resolve_selection(entry, self.archive.directory())?;
        let value = (member as u64).checked_add(1).ok_or(SavedRepositoryError::IdentityExhausted)?;
        let file = FileId::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        let revision = SourceRevision::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        out.literal(",\"trail_generation\":")?; out.integer(generation)?;
        out.literal(",\"trail_digest\":")?; out.quoted(&trail.digest.to_hex())?;
        out.literal(",\"item\":")?; out.integer(item as u64)?;
        out.literal(",\"entry\":")?;
        encode_entry(&mut out, item, entry, Readiness::Ready(member))?;
        let capture = target.open(&mut self.archive, trail.digest, navigator.generation(), file,
            revision, &self.budget, ids, &mut canceled)?;
        let label = RawPath::from_bytes(entry.path).to_path_buf();
        let mut reader = ReaderSession::from_bytes(reader_owner, &label, capture.bytes(), &mut canceled)?;
        let info = reader.info(&mut canceled)?;
        let offset = entry.range.start().get();
        let window = reader.read_window(offset.saturating_sub(132), 16 * 1024, &mut canceled)?;
        let after = self.archive.load_stats();
        out.literal(",\"member\":")?; out.integer(member as u64)?;
        out.literal(",\"file_id\":")?; out.integer(file.get())?;
        out.literal(",\"source_revision\":")?; out.integer(revision.get())?;
        out.literal(",\"source_digest\":")?; out.quoted(&entry.source.to_hex())?;
        out.literal(",\"path\":")?; out.path(&label)?;
        out.literal(",\"source_bytes\":")?; out.integer(entry.source_length)?;
        out.literal(",\"reader_owner\":")?; out.integer(reader_owner.get())?;
        out.literal(",\"source_observation\":\"verified-saved-member\",\"live_source_reopened\":false,\"offset\":")?;
        out.integer(offset)?;
        out.literal(",\"selection\":")?; out.range(entry.range)?;
        out.literal(",\"member_bytes_read\":")?; out.integer(after.bytes_read - before.bytes_read)?;
        out.literal(",\"member_read_calls\":")?; out.integer(after.read_calls - before.read_calls)?;
        out.literal(",\"reader\":")?; out.literal(info.as_str().trim_end())?;
        out.literal(",\"window\":")?; out.literal(window.as_str().trim_end())?;
        out.literal("}\n")?;
        drop(capture);
        let response = self.finish(out, false, &mut canceled)?;
        Ok((reader, response))
    }

    fn accepted_trail(&self, generation: u64) -> Result<&AcceptedTrail, SavedTrailError> {
        let trail = self.trail.as_ref().ok_or(SavedTrailError::MissingTrail)?;
        trail.validate(self.owner(), self.archive_digest(), generation)?;
        Ok(trail)
    }

    fn encode_trail_page(&self, out: &mut Output, trail: &AcceptedTrail,
        start: usize, limit: usize, canceled: &mut impl FnMut() -> bool) -> Result<(), SavedTrailError> {
        let view = TrailView::open(&trail.bytes, &mut *canceled)?;
        if !(1..=MAX_SAVED_TRAIL_PAGE).contains(&limit) || start > view.len() {
            return Err(SavedTrailError::InvalidLimits);
        }
        out.literal(",\"trail_generation\":")?; out.integer(trail.generation)?;
        out.literal(",\"accepted_trail_generation\":")?; out.integer(trail.generation)?;
        out.literal(",\"last_trail_attempt\":")?; out.integer(self.last_trail_attempt)?;
        out.literal(",\"trail_digest\":")?; out.quoted(&trail.digest.to_hex())?;
        out.literal(",\"trail_path\":")?; out.path(&trail.path)?;
        out.literal(",\"title\":")?; out.quoted(view.title())?;
        out.literal(",\"parent_digest\":")?;
        if let Some(parent) = view.parent_digest() { out.quoted(&parent.to_hex())?; } else { out.literal("null")?; }
        for (key, value) in [("total_items", view.len() as u64), ("reference_bytes", view.reference_bytes()),
            ("retained_trail_bytes", trail.bytes.len() as u64), ("ready_items", trail.ready as u64),
            ("unavailable_items", (view.len() - trail.ready) as u64), ("trail_read_calls", trail.read_calls),
            ("max_trail_bytes", MAX_TRAIL_BYTES as u64), ("max_trail_items", MAX_TRAIL_ITEMS as u64),
            ("max_page_items", MAX_SAVED_TRAIL_PAGE as u64), ("offset", start as u64), ("limit", limit as u64)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"readiness_scope\":\"selected-saved-archive-metadata\",\"payload_verified\":false,\"source_observation\":\"saved-manifest-metadata\",\"truncated\":false,\"retained_source_payload_bytes\":\"0\",\"member_bytes_read\":\"0\",\"member_read_calls\":\"0\",\"items\":[")?;
        let end = start.saturating_add(limit).min(view.len());
        for (item, entry) in view.entries().enumerate().skip(start).take(end - start) {
            check(canceled)?;
            if item != start { out.literal(",")?; }
            let entry = entry?;
            encode_entry(out, item, entry, readiness(entry, self.archive.directory())?)?;
        }
        out.literal("],\"next_offset\":")?;
        optional(out, (end < view.len()).then_some(end as u64))?;
        out.literal("}\n")?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Readiness { Ready(usize), OtherArchive, MissingMember, UnavailableMember, ChangedSource, InvalidRange }
impl Readiness {
    fn name(self) -> &'static str {
        match self {
            Self::Ready(_) => "ready", Self::OtherArchive => "other-archive",
            Self::MissingMember => "missing-member", Self::UnavailableMember => "unavailable-member",
            Self::ChangedSource => "changed-source", Self::InvalidRange => "invalid-range",
        }
    }
    fn member(self) -> Option<usize> { match self { Self::Ready(member) => Some(member), _ => None } }
}
fn readiness(entry: TrailEntry<'_>, directory: &fcb::search::paged_snapshot::SnapshotDirectory)
    -> Result<Readiness, SavedTrailError> {
    match resolve_selection(entry, directory) {
        Ok(member) => Ok(Readiness::Ready(member)),
        Err(TrailReadError::OtherArchive) => Ok(Readiness::OtherArchive),
        Err(TrailReadError::MissingMember) => Ok(Readiness::MissingMember),
        Err(TrailReadError::UnavailableMember) => Ok(Readiness::UnavailableMember),
        Err(TrailReadError::ChangedSource) => Ok(Readiness::ChangedSource),
        Err(TrailReadError::InvalidRange) => Ok(Readiness::InvalidRange),
        Err(error) => Err(error.into()),
    }
}
fn encode_entry(out: &mut Output, item: usize, entry: TrailEntry<'_>, readiness: Readiness)
    -> Result<(), SavedTrailError> {
    out.literal("{\"item\":")?; out.integer(item as u64)?;
    out.literal(",\"archive_digest\":")?; out.quoted(&entry.archive.to_hex())?;
    out.literal(",\"source_digest\":")?; out.quoted(&entry.source.to_hex())?;
    out.literal(",\"source_bytes\":")?; out.integer(entry.source_length)?;
    out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(entry.path).to_path_buf())?;
    out.literal(",\"original_range\":")?; out.range(entry.range)?;
    out.literal(",\"rationale\":")?; out.quoted(entry.rationale)?;
    out.literal(",\"evidence\":\"explicit-user-selection\",\"readiness\":")?; out.quoted(readiness.name())?;
    out.literal(",\"member\":")?; optional(out, readiness.member().map(|n| n as u64))?;
    out.literal(",\"file_id\":")?; optional(out, readiness.member().map(|n| n as u64 + 1))?;
    out.literal(",\"source_revision\":")?; optional(out, readiness.member().map(|n| n as u64 + 1))?;
    out.literal("}")?;
    Ok(())
}
fn trail_attempt(last: &mut u64, next: u64) -> Result<(), SavedTrailError> {
    if next == 0 || next <= *last { return Err(SavedTrailError::StaleGeneration); }
    *last = next;
    Ok(())
}

#![forbid(unsafe_code)]

//! One explicit supplemental archive can satisfy an otherwise unavailable
//! trail visit. The primary saved session and its attachment remain unchanged;
//! the prepared reader owns an independent copy of the verified member.

use std::path::Path;
use fcb::search::paged_snapshot::PagedMemberData;
use crate::trail::export::archives::{Archives, MAX_VALIDATION_BYTES};
use super::{ArenaOwnerId, AppError, FileId, HostResponse, QueryGeneration, RawPath,
    ReaderSession, Readiness, SavedRepositoryError, SavedRepositorySession, SavedTrailError,
    SavedTrailWriteParent, SourceRevision, TrailError, TrailNavigator, TrailReadError,
    TrailView, check, encode_entry, input, resolve_selection};

/// An explicit file grant satisfies one frozen trail selection only. The path
/// is borrowed through preparation and never retained as reader authority.
#[derive(Clone, Copy)]
pub struct SavedTrailArchiveRequest<'a> {
    pub trail: SavedTrailWriteParent,
    pub item: usize,
    pub archive: &'a Path,
    /// Full-envelope validation is independent of the selected member read.
    pub validation_bytes: u64,
}

impl SavedRepositorySession {
    /// Prepare an independent reader from one explicitly selected archive.
    /// Registry callers must admit an empty reader destination before invoking
    /// this method. Stale requests and already-primary selections are rejected
    /// before opening the supplied path. A wrong archive never substitutes a
    /// same-named member for the pinned archive/source/range.
    pub fn open_trail_reader_with_archive(&mut self, reader_owner: ArenaOwnerId,
        request: SavedTrailArchiveRequest<'_>, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), SavedTrailError> {
        check(&mut canceled)?;
        let owner = self.owner();
        let primary_digest = self.archive_digest();
        if reader_owner == owner { return Err(TrailReadError::OwnerMismatch.into()); }
        if request.trail.generation == 0 || !request.archive.is_absolute()
            || request.archive.as_os_str().is_empty() || request.archive.as_os_str().len() > 16_384
            || request.validation_bytes > MAX_VALIDATION_BYTES {
            return Err(SavedTrailError::InvalidLimits);
        }
        if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
        let [source_id, capture_id, archives_id, identity_id] = self.allocations()?;
        // Primary members use IDs 1..=primary.len(). Every supplemental
        // preparation gets a fresh identity outside that interval. Its separate
        // catalog ordinal remains local to the verified supplemental digest.
        let value = (self.archive.directory().len() as u64).checked_add(identity_id.get())
            .ok_or(SavedRepositoryError::IdentityExhausted)?;
        let file = FileId::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        let revision = SourceRevision::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        let mut out = self.output("trail-archive-open-reader")?;
        let trail = self.trail.as_ref().ok_or(SavedTrailError::MissingTrail)?;
        trail.validate(owner, primary_digest, request.trail.generation)?;
        if trail.digest != request.trail.digest { return Err(SavedTrailError::ParentMismatch); }
        let view = TrailView::open(&trail.bytes, &mut canceled)?;
        let local = QueryGeneration::new(owner, 1).map_err(|_| TrailReadError::GenerationExhausted)?;
        let mut navigator = TrailNavigator::new(view, local);
        navigator.select(request.item)?;
        let target = navigator.target()?.ok_or(TrailError::MissingItem)?;
        let entry = target.entry();
        if entry.archive == primary_digest { return Err(SavedTrailError::InvalidLimits); }

        let mut archives = Archives::new(Some(primary_digest), 1, request.validation_bytes);
        archives.open_paths(&[request.archive], owner, &self.budget, archives_id, &mut canceled)?;
        out.literal(",\"trail_generation\":")?; out.integer(request.trail.generation)?;
        out.literal(",\"trail_digest\":")?; out.quoted(&trail.digest.to_hex())?;
        out.literal(",\"item\":")?; out.integer(request.item as u64)?;
        // Keep the accepted page's original primary-relative readiness. The
        // resolved member below belongs only to source_archive, not the primary.
        out.literal(",\"entry\":")?;
        encode_entry(&mut out, request.item, entry, Readiness::OtherArchive)?;
        out.literal(",\"archive_validation\":")?; archives.encode_validation(&mut out)?;
        let archive = archives.for_digest(entry.archive).ok_or(TrailReadError::OtherArchive)?;
        let ordinal = resolve_selection(entry, archive.directory())?;
        let member = archive.directory().member(ordinal).ok_or(TrailReadError::MissingMember)?;
        let PagedMemberData::Captured { digest, .. } = member.data else {
            return Err(TrailReadError::UnavailableMember.into());
        };
        let label = RawPath::from_bytes(member.path).to_path_buf();
        out.literal(",\"source_archive\":{\"path\":")?; out.path(request.archive)?;
        out.literal(",\"digest\":")?; out.quoted(&archive.directory().digest().to_hex())?;
        out.literal(",\"members_total\":")?; out.integer(archive.directory().len() as u64)?;
        out.literal(",\"captured_members\":")?; out.integer(archive.directory().captured_files() as u64)?;
        out.literal(",\"discovery_complete\":")?; out.boolean(archive.directory().discovery_complete())?;
        out.literal("},\"resolved_member\":{\"member\":")?; out.integer(ordinal as u64)?;
        out.literal(",\"file_id\":")?; out.integer(ordinal as u64 + 1)?;
        out.literal(",\"path\":")?; out.path(&label)?;
        out.literal(",\"observed_bytes\":")?; out.integer(member.observed_bytes)?;
        out.literal(",\"captured\":true,\"source_digest\":")?; out.quoted(&digest.to_hex())?;
        out.literal("}")?;
        let before = archive.load_stats();
        let capture = target.open(archive, trail.digest, navigator.generation(), file,
            revision, &self.budget, [source_id, capture_id], &mut canceled)?;
        let after = archive.load_stats();
        let mut reader = ReaderSession::from_bytes(reader_owner, &label, capture.bytes(), &mut canceled)?;
        let info = reader.info(&mut canceled)?;
        let offset = entry.range.start().get();
        let window = reader.read_window(offset.saturating_sub(132), 16 * 1024, &mut canceled)?;
        out.literal(",\"member\":")?; out.integer(ordinal as u64)?;
        out.literal(",\"file_id\":")?; out.integer(file.get())?;
        out.literal(",\"source_revision\":")?; out.integer(revision.get())?;
        out.literal(",\"source_digest\":")?; out.quoted(&entry.source.to_hex())?;
        out.literal(",\"path\":")?; out.path(&label)?;
        out.literal(",\"source_bytes\":")?; out.integer(entry.source_length)?;
        out.literal(",\"reader_owner\":")?; out.integer(reader_owner.get())?;
        out.literal(",\"source_observation\":\"verified-saved-member\",\"archive_scope\":\"explicit-supplemental-archive\"")?;
        out.literal(",\"archive_path_reopened\":false,\"live_source_reopened\":false,\"accepted_trail_unchanged\":true,\"accepted_query_unchanged\":true,\"offset\":")?;
        out.integer(offset)?;
        out.literal(",\"selection\":")?; out.range(entry.range)?;
        out.literal(",\"member_bytes_read\":")?; out.integer(after.bytes_read - before.bytes_read)?;
        out.literal(",\"member_read_calls\":")?; out.integer(after.read_calls - before.read_calls)?;
        out.literal(",\"primary_member_bytes_read\":\"0\",\"primary_member_read_calls\":\"0\",\"reader\":")?;
        out.literal(info.as_str().trim_end())?;
        out.literal(",\"window\":")?; out.literal(window.as_str().trim_end())?;
        out.literal("}\n")?;
        drop(capture);
        let response = self.finish(out, false, &mut canceled)?;
        Ok((reader, response))
    }
}

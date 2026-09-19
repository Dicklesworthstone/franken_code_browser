#![forbid(unsafe_code)]

//! Metadata-only file finding and explicit path-result capture into the desk.
//! The existing PathIndex owns matching/ranking/native keys. Path selection is
//! NOT a source observation; open_path_hit captures current bytes separately.
//! This intentionally differs from open_hit, which keeps searched OLD bytes.

use super::*;
use crate::host::atlas_paths::AtlasPathOptions;
use crate::output::Output;

/// Successful source admission, before response delivery. Catalog file identity
/// and receiving-desk identity are distinct; path results carry no old revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepositoryPathOpen {
    pub change: DeskChange,
    pub path_generation: u64,
    pub catalog_file: FileId,
}

impl DeskRepository {
    pub fn path_generation(&self) -> Option<u64> { self.paths.accepted_generation() }
    pub fn find_paths(&mut self, generation: u64, needle: &[u8], options: AtlasPathOptions,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.paths.find(&self.atlas, generation, needle, options, canceled)?)
    }
    pub fn path_page(&mut self, generation: u64, start: usize, count: usize,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.paths.page(&self.atlas, generation, start, count, canceled)?)
    }
    pub fn select_path(&mut self, generation: u64, file: FileId,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.paths.select(&self.atlas, generation, file, canceled)?)
    }
    pub fn clear_paths(&mut self, generation: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.paths.clear(&self.atlas, generation, canceled)?)
    }

    /// File IDs must be returned by the current path query (or its protected
    /// selected match). No displayed label/caller path is used as authority.
    /// Reuses the SAME regular-file opener and symlink policy as the app. This
    /// pathname-based policy does NOT qualify hostile ancestor-race confinement.
    /// The root grant participates in cancellation through final desk acceptance.
    /// Each explicit open admits a NEW capture, capped by the receiving desk's
    /// source-byte limit; earlier pinned/history/bookmarked bytes stay unchanged.
    pub fn open_path_hit(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        generation: u64, file: FileId, mut canceled: impl FnMut() -> bool)
        -> Result<RepositoryPathOpen, DeskRepositoryError> {
        desk.validate_mutation(expected, attempt)?;
        check(&mut canceled)?;
        let owner = desk.model().owner();
        if owner == self.owner() || self.desk_owner.is_some_and(|old| old != owner) {
            return Err(DeskRepositoryError::WrongDesk);
        }
        self.paths.hit(&self.atlas, generation, file)?;
        let catalog = self.atlas.atlas().catalog();
        let entry = catalog.entry(file).ok_or(AtlasPathError::MissingHit)?;
        let allocation = desk.next_id()?;
        let _path_lease = desk.budget.try_reserve_managed(owner, allocation,
            ByteLength::new((2 * catalog.grant().root_path().len() + 256 * 1024) as u64))
            .map_err(|_| AppError::Admission)?;
        let root = catalog.grant().root_path().to_path_buf();
        let path = crate::workspace::checked_source_path(&root, entry.path())
            .map_err(|_| AppError::SourceChanged)?;
        let change = desk.open_file(expected, attempt, &path,
            &mut || canceled() || self.atlas.validate_active().is_err())?;
        // No fallible work after source/pane acceptance. A response/transport
        // failure cannot turn a committed navigation into an unaccepted attempt.
        self.desk_owner = Some(owner);
        Ok(RepositoryPathOpen { change, path_generation: generation, catalog_file: file })
    }
}

impl DeskSession {
    pub fn repository_path_open_response(&mut self, opened: &RepositoryPathOpen)
        -> Result<HostResponse, DeskSessionError> {
        if opened.change.revision != self.model().revision() { return Err(DeskError::StaleRevision.into()); }
        let pane = opened.change.active.ok_or(DeskError::MissingPane)?;
        let source = self.model().source(pane, opened.change.revision)?;
        let (file, revision, bytes) = (source.file(), source.revision(), source.bytes().len() as u64);
        let id = self.next_id()?;
        let mut out = Output::new(self.model().owner(), 16 * 1024, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.reading-desk/1\",\"status\":\"ok\",\"command\":\"repo-path-open\",\"owner\":")?;
        out.integer(self.model().owner().get())?;
        out.literal(",\"model_revision\":")?; out.integer(self.model().revision())?;
        out.literal(",\"last_attempt\":")?; out.integer(self.model().last_attempt())?;
        out.literal(",\"repository_owner\":")?; out.integer(opened.catalog_file.owner().get())?;
        out.literal(",\"path_query_generation\":")?; out.integer(opened.path_generation)?;
        out.literal(",\"catalog_file_id\":")?; out.integer(opened.catalog_file.get())?;
        out.literal(",\"pane\":")?; out.integer(pane.get())?;
        out.literal(",\"file_id\":")?; out.integer(file.get())?;
        out.literal(",\"source_revision\":")?; out.integer(revision.get())?;
        out.literal(",\"source_bytes_read\":")?; out.integer(bytes)?;
        out.literal(",\"source_observation\":\"new-capture-after-path-selection\",\"native_presented\":false}\n")?;
        self.finish(out, EXIT_OK, &mut || false)
    }
}

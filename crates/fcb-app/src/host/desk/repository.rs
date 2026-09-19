#![forbid(unsafe_code)]

//! Repository-wide discovery/search composed with the source-backed reading desk.
//!
//! Discovery and queries use the existing atlas/catalog/capture/search engines.
//! Activation uses the shared desk import API under the receiving desk's budget;
//! it NEVER opens a path. Repeated hits share their imported immutable capture.
//! Bookmarks/history/pins own their sources independently of this repository,
//! its next query, or its eventual destruction. All work is worker
//! work, not an input/paint callback. No root authority is saved in a checkpoint.

mod work;
pub use work::RepositoryWorkState;

use std::{mem::size_of, path::Path};
use super::{DeskSession, DeskSessionError, DeskChange, DeskError,
    ArenaOwnerId, FileId, SourceRevision, ByteRange,
    ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease, HostResponse};
use crate::{AppError, EXIT_OK};
use crate::host::atlas_session::{AtlasSession, AtlasSessionError, AtlasSessionOptions};
use crate::host::atlas_search::{RetainedAtlasSearch, AtlasSearchError, AtlasSearchOptions, AtlasDeskError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeskRepositoryError {
    Desk(DeskSessionError), Atlas(AtlasSessionError), Search(AtlasSearchError),
    WrongDesk, StaleStep, Canceled,
}
impl std::fmt::Display for DeskRepositoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Desk(e) => write!(f, "{e}"), Self::Atlas(e) => write!(f, "{e}"),
            Self::Search(e) => write!(f, "{e}"),
            Self::WrongDesk => f.write_str("DESK_REPOSITORY_WRONG_DESK"),
            Self::StaleStep => f.write_str("DESK_REPOSITORY_STALE_STEP"),
            Self::Canceled => f.write_str("DESK_REPOSITORY_CANCELED"),
        }
    }
}
impl std::error::Error for DeskRepositoryError {}
impl From<DeskSessionError> for DeskRepositoryError { fn from(e: DeskSessionError) -> Self { Self::Desk(e) } }
impl From<DeskError> for DeskRepositoryError { fn from(e: DeskError) -> Self { Self::Desk(e.into()) } }
impl From<AppError> for DeskRepositoryError { fn from(e: AppError) -> Self { Self::Desk(e.into()) } }
impl From<AtlasSessionError> for DeskRepositoryError { fn from(e: AtlasSessionError) -> Self { Self::Atlas(e) } }
impl From<AtlasSearchError> for DeskRepositoryError { fn from(e: AtlasSearchError) -> Self { Self::Search(e) } }
impl From<AtlasDeskError> for DeskRepositoryError {
    fn from(e: AtlasDeskError) -> Self {
        match e { AtlasDeskError::Search(e) => Self::Search(e), AtlasDeskError::Desk(e) => Self::Desk(e) }
    }
}
impl DeskRepositoryError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Canceled => true, Self::Desk(e) => e.is_canceled(),
            // The wrapped engines expose stable error codes, including nested
            // capture/workspace cancellation. Do not collapse those diagnostics.
            other => other.to_string().ends_with("CANCELED"),
        }
    }
}

/// Typed acceptance precedes response delivery. A response failure cannot undo
/// an accepted reader. Original search identities are linked, not relabeled as
/// the new owner domain. `reused_source` refers to backing already in the desk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepositoryOpen {
    pub change: DeskChange,
    pub query_generation: u64,
    pub hit_id: u64,
    pub source_file: FileId,
    pub source_revision: SourceRevision,
    pub desk_file: FileId,
    pub desk_revision: SourceRevision,
    pub original_range: ByteRange,
    pub reused_source: bool,
}

/// One explicit live repository and at most one receiving desk owner. Hosts
/// supply a fresh owner distinct from every desk/repository they already own.
/// Source import bindings belong to the desk, not this repository. Closing this
/// object cannot discard source bytes retained by the receiving desk.
pub struct DeskRepository {
    atlas: AtlasSession,
    search: RetainedAtlasSearch,
    desk_owner: Option<ArenaOwnerId>,
    _lease: ResourceLease,
}
impl DeskRepository {
    pub fn open(owner: ArenaOwnerId, root: &Path, options: AtlasSessionOptions,
        mut canceled: impl FnMut() -> bool) -> Result<Self, DeskRepositoryError> {
        check(&mut canceled)?;
        let budget = ResourceBudget::new(owner, ByteLength::new((size_of::<Self>() + 4096) as u64))
            .map_err(|_| AppError::Admission)?;
        let lease = budget.try_reserve_managed(owner,
            ResourceAllocationId::new(1).map_err(|_| AppError::Admission)?,
            ByteLength::new(size_of::<Self>() as u64)).map_err(|_| AppError::Admission)?;
        let atlas = AtlasSession::open(owner, root, options, &mut canceled)?;
        let search = RetainedAtlasSearch::new(&atlas)?;
        check(&mut canceled)?;
        Ok(Self { atlas, search, desk_owner: None, _lease: lease })
    }
    pub fn owner(&self) -> ArenaOwnerId { self.atlas.atlas().catalog().id().owner() }
    pub fn accepted_generation(&self) -> Option<u64> { self.search.accepted_generation() }
    pub fn info(&mut self, canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.atlas.info(canceled)?)
    }
    pub fn search(&mut self, generation: u64, needle: &str, options: AtlasSearchOptions,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.search.search(&self.atlas, generation, needle, options, canceled)?)
    }
    pub fn page(&mut self, generation: u64, start: usize, limit: usize,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.search.page(&self.atlas, generation, start, limit, canceled)?)
    }
    pub fn clear(&mut self, generation: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.search.clear(&self.atlas, generation, canceled)?)
    }

    /// Open the accepted occurrence into the normal desk navigation/history
    /// transaction. Failed or canceled transfers preserve every accepted pane,
    /// bookmark and local query. Repeated hits in one captured file reuse its
    /// desk identity, rather than exhausting the source limit one click at a time.
    pub fn open_hit(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        generation: u64, id: u64, mut canceled: impl FnMut() -> bool)
        -> Result<RepositoryOpen, DeskRepositoryError> {
        desk.validate_mutation(expected, attempt)?;
        check(&mut canceled)?;
        let owner = desk.model().owner();
        if owner == self.owner() || self.desk_owner.is_some_and(|old| old != owner) {
            return Err(DeskRepositoryError::WrongDesk);
        }
        let hit = self.search.hit(&self.atlas, generation, id)?;
        let imported = self.search.open_desk(&self.atlas, desk, expected, attempt,
            generation, id, &mut canceled)?;
        // No fallible work after acceptance. The shared import route owns
        // identity translation, exact-byte reuse and final grant validation.
        self.desk_owner = Some(owner);
        Ok(RepositoryOpen { change: imported.change, query_generation: generation, hit_id: id,
            source_file: imported.origin.file, source_revision: imported.origin.revision,
            desk_file: imported.file, desk_revision: imported.revision,
            original_range: hit.original_range, reused_source: imported.reused_capture })
    }
}
impl DeskSession {
    /// Encode an already accepted transfer without a cancellation gate. Recover
    /// state by revision after delivery failure; do not replay the same attempt.
    pub fn repository_open_response(&mut self, opened: &RepositoryOpen) -> Result<HostResponse, DeskSessionError> {
        if opened.change.revision != self.model().revision() || opened.desk_file.owner() != self.model().owner() {
            return Err(DeskError::StaleRevision.into());
        }
        let mut out = self.output("repo-hit")?;
        out.literal(",\"source_observation\":\"retained-repository-search\",\"source_reopened\":false,\"repository_owner\":")?;
        out.integer(opened.source_file.owner().get())?;
        out.literal(",\"repository_query_generation\":")?; out.integer(opened.query_generation)?;
        out.literal(",\"hit_id\":")?; out.integer(opened.hit_id)?;
        out.literal(",\"origin_file_id\":")?; out.integer(opened.source_file.get())?;
        out.literal(",\"origin_source_revision\":")?; out.integer(opened.source_revision.get())?;
        out.literal(",\"file_id\":")?; out.integer(opened.desk_file.get())?;
        out.literal(",\"source_revision\":")?; out.integer(opened.desk_revision.get())?;
        out.literal(",\"pane\":")?;
        match opened.change.active { Some(p) => out.integer(p.get())?, None => out.literal("null")? }
        out.literal(",\"original_range\":")?; out.range(opened.original_range)?;
        out.literal(",\"reused_source\":")?; out.boolean(opened.reused_source)?;
        out.literal("}\n")?;
        self.finish(out, EXIT_OK, &mut || false)
    }
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), DeskRepositoryError> {
    if canceled() { Err(DeskRepositoryError::Canceled) } else { Ok(()) }
}

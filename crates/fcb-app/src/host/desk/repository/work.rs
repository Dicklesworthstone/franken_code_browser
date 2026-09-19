#![forbid(unsafe_code)]

//! Resumable repository search and reusable captured-source indexes for desks.
//! Each step uses the existing engine's one-file worker slice. It is NOT a
//! fixed-duration UI callback. Progress/page operations never advance work.
//! Generation + expected step protect retries from accidentally scanning again.
//! A canceled replacement preserves accepted results/indexes and imported panes.

use super::*;
use crate::host::atlas_search::{AtlasIndexOptions, AtlasIndexBuildProgress, AtlasSearchProgress};

/// Query and index preparation are distinct lifecycles. All new work uses the
/// engine's common strictly increasing generation sequence. A build may coexist
/// with a query on the old index; publication invalidates only an old-index
/// pending query, not accepted rows or captures already imported into a desk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepositoryWorkState {
    pub accepted_query: Option<u64>,
    pub pending_query: Option<u64>,
    pub accepted_index: Option<u64>,
    pub pending_index: Option<u64>,
}
impl RepositoryWorkState {
    pub const fn has_pending(self) -> bool { self.pending_query.is_some() || self.pending_index.is_some() }
}

impl DeskRepository {
    /// Inert identity inspection, including after a failed operation. This does
    /// not validate a root grant or establish coverage; operational APIs do both.
    pub fn work_state(&self) -> RepositoryWorkState {
        RepositoryWorkState { accepted_query: self.search.accepted_generation(),
            pending_query: self.search.pending_generation(), accepted_index: self.search.index_generation(),
            pending_index: self.search.pending_index_generation() }
    }
    pub fn query_progress(&self, generation: u64) -> Result<AtlasSearchProgress, DeskRepositoryError> {
        Ok(self.search.progress(&self.atlas, generation)?)
    }
    pub fn begin_query(&mut self, generation: u64, needle: &str, options: AtlasSearchOptions,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.search.begin(&self.atlas, generation, needle, options, canceled)?)
    }
    /// Advance one file only when the caller observed the current step count.
    /// A delayed duplicate fails without retiring/advancing the current query.
    /// An already terminal generation is inspectable even after another work
    /// generation has started, and terminal inspection performs no verification.
    pub fn step_query(&mut self, generation: u64, expected_step: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskRepositoryError> {
        check(&mut canceled)?;
        let progress = self.query_progress(generation)?;
        if expected_step != progress.step_count { return Err(DeskRepositoryError::StaleStep); }
        if !progress.is_running() { return self.page(generation, 0, 64, canceled); }
        Ok(self.search.step(&self.atlas, generation, canceled)?)
    }
    /// Explicit user cancellation is not a failed search result or an empty
    /// replacement. Require the exact pending generation so stale Cancel events
    /// cannot discard a newer request. No fallible work follows cancellation.
    pub fn cancel_query(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<RepositoryWorkState, DeskRepositoryError> {
        self.atlas.validate_active()?; check(&mut canceled)?;
        if self.search.pending_generation() != Some(generation) { return Err(AtlasSearchError::StaleQuery.into()); }
        self.search.cancel_pending();
        Ok(self.work_state())
    }
    /// Explicitly prepare captured membership and trigram segments, not live
    /// refresh or a persisted index. Queries against it use the indexed capture
    /// generation even if the working tree later changes or disappears.
    pub fn begin_index(&mut self, generation: u64, options: AtlasIndexOptions,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.search.begin_index(&self.atlas, generation, options, canceled)?)
    }
    pub fn index_progress(&self, generation: u64) -> Result<AtlasIndexBuildProgress, DeskRepositoryError> {
        Ok(self.search.index_build_progress(&self.atlas, generation)?)
    }
    pub fn index_info(&mut self, generation: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.search.index_build_info(&self.atlas, generation, canceled)?)
    }
    pub fn step_index(&mut self, generation: u64, expected_step: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskRepositoryError> {
        check(&mut canceled)?;
        if self.index_progress(generation)?.steps != expected_step { return Err(DeskRepositoryError::StaleStep); }
        Ok(self.search.step_index(&self.atlas, generation, canceled)?)
    }
    pub fn cancel_index(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<RepositoryWorkState, DeskRepositoryError> {
        self.atlas.validate_active()?; check(&mut canceled)?;
        if self.search.pending_index_generation() != Some(generation) { return Err(AtlasSearchError::StaleIndex.into()); }
        self.search.cancel_index_build();
        Ok(self.work_state())
    }
    pub fn clear_index(&mut self, generation: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.search.clear_index(&self.atlas, generation, canceled)?)
    }
    pub fn begin_indexed_query(&mut self, generation: u64, index_generation: u64, needle: &str,
        max_matches: usize, max_scan_bytes: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.search.begin_indexed(&self.atlas, generation, index_generation, needle,
            max_matches, max_scan_bytes, canceled)?)
    }
    pub fn search_indexed(&mut self, generation: u64, index_generation: u64, needle: &str,
        max_matches: usize, max_scan_bytes: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskRepositoryError> {
        Ok(self.search.search_indexed(&self.atlas, generation, index_generation, needle,
            max_matches, max_scan_bytes, canceled)?)
    }
}

impl DeskSession {
    /// Bounded reconciliation response. The repository owner identifies these
    /// generations; the desk revision identifies panes. Neither one grants new
    /// source access. Cancellation receipts can call this with an uncanceled
    /// encoder after the typed cancellation was already accepted.
    pub fn repository_work_response(&mut self, repo: &DeskRepository, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskRepositoryError> {
        repo.atlas.validate_active()?; check(&mut canceled)?;
        let state = repo.work_state();
        let mut out = self.output("repo-work")?;
        out.literal(",\"repository_owner\":").map_err(DeskSessionError::from)?;
        out.integer(repo.owner().get()).map_err(DeskSessionError::from)?;
        for (key, value) in [("repository_query_generation", state.accepted_query),
            ("repository_pending_query_generation", state.pending_query),
            ("repository_index_generation", state.accepted_index),
            ("repository_pending_index_generation", state.pending_index)] {
            out.literal(",").map_err(DeskSessionError::from)?;
            out.quoted(key).map_err(DeskSessionError::from)?;
            out.literal(":").map_err(DeskSessionError::from)?;
            super::super::optional(&mut out, value).map_err(DeskSessionError::from)?;
        }
        out.literal(",\"work_pending\":").map_err(DeskSessionError::from)?;
        out.boolean(state.has_pending()).map_err(DeskSessionError::from)?;
        out.literal(",\"source_reopened\":false,\"index_persistence\":\"memory-only\"}\n")
            .map_err(DeskSessionError::from)?;
        Ok(self.finish(out, EXIT_OK, &mut canceled)?)
    }
}

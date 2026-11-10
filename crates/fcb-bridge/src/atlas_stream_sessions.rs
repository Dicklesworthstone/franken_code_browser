#![forbid(unsafe_code)]

//! Streaming search dispatch over the existing atlas cell. The application
//! service owns matching, retained witnesses and query identities. No borrowed
//! job survives a call and no registry lock is held during source work.

use fcb_app::host::atlas_stream_search::AtlasStreamSearchOptions;
use super::*;

pub(crate) enum StreamCommand<'a> {
    Search { needle: &'a str, options: AtlasStreamSearchOptions },
    Page { generation: u64, start: usize, count: usize },
    Hit { generation: u64, position: usize },
    Clear,
}

impl AtlasSessions {
    /// Synchronous progress holds this session's permit until the entered call
    /// drains. Other sessions and nonblocking cancellation/close remain usable.
    pub(crate) fn execute_stream_with_progress(&self, handle: u64, needle: &str,
        options: AtlasStreamSearchOptions, mut canceled: impl FnMut() -> bool,
        stopped: impl FnMut() -> bool, mut progress: impl FnMut(&HostResponse))
        -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let result = session.stream.search_with_progress(&session.atlas, needle, options,
            || cell.validate(epoch).is_err() || canceled(), stopped, |response| {
                if cell.validate(epoch).is_ok() { progress(response); }
            });
        if let Err(error) = cell.validate(epoch) {
            session.search.cancel_pending();
            session.search.cancel_index_build();
            return Err(error);
        }
        if canceled() { return Err(AccessError::Canceled); }
        result.map_err(AccessError::from)
    }

    pub(crate) fn execute_stream(&self, handle: u64, command: StreamCommand<'_>,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = match command {
            StreamCommand::Search { needle, options } =>
                session.stream.search(&session.atlas, needle, options, &mut stop),
            StreamCommand::Page { generation, start, count } =>
                session.stream.page(&session.atlas, generation, start, count),
            StreamCommand::Hit { generation, position } =>
                session.stream.hit(&session.atlas, generation, position),
            StreamCommand::Clear => session.stream.clear(&session.atlas),
        };
        // A late cancel/close suppresses delivery. It cannot undo a completed
        // replacement; callers must reconcile or revoke old result actions
        // after an uncertain reply. The cell permit outlives the entered work.
        if let Err(error) = cell.validate(epoch) {
            session.search.cancel_pending();
            session.search.cancel_index_build();
            return Err(error);
        }
        if canceled() { return Err(AccessError::Canceled); }
        result.map_err(AccessError::from)
    }
}

#[cfg(all(test, unix))]
#[path = "atlas_stream_sessions_tests.rs"]
mod tests;

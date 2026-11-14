#![forbid(unsafe_code)]

//! Streaming search dispatch over the existing atlas cell. The application
//! service owns matching, retained witnesses and query identities. No borrowed
//! job survives a call and no registry lock is held during source work.

use fcb_app::host::atlas_stream_search::AtlasStreamSearchOptions;
use super::*;

pub(crate) enum StreamCommand<'a> {
    Search { needle: &'a str, options: AtlasStreamSearchOptions },
    SearchScoped { needle: &'a str, options: AtlasStreamSearchOptions, scope: &'a AtlasSearchScope },
    Page { generation: u64, start: usize, count: usize },
    Hit { generation: u64, position: usize },
    Clear,
}

impl AtlasSessions {
    /// Synchronous progress holds this session's permit until the entered call
    /// drains. Other sessions and nonblocking cancellation/close remain usable.
    pub(crate) fn execute_stream_with_progress(&self, handle: u64, needle: &str,
        options: AtlasStreamSearchOptions, canceled: impl FnMut() -> bool,
        stopped: impl FnMut() -> bool, progress: impl FnMut(&HostResponse))
        -> Result<HostResponse, AccessError> {
        self.execute_stream_scoped_with_progress(handle, needle, options, &AtlasSearchScope::all(),
            canceled, stopped, progress)
    }

    pub(crate) fn execute_stream_scoped_with_progress(&self, handle: u64, needle: &str,
        options: AtlasStreamSearchOptions, scope: &AtlasSearchScope, canceled: impl FnMut() -> bool,
        stopped: impl FnMut() -> bool, progress: impl FnMut(&HostResponse))
        -> Result<HostResponse, AccessError> {
        self.execute_stream_progress_inner(handle, needle, options, scope, canceled, stopped, progress, false, false, 0)
    }

    pub(crate) fn execute_stream_scoped_counted_with_progress(&self, handle: u64, needle: &str,
        options: AtlasStreamSearchOptions, scope: &AtlasSearchScope, canceled: impl FnMut() -> bool,
        stopped: impl FnMut() -> bool, progress: impl FnMut(&HostResponse))
        -> Result<HostResponse, AccessError> {
        self.execute_stream_progress_inner(handle, needle, options, scope, canceled, stopped, progress, true, false, 0)
    }

    /// The scope carries the immutable case policy. The host chooses the shared
    /// exact/lowercase engine; this layer only preserves lifecycle and counting.
    pub(crate) fn execute_stream_scoped_matching_with_progress(&self, handle: u64, needle: &str,
        options: AtlasStreamSearchOptions, scope: &AtlasSearchScope, count_all_matches: bool,
        canceled: impl FnMut() -> bool, stopped: impl FnMut() -> bool,
        progress: impl FnMut(&HostResponse)) -> Result<HostResponse, AccessError> {
        self.execute_stream_progress_inner(handle, needle, options, scope, canceled, stopped, progress, count_all_matches, false, 0)
    }

    pub(crate) fn execute_stream_scoped_occurrences_with_progress(&self, handle: u64, needle: &str,
        options: AtlasStreamSearchOptions, scope: &AtlasSearchScope, count_all_matches: bool,
        canceled: impl FnMut() -> bool, stopped: impl FnMut() -> bool,
        progress: impl FnMut(&HostResponse)) -> Result<HostResponse, AccessError> {
        self.execute_stream_progress_inner(handle, needle, options, scope, canceled, stopped, progress, count_all_matches, true, 0)
    }

    pub(crate) fn execute_stream_scoped_page_with_progress(&self, handle: u64, needle: &str,
        options: AtlasStreamSearchOptions, scope: &AtlasSearchScope, start_after: u64,
        count_all_matches: bool, occurrences: bool, canceled: impl FnMut() -> bool,
        stopped: impl FnMut() -> bool, progress: impl FnMut(&HostResponse))
        -> Result<HostResponse, AccessError> {
        self.execute_stream_progress_inner(handle, needle, options, scope, canceled, stopped, progress,
            count_all_matches, occurrences, start_after)
    }

    fn execute_stream_progress_inner(&self, handle: u64, needle: &str,
        options: AtlasStreamSearchOptions, scope: &AtlasSearchScope, mut canceled: impl FnMut() -> bool,
        stopped: impl FnMut() -> bool, mut progress: impl FnMut(&HostResponse), count_all_matches: bool, occurrences: bool,
        start_after: u64)
        -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let result = {
            let mut guarded_canceled = || cell.validate(epoch).is_err() || canceled();
            let mut guarded_progress = |response: &HostResponse| {
                if cell.validate(epoch).is_ok() { progress(response); }
            };
            session.stream.search_scoped_page_with_progress(&session.atlas, needle, options, scope.clone(),
                start_after, count_all_matches, occurrences, &mut guarded_canceled, stopped, &mut guarded_progress)
        };
        if let Err(error) = cell.validate(epoch) {
            session.cancel_work();
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
            StreamCommand::SearchScoped { needle, options, scope } =>
                session.stream.search_scoped(&session.atlas, needle, options, scope.clone(), &mut stop),
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
            session.cancel_work();
            return Err(error);
        }
        if canceled() { return Err(AccessError::Canceled); }
        result.map_err(AccessError::from)
    }
}

#[cfg(all(test, unix))]
#[path = "atlas_stream_sessions_tests.rs"]
mod tests;

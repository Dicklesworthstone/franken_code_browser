#![forbid(unsafe_code)]

//! Dispatch only. Index preparation and queries share the registered atlas,
//! cancellation epoch, resource owner and captured-reader activation route.
use fcb_app::host::atlas_search::AtlasIndexOptions;
use super::*;

pub(crate) enum IndexCommand<'a> {
    Prepare { generation: u64, options: AtlasIndexOptions },
    PrepareBegin { generation: u64, options: AtlasIndexOptions },
    PrepareStep { generation: u64 },
    PrepareProgress { generation: u64 },
    Info,
    Query { generation: u64, index_generation: u64, needle: &'a str, max_matches: usize, max_scan_bytes: u64 },
    Begin { generation: u64, index_generation: u64, needle: &'a str, max_matches: usize, max_scan_bytes: u64 },
    Clear { generation: u64 },
}
impl AtlasSessions {
    pub(crate) fn execute_index(&self, handle: u64, command: IndexCommand<'_>,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = match command {
            IndexCommand::Prepare { generation, options } => session.search.prepare_index(&session.atlas, generation, options, &mut stop),
            IndexCommand::PrepareBegin { generation, options } => session.search.begin_index(&session.atlas, generation, options, &mut stop),
            IndexCommand::PrepareStep { generation } => session.search.step_index(&session.atlas, generation, &mut stop),
            IndexCommand::PrepareProgress { generation } => session.search.index_build_info(&session.atlas, generation, &mut stop),
            IndexCommand::Info => session.search.index_info(&session.atlas, &mut stop),
            IndexCommand::Query { generation, index_generation, needle, max_matches, max_scan_bytes } =>
                session.search.search_indexed(&session.atlas, generation, index_generation, needle, max_matches, max_scan_bytes, &mut stop),
            IndexCommand::Begin { generation, index_generation, needle, max_matches, max_scan_bytes } =>
                session.search.begin_indexed(&session.atlas, generation, index_generation, needle, max_matches, max_scan_bytes, &mut stop),
            IndexCommand::Clear { generation } => session.search.clear_index(&session.atlas, generation, &mut stop),
        };
        // Epoch cancellation invalidates both paused work kinds. A terminal
        // acceptance cannot be rolled back merely because its reply was canceled.
        if let Err(error) = cell.validate(epoch) {
            session.search.cancel_pending();
            session.search.cancel_index_build();
            return Err(error);
        }
        result.map_err(AccessError::from)
    }

    /// The same destination admission and atlas -> reader lock order as search
    /// activation. Index/file/revision identify SOURCE, not a reusable query row.
    pub(crate) fn open_index_reader(&self, handle: u64, readers: &ReaderSessions,
        reader_handle: u64, index_generation: u64, file: u64, revision: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let owner = ArenaOwnerId::new(handle).map_err(|_| AccessError::InvalidArgument)?;
        let file = fcb_core::FileId::new(owner, file).map_err(|_| AccessError::InvalidArgument)?;
        let revision = fcb_core::SourceRevision::new(owner, revision).map_err(|_| AccessError::InvalidArgument)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = readers.initialize_prepared(reader_handle,
            |reader_owner, reader_stop| session.search.open_index_reader(&session.atlas,
                reader_owner, index_generation, file, revision, reader_stop).map_err(AccessError::from),
            &mut stop);
        if let Err(error) = cell.validate(epoch) {
            session.search.cancel_pending();
            session.search.cancel_index_build();
            return Err(error);
        }
        result
    }
}

#[cfg(all(test, unix))]
#[path = "atlas_index_sessions_tests.rs"]
mod tests;
#[cfg(all(test, unix))]
#[path = "atlas_index_progressive_tests.rs"]
mod progressive_tests;
#[cfg(all(test, unix))]
#[path = "atlas_index_build_sessions_tests.rs"]
mod build_tests;

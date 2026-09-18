#![forbid(unsafe_code)]

//! File-finder operations share existing atlas/reader ownership and locks.
//! Separate query generations/results cannot overwrite a paused content search.
use super::*;
use fcb_core::FileId;
use fcb_app::host::atlas_paths::AtlasPathOptions;

pub(crate) enum PathCommand<'a> {
    Find { generation: u64, needle: &'a [u8], options: AtlasPathOptions },
    Page { generation: u64, start: usize, limit: usize },
    Select { generation: u64, file: u64 },
    Focus { generation: u64, file: u64, plan_generation: u64 },
    Clear { generation: u64 },
}
impl AtlasSessions {
    pub(crate) fn execute_paths(&self, handle: u64, command: PathCommand<'_>,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = match command {
            PathCommand::Find { generation, needle, options } => session.paths.find(&session.atlas, generation, needle, options, &mut stop),
            PathCommand::Page { generation, start, limit } => session.paths.page(&session.atlas, generation, start, limit, &mut stop),
            PathCommand::Select { generation, file } => session.paths.select(&session.atlas, generation, file_id(handle, file)?, &mut stop),
            PathCommand::Focus { generation, file, plan_generation } => session.paths.focus_hit(&mut session.atlas, generation, file_id(handle, file)?, plan_generation, &mut stop),
            PathCommand::Clear { generation } => session.paths.clear(&session.atlas, generation, &mut stop),
        };
        if let Err(error) = cell.validate(epoch) {
            session.search.cancel_pending();
            return Err(error);
        }
        result.map_err(AccessError::from)
    }
    /// Admit the existing EMPTY destination before path lookup/open. The caller
    /// names a published file identity, never an arbitrary path or display label.
    /// Lock order is atlas then reader; both are nonblocking, as for other routes.
    pub(crate) fn open_path_reader(&self, handle: u64, readers: &ReaderSessions,
        reader_handle: u64, generation: u64, file: u64, max_bytes: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let file = file_id(handle, file)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = readers.initialize_prepared(reader_handle,
            |owner, reader_stop| session.paths.open_reader(&session.atlas, owner, generation, file, max_bytes, reader_stop).map_err(AccessError::from),
            &mut stop);
        // Once installed, a late cancellation suppresses the reply but cannot
        // roll back the independently owned reader. Inspect/close that handle.
        if let Err(error) = cell.validate(epoch) {
            session.search.cancel_pending();
            return Err(error);
        }
        result
    }
}
fn file_id(handle: u64, file: u64) -> Result<FileId, AccessError> {
    let owner = ArenaOwnerId::new(handle).map_err(|_| AccessError::InvalidArgument)?;
    FileId::new(owner, file).map_err(|_| AccessError::InvalidArgument)
}

#[cfg(all(test, unix))]
#[path = "atlas_path_sessions_tests.rs"]
mod tests;

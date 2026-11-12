#![forbid(unsafe_code)]

//! Lexical import inventories share the atlas worker, frozen captured index,
//! and cancellation epoch. They own no filesystem or language implementation.
use fcb_app::host::atlas_search::{AtlasImportDirection, AtlasImportSide};
use super::*;

pub(crate) enum ImportsCommand<'a> {
    Begin { generation: u64, index_generation: u64, max_imports: usize, max_parse_bytes: u64 },
    Step { generation: u64 },
    Page { generation: u64, direction: AtlasImportDirection,
        relative_path: Option<&'a [u8]>, start: usize, limit: usize },
}

impl AtlasSessions {
    pub(crate) fn execute_imports(&self, handle: u64, command: ImportsCommand<'_>,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = match command {
            ImportsCommand::Begin { generation, index_generation, max_imports, max_parse_bytes } =>
                session.search.begin_imports(&session.atlas, generation, index_generation,
                    max_imports, max_parse_bytes, &mut stop),
            ImportsCommand::Step { generation } =>
                session.search.step_imports(&session.atlas, generation, &mut stop),
            ImportsCommand::Page { generation, direction, relative_path, start, limit } =>
                session.search.imports_page(&session.atlas, generation, direction,
                    relative_path, start, limit, &mut stop),
        };
        if let Err(error) = cell.validate(epoch) {
            session.cancel_work();
            return Err(error);
        }
        result.map_err(AccessError::from)
    }

    /// An existing empty reader receives the exact captured source or target.
    /// Hold the established atlas -> reader lock order; a canceled/lost reply
    /// may follow receiver installation and must never trigger a live reopen.
    pub(crate) fn open_import_reader(&self, handle: u64, readers: &ReaderSessions,
        reader_handle: u64, generation: u64, id: u64, side: AtlasImportSide,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = readers.initialize_prepared(reader_handle,
            |owner, reader_stop| session.search.open_import_reader(&session.atlas,
                owner, generation, id, side, reader_stop).map_err(AccessError::from),
            &mut stop);
        if let Err(error) = cell.validate(epoch) {
            session.cancel_work();
            return Err(error);
        }
        result
    }
}

#[cfg(all(test, unix))]
#[path = "atlas_imports_sessions_tests.rs"]
mod tests;

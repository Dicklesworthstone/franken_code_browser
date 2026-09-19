#![forbid(unsafe_code)]

//! Dispatch only. Live and indexed queries share the same retained search owner,
//! pending slot, cancellation epoch and captured-reader activation route.
use fcb_app::host::atlas_search::AtlasIndexOptions;
use super::*;

pub(crate) enum IndexCommand<'a> {
    Prepare { generation: u64, options: AtlasIndexOptions },
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
            IndexCommand::Info => session.search.index_info(&session.atlas, &mut stop),
            IndexCommand::Query { generation, index_generation, needle, max_matches, max_scan_bytes } =>
                session.search.search_indexed(&session.atlas, generation, index_generation, needle, max_matches, max_scan_bytes, &mut stop),
            IndexCommand::Begin { generation, index_generation, needle, max_matches, max_scan_bytes } =>
                session.search.begin_indexed(&session.atlas, generation, index_generation, needle, max_matches, max_scan_bytes, &mut stop),
            IndexCommand::Clear { generation } => session.search.clear_index(&session.atlas, generation, &mut stop),
        };
        // Canceling while paused is also observed by execute(SearchStep/Page)
        // through the shared session epoch; it cannot start a fresh old cursor.
        if let Err(error) = cell.validate(epoch) {
            session.search.cancel_pending();
            return Err(error);
        }
        result.map_err(AccessError::from)
    }
}

#[cfg(all(test, unix))]
#[path = "atlas_index_sessions_tests.rs"]
mod tests;
#[cfg(all(test, unix))]
#[path = "atlas_index_progressive_tests.rs"]
mod progressive_tests;

#![forbid(unsafe_code)]

//! Dispatch only. Index data, query results and source pins remain in the same
//! RetainedAtlasSearch used by live queries and captured-reader activation.
use fcb_app::host::atlas_search::AtlasIndexOptions;
use super::*;

pub(crate) enum IndexCommand<'a> {
    Prepare { generation: u64, options: AtlasIndexOptions },
    Info,
    Query { generation: u64, index_generation: u64, needle: &'a str, max_matches: usize, max_scan_bytes: u64 },
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
            IndexCommand::Clear { generation } => session.search.clear_index(&session.atlas, generation, &mut stop),
        };
        // As with other session operations, cancellation after acceptance can
        // suppress delivery but does not pretend to undo an already committed
        // index/results snapshot. Info/page on the known handle reconcile it.
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

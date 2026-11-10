#![forbid(unsafe_code)]

//! Captured-index documentation relationships use the existing atlas owner,
//! epoch and worker lock. A link identity never becomes a live-path authority.
use fcb_app::host::atlas_search::{AtlasRelationshipDirection, AtlasRelationshipSide};
use super::*;

pub(crate) enum LinksCommand<'a> {
    Begin { generation: u64, index_generation: u64, max_links: usize, max_parse_bytes: u64 },
    Step { generation: u64 },
    Page { generation: u64, direction: AtlasRelationshipDirection,
        relative_path: Option<&'a [u8]>, start: usize, limit: usize },
}

impl AtlasSessions {
    pub(crate) fn execute_links(&self, handle: u64, command: LinksCommand<'_>,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = match command {
            LinksCommand::Begin { generation, index_generation, max_links, max_parse_bytes } =>
                session.search.begin_relationships(&session.atlas, generation, index_generation,
                    max_links, max_parse_bytes, &mut stop),
            LinksCommand::Step { generation } =>
                session.search.step_relationships(&session.atlas, generation, &mut stop),
            LinksCommand::Page { generation, direction, relative_path, start, limit } =>
                session.search.relationships_page(&session.atlas, generation, direction,
                    relative_path, start, limit, &mut stop),
        };
        if let Err(error) = cell.validate(epoch) {
            session.cancel_work();
            return Err(error);
        }
        result.map_err(AccessError::from)
    }

    /// Check an empty independently owned destination before selecting either
    /// the whole Markdown evidence or its exact captured target. The atlas ->
    /// reader lock order and late-cancel reconciliation match other transfers.
    pub(crate) fn open_link_reader(&self, handle: u64, readers: &ReaderSessions,
        reader_handle: u64, generation: u64, id: u64, side: AtlasRelationshipSide,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = readers.initialize_prepared(reader_handle,
            |owner, reader_stop| session.search.open_relationship_reader(&session.atlas,
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
#[path = "atlas_links_sessions_tests.rs"]
mod tests;

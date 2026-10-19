#![forbid(unsafe_code)]

//! Open an exact desk pane in the ordinary retained reader. This deliberately
//! copies the bounded ORIGINAL capture, never a decoded page or live pathname.
//! The receiving reader has a fresh owner, private analysis and source admission,
//! and remains usable after the desk or its saved checkpoint is closed.

use std::path::Path;
use crate::host::reader::{ReaderSession, ReaderSessionError};
use super::{ArenaOwnerId, DeskError, DeskPaneId, DeskSession, DeskSessionError,
    HostResponse, OutputError, EXIT_OK, check, location_fields};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeskReaderError {
    Desk(DeskSessionError),
    Reader(ReaderSessionError),
}
impl std::fmt::Display for DeskReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Desk(error) => write!(f, "{error}"), Self::Reader(error) => write!(f, "{error}") }
    }
}
impl std::error::Error for DeskReaderError {}
impl From<DeskSessionError> for DeskReaderError { fn from(error: DeskSessionError) -> Self { Self::Desk(error) } }
impl From<DeskError> for DeskReaderError { fn from(error: DeskError) -> Self { Self::Desk(error.into()) } }
impl From<ReaderSessionError> for DeskReaderError { fn from(error: ReaderSessionError) -> Self { Self::Reader(error) } }
impl From<OutputError> for DeskReaderError { fn from(error: OutputError) -> Self { Self::Desk(error.into()) } }
impl DeskReaderError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Desk(error) => error.is_canceled(),
            Self::Reader(ReaderSessionError::Canceled) => true,
            Self::Reader(ReaderSessionError::App(error)) => error.is_canceled(),
            _ => false,
        }
    }
}

impl DeskSession {
    /// Prepare an independent reader and a source-bound handoff receipt before
    /// either is published by the host. The host supplies a fresh, non-reused
    /// receiving owner and retires refused candidates on its worker. This does
    /// not change desk navigation, bookmarks, source IDs or mutation attempts.
    ///
    /// The receipt carries the original desk location separately from the new
    /// reader's IDs. The retained label is ONLY provenance; it must never grant
    /// a native refresh route. All later reader features use the copied capture.
    /// Source admission in ReaderSession::from_bytes covers the bounded copy and
    /// its allocation overlap while the desk continues to charge its own source.
    pub fn open_reader(&mut self, expected: u64, pane: DeskPaneId, owner: ArenaOwnerId,
        mut canceled: impl FnMut() -> bool) -> Result<(ReaderSession, HostResponse), DeskReaderError> {
        let location = self.model().location(pane, expected)?;
        if owner == self.model().owner() { return Err(DeskError::OwnerMismatch.into()); }
        check(&mut canceled)?;
        let source = self.model().source(pane, expected)?;
        let captured_bytes = source.bytes().len() as u64;
        let mut reader = ReaderSession::from_bytes(owner, Path::new(source.logical_path()),
            source.bytes(), &mut canceled)?;
        let info = reader.info(&mut canceled)?;
        let mut out = self.output("open-reader")?;
        out.literal(",\"pane\":")?; out.integer(pane.get())?;
        location_fields(&mut out, location)?;
        out.literal(",\"captured_bytes\":")?; out.integer(captured_bytes)?;
        out.literal(",\"label\":")?;
        out.quoted(reader.path().to_str().ok_or(DeskError::InvalidLocation)?)?;
        out.literal(",\"source_reopened\":false,\"reader\":")?;
        out.literal(info.as_str().trim_end())?;
        out.literal("}\n")?;
        let response = self.finish(out, EXIT_OK, &mut canceled)?;
        check(&mut canceled)?;
        Ok((reader, response))
    }
}

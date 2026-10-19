#![forbid(unsafe_code)]

//! Bounded native ownership of the public reading desk. Source capture,
//! bookmark/history semantics and checkpoint I/O remain in fcb-app/fcb. All
//! operations, including final retirement, belong on a host worker.

use std::{mem::size_of, path::Path, sync::{Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}}};
use fcb::{ByteOffset, ByteRange};
use fcb::search::{ReadingTarget, ReadingWindowOptions};
use fcb_app::host::{HostResponse, desk::{DeskCommand, DeskLimits, DeskSession, DeskSessionError,
    imports::ImportedSourceId, persistence::CheckpointIoError, reader::DeskReaderError}};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use super::reader_sessions::{self, ReaderSessions};

pub(super) const MAX_DESK_SESSIONS: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccessError {
    Unknown, NotOpen, AlreadyOpen, Busy, Capacity, Canceled, Closed, Poisoned,
    InvalidArgument, Desk(DeskSessionError), Checkpoint(CheckpointIoError),
    Reader(reader_sessions::AccessError), ReaderTransfer(DeskReaderError),
}
impl From<DeskSessionError> for AccessError { fn from(e: DeskSessionError) -> Self { Self::Desk(e) } }
impl From<CheckpointIoError> for AccessError { fn from(e: CheckpointIoError) -> Self { Self::Checkpoint(e) } }
impl From<reader_sessions::AccessError> for AccessError { fn from(e: reader_sessions::AccessError) -> Self { Self::Reader(e) } }
impl From<DeskReaderError> for AccessError { fn from(e: DeskReaderError) -> Self { Self::ReaderTransfer(e) } }
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "DESK_HANDLE_UNKNOWN", Self::NotOpen => "DESK_HANDLE_NOT_OPEN",
            Self::AlreadyOpen => "DESK_HANDLE_ALREADY_OPEN", Self::Busy => "DESK_HANDLE_BUSY",
            Self::Capacity => "DESK_HANDLE_CAPACITY", Self::Canceled => "DESK_HANDLE_CANCELED",
            Self::Closed => "DESK_HANDLE_CLOSED", Self::Poisoned => "DESK_HANDLE_POISONED",
            Self::InvalidArgument => "DESK_HANDLE_INVALID_ARGUMENT",
            Self::Desk(e) => return write!(f, "{e}"), Self::Checkpoint(e) => return write!(f, "{e}"),
            Self::Reader(e) => return write!(f, "{e}"), Self::ReaderTransfer(e) => return write!(f, "{e}"),
        })
    }
}
impl AccessError {
    fn canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Closed => true,
            Self::Desk(e) => e.is_canceled(), Self::Checkpoint(e) => e.is_canceled(),
            Self::Reader(e) => e.canceled(), Self::ReaderTransfer(e) => e.is_canceled(), _ => false,
        }
    }
    pub(super) fn json(self, handle: u64) -> String {
        use std::fmt::Write;
        let code = self.to_string();
        let code = if code.len() <= 512 { code.as_str() } else { "DESK_DIAGNOSTIC_LIMIT" };
        let mut out = String::with_capacity(4096);
        let _ = write!(out, "{{\"schema\":\"fcb.reading-desk/1\",\"status\":\"error\",\"owner\":\"{handle}\",\"exit_code\":\"{}\",\"error\":{{\"code\":\"", if self.canceled() { 130 } else { 2 });
        for ch in code.chars() {
            match ch {
                '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"),
                ch if ch <= '\u{1f}' => { let _ = write!(out, "\\u{:04x}", ch as u32); }
                ch => out.push(ch),
            }
        }
        out.push_str("\"}}\n"); out
    }
}

pub(super) enum Command<'a> {
    State,
    Open { expected: u64, attempt: u64, path: &'a Path },
    Apply { expected: u64, attempt: u64, kind: u64, subject: u64,
        start: u64, end: u64, label: &'a str },
    Window { expected: u64, pane: u64, offset: u64, bytes: usize },
    Restore { expected: u64, attempt: u64, path: &'a Path },
}

struct Permit(Arc<AtomicUsize>);
impl Drop for Permit { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
struct Cell {
    id: u64, closed: AtomicBool, epoch: AtomicU64, state: Mutex<Option<DeskSession>>,
    _lease: ResourceLease,
    _permit: Permit,
}
impl Cell {
    fn validate(&self, epoch: u64) -> Result<(), AccessError> {
        if self.closed.load(Ordering::Acquire) { Err(AccessError::Closed) }
        else if self.epoch.load(Ordering::Acquire) != epoch { Err(AccessError::Canceled) }
        else { Ok(()) }
    }
}
pub(super) struct DeskSessions {
    cells: Mutex<[Option<Arc<Cell>>; MAX_DESK_SESSIONS]>,
    live: Arc<AtomicUsize>, budget: ResourceBudget,
}
impl DeskSessions {
    pub(super) fn new() -> Self {
        Self { cells: Mutex::new(std::array::from_fn(|_| None)), live: Arc::new(AtomicUsize::new(0)),
            budget: ResourceBudget::new(ArenaOwnerId::new(1).expect("registry owner"),
                ByteLength::new(64 * 1024)).expect("constant registry budget") }
    }
    /// Admit an empty slot without source I/O or desk construction. The global
    /// allocator prevents a desk handle from aliasing a reader or atlas handle.
    pub(super) fn create(&self) -> Result<u64, AccessError> {
        let mut cells = lock(&self.cells)?;
        let position = cells.iter().position(Option::is_none).ok_or(AccessError::Capacity)?;
        if self.live.load(Ordering::Acquire) >= MAX_DESK_SESSIONS { return Err(AccessError::Capacity); }
        self.live.fetch_add(1, Ordering::AcqRel);
        let permit = Permit(Arc::clone(&self.live));
        let id = reader_sessions::fresh_handle()?;
        let lease = self.budget.try_reserve_managed(ArenaOwnerId::new(1).expect("registry owner"),
            ResourceAllocationId::new(id).map_err(|_| AccessError::Capacity)?,
            ByteLength::new((size_of::<Cell>() + 256) as u64)).map_err(|_| AccessError::Capacity)?;
        cells[position] = Some(Arc::new(Cell { id, closed: AtomicBool::new(false), epoch: AtomicU64::new(1),
            state: Mutex::new(None), _lease: lease, _permit: permit }));
        Ok(id)
    }
    fn get(&self, handle: u64) -> Result<Arc<Cell>, AccessError> {
        let cells = lock(&self.cells)?;
        cells.iter().flatten().find(|cell| cell.id == handle).cloned().ok_or(AccessError::Unknown)
    }
    pub(super) fn start(&self, handle: u64, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        if state.is_some() { return Err(AccessError::AlreadyOpen); }
        let owner = ArenaOwnerId::new(handle).map_err(|_| AccessError::InvalidArgument)?;
        let mut candidate = DeskSession::new(owner, DeskLimits::default())?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let reply = candidate.state(&mut stop)?;
        if stop() { return Err(AccessError::Canceled); }
        *state = Some(candidate);
        Ok(reply)
    }
    fn with_session<T>(&self, handle: u64, mut canceled: impl FnMut() -> bool,
        work: impl FnOnce(&mut DeskSession, &mut dyn FnMut() -> bool) -> Result<T, AccessError>) -> Result<T, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let desk = state.as_mut().ok_or(AccessError::NotOpen)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = work(desk, &mut stop);
        // A canceled response cannot roll back a published mutation. Native
        // callers reconcile state and last_attempt instead of replaying intent.
        cell.validate(epoch)?;
        result
    }
    pub(super) fn execute(&self, handle: u64, command: Command<'_>, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AccessError> {
        self.with_session(handle, canceled, |desk, stop| {
            match command {
                Command::State => return Ok(desk.state(&mut *stop)?),
                Command::Open { expected, attempt, path } => { desk.open_file(expected, attempt, path, &mut *stop)?; }
                Command::Restore { expected, attempt, path } => { desk.restore_checkpoint_file(expected, attempt, path, &mut *stop)?; }
                Command::Window { expected, pane, offset, bytes } => {
                    if !(4..=64 * 1024).contains(&bytes) { return Err(AccessError::InvalidArgument); }
                    let pane = desk.model().pane_id(pane).map_err(DeskSessionError::from)?;
                    return Ok(desk.window(expected, pane, Some(ReadingTarget::Byte(ByteOffset::new(offset))),
                        ReadingWindowOptions { max_bytes: bytes, max_lines: 1024 }, &mut *stop)?);
                }
                Command::Apply { expected, attempt, kind, subject, start, end, label } => {
                    let pane = || desk.model().pane_id(subject).map_err(DeskSessionError::from);
                    let command = match kind {
                        0 => DeskCommand::Focus(pane()?),
                        1 => DeskCommand::Navigate { pane: pane()?, offset: start, selection: None },
                        2 => DeskCommand::Navigate { pane: pane()?, offset: start, selection: Some(range(start, end)?) },
                        3 => {
                            if label.is_empty() || label.len() > 256 { return Err(AccessError::InvalidArgument); }
                            DeskCommand::Bookmark { pane: pane()?, label: label.to_owned() }
                        }
                        4 => DeskCommand::RecallBookmark(subject), 5 => DeskCommand::ForgetBookmark(subject),
                        6 => DeskCommand::Pin { pane: pane()?, pinned: true },
                        7 => DeskCommand::Pin { pane: pane()?, pinned: false },
                        8 => DeskCommand::Close(pane()?), 9 => DeskCommand::Back, 10 => DeskCommand::Forward,
                        11 => DeskCommand::ClearHistory,
                        _ => return Err(AccessError::InvalidArgument),
                    };
                    desk.apply(expected, attempt, command, &mut *stop)?;
                }
            }
            Ok(desk.state(&mut *stop)?)
        })
    }
    /// Explicit original-source transfer; no UI text/hex roundtrip or live path
    /// lookup. Receiving IDs are allocated by DeskSession, never copied from a
    /// foreign owner. Both registries use try_lock and retain cells until drain.
    pub(super) fn import_reader(&self, handle: u64, readers: &ReaderSessions, reader: u64,
        expected: u64, attempt: u64, start: u64, end: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AccessError> {
        let selected = range(start, end)?;
        self.with_session(handle, canceled, |desk, desk_stop| {
            readers.with_retained(reader, |source, source_stop| {
                let capture = source.capture();
                let origin = ImportedSourceId { file: capture.request().file(), revision: capture.request().revision() };
                let label = fcb::source::RawPath::from_path(source.path()).display_escaped().to_string();
                desk.import_source(expected, attempt, origin, &label, capture.bytes(), start, Some(selected), &mut *source_stop)?;
                Ok(desk.state(&mut *source_stop)?)
            }, &mut *desk_stop)
        })
    }
    /// Open a desk pane through the same retained reader registry used by file,
    /// search and pinned readers. The destination must already be admitted and
    /// empty. Source and destination cells survive the complete worker call;
    /// both epochs revoke delivery, and try_lock avoids reciprocal-transfer
    /// deadlock. Only a complete source-bound receipt publishes the destination.
    pub(super) fn open_reader(&self, handle: u64, readers: &ReaderSessions,
        expected: u64, pane: u64, destination: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AccessError> {
        self.with_session(handle, canceled, |desk, desk_stop| {
            let pane = desk.model().pane_id(pane).map_err(DeskSessionError::from)?;
            readers.initialize_prepared(destination, |owner, reader_stop| -> Result<_, AccessError> {
                Ok(desk.open_reader(expected, pane, owner, &mut *reader_stop)?)
            }, &mut *desk_stop)
        })
    }
    /// Preserve the authoritative write receipt even if cancel/close races its
    /// completion. A filesystem effect is never translated into a generic
    /// canceled/no-effect result by a final registry epoch check.
    pub(super) fn save(&self, handle: u64, expected: u64, path: &Path, max_source_bytes: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let desk = state.as_mut().ok_or(AccessError::NotOpen)?;
        let outcome = desk.save_checkpoint(expected, path, max_source_bytes,
            || cell.validate(epoch).is_err() || canceled());
        Ok(desk.checkpoint_save_response(&outcome)?)
    }
    pub(super) fn cancel(&self, handle: u64) -> Result<(), AccessError> {
        let cell = self.get(handle)?;
        if cell.epoch.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1)).is_err() {
            cell.closed.store(true, Ordering::Release); return Err(AccessError::Closed);
        }
        Ok(())
    }
    pub(super) fn close(&self, handle: u64) -> Result<(), AccessError> {
        let removed = {
            let mut cells = lock(&self.cells)?;
            let position = cells.iter().position(|c| c.as_ref().is_some_and(|c| c.id == handle)).ok_or(AccessError::Unknown)?;
            let cell = cells[position].take().ok_or(AccessError::Unknown)?;
            cell.closed.store(true, Ordering::Release); cell
        };
        drop(removed); Ok(())
    }
}
fn range(start: u64, end: u64) -> Result<ByteRange, AccessError> {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| AccessError::InvalidArgument)
}
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AccessError> {
    mutex.try_lock().map_err(|e| match e { TryLockError::WouldBlock => AccessError::Busy,
        TryLockError::Poisoned(_) => AccessError::Poisoned })
}

#[cfg(all(test, unix))]
#[path = "desk_sessions_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "desk_reader_sessions_tests.rs"]
mod reader_tests;

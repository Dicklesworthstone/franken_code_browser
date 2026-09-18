#![forbid(unsafe_code)]

//! Bounded ownership for native retained atlases. This table owns no discovery,
//! geometry, source policy, camera math or renderer. All work uses AtlasSession.
//! The table lock is never held during session work. All locks are try-locks.

use std::{mem::size_of, path::Path, sync::{Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}}};
use fcb_app::host::{HostResponse, atlas_session::{AtlasAction, AtlasSession, AtlasSessionError, AtlasSessionOptions}};
use fcb_core::{ArenaOwnerId, ByteLength, Point2D, ResourceAllocationId, ResourceBudget, ResourceLease};
use super::reader_sessions::{self, ReaderSessions};

pub(super) const MAX_ATLAS_SESSIONS: usize = 4;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccessError {
    Unknown, NotOpen, AlreadyOpen, Busy, Capacity, Canceled, Closed, Poisoned,
    InvalidArgument, Atlas(AtlasSessionError), Reader(reader_sessions::AccessError),
}
impl From<AtlasSessionError> for AccessError { fn from(e: AtlasSessionError) -> Self { Self::Atlas(e) } }
impl From<reader_sessions::AccessError> for AccessError { fn from(e: reader_sessions::AccessError) -> Self { Self::Reader(e) } }
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "ATLAS_HANDLE_UNKNOWN", Self::NotOpen => "ATLAS_HANDLE_NOT_OPEN",
            Self::AlreadyOpen => "ATLAS_HANDLE_ALREADY_OPEN", Self::Busy => "ATLAS_HANDLE_BUSY",
            Self::Capacity => "ATLAS_HANDLE_CAPACITY", Self::Canceled => "ATLAS_HANDLE_CANCELED",
            Self::Closed => "ATLAS_HANDLE_CLOSED", Self::Poisoned => "ATLAS_HANDLE_POISONED",
            Self::InvalidArgument => "ATLAS_HANDLE_INVALID_ARGUMENT",
            Self::Atlas(e) => return write!(f, "{e}"), Self::Reader(e) => return write!(f, "{e}"),
        })
    }
}
impl AccessError {
    pub(super) fn json(self, handle: u64) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(4096);
        let _ = write!(out, "{{\"schema\":\"fcb.atlas-session/1\",\"status\":\"error\",\"owner\":\"{handle}\",\"error\":{{\"code\":\"");
        let code = self.to_string();
        let code = if code.len() <= 512 { code.as_str() } else { "ATLAS_HANDLE_DIAGNOSTIC_LIMIT" };
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

pub(super) enum Command {
    Info,
    Prepare { generation: u64, action: AtlasAction },
    Present { generation: u64, frame: u64, display: u64 },
    Pick { frame: u64, display: u64, point: Point2D },
    Children { parent: u32, start: usize, limit: usize },
}
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
struct Cell {
    id: u64,
    closed: AtomicBool,
    epoch: AtomicU64,
    state: Mutex<Option<AtlasSession>>,
    _lease: ResourceLease,
    _permit: Permit, // Last: source/geometry destruction precedes slot release.
}
impl Cell {
    fn validate(&self, epoch: u64) -> Result<(), AccessError> {
        if self.closed.load(Ordering::Acquire) { Err(AccessError::Closed) }
        else if self.epoch.load(Ordering::Acquire) != epoch { Err(AccessError::Canceled) }
        else { Ok(()) }
    }
}

pub(super) struct AtlasSessions {
    cells: Mutex<[Option<Arc<Cell>>; MAX_ATLAS_SESSIONS]>,
    live: Arc<AtomicUsize>,
    budget: ResourceBudget,
}
impl AtlasSessions {
    pub(super) fn new() -> Self {
        Self { cells: Mutex::new(std::array::from_fn(|_| None)), live: Arc::new(AtomicUsize::new(0)),
            budget: ResourceBudget::new(ArenaOwnerId::new(1).expect("nonzero registry owner"),
                ByteLength::new(64 * 1024)).expect("constant registry admission") }
    }
    pub(super) fn create(&self) -> Result<u64, AccessError> {
        let mut cells = lock(&self.cells)?;
        let slot = cells.iter().position(Option::is_none).ok_or(AccessError::Capacity)?;
        if self.live.load(Ordering::Acquire) >= MAX_ATLAS_SESSIONS { return Err(AccessError::Capacity); }
        self.live.fetch_add(1, Ordering::AcqRel);
        let permit = Permit(Arc::clone(&self.live));
        // Share the reader ID allocator. A wrong-kind handle can never alias a
        // live object in the other table, even if an array slot is reused.
        let id = reader_sessions::fresh_handle()?;
        let lease = self.budget.try_reserve_managed(ArenaOwnerId::new(1).expect("nonzero registry owner"),
            ResourceAllocationId::new(id).map_err(|_| AccessError::Capacity)?,
            ByteLength::new((size_of::<Cell>() + 256) as u64)).map_err(|_| AccessError::Capacity)?;
        cells[slot] = Some(Arc::new(Cell { id, closed: AtomicBool::new(false), epoch: AtomicU64::new(1),
            state: Mutex::new(None), _lease: lease, _permit: permit }));
        Ok(id)
    }
    fn get(&self, handle: u64) -> Result<Arc<Cell>, AccessError> {
        let cells = lock(&self.cells)?;
        cells.iter().flatten().find(|cell| cell.id == handle).cloned().ok_or(AccessError::Unknown)
    }
    pub(super) fn open(&self, handle: u64, root: &Path, options: AtlasSessionOptions,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        if state.is_some() { return Err(AccessError::AlreadyOpen); }
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = (|| {
            let owner = ArenaOwnerId::new(handle).map_err(|_| AccessError::InvalidArgument)?;
            let mut candidate = AtlasSession::open(owner, root, options, &mut stop)?;
            let response = candidate.info(&mut stop)?;
            if stop() { return Err(AccessError::Canceled); }
            *state = Some(candidate);
            Ok(response)
        })();
        cell.validate(epoch)?;
        result
    }
    pub(super) fn execute(&self, handle: u64, command: Command,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let atlas = state.as_mut().ok_or(AccessError::NotOpen)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = match command {
            Command::Info => atlas.info(&mut stop),
            Command::Prepare { generation, action } => atlas.prepare(generation, action, &mut stop),
            Command::Present { generation, frame, display } => atlas.acknowledge(generation, frame, display, &mut stop),
            Command::Pick { frame, display, point } => atlas.pick(frame, display, point, &mut stop),
            Command::Children { parent, start, limit } => atlas.children(parent, start, limit, &mut stop),
        };
        cell.validate(epoch)?;
        result.map_err(AccessError::from)
    }
    /// Fixed lock order: atlas operation then reader operation, both nonblocking.
    /// Reader admission/empty-state checks precede capture. The shared safe app
    /// builds the reader and linked receipt before registry publication.
    pub(super) fn open_reader(&self, handle: u64, readers: &ReaderSessions, reader_handle: u64,
        frame: u64, display: u64, point: Point2D, max_bytes: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let atlas = state.as_mut().ok_or(AccessError::NotOpen)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = readers.initialize_prepared(reader_handle,
            |owner, reader_stop| atlas.open_reader(owner, frame, display, point, max_bytes, reader_stop).map_err(AccessError::from),
            &mut stop);
        // A late close/cancel suppresses delivery, but does not pretend to undo
        // an already installed reader. The caller knows its destination handle
        // and can inspect or close it explicitly to reconcile that boundary.
        cell.validate(epoch)?;
        result
    }
    pub(super) fn cancel(&self, handle: u64) -> Result<(), AccessError> {
        let cell = self.get(handle)?;
        if cell.closed.load(Ordering::Acquire) { return Err(AccessError::Closed); }
        if cell.epoch.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1)).is_err() {
            cell.closed.store(true, Ordering::Release);
            return Err(AccessError::Closed);
        }
        Ok(())
    }
    pub(super) fn close(&self, handle: u64) -> Result<(), AccessError> {
        let removed = {
            let mut cells = lock(&self.cells)?;
            let slot = cells.iter().position(|cell| cell.as_ref().is_some_and(|cell| cell.id == handle))
                .ok_or(AccessError::Unknown)?;
            let cell = cells[slot].take().ok_or(AccessError::Unknown)?;
            cell.closed.store(true, Ordering::Release);
            cell
        };
        drop(removed); // Worker-side final reclamation, never under table lock.
        Ok(())
    }
}
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AccessError> {
    mutex.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => AccessError::Busy, TryLockError::Poisoned(_) => AccessError::Poisoned,
    })
}

#[cfg(all(test, unix))]
#[path = "atlas_sessions_tests.rs"]
mod tests;

#![forbid(unsafe_code)]

//! Bounded ownership for progressively discovered project metadata. The shared
//! app owns traversal, retained pages, path authority and reader capture. This
//! registry supplies nonblocking admission, cancellation and handle retirement.

use std::{mem::size_of, path::Path, sync::{Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}}};
use fcb_app::host::{HostResponse, catalog_session::{CatalogSession, CatalogSessionError}};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, ResourceAllocationId, ResourceBudget, ResourceLease};
use super::reader_sessions::{self, ReaderSessions};

pub(super) const MAX_CATALOG_SESSIONS: usize = 4;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccessError {
    Unknown, NotOpen, AlreadyOpen, Busy, Capacity, Canceled, Closed, Poisoned,
    InvalidArgument, Catalog(CatalogSessionError), Reader(reader_sessions::AccessError),
}
impl From<CatalogSessionError> for AccessError { fn from(e: CatalogSessionError) -> Self { Self::Catalog(e) } }
impl From<reader_sessions::AccessError> for AccessError { fn from(e: reader_sessions::AccessError) -> Self { Self::Reader(e) } }
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "CATALOG_HANDLE_UNKNOWN", Self::NotOpen => "CATALOG_HANDLE_NOT_OPEN",
            Self::AlreadyOpen => "CATALOG_HANDLE_ALREADY_OPEN", Self::Busy => "CATALOG_HANDLE_BUSY",
            Self::Capacity => "CATALOG_HANDLE_CAPACITY", Self::Canceled => "CATALOG_HANDLE_CANCELED",
            Self::Closed => "CATALOG_HANDLE_CLOSED", Self::Poisoned => "CATALOG_HANDLE_POISONED",
            Self::InvalidArgument => "CATALOG_HANDLE_INVALID_ARGUMENT",
            Self::Catalog(e) => return write!(f, "{e}"), Self::Reader(e) => return write!(f, "{e}"),
        })
    }
}
impl AccessError {
    fn canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Closed => true,
            Self::Catalog(error) => error.is_canceled(), Self::Reader(error) => error.canceled(),
            _ => false,
        }
    }
    pub(super) fn json(self, handle: u64) -> String {
        use std::fmt::Write;
        let code = self.to_string();
        let code = if code.len() <= 512 { code.as_str() } else { "CATALOG_HANDLE_DIAGNOSTIC_LIMIT" };
        let mut out = String::with_capacity(4096);
        let _ = write!(out, "{{\"schema\":\"fcb.project-catalog-session/1\",\"status\":\"error\",\"owner\":\"{handle}\",\"exit_code\":\"{}\",\"error\":{{\"code\":\"", if self.canceled() { 130 } else { 2 });
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
    Info, Page { page: u64 }, Advance { after_page: u64 }, Stop,
}
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
struct Cell {
    id: u64, closed: AtomicBool, epoch: AtomicU64,
    state: Mutex<Option<CatalogSession>>,
    _lease: ResourceLease,
    _permit: Permit, // Cursor/page destruction precedes capacity release.
}
impl Cell {
    fn validate(&self, epoch: u64) -> Result<(), AccessError> {
        if self.closed.load(Ordering::Acquire) { Err(AccessError::Closed) }
        else if self.epoch.load(Ordering::Acquire) != epoch { Err(AccessError::Canceled) }
        else { Ok(()) }
    }
}
pub(super) struct CatalogSessions {
    cells: Mutex<[Option<Arc<Cell>>; MAX_CATALOG_SESSIONS]>,
    live: Arc<AtomicUsize>, budget: ResourceBudget,
}
impl CatalogSessions {
    pub(super) fn new() -> Self {
        Self { cells: Mutex::new(std::array::from_fn(|_| None)), live: Arc::new(AtomicUsize::new(0)),
            budget: ResourceBudget::new(ArenaOwnerId::new(1).expect("nonzero registry owner"),
                ByteLength::new(64 * 1024)).expect("constant registry admission") }
    }
    /// Reserve an empty cancelable handle before any filesystem operation.
    /// Its identity cannot alias reader, saved repository or atlas handles.
    pub(super) fn create(&self) -> Result<u64, AccessError> {
        let mut cells = lock(&self.cells)?;
        let position = cells.iter().position(Option::is_none).ok_or(AccessError::Capacity)?;
        if self.live.load(Ordering::Acquire) >= MAX_CATALOG_SESSIONS { return Err(AccessError::Capacity); }
        self.live.fetch_add(1, Ordering::AcqRel);
        let permit = Permit(Arc::clone(&self.live));
        let id = reader_sessions::fresh_handle()?;
        let lease = self.budget.try_reserve_managed(ArenaOwnerId::new(1).expect("nonzero registry owner"),
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
    pub(super) fn open(&self, handle: u64, root: &Path, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        if state.is_some() { return Err(AccessError::AlreadyOpen); }
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = (|| {
            let owner = ArenaOwnerId::new(handle).map_err(|_| AccessError::InvalidArgument)?;
            let mut candidate = CatalogSession::open(owner, root, &mut stop)?;
            let response = candidate.page(1, &mut stop)?;
            if stop() { return Err(AccessError::Canceled); }
            *state = Some(candidate);
            Ok(response)
        })();
        cell.validate(epoch)?;
        result
    }
    pub(super) fn execute(&self, handle: u64, command: Command, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = match command {
            Command::Info => session.info(&mut stop),
            Command::Page { page } => session.page(page, &mut stop),
            Command::Advance { after_page } => session.advance(after_page, &mut stop),
            Command::Stop => session.stop(&mut stop),
        };
        // A directory cursor cannot roll back. The app retains every consumed
        // page before delivery; info/page and advance replay recover late loss.
        cell.validate(epoch)?;
        result.map_err(AccessError::from)
    }
    /// Source session first, empty destination second; both locks are try-locks.
    /// Destination admission precedes metadata resolution and source capture.
    pub(super) fn open_reader(&self, handle: u64, readers: &ReaderSessions, reader: u64,
        page: u64, file: u64, max_source_bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        let file = FileId::new(session.owner(), file).map_err(|_| AccessError::InvalidArgument)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = readers.initialize_prepared(reader, |owner, reader_stop| {
            session.open_reader(owner, page, file, max_source_bytes, reader_stop).map_err(AccessError::from)
        }, &mut stop);
        cell.validate(epoch)?;
        result
    }
    pub(super) fn cancel(&self, handle: u64) -> Result<(), AccessError> {
        let cell = self.get(handle)?;
        if cell.closed.load(Ordering::Acquire) { return Err(AccessError::Closed); }
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
        drop(removed); // In-flight calls keep the permit until their work drains.
        Ok(())
    }
}
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AccessError> {
    mutex.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => AccessError::Busy, TryLockError::Poisoned(_) => AccessError::Poisoned,
    })
}

#[cfg(all(test, unix))]
#[path = "catalog_sessions_tests.rs"]
mod tests;

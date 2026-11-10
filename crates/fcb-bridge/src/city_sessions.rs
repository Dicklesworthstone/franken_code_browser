#![forbid(unsafe_code)]

//! Bounded, nonblocking ownership for metadata-only City scenes. No geometry,
//! projection, file policy or source access lives in this registry. A closed
//! cell retains its admission permit until all in-flight calls have drained.

use std::{mem::size_of, sync::{Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}}};
use fcb_app::host::{HostResponse, city::{CityInput, CitySession, CitySessionError, CityViewAction}};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};

pub(super) const MAX_CITY_SESSIONS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccessError {
    Unknown, NotOpen, AlreadyOpen, Busy, Capacity, Canceled, Closed, Poisoned,
    InvalidArgument, City(CitySessionError),
}
impl From<CitySessionError> for AccessError { fn from(e: CitySessionError) -> Self { Self::City(e) } }
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "CITY_HANDLE_UNKNOWN", Self::NotOpen => "CITY_HANDLE_NOT_OPEN",
            Self::AlreadyOpen => "CITY_HANDLE_ALREADY_OPEN", Self::Busy => "CITY_HANDLE_BUSY",
            Self::Capacity => "CITY_HANDLE_CAPACITY", Self::Canceled => "CITY_HANDLE_CANCELED",
            Self::Closed => "CITY_HANDLE_CLOSED", Self::Poisoned => "CITY_HANDLE_POISONED",
            Self::InvalidArgument => "CITY_HANDLE_INVALID_ARGUMENT",
            Self::City(error) => return write!(f, "{error}"),
        })
    }
}
impl AccessError {
    pub(super) fn json(self, handle: u64) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(4096);
        let _ = write!(out, "{{\"schema\":\"fcb.city/1\",\"status\":\"error\",\"owner\":\"{handle}\",\"error\":{{\"code\":\"");
        let diagnostic = self.to_string();
        let code = if diagnostic.len() <= 512 { diagnostic.as_str() } else { "CITY_HANDLE_DIAGNOSTIC_LIMIT" };
        for ch in code.chars() {
            match ch {
                '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"),
                ch if ch <= '\u{1f}' => { let _ = write!(out, "\\u{:04x}", ch as u32); },
                ch => out.push(ch),
            }
        }
        out.push_str("\"}}\n"); out
    }
}

pub(super) enum Command {
    Info,
    Prepare { generation: u64, action: CityViewAction },
    Present { generation: u64, frame: u64, display: u64 },
    Pick { frame: u64, display: u64, x: f64, y: f64 },
}
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
struct Session { epoch: u64, inner: CitySession }
struct Cell {
    id: u64,
    closed: AtomicBool,
    epoch: AtomicU64,
    state: Mutex<Option<Session>>,
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
pub(super) struct CitySessions {
    cells: Mutex<[Option<Arc<Cell>>; MAX_CITY_SESSIONS]>,
    live: Arc<AtomicUsize>,
    budget: ResourceBudget,
}
impl CitySessions {
    pub(super) fn new() -> Self {
        Self { cells: Mutex::new(std::array::from_fn(|_| None)), live: Arc::new(AtomicUsize::new(0)),
            budget: ResourceBudget::new(ArenaOwnerId::new(1).expect("registry owner"),
                ByteLength::new(64 * 1024)).expect("registry admission") }
    }
    pub(super) fn create(&self) -> Result<u64, AccessError> {
        let mut cells = lock(&self.cells)?;
        let slot = cells.iter().position(Option::is_none).ok_or(AccessError::Capacity)?;
        if self.live.load(Ordering::Acquire) >= MAX_CITY_SESSIONS { return Err(AccessError::Capacity); }
        self.live.fetch_add(1, Ordering::AcqRel);
        let permit = Permit(Arc::clone(&self.live));
        let id = super::reader_sessions::fresh_handle().map_err(|_| AccessError::Capacity)?;
        let lease = self.budget.try_reserve_managed(ArenaOwnerId::new(1).expect("registry owner"),
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
    pub(super) fn open(&self, handle: u64, layout: u64, input: CityInput<'_>, discovery_complete: bool,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        if state.is_some() { return Err(AccessError::AlreadyOpen); }
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let owner = ArenaOwnerId::new(handle).map_err(|_| AccessError::InvalidArgument)?;
        let mut candidate = CitySession::open(owner, layout, input, discovery_complete, &mut stop)?;
        let response = candidate.info(&mut stop)?;
        if stop() { return Err(AccessError::Canceled); }
        *state = Some(Session { epoch, inner: candidate });
        // A concurrent close/cancel can win immediately after publication.
        // Info/close reconcile a lost open response; no rollback is promised.
        cell.validate(epoch)?;
        Ok(response)
    }
    pub(super) fn execute(&self, handle: u64, command: Command,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        if session.epoch != epoch { session.inner.cancel_pending(); session.epoch = epoch; }
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = match command {
            Command::Info => session.inner.info(&mut stop),
            Command::Prepare { generation, action } => session.inner.prepare(generation, action, &mut stop),
            Command::Present { generation, frame, display } => session.inner.present(generation, frame, display, &mut stop),
            Command::Pick { frame, display, x, y } => session.inner.pick(frame, display, x, y, &mut stop),
        }.map_err(AccessError::from);
        if let Err(error) = cell.validate(epoch) { session.inner.cancel_pending(); return Err(error); }
        if matches!(result, Err(AccessError::City(CitySessionError::Canceled))) { session.inner.cancel_pending(); }
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
            let slot = cells.iter().position(|c| c.as_ref().is_some_and(|c| c.id == handle)).ok_or(AccessError::Unknown)?;
            let cell = cells[slot].take().ok_or(AccessError::Unknown)?;
            cell.closed.store(true, Ordering::Release);
            cell
        };
        drop(removed); // Native worker, outside registry lock.
        Ok(())
    }
}
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AccessError> {
    mutex.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => AccessError::Busy, TryLockError::Poisoned(_) => AccessError::Poisoned,
    })
}

#[cfg(test)]
#[path = "city_sessions_tests.rs"]
mod tests;

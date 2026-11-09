#![forbid(unsafe_code)]

//! Bounded ownership for observed-range readers. No source policy or decoder
//! lives here. A source operation holds its cell, but never the registry lock.
//! Closing retires authority immediately and keeps the permit until work drains.

use std::{mem::size_of, path::Path, sync::{Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}}};
use fcb_app::host::{HostResponse, observed_reader::{ObservedReaderSession,
    ObservedReaderError, ObservedReaderEncoding}};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};

pub(super) const MAX_SESSIONS: usize = 4;

#[derive(Debug)]
pub(super) enum AccessError {
    Unknown, NotOpen, AlreadyOpen, Busy, Capacity, Canceled, Closed,
    Poisoned, IdentityExhausted, InvalidArgument, Reader(ObservedReaderError),
}
impl From<ObservedReaderError> for AccessError {
    fn from(error: ObservedReaderError) -> Self { Self::Reader(error) }
}
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "OBSERVED_READER_HANDLE_UNKNOWN",
            Self::NotOpen => "OBSERVED_READER_HANDLE_NOT_OPEN",
            Self::AlreadyOpen => "OBSERVED_READER_HANDLE_ALREADY_OPEN",
            Self::Busy => "OBSERVED_READER_HANDLE_BUSY",
            Self::Capacity => "OBSERVED_READER_HANDLE_CAPACITY",
            Self::Canceled => "OBSERVED_READER_HANDLE_CANCELED",
            Self::Closed => "OBSERVED_READER_HANDLE_CLOSED",
            Self::Poisoned => "OBSERVED_READER_HANDLE_POISONED",
            Self::IdentityExhausted => "OBSERVED_READER_HANDLE_IDENTITY_EXHAUSTED",
            Self::InvalidArgument => "OBSERVED_READER_HANDLE_INVALID_ARGUMENT",
            Self::Reader(error) => return write!(f, "{error}"),
        })
    }
}
impl AccessError {
    fn canceled(&self) -> bool {
        matches!(self, Self::Canceled | Self::Closed)
            || matches!(self, Self::Reader(error) if error.is_canceled())
    }
    pub(super) fn json(&self, handle: u64) -> String {
        use std::fmt::Write;
        let diagnostic = self.to_string();
        let code = if diagnostic.len() <= 512 { diagnostic.as_str() }
            else { "OBSERVED_READER_ERROR_DIAGNOSTIC_LIMIT" };
        let mut out = String::with_capacity(4096);
        let _ = write!(out, "{{\"schema\":\"fcb.observed-reader/1\",\"status\":\"error\",\"owner\":\"{handle}\",\"exit_code\":\"{}\",\"error\":{{\"code\":\"",
            if self.canceled() { 130 } else { 2 });
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
    Info,
    Window { offset: u64, bytes: usize, encoding: ObservedReaderEncoding },
    Retained { observation: u64 },
    Copy { observation: u64, start: u64, end: u64 },
    Find { generation: u64, needle: &'a str, encoding: ObservedReaderEncoding,
        max_matches: usize, max_scan_bytes: u64 },
    SearchHit { generation: u64, position: usize },
    ObserveHit { generation: u64, hit_index: usize, context_bytes: usize, window_bytes: usize },
}

struct Permit(Arc<AtomicUsize>);
impl Drop for Permit { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
struct Cell {
    id: u64,
    closed: AtomicBool,
    epoch: AtomicU64,
    state: Mutex<Option<ObservedReaderSession>>,
    _lease: ResourceLease,
    _permit: Permit,
}
const CELL_CHARGE: u64 = (size_of::<Cell>() + 256) as u64;
impl Cell {
    fn validate(&self, epoch: u64) -> Result<(), AccessError> {
        if self.closed.load(Ordering::Acquire) { Err(AccessError::Closed) }
        else if self.epoch.load(Ordering::Acquire) != epoch { Err(AccessError::Canceled) }
        else { Ok(()) }
    }
}

pub(super) struct ObservedReaderSessions {
    cells: Mutex<[Option<Arc<Cell>>; MAX_SESSIONS]>,
    live: Arc<AtomicUsize>,
    budget: ResourceBudget,
}
impl ObservedReaderSessions {
    pub(super) fn new() -> Self {
        Self { cells: Mutex::new(std::array::from_fn(|_| None)),
            live: Arc::new(AtomicUsize::new(0)),
            budget: ResourceBudget::new(ArenaOwnerId::new(1).expect("registry owner"),
                ByteLength::new(MAX_SESSIONS as u64 * CELL_CHARGE)).expect("bounded registry") }
    }
    pub(super) fn create(&self) -> Result<u64, AccessError> {
        let mut cells = lock(&self.cells)?;
        let position = cells.iter().position(Option::is_none).ok_or(AccessError::Capacity)?;
        if self.live.load(Ordering::Acquire) >= MAX_SESSIONS { return Err(AccessError::Capacity); }
        self.live.fetch_add(1, Ordering::AcqRel);
        let permit = Permit(Arc::clone(&self.live));
        // Share the identity allocator with complete readers, catalog and atlas
        // handles. Different reader kinds cannot accidentally alias an owner.
        let id = super::reader_sessions::fresh_handle().map_err(|_| AccessError::IdentityExhausted)?;
        let lease = self.budget.try_reserve_managed(ArenaOwnerId::new(1).expect("registry owner"),
            ResourceAllocationId::new(id).map_err(|_| AccessError::IdentityExhausted)?,
            ByteLength::new(CELL_CHARGE)).map_err(|_| AccessError::Capacity)?;
        cells[position] = Some(Arc::new(Cell { id, closed: AtomicBool::new(false),
            epoch: AtomicU64::new(1), state: Mutex::new(None), _lease: lease, _permit: permit }));
        Ok(id)
    }
    fn get(&self, handle: u64) -> Result<Arc<Cell>, AccessError> {
        lock(&self.cells)?.iter().flatten().find(|cell| cell.id == handle)
            .cloned().ok_or(AccessError::Unknown)
    }
    pub(super) fn open(&self, handle: u64, root: &Path, relative: &[u8],
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        if state.is_some() { return Err(AccessError::AlreadyOpen); }
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let mut candidate = ObservedReaderSession::open(
            ArenaOwnerId::new(handle).map_err(|_| AccessError::IdentityExhausted)?, root, relative, &mut stop)?;
        let response = candidate.info(&mut stop)?;
        if stop() { return Err(AccessError::Canceled); }
        *state = Some(candidate);
        cell.validate(epoch)?;
        Ok(response)
    }
    pub(super) fn execute(&self, handle: u64, command: Command<'_>,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let reader = state.as_mut().ok_or(AccessError::NotOpen)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = match command {
            Command::Info => reader.info(&mut stop),
            Command::Window { offset, bytes, encoding } => reader.read_window(offset, bytes, encoding, &mut stop),
            Command::Retained { observation } => reader.retained_window(observation, &mut stop),
            Command::Copy { observation, start, end } => reader.copy_range(observation, start, end, &mut stop),
            Command::Find { generation, needle, encoding, max_matches, max_scan_bytes } =>
                reader.search(generation, needle, encoding, max_matches, max_scan_bytes, &mut stop),
            Command::SearchHit { generation, position } => reader.search_hit(generation, position, &mut stop),
            Command::ObserveHit { generation, hit_index, context_bytes, window_bytes } =>
                reader.observe_hit(generation, hit_index, context_bytes, window_bytes, &mut stop),
        };
        cell.validate(epoch)?;
        result.map_err(AccessError::from)
    }
    pub(super) fn cancel(&self, handle: u64) -> Result<(), AccessError> {
        let cell = self.get(handle)?;
        if cell.closed.load(Ordering::Acquire) { return Err(AccessError::Closed); }
        if cell.epoch.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1)).is_err() {
            cell.closed.store(true, Ordering::Release);
            return Err(AccessError::IdentityExhausted);
        }
        Ok(())
    }
    pub(super) fn close(&self, handle: u64) -> Result<(), AccessError> {
        let removed = {
            let mut cells = lock(&self.cells)?;
            let position = cells.iter().position(|cell| cell.as_ref().is_some_and(|cell| cell.id == handle))
                .ok_or(AccessError::Unknown)?;
            let cell = cells[position].take().ok_or(AccessError::Unknown)?;
            cell.closed.store(true, Ordering::Release);
            cell
        };
        drop(removed);
        Ok(())
    }
}
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AccessError> {
    mutex.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => AccessError::Busy, TryLockError::Poisoned(_) => AccessError::Poisoned,
    })
}

#[cfg(all(test, unix))]
#[path = "observed_reader_sessions_tests.rs"]
mod tests;

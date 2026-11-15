#![forbid(unsafe_code)]

//! Cancelable ownership for complete-catalog content queries. Discovery,
//! streaming decoding and exact retained witnesses belong to the application
//! service; this registry only admits, revokes and retires independent handles.

use std::{mem::size_of, path::Path, sync::{Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}}};
use fcb_app::host::{HostResponse, catalog_session::{CatalogContentSearchError,
    CatalogContentSearchOptions, CatalogContentSearchSession, CatalogDiscoveryPolicy}};
use fcb_app::host::atlas_search::AtlasSearchError;
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use super::reader_sessions;

pub(super) const MAX_CATALOG_STREAM_SESSIONS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccessError {
    Unknown, NotOpen, AlreadyOpen, Busy, Capacity, Canceled, Closed, Poisoned,
    InvalidArgument, Search(CatalogContentSearchError), Scope(AtlasSearchError), Reader(reader_sessions::AccessError),
}
impl From<CatalogContentSearchError> for AccessError {
    fn from(error: CatalogContentSearchError) -> Self { Self::Search(error) }
}
impl From<reader_sessions::AccessError> for AccessError {
    fn from(error: reader_sessions::AccessError) -> Self { Self::Reader(error) }
}
impl From<AtlasSearchError> for AccessError {
    fn from(error: AtlasSearchError) -> Self { Self::Scope(error) }
}
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "CATALOG_STREAM_HANDLE_UNKNOWN", Self::NotOpen => "CATALOG_STREAM_HANDLE_NOT_OPEN",
            Self::AlreadyOpen => "CATALOG_STREAM_HANDLE_ALREADY_OPEN", Self::Busy => "CATALOG_STREAM_HANDLE_BUSY",
            Self::Capacity => "CATALOG_STREAM_HANDLE_CAPACITY", Self::Canceled => "CATALOG_STREAM_HANDLE_CANCELED",
            Self::Closed => "CATALOG_STREAM_HANDLE_CLOSED", Self::Poisoned => "CATALOG_STREAM_HANDLE_POISONED",
            Self::InvalidArgument => "CATALOG_STREAM_HANDLE_INVALID_ARGUMENT",
            Self::Search(error) => return write!(f, "{error}"),
            Self::Scope(error) => return write!(f, "{error}"),
            Self::Reader(error) => return write!(f, "{error}"),
        })
    }
}
impl AccessError {
    fn canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Closed => true,
            Self::Search(error) => error.is_canceled(),
            Self::Scope(error) => error == AtlasSearchError::Canceled,
            Self::Reader(error) => error.canceled(),
            _ => false,
        }
    }
    pub(super) fn json(self, handle: u64) -> String {
        use std::fmt::Write;
        let code = self.to_string();
        let code = if code.len() <= 512 { code.as_str() } else { "CATALOG_STREAM_HANDLE_DIAGNOSTIC_LIMIT" };
        let mut out = String::with_capacity(4096);
        let _ = write!(out, "{{\"schema\":\"fcb.catalog-stream/1\",\"status\":\"error\",\"owner\":\"{handle}\",\"exit_code\":\"{}\",\"error\":{{\"code\":\"", if self.canceled() { 130 } else { 2 });
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
    Advance { generation: u64, after_revision: u64, max_steps: usize },
    Page { generation: u64, revision: u64, start: usize, count: usize },
    Hit { generation: u64, revision: u64, position: usize },
    Stop { generation: u64 },
}
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
struct Cell {
    id: u64, closed: AtomicBool, epoch: AtomicU64,
    state: Mutex<Option<CatalogContentSearchSession>>,
    _lease: ResourceLease,
    _permit: Permit, // Cursor and witness destruction precede capacity release.
}
impl Cell {
    fn validate(&self, epoch: u64) -> Result<(), AccessError> {
        if self.closed.load(Ordering::Acquire) { Err(AccessError::Closed) }
        else if self.epoch.load(Ordering::Acquire) != epoch { Err(AccessError::Canceled) }
        else { Ok(()) }
    }
}
pub(super) struct CatalogStreamSessions {
    cells: Mutex<[Option<Arc<Cell>>; MAX_CATALOG_STREAM_SESSIONS]>,
    live: Arc<AtomicUsize>, budget: ResourceBudget,
}
impl CatalogStreamSessions {
    pub(super) fn new() -> Self {
        Self { cells: Mutex::new(std::array::from_fn(|_| None)), live: Arc::new(AtomicUsize::new(0)),
            budget: ResourceBudget::new(ArenaOwnerId::new(1).expect("nonzero registry owner"),
                ByteLength::new(64 * 1024)).expect("constant registry admission") }
    }
    /// An inert reservation. The shared allocator prevents cross-family aliases.
    pub(super) fn create(&self) -> Result<u64, AccessError> {
        let mut cells = lock(&self.cells)?;
        let position = cells.iter().position(Option::is_none).ok_or(AccessError::Capacity)?;
        if self.live.load(Ordering::Acquire) >= MAX_CATALOG_STREAM_SESSIONS { return Err(AccessError::Capacity); }
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
    pub(super) fn open(&self, handle: u64, root: &Path, generation: u64, query: &str,
        options: CatalogContentSearchOptions, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AccessError> {
        self.open_with_policy(handle, root, generation, query, options,
            CatalogDiscoveryPolicy::RepositoryRules, canceled)
    }
    pub(super) fn open_with_policy(&self, handle: u64, root: &Path, generation: u64, query: &str,
        options: CatalogContentSearchOptions, policy: CatalogDiscoveryPolicy,
        mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        if state.is_some() { return Err(AccessError::AlreadyOpen); }
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = (|| {
            let owner = ArenaOwnerId::new(handle).map_err(|_| AccessError::InvalidArgument)?;
            let candidate = CatalogContentSearchSession::open_with_policy(owner, root, generation, query, options, policy, &mut stop)?;
            // Installation precedes fallible reply delivery. The metadata batch
            // and immutable query stay recoverable through info after a lost reply.
            *state = Some(candidate);
            state.as_mut().ok_or(AccessError::NotOpen)?.info(&mut stop).map_err(AccessError::from)
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
            Command::Advance { generation, after_revision, max_steps } => session.advance(generation, after_revision, max_steps, &mut stop),
            Command::Page { generation, revision, start, count } => session.page(generation, revision, start, count, &mut stop),
            Command::Hit { generation, revision, position } => session.hit(generation, revision, position, &mut stop),
            Command::Stop { generation } => session.stop(generation, &mut stop),
        };
        // Complete file receipts are retained before fallible publication. Epoch
        // validation suppresses delivery to a canceled or retired native owner.
        cell.validate(epoch)?;
        result.map_err(AccessError::from)
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
            let position = cells.iter().position(|cell| cell.as_ref().is_some_and(|cell| cell.id == handle))
                .ok_or(AccessError::Unknown)?;
            let cell = cells[position].take().ok_or(AccessError::Unknown)?;
            cell.closed.store(true, Ordering::Release); cell
        };
        drop(removed); // Entered work holds the capacity permit until drained.
        Ok(())
    }
}
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AccessError> {
    mutex.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => AccessError::Busy, TryLockError::Poisoned(_) => AccessError::Poisoned,
    })
}

#[cfg(all(test, unix))]
#[path = "catalog_stream_sessions_tests.rs"]
mod tests;

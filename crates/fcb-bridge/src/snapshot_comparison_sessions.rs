#![forbid(unsafe_code)]

//! Fixed-capacity C ownership for comparing two saved project captures.
//! Archive verification, classification and retained reader construction belong
//! to the safe host service. A registry slot is admitted before archive I/O;
//! closing revokes authority immediately and retains its permit while calls drain.

use std::{mem::size_of, path::Path, sync::{Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}}};
use fcb_app::host::{HostResponse, saved_repository::SnapshotLimits};
use fcb_app::host::snapshot_comparison::{SnapshotComparisonSession, SnapshotComparisonError,
    SnapshotChangeFilter, SnapshotComparisonSide};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::reader_sessions::{self, ReaderSessions};

pub(super) const MAX_SNAPSHOT_COMPARISON_SESSIONS: usize = 2;

#[derive(Debug)]
pub(super) enum AccessError {
    Unknown, NotOpen, AlreadyOpen, Busy, Capacity, Canceled, Closed, Poisoned,
    InvalidArgument, Comparison(SnapshotComparisonError), Reader(reader_sessions::AccessError),
}
impl From<SnapshotComparisonError> for AccessError {
    fn from(error: SnapshotComparisonError) -> Self { Self::Comparison(error) }
}
impl From<reader_sessions::AccessError> for AccessError {
    fn from(error: reader_sessions::AccessError) -> Self { Self::Reader(error) }
}
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "SNAPSHOT_COMPARISON_HANDLE_UNKNOWN",
            Self::NotOpen => "SNAPSHOT_COMPARISON_HANDLE_NOT_OPEN",
            Self::AlreadyOpen => "SNAPSHOT_COMPARISON_HANDLE_ALREADY_OPEN",
            Self::Busy => "SNAPSHOT_COMPARISON_HANDLE_BUSY",
            Self::Capacity => "SNAPSHOT_COMPARISON_HANDLE_CAPACITY",
            Self::Canceled => "SNAPSHOT_COMPARISON_HANDLE_CANCELED",
            Self::Closed => "SNAPSHOT_COMPARISON_HANDLE_CLOSED",
            Self::Poisoned => "SNAPSHOT_COMPARISON_HANDLE_POISONED",
            Self::InvalidArgument => "SNAPSHOT_COMPARISON_HANDLE_INVALID_ARGUMENT",
            Self::Comparison(error) => return write!(f, "{error}"),
            Self::Reader(error) => return write!(f, "{error}"),
        })
    }
}
impl AccessError {
    fn canceled(&self) -> bool {
        match self {
            Self::Canceled | Self::Closed => true,
            Self::Comparison(error) => error.is_canceled(),
            Self::Reader(error) => error.canceled(),
            _ => false,
        }
    }
    pub(super) fn json(self, handle: u64) -> String {
        use std::fmt::Write;
        let diagnostic = self.to_string();
        let code = if diagnostic.len() <= 512 { diagnostic.as_str() }
            else { "SNAPSHOT_COMPARISON_HANDLE_DIAGNOSTIC_LIMIT" };
        let mut out = String::with_capacity(4096);
        let _ = write!(out, "{{\"schema\":\"fcb.snapshot-comparison/1\",\"status\":\"error\",\"owner\":\"{handle}\",\"exit_code\":\"{}\",\"error\":{{\"code\":\"",
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

pub(super) enum Command {
    Info,
    Begin { generation: u64 },
    Step { generation: u64, max_entries: usize },
    Page { generation: u64, filter: SnapshotChangeFilter, start: usize, limit: usize },
}
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); }
}
struct Session {
    observed_epoch: u64,
    inner: SnapshotComparisonSession,
}
impl Session {
    fn synchronize(&mut self, epoch: u64) {
        if self.observed_epoch != epoch {
            // Cancellation between steps must retire pending work instead of
            // continuing it under the next call's newly sampled epoch.
            self.inner.cancel_pending();
            self.observed_epoch = epoch;
        }
    }
}
struct Cell {
    id: u64,
    closed: AtomicBool,
    epoch: AtomicU64,
    state: Mutex<Option<Session>>,
    _lease: ResourceLease,
    _permit: Permit, // Last: archive/state destruction precedes slot release.
}
impl Cell {
    fn validate(&self, epoch: u64) -> Result<(), AccessError> {
        if self.closed.load(Ordering::Acquire) { Err(AccessError::Closed) }
        else if self.epoch.load(Ordering::Acquire) != epoch { Err(AccessError::Canceled) }
        else { Ok(()) }
    }
}
pub(super) struct SnapshotComparisonSessions {
    cells: Mutex<[Option<Arc<Cell>>; MAX_SNAPSHOT_COMPARISON_SESSIONS]>,
    live: Arc<AtomicUsize>,
    budget: ResourceBudget,
}
impl SnapshotComparisonSessions {
    pub(super) fn new() -> Self {
        Self { cells: Mutex::new(std::array::from_fn(|_| None)), live: Arc::new(AtomicUsize::new(0)),
            budget: ResourceBudget::new(ArenaOwnerId::new(1).expect("nonzero registry owner"),
                ByteLength::new(64 * 1024)).expect("constant registry admission") }
    }
    pub(super) fn create(&self) -> Result<u64, AccessError> {
        let mut cells = lock(&self.cells)?;
        let position = cells.iter().position(Option::is_none).ok_or(AccessError::Capacity)?;
        if self.live.load(Ordering::Acquire) >= MAX_SNAPSHOT_COMPARISON_SESSIONS {
            return Err(AccessError::Capacity);
        }
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
    pub(super) fn open(&self, handle: u64, before: &Path, after: &Path, limits: SnapshotLimits,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        if state.is_some() { return Err(AccessError::AlreadyOpen); }
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = (|| {
            let owner = ArenaOwnerId::new(handle).map_err(|_| AccessError::InvalidArgument)?;
            let mut candidate = SnapshotComparisonSession::open(owner, before, after, limits, &mut stop)?;
            let response = candidate.info(&mut stop)?;
            if stop() { return Err(AccessError::Canceled); }
            *state = Some(Session { observed_epoch: epoch, inner: candidate });
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
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = match command {
            Command::Info => session.inner.info(&mut stop),
            Command::Begin { generation } => session.inner.begin(generation, &mut stop),
            Command::Step { generation, max_entries } => session.inner.step(generation, max_entries, &mut stop),
            Command::Page { generation, filter, start, limit } =>
                session.inner.page(generation, filter, start, limit, &mut stop),
        }.map_err(AccessError::from);
        if let Err(error) = cell.validate(epoch) {
            session.inner.cancel_pending();
            return Err(error);
        }
        if result.as_ref().is_err_and(AccessError::canceled) { session.inner.cancel_pending(); }
        result
    }
    /// Borrow source before destination; both use nonblocking locks. An unknown,
    /// occupied or canceled reader is refused before member payload I/O begins.
    /// Both archives share the comparison owner but each new reader owns its
    /// transferred exact capture independently of either archive's later lifetime.
    pub(super) fn open_reader(&self, handle: u64, readers: &ReaderSessions, reader: u64,
        generation: u64, entry_id: u64, side: SnapshotComparisonSide,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        if handle == reader { return Err(AccessError::InvalidArgument); }
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        session.synchronize(epoch);
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = readers.initialize_prepared(reader, |owner, reader_stop|
            session.inner.open_reader(owner, generation, entry_id, side, reader_stop).map_err(AccessError::from),
            &mut stop);
        if let Err(error) = cell.validate(epoch) {
            session.inner.cancel_pending();
            return Err(error);
        }
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
            let position = cells.iter().position(|cell| cell.as_ref().is_some_and(|cell| cell.id == handle))
                .ok_or(AccessError::Unknown)?;
            let cell = cells[position].take().ok_or(AccessError::Unknown)?;
            cell.closed.store(true, Ordering::Release);
            cell
        };
        drop(removed); // Outside table lock; active calls retain their permit.
        Ok(())
    }
}
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AccessError> {
    mutex.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => AccessError::Busy,
        TryLockError::Poisoned(_) => AccessError::Poisoned,
    })
}

#[cfg(all(test, unix))]
#[path = "snapshot_comparison_sessions_tests.rs"]
mod tests;

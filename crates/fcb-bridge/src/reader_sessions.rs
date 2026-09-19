#![forbid(unsafe_code)]

//! Fixed-capacity ownership for C reader handles. No raw session pointers,
//! filesystem policy, worker threads or alternate search/decoder implementation.
//! Table locks never enclose source work; a busy session fails instead of
//! blocking another caller. Closing removes authority immediately, but its
//! admission permit survives until the last in-flight call releases the cell.

use std::{mem::size_of, path::Path, sync::{Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}}};
use fcb_app::host::{HostError, HostResponse};
use fcb_app::host::reader::{ReaderSession, ReaderSessionError, ReaderOutlineOptions};
use fcb::search::{SymbolError, SymbolNameMode};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};

pub(super) const MAX_READER_SESSIONS: usize = 8;
// Process/library-instance-local identities, not persistent file anchors. Value
// 1 is reserved for the old one-shot application service's observation domain.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccessError {
    UnknownHandle, NotOpen, AlreadyOpen, Busy, Capacity, Canceled, Closed,
    Poisoned, IdentityExhausted, InvalidArgument, Reader(ReaderSessionError),
}
impl From<ReaderSessionError> for AccessError { fn from(e: ReaderSessionError) -> Self { Self::Reader(e) } }
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownHandle => "READER_HANDLE_UNKNOWN", Self::NotOpen => "READER_HANDLE_NOT_OPEN",
            Self::AlreadyOpen => "READER_HANDLE_ALREADY_OPEN", Self::Busy => "READER_HANDLE_BUSY",
            Self::Capacity => "READER_HANDLE_CAPACITY", Self::Canceled => "READER_HANDLE_CANCELED",
            Self::Closed => "READER_HANDLE_CLOSED", Self::Poisoned => "READER_HANDLE_POISONED",
            Self::IdentityExhausted => "READER_HANDLE_IDENTITY_EXHAUSTED", Self::InvalidArgument => "READER_HANDLE_INVALID_ARGUMENT",
            Self::Reader(error) => return write!(f, "{error}"),
        })
    }
}
impl AccessError {
    pub(super) fn canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Closed | Self::Reader(ReaderSessionError::Canceled)
                | Self::Reader(ReaderSessionError::Symbol(SymbolError::Canceled)) => true,
            Self::Reader(ReaderSessionError::Host(HostError::App(e)) | ReaderSessionError::App(e)) => e.is_canceled(),
            Self::Reader(ReaderSessionError::View(e)) => fcb_app::AppError::View(e).is_canceled(),
            _ => false,
        }
    }
    /// Small, bounded error marshaling only. Never includes a source payload or
    /// native path, and never turns a failure into an empty successful source.
    pub(super) fn json(self, handle: u64) -> String {
        use std::fmt::Write;
        let diagnostic = self.to_string();
        let code = if diagnostic.len() <= 512 { diagnostic.as_str() } else { "READER_ERROR_DIAGNOSTIC_LIMIT" };
        let mut out = String::with_capacity(4096);
        let _ = write!(out, "{{\"schema\":\"fcb.reader-session/1\",\"status\":\"error\",\"owner\":\"{handle}\",\"exit_code\":\"{}\",\"error\":{{\"code\":\"", if self.canceled() { 130 } else { 2 });
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
    Window { offset: u64, bytes: usize },
    Lines { first: u64, count: u64, bytes: usize },
    Find { generation: u64, needle: &'a str, limit: usize, scan_bytes: u64 },
    Hit { generation: u64, index: usize, context: usize },
    CopyRange { start: u64, end: u64 },
    CopyHit { generation: u64, index: usize },
    Outline { generation: u64, options: ReaderOutlineOptions },
    Symbols { generation: u64, needle: &'a str, mode: SymbolNameMode, start: usize, limit: usize },
    Symbol { generation: u64, id: u64, context: usize },
    CopySymbol { generation: u64, id: u64, whole_declaration: bool },
    ClearOutline { generation: u64 },
}

struct Permit(Arc<AtomicUsize>);
impl Drop for Permit { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
struct Cell {
    id: u64,
    closed: AtomicBool,
    epoch: AtomicU64,
    state: Mutex<Option<ReaderSession>>,
    _lease: ResourceLease,
    // Last: source destruction precedes releasing the live/retiring slot.
    _permit: Permit,
}
impl Cell {
    fn validate(&self, epoch: u64) -> Result<(), AccessError> {
        if self.closed.load(Ordering::Acquire) { Err(AccessError::Closed) }
        else if self.epoch.load(Ordering::Acquire) != epoch { Err(AccessError::Canceled) }
        else { Ok(()) }
    }
}

pub(super) struct ReaderSessions {
    cells: Mutex<[Option<Arc<Cell>>; MAX_READER_SESSIONS]>,
    live: Arc<AtomicUsize>,
    budget: ResourceBudget,
}
impl ReaderSessions {
    pub(super) fn new() -> Self {
        Self { cells: Mutex::new(std::array::from_fn(|_| None)), live: Arc::new(AtomicUsize::new(0)),
            budget: ResourceBudget::new(ArenaOwnerId::new(1).expect("nonzero registry owner"),
                ByteLength::new(64 * 1024)).expect("constant registry admission") }
    }
    /// Acquire a small empty handle before any source open, so another native
    /// thread can cancel/close even initial capture. There is no hidden work.
    pub(super) fn create(&self) -> Result<u64, AccessError> {
        let mut cells = lock(&self.cells)?;
        let position = cells.iter().position(Option::is_none).ok_or(AccessError::Capacity)?;
        // create is serialized by the table. Other threads can only decrement
        // this counter by releasing retired cells, never race a second increment.
        if self.live.load(Ordering::Acquire) >= MAX_READER_SESSIONS { return Err(AccessError::Capacity); }
        self.live.fetch_add(1, Ordering::AcqRel);
        let permit = Permit(Arc::clone(&self.live));
        let id = fresh_handle()?;
        let lease = self.budget.try_reserve_managed(ArenaOwnerId::new(1).expect("nonzero registry owner"),
            ResourceAllocationId::new(id).map_err(|_| AccessError::IdentityExhausted)?,
            ByteLength::new((size_of::<Cell>() + 256) as u64)).map_err(|_| AccessError::Capacity)?;
        cells[position] = Some(Arc::new(Cell { id, closed: AtomicBool::new(false), epoch: AtomicU64::new(1),
            state: Mutex::new(None), _lease: lease, _permit: permit }));
        Ok(id)
    }
    fn get(&self, handle: u64) -> Result<Arc<Cell>, AccessError> {
        let cells = lock(&self.cells)?;
        cells.iter().flatten().find(|cell| cell.id == handle).cloned().ok_or(AccessError::UnknownHandle)
    }
    pub(super) fn open(&self, handle: u64, path: &Path, limit: usize,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        self.initialize(handle, |owner, stop| ReaderSession::open(owner, path, limit, stop), canceled)
    }
    fn initialize(&self, handle: u64,
        build: impl FnOnce(ArenaOwnerId, &mut dyn FnMut() -> bool) -> Result<ReaderSession, ReaderSessionError>,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        self.initialize_prepared(handle, |owner, stop| {
            let mut candidate = build(owner, &mut *stop)?;
            let reply = candidate.info(&mut *stop)?;
            Ok((candidate, reply))
        }, canceled)
    }
    /// Admit an empty destination before a shared application workflow prepares
    /// its capture and complete receipt. Used by acknowledged atlas activation;
    /// the same cancellation/close/owner rules apply as ordinary reader open.
    pub(super) fn initialize_prepared<E: From<AccessError>>(&self, handle: u64,
        build: impl FnOnce(ArenaOwnerId, &mut dyn FnMut() -> bool) -> Result<(ReaderSession, HostResponse), E>,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, E> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        if state.is_some() { return Err(AccessError::AlreadyOpen.into()); }
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = (|| {
            let (candidate, reply) = build(ArenaOwnerId::new(handle).map_err(|_| AccessError::IdentityExhausted)?, &mut stop)?;
            if candidate.capture().request().file().owner().get() != handle {
                return Err(AccessError::InvalidArgument.into());
            }
            if stop() { return Err(AccessError::Canceled.into()); }
            *state = Some(candidate);
            Ok(reply)
        })();
        // A close/revocation during work suppresses newly returned output. A
        // cancel after state acceptance is not a rollback; info exposes accepted
        // state, and hosts still reject responses for closed/obsolete handles.
        cell.validate(epoch)?;
        result
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
            Command::Window { offset, bytes } => reader.read_window(offset, bytes, &mut stop),
            Command::Lines { first, count, bytes } => reader.read_lines(first, count, bytes, &mut stop),
            Command::Find { generation, needle, limit, scan_bytes } => reader.search(generation, needle, limit, scan_bytes, &mut stop),
            Command::Hit { generation, index, context } => reader.hit_window(generation, index, context, &mut stop),
            Command::CopyRange { start, end } => reader.copy_range(start, end, &mut stop),
            Command::CopyHit { generation, index } => reader.copy_hit(generation, index, &mut stop),
            Command::Outline { generation, options } => reader.prepare_outline(generation, options, &mut stop),
            Command::Symbols { generation, needle, mode, start, limit } => reader.symbol_page(generation, needle, mode, start, limit, &mut stop),
            Command::Symbol { generation, id, context } => reader.symbol_window(generation, id, context, &mut stop),
            Command::CopySymbol { generation, id, whole_declaration } => reader.copy_symbol(generation, id, whole_declaration, &mut stop),
            Command::ClearOutline { generation } => reader.clear_outline(generation, &mut stop),
        };
        cell.validate(epoch)?;
        result.map_err(AccessError::from)
    }
    /// Invalidate the current operation without locking source/query state. The
    /// immutable capture remains open; a later operation starts in the new epoch.
    /// No worker is killed and no foreign filesystem call gains a hard deadline.
    pub(super) fn cancel(&self, handle: u64) -> Result<(), AccessError> {
        let cell = self.get(handle)?;
        if cell.closed.load(Ordering::Acquire) { return Err(AccessError::Closed); }
        if cell.epoch.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1)).is_err() {
            cell.closed.store(true, Ordering::Release);
            return Err(AccessError::IdentityExhausted);
        }
        Ok(())
    }
    /// Nonblocking with respect to active source work. Destruction of the final
    /// (bounded) source allocation is worker work and happens outside the table.
    pub(super) fn close(&self, handle: u64) -> Result<(), AccessError> {
        let removed = {
            let mut cells = lock(&self.cells)?;
            let position = cells.iter().position(|c| c.as_ref().is_some_and(|c| c.id == handle))
                .ok_or(AccessError::UnknownHandle)?;
            let cell = cells[position].take().ok_or(AccessError::UnknownHandle)?;
            cell.closed.store(true, Ordering::Release);
            cell
        };
        drop(removed);
        Ok(())
    }
    #[cfg(all(test, unix))]
    fn supply(&self, handle: u64, bytes: &[u8]) -> Result<HostResponse, AccessError> {
        self.initialize(handle, |owner, stop| ReaderSession::from_bytes(owner, Path::new("retained.rs"), bytes, stop), || false)
    }
}
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AccessError> {
    mutex.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => AccessError::Busy, TryLockError::Poisoned(_) => AccessError::Poisoned,
    })
}
// One identity source for every retained C handle kind in this loaded library.
pub(super) fn fresh_handle() -> Result<u64, AccessError> { allocate_handle(&NEXT_HANDLE) }
fn allocate_handle(next: &AtomicU64) -> Result<u64, AccessError> {
    next.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
        .map_err(|_| AccessError::IdentityExhausted)
}

#[cfg(all(test, unix))]
#[path = "reader_sessions_tests.rs"]
mod tests;
#[cfg(all(test, unix))]
#[path = "reader_outline_sessions_tests.rs"]
mod outline_tests;

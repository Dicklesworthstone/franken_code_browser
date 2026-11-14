#![forbid(unsafe_code)]

//! Bounded handle ownership for saved repositories. Archive verification,
//! postings, search, copying and source policy stay in the shared host engine.
//! Uses the existing global handle allocator and reader destination registry.

use std::{mem::size_of, path::Path, sync::{Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}}};
use fcb_app::host::{HostResponse, saved_repository::{SavedRepositorySession, SavedRepositoryError,
    SnapshotLimits, Sha256Digest}};
use fcb_app::host::saved_repository::relationships::{SavedRelationshipError, RelationshipDirection, RelationshipSide};
use fcb_app::host::saved_repository::imports::{SavedImportError, SavedImportDirection, SavedImportSide};
use fcb_app::host::saved_repository::paths::SavedPathOptions;
use fcb_app::host::saved_repository::index_build::SavedIndexBuildLimits;
use fcb_app::host::saved_repository::desk::SavedDeskError;
use fcb_app::host::saved_repository::structure::{StructureMode, SymbolNameMode};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use super::reader_sessions::{self, ReaderSessions};

pub(super) const MAX_SAVED_SESSIONS: usize = 4;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccessError {
    Unknown, NotOpen, AlreadyOpen, Busy, Capacity, Canceled, Closed, Poisoned,
    InvalidArgument, Saved(SavedRepositoryError), Relationship(SavedRelationshipError),
    Expression(SavedDeskError), Import(SavedImportError), Reader(reader_sessions::AccessError),
}
impl From<SavedRepositoryError> for AccessError { fn from(e: SavedRepositoryError) -> Self { Self::Saved(e) } }
impl From<SavedRelationshipError> for AccessError { fn from(e: SavedRelationshipError) -> Self { Self::Relationship(e) } }
impl From<SavedDeskError> for AccessError { fn from(e: SavedDeskError) -> Self { Self::Expression(e) } }
impl From<SavedImportError> for AccessError { fn from(e: SavedImportError) -> Self { Self::Import(e) } }
impl From<reader_sessions::AccessError> for AccessError { fn from(e: reader_sessions::AccessError) -> Self { Self::Reader(e) } }
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "SAVED_HANDLE_UNKNOWN", Self::NotOpen => "SAVED_HANDLE_NOT_OPEN",
            Self::AlreadyOpen => "SAVED_HANDLE_ALREADY_OPEN", Self::Busy => "SAVED_HANDLE_BUSY",
            Self::Capacity => "SAVED_HANDLE_CAPACITY", Self::Canceled => "SAVED_HANDLE_CANCELED",
            Self::Closed => "SAVED_HANDLE_CLOSED", Self::Poisoned => "SAVED_HANDLE_POISONED",
            Self::InvalidArgument => "SAVED_HANDLE_INVALID_ARGUMENT",
            Self::Saved(e) => return write!(f, "{e}"), Self::Relationship(e) => return write!(f, "{e}"),
            Self::Expression(e) => return write!(f, "{e}"),
            Self::Import(e) => return write!(f, "{e}"),
            Self::Reader(e) => return write!(f, "{e}"),
        })
    }
}
impl AccessError {
    fn canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Closed => true,
            Self::Saved(error) => error.is_canceled(), Self::Reader(error) => error.canceled(),
            Self::Relationship(error) => error.is_canceled(),
            Self::Expression(error) => error.is_canceled(),
            Self::Import(error) => error.is_canceled(),
            _ => false,
        }
    }
    pub(super) fn json(self, handle: u64) -> String {
        use std::fmt::Write;
        let code = self.to_string();
        let code = if code.len() <= 512 { code.as_str() } else { "SAVED_HANDLE_DIAGNOSTIC_LIMIT" };
        let mut out = String::with_capacity(4096);
        let _ = write!(out, "{{\"schema\":\"fcb.saved-repository/1\",\"status\":\"error\",\"owner\":\"{handle}\",\"exit_code\":\"{}\",\"error\":{{\"code\":\"", if self.canceled() { 130 } else { 2 });
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
    Info, Members { start: usize, limit: usize },
    IndexBuildReceipt { operation: u64 },
    AttachIndex { generation: u64, path: &'a Path, pin: Sha256Digest },
    DetachIndex { generation: u64 },
    Search { generation: u64, needle: &'a str, limit: usize },
    SearchCounted { generation: u64, needle: &'a str, limit: usize },
    SearchMatching { generation: u64, needle: &'a str, limit: usize,
        case_sensitive: bool, count_all_matches: bool },
    Results { generation: u64, start: usize, limit: usize },
    Clear { generation: u64 },
    ExpressionSearch { generation: u64, query: &'a str, max_hits: usize, max_scan_bytes: u64 },
    ExpressionSearchCounted { generation: u64, query: &'a str, max_hits: usize, max_scan_bytes: u64 },
    ExpressionSearchMatching { generation: u64, query: &'a str, max_hits: usize, max_scan_bytes: u64,
        case_sensitive: bool, count_all_matches: bool },
    ExpressionResults { generation: u64, start: usize, limit: usize },
    ClearExpression { generation: u64 },
    StructureSearch { generation: u64, mode: StructureMode, name_mode: SymbolNameMode,
        needle: &'a str, max_hits: usize, max_source_bytes: u64 },
    StructureResults { generation: u64, start: usize, limit: usize },
    ClearStructure { generation: u64 },
    PrepareRelationships { generation: u64, max_links: usize, max_source_bytes: u64 },
    Relationships { generation: u64, member: Option<usize>, direction: RelationshipDirection, start: usize, limit: usize },
    ClearRelationships { generation: u64 },
    PrepareImports { generation: u64, max_links: usize, max_source_bytes: u64 },
    Imports { generation: u64, member: Option<usize>, direction: SavedImportDirection, start: usize, limit: usize },
    ClearImports { generation: u64 },
    PathSearch { generation: u64, needle: &'a [u8], options: SavedPathOptions },
    PathResults { generation: u64, start: usize, limit: usize },
    ClearPaths { generation: u64 },
}
pub(super) enum DocumentImageCommand {
    Authorize,
    Prepare { item_index: usize, request_id: u64 },
}
#[derive(Clone, Copy)]
pub(super) enum Selection {
    Hit { generation: u64, id: u64 }, Member(usize),
    ExpressionHit { generation: u64, id: u64 },
    StructureHit { generation: u64, id: u64 },
    Relationship { generation: u64, id: u64, side: RelationshipSide },
    Import { generation: u64, id: u64, side: SavedImportSide },
    PathHit { generation: u64, id: u64 },
}

struct Permit(Arc<AtomicUsize>);
impl Drop for Permit { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
struct Cell {
    id: u64, closed: AtomicBool, epoch: AtomicU64,
    state: Mutex<Option<SavedRepositorySession>>,
    _lease: ResourceLease,
    _permit: Permit, // Last: archive/index destruction precedes slot release.
}
impl Cell {
    fn validate(&self, epoch: u64) -> Result<(), AccessError> {
        if self.closed.load(Ordering::Acquire) { Err(AccessError::Closed) }
        else if self.epoch.load(Ordering::Acquire) != epoch { Err(AccessError::Canceled) }
        else { Ok(()) }
    }
}
pub(super) struct SavedSessions {
    cells: Mutex<[Option<Arc<Cell>>; MAX_SAVED_SESSIONS]>,
    live: Arc<AtomicUsize>, budget: ResourceBudget,
}
impl SavedSessions {
    pub(super) fn new() -> Self {
        Self { cells: Mutex::new(std::array::from_fn(|_| None)), live: Arc::new(AtomicUsize::new(0)),
            budget: ResourceBudget::new(ArenaOwnerId::new(1).expect("nonzero registry owner"),
                ByteLength::new(64 * 1024)).expect("constant registry admission") }
    }
    /// Reserve a cancelable empty slot before any archive I/O. Handles share
    /// the global allocator with atlas/readers and cannot alias another kind.
    pub(super) fn create(&self) -> Result<u64, AccessError> {
        let mut cells = lock(&self.cells)?;
        let position = cells.iter().position(Option::is_none).ok_or(AccessError::Capacity)?;
        if self.live.load(Ordering::Acquire) >= MAX_SAVED_SESSIONS { return Err(AccessError::Capacity); }
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
    pub(super) fn open(&self, handle: u64, path: &Path, limits: SnapshotLimits,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        if state.is_some() { return Err(AccessError::AlreadyOpen); }
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = (|| {
            let owner = ArenaOwnerId::new(handle).map_err(|_| AccessError::InvalidArgument)?;
            let mut candidate = SavedRepositorySession::open(owner, path, limits, &mut stop)?;
            let response = candidate.info(&mut stop)?;
            if stop() { return Err(AccessError::Canceled); }
            *state = Some(candidate);
            Ok(response)
        })();
        cell.validate(epoch)?;
        result
    }
    pub(super) fn execute(&self, handle: u64, command: Command<'_>,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result: Result<HostResponse, AccessError> = match command {
            Command::ExpressionSearch { generation, query, max_hits, max_scan_bytes } =>
                session.expression_search(generation, query, max_hits, max_scan_bytes, &mut stop).map_err(AccessError::from),
            Command::ExpressionSearchCounted { generation, query, max_hits, max_scan_bytes } =>
                session.expression_search_counted(generation, query, max_hits, max_scan_bytes, &mut stop).map_err(AccessError::from),
            Command::ExpressionSearchMatching { generation, query, max_hits, max_scan_bytes, case_sensitive, count_all_matches } =>
                session.expression_search_matching(generation, query, max_hits, max_scan_bytes,
                    case_sensitive, count_all_matches, &mut stop).map_err(AccessError::from),
            Command::ExpressionResults { generation, start, limit } =>
                session.expression_results(generation, start, limit, &mut stop).map_err(AccessError::from),
            Command::ClearExpression { generation } =>
                session.clear_expression(generation, &mut stop).map_err(AccessError::from),
            Command::StructureSearch { generation, mode, name_mode, needle, max_hits, max_source_bytes } =>
                session.structure_search(generation, mode, name_mode, needle, max_hits, max_source_bytes,
                    &mut stop).map_err(AccessError::from),
            Command::StructureResults { generation, start, limit } =>
                session.structure_results(generation, start, limit, &mut stop).map_err(AccessError::from),
            Command::ClearStructure { generation } =>
                session.clear_structure(generation, &mut stop).map_err(AccessError::from),
            Command::PrepareRelationships { generation, max_links, max_source_bytes } =>
                session.prepare_relationships(generation, max_links, max_source_bytes, &mut stop).map_err(AccessError::from),
            Command::Relationships { generation, member, direction, start, limit } =>
                session.relationship_page(generation, member, direction, start, limit, &mut stop).map_err(AccessError::from),
            Command::ClearRelationships { generation } =>
                session.clear_relationships(generation, &mut stop).map_err(AccessError::from),
            Command::PrepareImports { generation, max_links, max_source_bytes } =>
                session.prepare_imports(generation, max_links, max_source_bytes, &mut stop).map_err(AccessError::from),
            Command::Imports { generation, member, direction, start, limit } =>
                session.import_page(generation, member, direction, start, limit, &mut stop).map_err(AccessError::from),
            Command::ClearImports { generation } =>
                session.clear_imports(generation, &mut stop).map_err(AccessError::from),
            Command::PathSearch { generation, needle, options } =>
                session.path_search(generation, needle, options, &mut stop).map_err(AccessError::from),
            Command::PathResults { generation, start, limit } =>
                session.path_results(generation, start, limit, &mut stop).map_err(AccessError::from),
            Command::ClearPaths { generation } =>
                session.clear_paths(generation, &mut stop).map_err(AccessError::from),
            Command::Info => session.info(&mut stop).map_err(AccessError::from),
            Command::IndexBuildReceipt { operation } =>
                session.index_build_receipt(operation, &mut stop).map_err(AccessError::from),
            Command::Members { start, limit } => session.members(start, limit, &mut stop).map_err(AccessError::from),
            Command::AttachIndex { generation, path, pin } => session.attach_index(generation, path, pin, &mut stop).map_err(AccessError::from),
            Command::DetachIndex { generation } => session.detach_index(generation, &mut stop).map_err(AccessError::from),
            Command::Search { generation, needle, limit } => session.search(generation, needle, limit, &mut stop).map_err(AccessError::from),
            Command::SearchCounted { generation, needle, limit } => session.search_counted(generation, needle, limit, &mut stop).map_err(AccessError::from),
            Command::SearchMatching { generation, needle, limit, case_sensitive, count_all_matches } =>
                session.search_matching(generation, needle, limit, case_sensitive, count_all_matches,
                    &mut stop).map_err(AccessError::from),
            Command::Results { generation, start, limit } => session.results(generation, start, limit, &mut stop).map_err(AccessError::from),
            Command::Clear { generation } => session.clear_results(generation, &mut stop).map_err(AccessError::from),
        };
        // Acceptance precedes delivery. Late cancellation can suppress output,
        // not roll back state. Info/results reconcile the known handle.
        cell.validate(epoch)?;
        result
    }
    /// A build can create a destination before cancellation/close arrives.
    /// The host retains and returns an effect-bearing terminal receipt. Never
    /// replace that receipt with a late handle error after the writer drains.
    /// This cell/lock also retains the archive and capacity through that drain.
    pub(super) fn build_index(&self, handle: u64, operation: u64,
        destination: &Path, limits: SavedIndexBuildLimits,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        session.build_index(operation, destination, limits, &mut stop).map_err(AccessError::from)
    }
    /// Lock saved source before destination, both nonblocking. The existing
    /// reader registry admits an empty handle BEFORE any member read occurs.
    /// Reader cancellation does not change the shared saved session's epoch.
    pub(super) fn open_reader(&self, handle: u64, readers: &ReaderSessions,
        reader: u64, selection: Selection, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = readers.initialize_prepared(reader, |owner, reader_stop| {
            match selection {
                Selection::Hit { generation, id } => session.open_hit_reader(owner, generation, id, reader_stop).map_err(AccessError::from),
                Selection::ExpressionHit { generation, id } =>
                    session.open_expression_hit_reader(owner, generation, id, reader_stop).map_err(AccessError::from),
                Selection::StructureHit { generation, id } =>
                    session.open_structure_hit_reader(owner, generation, id, reader_stop).map_err(AccessError::from),
                Selection::Member(ordinal) => session.open_member_reader(owner, ordinal, reader_stop).map_err(AccessError::from),
                Selection::Relationship { generation, id, side } =>
                    session.open_relationship_reader(owner, generation, id, side, reader_stop).map_err(AccessError::from),
                Selection::Import { generation, id, side } =>
                    session.open_import_reader(owner, generation, id, side, reader_stop).map_err(AccessError::from),
                Selection::PathHit { generation, id } =>
                    session.open_path_hit_reader(owner, generation, id, reader_stop).map_err(AccessError::from),
            }
        }, &mut stop);
        cell.validate(epoch)?;
        result
    }
    /// Existing reader destination: image acquisition may mutate only its
    /// bounded image cache. Closing/canceling either service prevents delivery;
    /// the source and already retained image bytes have independent lifetimes.
    pub(super) fn document_images(&self, handle: u64, readers: &ReaderSessions,
        reader: u64, source_member: usize, generation: u64, command: DocumentImageCommand,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AccessError> {
        let cell = self.get(handle)?;
        let epoch = cell.epoch.load(Ordering::Acquire);
        let mut state = lock(&cell.state)?;
        cell.validate(epoch)?;
        let session = state.as_mut().ok_or(AccessError::NotOpen)?;
        let mut stop = || cell.validate(epoch).is_err() || canceled();
        let result = readers.with_document_images(reader, |reader, reader_stop| match command {
            DocumentImageCommand::Authorize =>
                session.authorize_document_images(reader, source_member, generation, reader_stop).map_err(AccessError::from),
            DocumentImageCommand::Prepare { item_index, request_id } =>
                session.prepare_document_image(reader, source_member, generation, item_index,
                    request_id, reader_stop).map_err(AccessError::from),
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
        drop(removed); // Outside table lock; in-flight calls keep the permit.
        Ok(())
    }
}
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AccessError> {
    mutex.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => AccessError::Busy, TryLockError::Poisoned(_) => AccessError::Poisoned,
    })
}

#[cfg(all(test, unix))]
#[path = "saved_sessions_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "saved_relationships_sessions_tests.rs"]
mod relationships_tests;

#[cfg(all(test, unix))]
#[path = "saved_imports_sessions_tests.rs"]
mod imports_tests;

#[cfg(all(test, unix))]
#[path = "saved_expression_sessions_tests.rs"]
mod expression_tests;

#[cfg(all(test, unix))]
#[path = "saved_structure_sessions_tests.rs"]
mod structure_tests;

#[cfg(all(test, unix))]
#[path = "saved_document_images_sessions_tests.rs"]
mod document_images_tests;

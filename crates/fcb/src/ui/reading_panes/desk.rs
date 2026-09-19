#![forbid(unsafe_code)]

//! Source-backed multi-reader navigation (FCB-050 / FCB-041).
//!
//! A pane descriptor is not source retention. This worker-side controller owns
//! the exact captures referenced by panes, back/forward entries and bookmarks.
//! Duplicate panes share source bytes but have independent reading positions.
//! No path is reopened, and no capture is rebound to new bytes. All operations,
//! including retirement/drop, belong on the host worker, not an input callback.
//! The returned model revision is NOT a presented frame: the host still owns
//! rendering, presentation acknowledgment, pointer routing and accessibility.

use std::{mem::size_of, sync::Arc};
use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId,
    ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use crate::{BrowserSession, BrowserView, SourceCapture};
use super::{ReadingPane, ReadingPaneManager, MAX_READING_PATH_BYTES};

pub const MAX_DESK_PANES: usize = 8;
pub const MAX_DESK_SOURCES: usize = 64;
pub const MAX_DESK_HISTORY: usize = 128;
pub const MAX_DESK_BOOKMARKS: usize = 64;
pub const MAX_BOOKMARK_LABEL_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeskLimits {
    pub panes: usize,
    pub sources: usize,
    pub history: usize,
    pub bookmarks: usize,
    pub source_bytes: u64,
    pub retained_bytes: u64,
}
impl Default for DeskLimits {
    fn default() -> Self {
        Self { panes: 8, sources: 32, history: 64, bookmarks: 32,
            source_bytes: 4 * 1024 * 1024, retained_bytes: 32 * 1024 * 1024 }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeskError {
    InvalidLimits, OwnerMismatch, StaleRevision, StaleAttempt, InvalidLocation,
    MissingPane, MissingSource, MissingBookmark, NoActivePane, EmptyHistory,
    PaneLimit, SourceLimit, RetainedByteLimit, BookmarkLimit, InvalidLabel,
    IdentityConflict, IdentityExhausted, ResourceDenied, Canceled,
}
impl DeskError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "DESK_INVALID_LIMITS", Self::OwnerMismatch => "DESK_OWNER_MISMATCH",
            Self::StaleRevision => "DESK_STALE_REVISION", Self::StaleAttempt => "DESK_STALE_ATTEMPT",
            Self::InvalidLocation => "DESK_INVALID_LOCATION", Self::MissingPane => "DESK_MISSING_PANE",
            Self::MissingSource => "DESK_MISSING_SOURCE", Self::MissingBookmark => "DESK_MISSING_BOOKMARK",
            Self::NoActivePane => "DESK_NO_ACTIVE_PANE", Self::EmptyHistory => "DESK_EMPTY_HISTORY",
            Self::PaneLimit => "DESK_PANE_LIMIT", Self::SourceLimit => "DESK_SOURCE_LIMIT",
            Self::RetainedByteLimit => "DESK_RETAINED_BYTE_LIMIT", Self::BookmarkLimit => "DESK_BOOKMARK_LIMIT",
            Self::InvalidLabel => "DESK_INVALID_LABEL", Self::IdentityConflict => "DESK_IDENTITY_CONFLICT",
            Self::IdentityExhausted => "DESK_IDENTITY_EXHAUSTED", Self::ResourceDenied => "DESK_RESOURCE_DENIED",
            Self::Canceled => "DESK_CANCELED",
        }
    }
}
impl std::fmt::Display for DeskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for DeskError {}

/// Qualified by the desk, not just a recycled ordinal. Source IDs remain separate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeskPaneId { owner: ArenaOwnerId, value: u64 }
impl DeskPaneId {
    pub const fn owner(self) -> ArenaOwnerId { self.owner }
    pub const fn get(self) -> u64 { self.value }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeskLocation {
    pub file: FileId,
    pub revision: SourceRevision,
    /// Original-byte viewport anchor, not a visual row or decoded UTF-8 offset.
    pub offset: u64,
    pub selection: Option<ByteRange>,
    pub preferred_pane: DeskPaneId,
}
#[derive(Clone, Debug)]
pub struct DeskBookmark { id: u64, label: String, location: DeskLocation }
impl DeskBookmark {
    pub const fn id(&self) -> u64 { self.id }
    pub fn label(&self) -> &str { &self.label }
    pub const fn location(&self) -> DeskLocation { self.location }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeskChange {
    pub revision: u64,
    pub active: Option<DeskPaneId>,
    pub created_bookmark: Option<u64>,
}

/// Semantic commands. Scrolling updates the current history entry rather than
/// appending every motion tick. Bookmarks are session-owned, not disk persistence.
pub enum DeskCommand {
    Focus(DeskPaneId),
    Navigate { pane: DeskPaneId, offset: u64, selection: Option<ByteRange> },
    Scroll { pane: DeskPaneId, offset: u64 },
    Pin { pane: DeskPaneId, pinned: bool },
    Duplicate(DeskPaneId),
    Close(DeskPaneId),
    Escape,
    Back,
    Forward,
    Bookmark { pane: DeskPaneId, label: String },
    RecallBookmark(u64),
    ForgetBookmark(u64),
    ClearHistory,
    Arrange { pane: DeskPaneId, position: (f32, f32), size: (f32, f32) },
}

struct RetainedSource { capture: SourceCapture, _lease: ResourceLease }
#[derive(Clone)]
struct State {
    revision: u64,
    panes: ReadingPaneManager,
    offsets: Vec<(u64, u64)>,
    sources: Vec<Arc<RetainedSource>>,
    history: Vec<DeskLocation>,
    cursor: Option<usize>,
    bookmarks: Vec<DeskBookmark>,
    next_bookmark: u64,
}

/// A retained source view with its accounting capability. Dropping the desk
/// cannot release the source reservation while this exported view is alive.
/// Explicitly cloning its borrowed BrowserView transfers accounting to the host.
pub struct DeskView { view: BrowserView, _lease: ResourceLease, _view_lease: ResourceLease }
impl DeskView {
    pub fn view(&self) -> &BrowserView { &self.view }
    pub fn source(&self) -> &SourceCapture { self.view.source() }
}

/// Construction is inert apart from explicitly admitted memory. The supplied
/// budget also accounts for retained sources; source allocations are shared
/// across panes/history, not copied for each view. The source's external host
/// may have its own overlapping reservation; this retention charge is explicit.
pub struct ReadingDesk {
    owner: ArenaOwnerId,
    limits: DeskLimits,
    state: State,
    last_attempt: u64,
    // Never reset on eviction; restored captures must not alias an older view.
    source_high_water: u64,
    budget: ResourceBudget,
    _lease: ResourceLease,
}
impl ReadingDesk {
    pub fn new(owner: ArenaOwnerId, limits: DeskLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<Self, DeskError> {
        if !(1..=MAX_DESK_PANES).contains(&limits.panes)
            || !(1..=MAX_DESK_SOURCES).contains(&limits.sources)
            || !(1..=MAX_DESK_HISTORY).contains(&limits.history)
            || limits.bookmarks > MAX_DESK_BOOKMARKS
            || limits.source_bytes > 64 * 1024 * 1024
            || limits.retained_bytes > 256 * 1024 * 1024 {
            return Err(DeskError::InvalidLimits);
        }
        // Three bounded descriptor generations plus allocation/label slack.
        // Captures live behind Arcs, so staging a transaction never clones bytes.
        let charge = limits.panes * (size_of::<ReadingPane>() + MAX_READING_PATH_BYTES + 128)
            + limits.sources * (size_of::<Arc<RetainedSource>>() + 128)
            + limits.history * (size_of::<DeskLocation>() + 32)
            + limits.bookmarks * (size_of::<DeskBookmark>() + MAX_BOOKMARK_LABEL_BYTES + 64);
        let lease = budget.try_reserve_managed(owner, allocation,
            ByteLength::new((charge * 3 + size_of::<Self>() + 4096) as u64))
            .map_err(|_| DeskError::ResourceDenied)?;
        let state = State { revision: 0, panes: ReadingPaneManager::new(), offsets: Vec::new(),
            sources: Vec::new(), history: Vec::new(), cursor: None, bookmarks: Vec::new(), next_bookmark: 1 };
        let mut desk = Self { owner, limits, state, last_attempt: 0, source_high_water: 0,
            budget: budget.clone(), _lease: lease };
        desk.reserve_state()?;
        Ok(desk)
    }
    pub const fn owner(&self) -> ArenaOwnerId { self.owner }
    pub const fn limits(&self) -> DeskLimits { self.limits }
    pub fn revision(&self) -> u64 { self.state.revision }
    pub const fn last_attempt(&self) -> u64 { self.last_attempt }
    pub fn panes(&self) -> &ReadingPaneManager { &self.state.panes }
    pub fn active(&self) -> Option<DeskPaneId> { self.state.panes.active_pane_id().map(|value| self.id(value)) }
    pub fn bookmarks(&self) -> &[DeskBookmark] { &self.state.bookmarks }
    pub fn history(&self) -> &[DeskLocation] { &self.state.history }
    pub fn history_cursor(&self) -> Option<usize> { self.state.cursor }
    pub fn can_go_back(&self) -> bool { self.state.cursor.is_some_and(|i| i > 0) }
    pub fn can_go_forward(&self) -> bool { self.state.cursor.is_some_and(|i| i + 1 < self.state.history.len()) }
    pub fn retained_source_count(&self) -> usize { self.state.sources.len() }
    pub fn retained_source_bytes(&self) -> u64 {
        self.state.sources.iter().map(|s| s.capture.bytes().len() as u64).sum()
    }
    /// Resolve a wire ordinal only within THIS desk's live model.
    pub fn pane_id(&self, ordinal: u64) -> Result<DeskPaneId, DeskError> {
        self.state.panes.get_pane(ordinal).ok_or(DeskError::MissingPane)?;
        Ok(self.id(ordinal))
    }
    pub fn location(&self, pane: DeskPaneId, expected: u64) -> Result<DeskLocation, DeskError> {
        self.validate(expected)?;
        location(&self.state, self.owner, pane)
    }
    pub fn source(&self, pane: DeskPaneId, expected: u64) -> Result<&SourceCapture, DeskError> {
        let at = self.location(pane, expected)?;
        source(&self.state, at.file, at.revision)
    }
    /// External hosts use the ordinary SourceReader/search/document APIs on this
    /// exact view. The clone shares bytes; its ownership can outlive this desk.
    pub fn view(&self, pane: DeskPaneId, expected: u64,
        allocation: ResourceAllocationId) -> Result<DeskView, DeskError> {
        let at = self.location(pane, expected)?;
        let retained = self.state.sources.iter().find(|s| s.capture.file() == at.file
            && s.capture.revision() == at.revision).ok_or(DeskError::MissingSource)?;
        let charge = retained.capture.logical_path().len() + size_of::<DeskView>() + 128;
        let view_lease = self.budget.try_reserve_managed(self.owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| DeskError::ResourceDenied)?;
        let view = BrowserSession::new(self.owner).open_capture(retained.capture.clone())
            .map_err(|_| DeskError::OwnerMismatch)?;
        Ok(DeskView { view, _lease: retained._lease.clone(), _view_lease: view_lease })
    }
    /// Cheap borrowed copy. Materializing a clipboard/export needs the caller's
    /// separate budget and explicit action. No line-ending or byte rewriting.
    pub fn selected_bytes(&self, pane: DeskPaneId, expected: u64) -> Result<&[u8], DeskError> {
        let at = self.location(pane, expected)?;
        let selected = at.selection.ok_or(DeskError::InvalidLocation)?;
        let (start, end) = selected.as_usize_bounds().map_err(|_| DeskError::InvalidLocation)?;
        self.source(pane, expected)?.bytes().get(start..end).ok_or(DeskError::InvalidLocation)
    }

    /// Open a host-supplied capture. Same identity + changed bytes is an error;
    /// a NEW revision may coexist with old pinned panes/history/bookmarks.
    /// The source allocation ID must be fresh in the supplied budget ledger.
    pub fn open(&mut self, expected: u64, attempt: u64, capture: SourceCapture,
        offset: u64, selection: Option<ByteRange>, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskError> {
        let mut next = self.begin(expected, attempt, &mut canceled)?;
        if capture.owner() != self.owner { return Err(DeskError::OwnerMismatch); }
        validate_location(&capture, offset, selection)?;
        if capture.logical_path().len() > MAX_READING_PATH_BYTES { return Err(DeskError::InvalidLocation); }
        self.source_high_water = self.source_high_water.max(capture.file().get()).max(capture.revision().get());
        if let Some(old) = next.sources.iter().find(|s| s.capture.file() == capture.file()
            && s.capture.revision() == capture.revision()) {
            if old.capture.bytes() != capture.bytes() { return Err(DeskError::IdentityConflict); }
        } else {
            if next.sources.len() == self.limits.sources { return Err(DeskError::SourceLimit); }
            let bytes = capture.bytes().len() as u64;
            if bytes > self.limits.source_bytes { return Err(DeskError::SourceLimit); }
            // Admit the old/new overlap, before history truncation or retirement.
            if bytes > self.limits.retained_bytes.saturating_sub(self.retained_source_bytes()) {
                return Err(DeskError::RetainedByteLimit);
            }
            let charge = bytes.checked_add((capture.logical_path().len() * 2 + size_of::<RetainedSource>() + 128) as u64)
                .ok_or(DeskError::ResourceDenied)?;
            let lease = self.budget.try_reserve_managed(self.owner, allocation, ByteLength::new(charge))
                .map_err(|_| DeskError::ResourceDenied)?;
            let retained = RetainedSource { capture: capture.clone(), _lease: lease };
            next.sources.push(Arc::new(retained));
        }
        let hint = next.panes.active_pane_id().map(|value| self.id(value)).unwrap_or(self.id(0));
        let at = DeskLocation { file: capture.file(), revision: capture.revision(), offset, selection, preferred_pane: hint };
        let at = open_location(&mut next, self.owner, self.limits, at, false)?;
        record(&mut next, self.limits.history, at);
        self.publish(next, attempt, None, &mut canceled)
    }

    /// Transactional semantic navigation. Failed/stale/canceled commands leave
    /// panes, source retention, cursor and bookmarks unchanged. An admitted
    /// attempt number is consumed even when its later work fails.
    pub fn apply(&mut self, expected: u64, attempt: u64, command: DeskCommand,
        mut canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskError> {
        let mut next = self.begin(expected, attempt, &mut canceled)?;
        let mut created = None;
        match command {
            DeskCommand::Focus(pane) => {
                let at = location(&next, self.owner, pane)?;
                next.panes.set_active_pane_id(Some(pane.value));
                record(&mut next, self.limits.history, at);
            }
            DeskCommand::Navigate { pane, offset, selection } => {
                let mut at = location(&next, self.owner, pane)?;
                validate_location(source(&next, at.file, at.revision)?, offset, selection)?;
                at.offset = offset; at.selection = selection;
                let at = open_location(&mut next, self.owner, self.limits, at, false)?;
                record(&mut next, self.limits.history, at);
            }
            DeskCommand::Scroll { pane, offset } => {
                let mut at = location(&next, self.owner, pane)?;
                validate_location(source(&next, at.file, at.revision)?, offset, at.selection)?;
                set_offset(&mut next, pane.value, offset)?;
                at.offset = offset;
                if let Some(i) = next.cursor {
                    if next.history[i].preferred_pane == pane { next.history[i] = at; }
                }
            }
            DeskCommand::Pin { pane, pinned } => {
                location(&next, self.owner, pane)?;
                next.panes.get_pane_mut(pane.value).ok_or(DeskError::MissingPane)?.is_pinned = pinned;
            }
            DeskCommand::Duplicate(pane) => {
                let at = location(&next, self.owner, pane)?;
                let at = open_location(&mut next, self.owner, self.limits, at, true)?;
                next.panes.pin_pane(at.preferred_pane.value);
                record(&mut next, self.limits.history, at);
            }
            DeskCommand::Close(pane) => {
                location(&next, self.owner, pane)?;
                next.panes.close_pane(pane.value);
                next.offsets.retain(|(id, _)| *id != pane.value);
            }
            DeskCommand::Escape => {
                let removed = next.panes.close_active_or_top_unpinned().ok_or(DeskError::NoActivePane)?;
                next.offsets.retain(|(id, _)| *id != removed);
            }
            DeskCommand::Back | DeskCommand::Forward => {
                let cursor = next.cursor.ok_or(DeskError::EmptyHistory)?;
                let target = if matches!(command, DeskCommand::Back) { cursor.checked_sub(1) }
                    else { cursor.checked_add(1).filter(|&i| i < next.history.len()) }.ok_or(DeskError::EmptyHistory)?;
                let at = next.history[target];
                let at = open_location(&mut next, self.owner, self.limits, at, false)?;
                next.history[target] = at;
                next.cursor = Some(target);
            }
            DeskCommand::Bookmark { pane, label } => {
                let at = location(&next, self.owner, pane)?;
                if label.is_empty() || label.len() > MAX_BOOKMARK_LABEL_BYTES { return Err(DeskError::InvalidLabel); }
                if next.bookmarks.len() == self.limits.bookmarks { return Err(DeskError::BookmarkLimit); }
                let id = next.next_bookmark;
                next.next_bookmark = id.checked_add(1).ok_or(DeskError::IdentityExhausted)?;
                next.bookmarks.push(DeskBookmark { id, label, location: at });
                created = Some(id);
            }
            DeskCommand::RecallBookmark(id) => {
                let at = next.bookmarks.iter().find(|b| b.id == id).ok_or(DeskError::MissingBookmark)?.location;
                let at = open_location(&mut next, self.owner, self.limits, at, false)?;
                record(&mut next, self.limits.history, at);
            }
            DeskCommand::ForgetBookmark(id) => {
                let at = next.bookmarks.iter().position(|b| b.id == id).ok_or(DeskError::MissingBookmark)?;
                next.bookmarks.remove(at);
            }
            DeskCommand::ClearHistory => {
                next.history.clear(); next.cursor = None;
                if let Some(value) = next.panes.active_pane_id() {
                    let at = location(&next, self.owner, self.id(value))?;
                    record(&mut next, self.limits.history, at);
                }
            }
            DeskCommand::Arrange { pane, position, size } => {
                location(&next, self.owner, pane)?;
                if ![position.0, position.1, size.0, size.1].iter().all(|n| n.is_finite())
                    || position.0 < 0.0 || position.1 < 0.0 || size.0 <= 0.0 || size.1 <= 0.0 {
                    return Err(DeskError::InvalidLocation);
                }
                let target = next.panes.get_pane_mut(pane.value).ok_or(DeskError::MissingPane)?;
                target.position = position; target.size = size;
            }
        }
        self.publish(next, attempt, created, &mut canceled)
    }
    fn id(&self, value: u64) -> DeskPaneId { DeskPaneId { owner: self.owner, value } }
    fn validate(&self, expected: u64) -> Result<(), DeskError> {
        if expected != self.revision() { Err(DeskError::StaleRevision) } else { Ok(()) }
    }
    fn reserve_state(&mut self) -> Result<(), DeskError> { reserve_state(&mut self.state, self.limits) }
    fn begin(&mut self, expected: u64, attempt: u64, canceled: &mut impl FnMut() -> bool) -> Result<State, DeskError> {
        self.validate(expected)?;
        if attempt == 0 || attempt <= self.last_attempt { return Err(DeskError::StaleAttempt); }
        self.last_attempt = attempt;
        check(canceled)?;
        let mut next = self.state.clone();
        reserve_state(&mut next, self.limits)?;
        Ok(next)
    }
    fn publish(&mut self, mut next: State, attempt: u64, created_bookmark: Option<u64>,
        canceled: &mut impl FnMut() -> bool) -> Result<DeskChange, DeskError> {
        // Keep exact bytes for CLOSED panes when history or a bookmark still
        // references them. Retire only truly unreferenced source generations.
        let panes = &next.panes; let history = &next.history; let bookmarks = &next.bookmarks;
        next.sources.retain(|s| {
            let file = s.capture.file(); let rev = s.capture.revision();
            panes.iter().any(|p| p.file_id == file && p.revision == Some(rev))
                || history.iter().any(|h| h.file == file && h.revision == rev)
                || bookmarks.iter().any(|b| b.location.file == file && b.location.revision == rev)
        });
        check(canceled)?;
        next.revision = attempt;
        self.state = next;
        Ok(DeskChange { revision: attempt, active: self.active(), created_bookmark })
    }
}

fn reserve<T>(values: &mut Vec<T>, limit: usize) -> Result<(), DeskError> {
    if values.len() > limit { return Err(DeskError::ResourceDenied); }
    values.try_reserve_exact(limit - values.len()).map_err(|_| DeskError::ResourceDenied)?;
    if values.capacity() > limit { return Err(DeskError::ResourceDenied); }
    Ok(())
}
fn reserve_state(state: &mut State, limits: DeskLimits) -> Result<(), DeskError> {
    reserve(&mut state.panes.panes, limits.panes)?;
    reserve(&mut state.offsets, limits.panes)?;
    reserve(&mut state.sources, limits.sources)?;
    reserve(&mut state.history, limits.history)?;
    reserve(&mut state.bookmarks, limits.bookmarks)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), DeskError> {
    if canceled() { Err(DeskError::Canceled) } else { Ok(()) }
}
fn source(state: &State, file: FileId, revision: SourceRevision) -> Result<&SourceCapture, DeskError> {
    state.sources.iter().find(|s| s.capture.file() == file && s.capture.revision() == revision)
        .map(|s| &s.capture).ok_or(DeskError::MissingSource)
}
fn validate_location(source: &SourceCapture, offset: u64, selection: Option<ByteRange>) -> Result<(), DeskError> {
    let length = source.bytes().len() as u64;
    if offset > length || selection.is_some_and(|r| r.end().get() > length) { Err(DeskError::InvalidLocation) } else { Ok(()) }
}
fn location(state: &State, owner: ArenaOwnerId, id: DeskPaneId) -> Result<DeskLocation, DeskError> {
    if id.owner != owner { return Err(DeskError::OwnerMismatch); }
    let pane = state.panes.get_pane(id.value).ok_or(DeskError::MissingPane)?;
    let offset = state.offsets.iter().find(|(p, _)| *p == id.value).ok_or(DeskError::MissingPane)?.1;
    let selection = pane.selection.map(|(a, b)| ByteRange::new(ByteOffset::new(a as u64), ByteOffset::new(b as u64))
        .map_err(|_| DeskError::InvalidLocation)).transpose()?;
    Ok(DeskLocation { file: pane.file_id, revision: pane.revision.ok_or(DeskError::MissingSource)?, offset,
        selection, preferred_pane: id })
}
fn set_offset(state: &mut State, pane: u64, offset: u64) -> Result<(), DeskError> {
    state.offsets.iter_mut().find(|(id, _)| *id == pane).ok_or(DeskError::MissingPane)?.1 = offset;
    Ok(())
}
fn open_location(state: &mut State, owner: ArenaOwnerId, limits: DeskLimits,
    mut at: DeskLocation, duplicate: bool) -> Result<DeskLocation, DeskError> {
    let capture = source(state, at.file, at.revision)?;
    validate_location(capture, at.offset, at.selection)?;
    let path = capture.logical_path().to_owned();
    let selection = at.selection.map(|r| r.as_usize_bounds().map_err(|_| DeskError::InvalidLocation)).transpose()?;
    let preferred = if duplicate { None } else {
        state.panes.get_pane(at.preferred_pane.value).filter(|p| !p.is_pinned
            || (p.file_id == at.file && p.revision == Some(at.revision))).map(|p| p.id)
            .or_else(|| state.panes.active_pane().filter(|p| !p.is_pinned).map(|p| p.id))
            .or_else(|| state.panes.iter().find(|p| !p.is_pinned).map(|p| p.id))
    };
    let id = if let Some(id) = preferred { id } else {
        if state.panes.len() == limits.panes { return Err(DeskError::PaneLimit); }
        let id = state.panes.next_id;
        state.panes.next_id = id.checked_add(1).filter(|_| id != 0).ok_or(DeskError::IdentityExhausted)?;
        let shift = state.panes.len() as f32 * 30.0;
        state.panes.panes.push(ReadingPane::new(id, at.file, String::new(), (80.0 + shift, 60.0 + shift)));
        state.offsets.push((id, at.offset));
        id
    };
    let pane = state.panes.get_pane_mut(id).ok_or(DeskError::MissingPane)?;
    pane.file_id = at.file; pane.revision = Some(at.revision); pane.path = path;
    pane.selection = selection; pane.target_line = None; // Derived by the exact reader, never guessed.
    pane.scroll_offset = (0.0, 0.0);
    set_offset(state, id, at.offset)?;
    state.panes.set_active_pane_id(Some(id));
    at.preferred_pane = DeskPaneId { owner, value: id };
    Ok(at)
}
fn record(state: &mut State, limit: usize, at: DeskLocation) {
    if let Some(i) = state.cursor {
        if state.history[i] == at { return; }
        state.history.truncate(i + 1);
    }
    if state.history.len() == limit { state.history.remove(0); }
    state.history.push(at); state.cursor = Some(state.history.len() - 1);
}

#[cfg(test)]
mod tests;

/// Explicit source-bearing checkpoint export and transactional restore.
#[cfg(feature = "snapshot")]
pub mod checkpoint;

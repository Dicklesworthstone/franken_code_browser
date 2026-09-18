#![forbid(unsafe_code)]

//! Capture-qualified result activation (FCB-029.A).
//!
//! On a worker, resolve an IndexedSearchReport hit using SourceReader::seek_hit
//! in bounded steps, then prepare CapturedSearchActivation from its anchor.
//! At delivery, activate_captured_search_result checks the PUBLISHED query and
//! selected occurrence again before changing any UI state. A pending draft is
//! deliberately not the identity of the still-visible results.
//!
//! Hosts keep the activation's source capture alive while displaying the pane.
//! The pane stores a revision/range descriptor, not a duplicate source buffer.
//! Scope/capture replacements require a fresh query generation; clear the old
//! result scope before publishing another universe. No native presentation,
//! grant validation, global source registry, or worker runtime is invented here.

use crate::SourceCapture;
use crate::search::{IndexedQueryState, IndexedSearchReport, ReaderError, ReadingAnchor};
use fcb_core::QueryGeneration;
use super::{FocusTarget, UiAction, UiCommand, UiEvent, UiEventKind, UiReducer, UiReductionOutcome, UiState};
use super::reading_panes::{ReadingPaneOpenError, MAX_READING_PATH_BYTES};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchActivationError {
    InvalidReport,
    MissingHit,
    StaleQuery,
    StaleSelection,
    InvalidTarget,
    ResourceDenied,
    Reader(ReaderError),
    Pane(ReadingPaneOpenError),
}
impl From<ReaderError> for SearchActivationError {
    fn from(error: ReaderError) -> Self { Self::Reader(error) }
}
impl From<ReadingPaneOpenError> for SearchActivationError {
    fn from(error: ReadingPaneOpenError) -> Self { Self::Pane(error) }
}
impl std::fmt::Display for SearchActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reader(error) => write!(f, "{error}"),
            other => write!(f, "SEARCH_ACTIVATION_{other:?}"),
        }
    }
}
impl std::error::Error for SearchActivationError {}

/// A private, validated occurrence/anchor pair borrowing the searched source.
/// Construction never seeks, reads, decodes or copies source bytes. Only an
/// engine-produced ReadingAnchor can establish a resolved source location.
pub struct CapturedSearchActivation<'source> {
    source: &'source SourceCapture,
    anchor: ReadingAnchor,
    occurrence_id: u64,
}
impl<'source> CapturedSearchActivation<'source> {
    pub fn from_report(source: &'source SourceCapture, report: &IndexedSearchReport<'_>,
        hit_index: usize, anchor: ReadingAnchor) -> Result<Self, SearchActivationError> {
        if report.state() != IndexedQueryState::Finished || report.failure().is_some() {
            return Err(SearchActivationError::InvalidReport);
        }
        if anchor.generation() != report.generation() { return Err(SearchActivationError::StaleQuery); }
        let hit = report.capture_results().matches.get(hit_index).ok_or(SearchActivationError::MissingHit)?;
        anchor.validate_delivery(source, report.generation())?;
        if hit.file_id != source.file() || hit.revision != source.revision()
            || anchor.selection() != Some(hit.original_byte_range)
            || hit.original_byte_range.is_empty()
            || hit.original_byte_range.end().get() > source.bytes().len() as u64 {
            return Err(SearchActivationError::InvalidTarget);
        }
        Ok(Self { source, anchor, occurrence_id: hit.occurrence_id })
    }
    pub fn source(&self) -> &'source SourceCapture { self.source }
    pub const fn anchor(&self) -> ReadingAnchor { self.anchor }
    pub const fn generation(&self) -> QueryGeneration { self.anchor.generation() }
    pub const fn occurrence_id(&self) -> u64 { self.occurrence_id }
}

impl UiReducer {
    /// Publish a previously resolved exact content hit. Unlike legacy path
    /// activation, this NEVER emits RequestCapture or uses a path as authority.
    /// Failed freshness/admission checks leave selection, focus and panes intact.
    /// Retain `target.source()` in the native host for the returned pane's life.
    pub fn activate_captured_search_result(state: &mut UiState,
        target: &CapturedSearchActivation<'_>, now_nanos: u64)
        -> Result<UiReductionOutcome, SearchActivationError> {
        if state.owner != target.source.owner() { return Err(SearchActivationError::InvalidTarget); }
        if state.sidebar.results.query_generation != Some(target.generation()) {
            return Err(SearchActivationError::StaleQuery);
        }
        let entry = state.sidebar.results.selected_result().ok_or(SearchActivationError::StaleSelection)?;
        let range = target.anchor.selection().ok_or(SearchActivationError::InvalidTarget)?;
        if entry.id != target.occurrence_id || entry.file_id != target.source.file()
            || entry.byte_range != (range.start().get(), range.end().get()) {
            return Err(SearchActivationError::StaleSelection);
        }
        target.anchor.validate_delivery(target.source, target.generation())?;
        let line = usize::try_from(target.anchor.line_number()).map_err(|_| SearchActivationError::InvalidTarget)?;
        // Prepare bounded labels before the first visible mutation. These are
        // display labels only; all source authority comes from target.source.
        let path = copy_path(&entry.path)?;
        let pane_path = copy_path(&entry.path)?;
        let lens_path = copy_path(&entry.path)?;
        let file_id = target.source.file();
        let revision = target.source.revision();
        state.reading_panes.open_captured(file_id, revision, pane_path, line, range)?;
        if state.sidebar.inspector.file_id != Some(file_id)
            || state.sidebar.inspector.revision != Some(revision) {
            state.sidebar.inspector.clear();
            state.sidebar.outline.clear();
        }
        let mut outcome = Self::reduce(state, UiAction::SelectFile { file_id, path }, now_nanos);
        outcome.commands.retain(|command| !matches!(command,
            UiCommand::RequestCapture { .. } | UiCommand::RequestRedraw));
        state.sidebar.inspector.revision = Some(revision);
        if state.search_palette_open {
            state.search_palette_open = false;
            if state.focus.current() == FocusTarget::SearchPalette { let _ = state.focus.return_focus(); }
        }
        state.reading_lens_open = true;
        state.reading_lens_file = Some((file_id, lens_path, Some(line)));
        let focus = Self::reduce(state, UiAction::FocusChange(FocusTarget::ReadingLens), now_nanos);
        outcome.commands.extend(focus.commands.into_iter().filter(|command| !matches!(command, UiCommand::RequestRedraw)));
        outcome.emitted_events.extend(focus.emitted_events);
        let kind = UiEventKind::LensToggled { open: true };
        state.seq_counter = state.seq_counter.wrapping_add(1);
        state.event_ring.push(UiEvent { seq: state.seq_counter, timestamp_nanos: now_nanos, kind: kind.clone() });
        outcome.emitted_events.push(kind);
        outcome.changed = true;
        outcome.commands.push(UiCommand::RequestRedraw);
        Ok(outcome)
    }
}

fn copy_path(path: &str) -> Result<String, SearchActivationError> {
    if path.is_empty() || path.len() > MAX_READING_PATH_BYTES { return Err(SearchActivationError::InvalidTarget); }
    let mut copy = String::new();
    copy.try_reserve_exact(path.len()).map_err(|_| SearchActivationError::ResourceDenied)?;
    if copy.capacity() > MAX_READING_PATH_BYTES { return Err(SearchActivationError::ResourceDenied); }
    copy.push_str(path);
    Ok(copy)
}

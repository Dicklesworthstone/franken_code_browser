#![forbid(unsafe_code)]
#![cfg(feature = "search")]

//! FCB-029.A: actual index -> exact reader -> UI activation, without a native
//! renderer or provider read. Every resolved anchor comes from SourceReader.

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, FcbError, FileId,
    SourceCapture, SourceProvider, SourceRevision};
use fcb::search::{EphemeralIndex, IndexLimits, IndexedSearchReport, ManifestLimits,
    MembershipState, ParsedQuery, QueryGeneration, QueryOptions, ReaderError,
    ReaderLimits, ReadingAnchor, ReadingSeekState, ResourceAllocationId,
    ResourceBudget, SearchManifest, SearchManifestId, SourceReader};
use fcb::ui::{CapturedSearchActivation, FocusTarget, ReadingPaneOpenError,
    SearchActivationError, SearchResultEntry, UiAction, UiCommand, UiReducer, UiState};
use fcb::ui::reading_panes::MAX_CAPTURED_READING_PANES;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2910).unwrap() }
fn file(id: u64) -> FileId { FileId::new(owner(), id).unwrap() }
fn revision(id: u64) -> SourceRevision { SourceRevision::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }

struct ForbiddenProvider(Arc<AtomicUsize>);
impl SourceProvider for ForbiddenProvider {
    fn capture(&self, _: &str) -> Result<SourceCapture, FcbError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(FcbError::ProviderUnavailable)
    }
}

fn with_fixture(raw: Vec<u8>, run: impl FnOnce(
    &SourceCapture, &IndexedSearchReport<'_>, &SourceReader<'_>, &mut UiState,
)) {
    let calls = Arc::new(AtomicUsize::new(0));
    let session = BrowserSession::with_provider(owner(), Arc::new(ForbiddenProvider(Arc::clone(&calls))));
    let supplied = SourceCapture::from_bytes(owner(), file(1), revision(1), "src/main.rs", raw).unwrap();
    let prepared = session.prepare_search_capture(supplied).unwrap();
    let documents = [prepared.document()];
    let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), &documents,
        &[], MembershipState::Closed, ManifestLimits::default()).unwrap();
    let budget = ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap();
    let index = EphemeralIndex::build(manifest, IndexLimits::default(), &budget, allocation(1), || false).unwrap();
    let mut state = UiState::new(owner());
    UiReducer::reduce(&mut state, UiAction::SearchQueryChanged("needle".into()), 0);
    let generation = state.active_query_generation.unwrap();
    let report = index.search(&ParsedQuery::parse("needle").unwrap(),
        QueryOptions::new(generation).with_max_matches(16), &budget, allocation(2), || false).unwrap();
    let rows = report.capture_results().matches.iter().map(|hit| SearchResultEntry {
        id: hit.occurrence_id, file_id: hit.file_id, path: "src/main.rs".into(),
        // Deliberately wrong: exact activation must use the resolved anchor.
        line_number: 9999,
        byte_range: (hit.original_byte_range.start().get(), hit.original_byte_range.end().get()),
        excerpt: hit.matched_text.clone(),
    }).collect();
    UiReducer::reduce(&mut state, UiAction::SearchResultsReceived {
        query_generation: generation, results: rows, has_more: !report.is_complete(),
    }, 0);
    let reader = SourceReader::new(prepared.source(), ReaderLimits::default(), &budget, allocation(3)).unwrap();
    run(prepared.source(), &report, &reader, &mut state);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "exact activation must not reopen a path");
}

fn anchor(reader: &SourceReader<'_>, report: &IndexedSearchReport<'_>, position: usize) -> ReadingAnchor {
    let hit = &report.capture_results().matches[position];
    let mut seek = reader.seek_hit(hit, report.generation()).unwrap();
    for _ in 0..20_000 {
        if let ReadingSeekState::Ready(anchor) = seek.state() { return anchor; }
        seek.step(8, report.generation(), || false).unwrap();
        assert!(seek.last_step_bytes() <= 8);
    }
    panic!("bounded fixture seek did not resolve");
}

#[test]
fn utf16_hit_lands_on_original_bytes_and_resolved_line_without_live_capture() {
    let mut raw = vec![0xff, 0xfe];
    for unit in "head\r\n😀 needle\r\nlast".encode_utf16() { raw.extend_from_slice(&unit.to_le_bytes()); }
    with_fixture(raw, |source, report, reader, state| {
        UiReducer::reduce(state, UiAction::FocusChange(FocusTarget::TreeProjection), 0);
        UiReducer::reduce(state, UiAction::OpenSearchPalette, 0);
        let at = anchor(reader, report, 0);
        let target = CapturedSearchActivation::from_report(source, report, 0, at).unwrap();
        assert_eq!(target.source().bytes().as_ptr(), source.bytes().as_ptr());
        let out = UiReducer::activate_captured_search_result(state, &target, 1).unwrap();
        assert!(out.changed);
        assert!(!out.commands.iter().any(|command| matches!(command,
            UiCommand::RequestCapture { .. } | UiCommand::RequestCameraFocus { .. })));
        let pane = state.reading_panes.active_pane().unwrap();
        assert_eq!(pane.revision, Some(revision(1)));
        assert_eq!(pane.target_line, Some(2));
        let bounds = report.capture_results().matches[0].original_byte_range.as_usize_bounds().unwrap();
        assert_eq!(pane.selection, Some(bounds));
        let expected: Vec<u8> = "needle".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(&source.bytes()[bounds.0..bounds.1], expected.as_slice());
        assert_eq!(state.sidebar.inspector.revision, Some(revision(1)));
        UiReducer::reduce(state, UiAction::KeyboardShortcut("Escape".into()), 2);
        assert!(state.reading_panes.is_empty());
        assert_eq!(state.focus.current(), FocusTarget::TreeProjection);
    });
}

#[test]
fn changing_selected_occurrence_rejects_a_late_anchor_without_ui_mutation() {
    with_fixture(b"needle\nneedle".to_vec(), |source, report, reader, state| {
        let target = CapturedSearchActivation::from_report(source, report, 0, anchor(reader, report, 0)).unwrap();
        UiReducer::reduce(state, UiAction::SelectNextSearchResult, 0);
        let before = state.clone();
        assert_eq!(UiReducer::activate_captured_search_result(state, &target, 1), Err(SearchActivationError::StaleSelection));
        assert_eq!(*state, before);
    });
}

#[test]
fn old_published_hit_can_activate_during_refinement_but_not_after_replacement() {
    with_fixture(b"needle".to_vec(), |source, report, reader, state| {
        let target = CapturedSearchActivation::from_report(source, report, 0, anchor(reader, report, 0)).unwrap();
        UiReducer::reduce(state, UiAction::SearchQueryChanged("replacement".into()), 0);
        UiReducer::activate_captured_search_result(state, &target, 1).unwrap();
        let generation = state.active_query_generation.unwrap();
        let rows = state.sidebar.results.results.clone();
        UiReducer::reduce(state, UiAction::SearchResultsReceived { query_generation: generation, results: rows, has_more: false }, 2);
        let before = state.clone();
        assert_eq!(UiReducer::activate_captured_search_result(state, &target, 3), Err(SearchActivationError::StaleQuery));
        assert_eq!(*state, before);
    });
}

#[test]
fn newer_live_revision_cannot_be_substituted_under_an_old_anchor() {
    with_fixture(b"needle".to_vec(), |source, report, reader, state| {
        let at = anchor(reader, report, 0);
        let changed = SourceCapture::from_bytes(owner(), file(1), revision(2), "src/main.rs", b"xxxxxx".to_vec()).unwrap();
        assert!(matches!(CapturedSearchActivation::from_report(&changed, report, 0, at),
            Err(SearchActivationError::Reader(ReaderError::StaleSource))));
        let target = CapturedSearchActivation::from_report(source, report, 0, at).unwrap();
        UiReducer::activate_captured_search_result(state, &target, 1).unwrap();
        assert_eq!(target.source().bytes(), b"needle");
    });
}

#[test]
fn captured_revisions_and_uncaptured_paths_do_not_replace_each_other() {
    with_fixture(b"needle".to_vec(), |source, report, reader, state| {
        let at = anchor(reader, report, 0);
        let range = at.selection().unwrap();
        let other = state.reading_panes.open_captured(file(1), revision(2), "src/main.rs".into(), 1, range).unwrap();
        state.reading_panes.pin_pane(other);
        let target = CapturedSearchActivation::from_report(source, report, 0, at).unwrap();
        UiReducer::activate_captured_search_result(state, &target, 1).unwrap();
        let captured = state.reading_panes.active_pane_id().unwrap();
        assert_ne!(captured, other);
        UiReducer::activate_captured_search_result(state, &target, 2).unwrap();
        assert_eq!(state.reading_panes.active_pane_id(), Some(captured));
        assert_eq!(state.reading_panes.len(), 2);
        let live = state.reading_panes.open_or_focus(file(1), "src/main.rs".into(), None);
        assert_ne!(live, captured);
        assert_ne!(live, other);
        assert_eq!(state.reading_panes.get_pane(other).unwrap().revision, Some(revision(2)));
        assert!(state.reading_panes.get_pane(other).unwrap().is_pinned);
        assert_eq!(state.reading_panes.get_pane(captured).unwrap().revision, Some(revision(1)));
    });
}

#[test]
fn pane_admission_failure_preserves_selection_focus_and_existing_panes() {
    with_fixture(b"needle".to_vec(), |source, report, reader, state| {
        let at = anchor(reader, report, 0);
        for id in 0..MAX_CAPTURED_READING_PANES as u64 {
            state.reading_panes.open_captured(file(id + 100), revision(1), format!("{id}.rs"), 1, at.selection().unwrap()).unwrap();
        }
        let target = CapturedSearchActivation::from_report(source, report, 0, at).unwrap();
        let before = state.clone();
        assert_eq!(UiReducer::activate_captured_search_result(state, &target, 1),
            Err(SearchActivationError::Pane(ReadingPaneOpenError::ResourceDenied)));
        assert_eq!(*state, before);
    });
}

#[test]
fn anchor_for_another_occurrence_or_query_cannot_prepare_a_target() {
    with_fixture(b"needle needle".to_vec(), |source, report, reader, _state| {
        let second = anchor(reader, report, 1);
        assert!(matches!(CapturedSearchActivation::from_report(source, report, 0, second),
            Err(SearchActivationError::InvalidTarget)));
        let wrong_generation = QueryGeneration::new(owner(), report.generation().get() + 1).unwrap();
        let mut seek = reader.seek_hit(&report.capture_results().matches[0], wrong_generation).unwrap();
        let at = match seek.step(8, wrong_generation, || false).unwrap() {
            ReadingSeekState::Ready(at) => at,
            other => panic!("first occurrence not ready: {other:?}"),
        };
        assert!(matches!(CapturedSearchActivation::from_report(source, report, 0, at),
            Err(SearchActivationError::StaleQuery)));
    });
}

#[test]
fn far_hit_uses_resumable_reader_and_empty_scope_invalidates_delivery() {
    let mut raw = vec![b'x'; 32 * 1024];
    raw.extend_from_slice(b"\nneedle");
    with_fixture(raw, |source, report, reader, state| {
        let at = anchor(reader, report, 0);
        assert_eq!(at.line_number(), 2);
        let target = CapturedSearchActivation::from_report(source, report, 0, at).unwrap();
        UiReducer::reduce(state, UiAction::SearchQueryChanged(String::new()), 0);
        let before = state.clone();
        assert_eq!(UiReducer::activate_captured_search_result(state, &target, 1), Err(SearchActivationError::StaleQuery));
        assert_eq!(*state, before);
    });
}

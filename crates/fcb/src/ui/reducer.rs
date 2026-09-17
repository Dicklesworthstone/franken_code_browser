#![forbid(unsafe_code)]

//! Deterministic UI reducer, command routing, and bounded event log ring.
//!
//! Enforces:
//! 1. Zero I/O, parsing, or bulk memory drops on the interaction thread.
//! 2. Strict separation of focus and selection.
//! 3. Predictable focus-return on overlay dismissal.
//! 4. Generation-validated search result publication (stale batches rejected).
//! 5. Bounded event log ring without unbounded memory growth.

use std::collections::VecDeque;
use fcb_core::{ArenaOwnerId, FileId, QueryGeneration};

use super::{
    breadcrumbs::ScopeBreadcrumbs,
    focus::{FocusDirection, FocusManager, FocusTarget},
    gesture::{
        GestureArbitrator, GestureKind, HitRegion, ModifierKeys, PointerButton, ScrollRouting,
    },
    motion_mailbox::{DiscreteInputEvent, MotionMailbox},
    reading_panes::ReadingPaneManager,
    sidebar::{HistoryItem, SearchResultEntry, SidebarPanel, SidebarState},
    tree::{TreeNodeId, TreeProjection},
};

/// Discrete action dispatched to the UI state machine.
#[derive(Clone, Debug, PartialEq)]
pub enum UiAction {
    /// Move focus directly to target.
    FocusChange(FocusTarget),
    /// Advance or retreat focus along the tab ring.
    FocusNavigate(FocusDirection),
    /// Return focus to the previous surface on the stack (e.g. Escape).
    FocusReturn,

    /// Change selection to the specified file (does not change focus).
    SelectFile { file_id: FileId, path: String },
    /// Clear active selection.
    ClearSelection,

    /// Toggle sidebar visibility.
    ToggleSidebar,
    /// Explicitly collapse or expand sidebar.
    SetSidebarCollapsed(bool),
    /// Resize sidebar width.
    SetSidebarWidth(f32),
    /// Switch active sidebar tab.
    SelectSidebarTab(SidebarPanel),

    /// Open source reading lens for a file and optional target line.
    OpenReadingLens {
        file_id: FileId,
        path: String,
        line: Option<usize>,
    },
    /// Close the source reading lens.
    CloseReadingLens,

    /// Open modal search / command palette.
    OpenSearchPalette,
    /// Close modal search / command palette.
    CloseSearchPalette,

    /// Update search query text.
    SearchQueryChanged(String),
    /// Search results received from background index worker.
    SearchResultsReceived {
        query_generation: QueryGeneration,
        results: Vec<SearchResultEntry>,
        has_more: bool,
    },
    /// Move to next search result row.
    SelectNextSearchResult,
    /// Move to previous search result row.
    SelectPrevSearchResult,
    /// Activate current search result (opens file in lens/inspector).
    ActivateSearchResult,

    /// Select a node in the conventional tree projection.
    TreeSelect(TreeNodeId),
    /// Toggle directory expansion in tree.
    TreeToggle(TreeNodeId),
    /// Move selection/focus in tree via arrow keys.
    TreeNavigate(FocusDirection),

    /// Navigate scope breadcrumbs.
    BreadcrumbNavigate(FocusDirection),
    /// Jump directly to breadcrumb segment index.
    BreadcrumbSelect(usize),

    /// Navigate history backward.
    HistoryNavigateBack,
    /// Navigate history forward.
    HistoryNavigateForward,

    /// Fit camera to selection.
    FitSelection,
    /// Fit camera to parent container.
    FitParent,
    /// Fit camera to entire project atlas.
    FitProject,

    /// Pointer pressed down at specified hit region.
    PointerDown {
        region: HitRegion,
        pos: (f32, f32),
        button: PointerButton,
        modifiers: ModifierKeys,
    },
    /// Pointer moved to new position during potential gesture.
    PointerMove { pos: (f32, f32) },
    /// Pointer released.
    PointerUp {
        pos: (f32, f32),
        button: PointerButton,
    },
    /// Scroll or wheel event on a specific hit region.
    ScrollEvent {
        region: HitRegion,
        delta: (f32, f32),
        modifiers: ModifierKeys,
    },
    /// Pinch magnification gesture on a hit region.
    PinchEvent {
        region: HitRegion,
        centroid: (f32, f32),
        magnification: f32,
    },
    /// Explicitly cancel any active gesture (e.g. on blur or Escape).
    CancelGesture,

    /// Promote currently selected file in atlas to an active reading lens pane.
    PromoteSelectedToReadingLens,
    /// Pin a reading pane for side-by-side reading.
    PinReadingPane(u64),
    /// Unpin a reading pane.
    UnpinReadingPane(u64),
    /// Toggle pin state of a reading pane.
    TogglePinReadingPane(u64),
    /// Close a specific reading pane by ID.
    CloseReadingPane(u64),
    /// Set City mode (enables deliberate 3D orbit gestures).
    SetCityMode(bool),

    /// Process a bounded batch from the motion mailbox (coalesced motion + discrete queue).
    ProcessInputBatch { max_discrete: usize },
    /// Enqueue discrete input into motion mailbox.
    EnqueueDiscreteInput(DiscreteInputEvent),
    /// Record continuous motion move tick into mailbox.
    RecordContinuousMove { pos: (f32, f32), delta: (f32, f32) },
    /// Record continuous scroll delta into mailbox.
    RecordContinuousScroll { delta: (f32, f32) },

    /// Generic pointer click on a UI surface.
    PointerClick {
        target: FocusTarget,
        select: bool,
    },
    /// Keyboard shortcut string (e.g. "Escape", "Cmd+P", "Cmd+B").
    KeyboardShortcut(String),
}

/// Outbound commands (intents) emitted for background workers or host renderers.
/// The reducer never executes I/O synchronously; it routes intents via commands.
#[derive(Clone, Debug, PartialEq)]
pub enum UiCommand {
    /// Request background worker to perform search.
    DispatchSearch {
        query: String,
        query_generation: QueryGeneration,
    },
    /// Request background source provider to capture or read a file.
    RequestCapture {
        file_id: FileId,
        path: String,
    },
    /// Request spatial camera movement.
    RequestCameraFocus {
        target_description: String,
    },
    /// Camera pan translation.
    CameraPan { dx: f32, dy: f32 },
    /// Camera zoom factor adjustment.
    CameraZoom { factor: f32 },
    /// Camera orbit rotation in City mode.
    CameraOrbit { dx: f32, dy: f32 },
    /// Floating reading pane translated on screen.
    ReadingPaneMoved { pane_id: u64, pos: (f32, f32) },
    /// Reading pane scrolled internally.
    ReadingPaneScrolled { pane_id: u64, offset: (f32, f32) },
    /// Text selected inside reading pane.
    ReadingPaneSelectedText { pane_id: u64, range: (usize, usize) },
    /// Notify host to schedule a redraw frame.
    RequestRedraw,
}

/// Category of semantic event recorded in the UI event ring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiEventKind {
    FocusChanged {
        from: FocusTarget,
        to: FocusTarget,
    },
    SelectionChanged {
        path: Option<String>,
    },
    SidebarToggled {
        is_collapsed: bool,
    },
    PanelChanged {
        panel: SidebarPanel,
    },
    LensToggled {
        open: bool,
    },
    SearchQueryUpdated {
        query: String,
    },
    SearchResultsUpdated {
        count: usize,
    },
    StaleResultsRejected {
        current_gen: Option<u64>,
        rejected_gen: u64,
    },
    HistoryNavigated {
        path: String,
    },
    CameraFitRequested {
        scope: String,
    },
    GestureStarted {
        kind: GestureKind,
    },
    GestureCompleted {
        kind: GestureKind,
    },
    GestureCancelled,
    ReadingPaneMoved {
        pane_id: u64,
    },
    ReadingPanePinned {
        pane_id: u64,
        is_pinned: bool,
    },
    ReadingPaneClosed {
        pane_id: u64,
    },
    TextSelectedInLens {
        pane_id: u64,
    },
    CityModeToggled {
        is_city_mode: bool,
    },
}

/// A structured event recorded in the bounded event ring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiEvent {
    pub seq: u64,
    pub timestamp_nanos: u64,
    pub kind: UiEventKind,
}

/// Bounded ring buffer of recent UI state transitions.
#[derive(Clone, Debug, PartialEq)]
pub struct UiEventRing {
    events: VecDeque<UiEvent>,
    capacity: usize,
}

impl UiEventRing {
    pub const DEFAULT_CAPACITY: usize = 256;

    pub fn new(capacity: usize) -> Self {
        let cap = if capacity == 0 {
            Self::DEFAULT_CAPACITY
        } else {
            capacity
        };
        Self {
            events: VecDeque::with_capacity(cap),
            capacity: cap,
        }
    }

    pub fn push(&mut self, event: UiEvent) {
        if self.events.len() >= self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn last(&self) -> Option<&UiEvent> {
        self.events.back()
    }

    pub fn iter(&self) -> impl Iterator<Item = &UiEvent> {
        self.events.iter()
    }
}

impl Default for UiEventRing {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

/// Result of reducing an action against the UI state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiReductionOutcome {
    pub commands: Vec<UiCommand>,
    pub emitted_events: Vec<UiEventKind>,
    pub changed: bool,
}

/// Complete authoritative state of the user interface.
#[derive(Clone, Debug, PartialEq)]
pub struct UiState {
    pub owner: ArenaOwnerId,
    pub focus: FocusManager,
    pub selected_file: Option<(FileId, String)>,
    pub reading_lens_open: bool,
    pub reading_lens_file: Option<(FileId, String, Option<usize>)>,
    pub search_palette_open: bool,
    pub sidebar: SidebarState,
    pub breadcrumbs: ScopeBreadcrumbs,
    pub tree: TreeProjection,
    pub active_query_generation: Option<QueryGeneration>,
    pub event_ring: UiEventRing,
    pub seq_counter: u64,
    pub gesture_arbitrator: GestureArbitrator,
    pub motion_mailbox: MotionMailbox,
    pub reading_panes: ReadingPaneManager,
    pub is_city_mode: bool,
}

impl UiState {
    pub fn new(owner: ArenaOwnerId) -> Self {
        Self {
            owner,
            focus: FocusManager::new(FocusTarget::Atlas),
            selected_file: None,
            reading_lens_open: false,
            reading_lens_file: None,
            search_palette_open: false,
            sidebar: SidebarState::new(),
            breadcrumbs: ScopeBreadcrumbs::new(),
            tree: TreeProjection::new(),
            active_query_generation: None,
            event_ring: UiEventRing::default(),
            seq_counter: 0,
            gesture_arbitrator: GestureArbitrator::new(),
            motion_mailbox: MotionMailbox::default(),
            reading_panes: ReadingPaneManager::new(),
            is_city_mode: false,
        }
    }

    fn record_event(&mut self, kind: UiEventKind, timestamp_nanos: u64) {
        self.seq_counter = self.seq_counter.wrapping_add(1);
        self.event_ring.push(UiEvent {
            seq: self.seq_counter,
            timestamp_nanos,
            kind,
        });
    }
}

/// Deterministic state machine reducer for all UI interactions.
pub struct UiReducer;

impl UiReducer {
    /// Reduce an action against `state`, returning outbound commands and logging events.
    pub fn reduce(state: &mut UiState, action: UiAction, now_nanos: u64) -> UiReductionOutcome {
        let mut outcome = UiReductionOutcome::default();

        match action {
            UiAction::FocusChange(target) => {
                let from = state.focus.current();
                if from != target {
                    state.focus.set_focus(target);
                    let event = UiEventKind::FocusChanged { from, to: target };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::FocusNavigate(direction) => {
                let from = state.focus.current();
                let to = state.focus.navigate_tab(direction, state.reading_lens_open);
                if from != to {
                    let event = UiEventKind::FocusChanged { from, to };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::FocusReturn => {
                let from = state.focus.current();
                if let Some(to) = state.focus.return_focus() {
                    let event = UiEventKind::FocusChanged { from, to };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::SelectFile { file_id, path } => {
                // Focus is explicitly NOT stolen by selection!
                state.selected_file = Some((file_id, path.clone()));
                state.sidebar.inspector.file_id = Some(file_id);
                state.sidebar.inspector.file_path = Some(path.clone());
                state.breadcrumbs.set_from_path(&path);

                state.sidebar.history.push(HistoryItem {
                    id: state.seq_counter.wrapping_add(1),
                    path: path.clone(),
                    description: format!("Selected file {path}"),
                    timestamp_nanos: now_nanos,
                });

                let event = UiEventKind::SelectionChanged {
                    path: Some(path.clone()),
                };
                state.record_event(event.clone(), now_nanos);
                outcome.emitted_events.push(event);
                outcome.changed = true;
                outcome.commands.push(UiCommand::RequestCapture {
                    file_id,
                    path,
                });
                outcome.commands.push(UiCommand::RequestRedraw);
            }

            UiAction::ClearSelection => {
                if state.selected_file.is_some() {
                    state.selected_file = None;
                    state.sidebar.inspector.clear();
                    let event = UiEventKind::SelectionChanged { path: None };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::ToggleSidebar => {
                state.sidebar.toggle_collapsed();
                let event = UiEventKind::SidebarToggled {
                    is_collapsed: state.sidebar.is_collapsed,
                };
                state.record_event(event.clone(), now_nanos);
                outcome.emitted_events.push(event);
                outcome.changed = true;
                outcome.commands.push(UiCommand::RequestRedraw);
            }

            UiAction::SetSidebarCollapsed(collapsed) => {
                if state.sidebar.is_collapsed != collapsed {
                    state.sidebar.is_collapsed = collapsed;
                    let event = UiEventKind::SidebarToggled {
                        is_collapsed: collapsed,
                    };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::SetSidebarWidth(width) => {
                let clamped = width.clamp(state.sidebar.min_width, state.sidebar.max_width);
                if (state.sidebar.width - clamped).abs() > f32::EPSILON {
                    state.sidebar.width = clamped;
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::SelectSidebarTab(panel) => {
                if state.sidebar.active_panel != panel {
                    state.sidebar.active_panel = panel;
                    let event = UiEventKind::PanelChanged { panel };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::OpenReadingLens {
                file_id,
                path,
                line,
            } => {
                let _pane_id = state.reading_panes.open_or_focus(file_id, path.clone(), line);
                state.reading_lens_open = true;
                state.reading_lens_file = Some((file_id, path.clone(), line));
                state.focus.set_focus(FocusTarget::ReadingLens);
                let event = UiEventKind::LensToggled { open: true };
                state.record_event(event.clone(), now_nanos);
                outcome.emitted_events.push(event);
                outcome.changed = true;
                outcome.commands.push(UiCommand::RequestCapture {
                    file_id,
                    path,
                });
                outcome.commands.push(UiCommand::RequestRedraw);
            }

            UiAction::CloseReadingLens => {
                if state.reading_panes.is_any_open() {
                    let closed_id = state.reading_panes.close_active_or_top_unpinned();
                    if let Some(pid) = closed_id {
                        let ev = UiEventKind::ReadingPaneClosed { pane_id: pid };
                        state.record_event(ev.clone(), now_nanos);
                        outcome.emitted_events.push(ev);
                    }
                    state.reading_lens_open = state.reading_panes.is_any_open();
                    state.reading_lens_file = state
                        .reading_panes
                        .active_pane()
                        .map(|p| (p.file_id, p.path.clone(), p.target_line));
                    if !state.reading_lens_open {
                        let _ = state.focus.return_focus();
                    }
                    let event = UiEventKind::LensToggled {
                        open: state.reading_lens_open,
                    };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                } else if state.reading_lens_open {
                    state.reading_lens_open = false;
                    state.reading_lens_file = None;
                    let _ = state.focus.return_focus();
                    let event = UiEventKind::LensToggled { open: false };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::OpenSearchPalette => {
                if !state.search_palette_open {
                    state.search_palette_open = true;
                    state.focus.set_focus(FocusTarget::SearchPalette);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::CloseSearchPalette => {
                if state.search_palette_open {
                    state.search_palette_open = false;
                    let _ = state.focus.return_focus();
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::SearchQueryChanged(query) => {
                state.sidebar.results.query = query.clone();
                state.sidebar.results.is_searching = true;
                // Generate a fresh QueryGeneration token for this query attempt
                let gen_val = state.seq_counter.wrapping_add(100);
                if let Ok(qgen) = QueryGeneration::new(state.owner, gen_val) {
                    state.active_query_generation = Some(qgen);
                    state.sidebar.results.query_generation = Some(qgen);
                    outcome.commands.push(UiCommand::DispatchSearch {
                        query: query.clone(),
                        query_generation: qgen,
                    });
                }
                let event = UiEventKind::SearchQueryUpdated { query };
                state.record_event(event.clone(), now_nanos);
                outcome.emitted_events.push(event);
                outcome.changed = true;
                outcome.commands.push(UiCommand::RequestRedraw);
            }

            UiAction::SearchResultsReceived {
                query_generation,
                results,
                has_more,
            } => {
                // Reject stale query results!
                if state.active_query_generation == Some(query_generation) {
                    state.sidebar.results.results = results;
                    state.sidebar.results.has_more = has_more;
                    state.sidebar.results.is_searching = false;
                    state.sidebar.results.selected_index =
                        if state.sidebar.results.results.is_empty() {
                            None
                        } else {
                            Some(0)
                        };

                    let count = state.sidebar.results.results.len();
                    let event = UiEventKind::SearchResultsUpdated { count };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                } else {
                    let event = UiEventKind::StaleResultsRejected {
                        current_gen: state.active_query_generation.map(|g| g.get()),
                        rejected_gen: query_generation.get(),
                    };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                }
            }

            UiAction::SelectNextSearchResult => {
                if state.sidebar.results.select_next().is_some() {
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::SelectPrevSearchResult => {
                if state.sidebar.results.select_prev().is_some() {
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::ActivateSearchResult => {
                if let Some(entry) = state.sidebar.results.selected_result().cloned() {
                    state.search_palette_open = false;
                    if state.focus.current() == FocusTarget::SearchPalette {
                        let _ = state.focus.return_focus();
                    }
                    state.selected_file = Some((entry.file_id, entry.path.clone()));
                    state.reading_lens_open = true;
                    state.reading_lens_file =
                        Some((entry.file_id, entry.path.clone(), Some(entry.line_number)));
                    state.focus.set_focus(FocusTarget::ReadingLens);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestCapture {
                        file_id: entry.file_id,
                        path: entry.path,
                    });
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::TreeSelect(node_id) => {
                if let Some(node) = state.tree.select(node_id).cloned() {
                    if node.is_file() {
                        if let Some(fid) = node.file_id {
                            state.selected_file = Some((fid, node.path.clone()));
                            state.breadcrumbs.set_from_path(&node.path);
                            outcome.commands.push(UiCommand::RequestCapture {
                                file_id: fid,
                                path: node.path,
                            });
                        }
                    } else {
                        state.breadcrumbs.set_from_path(&node.path);
                    }
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::TreeToggle(node_id) => {
                if state.tree.toggle(node_id).is_ok() {
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::TreeNavigate(direction) => {
                let moved = match direction {
                    FocusDirection::Up => state.tree.move_up(),
                    FocusDirection::Down => state.tree.move_down(),
                    FocusDirection::Left => state.tree.move_left(),
                    FocusDirection::Right => state.tree.move_right(),
                    _ => None,
                };
                if moved.is_some() {
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::BreadcrumbNavigate(direction) => {
                let moved = match direction {
                    FocusDirection::Left => state.breadcrumbs.navigate_left(),
                    FocusDirection::Right => state.breadcrumbs.navigate_right(),
                    _ => None,
                };
                if moved.is_some() {
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::BreadcrumbSelect(index) => {
                if let Some(seg) = state.breadcrumbs.truncate_to(index).cloned() {
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestCameraFocus {
                        target_description: seg.label,
                    });
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::HistoryNavigateBack => {
                if let Some(item) = state.sidebar.history.go_back().cloned() {
                    state.breadcrumbs.set_from_path(&item.path);
                    let event = UiEventKind::HistoryNavigated {
                        path: item.path.clone(),
                    };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestCameraFocus {
                        target_description: item.path,
                    });
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::HistoryNavigateForward => {
                if let Some(item) = state.sidebar.history.go_forward().cloned() {
                    state.breadcrumbs.set_from_path(&item.path);
                    let event = UiEventKind::HistoryNavigated {
                        path: item.path.clone(),
                    };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestCameraFocus {
                        target_description: item.path,
                    });
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::FitSelection => {
                let event = UiEventKind::CameraFitRequested {
                    scope: "Selection".to_string(),
                };
                state.record_event(event.clone(), now_nanos);
                outcome.emitted_events.push(event);
                outcome.commands.push(UiCommand::RequestCameraFocus {
                    target_description: "FitSelection".to_string(),
                });
            }

            UiAction::FitParent => {
                let event = UiEventKind::CameraFitRequested {
                    scope: "Parent".to_string(),
                };
                state.record_event(event.clone(), now_nanos);
                outcome.emitted_events.push(event);
                outcome.commands.push(UiCommand::RequestCameraFocus {
                    target_description: "FitParent".to_string(),
                });
            }

            UiAction::FitProject => {
                let event = UiEventKind::CameraFitRequested {
                    scope: "Project".to_string(),
                };
                state.record_event(event.clone(), now_nanos);
                outcome.emitted_events.push(event);
                outcome.commands.push(UiCommand::RequestCameraFocus {
                    target_description: "FitProject".to_string(),
                });
            }

            UiAction::PointerClick { target, select } => {
                let from = state.focus.current();
                if from != target {
                    state.focus.set_focus(target);
                    let event = UiEventKind::FocusChanged { from, to: target };
                    state.record_event(event.clone(), now_nanos);
                    outcome.emitted_events.push(event);
                    outcome.changed = true;
                }
                if select {
                    outcome.changed = true;
                }
                outcome.commands.push(UiCommand::RequestRedraw);
            }

            UiAction::PromoteSelectedToReadingLens => {
                if let Some((file_id, path)) = state.selected_file.clone() {
                    return Self::reduce(
                        state,
                        UiAction::OpenReadingLens {
                            file_id,
                            path,
                            line: None,
                        },
                        now_nanos,
                    );
                }
            }

            UiAction::PinReadingPane(pane_id) => {
                if state.reading_panes.pin_pane(pane_id) {
                    let ev = UiEventKind::ReadingPanePinned {
                        pane_id,
                        is_pinned: true,
                    };
                    state.record_event(ev.clone(), now_nanos);
                    outcome.emitted_events.push(ev);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::UnpinReadingPane(pane_id) => {
                if state.reading_panes.unpin_pane(pane_id) {
                    let ev = UiEventKind::ReadingPanePinned {
                        pane_id,
                        is_pinned: false,
                    };
                    state.record_event(ev.clone(), now_nanos);
                    outcome.emitted_events.push(ev);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::TogglePinReadingPane(pane_id) => {
                if state.reading_panes.toggle_pin(pane_id) {
                    let is_pinned = state
                        .reading_panes
                        .get_pane(pane_id)
                        .map(|p| p.is_pinned)
                        .unwrap_or(false);
                    let ev = UiEventKind::ReadingPanePinned {
                        pane_id,
                        is_pinned,
                    };
                    state.record_event(ev.clone(), now_nanos);
                    outcome.emitted_events.push(ev);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::CloseReadingPane(pane_id) => {
                if state.reading_panes.close_pane(pane_id).is_some() {
                    let ev = UiEventKind::ReadingPaneClosed { pane_id };
                    state.record_event(ev.clone(), now_nanos);
                    outcome.emitted_events.push(ev);
                    state.reading_lens_open = state.reading_panes.is_any_open();
                    state.reading_lens_file = state
                        .reading_panes
                        .active_pane()
                        .map(|p| (p.file_id, p.path.clone(), p.target_line));
                    if !state.reading_lens_open
                        && state.focus.current() == FocusTarget::ReadingLens
                    {
                        let _ = state.focus.return_focus();
                    }
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::SetCityMode(is_city) => {
                if state.is_city_mode != is_city {
                    state.is_city_mode = is_city;
                    let ev = UiEventKind::CityModeToggled {
                        is_city_mode: is_city,
                    };
                    state.record_event(ev.clone(), now_nanos);
                    outcome.emitted_events.push(ev);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::PointerDown {
                region,
                pos,
                button,
                modifiers,
            } => {
                if let Some(kind) = state.gesture_arbitrator.start_pointer_drag(
                    region,
                    pos,
                    button,
                    modifiers,
                    state.is_city_mode,
                ) {
                    let ev = UiEventKind::GestureStarted { kind };
                    state.record_event(ev.clone(), now_nanos);
                    outcome.emitted_events.push(ev);
                    outcome.changed = true;
                }

                // Handle surface focus shift
                match region {
                    HitRegion::ReadingLensBody(pane_id)
                    | HitRegion::ReadingLensTitleBar(pane_id) => {
                        state.reading_panes.set_active_pane_id(Some(pane_id));
                        if state.focus.current() != FocusTarget::ReadingLens {
                            let from = state.focus.current();
                            state.focus.set_focus(FocusTarget::ReadingLens);
                            let ev = UiEventKind::FocusChanged {
                                from,
                                to: FocusTarget::ReadingLens,
                            };
                            state.record_event(ev.clone(), now_nanos);
                            outcome.emitted_events.push(ev);
                        }
                        outcome.changed = true;
                        outcome.commands.push(UiCommand::RequestRedraw);
                    }
                    HitRegion::AtlasBackground => {
                        if state.focus.current() != FocusTarget::Atlas {
                            let from = state.focus.current();
                            state.focus.set_focus(FocusTarget::Atlas);
                            let ev = UiEventKind::FocusChanged {
                                from,
                                to: FocusTarget::Atlas,
                            };
                            state.record_event(ev.clone(), now_nanos);
                            outcome.emitted_events.push(ev);
                            outcome.changed = true;
                            outcome.commands.push(UiCommand::RequestRedraw);
                        }
                    }
                    HitRegion::SidebarContent | HitRegion::SidebarSplitter => {
                        let target = FocusTarget::Sidebar(state.sidebar.active_panel);
                        if state.focus.current() != target {
                            let from = state.focus.current();
                            state.focus.set_focus(target);
                            let ev = UiEventKind::FocusChanged { from, to: target };
                            state.record_event(ev.clone(), now_nanos);
                            outcome.emitted_events.push(ev);
                            outcome.changed = true;
                            outcome.commands.push(UiCommand::RequestRedraw);
                        }
                    }
                    HitRegion::Breadcrumbs => {
                        if state.focus.current() != FocusTarget::Breadcrumbs {
                            let from = state.focus.current();
                            state.focus.set_focus(FocusTarget::Breadcrumbs);
                            let ev = UiEventKind::FocusChanged {
                                from,
                                to: FocusTarget::Breadcrumbs,
                            };
                            state.record_event(ev.clone(), now_nanos);
                            outcome.emitted_events.push(ev);
                            outcome.changed = true;
                            outcome.commands.push(UiCommand::RequestRedraw);
                        }
                    }
                    HitRegion::SearchPalette => {
                        if state.focus.current() != FocusTarget::SearchPalette {
                            let from = state.focus.current();
                            state.focus.set_focus(FocusTarget::SearchPalette);
                            let ev = UiEventKind::FocusChanged {
                                from,
                                to: FocusTarget::SearchPalette,
                            };
                            state.record_event(ev.clone(), now_nanos);
                            outcome.emitted_events.push(ev);
                            outcome.changed = true;
                            outcome.commands.push(UiCommand::RequestRedraw);
                        }
                    }
                    HitRegion::TreeProjection => {
                        if state.focus.current() != FocusTarget::TreeProjection {
                            let from = state.focus.current();
                            state.focus.set_focus(FocusTarget::TreeProjection);
                            let ev = UiEventKind::FocusChanged {
                                from,
                                to: FocusTarget::TreeProjection,
                            };
                            state.record_event(ev.clone(), now_nanos);
                            outcome.emitted_events.push(ev);
                            outcome.changed = true;
                            outcome.commands.push(UiCommand::RequestRedraw);
                        }
                    }
                }
            }

            UiAction::PointerMove { pos } => {
                if let Some((kind, (dx, dy))) =
                    state.gesture_arbitrator.update_pointer_move(pos)
                {
                    outcome.changed = true;
                    match kind {
                        GestureKind::LensDrag { pane_id } => {
                            state.reading_panes.translate_pane(pane_id, dx, dy);
                            if let Some(pane) = state.reading_panes.get_pane(pane_id) {
                                outcome.commands.push(UiCommand::ReadingPaneMoved {
                                    pane_id,
                                    pos: pane.position,
                                });
                            }
                            let ev = UiEventKind::ReadingPaneMoved { pane_id };
                            state.record_event(ev.clone(), now_nanos);
                            outcome.emitted_events.push(ev);
                            outcome.commands.push(UiCommand::RequestRedraw);
                        }
                        GestureKind::TextSelect { pane_id } => {
                            let ev = UiEventKind::TextSelectedInLens { pane_id };
                            state.record_event(ev.clone(), now_nanos);
                            outcome.emitted_events.push(ev);
                            outcome.commands.push(UiCommand::ReadingPaneSelectedText {
                                pane_id,
                                range: (0, (dx.abs() * 10.0) as usize),
                            });
                            outcome.commands.push(UiCommand::RequestRedraw);
                        }
                        GestureKind::SidebarResize => {
                            let new_width = state.sidebar.width - dx;
                            state.sidebar.width = new_width
                                .clamp(state.sidebar.min_width, state.sidebar.max_width);
                            outcome.commands.push(UiCommand::RequestRedraw);
                        }
                        GestureKind::CameraPan => {
                            outcome.commands.push(UiCommand::CameraPan { dx, dy });
                            outcome.commands.push(UiCommand::RequestRedraw);
                        }
                        GestureKind::CameraOrbit => {
                            outcome.commands.push(UiCommand::CameraOrbit { dx, dy });
                            outcome.commands.push(UiCommand::RequestRedraw);
                        }
                        GestureKind::PinchZoom => {}
                    }
                }
            }

            UiAction::PointerUp { pos: _, button: _ } => {
                if let Some(kind) = state.gesture_arbitrator.complete_pointer() {
                    let ev = UiEventKind::GestureCompleted { kind };
                    state.record_event(ev.clone(), now_nanos);
                    outcome.emitted_events.push(ev);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::ScrollEvent {
                region,
                delta,
                modifiers,
            } => {
                let routing = state
                    .gesture_arbitrator
                    .route_scroll(region, delta, modifiers);
                match routing {
                    ScrollRouting::LensScroll { pane_id, dx, dy } => {
                        state.reading_panes.scroll_pane(pane_id, dx, dy);
                        if let Some(pane) = state.reading_panes.get_pane(pane_id) {
                            outcome.commands.push(UiCommand::ReadingPaneScrolled {
                                pane_id,
                                offset: pane.scroll_offset,
                            });
                        }
                        outcome.changed = true;
                        outcome.commands.push(UiCommand::RequestRedraw);
                        // NEGATIVE INVARIANT: CameraPan is NOT emitted for reading lens scroll!
                    }
                    ScrollRouting::AtlasPan { dx, dy } => {
                        outcome.commands.push(UiCommand::CameraPan { dx, dy });
                        outcome.changed = true;
                        outcome.commands.push(UiCommand::RequestRedraw);
                    }
                    ScrollRouting::AtlasZoom { factor } => {
                        outcome.commands.push(UiCommand::CameraZoom { factor });
                        outcome.changed = true;
                        outcome.commands.push(UiCommand::RequestRedraw);
                    }
                    ScrollRouting::SidebarScroll { dy: _ } => {
                        outcome.changed = true;
                        outcome.commands.push(UiCommand::RequestRedraw);
                    }
                    ScrollRouting::TreeScroll { dy: _ } => {
                        outcome.changed = true;
                        outcome.commands.push(UiCommand::RequestRedraw);
                    }
                    ScrollRouting::Ignored => {}
                }
            }

            UiAction::PinchEvent {
                region,
                centroid: _,
                magnification,
            } => {
                if let Some(factor) = state
                    .gesture_arbitrator
                    .route_pinch(region, magnification)
                {
                    outcome.commands.push(UiCommand::CameraZoom { factor });
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::CancelGesture => {
                if state.gesture_arbitrator.cancel() {
                    let ev = UiEventKind::GestureCancelled;
                    state.record_event(ev.clone(), now_nanos);
                    outcome.emitted_events.push(ev);
                    outcome.changed = true;
                    outcome.commands.push(UiCommand::RequestRedraw);
                }
            }

            UiAction::EnqueueDiscreteInput(ev) => {
                state.motion_mailbox.push_discrete(ev);
            }

            UiAction::RecordContinuousMove { pos, delta } => {
                state.motion_mailbox.record_move(pos, delta);
            }

            UiAction::RecordContinuousScroll { delta } => {
                state.motion_mailbox.record_scroll(delta);
            }

            UiAction::ProcessInputBatch { max_discrete } => {
                // 1. Consume coalesced motion
                if let Some(motion) = state.motion_mailbox.take_motion() {
                    outcome.changed = true;
                    if motion.pan_delta.0.abs() > f32::EPSILON
                        || motion.pan_delta.1.abs() > f32::EPSILON
                    {
                        outcome.commands.push(UiCommand::CameraPan {
                            dx: motion.pan_delta.0,
                            dy: motion.pan_delta.1,
                        });
                    }
                    if motion.scroll_delta.0.abs() > f32::EPSILON
                        || motion.scroll_delta.1.abs() > f32::EPSILON
                    {
                        outcome.commands.push(UiCommand::CameraPan {
                            dx: motion.scroll_delta.0,
                            dy: motion.scroll_delta.1,
                        });
                    }
                    if motion.pinch_magnification.abs() > f32::EPSILON {
                        outcome.commands.push(UiCommand::CameraZoom {
                            factor: 1.0 + motion.pinch_magnification,
                        });
                    }
                    outcome.commands.push(UiCommand::RequestRedraw);
                }

                // 2. Consume bounded batch of discrete events
                let discrete_batch =
                    state.motion_mailbox.drain_discrete_batch(max_discrete);
                for discrete in discrete_batch {
                    let sub_outcome = match discrete {
                        DiscreteInputEvent::PointerDown {
                            region,
                            pos,
                            button,
                            modifiers,
                        } => Self::reduce(
                            state,
                            UiAction::PointerDown {
                                region,
                                pos,
                                button,
                                modifiers,
                            },
                            now_nanos,
                        ),
                        DiscreteInputEvent::PointerUp { pos, button } => Self::reduce(
                            state,
                            UiAction::PointerUp { pos, button },
                            now_nanos,
                        ),
                        DiscreteInputEvent::KeyDown { key, modifiers: _ } => {
                            Self::reduce(state, UiAction::KeyboardShortcut(key), now_nanos)
                        }
                        DiscreteInputEvent::KeyUp { .. } => UiReductionOutcome::default(),
                        DiscreteInputEvent::FocusTargetSelected(target) => {
                            Self::reduce(state, UiAction::FocusChange(target), now_nanos)
                        }
                        DiscreteInputEvent::MarkedTextCommit(text) => Self::reduce(
                            state,
                            UiAction::SearchQueryChanged(text),
                            now_nanos,
                        ),
                    };
                    outcome.changed |= sub_outcome.changed;
                    outcome.commands.extend(sub_outcome.commands);
                    outcome.emitted_events.extend(sub_outcome.emitted_events);
                }
            }

            UiAction::KeyboardShortcut(cmd) => {
                match cmd.as_str() {
                    "Escape" => {
                        // Priority 1: Cancel active gesture first before changing lens or scope
                        if state.gesture_arbitrator.is_active() {
                            return Self::reduce(state, UiAction::CancelGesture, now_nanos);
                        } else if state.search_palette_open {
                            return Self::reduce(state, UiAction::CloseSearchPalette, now_nanos);
                        } else if state.reading_panes.is_any_open() || state.reading_lens_open {
                            return Self::reduce(state, UiAction::CloseReadingLens, now_nanos);
                        } else {
                            return Self::reduce(state, UiAction::FocusReturn, now_nanos);
                        }
                    }
                    "Enter" => {
                        // Enter on Atlas with selected file promotes to reading lens
                        if state.focus.current() == FocusTarget::Atlas && state.selected_file.is_some() {
                            return Self::reduce(state, UiAction::PromoteSelectedToReadingLens, now_nanos);
                        }
                    }
                    "Cmd+P" | "Ctrl+P" => {
                        return Self::reduce(state, UiAction::OpenSearchPalette, now_nanos);
                    }
                    "Cmd+B" | "Ctrl+B" => {
                        return Self::reduce(state, UiAction::ToggleSidebar, now_nanos);
                    }
                    _ => {}
                }
            }
        }

        outcome
    }
}

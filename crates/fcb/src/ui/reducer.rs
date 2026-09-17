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
#[derive(Clone, Debug, Eq, PartialEq)]
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
                if state.reading_lens_open {
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

            UiAction::KeyboardShortcut(cmd) => {
                match cmd.as_str() {
                    "Escape" => {
                        if state.search_palette_open {
                            return Self::reduce(state, UiAction::CloseSearchPalette, now_nanos);
                        } else if state.reading_lens_open {
                            return Self::reduce(state, UiAction::CloseReadingLens, now_nanos);
                        } else {
                            return Self::reduce(state, UiAction::FocusReturn, now_nanos);
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

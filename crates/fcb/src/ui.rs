#![forbid(unsafe_code)]

//! Deterministic UI state machine, focus model, sidebar, and command routing.
//!
//! Enforces:
//! - Strict separation of focus and selection.
//! - Bounded focus-return stack for predictable overlay exit.
//! - Conventional file tree projection for accessibility and precision navigation.
//! - Sidebar with Inspector, Results, History, and Outline panels.
//! - Zero I/O, parsing, or bulk-drop on interaction thread.
//! - Generation-validated search results with stale batch rejection.

pub mod breadcrumbs;
pub mod focus;
pub mod reducer;
pub mod sidebar;
pub mod tree;

pub use breadcrumbs::{ScopeBreadcrumbs, ScopeSegment};
pub use focus::{FocusDirection, FocusManager, FocusStack, FocusTarget};
pub use reducer::{
    UiAction, UiCommand, UiEvent, UiEventKind, UiEventRing, UiReducer, UiReductionOutcome, UiState,
};
pub use sidebar::{
    FactCertainty, HistoryItem, HistoryState, InspectorFact, InspectorState, OutlineState,
    OutlineSymbol, ResultsState, SearchResultEntry, SidebarPanel, SidebarState,
};
pub use tree::{TreeError, TreeNode, TreeNodeId, TreeNodeKind, TreeProjection};

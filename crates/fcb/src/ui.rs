#![forbid(unsafe_code)]

//! Deterministic UI state machine, focus model, sidebar, and command routing.
//!
//! Enforces:
//! - Strict separation of focus and selection.
//! - Bounded focus-return stack for predictable overlay exit.
//! - Conventional file tree projection for accessibility and precision navigation.
//! - Sidebar with Inspector, Results, History, and Outline panels.
//! - No I/O or parsing on the interaction thread; producers admit payloads.
//! - Generation-validated search results with stale batch rejection.

pub mod breadcrumbs;
pub mod focus;
pub mod gesture;
pub mod motion_mailbox;
pub mod reading_panes;
pub mod reducer;
pub mod sidebar;
pub mod tree;
/// Exact captured-content activation, separate from live path navigation.
#[cfg(feature = "search")]
pub mod search_navigation;

pub use breadcrumbs::{ScopeBreadcrumbs, ScopeSegment};
pub use focus::{FocusDirection, FocusManager, FocusStack, FocusTarget};
pub use gesture::{
    GestureArbitrator, GestureKind, GestureState, HitRegion, ModifierKeys, PointerButton,
    ScrollRouting,
};
pub use motion_mailbox::{CoalescedMotion, DiscreteInputEvent, MotionMailbox};
pub use reading_panes::{ReadingPane, ReadingPaneManager, ReadingPaneOpenError};
pub use reducer::{
    UiAction, UiCommand, UiEvent, UiEventKind, UiEventRing, UiReducer, UiReductionOutcome, UiState,
};
pub use sidebar::{
    FactCertainty, HistoryItem, HistoryState, InspectorFact, InspectorState, OutlineState,
    OutlineSymbol, ResultsState, SearchFailure, SearchResultEntry, SidebarPanel, SidebarState,
};
#[cfg(feature = "search")]
pub use search_navigation::{CapturedSearchActivation, SearchActivationError};
pub use tree::{TreeError, TreeNode, TreeNodeId, TreeNodeKind, TreeProjection};

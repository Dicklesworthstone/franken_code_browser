#![forbid(unsafe_code)]

//! Focus model, focus ring navigation, and bounded focus-return stack.
//!
//! Focus is independent of selection: changing focus does not steal or alter
//! the active selection, and changing selection does not steal focus unless
//! explicitly commanded. The focus-return stack guarantees predictable return
//! to the exact prior surface when modal overlays (e.g. search palette) dismiss.

use std::collections::VecDeque;
use super::sidebar::SidebarPanel;

/// A focusable surface in the user interface.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FocusTarget {
    /// The main spatial atlas surface.
    Atlas,
    /// An active panel in the collapsible sidebar.
    Sidebar(SidebarPanel),
    /// The open source reading lens.
    ReadingLens,
    /// The overlay search and command palette.
    SearchPalette,
    /// The hierarchical scope breadcrumbs strip.
    Breadcrumbs,
    /// The conventional hierarchical outline / file tree view.
    TreeProjection,
}

impl FocusTarget {
    /// Human-readable label for debugging and accessibility surfaces.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Atlas => "Spatial Atlas",
            Self::Sidebar(panel) => panel.label(),
            Self::ReadingLens => "Source Reading Lens",
            Self::SearchPalette => "Search Palette",
            Self::Breadcrumbs => "Scope Breadcrumbs",
            Self::TreeProjection => "File Tree Projection",
        }
    }

    /// Whether this target is a modal overlay that requires focus-return on exit.
    pub const fn is_modal(self) -> bool {
        matches!(self, Self::SearchPalette)
    }
}

/// Directional movement across focusable surfaces or list elements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusDirection {
    /// Next element in the tab sequence.
    Next,
    /// Previous element in the tab sequence.
    Prev,
    /// Upward directional movement.
    Up,
    /// Downward directional movement.
    Down,
    /// Leftward directional movement (e.g. tree collapse, breadcrumb back).
    Left,
    /// Rightward directional movement (e.g. tree expand, breadcrumb forward).
    Right,
}

/// Bounded focus-return stack for modal overlay dismissal.
#[derive(Clone, Debug, PartialEq)]
pub struct FocusStack {
    stack: VecDeque<FocusTarget>,
    capacity: usize,
}

impl FocusStack {
    pub const DEFAULT_CAPACITY: usize = 32;

    pub fn new(capacity: usize) -> Self {
        let cap = if capacity == 0 {
            Self::DEFAULT_CAPACITY
        } else {
            capacity
        };
        Self {
            stack: VecDeque::with_capacity(cap),
            capacity: cap,
        }
    }

    pub fn push(&mut self, target: FocusTarget) {
        if self.stack.len() >= self.capacity {
            self.stack.pop_front();
        }
        self.stack.push_back(target);
    }

    pub fn pop(&mut self) -> Option<FocusTarget> {
        self.stack.pop_back()
    }

    pub fn peek(&self) -> Option<FocusTarget> {
        self.stack.back().copied()
    }

    pub fn len(&self) -> usize {
        self.stack.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }
}

impl Default for FocusStack {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

/// Coordinates the current focus target, tab ring order, and focus-return.
#[derive(Clone, Debug, PartialEq)]
pub struct FocusManager {
    current: FocusTarget,
    stack: FocusStack,
}

impl FocusManager {
    /// Ordered sequence of main non-modal surfaces for tab-ring navigation.
    pub const TAB_RING: [FocusTarget; 5] = [
        FocusTarget::Atlas,
        FocusTarget::Breadcrumbs,
        FocusTarget::TreeProjection,
        FocusTarget::Sidebar(SidebarPanel::Inspector),
        FocusTarget::ReadingLens,
    ];

    pub fn new(initial: FocusTarget) -> Self {
        Self {
            current: initial,
            stack: FocusStack::default(),
        }
    }

    pub const fn current(&self) -> FocusTarget {
        self.current
    }

    pub fn stack_depth(&self) -> usize {
        self.stack.len()
    }

    /// Explicitly set focus to `target`.
    ///
    /// If `target` is different from `current`, the previous target is pushed
    /// onto the focus-return stack.
    pub fn set_focus(&mut self, target: FocusTarget) {
        if self.current != target {
            self.stack.push(self.current);
            self.current = target;
        }
    }

    /// Return focus to the most recent prior target on the stack.
    ///
    /// Returns the restored target, or `None` if the stack was empty.
    pub fn return_focus(&mut self) -> Option<FocusTarget> {
        if let Some(prev) = self.stack.pop() {
            self.current = prev;
            Some(prev)
        } else {
            None
        }
    }

    /// Advance or retreat through the tab ring.
    pub fn navigate_tab(&mut self, direction: FocusDirection, lens_available: bool) -> FocusTarget {
        let candidates: Vec<FocusTarget> = Self::TAB_RING
            .iter()
            .copied()
            .filter(|&t| t != FocusTarget::ReadingLens || lens_available)
            .collect();

        if candidates.is_empty() {
            return self.current;
        }

        let curr_idx = candidates.iter().position(|&t| t == self.current);

        let next_idx = match (curr_idx, direction) {
            (Some(idx), FocusDirection::Next) => (idx + 1) % candidates.len(),
            (Some(idx), FocusDirection::Prev) => {
                if idx == 0 {
                    candidates.len() - 1
                } else {
                    idx - 1
                }
            }
            (None, FocusDirection::Next) => 0,
            (None, FocusDirection::Prev) => candidates.len() - 1,
            _ => curr_idx.unwrap_or(0),
        };

        let target = candidates.get(next_idx).copied().unwrap_or(self.current);
        self.set_focus(target);
        target
    }
}

impl Default for FocusManager {
    fn default() -> Self {
        Self::new(FocusTarget::Atlas)
    }
}

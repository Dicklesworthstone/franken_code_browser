#![forbid(unsafe_code)]

//! Native macOS VoiceOver and accessibility (AX) route adapter (§23.5).
//!
//! Provides the typed bridge between AppKit / `NSAccessibility` protocols and
//! FCB's accepted semantic layout snapshot. Virtualized text queries resolve
//! through [`PendingTextRangeResolver`] with explicit `Pending`, `Ready`,
//! `Stale`, and `Refused` states. Giant paragraphs are not shaped synchronously
//! inside AppKit callbacks.

use std::sync::Mutex;

use fcb_core::{
    geometry::{Point2D, Rect2D},
    AcceptedLayoutSnapshot, ArenaOwnerId, BidiBoundary, CoreError,
    PendingRangeStatus, PendingTextRangeResolver, RangeRequestToken,
    SemanticFocusState, SemanticNode, SemanticNodeId, SemanticRole,
    Utf16CodeUnitOffset, Utf16CodeUnitRange, NATIVE_NOT_FOUND,
};

/// macOS accessibility roles mapped from [`SemanticRole`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AxRole {
    StaticText,
    Heading,
    Paragraph,
    SourceText,
    CodeBlock,
    Table,
    TableRow,
    TableCell,
    Link,
    Button,
    TextField,
    List,
    ListItem,
    Toolbar,
    Splitter,
    ScrollArea,
    Container,
    Group,
}

impl AxRole {
    /// Maps an internal [`SemanticRole`] to the standard macOS accessibility role.
    pub const fn from_semantic_role(role: SemanticRole) -> Self {
        match role {
            SemanticRole::Document => Self::Group,
            SemanticRole::Heading => Self::Heading,
            SemanticRole::Paragraph => Self::Paragraph,
            SemanticRole::SourceText => Self::SourceText,
            SemanticRole::CodeBlock => Self::CodeBlock,
            SemanticRole::Table => Self::Table,
            SemanticRole::TableRow => Self::TableRow,
            SemanticRole::TableCell => Self::TableCell,
            SemanticRole::Link => Self::Link,
            SemanticRole::Button => Self::Button,
            SemanticRole::TextField => Self::TextField,
            SemanticRole::List => Self::List,
            SemanticRole::ListItem => Self::ListItem,
            SemanticRole::Toolbar => Self::Toolbar,
            SemanticRole::Splitter => Self::Splitter,
            SemanticRole::ScrollArea => Self::ScrollArea,
            SemanticRole::Container => Self::Container,
            _ => Self::Group,
        }
    }

    /// Standard Apple AppKit / NSAccessibility role identifier.
    pub const fn ax_identifier(self) -> &'static str {
        match self {
            Self::StaticText | Self::SourceText | Self::Paragraph => "AXStaticText",
            Self::Heading => "AXHeading",
            Self::CodeBlock => "AXTextArea",
            Self::Table => "AXTable",
            Self::TableRow => "AXRow",
            Self::TableCell => "AXCell",
            Self::Link => "AXLink",
            Self::Button => "AXButton",
            Self::TextField => "AXTextField",
            Self::List => "AXList",
            Self::ListItem => "AXGroup",
            Self::Toolbar => "AXToolbar",
            Self::Splitter => "AXSplitter",
            Self::ScrollArea => "AXScrollArea",
            Self::Container | Self::Group => "AXGroup",
        }
    }
}

/// Actions an accessibility client (e.g. VoiceOver) can perform.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AxAction {
    Press,
    ShowMenu,
    ScrollToVisible,
    Pick,
}

impl AxAction {
    pub const fn ax_name(self) -> &'static str {
        match self {
            Self::Press => "AXPress",
            Self::ShowMenu => "AXShowMenu",
            Self::ScrollToVisible => "AXScrollToVisible",
            Self::Pick => "AXPick",
        }
    }
}

/// Notifications emitted to the macOS accessibility subsystem.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AxNotification {
    FocusedUIElementChanged,
    ValueChanged,
    SelectedTextChanged,
    LayoutChanged,
    ElementBusyChanged,
}

impl AxNotification {
    pub const fn ax_name(self) -> &'static str {
        match self {
            Self::FocusedUIElementChanged => "AXFocusedUIElementChanged",
            Self::ValueChanged => "AXValueChanged",
            Self::SelectedTextChanged => "AXSelectedTextChanged",
            Self::LayoutChanged => "AXLayoutChanged",
            Self::ElementBusyChanged => "AXElementBusyChanged",
        }
    }
}

/// Efficient index mapping UTF-16 code unit offsets to 1-based line numbers and ranges.
#[derive(Clone, Debug, PartialEq)]
pub struct AxLineIndex {
    line_starts: Vec<u64>,
    total_units: u64,
}

impl AxLineIndex {
    pub fn new(line_starts: Vec<u64>, total_units: u64) -> Result<Self, CoreError> {
        if line_starts.is_empty() || line_starts.first().copied() != Some(0) {
            return Err(CoreError::LimitExceeded);
        }
        for w in line_starts.windows(2) {
            let a = w.first().copied().unwrap_or(0);
            let b = w.get(1).copied().unwrap_or(0);
            if a >= b {
                return Err(CoreError::RangeReversed);
            }
        }
        if let Some(&last) = line_starts.last() {
            if last > total_units {
                return Err(CoreError::LimitExceeded);
            }
        }
        Ok(Self {
            line_starts,
            total_units,
        })
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    pub const fn total_units(&self) -> u64 {
        self.total_units
    }

    /// Maps a UTF-16 code unit offset to a 1-based line number.
    /// Rejects `NATIVE_NOT_FOUND` sentinel and out-of-bounds offsets.
    pub fn line_for_offset(&self, offset: Utf16CodeUnitOffset) -> Result<usize, CoreError> {
        if offset.get() == NATIVE_NOT_FOUND {
            return Err(CoreError::NativeSentinel);
        }
        if offset.get() > self.total_units {
            return Err(CoreError::LimitExceeded);
        }

        let off = offset.get();
        // partition_point finds first start > off
        let idx = self.line_starts.partition_point(|&s| s <= off);
        // 1-based line number
        Ok(idx.max(1))
    }

    /// Returns the UTF-16 range for a 1-based line number.
    pub fn range_for_line(&self, line_num: usize) -> Result<Utf16CodeUnitRange, CoreError> {
        if line_num == 0 || line_num > self.line_starts.len() {
            return Err(CoreError::LimitExceeded);
        }
        let start_idx = line_num.saturating_sub(1);
        let start = self
            .line_starts
            .get(start_idx)
            .copied()
            .ok_or(CoreError::LimitExceeded)?;
        let end = if start_idx + 1 < self.line_starts.len() {
            self.line_starts
                .get(start_idx + 1)
                .copied()
                .unwrap_or(self.total_units)
        } else {
            self.total_units
        };

        let start_off = Utf16CodeUnitOffset::new(start);
        let end_off = Utf16CodeUnitOffset::new(end);
        Utf16CodeUnitRange::new(start_off, end_off).map_err(|_| CoreError::RangeReversed)
    }
}

/// The production VoiceOver / accessibility adapter for FCB reader views.
#[derive(Debug)]
pub struct NativeAxRoute {
    owner: ArenaOwnerId,
    notifications: Mutex<Vec<(AxNotification, SemanticNodeId)>>,
}

impl NativeAxRoute {
    pub const fn new(owner: ArenaOwnerId) -> Self {
        Self {
            owner,
            notifications: Mutex::new(Vec::new()),
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    /// Queries the accessibility role of `node`.
    pub const fn role(&self, node: &SemanticNode) -> AxRole {
        AxRole::from_semantic_role(node.role())
    }

    /// Queries the accessibility label of `node`.
    pub fn label<'a>(&self, node: &'a SemanticNode) -> Option<&'a str> {
        node.label()
    }

    /// Queries the accessibility value of `node`.
    pub fn value<'a>(&self, node: &'a SemanticNode) -> Option<&'a str> {
        node.value()
    }

    /// Queries the total number of characters (UTF-16 code units) in `node`.
    pub fn number_of_characters(&self, node: &SemanticNode) -> u64 {
        node.text_range().map_or(0, |r| r.len())
    }

    /// Queries the selected text range of `node`, if any.
    pub fn selected_text_range(
        &self,
        node: &SemanticNode,
        active_selection: Option<Utf16CodeUnitRange>,
    ) -> Option<Utf16CodeUnitRange> {
        let node_range = node.text_range()?;
        let selection = active_selection?;

        // Intersect selection with node range
        let sel_start = selection.start().get();
        let sel_end = selection.end().get();
        let node_start = node_range.start().get();
        let node_end = node_range.end().get();

        if sel_end <= node_start || sel_start >= node_end {
            return None;
        }

        let inter_start = sel_start.max(node_start);
        let inter_end = sel_end.min(node_end);

        Utf16CodeUnitRange::new(
            Utf16CodeUnitOffset::new(inter_start),
            Utf16CodeUnitOffset::new(inter_end),
        )
        .ok()
    }

    /// Resolves an accessibility text range virtualized request (§23.5).
    ///
    /// If `text` is `None`, returns [`PendingRangeStatus::Pending`], indicating
    /// that asynchronous source context preparation has been scheduled and the
    /// caller must defer rendering/accessibility return without blocking.
    ///
    /// Wrong offsets, out-of-bounds requests, and surrogate splits are refused
    /// rather than silently clamped.
    pub fn string_for_range(
        &self,
        token: RangeRequestToken,
        layout: &AcceptedLayoutSnapshot,
        text: Option<&str>,
        requested_range: Utf16CodeUnitRange,
        bidi_boundaries: &[BidiBoundary],
    ) -> PendingRangeStatus {
        PendingTextRangeResolver::resolve_range(
            token,
            layout,
            text,
            requested_range,
            bidi_boundaries,
        )
    }

    /// Queries visual bounding rectangle for a sub-range within `node`.
    pub fn bounds_for_range(
        &self,
        node: &SemanticNode,
        range: Utf16CodeUnitRange,
        line_height: f64,
        char_width: f64,
    ) -> Result<Rect2D, CoreError> {
        let node_range = node.text_range().ok_or(CoreError::LimitExceeded)?;
        if range.start().get() < node_range.start().get() || range.end().get() > node_range.end().get()
        {
            return Err(CoreError::LimitExceeded);
        }

        let node_rect = node.geometry().visible_rect().ok_or(CoreError::LimitExceeded)?;
        let rel_start = range.start().get().saturating_sub(node_range.start().get());
        let len = range.len();

        let x = node_rect.min_x() + (rel_start as f64 * char_width);
        let y = node_rect.min_y();
        let width = len as f64 * char_width;
        let height = line_height.min(node_rect.size().height());

        Rect2D::from_xywh(x, y, width, height)
    }

    /// Point hit-testing across the accepted layout snapshot.
    /// Returns the deepest semantic node containing `point`.
    pub fn hit_test(
        &self,
        point: Point2D,
        layout: &AcceptedLayoutSnapshot,
    ) -> Option<SemanticNodeId> {
        fn search_node(
            id: SemanticNodeId,
            point: Point2D,
            layout: &AcceptedLayoutSnapshot,
        ) -> Option<SemanticNodeId> {
            let node = layout.node(id)?;
            let rect = node.geometry().visible_rect()?;
            if !rect.contains_point(point) {
                return None;
            }

            // Check children first for deepest match
            for &child_id in node.children() {
                if let Some(hit) = search_node(child_id, point, layout) {
                    return Some(hit);
                }
            }

            Some(id)
        }

        search_node(layout.root_node(), point, layout)
    }

    /// Returns the currently focused element and its bounds.
    pub fn focused_element(
        &self,
        focus_state: &SemanticFocusState,
        layout: &AcceptedLayoutSnapshot,
    ) -> Option<(SemanticNodeId, Rect2D)> {
        let focused_id = focus_state.current_focus()?;
        let bounds = focus_state.focus_bounds(layout)?;
        Some((focused_id, bounds))
    }

    /// Advances focus to the next focusable candidate.
    pub fn focus_next(
        &self,
        focus_state: &mut SemanticFocusState,
        layout: &AcceptedLayoutSnapshot,
        candidates: &[SemanticNodeId],
    ) -> Result<Option<SemanticNodeId>, CoreError> {
        if candidates.is_empty() {
            return Ok(None);
        }

        let next_id = match focus_state.current_focus() {
            None => candidates.first().copied(),
            Some(curr) => {
                let idx = candidates.iter().position(|&id| id == curr).unwrap_or(0);
                let next_idx = (idx + 1) % candidates.len();
                candidates.get(next_idx).copied()
            }
        };

        if let Some(target) = next_id {
            focus_state.focus_node(target, layout)?;
            self.emit_notification(AxNotification::FocusedUIElementChanged, target);
        }

        Ok(next_id)
    }

    /// Retreats focus to the previous focusable candidate.
    pub fn focus_prev(
        &self,
        focus_state: &mut SemanticFocusState,
        layout: &AcceptedLayoutSnapshot,
        candidates: &[SemanticNodeId],
    ) -> Result<Option<SemanticNodeId>, CoreError> {
        if candidates.is_empty() {
            return Ok(None);
        }

        let prev_id = match focus_state.current_focus() {
            None => candidates.last().copied(),
            Some(curr) => {
                let idx = candidates.iter().position(|&id| id == curr).unwrap_or(0);
                let prev_idx = if idx == 0 {
                    candidates.len().saturating_sub(1)
                } else {
                    idx - 1
                };
                candidates.get(prev_idx).copied()
            }
        };

        if let Some(target) = prev_id {
            focus_state.focus_node(target, layout)?;
            self.emit_notification(AxNotification::FocusedUIElementChanged, target);
        }

        Ok(prev_id)
    }

    /// Emits an accessibility notification.
    pub fn emit_notification(&self, notification: AxNotification, node_id: SemanticNodeId) {
        if let Ok(mut q) = self.notifications.lock() {
            q.push((notification, node_id));
        }
    }

    /// Drains all pending accessibility notifications.
    pub fn drain_notifications(&self) -> Vec<(AxNotification, SemanticNodeId)> {
        self.notifications
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }
}

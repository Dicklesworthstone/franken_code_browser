#![forbid(unsafe_code)]

//! Full virtual semantic accessibility surfaces (§23.1, §23.2, §23.3, §23.5).
//!
//! A custom GPU canvas does not automatically provide usable accessibility.
//! This module provides:
//! - Virtualized outline / file tree (`VirtualOutlineTree`) that resolves
//!   bounded slices on demand without instantiating million-node trees.
//! - Virtualized search results (`VirtualSearchResults`) with linear navigation.
//! - Document semantic structure (`DocumentAxStructure`) exposing headings,
//!   tables, links, and source lines with milestone jump navigation.
//! - City mode non-spatial accessibility surface (`CityAxSurface`) translating
//!   3D building height/size metrics into descriptive accessible text.
//! - Unified keyboard navigation engine (`KeyboardNavEngine`) ensuring full
//!   pointer equivalence, visible focus independent of selection, and return focus.

use fcb_core::{
    geometry::Rect2D,
    ArenaOwnerId, CoreError, SemanticNodeId,
};

use crate::accessibility::AxRole;

/// Maximum number of items an accessibility slice query can materialize at once.
pub const MAX_ACCESSIBILITY_WINDOW: usize = 256;

/// A virtualized outline node presented to screen readers or keyboard navigation.
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualOutlineEntry {
    pub node_id: SemanticNodeId,
    pub label: String,
    pub role: AxRole,
    pub depth: usize,
    pub child_count: usize,
    pub is_expanded: bool,
    pub geometry: Rect2D,
    pub accessibility_value: String,
}

/// Virtualized outline / repository tree.
///
/// Holds the index hierarchy of a repository without eagerly allocating
/// full native accessibility objects for all items.
#[derive(Debug)]
pub struct VirtualOutlineTree {
    owner: ArenaOwnerId,
    total_items: usize,
    entries: Vec<VirtualOutlineEntry>,
}

impl VirtualOutlineTree {
    pub fn new(owner: ArenaOwnerId, total_items: usize) -> Self {
        Self {
            owner,
            total_items,
            entries: Vec::new(),
        }
    }

    pub fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn total_items(&self) -> usize {
        self.total_items
    }

    pub fn add_entry(&mut self, entry: VirtualOutlineEntry) {
        self.entries.push(entry);
    }

    /// Queries a bounded window of outline items `[start .. start + limit]`.
    ///
    /// Refuses requests exceeding [`MAX_ACCESSIBILITY_WINDOW`] to prevent
    /// memory exhaustion or frame stalls in accessibility callbacks.
    pub fn slice(&self, start: usize, limit: usize) -> Result<Vec<VirtualOutlineEntry>, CoreError> {
        if limit > MAX_ACCESSIBILITY_WINDOW {
            return Err(CoreError::LimitExceeded);
        }
        if start >= self.entries.len() {
            return Ok(Vec::new());
        }

        let end = (start + limit).min(self.entries.len());
        Ok(self.entries.get(start..end).map(|s| s.to_vec()).unwrap_or_default())
    }

    pub fn parent_index(&self, current: usize) -> Option<usize> {
        let entry = self.entries.get(current)?;
        if entry.depth == 0 {
            return None;
        }
        // Walk backwards to find first node with depth == entry.depth - 1
        for idx in (0..current).rev() {
            if let Some(prev) = self.entries.get(idx) {
                if prev.depth == entry.depth - 1 {
                    return Some(idx);
                }
            }
        }
        None
    }

    pub fn first_child_index(&self, current: usize) -> Option<usize> {
        let entry = self.entries.get(current)?;
        let next_idx = current + 1;
        let next_entry = self.entries.get(next_idx)?;
        if next_entry.depth == entry.depth + 1 {
            Some(next_idx)
        } else {
            None
        }
    }

    pub fn next_sibling_index(&self, current: usize) -> Option<usize> {
        let entry = self.entries.get(current)?;
        for idx in (current + 1)..self.entries.len() {
            if let Some(next) = self.entries.get(idx) {
                if next.depth == entry.depth {
                    return Some(idx);
                }
                if next.depth < entry.depth {
                    break;
                }
            }
        }
        None
    }

    pub fn prev_sibling_index(&self, current: usize) -> Option<usize> {
        let entry = self.entries.get(current)?;
        for idx in (0..current).rev() {
            if let Some(prev) = self.entries.get(idx) {
                if prev.depth == entry.depth {
                    return Some(idx);
                }
                if prev.depth < entry.depth {
                    break;
                }
            }
        }
        None
    }
}

/// A search result entry formatted for linear accessibility navigation.
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualSearchResultItem {
    pub rank: usize,
    pub node_id: SemanticNodeId,
    pub file_path: String,
    pub line_number: u32,
    pub column_number: u32,
    pub match_snippet: String,
    pub geometry: Rect2D,
    pub accessible_label: String,
}

/// Virtualized search results list.
#[derive(Debug)]
pub struct VirtualSearchResults {
    results: Vec<VirtualSearchResultItem>,
    selected_index: Option<usize>,
}

impl VirtualSearchResults {
    pub fn new(results: Vec<VirtualSearchResultItem>) -> Self {
        Self {
            results,
            selected_index: None,
        }
    }

    pub fn total_count(&self) -> usize {
        self.results.len()
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    pub fn selected_item(&self) -> Option<&VirtualSearchResultItem> {
        self.selected_index.and_then(|idx| self.results.get(idx))
    }

    pub fn slice(&self, start: usize, limit: usize) -> Result<&[VirtualSearchResultItem], CoreError> {
        if limit > MAX_ACCESSIBILITY_WINDOW {
            return Err(CoreError::LimitExceeded);
        }
        if start >= self.results.len() {
            return Ok(&[]);
        }
        let end = (start + limit).min(self.results.len());
        Ok(&self.results[start..end])
    }

    pub fn select_next(&mut self) -> Option<&VirtualSearchResultItem> {
        if self.results.is_empty() {
            return None;
        }
        let next_idx = match self.selected_index {
            Some(idx) => (idx + 1).min(self.results.len() - 1),
            None => 0,
        };
        self.selected_index = Some(next_idx);
        self.results.get(next_idx)
    }

    pub fn select_prev(&mut self) -> Option<&VirtualSearchResultItem> {
        if self.results.is_empty() {
            return None;
        }
        let prev_idx = match self.selected_index {
            Some(idx) => idx.saturating_sub(1),
            None => 0,
        };
        self.selected_index = Some(prev_idx);
        self.results.get(prev_idx)
    }
}

/// Structural elements of a document exposed for milestone jumping.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadingMilestone {
    pub level: u8,
    pub text: String,
    pub geometry: Rect2D,
    pub anchor: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableMilestone {
    pub caption: String,
    pub rows: usize,
    pub cols: usize,
    pub headers: Vec<String>,
    pub cells: Vec<Vec<String>>,
    pub geometry: Rect2D,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinkMilestone {
    pub label: String,
    pub target_url: String,
    pub geometry: Rect2D,
    pub is_visited: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceLineMilestone {
    pub line_number: u32,
    pub text: String,
    pub is_selected: bool,
    pub geometry: Rect2D,
}

/// Document semantic accessibility structure.
///
/// Enables screen readers to skip directly to headings, tables, or links
/// without traversing every individual character quad or atlas label.
#[derive(Debug, Default)]
pub struct DocumentAxStructure {
    pub headings: Vec<HeadingMilestone>,
    pub tables: Vec<TableMilestone>,
    pub links: Vec<LinkMilestone>,
    pub lines: Vec<SourceLineMilestone>,
}

impl DocumentAxStructure {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_heading(&mut self, heading: HeadingMilestone) {
        self.headings.push(heading);
    }

    pub fn add_table(&mut self, table: TableMilestone) {
        self.tables.push(table);
    }

    pub fn add_link(&mut self, link: LinkMilestone) {
        self.links.push(link);
    }

    pub fn add_line(&mut self, line: SourceLineMilestone) {
        self.lines.push(line);
    }

    pub fn next_heading(&self, current_level: Option<u8>, from_index: usize) -> Option<(usize, &HeadingMilestone)> {
        for (idx, h) in self.headings.iter().enumerate().skip(from_index) {
            if let Some(lvl) = current_level {
                if h.level <= lvl {
                    return Some((idx, h));
                }
            } else {
                return Some((idx, h));
            }
        }
        None
    }

    pub fn prev_heading(&self, current_level: Option<u8>, from_index: usize) -> Option<(usize, &HeadingMilestone)> {
        for idx in (0..from_index.min(self.headings.len())).rev() {
            let h = &self.headings[idx];
            if let Some(lvl) = current_level {
                if h.level <= lvl {
                    return Some((idx, h));
                }
            } else {
                return Some((idx, h));
            }
        }
        None
    }

    pub fn next_table(&self, from_index: usize) -> Option<(usize, &TableMilestone)> {
        self.tables.iter().enumerate().skip(from_index).next()
    }

    pub fn prev_table(&self, from_index: usize) -> Option<(usize, &TableMilestone)> {
        if from_index == 0 || self.tables.is_empty() {
            return None;
        }
        let idx = (from_index - 1).min(self.tables.len() - 1);
        Some((idx, &self.tables[idx]))
    }

    pub fn next_link(&self, from_index: usize) -> Option<(usize, &LinkMilestone)> {
        self.links.iter().enumerate().skip(from_index).next()
    }

    pub fn prev_link(&self, from_index: usize) -> Option<(usize, &LinkMilestone)> {
        if from_index == 0 || self.links.is_empty() {
            return None;
        }
        let idx = (from_index - 1).min(self.links.len() - 1);
        Some((idx, &self.links[idx]))
    }

    /// Formats a linear accessible reading flow.
    pub fn linear_reading_stream(&self) -> Vec<String> {
        let mut stream = Vec::new();
        for h in &self.headings {
            stream.push(format!("Heading level {}: {}", h.level, h.text));
        }
        for t in &self.tables {
            stream.push(format!("Table '{}': {} rows, {} columns", t.caption, t.rows, t.cols));
        }
        for l in &self.links {
            stream.push(format!("Link: {} -> {}", l.label, l.target_url));
        }
        stream
    }
}

/// City mode building accessibility representation.
#[derive(Clone, Debug, PartialEq)]
pub struct CityBuildingAxNode {
    pub name: String,
    pub file_path: String,
    pub line_count: u32,
    pub byte_size: u64,
    pub change_count: u32,
    pub height_meters: f64,
    pub status: String,
    pub geometry: Rect2D,
}

impl CityBuildingAxNode {
    pub fn accessible_description(&self) -> String {
        format!(
            "Building '{}': {} lines (height {:.0}m), {} bytes, {} modifications. Status: {}",
            self.name, self.line_count, self.height_meters, self.byte_size, self.change_count, self.status
        )
    }
}

/// City mode district accessibility representation.
#[derive(Clone, Debug, PartialEq)]
pub struct CityDistrictAxNode {
    pub name: String,
    pub path_prefix: String,
    pub buildings: Vec<CityBuildingAxNode>,
}

/// City mode non-spatial accessibility surface.
///
/// Translates spatial 3D city structures into a clean linear hierarchy
/// accessible without vision.
#[derive(Debug, Default)]
pub struct CityAxSurface {
    pub districts: Vec<CityDistrictAxNode>,
    pub selected_district: usize,
    pub selected_building: usize,
}

impl CityAxSurface {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_district(&mut self, district: CityDistrictAxNode) {
        self.districts.push(district);
    }

    pub fn current_district(&self) -> Option<&CityDistrictAxNode> {
        self.districts.get(self.selected_district)
    }

    pub fn current_building(&self) -> Option<&CityBuildingAxNode> {
        self.current_district().and_then(|d| d.buildings.get(self.selected_building))
    }

    pub fn next_building(&mut self) -> Option<&CityBuildingAxNode> {
        let d = self.districts.get(self.selected_district)?;
        if d.buildings.is_empty() {
            return None;
        }
        self.selected_building = (self.selected_building + 1).min(d.buildings.len() - 1);
        d.buildings.get(self.selected_building)
    }

    pub fn prev_building(&mut self) -> Option<&CityBuildingAxNode> {
        let d = self.districts.get(self.selected_district)?;
        if d.buildings.is_empty() {
            return None;
        }
        self.selected_building = self.selected_building.saturating_sub(1);
        d.buildings.get(self.selected_building)
    }

    pub fn next_district(&mut self) -> Option<&CityDistrictAxNode> {
        if self.districts.is_empty() {
            return None;
        }
        self.selected_district = (self.selected_district + 1).min(self.districts.len() - 1);
        self.selected_building = 0;
        self.districts.get(self.selected_district)
    }

    pub fn prev_district(&mut self) -> Option<&CityDistrictAxNode> {
        if self.districts.is_empty() {
            return None;
        }
        self.selected_district = self.selected_district.saturating_sub(1);
        self.selected_building = 0;
        self.districts.get(self.selected_district)
    }

    pub fn linear_summary(&self) -> String {
        let mut out = format!("City mode: {} districts.\n", self.districts.len());
        if let Some(b) = self.current_building() {
            out.push_str(&format!("Selected: {}\n", b.accessible_description()));
        }
        out
    }
}

/// Active surface mode for keyboard navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavSurface {
    Outline,
    Reader,
    Search,
    City,
}

/// Pointer-equivalent keyboard navigation actions (§23.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavAction {
    ScopeParent,
    ScopeChild,
    NextSibling,
    PrevSibling,
    FocusSearch,
    NextSearchResult,
    PrevSearchResult,
    ActivateSearchResult,
    OpenReader,
    PinReader,
    ToggleSourcePreview,
    FollowLink,
    BacktrackLink,
    ToggleProjection,
    InspectRelationships,
}

/// Result of executing a keyboard navigation action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavOutcome {
    FocusChanged { from: Option<SemanticNodeId>, to: SemanticNodeId },
    SelectionChanged { selected: SemanticNodeId },
    SurfaceChanged { new_surface: NavSurface },
    ReaderOpened { node: SemanticNodeId, pinned: bool },
    LinkNavigated { target: String },
    ProjectionToggled { mode: &'static str },
    NoOp { reason: &'static str },
}

/// Keyboard navigation controller providing full keyboard equivalence.
#[derive(Debug)]
pub struct KeyboardNavEngine {
    surface: NavSurface,
    focused_node: Option<SemanticNodeId>,
    selected_node: Option<SemanticNodeId>,
    focus_history: Vec<SemanticNodeId>,
    link_history: Vec<String>,
    is_reader_pinned: bool,
    source_preview_mode: bool,
}

impl KeyboardNavEngine {
    pub fn new(initial_surface: NavSurface, initial_focus: Option<SemanticNodeId>) -> Self {
        Self {
            surface: initial_surface,
            focused_node: initial_focus,
            selected_node: None,
            focus_history: Vec::new(),
            link_history: Vec::new(),
            is_reader_pinned: false,
            source_preview_mode: false,
        }
    }

    pub fn surface(&self) -> NavSurface {
        self.surface
    }

    pub fn focused_node(&self) -> Option<SemanticNodeId> {
        self.focused_node
    }

    pub fn selected_node(&self) -> Option<SemanticNodeId> {
        self.selected_node
    }

    pub fn is_reader_pinned(&self) -> bool {
        self.is_reader_pinned
    }

    pub fn set_focus(&mut self, target: SemanticNodeId) {
        if let Some(cur) = self.focused_node {
            if cur != target {
                self.focus_history.push(cur);
            }
        }
        self.focused_node = Some(target);
    }

    pub fn set_selection(&mut self, target: SemanticNodeId) {
        // Selection is independent of focus: setting selection does NOT alter focus!
        self.selected_node = Some(target);
    }

    pub fn return_focus(&mut self) -> Option<SemanticNodeId> {
        if let Some(prev) = self.focus_history.pop() {
            self.focused_node = Some(prev);
            Some(prev)
        } else {
            self.focused_node
        }
    }

    pub fn execute(
        &mut self,
        action: NavAction,
        outline: &VirtualOutlineTree,
        search: &mut VirtualSearchResults,
        doc: &DocumentAxStructure,
        city: &mut CityAxSurface,
    ) -> NavOutcome {
        match action {
            NavAction::FocusSearch => {
                self.surface = NavSurface::Search;
                NavOutcome::SurfaceChanged {
                    new_surface: NavSurface::Search,
                }
            }
            NavAction::NextSearchResult => {
                if let Some(res) = search.select_next() {
                    NavOutcome::SelectionChanged {
                        selected: res.node_id,
                    }
                } else {
                    NavOutcome::NoOp { reason: "No search results" }
                }
            }
            NavAction::PrevSearchResult => {
                if let Some(res) = search.select_prev() {
                    NavOutcome::SelectionChanged {
                        selected: res.node_id,
                    }
                } else {
                    NavOutcome::NoOp { reason: "No search results" }
                }
            }
            NavAction::ActivateSearchResult => {
                if let Some(res) = search.selected_item() {
                    let node_id = res.node_id;
                    self.set_focus(node_id);
                    self.surface = NavSurface::Reader;
                    NavOutcome::ReaderOpened {
                        node: node_id,
                        pinned: self.is_reader_pinned,
                    }
                } else {
                    NavOutcome::NoOp { reason: "No active search result to open" }
                }
            }
            NavAction::OpenReader => {
                if let Some(focused) = self.focused_node {
                    self.surface = NavSurface::Reader;
                    NavOutcome::ReaderOpened {
                        node: focused,
                        pinned: self.is_reader_pinned,
                    }
                } else {
                    NavOutcome::NoOp { reason: "No focused node to open in reader" }
                }
            }
            NavAction::PinReader => {
                self.is_reader_pinned = !self.is_reader_pinned;
                NavOutcome::NoOp {
                    reason: if self.is_reader_pinned { "Reader pinned" } else { "Reader unpinned" },
                }
            }
            NavAction::ToggleSourcePreview => {
                self.source_preview_mode = !self.source_preview_mode;
                NavOutcome::NoOp {
                    reason: if self.source_preview_mode { "Preview mode active" } else { "Source mode active" },
                }
            }
            NavAction::FollowLink => {
                if let Some((_, link)) = doc.next_link(0) {
                    self.link_history.push(link.target_url.clone());
                    NavOutcome::LinkNavigated {
                        target: link.target_url.clone(),
                    }
                } else {
                    NavOutcome::NoOp { reason: "No link available" }
                }
            }
            NavAction::BacktrackLink => {
                if let Some(prev) = self.link_history.pop() {
                    NavOutcome::LinkNavigated { target: prev }
                } else {
                    NavOutcome::NoOp { reason: "Link history empty" }
                }
            }
            NavAction::ToggleProjection => {
                self.surface = match self.surface {
                    NavSurface::Outline => NavSurface::City,
                    NavSurface::City => NavSurface::Reader,
                    NavSurface::Reader => NavSurface::Outline,
                    NavSurface::Search => NavSurface::Outline,
                };
                let mode = match self.surface {
                    NavSurface::Outline => "Outline Atlas",
                    NavSurface::City => "City 3D Projection",
                    NavSurface::Reader => "Linear Reader",
                    NavSurface::Search => "Search Panel",
                };
                NavOutcome::ProjectionToggled { mode }
            }
            NavAction::ScopeParent => {
                if self.surface == NavSurface::City {
                    if city.prev_district().is_some() {
                        return NavOutcome::ProjectionToggled { mode: "City: navigated to previous district" };
                    }
                    return NavOutcome::NoOp { reason: "Already at first district" };
                }
                // Find current node index in outline and navigate to parent
                let cur_idx = self.find_current_outline_index(outline);
                if let Some(parent_idx) = cur_idx.and_then(|i| outline.parent_index(i)) {
                    if let Some(entry) = outline.entries.get(parent_idx) {
                        let from = self.focused_node;
                        self.set_focus(entry.node_id);
                        return NavOutcome::FocusChanged { from, to: entry.node_id };
                    }
                }
                NavOutcome::NoOp { reason: "No parent in scope" }
            }
            NavAction::ScopeChild => {
                if self.surface == NavSurface::City {
                    if city.next_district().is_some() {
                        return NavOutcome::ProjectionToggled { mode: "City: navigated to next district" };
                    }
                    return NavOutcome::NoOp { reason: "Already at last district" };
                }
                let cur_idx = self.find_current_outline_index(outline);
                if let Some(child_idx) = cur_idx.and_then(|i| outline.first_child_index(i)) {
                    if let Some(entry) = outline.entries.get(child_idx) {
                        let from = self.focused_node;
                        self.set_focus(entry.node_id);
                        return NavOutcome::FocusChanged { from, to: entry.node_id };
                    }
                }
                NavOutcome::NoOp { reason: "No child in scope" }
            }
            NavAction::NextSibling => {
                if self.surface == NavSurface::City {
                    if city.next_building().is_some() {
                        return NavOutcome::ProjectionToggled { mode: "City: navigated to next building" };
                    }
                    return NavOutcome::NoOp { reason: "Already at last building" };
                }
                let cur_idx = self.find_current_outline_index(outline);
                if let Some(sib_idx) = cur_idx.and_then(|i| outline.next_sibling_index(i)) {
                    if let Some(entry) = outline.entries.get(sib_idx) {
                        let from = self.focused_node;
                        self.set_focus(entry.node_id);
                        return NavOutcome::FocusChanged { from, to: entry.node_id };
                    }
                }
                NavOutcome::NoOp { reason: "No next sibling" }
            }
            NavAction::PrevSibling => {
                if self.surface == NavSurface::City {
                    if city.prev_building().is_some() {
                        return NavOutcome::ProjectionToggled { mode: "City: navigated to previous building" };
                    }
                    return NavOutcome::NoOp { reason: "Already at first building" };
                }
                let cur_idx = self.find_current_outline_index(outline);
                if let Some(sib_idx) = cur_idx.and_then(|i| outline.prev_sibling_index(i)) {
                    if let Some(entry) = outline.entries.get(sib_idx) {
                        let from = self.focused_node;
                        self.set_focus(entry.node_id);
                        return NavOutcome::FocusChanged { from, to: entry.node_id };
                    }
                }
                NavOutcome::NoOp { reason: "No previous sibling" }
            }
            NavAction::InspectRelationships => {
                NavOutcome::NoOp { reason: "Displaying relationships (callers/callees)" }
            }
        }
    }

    fn find_current_outline_index(&self, outline: &VirtualOutlineTree) -> Option<usize> {
        let focused = self.focused_node?;
        outline.entries.iter().position(|e| e.node_id == focused)
    }
}

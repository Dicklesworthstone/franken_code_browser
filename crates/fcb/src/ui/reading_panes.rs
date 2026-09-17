//! Reading lens floating pane management and side-by-side pinning.
//!
//! Under §5.3:
//! - Dragging inside a reading surface selects text.
//! - Dragging its title bar moves the pane on screen without changing atlas layout.
//! - Pinning two or more files permits side-by-side reading.
//! - Closing / Escape backs out of the active unpinned lens without disturbing pinned panes.

#![forbid(unsafe_code)]

use fcb_core::FileId;

/// Individual floating reading lens pane.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadingPane {
    pub id: u64,
    pub file_id: FileId,
    pub path: String,
    pub position: (f32, f32),
    pub size: (f32, f32),
    pub is_pinned: bool,
    pub scroll_offset: (f32, f32),
    pub target_line: Option<usize>,
    pub selection: Option<(usize, usize)>,
}

impl ReadingPane {
    pub fn new(id: u64, file_id: FileId, path: String, position: (f32, f32)) -> Self {
        Self {
            id,
            file_id,
            path,
            position,
            size: (640.0, 480.0),
            is_pinned: false,
            scroll_offset: (0.0, 0.0),
            target_line: None,
            selection: None,
        }
    }
}

/// Manages multiple reading panes, active focus, and side-by-side pinning.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadingPaneManager {
    panes: Vec<ReadingPane>,
    active_pane_id: Option<u64>,
    next_id: u64,
}

impl ReadingPaneManager {
    pub fn new() -> Self {
        Self {
            panes: Vec::new(),
            active_pane_id: None,
            next_id: 1,
        }
    }

    pub fn is_any_open(&self) -> bool {
        !self.panes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.panes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }

    pub fn active_pane_id(&self) -> Option<u64> {
        self.active_pane_id
    }

    pub fn set_active_pane_id(&mut self, id: Option<u64>) {
        self.active_pane_id = id;
    }

    pub fn active_pane(&self) -> Option<&ReadingPane> {
        self.active_pane_id.and_then(|id| self.get_pane(id))
    }

    pub fn active_pane_mut(&mut self) -> Option<&mut ReadingPane> {
        let id = self.active_pane_id?;
        self.get_pane_mut(id)
    }

    pub fn get_pane(&self, id: u64) -> Option<&ReadingPane> {
        self.panes.iter().find(|p| p.id == id)
    }

    pub fn get_pane_mut(&mut self, id: u64) -> Option<&mut ReadingPane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ReadingPane> {
        self.panes.iter()
    }

    pub fn pinned_count(&self) -> usize {
        self.panes.iter().filter(|p| p.is_pinned).count()
    }

    pub fn unpinned_count(&self) -> usize {
        self.panes.iter().filter(|p| !p.is_pinned).count()
    }

    /// Open a file in a reading pane, or focus it if already open.
    ///
    /// If an existing pane has matching `file_id`, it is promoted to active and its target line updated.
    /// If opening a new pane and existing panes exist, positions are staggered so they remain accessible.
    pub fn open_or_focus(&mut self, file_id: FileId, path: String, line: Option<usize>) -> u64 {
        if let Some(existing) = self.panes.iter_mut().find(|p| p.file_id == file_id) {
            existing.target_line = line;
            let id = existing.id;
            self.active_pane_id = Some(id);
            return id;
        }

        // Stagger new pane position
        let offset = (self.panes.len() as f32) * 30.0;
        let pos = (80.0 + offset, 60.0 + offset);

        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        let mut pane = ReadingPane::new(id, file_id, path, pos);
        pane.target_line = line;

        self.panes.push(pane);
        self.active_pane_id = Some(id);
        id
    }

    /// Set pinning state on a pane.
    pub fn pin_pane(&mut self, id: u64) -> bool {
        if let Some(pane) = self.get_pane_mut(id) {
            pane.is_pinned = true;
            true
        } else {
            false
        }
    }

    /// Unpin a pane.
    pub fn unpin_pane(&mut self, id: u64) -> bool {
        if let Some(pane) = self.get_pane_mut(id) {
            pane.is_pinned = false;
            true
        } else {
            false
        }
    }

    /// Toggle pin state.
    pub fn toggle_pin(&mut self, id: u64) -> bool {
        if let Some(pane) = self.get_pane_mut(id) {
            pane.is_pinned = !pane.is_pinned;
            true
        } else {
            false
        }
    }

    /// Translate a reading pane position by `(dx, dy)`.
    ///
    /// Moving a pane does NOT change the atlas layout!
    pub fn translate_pane(&mut self, id: u64, dx: f32, dy: f32) -> bool {
        if let Some(pane) = self.get_pane_mut(id) {
            pane.position.0 = (pane.position.0 + dx).max(0.0);
            pane.position.1 = (pane.position.1 + dy).max(0.0);
            true
        } else {
            false
        }
    }

    /// Scroll inside a reading pane.
    pub fn scroll_pane(&mut self, id: u64, dx: f32, dy: f32) -> bool {
        if let Some(pane) = self.get_pane_mut(id) {
            pane.scroll_offset.0 = (pane.scroll_offset.0 + dx).max(0.0);
            pane.scroll_offset.1 = (pane.scroll_offset.1 + dy).max(0.0);
            true
        } else {
            false
        }
    }

    /// Set text selection range on a reading pane.
    pub fn set_text_selection(&mut self, id: u64, range: Option<(usize, usize)>) -> bool {
        if let Some(pane) = self.get_pane_mut(id) {
            pane.selection = range;
            true
        } else {
            false
        }
    }

    /// Close a specific pane by id.
    pub fn close_pane(&mut self, id: u64) -> Option<ReadingPane> {
        if let Some(pos) = self.panes.iter().position(|p| p.id == id) {
            let removed = self.panes.remove(pos);
            if self.active_pane_id == Some(id) {
                self.active_pane_id = self.panes.last().map(|p| p.id);
            }
            Some(removed)
        } else {
            None
        }
    }

    /// Close the active unpinned pane (e.g. on Escape).
    /// Pinned panes remain open!
    pub fn close_active_or_top_unpinned(&mut self) -> Option<u64> {
        // If active pane is unpinned, close it
        if let Some(active_id) = self.active_pane_id {
            if let Some(pane) = self.get_pane(active_id) {
                if !pane.is_pinned {
                    self.close_pane(active_id);
                    return Some(active_id);
                }
            }
        }

        // Otherwise close the topmost unpinned pane
        if let Some(top_unpinned_id) = self
            .panes
            .iter()
            .rev()
            .find(|p| !p.is_pinned)
            .map(|p| p.id)
        {
            self.close_pane(top_unpinned_id);
            return Some(top_unpinned_id);
        }

        None
    }
}

impl Default for ReadingPaneManager {
    fn default() -> Self {
        Self::new()
    }
}

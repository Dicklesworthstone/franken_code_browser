#![forbid(unsafe_code)]

//! Conventional hierarchical outline / file tree projection.
//!
//! An alternate projection of the spatial repository model for precision
//! keyboard navigation and screen-reader accessibility. Flattens into a
//! virtualized visible row list respecting node expansion state.

use fcb_core::FileId;

/// Unique identifier of a tree node within the projection.
pub type TreeNodeId = u64;

/// Kind of tree node (directory container vs source file leaf).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeNodeKind {
    Directory,
    File,
}

/// A node in the conventional file tree projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeNode {
    pub id: TreeNodeId,
    pub parent_id: Option<TreeNodeId>,
    pub name: String,
    pub path: String,
    pub kind: TreeNodeKind,
    pub depth: usize,
    pub is_expanded: bool,
    pub file_id: Option<FileId>,
}

impl TreeNode {
    pub const fn is_directory(&self) -> bool {
        matches!(self.kind, TreeNodeKind::Directory)
    }

    pub const fn is_file(&self) -> bool {
        matches!(self.kind, TreeNodeKind::File)
    }
}

/// Errors originating from tree navigation or manipulation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeError {
    NodeNotFound,
    CannotExpandFile,
}

/// The conventional hierarchical tree projection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TreeProjection {
    nodes: Vec<TreeNode>,
    visible_rows: Vec<TreeNodeId>,
    selected_id: Option<TreeNodeId>,
    focused_id: Option<TreeNodeId>,
}

impl TreeProjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_nodes(nodes: Vec<TreeNode>) -> Self {
        let mut tree = Self {
            nodes,
            visible_rows: Vec::new(),
            selected_id: None,
            focused_id: None,
        };
        tree.rebuild_visible_rows();
        if let Some(&first) = tree.visible_rows.first() {
            tree.focused_id = Some(first);
        }
        tree
    }

    pub fn nodes(&self) -> &[TreeNode] {
        &self.nodes
    }

    pub fn visible_rows(&self) -> &[TreeNodeId] {
        &self.visible_rows
    }

    pub fn selected_id(&self) -> Option<TreeNodeId> {
        self.selected_id
    }

    pub fn focused_id(&self) -> Option<TreeNodeId> {
        self.focused_id
    }

    pub fn get_node(&self, id: TreeNodeId) -> Option<&TreeNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    fn get_node_mut(&mut self, id: TreeNodeId) -> Option<&mut TreeNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn find_by_path(&self, path: &str) -> Option<&TreeNode> {
        self.nodes.iter().find(|n| n.path == path)
    }

    /// Rebuilds the flattened list of visible rows based on parent expansion state.
    pub fn rebuild_visible_rows(&mut self) {
        let mut visible = Vec::new();
        self.collect_visible(None, &mut visible);
        self.visible_rows = visible;
    }

    fn collect_visible(&self, parent_id: Option<TreeNodeId>, out: &mut Vec<TreeNodeId>) {
        let children: Vec<&TreeNode> = self
            .nodes
            .iter()
            .filter(|n| n.parent_id == parent_id)
            .collect();

        for child in children {
            out.push(child.id);
            if child.is_directory() && child.is_expanded {
                self.collect_visible(Some(child.id), out);
            }
        }
    }

    /// Expand a directory node. Returns error if node is a file.
    pub fn expand(&mut self, id: TreeNodeId) -> Result<(), TreeError> {
        let node = self.get_node_mut(id).ok_or(TreeError::NodeNotFound)?;
        if !node.is_directory() {
            return Err(TreeError::CannotExpandFile);
        }
        node.is_expanded = true;
        self.rebuild_visible_rows();
        Ok(())
    }

    /// Collapse a directory node.
    pub fn collapse(&mut self, id: TreeNodeId) -> Result<(), TreeError> {
        let node = self.get_node_mut(id).ok_or(TreeError::NodeNotFound)?;
        if !node.is_directory() {
            return Err(TreeError::CannotExpandFile);
        }
        node.is_expanded = false;
        self.rebuild_visible_rows();
        Ok(())
    }

    /// Toggle expansion state of a directory node.
    pub fn toggle(&mut self, id: TreeNodeId) -> Result<(), TreeError> {
        let node = self.get_node(id).ok_or(TreeError::NodeNotFound)?;
        if node.is_expanded {
            self.collapse(id)
        } else {
            self.expand(id)
        }
    }

    /// Select a specific node by ID.
    pub fn select(&mut self, id: TreeNodeId) -> Option<&TreeNode> {
        if self.nodes.iter().any(|n| n.id == id) {
            self.selected_id = Some(id);
            self.focused_id = Some(id);
            self.get_node(id)
        } else {
            None
        }
    }

    /// Set focus to the previous visible row.
    pub fn move_up(&mut self) -> Option<&TreeNode> {
        if self.visible_rows.is_empty() {
            return None;
        }
        let curr_idx = self
            .focused_id
            .and_then(|id| self.visible_rows.iter().position(|&row_id| row_id == id));

        let next_idx = match curr_idx {
            Some(idx) => idx.saturating_sub(1),
            None => 0,
        };

        let target_id = *self.visible_rows.get(next_idx)?;
        self.focused_id = Some(target_id);
        self.get_node(target_id)
    }

    /// Set focus to the next visible row.
    pub fn move_down(&mut self) -> Option<&TreeNode> {
        if self.visible_rows.is_empty() {
            return None;
        }
        let curr_idx = self
            .focused_id
            .and_then(|id| self.visible_rows.iter().position(|&row_id| row_id == id));

        let next_idx = match curr_idx {
            Some(idx) => (idx + 1).min(self.visible_rows.len().saturating_sub(1)),
            None => 0,
        };

        let target_id = *self.visible_rows.get(next_idx)?;
        self.focused_id = Some(target_id);
        self.get_node(target_id)
    }

    /// Handle Left arrow: if expanded directory, collapse it; if collapsed or file, jump to parent.
    pub fn move_left(&mut self) -> Option<&TreeNode> {
        let focused_id = self.focused_id?;
        let node = self.get_node(focused_id)?;

        if node.is_directory() && node.is_expanded {
            let _ = self.collapse(focused_id);
            self.get_node(focused_id)
        } else if let Some(parent_id) = node.parent_id {
            self.focused_id = Some(parent_id);
            self.get_node(parent_id)
        } else {
            Some(node)
        }
    }

    /// Handle Right arrow: if collapsed directory, expand it; if expanded, move to first child.
    pub fn move_right(&mut self) -> Option<&TreeNode> {
        let focused_id = self.focused_id?;
        let node = self.get_node(focused_id)?;

        if node.is_directory() && !node.is_expanded {
            let _ = self.expand(focused_id);
            self.get_node(focused_id)
        } else if node.is_directory() && node.is_expanded {
            // Find first child
            if let Some(first_child) = self.nodes.iter().find(|n| n.parent_id == Some(focused_id)) {
                self.focused_id = Some(first_child.id);
                Some(first_child)
            } else {
                Some(node)
            }
        } else {
            Some(node)
        }
    }
}

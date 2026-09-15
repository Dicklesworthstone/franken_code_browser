#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};

use crate::{
    geometry::{DisplayMetrics, Point2D, Rect2D, SemanticGeometry},
    ArenaOwnerId, CoreError, DisplayGeneration, LayoutRevision, PresentedFrameId, SemanticNodeId,
    SourceRevision, Utf16CodeUnitRange,
};

/// The semantic accessibility role of a layout element.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum SemanticRole {
    Document,
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
}

/// A node in the accepted semantic layout tree with geometry and accessibility properties.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticNode {
    id: SemanticNodeId,
    geometry: SemanticGeometry,
    parent: Option<SemanticNodeId>,
    children: Vec<SemanticNodeId>,
    role: SemanticRole,
    label: Option<String>,
    value: Option<String>,
    text_range: Option<Utf16CodeUnitRange>,
    focusable: bool,
}

impl SemanticNode {
    pub fn new(
        id: SemanticNodeId,
        geometry: SemanticGeometry,
        role: SemanticRole,
        focusable: bool,
    ) -> Self {
        Self {
            id,
            geometry,
            parent: None,
            children: Vec::new(),
            role,
            label: None,
            value: None,
            text_range: None,
            focusable,
        }
    }

    pub const fn id(&self) -> SemanticNodeId {
        self.id
    }

    pub const fn geometry(&self) -> &SemanticGeometry {
        &self.geometry
    }

    pub const fn parent(&self) -> Option<SemanticNodeId> {
        self.parent
    }

    pub fn set_parent(&mut self, parent: Option<SemanticNodeId>) -> Result<(), CoreError> {
        if parent.is_some_and(|p| p.owner() != self.id.owner()) {
            return Err(CoreError::OwnershipMismatch);
        }
        self.parent = parent;
        Ok(())
    }

    pub fn children(&self) -> &[SemanticNodeId] {
        &self.children
    }

    pub fn add_child(&mut self, child: SemanticNodeId) -> Result<(), CoreError> {
        if child.owner() != self.id.owner() {
            return Err(CoreError::OwnershipMismatch);
        }
        self.children.push(child);
        Ok(())
    }

    pub const fn role(&self) -> SemanticRole {
        self.role
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub fn set_label(&mut self, label: Option<String>) {
        self.label = label;
    }

    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    pub fn set_value(&mut self, value: Option<String>) {
        self.value = value;
    }

    pub const fn text_range(&self) -> Option<Utf16CodeUnitRange> {
        self.text_range
    }

    pub fn set_text_range(&mut self, range: Option<Utf16CodeUnitRange>) {
        self.text_range = range;
    }

    pub const fn is_focusable(&self) -> bool {
        self.focusable
    }

    pub fn set_focusable(&mut self, focusable: bool) {
        self.focusable = focusable;
    }
}

/// The exact multi-dimensional identity of an accepted semantic layout.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AcceptedLayoutIdentity {
    owner: ArenaOwnerId,
    layout_revision: LayoutRevision,
    source_revision: SourceRevision,
    display_generation: DisplayGeneration,
    presented_frame: Option<PresentedFrameId>,
}

impl AcceptedLayoutIdentity {
    pub fn new(
        owner: ArenaOwnerId,
        layout_revision: LayoutRevision,
        source_revision: SourceRevision,
        display_generation: DisplayGeneration,
        presented_frame: Option<PresentedFrameId>,
    ) -> Result<Self, CoreError> {
        if layout_revision.owner() != owner
            || source_revision.owner() != owner
            || display_generation.owner() != owner
        {
            return Err(CoreError::OwnershipMismatch);
        }
        if presented_frame.is_some_and(|frame| frame.owner() != owner) {
            return Err(CoreError::OwnershipMismatch);
        }
        Ok(Self {
            owner,
            layout_revision,
            source_revision,
            display_generation,
            presented_frame,
        })
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn layout_revision(self) -> LayoutRevision {
        self.layout_revision
    }

    pub const fn source_revision(self) -> SourceRevision {
        self.source_revision
    }

    pub const fn display_generation(self) -> DisplayGeneration {
        self.display_generation
    }

    pub const fn presented_frame(self) -> Option<PresentedFrameId> {
        self.presented_frame
    }

    /// Validates this layout identity against the current publication context or state.
    pub fn validate_against(&self, current: &AcceptedLayoutIdentity) -> Result<(), CoreError> {
        if self.owner != current.owner {
            return Err(CoreError::OwnershipMismatch);
        }
        if self.layout_revision != current.layout_revision {
            return Err(CoreError::StaleLayoutRevision);
        }
        if self.source_revision != current.source_revision {
            return Err(CoreError::StaleSourceRevision);
        }
        if self.display_generation != current.display_generation {
            return Err(CoreError::StaleDisplayGeneration);
        }
        Ok(())
    }
}

/// An accepted, immutable semantic layout snapshot available for hit testing,
/// accessibility querying, and focus targeting.
#[derive(Clone, Debug, PartialEq)]
pub struct AcceptedLayoutSnapshot {
    identity: AcceptedLayoutIdentity,
    metrics: DisplayMetrics,
    root_node: SemanticNodeId,
    nodes: BTreeMap<SemanticNodeId, SemanticNode>,
}

impl AcceptedLayoutSnapshot {
    pub fn new(
        identity: AcceptedLayoutIdentity,
        metrics: DisplayMetrics,
        root_node: SemanticNodeId,
        nodes: BTreeMap<SemanticNodeId, SemanticNode>,
    ) -> Result<Self, CoreError> {
        if root_node.owner() != identity.owner() {
            return Err(CoreError::OwnershipMismatch);
        }
        if !nodes.contains_key(&root_node) {
            return Err(CoreError::NodeNotFound);
        }
        if metrics.generation() != identity.display_generation() {
            return Err(CoreError::StaleDisplayGeneration);
        }
        for (id, node) in &nodes {
            if id.owner() != identity.owner() || node.id().owner() != identity.owner() {
                return Err(CoreError::OwnershipMismatch);
            }
            if node.parent().is_some_and(|p| !nodes.contains_key(&p)) {
                return Err(CoreError::NodeNotFound);
            }
            for child in node.children() {
                if !nodes.contains_key(child) {
                    return Err(CoreError::NodeNotFound);
                }
            }
        }
        Ok(Self {
            identity,
            metrics,
            root_node,
            nodes,
        })
    }

    pub const fn identity(&self) -> &AcceptedLayoutIdentity {
        &self.identity
    }

    pub const fn metrics(&self) -> &DisplayMetrics {
        &self.metrics
    }

    pub const fn root_node(&self) -> SemanticNodeId {
        self.root_node
    }

    pub fn node(&self, id: SemanticNodeId) -> Option<&SemanticNode> {
        self.nodes.get(&id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn visible_rect_for(&self, id: SemanticNodeId) -> Option<Rect2D> {
        self.nodes.get(&id).and_then(|n| n.geometry().visible_rect())
    }

    /// Hierarchical hit-test starting from the root node. Traverses children
    /// in reverse order (front-to-back paint order) and returns the deepest
    /// leaf or matching node.
    pub fn hit_test(&self, point: Point2D) -> Option<SemanticNodeId> {
        self.hit_test_node(self.root_node, point)
    }

    fn hit_test_node(&self, current: SemanticNodeId, point: Point2D) -> Option<SemanticNodeId> {
        let node = self.nodes.get(&current)?;
        if !node.geometry().contains_hit(point) {
            return None;
        }

        // Test children in reverse paint order (last child drawn on top)
        for child in node.children().iter().rev() {
            if let Some(hit) = self.hit_test_node(*child, point) {
                return Some(hit);
            }
        }

        // If no child was hit, this node itself is the hit target
        Some(current)
    }

    /// Returns all focusable node IDs in document/tree order.
    pub fn focusable_nodes(&self) -> Vec<SemanticNodeId> {
        let mut focusable = Vec::new();
        self.collect_focusable(self.root_node, &mut focusable);
        focusable
    }

    fn collect_focusable(&self, current: SemanticNodeId, out: &mut Vec<SemanticNodeId>) {
        if let Some(node) = self.nodes.get(&current) {
            if node.is_focusable() {
                out.push(current);
            }
            for child in node.children() {
                self.collect_focusable(*child, out);
            }
        }
    }
}

/// Bounded semantic focus state and focus-return stack for native embedding.
/// Preserves the host's responder hierarchy without leaking focus or dropping
/// focus return targets when ephemeral overlays dismiss.
pub struct SemanticFocusState {
    owner: ArenaOwnerId,
    current_focus: Option<SemanticNodeId>,
    focus_stack: VecDeque<SemanticNodeId>,
    max_stack_depth: usize,
}

impl SemanticFocusState {
    pub const DEFAULT_MAX_STACK_DEPTH: usize = 32;

    pub fn new(owner: ArenaOwnerId, max_stack_depth: usize) -> Self {
        let max_stack_depth = if max_stack_depth == 0 {
            Self::DEFAULT_MAX_STACK_DEPTH
        } else {
            max_stack_depth
        };
        Self {
            owner,
            current_focus: None,
            focus_stack: VecDeque::with_capacity(max_stack_depth),
            max_stack_depth,
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn current_focus(&self) -> Option<SemanticNodeId> {
        self.current_focus
    }

    pub fn focus_stack_depth(&self) -> usize {
        self.focus_stack.len()
    }

    /// Moves focus to `target`. If `target` is already focused, this is a no-op.
    /// Otherwise, pushes the previous focus target onto the focus stack for focus return.
    pub fn focus_node(
        &mut self,
        target: SemanticNodeId,
        layout: &AcceptedLayoutSnapshot,
    ) -> Result<(), CoreError> {
        if target.owner() != self.owner || layout.identity().owner() != self.owner {
            return Err(CoreError::OwnershipMismatch);
        }

        let node = layout.node(target).ok_or(CoreError::NodeNotFound)?;
        if !node.is_focusable() {
            return Err(CoreError::FocusTargetNotFound);
        }

        if self.current_focus == Some(target) {
            return Ok(());
        }

        if let Some(prev) = self.current_focus {
            if self.focus_stack.len() >= self.max_stack_depth {
                // Drop oldest target from front
                let _ = self.focus_stack.pop_front();
            }
            self.focus_stack.push_back(prev);
        }

        self.current_focus = Some(target);
        Ok(())
    }

    /// Clears the current focus without altering the focus-return stack.
    pub fn blur(&mut self) {
        self.current_focus = None;
    }

    /// Returns focus to the most recent valid candidate on the focus stack.
    /// Pops from the stack until a node is found that still exists and is focusable
    /// in `layout`. If none is valid or the stack is empty, sets current focus to None.
    pub fn return_focus(
        &mut self,
        layout: &AcceptedLayoutSnapshot,
    ) -> Result<Option<SemanticNodeId>, CoreError> {
        if layout.identity().owner() != self.owner {
            return Err(CoreError::OwnershipMismatch);
        }

        while let Some(candidate) = self.focus_stack.pop_back() {
            if candidate.owner() == self.owner
                && layout.node(candidate).is_some_and(|node| node.is_focusable())
            {
                self.current_focus = Some(candidate);
                return Ok(Some(candidate));
            }
        }

        self.current_focus = None;
        Ok(None)
    }

    /// Returns the bounding rectangle of the currently focused node in `layout`
    /// (for native focus ring drawing in AppKit/Metal).
    pub fn focus_bounds(&self, layout: &AcceptedLayoutSnapshot) -> Option<Rect2D> {
        let focused_id = self.current_focus?;
        layout.visible_rect_for(focused_id)
    }
}

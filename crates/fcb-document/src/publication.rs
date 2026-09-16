#![forbid(unsafe_code)]

use fcb_core::{DocumentGeneration, DocumentId};
use fcb_source::ObservationDigest;
use franken_markdown::{
    AccessibleReadingNode, DisplayItem, DisplayRect,
};

use crate::display_adapter::DocumentDisplayPlan;
use crate::error::DocumentError;

/// Retained layout metrics computed from the display plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetainedLayoutMetrics {
    /// Enclosing bounds of the formatted document.
    pub total_bounds: DisplayRect,
    /// Total number of display items in the retained display plan.
    pub total_items: usize,
    /// Total number of accessible semantic reading nodes.
    pub total_reading_nodes: usize,
    /// Number of unresolved external assets pending in this publication.
    pub unresolved_assets: usize,
}

/// A published, immutable document presentation ready for the retained renderer.
///
/// Encapsulates the display plan, view generation, observation digest, and layout metrics.
/// Holds no GPU resources, textures, or platform-specific windows.
/// Stale publications are rejected without presentation.
#[derive(Clone, Debug)]
pub struct DocumentPublication {
    document_id: DocumentId,
    generation: DocumentGeneration,
    digest: ObservationDigest,
    display_plan: DocumentDisplayPlan,
    metrics: RetainedLayoutMetrics,
}

impl DocumentPublication {
    /// Creates a new publication from a verified display plan and observation digest.
    pub fn new(
        document_id: DocumentId,
        generation: DocumentGeneration,
        digest: ObservationDigest,
        display_plan: DocumentDisplayPlan,
    ) -> Result<Self, DocumentError> {
        // Enforce generation match with display plan
        display_plan.validate_delivery(generation)?;

        let metrics = RetainedLayoutMetrics {
            total_bounds: display_plan.bounds(),
            total_items: display_plan.item_count(),
            total_reading_nodes: display_plan.reading_node_count(),
            unresolved_assets: display_plan.unresolved_assets().len(),
        };

        Ok(Self {
            document_id,
            generation,
            digest,
            display_plan,
            metrics,
        })
    }

    pub fn document_id(&self) -> DocumentId {
        self.document_id
    }

    pub fn generation(&self) -> DocumentGeneration {
        self.generation
    }

    pub fn digest(&self) -> ObservationDigest {
        self.digest
    }

    pub fn display_plan(&self) -> &DocumentDisplayPlan {
        &self.display_plan
    }

    pub fn metrics(&self) -> RetainedLayoutMetrics {
        self.metrics
    }

    pub fn total_bounds(&self) -> DisplayRect {
        self.metrics.total_bounds
    }

    /// Whether this publication is complete (all assets resolved).
    pub fn is_complete(&self) -> bool {
        self.display_plan.is_complete()
    }

    /// Whether this publication is stale relative to a newer generation.
    pub fn is_stale_for(&self, current_generation: DocumentGeneration) -> bool {
        self.generation != current_generation
    }

    /// Validates that an incoming presentation request matches this publication's generation.
    pub fn validate_presentation(&self, expected_generation: DocumentGeneration) -> Result<(), DocumentError> {
        if self.generation != expected_generation {
            return Err(DocumentError::StaleRequest {
                expected: expected_generation,
                actual: self.generation,
            });
        }
        Ok(())
    }

    /// Slices visible display items that intersect `viewport` for retained rendering.
    ///
    /// Avoids whole-document rendering or full layout traversal.
    pub fn visible_items(&self, viewport: DisplayRect) -> Vec<&DisplayItem> {
        self.display_plan
            .items()
            .iter()
            .filter(|item| rects_intersect(item.bounds(), viewport))
            .collect()
    }

    /// Borrow the accessible reading hierarchy for screen readers and semantic focus.
    pub fn reading_order(&self) -> &[AccessibleReadingNode] {
        self.display_plan.reading_order()
    }
}

/// Returns true if two display rectangles overlap/intersect.
#[inline]
#[must_use]
pub fn rects_intersect(a: DisplayRect, b: DisplayRect) -> bool {
    a.x < b.right() && a.right() > b.x && a.y < b.bottom() && a.bottom() > b.y
}

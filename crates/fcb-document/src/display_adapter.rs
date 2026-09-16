#![forbid(unsafe_code)]

use fcb_core::DocumentGeneration;
use franken_markdown::{
    AccessibleReadingNode, AssetRequest, DisplayItem, DisplayList,
};

use crate::error::DocumentError;

/// Presentation-ready document display plan derived from upstream flow output.
///
/// Contains renderer-neutral drawing primitives and accessibility reading structure.
/// Holds no GPU resources, textures, or platform-specific handles.
#[derive(Clone, Debug)]
pub struct DocumentDisplayPlan {
    generation: DocumentGeneration,
    display_list: DisplayList,
    unresolved_assets: Vec<AssetRequest>,
}

impl DocumentDisplayPlan {
    /// Creates a display plan from an upstream `DisplayList` and optional unresolved assets.
    pub fn new(
        generation: DocumentGeneration,
        display_list: DisplayList,
        unresolved_assets: Vec<AssetRequest>,
    ) -> Self {
        Self {
            generation,
            display_list,
            unresolved_assets,
        }
    }

    pub fn generation(&self) -> DocumentGeneration {
        self.generation
    }

    pub fn display_list(&self) -> &DisplayList {
        &self.display_list
    }

    pub fn items(&self) -> &[DisplayItem] {
        self.display_list.items()
    }

    pub fn reading_order(&self) -> &[AccessibleReadingNode] {
        self.display_list.reading_order()
    }

    pub fn bounds(&self) -> (f32, f32) {
        self.display_list.bounds()
    }

    pub fn unresolved_assets(&self) -> &[AssetRequest] {
        &self.unresolved_assets
    }

    /// Whether all assets have been resolved and the display plan is complete.
    pub fn is_complete(&self) -> bool {
        self.unresolved_assets.is_empty()
    }

    /// Number of renderer-neutral display items.
    pub fn item_count(&self) -> usize {
        self.display_list.items().len()
    }

    /// Number of semantic reading nodes for accessibility.
    pub fn reading_node_count(&self) -> usize {
        self.display_list.reading_order().len()
    }

    /// Validates that an incoming render or presentation request matches this plan's generation.
    pub fn validate_delivery(&self, expected_generation: DocumentGeneration) -> Result<(), DocumentError> {
        if self.generation != expected_generation {
            return Err(DocumentError::StaleRequest {
                expected: expected_generation,
                actual: self.generation,
            });
        }
        Ok(())
    }
}

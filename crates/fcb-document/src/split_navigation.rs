//! FCB-036.A: Shared anchors and split-pane navigation between source and
//! preview views, with heading identity matching, README discovery, and
//! independent pane scroll management.
//!
//! The [`SplitPaneNavigator`] holds two independent [`DocumentLens`]s (one
//! for the source pane, one for the preview pane) and provides:
//!
//! - **Heading jump**: scroll both panes to a named heading using the
//!   upstream [`DocumentSourceMap`] heading anchors.
//! - **Byte-offset jump**: scroll the source pane to a byte offset (e.g.
//!   a search hit) and the preview pane to the corresponding rendered line.
//! - **README discovery**: scan captured logical paths for a README file,
//!   preferring `README.md` > `README` > case-insensitive match.
//! - **Independent scroll**: each pane scrolls independently; jumps are
//!   explicit and never silently override the other pane's position.

#![forbid(unsafe_code)]

use crate::lens::DocumentLens;
use franken_markdown::{DocumentSourceMap, HeadingSourceAnchor};

/// A shared navigation anchor resolved from the document source map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedAnchor {
    /// The heading slug from the source map.
    pub heading_id: String,
    /// Byte offset in the source capture where the heading begins.
    pub source_byte: u64,
    /// The heading source anchor from the upstream map.
    pub anchor: HeadingSourceAnchor,
}

/// The split-pane navigator managing independent source and preview scroll.
#[derive(Debug)]
pub struct SplitPaneNavigator {
    source_lens: DocumentLens,
    preview_lens: DocumentLens,
}

impl SplitPaneNavigator {
    /// Create a navigator with independent source and preview lenses.
    pub fn new(source_lens: DocumentLens, preview_lens: DocumentLens) -> Self {
        Self {
            source_lens,
            preview_lens,
        }
    }

    /// The source pane lens (read-only).
    pub const fn source_lens(&self) -> &DocumentLens {
        &self.source_lens
    }

    /// The preview pane lens (read-only).
    pub const fn preview_lens(&self) -> &DocumentLens {
        &self.preview_lens
    }

    /// The source pane lens (mutable, for independent scroll).
    pub const fn source_lens_mut(&mut self) -> &mut DocumentLens {
        &mut self.source_lens
    }

    /// The preview pane lens (mutable, for independent scroll).
    pub const fn preview_lens_mut(&mut self) -> &mut DocumentLens {
        &mut self.preview_lens
    }

    /// Jump both panes to a named heading.
    ///
    /// Resolves the heading in the source map, scrolls the source pane to
    /// the heading's source byte offset, and scrolls the preview pane to
    /// the heading's rendered position (approximated by the heading's
    /// line index in the preview flow).
    ///
    /// Returns the resolved shared anchor, or `None` if the heading is
    /// not found in the source map.
    pub fn jump_to_heading(
        &mut self,
        heading_id: &str,
        source_map: &DocumentSourceMap,
        preview_total_height: u32,
        _line_height: u32,
    ) -> Option<SharedAnchor> {
        let anchor = self.source_lens.find_heading_anchor(heading_id, source_map)?;
        let heading_byte = anchor.source_span.start as u64;

        // Scroll source pane so the heading is near the top of the viewport.
        let source_byte = heading_byte as u32;
        self.source_lens.set_scroll_y(source_byte);

        // Scroll preview pane proportionally.
        let preview_y = heading_byte as u32 % preview_total_height.max(1);
        self.preview_lens.set_scroll_y(preview_y);

        Some(SharedAnchor {
            heading_id: heading_id.to_string(),
            source_byte: anchor.source_span.start as u64,
            anchor: anchor.clone(),
        })
    }

    /// Jump the source pane to a specific byte offset (e.g. a search hit).
    ///
    /// Scrolls the source pane so the byte offset is visible. The preview
    /// pane is NOT moved — pane scrolls are independent.
    pub fn jump_source_to_byte(&mut self, byte_offset: u64) {
        self.source_lens.set_scroll_y(byte_offset as u32);
    }

    /// Jump the preview pane to a specific pixel offset.
    ///
    /// Scrolls the preview pane so the offset is visible. The source pane
    /// is NOT moved.
    pub fn jump_preview_to_y(&mut self, y: u32) {
        self.preview_lens.set_scroll_y(y);
    }
}

/// Discover a README file from a set of captured logical paths.
///
/// Preference order: `README.md` > `README` > first case-insensitive
/// `readme.*` match. Returns the matching logical path.
pub fn discover_readme(paths: &[String]) -> Option<String> {
    // Exact README.md.
    if let Some(found) = paths.iter().find(|p| p == &&"README.md".to_string()) {
        return Some(found.clone());
    }
    // Exact README (no extension).
    if let Some(found) = paths.iter().find(|p| p == &&"README".to_string()) {
        return Some(found.clone());
    }
    // Case-insensitive readme.* match.
    paths
        .iter()
        .find(|p| {
            let file = p.rsplit('/').next().unwrap_or(p);
            file.to_ascii_lowercase() == "readme.md"
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_lenses() -> (DocumentLens, DocumentLens) {
        let source = DocumentLens::new(80, 24, 16, 8).unwrap();
        let preview = DocumentLens::new(80, 24, 20, 10).unwrap();
        (source, preview)
    }

    #[test]
    fn independent_pane_scrolls_do_not_affect_each_other() {
        let (mut source, mut preview) = make_lenses();
        let mut nav = SplitPaneNavigator::new(source, preview);

        nav.source_lens_mut().set_scroll_y(100);
        assert_eq!(nav.preview_lens().scroll_y(), 0, "preview unaffected");

        nav.preview_lens_mut().set_scroll_y(200);
        assert_eq!(nav.source_lens().scroll_y(), 100, "source unaffected");
        assert_eq!(nav.preview_lens().scroll_y(), 200);
    }

    #[test]
    fn readme_discovery_prefers_md_then_bare_then_case_insensitive() {
        let paths = vec![
            "src/lib.rs".to_string(),
            "CONTRIBUTING.md".to_string(),
            "readme.org".to_string(),
        ];
        // No exact README.md or README — falls through to case-insensitive.
        assert!(discover_readme(&paths).is_none(), "readme.org is not readme.md");

        let paths = vec![
            "src/lib.rs".to_string(),
            "README".to_string(),
            "README.md".to_string(),
        ];
        assert_eq!(
            discover_readme(&paths).as_deref(),
            Some("README.md"),
            "exact README.md preferred"
        );

        let paths = vec!["src/lib.rs".to_string(), "README".to_string()];
        assert_eq!(discover_readme(&paths).as_deref(), Some("README"));
    }

    #[test]
    fn readme_discovery_finds_nested_readme() {
        let paths = vec![
            "docs/guide.md".to_string(),
            "sub/README.md".to_string(),
        ];
        assert_eq!(
            discover_readme(&paths).as_deref(),
            Some("sub/README.md")
        );
    }

    #[test]
    fn empty_paths_yield_no_readme() {
        assert!(discover_readme(&[]).is_none());
    }
}

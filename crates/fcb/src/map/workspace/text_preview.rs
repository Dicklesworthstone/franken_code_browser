#![forbid(unsafe_code)]

//! Bounded logical source previews from an atlas hit's retained capture.
//! No file is reopened. Decoding, original-byte maps and scalar/CRLF boundary
//! handling come from the existing extent reader, not a second text engine.

use fcb_core::{ByteLength, ByteOffset, ByteRange, DecodedUtf8Range, QueryGeneration,
    ResourceAllocationId, ResourceBudget};
use fcb_source::{CompleteCapture, RootGrant, SourceError, detect_encoding};
use crate::BrowserSession;
use crate::search::{ExtentError, ExtentText, ExtentViewError, ObservedExtent};
use super::text_search::{AtlasTextHit, AtlasTextOverlay, WorkspaceTextError, WorkspaceTextSource};

pub const MAX_ATLAS_CONTEXT_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasTextPreviewOptions {
    /// Requested bytes on EACH side of the entire match. Small bounded edge
    /// padding preserves scalar and CRLF boundaries; a further four-byte
    /// decoder halo is retained but not necessarily displayed.
    pub context_bytes: usize,
}
impl Default for AtlasTextPreviewOptions {
    fn default() -> Self { Self { context_bytes: 256 } }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AtlasTextPreviewError {
    Search(WorkspaceTextError), View(ExtentViewError), Source(SourceError),
    InvalidLimits, WrongSelection, Canceled,
}
impl std::fmt::Display for AtlasTextPreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Search(e) => write!(f, "{e}"), Self::View(e) => write!(f, "{e}"),
            Self::Source(e) => write!(f, "{e}"),
            Self::InvalidLimits => f.write_str("ATLAS_PREVIEW_INVALID_LIMITS"),
            Self::WrongSelection => f.write_str("ATLAS_PREVIEW_WRONG_SELECTION"),
            Self::Canceled => f.write_str("ATLAS_PREVIEW_CANCELED"),
        }
    }
}
impl std::error::Error for AtlasTextPreviewError {}
impl From<WorkspaceTextError> for AtlasTextPreviewError {
    fn from(e: WorkspaceTextError) -> Self { Self::Search(e) }
}
impl From<ExtentViewError> for AtlasTextPreviewError {
    fn from(e: ExtentViewError) -> Self { Self::View(e) }
}
impl From<ExtentError> for AtlasTextPreviewError {
    fn from(e: ExtentError) -> Self { Self::View(ExtentViewError::Extent(e)) }
}
impl From<SourceError> for AtlasTextPreviewError {
    fn from(e: SourceError) -> Self { Self::Source(e) }
}

pub struct AtlasTextPreview<'capture> {
    capture: &'capture CompleteCapture,
    grant: &'capture RootGrant,
    hit: AtlasTextHit,
    hit_index: usize,
    text: ExtentText,
    selected: DecodedUtf8Range,
}
impl AtlasTextPreview<'_> {
    pub fn hit(&self) -> AtlasTextHit { self.hit }
    pub fn hit_index(&self) -> usize { self.hit_index }
    pub fn text(&self) -> &ExtentText { &self.text }
    pub fn selected_text_range(&self) -> DecodedUtf8Range { self.selected }
    pub fn prefix_bytes_omitted(&self) -> bool { self.text.range().start().get() != 0 }
    pub fn suffix_bytes_omitted(&self) -> bool { self.text.range().end().get() < self.capture.bytes().len() as u64 }
    /// A preview cannot silently become a different occurrence or capture,
    /// including an independently supplied capture with equal numeric IDs.
    pub fn validate_delivery(&self, overlay: &AtlasTextOverlay<'_, '_, '_>,
        source: &WorkspaceTextSource<'_, '_>, generation: QueryGeneration)
        -> Result<(), AtlasTextPreviewError> {
        self.grant.validate_active()?;
        let selected = overlay.select_hit(source, self.hit_index, generation)?;
        if generation != self.text.generation() || selected.hit() != self.hit
            || !std::ptr::eq(selected.capture(), self.capture) {
            return Err(AtlasTextPreviewError::WrongSelection);
        }
        Ok(())
    }
}

impl<'index, 'source, 'catalog> AtlasTextOverlay<'index, 'source, 'catalog> {
    /// Materialize one bounded reader window, preserving the ENTIRE selected
    /// occurrence even when context cuts through UTF-8, UTF-16 or a CRLF pair.
    /// Allocations are [owned original extent, decoder/output]. Existing source
    /// and query leases remain separate; no full-file copy or prefix scan occurs.
    /// This is logical text, not qualified paragraph shaping or native pixels.
    pub fn preview_hit(&self, source: &WorkspaceTextSource<'_, '_>, position: usize,
        generation: QueryGeneration, options: AtlasTextPreviewOptions,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
        mut canceled: impl FnMut() -> bool) -> Result<AtlasTextPreview<'_>, AtlasTextPreviewError> {
        if options.context_bytes > MAX_ATLAS_CONTEXT_BYTES || allocations[0] == allocations[1] {
            return Err(AtlasTextPreviewError::InvalidLimits);
        }
        let selection = self.select_hit(source, position, generation)?;
        if canceled() { return Err(AtlasTextPreviewError::Canceled); }
        let capture = selection.capture();
        let hit = selection.hit();
        let length = capture.bytes().len() as u64;
        let encoding = detect_encoding(capture.bytes());
        let context = options.context_bytes as u64;
        let mut start = hit.original_range().start().get().saturating_sub(context).saturating_sub(4);
        let mut end = hit.original_range().end().get().saturating_add(context).saturating_add(4).min(length);
        // UTF-16 units are aligned to the original file origin, not this window.
        if encoding.is_utf16() {
            start -= start % 2;
            if end % 2 != 0 { end = end.saturating_add(1).min(length); }
        }
        let visible = range(start, end)?;
        if visible.len().get() > crate::search::extents::MAX_EXTENT_TEXT_BYTES as u64 {
            return Err(AtlasTextPreviewError::InvalidLimits);
        }
        let captured = range(start.saturating_sub(4), end.saturating_add(4).min(length))?;
        let (first, last) = captured.as_usize_bounds().map_err(|_| AtlasTextPreviewError::WrongSelection)?;
        let raw = capture.bytes().get(first..last).ok_or(AtlasTextPreviewError::WrongSelection)?;
        let request = (*capture.request()).with_range(captured)?;
        let extent = ObservedExtent::from_bytes(request, ByteLength::new(length), raw, budget, allocations[0])?;
        let view = BrowserSession::new(hit.file().owner()).open_extent(extent)?;
        let text = view.decode(visible, encoding, generation, budget, allocations[1], &mut canceled)?;
        let selected = text.source_to_text(hit.original_range())?;
        // This is an exact logical selection, not merely a plausible excerpt.
        let decoded = text.text_selection(selected)?;
        if decoded.original_range != hit.original_range() || decoded.original_bytes != selection.original_bytes()
            || decoded.text != selection.matched_text() || decoded.contains_replacements {
            return Err(AtlasTextPreviewError::WrongSelection);
        }
        self.validate_delivery(source, generation)?;
        if canceled() { return Err(AtlasTextPreviewError::Canceled); }
        Ok(AtlasTextPreview { capture, grant: self.source().atlas().catalog().grant(),
            hit, hit_index: position, text, selected })
    }
}
fn range(start: u64, end: u64) -> Result<ByteRange, AtlasTextPreviewError> {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| AtlasTextPreviewError::WrongSelection)
}

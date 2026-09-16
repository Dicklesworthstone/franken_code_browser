#![forbid(unsafe_code)]

use fcb_core::{ByteOffset, ByteRange};
use fcb_source::CompleteCapture;
use franken_markdown::{DocumentSourceMap, HeadingSourceAnchor, SourceSpan};

use crate::error::DocumentError;
use crate::session::DocumentFlowLine;

/// Interactive viewing lens managing viewport geometry, scroll offset, and line visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentLens {
    viewport_width: u32,
    viewport_height: u32,
    scroll_y: u32,
    line_height: u32,
    char_width: u32,
}

impl DocumentLens {
    pub fn new(
        viewport_width: u32,
        viewport_height: u32,
        line_height: u32,
        char_width: u32,
    ) -> Result<Self, DocumentError> {
        if viewport_width == 0 || viewport_height == 0 || line_height == 0 || char_width == 0 {
            return Err(DocumentError::InvalidRange);
        }
        Ok(Self {
            viewport_width,
            viewport_height,
            scroll_y: 0,
            line_height,
            char_width,
        })
    }

    pub fn viewport_width(&self) -> u32 {
        self.viewport_width
    }

    pub fn viewport_height(&self) -> u32 {
        self.viewport_height
    }

    pub fn scroll_y(&self) -> u32 {
        self.scroll_y
    }

    pub fn line_height(&self) -> u32 {
        self.line_height
    }

    pub fn char_width(&self) -> u32 {
        self.char_width
    }

    pub fn set_scroll_y(&mut self, scroll_y: u32) {
        self.scroll_y = scroll_y;
    }

    /// Computes the clamped visible vertical interval `(start_y, end_y)`.
    pub fn visible_y_range(&self, total_height: u32) -> (u32, u32) {
        let start_y = self.scroll_y.min(total_height);
        let end_y = self.scroll_y.saturating_add(self.viewport_height).min(total_height);
        (start_y, end_y)
    }

    /// Slices flow lines visible within the current viewport window.
    pub fn visible_lines<'a>(&self, lines: &'a [DocumentFlowLine]) -> &'a [DocumentFlowLine] {
        if lines.is_empty() {
            return &[];
        }

        let view_top = self.scroll_y;
        let view_bottom = self.scroll_y.saturating_add(self.viewport_height);

        let first = lines
            .partition_point(|l| l.y_offset.saturating_add(l.height) < view_top);
        let last = lines
            .partition_point(|l| l.y_offset <= view_bottom);

        if first < last && first < lines.len() {
            &lines[first..last.min(lines.len())]
        } else {
            &[]
        }
    }

    /// Resolves a heading ID to its source anchor using the upstream document source map.
    pub fn find_heading_anchor<'a>(
        &self,
        heading_id: &str,
        source_map: &'a DocumentSourceMap,
    ) -> Option<&'a HeadingSourceAnchor> {
        source_map.find_heading(heading_id)
    }

    /// Translates an upstream `SourceSpan` into an authoritative FCB `ByteRange`.
    pub fn map_source_span_to_byte_range(
        span: SourceSpan,
        capture: &CompleteCapture,
    ) -> Result<ByteRange, DocumentError> {
        let len = capture.bytes().len();
        if span.start > span.end || span.end > len {
            return Err(DocumentError::InvalidRange);
        }

        let start = ByteOffset::new(span.start as u64);
        let end = ByteOffset::new(span.end as u64);

        ByteRange::new(start, end).map_err(|_| DocumentError::InvalidRange)
    }
}

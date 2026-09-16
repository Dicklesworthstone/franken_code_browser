#![forbid(unsafe_code)]

use fcb_core::{ByteOffset, ByteRange};
use fcb_source::CompleteCapture;
use franken_markdown::{
    DocumentSourceMap, NestedProvenanceGraph, ProvenanceAuditReport, ProvenanceOracle,
    TextSelectionRange,
};

use crate::error::DocumentError;

/// Resolved user selection distinguishing rendered reading text from exact source ranges.
///
/// Plan §12.4: The caller must not invent contiguous source by concatenating
/// unrelated spans and presenting it as an original literal slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionResolution {
    pub reading_text: String,
    pub source_ranges: Vec<ByteRange>,
    pub is_disjoint: bool,
}

/// Verifies provenance truthfulness over the document's nested provenance graph.
///
/// Ensures no contiguous slices were invented and all spans resolve within the source.
pub fn verify_provenance_truthfulness(
    graph: &NestedProvenanceGraph,
    source: &str,
) -> Result<ProvenanceAuditReport, DocumentError> {
    ProvenanceOracle::verify_truthfulness(graph, source, &|_| None).map_err(DocumentError::from)
}

/// Resolves a reading text selection into distinct reading text and exact source byte ranges.
pub fn resolve_reading_selection(
    selection: TextSelectionRange,
    source_map: &DocumentSourceMap,
    capture: &CompleteCapture,
) -> Result<SelectionResolution, DocumentError> {
    let source_len = capture.bytes().len();
    let reading_text = source_map.copy_rendered_text(selection)?;

    let mut source_ranges = Vec::new();
    for elem in source_map.elements() {
        if elem.rendered_range.start < selection.end && elem.rendered_range.end > selection.start {
            for span in elem.source_ranges.spans() {
                if span.start <= span.end && span.end <= source_len {
                    let start = ByteOffset::new(span.start as u64);
                    let end = ByteOffset::new(span.end as u64);
                    if let Ok(range) = ByteRange::new(start, end) {
                        if !source_ranges.contains(&range) {
                            source_ranges.push(range);
                        }
                    }
                }
            }
        }
    }

    // Determine if the collected ranges are disjoint (non-adjacent)
    let is_disjoint = if source_ranges.len() <= 1 {
        false
    } else {
        source_ranges
            .windows(2)
            .any(|w| w[0].end() != w[1].start())
    };

    Ok(SelectionResolution {
        reading_text,
        source_ranges,
        is_disjoint,
    })
}

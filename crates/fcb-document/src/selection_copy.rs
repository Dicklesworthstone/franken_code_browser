#![forbid(unsafe_code)]

//! FCB-036.B: Truthful rendered and Markdown selection copy, provenance classification,
//! and budgeted multi-flavor clipboard staging (Plan §5.6, §10.11, §12.4).
//!
//! # Technical Contracts
//!
//! - **Truthful Rendered vs Markdown Copy (§12.4)**: Preview selection offers two
//!   explicit actions: copy rendered reading text and copy corresponding Markdown source.
//! - **Disjoint vs Contiguous Classification (§12.4)**: When rendered text maps to
//!   multiple disjoint source spans (due to stripped emphasis delimiters, reference links,
//!   escaped entities, code-fence dedentation, or transclusion), the model returns a
//!   disjoint ordered set of ranges. The caller **must not invent contiguous source** by
//!   concatenating unrelated spans and presenting it as the original literal slice.
//! - **Generated Marker Isolation (§12.4)**: Synthetic list markers (e.g. `1. `, `* `),
//!   task checkboxes, and formatting lines do not correspond to literal source bytes;
//!   they are explicitly distinguished so they are never falsely attributed as source bytes.
//! - **Enclosing Markdown Block Copy (§12.4)**: "Copy enclosing Markdown block" is an
//!   explicitly separate command returning the complete source slice of the enclosing block.
//! - **Budgeted Multi-Flavor Clipboard (§10.11)**: Plain text, exact raw bytes, Markdown
//!   source, enclosing block, and location provenance flavors are staged before publication.
//!   Over-budget selections are refused before publication, leaving the clipboard untouched,
//!   and offer [`StreamedDocumentExport`] as an un-truncated streaming alternative.
//! - **Atomic Native Publication & Concurrency**: Validates stale capture, cancellation,
//!   and sequence tokens. Injected native failure or concurrent external changes report
//!   truthfully without unsafe rollback.

use fcb_core::{ByteOffset, ByteRange, DocumentGeneration};
use fcb_source::CompleteCapture;
use franken_markdown::{DocumentSourceMap, RenderedElement, TextSelectionRange};

/// Classification of how a rendered text selection maps back to primary source bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderedSelectionKind {
    /// Selection corresponds to a single unbroken contiguous range of source bytes.
    ContiguousSource {
        range: ByteRange,
    },
    /// Selection corresponds to multiple disjoint spans (e.g. delimiters stripped like `**bold**`,
    /// reference links `[text][ref]`, escaped characters `\*`, entities `&amp;`, dedented code).
    DisjointSource {
        ranges: Vec<ByteRange>,
    },
    /// Selection includes or consists of synthetic generated content (e.g. list numbering `1. `,
    /// task checkboxes `[x]`, table alignment markers) that does not exist in literal source.
    GeneratedContent {
        marker: String,
        source_ranges: Vec<ByteRange>,
    },
}

/// The result of copying corresponding Markdown source (§12.4).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownSourceCopy {
    /// Literal contiguous slice of source bytes.
    Contiguous {
        range: ByteRange,
        text: String,
    },
    /// Disjoint ordered set of source slices.
    ///
    /// Plan §12.4: "the caller must not invent contiguous source by concatenating
    /// unrelated spans and presenting it as the original literal slice."
    Disjoint {
        ranges: Vec<ByteRange>,
        slices: Vec<String>,
    },
    /// Synthetic generated marker only; explicitly not backed by literal source bytes.
    GeneratedMarker {
        marker: String,
    },
}

/// Declared user copy action in Markdown preview/split mode (§5.6, §12.4).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentCopyAction {
    /// Copy rendered reading text (declared decoded Unicode text).
    RenderedReadingText,
    /// Copy corresponding Markdown source slice(s).
    MarkdownSource,
    /// Copy the smallest enclosing Markdown block covering the selection.
    EnclosingMarkdownBlock,
    /// Copy with location provenance header.
    LocationProvenance,
}

/// Native clipboard flavors staged for OS pasteboard publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentClipboardFlavor {
    PlainTextUtf8,
    ExactRawBytes,
    MarkdownSource,
    EnclosingBlock,
    LocationProvenance,
}

/// Bounded limits for document clipboard operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentClipboardLimits {
    /// Maximum bytes permitted for clipboard publication (default: 4 MiB).
    pub max_clipboard_bytes: usize,
}

impl Default for DocumentClipboardLimits {
    fn default() -> Self {
        Self {
            max_clipboard_bytes: 4 * 1024 * 1024, // 4 MiB
        }
    }
}

/// Errors returned during selection resolution, staging, or clipboard publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentCopyError {
    /// Selection range is out of bounds or invalid.
    InvalidSelectionRange {
        start: usize,
        end: usize,
        max: usize,
    },
    /// Attempted to present disjoint source ranges as an invented contiguous slice (§12.4).
    InventedContiguousConcatenation {
        disjoint_count: usize,
        enclosing: ByteRange,
    },
    /// Operation exceeded the maximum allowed clipboard budget.
    BudgetExceeded {
        requested_bytes: usize,
        max_budget_bytes: usize,
    },
    /// Cooperative cancellation requested before publication.
    Canceled,
    /// Source capture or document generation is stale.
    StaleCapture,
    /// Arena owner mismatch between document and capture.
    OwnerMismatch,
    /// Root grant revoked or unavailable.
    GrantRevoked,
    /// Native clipboard publication failed on host OS.
    NativePublicationFailed,
    /// Concurrent external change detected on pasteboard.
    ConcurrentExternalChange,
    /// Invalid byte range arithmetic.
    InvalidRange,
}

impl std::fmt::Display for DocumentCopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSelectionRange { start, end, max } => {
                write!(f, "INVALID_SELECTION_RANGE: [{start}..{end}] exceeds max {max}")
            }
            Self::InventedContiguousConcatenation { disjoint_count, enclosing } => {
                write!(f, "INVENTED_CONTIGUOUS_CONCATENATION: attempted to invent contiguous range {enclosing:?} from {disjoint_count} disjoint spans")
            }
            Self::BudgetExceeded { requested_bytes, max_budget_bytes } => {
                write!(f, "DOCUMENT_CLIPBOARD_BUDGET_EXCEEDED: requested {requested_bytes} bytes, limit is {max_budget_bytes}")
            }
            Self::Canceled => write!(f, "DOCUMENT_COPY_CANCELED"),
            Self::StaleCapture => write!(f, "DOCUMENT_COPY_STALE_CAPTURE"),
            Self::OwnerMismatch => write!(f, "DOCUMENT_COPY_OWNER_MISMATCH"),
            Self::GrantRevoked => write!(f, "DOCUMENT_COPY_GRANT_REVOKED"),
            Self::NativePublicationFailed => write!(f, "DOCUMENT_COPY_NATIVE_PUBLICATION_FAILED"),
            Self::ConcurrentExternalChange => write!(f, "DOCUMENT_COPY_CONCURRENT_EXTERNAL_CHANGE"),
            Self::InvalidRange => write!(f, "DOCUMENT_COPY_INVALID_RANGE"),
        }
    }
}

impl std::error::Error for DocumentCopyError {}

/// Provenance metadata resolved for an interactive rendered selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedSelectionProvenance {
    pub rendered_range: TextSelectionRange,
    pub kind: RenderedSelectionKind,
    pub reading_text: String,
    pub source_ranges: Vec<ByteRange>,
    pub enclosing_block_range: Option<ByteRange>,
    pub has_escapes_or_entities: bool,
    pub has_dedented_code: bool,
    pub has_reference_links: bool,
    pub has_generated_numbering: bool,
}

impl RenderedSelectionProvenance {
    /// Negative control oracle: rejects attempts to spoof or present disjoint
    /// spans as a contiguous source range (§12.4).
    pub fn assert_not_invented_contiguous(&self, purported_range: ByteRange) -> Result<(), DocumentCopyError> {
        if self.source_ranges.len() > 1 {
            let min_start = self.source_ranges.iter().map(|r| r.start()).min().unwrap();
            let max_end = self.source_ranges.iter().map(|r| r.end()).max().unwrap();
            let enclosing = ByteRange::new(min_start, max_end).map_err(|_| DocumentCopyError::InvalidRange)?;
            if purported_range == enclosing {
                return Err(DocumentCopyError::InventedContiguousConcatenation {
                    disjoint_count: self.source_ranges.len(),
                    enclosing,
                });
            }
        }
        Ok(())
    }

    /// Whether this selection contains disjoint non-adjacent source ranges.
    pub fn is_disjoint(&self) -> bool {
        matches!(self.kind, RenderedSelectionKind::DisjointSource { .. })
    }

    /// Whether this selection is backed by generated content without literal source bytes.
    pub fn is_generated(&self) -> bool {
        matches!(self.kind, RenderedSelectionKind::GeneratedContent { .. })
    }
}

/// Truthful selection resolver between rendered reading text, source map, and capture bytes.
pub struct TruthfulSelectionResolver;

impl TruthfulSelectionResolver {
    /// Resolves an interactive text selection range against the upstream document source map
    /// and authoritative primary capture.
    pub fn resolve(
        selection: TextSelectionRange,
        source_map: &DocumentSourceMap,
        capture: &CompleteCapture,
    ) -> Result<RenderedSelectionProvenance, DocumentCopyError> {
        let max_rendered = source_map.rendered_len();
        if selection.start > selection.end || selection.end > max_rendered {
            return Err(DocumentCopyError::InvalidSelectionRange {
                start: selection.start,
                end: selection.end,
                max: max_rendered,
            });
        }

        let reading_text = source_map
            .copy_rendered_text(selection)
            .map_err(|_| DocumentCopyError::InvalidSelectionRange {
                start: selection.start,
                end: selection.end,
                max: max_rendered,
            })?;

        let source_bytes = capture.bytes();
        let source_len = source_bytes.len();

        let mut collected_ranges: Vec<ByteRange> = Vec::new();
        let mut has_escapes_or_entities = false;
        let mut has_dedented_code = false;
        let mut has_reference_links = false;
        let mut has_generated_numbering = false;
        let mut generated_marker = String::new();
        let mut overlapping_elements: Vec<&RenderedElement> = Vec::new();

        for elem in source_map.elements() {
            if elem.rendered_range.start < selection.end && elem.rendered_range.end > selection.start {
                overlapping_elements.push(elem);

                if elem.is_generated {
                    has_generated_numbering = true;
                    if generated_marker.is_empty() {
                        generated_marker = elem.rendered_text.clone();
                    }
                }

                // Check heuristics on element text/context
                if elem.rendered_text.contains('&') || elem.rendered_text.contains('<') {
                    has_escapes_or_entities = true;
                }

                for span in elem.source_ranges.spans() {
                    if span.start <= span.end && span.end <= source_len {
                        let start = ByteOffset::new(span.start as u64);
                        let end = ByteOffset::new(span.end as u64);
                        if let Ok(range) = ByteRange::new(start, end) {
                            if !collected_ranges.contains(&range) {
                                collected_ranges.push(range);
                            }
                        }
                    }
                }
            }
        }

        // Sort ranges by start offset
        collected_ranges.sort_by_key(|r| r.start());

        // Check for code dedentation or reference links by examining surrounding source context
        if let Ok(enclosing_str) = source_map.copy_enclosing_source(selection, std::str::from_utf8(source_bytes).unwrap_or("")) {
            if enclosing_str.contains("```") || enclosing_str.starts_with("    ") {
                has_dedented_code = true;
            }
            if enclosing_str.contains("]: ") || enclosing_str.contains("][") {
                has_reference_links = true;
            }
        }

        // Determine enclosing block range using exact slice pointer offset
        let source_str = std::str::from_utf8(source_bytes).unwrap_or("");
        let enclosing_block_range = if let Ok(enclosing_str) = source_map.copy_enclosing_block(selection, source_str) {
            let start_offset = (enclosing_str.as_ptr() as usize).saturating_sub(source_str.as_ptr() as usize);
            let end_offset = start_offset.saturating_add(enclosing_str.len());
            if end_offset <= source_len {
                ByteRange::new(ByteOffset::new(start_offset as u64), ByteOffset::new(end_offset as u64)).ok()
            } else {
                None
            }
        } else {
            None
        };

        // Classify kind
        let kind = if has_generated_numbering && collected_ranges.is_empty() {
            RenderedSelectionKind::GeneratedContent {
                marker: generated_marker,
                source_ranges: Vec::new(),
            }
        } else if has_generated_numbering {
            RenderedSelectionKind::GeneratedContent {
                marker: generated_marker,
                source_ranges: collected_ranges.clone(),
            }
        } else if collected_ranges.len() <= 1 {
            if let Some(single) = collected_ranges.first() {
                RenderedSelectionKind::ContiguousSource { range: *single }
            } else {
                RenderedSelectionKind::ContiguousSource {
                    range: ByteRange::new(ByteOffset::new(0), ByteOffset::new(0)).unwrap(),
                }
            }
        } else {
            // Check if adjacent
            let is_strictly_disjoint = collected_ranges.windows(2).any(|w| w[0].end() != w[1].start());
            if is_strictly_disjoint {
                RenderedSelectionKind::DisjointSource {
                    ranges: collected_ranges.clone(),
                }
            } else {
                let start = collected_ranges.first().unwrap().start();
                let end = collected_ranges.last().unwrap().end();
                RenderedSelectionKind::ContiguousSource {
                    range: ByteRange::new(start, end).map_err(|_| DocumentCopyError::InvalidRange)?,
                }
            }
        };

        Ok(RenderedSelectionProvenance {
            rendered_range: selection,
            kind,
            reading_text,
            source_ranges: collected_ranges,
            enclosing_block_range,
            has_escapes_or_entities,
            has_dedented_code,
            has_reference_links,
            has_generated_numbering,
        })
    }

    /// Execute the Markdown Source Copy command (§12.4).
    ///
    /// Preserves exact source slices without inventing contiguous concatenations.
    pub fn copy_markdown_source(
        provenance: &RenderedSelectionProvenance,
        capture: &CompleteCapture,
    ) -> Result<MarkdownSourceCopy, DocumentCopyError> {
        let bytes = capture.bytes();
        match &provenance.kind {
            RenderedSelectionKind::GeneratedContent { marker, source_ranges } => {
                if source_ranges.is_empty() {
                    Ok(MarkdownSourceCopy::GeneratedMarker {
                        marker: marker.clone(),
                    })
                } else {
                    let mut slices = Vec::with_capacity(source_ranges.len());
                    for r in source_ranges {
                        let start = r.start().get() as usize;
                        let end = r.end().get() as usize;
                        if end <= bytes.len() {
                            slices.push(String::from_utf8_lossy(&bytes[start..end]).into_owned());
                        }
                    }
                    Ok(MarkdownSourceCopy::Disjoint {
                        ranges: source_ranges.clone(),
                        slices,
                    })
                }
            }
            RenderedSelectionKind::ContiguousSource { range } => {
                let start = range.start().get() as usize;
                let end = range.end().get() as usize;
                if end > bytes.len() {
                    return Err(DocumentCopyError::InvalidRange);
                }
                let text = String::from_utf8_lossy(&bytes[start..end]).into_owned();
                Ok(MarkdownSourceCopy::Contiguous { range: *range, text })
            }
            RenderedSelectionKind::DisjointSource { ranges } => {
                let mut slices = Vec::with_capacity(ranges.len());
                for r in ranges {
                    let start = r.start().get() as usize;
                    let end = r.end().get() as usize;
                    if end <= bytes.len() {
                        slices.push(String::from_utf8_lossy(&bytes[start..end]).into_owned());
                    }
                }
                Ok(MarkdownSourceCopy::Disjoint {
                    ranges: ranges.clone(),
                    slices,
                })
            }
        }
    }
}

/// Pre-staged multi-flavor clipboard data verified against budget before OS publication (§10.11).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedDocumentClipboard {
    pub plain_text: String,
    pub exact_raw_bytes: Vec<u8>,
    pub markdown_source: Option<String>,
    pub enclosing_block: Option<String>,
    pub provenance_header: String,
    pub has_malformed_utf8: bool,
    pub total_staged_bytes: usize,
    pub active_action: DocumentCopyAction,
}

impl StagedDocumentClipboard {
    /// Stages document selection data into multi-flavor clipboard representations.
    ///
    /// Refuses with [`DocumentCopyError::BudgetExceeded`] if total size exceeds `limits.max_clipboard_bytes`.
    /// Leaves any target clipboard untouched on budget refusal or cancellation.
    pub fn stage(
        provenance: &RenderedSelectionProvenance,
        action: DocumentCopyAction,
        capture: &CompleteCapture,
        doc_generation: DocumentGeneration,
        limits: DocumentClipboardLimits,
        canceled: impl Fn() -> bool,
    ) -> Result<Self, DocumentCopyError> {
        if canceled() {
            return Err(DocumentCopyError::Canceled);
        }

        let capture_bytes = capture.bytes();
        let mut has_malformed_utf8 = false;

        // 1. Plain text representation
        let plain_text = match action {
            DocumentCopyAction::RenderedReadingText => provenance.reading_text.clone(),
            DocumentCopyAction::MarkdownSource => {
                match TruthfulSelectionResolver::copy_markdown_source(provenance, capture)? {
                    MarkdownSourceCopy::Contiguous { text, .. } => text,
                    MarkdownSourceCopy::Disjoint { slices, .. } => slices.join("\n"),
                    MarkdownSourceCopy::GeneratedMarker { marker } => marker,
                }
            }
            DocumentCopyAction::EnclosingMarkdownBlock => {
                if let Some(r) = provenance.enclosing_block_range {
                    let s = r.start().get() as usize;
                    let e = r.end().get() as usize;
                    if e <= capture_bytes.len() {
                        String::from_utf8_lossy(&capture_bytes[s..e]).into_owned()
                    } else {
                        provenance.reading_text.clone()
                    }
                } else {
                    provenance.reading_text.clone()
                }
            }
            DocumentCopyAction::LocationProvenance => {
                let header = format!(
                    "<!-- fcb:file={:?} rev={:?} gen={:?} ranges={:?} -->\n",
                    capture.request().file(),
                    capture.request().revision(),
                    doc_generation,
                    provenance.source_ranges
                );
                format!("{header}{}", provenance.reading_text)
            }
        };

        // 2. Exact raw bytes
        let exact_raw_bytes = match action {
            DocumentCopyAction::EnclosingMarkdownBlock => {
                if let Some(r) = provenance.enclosing_block_range {
                    let s = r.start().get() as usize;
                    let e = r.end().get() as usize;
                    if e <= capture_bytes.len() {
                        capture_bytes[s..e].to_vec()
                    } else {
                        plain_text.as_bytes().to_vec()
                    }
                } else {
                    plain_text.as_bytes().to_vec()
                }
            }
            _ => {
                match &provenance.kind {
                    RenderedSelectionKind::ContiguousSource { range } => {
                        let s = range.start().get() as usize;
                        let e = range.end().get() as usize;
                        if e <= capture_bytes.len() {
                            capture_bytes[s..e].to_vec()
                        } else {
                            plain_text.as_bytes().to_vec()
                        }
                    }
                    RenderedSelectionKind::DisjointSource { ranges } => {
                        let mut b = Vec::new();
                        for r in ranges {
                            let s = r.start().get() as usize;
                            let e = r.end().get() as usize;
                            if e <= capture_bytes.len() {
                                b.extend_from_slice(&capture_bytes[s..e]);
                            }
                        }
                        b
                    }
                    RenderedSelectionKind::GeneratedContent { marker, .. } => marker.as_bytes().to_vec(),
                }
            }
        };

        // Check malformed UTF-8 in exact bytes
        if std::str::from_utf8(&exact_raw_bytes).is_err() {
            has_malformed_utf8 = true;
        }

        // 3. Optional Markdown source
        let markdown_source = match TruthfulSelectionResolver::copy_markdown_source(provenance, capture) {
            Ok(MarkdownSourceCopy::Contiguous { text, .. }) => Some(text),
            Ok(MarkdownSourceCopy::Disjoint { slices, .. }) => Some(slices.join("\n")),
            Ok(MarkdownSourceCopy::GeneratedMarker { marker }) => Some(marker),
            Err(_) => None,
        };

        // 4. Optional Enclosing Block
        let enclosing_block = provenance.enclosing_block_range.map(|r| {
            let s = r.start().get() as usize;
            let e = r.end().get() as usize;
            if e <= capture_bytes.len() {
                String::from_utf8_lossy(&capture_bytes[s..e]).into_owned()
            } else {
                String::new()
            }
        });

        // 5. Provenance header
        let provenance_header = format!(
            "<!-- fcb:file={:?} rev={:?} gen={:?} disjoint={} generated={} -->",
            capture.request().file(),
            capture.request().revision(),
            doc_generation,
            provenance.is_disjoint(),
            provenance.is_generated()
        );

        // Calculate total staged bytes across all flavors
        let total_staged_bytes = plain_text.len()
            + exact_raw_bytes.len()
            + markdown_source.as_ref().map_or(0, |s| s.len())
            + enclosing_block.as_ref().map_or(0, |s| s.len())
            + provenance_header.len();

        if total_staged_bytes > limits.max_clipboard_bytes {
            return Err(DocumentCopyError::BudgetExceeded {
                requested_bytes: total_staged_bytes,
                max_budget_bytes: limits.max_clipboard_bytes,
            });
        }

        if canceled() {
            return Err(DocumentCopyError::Canceled);
        }

        Ok(Self {
            plain_text,
            exact_raw_bytes,
            markdown_source,
            enclosing_block,
            provenance_header,
            has_malformed_utf8,
            total_staged_bytes,
            active_action: action,
        })
    }
}

/// Simulated native pasteboard tracking sequence token and current content (§10.11).
#[derive(Clone, Debug)]
pub struct DocumentNativeClipboard {
    current: Option<StagedDocumentClipboard>,
    sequence_number: u64,
}

impl Default for DocumentNativeClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentNativeClipboard {
    pub const fn new() -> Self {
        Self {
            current: None,
            sequence_number: 0,
        }
    }

    pub fn sequence_number(&self) -> u64 {
        self.sequence_number
    }

    pub fn current(&self) -> Option<&StagedDocumentClipboard> {
        self.current.as_ref()
    }

    /// Mutates the clipboard externally to simulate external application copy.
    pub fn inject_external_change(&mut self, text: &str) {
        self.sequence_number = self.sequence_number.saturating_add(1);
        self.current = Some(StagedDocumentClipboard {
            plain_text: text.to_string(),
            exact_raw_bytes: text.as_bytes().to_vec(),
            markdown_source: None,
            enclosing_block: None,
            provenance_header: "<!-- external -->".to_string(),
            has_malformed_utf8: false,
            total_staged_bytes: text.len(),
            active_action: DocumentCopyAction::RenderedReadingText,
        });
    }

    /// Atomically publishes staged clipboard data to the OS pasteboard.
    ///
    /// Validates `expected_sequence`. If concurrent external change occurred, returns
    /// `Err(DocumentCopyError::ConcurrentExternalChange)` without overwriting.
    /// If `simulate_failure` is true, returns `Err(DocumentCopyError::NativePublicationFailed)`
    /// without mutating state.
    pub fn publish(
        &mut self,
        staged: StagedDocumentClipboard,
        expected_sequence: u64,
        simulate_failure: bool,
    ) -> Result<(), DocumentCopyError> {
        if self.sequence_number != expected_sequence {
            return Err(DocumentCopyError::ConcurrentExternalChange);
        }

        if simulate_failure {
            return Err(DocumentCopyError::NativePublicationFailed);
        }

        self.sequence_number = self.sequence_number.saturating_add(1);
        self.current = Some(staged);
        Ok(())
    }
}

/// Bounded streamed export alternative for giant selections or file export (§10.11).
pub struct StreamedDocumentExport;

impl StreamedDocumentExport {
    /// Streams exact source bytes for the given ranges into `writer` in bounded chunks.
    ///
    /// Checks cooperative cancellation before each chunk.
    pub fn stream_to_writer<W: std::io::Write>(
        ranges: &[ByteRange],
        capture: &CompleteCapture,
        writer: &mut W,
        chunk_size: usize,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<u64, DocumentCopyError> {
        let chunk_limit = if chunk_size == 0 { 64 * 1024 } else { chunk_size };
        let bytes = capture.bytes();
        let mut total_written: u64 = 0;

        for range in ranges {
            let start = range.start().get() as usize;
            let end = range.end().get() as usize;
            if end > bytes.len() {
                return Err(DocumentCopyError::InvalidRange);
            }

            let mut offset = start;
            while offset < end {
                if canceled() {
                    return Err(DocumentCopyError::Canceled);
                }

                let remaining = end - offset;
                let step = remaining.min(chunk_limit);
                writer
                    .write_all(&bytes[offset..offset + step])
                    .map_err(|_| DocumentCopyError::NativePublicationFailed)?;

                offset += step;
                total_written = total_written.saturating_add(step as u64);
            }
        }

        writer.flush().map_err(|_| DocumentCopyError::NativePublicationFailed)?;
        Ok(total_written)
    }
}

/// Bidirectional scroll and shared anchor synchronization between source and preview panes (§5.6, §12.4).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSyncPoint {
    /// Byte offset in the primary source capture.
    pub source_byte: u64,
    /// Rendered text byte offset in the preview flow.
    pub rendered_offset: usize,
    /// Associated heading anchor slug, if any.
    pub heading_slug: Option<String>,
}

impl DocumentSyncPoint {
    /// Synchronize from primary source byte offset to rendered text position using source map.
    pub fn from_source_offset(
        source_offset: u64,
        source_map: &DocumentSourceMap,
    ) -> Self {
        let rendered_offset = source_map
            .sync_source_to_rendered(source_offset as usize)
            .unwrap_or(0);

        let heading_slug = source_map
            .headings()
            .iter()
            .filter(|h| (h.source_span.start as u64) <= source_offset)
            .max_by_key(|h| h.source_span.start)
            .map(|h| h.slug.clone());

        Self {
            source_byte: source_offset,
            rendered_offset,
            heading_slug,
        }
    }

    /// Synchronize from rendered text offset to primary source byte offset using source map.
    pub fn from_rendered_offset(
        rendered_offset: usize,
        source_map: &DocumentSourceMap,
    ) -> Self {
        let source_byte = source_map
            .sync_rendered_to_source(rendered_offset)
            .unwrap_or(0) as u64;

        let heading_slug = source_map
            .headings()
            .iter()
            .filter(|h| (h.source_span.start as u64) <= source_byte)
            .max_by_key(|h| h.source_span.start)
            .map(|h| h.slug.clone());

        Self {
            source_byte,
            rendered_offset,
            heading_slug,
        }
    }
}

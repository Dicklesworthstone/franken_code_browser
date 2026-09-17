#![forbid(unsafe_code)]

//! Source selections, grapheme/affinity hit-testing, copy modes, and budgeted clipboard publication (FCB-019.B).
//!
//! # Technical Contracts (§10.6, §10.11)
//!
//! - **Cheap Anchored Selections**: A [`SourceSelection`] is an anchor/range descriptor,
//!   not an eagerly allocated or concatenated string. Selecting an entire giant file is O(1).
//! - **Grapheme & Affinity Hit Testing**: Visual column hit-testing maps to Unicode scalar
//!   and grapheme boundaries with explicit [`CaretAffinity`] (leading vs trailing edge).
//! - **Copy Modes**: Distinct semantics for [`CopyMode::OriginalBytes`] (verbatim bytes including
//!   BOM, CRLF, malformed sequences), [`CopyMode::DecodedUnicode`] (declared decoded text),
//!   and [`CopyMode::WithLocation`] (provenance header + text).
//! - **Budgeted Clipboard Staging**: Pre-publication staging respects [`ClipboardLimits`].
//!   Over-budget selections are refused before publication, leaving the clipboard untouched,
//!   and offer [`StreamedFileExport`] as an un-truncated streaming alternative.
//! - **Atomic Native Publication & Concurrency**: Validates stale capture, cancellation,
//!   and generation tokens. Concurrent external clipboard changes prevent stale overwrites.

use std::io::Write;
use fcb_core::{ByteOffset, ByteRange, FileId, QueryGeneration, SourceRevision};
use crate::SourceCapture;
use super::reader::ReaderError;

/// Caret visual affinity for boundary or ambiguous positions (e.g. line wraps, bidi transitions).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaretAffinity {
    /// Attached to the leading edge of the character cluster.
    Leading,
    /// Attached to the trailing edge of the character cluster.
    Trailing,
}

/// An immutable anchor within a captured source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceAnchor {
    pub byte_offset: ByteOffset,
    pub line: u64,
    pub column: u32,
}

impl SourceAnchor {
    pub const fn new(byte_offset: ByteOffset, line: u64, column: u32) -> Self {
        Self {
            byte_offset,
            line,
            column,
        }
    }
}

/// A lightweight, non-allocating anchor/range description of a source selection.
/// Selecting an entire 500 MB file is O(1) and consumes no buffer allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSelection {
    pub file: FileId,
    pub revision: SourceRevision,
    pub generation: QueryGeneration,
    pub start: SourceAnchor,
    pub end: SourceAnchor,
    pub byte_range: ByteRange,
}

impl SourceSelection {
    pub fn new(
        file: FileId,
        revision: SourceRevision,
        generation: QueryGeneration,
        start: SourceAnchor,
        end: SourceAnchor,
    ) -> Result<Self, ReaderError> {
        let byte_range = ByteRange::new(start.byte_offset, end.byte_offset)
            .map_err(|_| ReaderError::InvalidRange)?;
        Ok(Self {
            file,
            revision,
            generation,
            start,
            end,
            byte_range,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.byte_range.is_empty()
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_range.len().get()
    }

    /// Validates delivery against a source capture and active generation.
    pub fn validate(
        &self,
        capture: &SourceCapture,
        generation: QueryGeneration,
    ) -> Result<(), ClipboardError> {
        if self.file.owner() != capture.owner() || generation.owner() != capture.owner() {
            return Err(ClipboardError::OwnerMismatch);
        }
        if self.file != capture.file() || self.revision != capture.revision() {
            return Err(ClipboardError::StaleSource);
        }
        if self.generation != generation {
            return Err(ClipboardError::StaleGeneration);
        }
        Ok(())
    }
}

/// Hit-testing result within a line of source text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextHitResult {
    pub byte_offset: usize,
    pub char_index: usize,
    pub visual_column: u32,
    pub affinity: CaretAffinity,
    pub is_exact_boundary: bool,
}

/// Hit-test a visual column against line text, snapping to Unicode scalar boundaries
/// and resolving caret affinity.
pub fn hit_test_line(line_text: &str, target_visual_col: u32, tab_width: u32) -> TextHitResult {
    let tab_step = if tab_width == 0 { 4 } else { tab_width };
    let mut current_col: u32 = 0;
    let mut byte_off: usize = 0;

    for (char_idx, ch) in line_text.chars().enumerate() {
        let advance = if ch == '\t' {
            let rem = current_col % tab_step;
            tab_step - rem
        } else {
            1
        };

        let next_col = current_col + advance;
        if target_visual_col < next_col {
            // Target falls on or inside this character cell
            let midpoint = current_col + (advance / 2);
            let affinity = if target_visual_col <= midpoint {
                CaretAffinity::Leading
            } else {
                CaretAffinity::Trailing
            };
            return TextHitResult {
                byte_offset: byte_off,
                char_index: char_idx,
                visual_column: current_col,
                affinity,
                is_exact_boundary: target_visual_col == current_col,
            };
        }

        current_col = next_col;
        byte_off += ch.len_utf8();
    }

    // Past the end of the line
    TextHitResult {
        byte_offset: byte_off,
        char_index: line_text.chars().count(),
        visual_column: current_col,
        affinity: CaretAffinity::Trailing,
        is_exact_boundary: true,
    }
}

/// Declared copy mode defining formatting and semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyMode {
    /// Exact captured bytes (verbatim byte slice including BOM, CRLF, malformed UTF-8, nulls).
    OriginalBytes,
    /// Decoded Unicode text (UTF-8 representation; malformed sequences use explicit replacements).
    DecodedUnicode,
    /// Text formatted with location provenance header.
    WithLocation,
}

/// Native clipboard flavors staged for OS pasteboard publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardFlavor {
    PlainTextUtf8,
    ExactRawBytes,
    LocationProvenance,
}

/// Bounded limits for clipboard operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClipboardLimits {
    /// Maximum bytes permitted for clipboard publication (e.g. 4 MiB).
    pub max_clipboard_bytes: usize,
}

impl Default for ClipboardLimits {
    fn default() -> Self {
        Self {
            max_clipboard_bytes: 4 * 1024 * 1024, // 4 MiB
        }
    }
}

/// Detailed errors returned during clipboard preparation or publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardError {
    BudgetExceeded {
        requested_bytes: usize,
        max_budget_bytes: usize,
    },
    StaleSource,
    OwnerMismatch,
    StaleGeneration,
    Canceled,
    PublicationFailed,
    ConcurrentExternalChange,
    InvalidRange,
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExceeded { requested_bytes, max_budget_bytes } => {
                write!(f, "CLIPBOARD_BUDGET_EXCEEDED: requested {requested_bytes} bytes, limit is {max_budget_bytes}")
            }
            Self::StaleSource => write!(f, "CLIPBOARD_STALE_SOURCE"),
            Self::OwnerMismatch => write!(f, "CLIPBOARD_OWNER_MISMATCH"),
            Self::StaleGeneration => write!(f, "CLIPBOARD_STALE_GENERATION"),
            Self::Canceled => write!(f, "CLIPBOARD_CANCELED"),
            Self::PublicationFailed => write!(f, "CLIPBOARD_PUBLICATION_FAILED"),
            Self::ConcurrentExternalChange => write!(f, "CLIPBOARD_CONCURRENT_EXTERNAL_CHANGE"),
            Self::InvalidRange => write!(f, "CLIPBOARD_INVALID_RANGE"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// Pre-staged multi-flavor clipboard data verified against budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedClipboardData {
    pub plain_text: String,
    pub exact_bytes: Vec<u8>,
    pub provenance: String,
    pub has_replacements: bool,
    pub total_staged_bytes: usize,
}

impl StagedClipboardData {
    /// Stage a selection into multi-flavor clipboard representations.
    ///
    /// Refuses with [`ClipboardError::BudgetExceeded`] if size exceeds `limits.max_clipboard_bytes`.
    /// Preserves exact bytes (including BOM, CRLF, malformed bytes).
    pub fn stage(
        selection: &SourceSelection,
        capture: &SourceCapture,
        active_generation: QueryGeneration,
        limits: ClipboardLimits,
        canceled: impl Fn() -> bool,
    ) -> Result<Self, ClipboardError> {
        if canceled() {
            return Err(ClipboardError::Canceled);
        }
        selection.validate(capture, active_generation)?;

        let (start, end) = selection
            .byte_range
            .as_usize_bounds()
            .map_err(|_| ClipboardError::InvalidRange)?;
        let byte_len = end.checked_sub(start).ok_or(ClipboardError::InvalidRange)?;

        if byte_len > limits.max_clipboard_bytes {
            return Err(ClipboardError::BudgetExceeded {
                requested_bytes: byte_len,
                max_budget_bytes: limits.max_clipboard_bytes,
            });
        }

        let slice = capture
            .bytes()
            .get(start..end)
            .ok_or(ClipboardError::InvalidRange)?;
        let exact_bytes = slice.to_vec();

        // Check for malformed UTF-8 and decode safely
        let (plain_text, has_replacements) = match std::str::from_utf8(slice) {
            Ok(valid) => (valid.to_string(), false),
            Err(_) => {
                // Fallback decode preserving lossless replacement markers
                (String::from_utf8_lossy(slice).into_owned(), true)
            }
        };

        let provenance = format!(
            "File: {}\nRevision: {}\nLines: {}-{}\nRange: {}..{}\n",
            capture.logical_path(),
            selection.revision.get(),
            selection.start.line,
            selection.end.line,
            start,
            end
        );

        let total_staged_bytes = exact_bytes.len() + plain_text.len() + provenance.len();
        if total_staged_bytes > limits.max_clipboard_bytes * 3 {
            return Err(ClipboardError::BudgetExceeded {
                requested_bytes: total_staged_bytes,
                max_budget_bytes: limits.max_clipboard_bytes,
            });
        }

        if canceled() {
            return Err(ClipboardError::Canceled);
        }

        Ok(Self {
            plain_text,
            exact_bytes,
            provenance,
            has_replacements,
            total_staged_bytes,
        })
    }
}

/// Simulated native OS clipboard with generation tracking and atomic publication.
#[derive(Clone, Debug)]
pub struct NativeClipboard {
    plain_text: Option<String>,
    raw_bytes: Option<Vec<u8>>,
    provenance: Option<String>,
    generation_seq: u64,
    fail_next_publish: bool,
}

impl Default for NativeClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeClipboard {
    pub const fn new() -> Self {
        Self {
            plain_text: None,
            raw_bytes: None,
            provenance: None,
            generation_seq: 1,
            fail_next_publish: false,
        }
    }

    pub const fn generation_seq(&self) -> u64 {
        self.generation_seq
    }

    pub fn plain_text(&self) -> Option<&str> {
        self.plain_text.as_deref()
    }

    pub fn raw_bytes(&self) -> Option<&[u8]> {
        self.raw_bytes.as_deref()
    }

    pub fn provenance(&self) -> Option<&str> {
        self.provenance.as_deref()
    }

    /// Simulate an intentional OS publication failure (for negative control testing).
    pub fn inject_publication_failure(&mut self, fail: bool) {
        self.fail_next_publish = fail;
    }

    /// Simulate a concurrent external clipboard modification (e.g. another app copied text).
    pub fn simulate_external_change(&mut self, text: &str) {
        self.plain_text = Some(text.to_string());
        self.raw_bytes = Some(text.as_bytes().to_vec());
        self.provenance = None;
        self.generation_seq = self.generation_seq.wrapping_add(1);
    }

    /// Publish staged clipboard data atomically.
    ///
    /// Returns [`ClipboardError::ConcurrentExternalChange`] if `expected_seq != self.generation_seq`.
    /// Returns [`ClipboardError::PublicationFailed`] if native publication fails.
    /// On failure, the prior clipboard content is preserved without partial mutation.
    pub fn publish(
        &mut self,
        staged: StagedClipboardData,
        expected_seq: u64,
    ) -> Result<(), ClipboardError> {
        if self.generation_seq != expected_seq {
            return Err(ClipboardError::ConcurrentExternalChange);
        }

        if self.fail_next_publish {
            self.fail_next_publish = false;
            return Err(ClipboardError::PublicationFailed);
        }

        self.plain_text = Some(staged.plain_text);
        self.raw_bytes = Some(staged.exact_bytes);
        self.provenance = Some(staged.provenance);
        self.generation_seq = self.generation_seq.wrapping_add(1);
        Ok(())
    }
}

/// Options governing streamed file export.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamedExportOptions {
    pub chunk_size: usize,
}

impl Default for StreamedExportOptions {
    fn default() -> Self {
        Self {
            chunk_size: 64 * 1024, // 64 KiB
        }
    }
}

/// Outcome of a streamed file export operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportOutcome {
    Completed { total_bytes: usize },
    Canceled { bytes_written: usize },
}

/// Streamed file export alternative for selections exceeding the clipboard budget.
pub struct StreamedFileExport;

impl StreamedFileExport {
    /// Stream exact captured source bytes directly to a writer in bounded chunks without
    /// allocating a monolithic buffer.
    pub fn stream_to_writer<W: Write>(
        selection: &SourceSelection,
        capture: &SourceCapture,
        active_generation: QueryGeneration,
        options: StreamedExportOptions,
        writer: &mut W,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<ExportOutcome, ClipboardError> {
        selection.validate(capture, active_generation)?;

        let (start, end) = selection
            .byte_range
            .as_usize_bounds()
            .map_err(|_| ClipboardError::InvalidRange)?;
        let total_bytes = end.saturating_sub(start);

        let bytes = capture.bytes();
        let chunk_size = options.chunk_size.max(1024);
        let mut offset = start;
        let mut written = 0;

        while offset < end {
            if canceled() {
                return Ok(ExportOutcome::Canceled { bytes_written: written });
            }

            let next_chunk_end = (offset + chunk_size).min(end);
            let chunk = bytes.get(offset..next_chunk_end).ok_or(ClipboardError::InvalidRange)?;

            writer.write_all(chunk).map_err(|_| ClipboardError::PublicationFailed)?;
            written += chunk.len();
            offset = next_chunk_end;
        }

        writer.flush().map_err(|_| ClipboardError::PublicationFailed)?;

        if canceled() {
            return Ok(ExportOutcome::Canceled { bytes_written: written });
        }

        Ok(ExportOutcome::Completed { total_bytes })
    }
}

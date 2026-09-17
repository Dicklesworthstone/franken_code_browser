//! IME text-composition state machine (fcb-8bqc.1).
//!
//! Mirrors `NSTextInputClient` semantics without AppKit: marked text is a
//! temporary in-document range that later stages replace in place, and a
//! commit replaces the marked region with final text. All ranges are UTF-16
//! code-unit ranges so native callers never convert; every offset is
//! validated against the document's actual code-unit count and every
//! replacement is byte-budget bounded.

use fcb_core::{CoreError, Utf16CodeUnitOffset, Utf16CodeUnitRange};

/// A marked (in-composition) region and the selection the IME wants inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkedRegion {
    pub range: Utf16CodeUnitRange,
}

/// Errors surfaced by composition transitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositionError {
    /// The replacement would exceed the bounded document budget.
    TextBudgetExceeded,
    /// A range referenced a non-scalar UTF-16 boundary inside the document.
    MidSurrogate,
    /// A range was empty or inverted where a non-empty one is required.
    InvalidRange,
    /// Commit attempted with no marked region to replace and no selection.
    NothingToCommit,
}

/// The document text plus marked/selection state, in UTF-16 code units.
///
/// Offsets are counts of UTF-16 code units. [`CompositionState`] validates
/// that every boundary lands on a scalar edge of the current document so a
/// native IME can never split a surrogate pair.
#[derive(Clone, Debug)]
pub struct CompositionState {
    text: String,
    marked: Option<MarkedRegion>,
    selection: Utf16CodeUnitRange,
    max_text_bytes: usize,
}

impl CompositionState {
    /// Creates an empty document with the given byte budget.
    pub fn new(max_text_bytes: usize) -> Self {
        Self {
            text: String::new(),
            marked: None,
            selection: utf16_range(0, 0),
            max_text_bytes,
        }
    }

    /// The current document text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The marked (composing) region, if any.
    pub fn marked(&self) -> Option<MarkedRegion> {
        self.marked.clone()
    }

    /// The current selection.
    pub fn selection(&self) -> Utf16CodeUnitRange {
        self.selection
    }

    /// Total UTF-16 code units in the document.
    pub fn text_length_units(&self) -> u64 {
        units_of(&self.text)
    }

    /// `setMarkedText:` — stage or restage composing text.
    ///
    /// A previously marked region is replaced by `marked_text`; the selection
    /// is placed at `selected_range` (interpreted inside the *new* marked
    /// region when one exists). This is the multi-stage IME path: each stage
    /// supersedes the previous marked text at the same location.
    pub fn set_marked_text(
        &mut self,
        marked_text: &str,
        replacement_range: Utf16CodeUnitRange,
        selected_range: Utf16CodeUnitRange,
    ) -> Result<(), CompositionError> {
        // An active composition is restaged in place: the previous marked
        // span is what gets replaced, whatever range the client nominates.
        // With no active composition the nominated range (e.g. selected
        // text) is replaced.
        let (start, end) = match &self.marked {
            Some(marked) => ordered(marked.range)?,
            None => ordered(replacement_range)?,
        };
        self.replace_units(start, end, marked_text)?;
        let start = start.min(self.text_length_units());
        self.marked = Some(MarkedRegion {
            range: utf16_range(start, start + units_of(marked_text)),
        });
        self.selection = clamp_range(selected_range, self.text_length_units());
        Ok(())
    }

    /// `insertText:` — commit final text, replacing the marked region (or the
    /// selection when unmarked). Clears the marked region on success.
    pub fn insert_text(
        &mut self,
        text: &str,
        replacement: Option<Utf16CodeUnitRange>,
    ) -> Result<Utf16CodeUnitRange, CompositionError> {
        let (start, end) = match (&self.marked, replacement) {
            (Some(marked), _) => ordered(marked.range)?,
            (None, Some(range)) => ordered(range)?,
            (None, None) => ordered(self.selection)?,
        };
        if text.is_empty() && start == end {
            return Err(CompositionError::NothingToCommit);
        }
        self.replace_units(start, end, text)?;
        self.marked = None;
        let committed = utf16_range(start, start + units_of(text));
        self.selection = committed;
        Ok(committed)
    }

    /// `unmarkText:` — confirm the marked text as final document content.
    pub fn unmark_text(&mut self) -> Result<(), CompositionError> {
        let marked = self.marked.take().ok_or(CompositionError::NothingToCommit)?;
        let (start, end) = ordered(marked.range)?;
        self.selection = utf16_range(end, end);
        let _ = start;
        Ok(())
    }

    /// Directly moves the selection (arrow keys, mouse); boundaries must be
    /// scalar edges.
    pub fn set_selection(&mut self, range: Utf16CodeUnitRange) -> Result<(), CompositionError> {
        let (start, end) = ordered(range)?;
        if !on_scalar_edge(&self.text, start) || !on_scalar_edge(&self.text, end) {
            return Err(CompositionError::MidSurrogate);
        }
        self.selection = utf16_range(start, end);
        Ok(())
    }

    /// Replaces units [start, end) with `text` under the byte budget,
    /// refusing mid-surrogate boundaries.
    fn replace_units(
        &mut self,
        start: u64,
        end: u64,
        text: &str,
    ) -> Result<(), CompositionError> {
        let len = self.text_length_units();
        if start > len || end > len || start > end {
            return Err(CompositionError::InvalidRange);
        }
        if !on_scalar_edge(&self.text, start) || !on_scalar_edge(&self.text, end) {
            return Err(CompositionError::MidSurrogate);
        }
        let byte_start = units_to_byte(&self.text, start);
        let byte_end = units_to_byte(&self.text, end);
        let mut next = String::with_capacity(self.text.len() + text.len());
        next.push_str(&self.text[..byte_start]);
        next.push_str(text);
        next.push_str(&self.text[byte_end..]);
        if next.len() > self.max_text_bytes {
            return Err(CompositionError::TextBudgetExceeded);
        }
        self.text = next;
        Ok(())
    }
}

/// Total UTF-16 code units of `text`.
fn units_of(text: &str) -> u64 {
    text.chars().map(|ch| u64::from(ch.len_utf16() as u16)).sum()
}

/// Byte offset of the UTF-16 unit boundary `units`.
fn units_to_byte(text: &str, units: u64) -> usize {
    let mut seen = 0_u64;
    let mut bytes = 0_usize;
    for ch in text.chars() {
        if seen >= units {
            break;
        }
        seen += u64::from(ch.len_utf16() as u16);
        bytes += ch.len_utf8();
    }
    bytes
}

/// True when `units` is a scalar boundary (not inside a surrogate pair).
fn on_scalar_edge(text: &str, units: u64) -> bool {
    let mut seen = 0_u64;
    for ch in text.chars() {
        if seen == units {
            return true;
        }
        if units > seen && units < seen + u64::from(ch.len_utf16() as u16) {
            return false;
        }
        seen += u64::from(ch.len_utf16() as u16);
    }
    seen == units
}

fn ordered(range: Utf16CodeUnitRange) -> Result<(u64, u64), CompositionError> {
    let start = range.start().get();
    let end = range.end().get();
    if start > end {
        return Err(CompositionError::InvalidRange);
    }
    Ok((start, end))
}

fn clamp_range(range: Utf16CodeUnitRange, max_units: u64) -> Utf16CodeUnitRange {
    let clamp = |v: Utf16CodeUnitOffset| Utf16CodeUnitOffset::new(v.get().min(max_units));
    Utf16CodeUnitRange::new(clamp(range.start()), clamp(range.end()))
        .expect("clamped range remains ordered")
}

fn utf16_range(start: u64, end: u64) -> Utf16CodeUnitRange {
    Utf16CodeUnitRange::new(Utf16CodeUnitOffset::new(start), Utf16CodeUnitOffset::new(end))
        .expect("ordered range valid")
}

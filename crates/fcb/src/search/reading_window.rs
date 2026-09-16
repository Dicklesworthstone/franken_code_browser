#![forbid(unsafe_code)]

//! Bounded source windows, not glyph layout or an AppKit text view.
//!
//! Uses the source crate's decoder and exact mapping, retaining only the visible
//! byte/line window plus at most four UTF-16 context bytes. Never materializes a
//! whole giant line. Continuation anchors preserve the logical line and exact
//! source position; columns, graphemes, shaping and pixel scroll are not inferred
//! from byte offsets. All returned decoded ranges are WINDOW-LOCAL UTF-8 ranges.

use std::mem::size_of;
use fcb_core::{ByteLength, ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range,
    QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_source::{CaptureEncodingMap, DetectedEncoding, MappingSpan, SpanKind};
use crate::{FramePlan, SourceCapture};
use super::reader::{Cursor, ReaderError, ReadingAnchor, SourceReader, newline_token, read_unit};

pub const MAX_READING_WINDOW_BYTES: usize = 256 * 1024;
pub const MAX_READING_WINDOW_LINES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadingWindowOptions { pub max_bytes: usize, pub max_lines: usize }
impl Default for ReadingWindowOptions {
    fn default() -> Self { Self { max_bytes: 32 * 1024, max_lines: 100 } }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineEnding { None, Lf, Cr, CrLf }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadingLine {
    pub number: u64,
    /// Visible original bytes including this row's terminator, when present.
    pub raw_range: ByteRange,
    /// Visible original bytes without the terminator.
    pub content_range: ByteRange,
    /// Window-local decoded UTF-8 bytes without the terminator.
    pub text_range: DecodedUtf8Range,
    pub ending: LineEnding,
    pub continued_before: bool,
    pub continued_after: bool,
}

/// No clipboard side effect. A host chooses whether to publish decoded text,
/// original bytes or neither. Malformed replacement text is labeled explicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadingSelection<'a> {
    pub text: &'a str,
    pub original_bytes: &'a [u8],
    pub original_range: ByteRange,
    pub contains_replacements: bool,
}

pub struct ReadingWindow<'source> {
    source: &'source SourceCapture,
    anchor: ReadingAnchor,
    raw_range: ByteRange,
    next: Option<ReadingAnchor>,
    rows: Vec<ReadingLine>,
    map: CaptureEncodingMap,
    decoded_base: usize,
    _lease: ResourceLease,
}
impl ReadingWindow<'_> {
    pub const fn anchor(&self) -> ReadingAnchor { self.anchor }
    pub const fn raw_range(&self) -> ByteRange { self.raw_range }
    pub const fn next_anchor(&self) -> Option<ReadingAnchor> { self.next }
    pub fn lines(&self) -> &[ReadingLine] { &self.rows }
    pub fn text(&self) -> &str { &self.map.decoded_text()[self.decoded_base..] }
    pub fn line_text(&self, row: usize) -> Option<&str> {
        let range = self.rows.get(row)?.text_range;
        self.text().get(usize::try_from(range.start().get()).ok()?..usize::try_from(range.end().get()).ok()?)
    }
    pub fn has_replacements(&self) -> bool { self.replacements_in(self.raw_range) }
    pub fn reaches_eof(&self) -> bool { self.next.is_none() }
    pub fn frame_plan(&self) -> FramePlan {
        FramePlan::new(self.source.owner(), self.source.file(), self.source.revision(), self.raw_range)
    }
    pub fn validate_delivery(&self, source: &SourceCapture, generation: QueryGeneration) -> Result<(), ReaderError> {
        self.anchor.validate_delivery(source, generation)
    }

    /// A decoded selection must end on real scalar boundaries. Interior UTF-8
    /// bytes, UTF-16 code-unit indices and out-of-window coordinates are refused.
    pub fn text_selection(&self, range: DecodedUtf8Range) -> Result<ReadingSelection<'_>, ReaderError> {
        let start = usize::try_from(range.start().get()).map_err(|_| ReaderError::InvalidRange)?;
        let end = usize::try_from(range.end().get()).map_err(|_| ReaderError::InvalidRange)?;
        let text = self.text().get(start..end).ok_or(ReaderError::InvalidRange)?;
        let mapped = DecodedUtf8Range::new(DecodedUtf8Offset::new((self.decoded_base + start) as u64),
            DecodedUtf8Offset::new((self.decoded_base + end) as u64)).map_err(|_| ReaderError::InvalidRange)?;
        let raw = self.map.decoded_utf8_range_to_byte_range(mapped).map_err(|_| ReaderError::InvalidRange)?;
        let (first, last) = raw.as_usize_bounds().map_err(|_| ReaderError::InvalidRange)?;
        Ok(ReadingSelection { text, original_bytes: &self.source.bytes()[first..last],
            original_range: raw, contains_replacements: self.replacements_in(raw) })
    }

    /// Translate original source bytes into WINDOW-LOCAL decoded UTF-8 offsets.
    /// A raw-byte search hit inside a scalar is still a valid raw selection, but
    /// has no exact decoded selection and receives an explicit error here.
    pub fn source_to_text(&self, range: ByteRange) -> Result<DecodedUtf8Range, ReaderError> {
        if range.start().get() < self.raw_range.start().get() || range.end().get() > self.raw_range.end().get() {
            return Err(ReaderError::InvalidRange);
        }
        let start = self.map.byte_to_decoded_utf8_offset(range.start()).map_err(|_| ReaderError::InvalidRange)?.get();
        let end = self.map.byte_to_decoded_utf8_offset(range.end()).map_err(|_| ReaderError::InvalidRange)?.get();
        let start = start.checked_sub(self.decoded_base as u64).ok_or(ReaderError::InvalidRange)?;
        let end = end.checked_sub(self.decoded_base as u64).ok_or(ReaderError::InvalidRange)?;
        DecodedUtf8Range::new(DecodedUtf8Offset::new(start), DecodedUtf8Offset::new(end)).map_err(|_| ReaderError::InvalidRange)
    }
    fn replacements_in(&self, range: ByteRange) -> bool {
        self.map.spans().iter().any(|span| span.raw_start < range.end().get() && span.raw_end() > range.start().get()
            && matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte))
    }
}

impl<'source> SourceReader<'source> {
    /// Produce a bounded plain-text reading window on a worker. At most
    /// max_bytes original visible bytes and max_lines rows are emitted; a
    /// scalar/CRLF boundary may shorten that prefix. Work includes two bounded
    /// range passes and one existing source-decoder invocation. No source-wide
    /// indexing, shaping, runtime creation or native clipboard work occurs.
    pub fn window(&self, anchor: ReadingAnchor, active_generation: QueryGeneration,
        options: ReadingWindowOptions, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<ReadingWindow<'source>, ReaderError> {
        anchor.validate_delivery(self.source, active_generation)?;
        if !(4..=MAX_READING_WINDOW_BYTES).contains(&options.max_bytes)
            || !(1..=MAX_READING_WINDOW_LINES).contains(&options.max_lines) { return Err(ReaderError::InvalidLimits); }
        if canceled() { return Err(ReaderError::Canceled); }
        let bytes = self.source.bytes();
        let start = floor_boundary(bytes, self.encoding, anchor.offset.max(self.content_start));
        let limit = floor_boundary(bytes, self.encoding, start.saturating_add(options.max_bytes).min(bytes.len()));
        let initial = Cursor { offset: start, start: anchor.line_start, line: anchor.line };
        let plan = plan_window(bytes, self.encoding, initial, limit, options.max_lines, &mut canceled)?;
        // The existing UTF-16 decoder recognizes a leading FEFF as a BOM even
        // for offset slices. Include a bounded predecessor as decoder context:
        // an in-content FEFF at the VISIBLE start must never be stripped. The
        // context is excluded from returned text, rows, selections and frames.
        let map_start = if self.encoding.is_utf16() && start > 0 {
            floor_boundary(bytes, self.encoding, start - 1)
        } else { start };
        let raw_count = plan.end - map_start;
        // Conservative admission covers the existing decoder's geometric Vec/
        // String growth AND old/new reallocation overlap, not just final length.
        let decoder_charge = raw_count.checked_add(8).and_then(|n| n.checked_mul(4 * size_of::<MappingSpan>() + 16))
            .ok_or(ReaderError::InvalidLimits)?;
        let charge = plan.rows.checked_mul(size_of::<ReadingLine>()).and_then(|n| n.checked_add(decoder_charge))
            .and_then(|n| n.checked_add(size_of::<ReadingWindow<'source>>())).ok_or(ReaderError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(self.source.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ReaderError::ResourceDenied)?;
        if canceled() { return Err(ReaderError::Canceled); }
        let encoding = match self.encoding { DetectedEncoding::Utf8 { .. } => DetectedEncoding::Utf8 { has_bom: false }, other => other };
        let map = CaptureEncodingMap::build_with_base_offset(&bytes[map_start..plan.end], encoding, map_start as u64, 0, 0, 0)
            .map_err(|_| ReaderError::EncodingError)?;
        let decoded_base = usize::try_from(map.byte_to_decoded_utf8_offset(ByteOffset::new(start as u64))
            .map_err(|_| ReaderError::EncodingError)?.get()).map_err(|_| ReaderError::InvalidRange)?;
        if !map.decoded_text().is_char_boundary(decoded_base) { return Err(ReaderError::EncodingError); }
        let mut rows = Vec::new();
        rows.try_reserve_exact(plan.rows).map_err(|_| ReaderError::AllocationFailed)?;
        if rows.capacity() > plan.rows { return Err(ReaderError::ResourceDenied); }
        let mut cursor = initial;
        for _ in 0..plan.rows {
            let draft = scan_row(bytes, self.encoding, cursor, plan.end, &mut canceled)?;
            let display_start = map.byte_to_decoded_utf8_offset(ByteOffset::new(draft.start as u64))
                .map_err(|_| ReaderError::EncodingError)?.get().checked_sub(decoded_base as u64).ok_or(ReaderError::EncodingError)?;
            let display_end = map.byte_to_decoded_utf8_offset(ByteOffset::new(draft.content_end as u64))
                .map_err(|_| ReaderError::EncodingError)?.get().checked_sub(decoded_base as u64).ok_or(ReaderError::EncodingError)?;
            rows.push(ReadingLine { number: cursor.line, raw_range: raw_range(draft.start, draft.end)?,
                content_range: raw_range(draft.start, draft.content_end)?,
                text_range: DecodedUtf8Range::new(DecodedUtf8Offset::new(display_start), DecodedUtf8Offset::new(display_end))
                    .map_err(|_| ReaderError::InvalidRange)?,
                ending: draft.ending, continued_before: draft.start > cursor.start,
                continued_after: draft.ending == LineEnding::None && draft.end < bytes.len() });
            if let Some(next) = draft.next { cursor = next; }
        }
        if canceled() { return Err(ReaderError::Canceled); }
        let next = plan.next.map(|cursor| ReadingAnchor { offset: cursor.offset, line: cursor.line,
            line_start: cursor.start, ..anchor });
        Ok(ReadingWindow { source: self.source, anchor, raw_range: raw_range(start, plan.end)?,
            next, rows, map, decoded_base, _lease: lease })
    }

    /// Borrow exact source bytes without allocating or changing the clipboard.
    /// This explicitly includes BOMs, CRLF and malformed bytes when selected.
    /// The host must separately admit any owned export/clipboard representation.
    pub fn raw_selection(&self, range: ByteRange, max_bytes: usize) -> Result<&'source [u8], ReaderError> {
        let (start, end) = range.as_usize_bounds().map_err(|_| ReaderError::InvalidRange)?;
        if end - start > max_bytes { return Err(ReaderError::InvalidLimits); }
        self.source.bytes().get(start..end).ok_or(ReaderError::InvalidRange)
    }
}

struct WindowPlan { end: usize, rows: usize, next: Option<Cursor> }
struct RowDraft { start: usize, content_end: usize, end: usize, ending: LineEnding, next: Option<Cursor> }
fn plan_window(bytes: &[u8], encoding: DetectedEncoding, mut cursor: Cursor, limit: usize,
    max_lines: usize, canceled: &mut impl FnMut() -> bool) -> Result<WindowPlan, ReaderError> {
    let mut rows = 0;
    loop {
        let row = scan_row(bytes, encoding, cursor, limit, canceled)?;
        rows += 1;
        match row.next {
            Some(next) if rows < max_lines && (next.offset < limit || limit == bytes.len()) => cursor = next,
            next => return Ok(WindowPlan { end: row.end, rows, next }),
        }
    }
}
fn scan_row(bytes: &[u8], encoding: DetectedEncoding, mut cursor: Cursor, limit: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<RowDraft, ReaderError> {
    let start = cursor.offset;
    while cursor.offset < limit {
        if canceled() { return Err(ReaderError::Canceled); }
        let (width, newline) = newline_token(bytes, encoding, cursor.offset);
        if width > limit - cursor.offset { return Err(ReaderError::EncodingError); }
        if newline {
            let content_end = cursor.offset;
            let unit_width = if encoding.is_utf16() { 2 } else { 1 };
            let ending = if width == unit_width * 2 { LineEnding::CrLf }
                else if read_unit(bytes, encoding, cursor.offset) == 13 { LineEnding::Cr } else { LineEnding::Lf };
            cursor.offset += width;
            cursor.start = cursor.offset;
            cursor.line = cursor.line.checked_add(1).ok_or(ReaderError::InvalidRange)?;
            return Ok(RowDraft { start, content_end, end: cursor.offset, ending, next: Some(cursor) });
        }
        cursor.offset += width;
    }
    Ok(RowDraft { start, content_end: cursor.offset, end: cursor.offset, ending: LineEnding::None,
        next: (cursor.offset < bytes.len()).then_some(cursor) })
}
fn raw_range(start: usize, end: usize) -> Result<ByteRange, ReaderError> {
    ByteRange::new(ByteOffset::new(start as u64), ByteOffset::new(end as u64)).map_err(|_| ReaderError::InvalidRange)
}

/// Constant-size context only. Never split a valid scalar, UTF-16 pair or CRLF.
/// Malformed UTF-8 bytes remain individual units, matching the source decoder.
fn floor_boundary(bytes: &[u8], encoding: DetectedEncoding, mut offset: usize) -> usize {
    if offset >= bytes.len() { return bytes.len(); }
    if encoding.is_utf16() {
        offset -= offset % 2;
        if offset >= 2 && offset + 1 < bytes.len() {
            let previous = read_unit(bytes, encoding, offset - 2);
            let current = read_unit(bytes, encoding, offset);
            if ((0xd800..=0xdbff).contains(&previous) && (0xdc00..=0xdfff).contains(&current))
                || (previous == 13 && current == 10) { offset -= 2; }
        }
    } else {
        for start in offset.saturating_sub(3)..offset {
            let width = match bytes[start] { 0xc2..=0xdf => 2, 0xe0..=0xef => 3, 0xf0..=0xf4 => 4, _ => 1 };
            if start + width > offset && start + width <= bytes.len()
                && std::str::from_utf8(&bytes[start..start + width]).is_ok() { offset = start; break; }
        }
        if offset > 0 && bytes[offset] == b'\n' && bytes[offset - 1] == b'\r' { offset -= 1; }
    }
    offset
}

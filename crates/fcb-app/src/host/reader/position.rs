#![forbid(unsafe_code)]

//! Exact logical line/column navigation over the reader's immutable capture.
//! The existing physical-line scanner and source decoder own line/encoding
//! semantics. This module composes their bounded worker steps; it never builds
//! a complete giant line, infers a visual column, or reopens a source pathname.

use std::mem::size_of;
use fcb::search::ExtentText;
use fcb::source::{CaptureEncodingMap, DetectedEncoding, MappingSpan, StatefulChunkDecoder};
use fcb_core::{DecodedUtf8Offset, ScalarIndex, Utf16CodeUnitOffset};
use super::{AppError, ByteLength, ByteOffset, HostResponse, LineNumber, LineWindowScanner,
    Output, ReaderSession, ReaderSessionError, WindowTarget, LINE_SCAN_STEP, check, range, window_limit};

/// Logical units counted from the decoded content start of one physical line.
/// Columns are one-based. Tabs count as one scalar/code unit/UTF-8 byte; these
/// are not display cells, graphemes, glyph positions or bidirectional order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReaderColumnKind {
    UnicodeScalar,
    Utf16CodeUnit,
    DecodedUtf8Byte,
}
impl ReaderColumnKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnicodeScalar => "unicode_scalar",
            Self::Utf16CodeUnit => "utf16_code_unit",
            Self::DecodedUtf8Byte => "decoded_utf8_byte",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ReaderPosition {
    line: u64,
    column: u64,
    kind: ReaderColumnKind,
    offset: ByteOffset,
}
impl ReaderPosition {
    pub(super) fn encode(self, out: &mut Output, text: &ExtentText) -> Result<(), ReaderSessionError> {
        let point = range(self.offset.get(), self.offset.get())?;
        let decoded = text.source_to_text(point)?;
        let verified = text.text_selection(decoded)?;
        if verified.original_range != point || !verified.original_bytes.is_empty() || !decoded.is_empty() {
            return Err(ReaderSessionError::InvalidRange);
        }
        out.literal("{\"line\":")?; out.integer(self.line)?;
        out.literal(",\"column\":")?; out.integer(self.column)?;
        out.literal(",\"column_kind\":")?; out.quoted(self.kind.code())?;
        out.literal(",\"original_offset\":")?; out.integer(self.offset.get())?;
        out.literal(",\"window_utf8_offset\":")?; out.integer(decoded.start().get())?;
        out.literal("}")?;
        Ok(())
    }
}

// Decoder maps and their geometric allocation overlap are admitted for this
// fixed scratch size, independently of source length and requested column.
const POSITION_DECODE_BYTES: usize = 4 * 1024;

impl ReaderSession {
    /// Resolve a one-based physical line and explicitly typed one-based column.
    /// CR/LF/CRLF and a real leading BOM do not contribute content columns.
    /// The content-end caret is valid; a nonexistent line/column, an interior
    /// UTF-8 scalar byte or a UTF-16 surrogate split is refused without snapping.
    /// Malformed source uses the shared decoder's declared replacement scalars.
    ///
    /// This returns an exact logical position, not a native grapheme/glyph
    /// caret guarantee. A source window may begin mid-line, so its independent
    /// first_physical_line remains unknown. Search/outline state is unchanged.
    pub fn read_position(&mut self, line: u64, column: u64, kind: ReaderColumnKind,
        max_bytes: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        window_limit(max_bytes)?;
        let target = column.checked_sub(1).ok_or(ReaderSessionError::InvalidRange)?;
        let first = LineNumber::new(line)?;
        check(&mut canceled)?;
        let start = self.position_line_start(first, &mut canceled)?;
        let [allocation] = self.allocations()?;
        // This matches the extent decoder's conservative old/new Vec and
        // String growth admission, plus its input/carry and fixed state.
        let charge = POSITION_DECODE_BYTES.checked_add(8)
            .and_then(|n| n.checked_mul(4 * size_of::<MappingSpan>() + 16))
            .and_then(|n| n.checked_add(size_of::<StatefulChunkDecoder>() + size_of::<CaptureEncodingMap>()))
            .ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let offset = locate_column(self.capture.bytes(), self.encoding, start, target, kind, &mut canceled)?;
        drop(lease); // Column scratch is gone before admitting the page decoder.
        check(&mut canceled)?;

        // Reserve at least four bytes after the point for one complete scalar
        // or UTF-16 pair. With a four-byte page this starts at the target; a
        // half-page prefix could otherwise produce empty non-EOF BOM windows.
        let before = (max_bytes as u64 - 4) / 2;
        let page_start = offset.get().saturating_sub(before);
        let page_end = page_start.checked_add(max_bytes as u64)
            .ok_or(ReaderSessionError::InvalidRange)?.min(self.length());
        let requested = range(page_start, page_end)?;
        self.window_with_target("position", requested, max_bytes,
            Some(WindowTarget::Position(ReaderPosition { line, column, kind, offset })), None, &mut canceled)
    }

    fn position_line_start(&mut self, first: LineNumber, canceled: &mut impl FnMut() -> bool)
        -> Result<ByteOffset, ReaderSessionError> {
        let mut scan = self.line_checkpoints.iter().flatten()
            .filter(|cursor| cursor.lines_seen() < first.get())
            .max_by_key(|cursor| cursor.bytes_scanned())
            .map(|cursor| cursor.retarget(first, 1))
            .unwrap_or_else(|| LineWindowScanner::new(first, 1, self.encoding))?;
        // Stop once the target line START is known. Finishing a one-line
        // window first would needlessly scan all of a pathological giant line.
        while scan.selected_start().is_none() && !scan.is_finished() && scan.bytes_scanned() < self.length() {
            check(canceled)?;
            let at = scan.bytes_scanned() as usize; // Admitted capture <= 32 MiB.
            scan.step(ByteOffset::new(at as u64), &self.capture.bytes()[at..], LINE_SCAN_STEP)?;
            if scan.selected_start().is_none() && !scan.is_finished() && scan.lines_seen() < first.get() {
                let slot = scan.bytes_scanned() as usize / LINE_SCAN_STEP;
                self.line_checkpoints[slot] = Some(scan.clone());
            }
        }
        check(canceled)?;
        if scan.selected_start().is_none() && !scan.is_finished() { scan.finish()?; }
        scan.selected_start().ok_or(ReaderSessionError::MissingLine)
    }
}

fn locate_column(bytes: &[u8], encoding: DetectedEncoding, start: ByteOffset, target: u64,
    kind: ReaderColumnKind, canceled: &mut impl FnMut() -> bool) -> Result<ByteOffset, ReaderSessionError> {
    let first = usize::try_from(start.get()).map_err(|_| ReaderSessionError::InvalidRange)?;
    let remaining = bytes.get(first..).ok_or(ReaderSessionError::InvalidRange)?;
    let mut decoder = StatefulChunkDecoder::with_raw_offset(encoding, start.get());
    for chunk in remaining.chunks(POSITION_DECODE_BYTES) {
        check(canceled)?;
        let map = decoder.decode_chunk(chunk)?;
        if let Some(offset) = column_in_chunk(&map, target, kind)? {
            check(canceled)?;
            return Ok(offset);
        }
    }
    check(canceled)?;
    if let Some(map) = decoder.finish()? {
        if let Some(offset) = column_in_chunk(&map, target, kind)? {
            check(canceled)?;
            return Ok(offset);
        }
    }
    Err(ReaderSessionError::MissingColumn)
}

fn column_in_chunk(map: &CaptureEncodingMap, target: u64, kind: ReaderColumnKind)
    -> Result<Option<ByteOffset>, ReaderSessionError> {
    let terminator = map.decoded_text().find(|ch| ch == '\r' || ch == '\n');
    let content = &map.decoded_text()[..terminator.unwrap_or(map.decoded_text().len())];
    let (base, count) = match kind {
        ReaderColumnKind::UnicodeScalar => (map.base_scalar_offset(), content.chars().count() as u64),
        ReaderColumnKind::Utf16CodeUnit => (map.base_utf16_offset(), content.encode_utf16().count() as u64),
        ReaderColumnKind::DecodedUtf8Byte => (map.base_decoded_offset(), content.len() as u64),
    };
    let end = base.checked_add(count).ok_or(ReaderSessionError::InvalidRange)?;
    if target < base { return Err(ReaderSessionError::InvalidRange); }
    if target <= end {
        let offset = match kind {
            ReaderColumnKind::UnicodeScalar => map.scalar_to_byte_offset(ScalarIndex::new(target))?,
            ReaderColumnKind::Utf16CodeUnit => map.utf16_to_byte_offset(Utf16CodeUnitOffset::new(target))?,
            ReaderColumnKind::DecodedUtf8Byte => map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(target))?,
        };
        return Ok(Some(offset));
    }
    if terminator.is_some() { return Err(ReaderSessionError::MissingColumn); }
    Ok(None)
}

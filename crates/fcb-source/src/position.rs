#![forbid(unsafe_code)]

//! Logical line/column positions in one ordered source observation.
//!
//! Physical line endings and decoder carry remain owned by the existing source
//! scanners. This adapter composes them without retaining a line or indexing the
//! whole input. Hosts supply bounded chunks, cancellation, source identity and
//! memory admission; resolving a position does not reread any source bytes.

use std::mem::size_of;
use fcb_core::{ByteOffset, DecodedUtf8Offset, ScalarIndex, Utf16CodeUnitOffset};
use crate::{CaptureEncodingMap, DetectedEncoding, MappingSpan, SourceError, StatefulChunkDecoder};
use crate::line_index::{LineNumber, LineWindowError, LineWindowScanner};

/// Logical one-based columns. A tab counts as one unit. These are not display
/// cells, graphemes, glyph positions or bidirectional presentation order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceColumnKind { UnicodeScalar, Utf16CodeUnit, DecodedUtf8Byte }
impl SourceColumnKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnicodeScalar => "unicode_scalar",
            Self::Utf16CodeUnit => "utf16_code_unit",
            Self::DecodedUtf8Byte => "decoded_utf8_byte",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColumnPositionError {
    InvalidRange, MissingColumn, Source(SourceError),
}
impl From<SourceError> for ColumnPositionError {
    fn from(error: SourceError) -> Self { Self::Source(error) }
}

/// Resolve a one-based logical column through a map emitted by one
/// StatefulChunkDecoder that starts at the selected physical line's raw offset.
/// Its decoded/scalar/UTF-16 domain begins at zero, even when the raw line does
/// not. None means more input is needed. Only an observed CR/LF establishes a
/// missing column here; callers finalize pending decoder bytes at real EOF.
/// The content-end caret is valid. Interior UTF-8 and surrogate boundaries are
/// rejected by the shared map instead of rounded to an adjacent scalar.
pub fn column_in_chunk(map: &CaptureEncodingMap, column: u64, kind: SourceColumnKind)
    -> Result<Option<ByteOffset>, ColumnPositionError> {
    let target = column.checked_sub(1).ok_or(ColumnPositionError::InvalidRange)?;
    // A pending partial BOM/scalar is not an empty resolved line.
    if map.total_raw_bytes().get() == 0 { return Ok(None); }
    let terminator = map.decoded_text().find(|ch| ch == '\r' || ch == '\n');
    let content = &map.decoded_text()[..terminator.unwrap_or(map.decoded_text().len())];
    let (base, count) = match kind {
        SourceColumnKind::UnicodeScalar => (map.base_scalar_offset(), content.chars().count() as u64),
        SourceColumnKind::Utf16CodeUnit => (map.base_utf16_offset(), content.encode_utf16().count() as u64),
        SourceColumnKind::DecodedUtf8Byte => (map.base_decoded_offset(), content.len() as u64),
    };
    let end = base.checked_add(count).ok_or(ColumnPositionError::InvalidRange)?;
    if target < base { return Err(ColumnPositionError::InvalidRange); }
    if target <= end {
        let offset = match kind {
            SourceColumnKind::UnicodeScalar => map.scalar_to_byte_offset(ScalarIndex::new(target))?,
            SourceColumnKind::Utf16CodeUnit => map.utf16_to_byte_offset(Utf16CodeUnitOffset::new(target))?,
            SourceColumnKind::DecodedUtf8Byte => map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(target))?,
        };
        return Ok(Some(offset));
    }
    if terminator.is_some() { return Err(ColumnPositionError::MissingColumn); }
    Ok(None)
}

pub const MAX_POSITION_STEP_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineColumnError {
    InvalidRequest, NoncontiguousInput, InvalidState, CounterOverflow,
    Lines(LineWindowError), Column(ColumnPositionError),
}
impl From<LineWindowError> for LineColumnError {
    fn from(error: LineWindowError) -> Self { Self::Lines(error) }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineColumnStatus {
    Pending,
    Resolved { original_offset: ByteOffset },
    MissingLine,
    MissingColumn,
}

/// Constant retained state plus one bounded decoder map per step. Original
/// bytes remain with the caller, which must retain any desired context from
/// the same supplied sequence. No source length or complete capture is needed.
pub struct LineColumnScanner {
    lines: LineWindowScanner,
    decoder: Option<StatefulChunkDecoder>,
    encoding: DetectedEncoding,
    column: u64,
    kind: SourceColumnKind,
    offset: u64,
    previous: Option<(u64, u8)>,
    line_start: Option<ByteOffset>,
    status: LineColumnStatus,
    eof: bool,
    failed: bool,
}
impl LineColumnScanner {
    pub fn new(line: u64, column: u64, kind: SourceColumnKind, encoding: DetectedEncoding)
        -> Result<Self, LineColumnError> {
        if column == 0 || line == 0 || encoding == DetectedEncoding::Unsupported {
            return Err(LineColumnError::InvalidRequest);
        }
        let first = LineNumber::new(line).map_err(|_| LineColumnError::InvalidRequest)?;
        Ok(Self { lines: LineWindowScanner::new(first, 1, encoding)?, decoder: None, encoding,
            column, kind, offset: 0, previous: None, line_start: None,
            status: LineColumnStatus::Pending, eof: false, failed: false })
    }

    /// Conservative workspace envelope before any step's decoder allocation:
    /// input/carry, geometrically grown map vectors, strings and old/new overlap.
    /// Retained context and I/O buffers require their own host admission.
    pub fn workspace_bytes() -> usize {
        (MAX_POSITION_STEP_BYTES + 8) * (4 * size_of::<MappingSpan>() + 16)
            + size_of::<StatefulChunkDecoder>() + size_of::<CaptureEncodingMap>()
            + size_of::<Self>()
    }
    pub const fn status(&self) -> LineColumnStatus { self.status }
    pub const fn bytes_scanned(&self) -> u64 { self.offset }
    pub const fn selected_line_start(&self) -> Option<ByteOffset> { self.line_start }

    /// At most MAX_POSITION_STEP_BYTES can enter a decoder map. Returning a
    /// result may have examined the rest of that bounded chunk too. A work
    /// cutoff is never an EOF; the caller keeps Pending or discards the attempt.
    pub fn step(&mut self, offset: ByteOffset, input: &[u8], byte_budget: usize)
        -> Result<usize, LineColumnError> {
        if self.failed || self.eof { return Err(LineColumnError::InvalidState); }
        if offset.get() != self.offset { return Err(LineColumnError::NoncontiguousInput); }
        if byte_budget > MAX_POSITION_STEP_BYTES { return Err(LineColumnError::InvalidRequest); }
        if self.status != LineColumnStatus::Pending { return Ok(0); }
        let count = input.len().min(byte_budget);
        if count == 0 { return Ok(0); }
        let end = self.offset.checked_add(count as u64).ok_or(LineColumnError::CounterOverflow)?;
        let result = self.consume(&input[..count]);
        if result.is_err() { self.failed = true; }
        result?;
        self.previous = Some((end - 1, input[count - 1]));
        self.offset = end;
        Ok(count)
    }

    fn consume(&mut self, bytes: &[u8]) -> Result<(), LineColumnError> {
        self.lines.step(ByteOffset::new(self.offset), bytes, bytes.len())?;
        let Some(start) = self.lines.selected_start() else { return Ok(()); };
        let mut from = 0;
        if self.decoder.is_none() {
            self.line_start = Some(start);
            self.decoder = Some(StatefulChunkDecoder::with_raw_offset(self.encoding, start.get()));
            if start.get() < self.offset {
                // Only a split UTF-16 code unit can make selected_start precede
                // this block. The physical-line scanner owns that determination.
                let (at, byte) = self.previous.ok_or(LineColumnError::InvalidState)?;
                if at != start.get() || at.checked_add(1) != Some(self.offset) {
                    return Err(LineColumnError::InvalidState);
                }
                self.decode(&[byte])?;
            } else {
                from = usize::try_from(start.get() - self.offset).map_err(|_| LineColumnError::CounterOverflow)?;
            }
        }
        self.decode(bytes.get(from..).ok_or(LineColumnError::InvalidState)?)
    }

    fn decode(&mut self, bytes: &[u8]) -> Result<(), LineColumnError> {
        if self.status != LineColumnStatus::Pending || bytes.is_empty() { return Ok(()); }
        let map = self.decoder.as_mut().ok_or(LineColumnError::InvalidState)?
            .decode_chunk(bytes).map_err(|error| LineColumnError::Column(error.into()))?;
        self.accept_map(&map)
    }

    fn accept_map(&mut self, map: &CaptureEncodingMap) -> Result<(), LineColumnError> {
        match column_in_chunk(map, self.column, self.kind) {
            Ok(Some(original_offset)) => self.status = LineColumnStatus::Resolved { original_offset },
            Ok(None) => {},
            Err(ColumnPositionError::MissingColumn) => self.status = LineColumnStatus::MissingColumn,
            Err(error) => return Err(LineColumnError::Column(error)),
        }
        Ok(())
    }

    /// Finalize only after a real zero-byte read. A trailing CR/LF/CRLF creates
    /// no phantom line. A final partial code unit follows shared replacement
    /// decoding, with exact raw boundaries retained.
    pub fn finish(&mut self) -> Result<LineColumnStatus, LineColumnError> {
        if self.failed { return Err(LineColumnError::InvalidState); }
        if self.eof || self.status != LineColumnStatus::Pending { return Ok(self.status); }
        self.eof = true;
        let result = self.finish_inner();
        if result.is_err() { self.failed = true; }
        result?;
        Ok(self.status)
    }

    fn finish_inner(&mut self) -> Result<(), LineColumnError> {
        self.lines.finish()?;
        let Some(start) = self.lines.selected_start() else {
            self.status = LineColumnStatus::MissingLine; return Ok(());
        };
        if self.decoder.is_none() {
            self.line_start = Some(start);
            self.decoder = Some(StatefulChunkDecoder::with_raw_offset(self.encoding, start.get()));
            // EOF can establish a final line containing one incomplete UTF-16
            // code unit. Its byte is still in the scanner's constant carry.
            let (at, byte) = self.previous.ok_or(LineColumnError::InvalidState)?;
            if at != start.get() { return Err(LineColumnError::InvalidState); }
            self.decode(&[byte])?;
        }
        if self.status == LineColumnStatus::Pending {
            if let Some(map) = self.decoder.as_mut().ok_or(LineColumnError::InvalidState)?
                .finish().map_err(|error| LineColumnError::Column(error.into()))? {
                self.accept_map(&map)?;
            }
        }
        if self.status == LineColumnStatus::Pending { self.status = LineColumnStatus::MissingColumn; }
        Ok(())
    }
}

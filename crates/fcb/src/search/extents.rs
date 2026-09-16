#![forbid(unsafe_code)]

//! Logical-text reading of bounded source extents. Original coordinates are
//! absolute; decoded coordinates are local to this window. Unseen source does
//! not yield global line numbers or a whole-file completeness claim.

use std::mem::size_of;
use fcb_core::{ByteLength, ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range,
    QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_source::{CaptureEncodingMap, DetectedEncoding, MappingSpan, SpanKind};
use crate::{BrowserSession, FcbError, FramePlan};
use super::reading_window::ReadingSelection;

pub use fcb_source::confined::range::{ExtentConsistency, ExtentError, ExtentReadState,
    ExtentReadStats, ExtentStepBudget, ExtentWindowRequest, FileExtentRead,
    FileRangeReader, ObservedExtent};

pub const MAX_EXTENT_TEXT_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExtentViewError {
    Extent(ExtentError), Source(FcbError), InvalidRange, InvalidLimits,
    ContextUnavailable, UnsupportedEncoding, ResourceDenied,
    Canceled, StaleGeneration, StaleObservation,
}
impl std::fmt::Display for ExtentViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Extent(error) => write!(f, "{error}"),
            Self::Source(error) => write!(f, "{error}"),
            Self::InvalidRange => f.write_str("EXTENT_VIEW_INVALID_RANGE"),
            Self::InvalidLimits => f.write_str("EXTENT_VIEW_INVALID_LIMITS"),
            Self::ContextUnavailable => f.write_str("EXTENT_VIEW_CONTEXT_UNAVAILABLE"),
            Self::UnsupportedEncoding => f.write_str("EXTENT_VIEW_UNSUPPORTED_ENCODING"),
            Self::ResourceDenied => f.write_str("EXTENT_VIEW_RESOURCE_DENIED"),
            Self::Canceled => f.write_str("EXTENT_VIEW_CANCELED"),
            Self::StaleGeneration => f.write_str("EXTENT_VIEW_STALE_GENERATION"),
            Self::StaleObservation => f.write_str("EXTENT_VIEW_STALE_OBSERVATION"),
        }
    }
}
impl std::error::Error for ExtentViewError {}
impl From<ExtentError> for ExtentViewError { fn from(error: ExtentError) -> Self { Self::Extent(error) } }

#[derive(Clone, Debug)]
pub struct ExtentView { extent: ObservedExtent }
impl BrowserSession {
    pub fn open_extent(&self, extent: ObservedExtent) -> Result<ExtentView, ExtentViewError> {
        if extent.request().file().owner() != self.owner() {
            return Err(ExtentViewError::Source(FcbError::OwnerMismatch));
        }
        Ok(ExtentView { extent })
    }
}
impl ExtentView {
    pub fn extent(&self) -> &ObservedExtent { &self.extent }
    pub fn raw_selection(&self, range: ByteRange) -> Result<&[u8], ExtentViewError> {
        Ok(self.extent.range_bytes(range)?)
    }
    pub fn frame_plan(&self) -> FramePlan {
        FramePlan::new(self.extent.request().file().owner(), self.extent.request().file(),
            self.extent.request().revision(), self.extent.range())
    }
    /// Encoding is declared for this observation. Four context bytes on each
    /// side are required except at known boundaries. This is scalar/CRLF
    /// context, not sufficient context for native shaping or bidi layout.
    pub fn decode(&self, requested: ByteRange, encoding: DetectedEncoding, generation: QueryGeneration,
        budget: &ResourceBudget, allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
        -> Result<ExtentText, ExtentViewError> {
        if generation.owner() != self.extent.request().file().owner() {
            return Err(ExtentViewError::Source(FcbError::OwnerMismatch));
        }
        if requested.len().get() > MAX_EXTENT_TEXT_BYTES as u64 { return Err(ExtentViewError::InvalidLimits); }
        if requested.is_empty() && requested.end().get() != self.extent.observed_length().get() {
            return Err(ExtentViewError::InvalidRange);
        }
        if encoding == DetectedEncoding::Unsupported { return Err(ExtentViewError::UnsupportedEncoding); }
        self.extent.range_bytes(requested)?;
        if canceled() { return Err(ExtentViewError::Canceled); }
        let range = self.extent.range();
        let eof_known = self.extent.request_filled()
            && range.end().get() == self.extent.observed_length().get()
            && matches!(self.extent.consistency(), ExtentConsistency::HostSupplied | ExtentConsistency::UnchangedMetadata);
        if range.start().get() > requested.start().get().saturating_sub(4)
            || (!eof_known && range.end().get() < requested.end().get().saturating_add(4)) {
            return Err(ExtentViewError::ContextUnavailable);
        }
        let mut start = floor_boundary(&self.extent, requested.start().get(), encoding)?;
        let mut end = floor_boundary(&self.extent, requested.end().get(), encoding)?;
        let bytes = self.extent.bytes();
        let header = if range.start().get() == 0 {
            match encoding {
                DetectedEncoding::Utf8 { has_bom: true } if bytes.starts_with(&[0xef, 0xbb, 0xbf]) => 3,
                DetectedEncoding::Utf16Le if bytes.starts_with(&[0xff, 0xfe]) => 2,
                DetectedEncoding::Utf16Be if bytes.starts_with(&[0xfe, 0xff]) => 2,
                _ => 0,
            }
        } else { 0 };
        start = start.max(header);
        end = end.max(start);
        // Prefix context keeps an in-content FEFF out of the decoder's leading
        // BOM position. None of that context is exposed as visible text.
        let mut map_start = start.saturating_sub(4).max(range.start().get());
        if encoding.is_utf16() { map_start -= map_start % 2; }
        if map_start < range.start().get() { return Err(ExtentViewError::ContextUnavailable); }
        let raw = self.extent.range_bytes(raw_range(map_start, end)?)?;
        let map_encoding = match encoding {
            DetectedEncoding::Utf8 { has_bom } => DetectedEncoding::Utf8 { has_bom: has_bom && map_start == 0 && header == 3 },
            other => other,
        };
        let charge = raw.len().checked_add(8).and_then(|n| n.checked_mul(4 * size_of::<MappingSpan>() + 16))
            .and_then(|n| n.checked_add(size_of::<ExtentText>())).ok_or(ExtentViewError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(generation.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ExtentViewError::ResourceDenied)?;
        if canceled() { return Err(ExtentViewError::Canceled); }
        let map = CaptureEncodingMap::build_with_base_offset(raw, map_encoding, map_start, 0, 0, 0)
            .map_err(|_| ExtentViewError::UnsupportedEncoding)?;
        let decoded_base = usize::try_from(map.byte_to_decoded_utf8_offset(ByteOffset::new(start))
            .map_err(|_| ExtentViewError::InvalidRange)?.get()).map_err(|_| ExtentViewError::InvalidRange)?;
        if !map.decoded_text().is_char_boundary(decoded_base) { return Err(ExtentViewError::InvalidRange); }
        let first_line = if map_start == 0 { Some(count_lines(&map.decoded_text()[..decoded_base])) } else { None };
        if canceled() { return Err(ExtentViewError::Canceled); }
        Ok(ExtentText { extent: self.extent.clone(), range: raw_range(start, end)?, generation,
            map, decoded_base, first_line, _lease: lease })
    }
}

pub struct ExtentText {
    extent: ObservedExtent,
    range: ByteRange,
    generation: QueryGeneration,
    map: CaptureEncodingMap,
    decoded_base: usize,
    first_line: Option<u64>,
    _lease: ResourceLease,
}
impl ExtentText {
    pub fn extent(&self) -> &ObservedExtent { &self.extent }
    pub fn range(&self) -> ByteRange { self.range }
    pub fn generation(&self) -> QueryGeneration { self.generation }
    pub fn text(&self) -> &str { &self.map.decoded_text()[self.decoded_base..] }
    pub fn first_line_number(&self) -> Option<u64> { self.first_line }
    pub fn has_replacements(&self) -> bool { self.replacements_in(self.range) }
    pub fn next_offset(&self) -> Option<ByteOffset> {
        (self.range.end().get() < self.extent.observed_length().get()).then_some(self.range.end())
    }
    pub fn frame_plan(&self) -> FramePlan {
        FramePlan::new(self.extent.request().file().owner(), self.extent.request().file(),
            self.extent.request().revision(), self.range)
    }
    pub fn validate_delivery(&self, view: &ExtentView, generation: QueryGeneration) -> Result<(), ExtentViewError> {
        if self.generation != generation { return Err(ExtentViewError::StaleGeneration); }
        if self.extent.request().file() != view.extent.request().file()
            || self.extent.request().revision() != view.extent.request().revision()
            || self.extent.range() != view.extent.range()
            || self.extent.bytes() != view.extent.bytes() {
            return Err(ExtentViewError::StaleObservation);
        }
        Ok(())
    }
    pub fn source_to_text(&self, range: ByteRange) -> Result<DecodedUtf8Range, ExtentViewError> {
        if range.start() < self.range.start() || range.end() > self.range.end() { return Err(ExtentViewError::InvalidRange); }
        let first = self.map.byte_to_decoded_utf8_offset(range.start()).map_err(|_| ExtentViewError::InvalidRange)?.get();
        let last = self.map.byte_to_decoded_utf8_offset(range.end()).map_err(|_| ExtentViewError::InvalidRange)?.get();
        decoded_range(first.checked_sub(self.decoded_base as u64).ok_or(ExtentViewError::InvalidRange)?,
            last.checked_sub(self.decoded_base as u64).ok_or(ExtentViewError::InvalidRange)?)
    }
    pub fn text_selection(&self, range: DecodedUtf8Range) -> Result<ReadingSelection<'_>, ExtentViewError> {
        let first = usize::try_from(range.start().get()).map_err(|_| ExtentViewError::InvalidRange)?;
        let last = usize::try_from(range.end().get()).map_err(|_| ExtentViewError::InvalidRange)?;
        let text = self.text().get(first..last).ok_or(ExtentViewError::InvalidRange)?;
        let mapped = decoded_range((self.decoded_base + first) as u64, (self.decoded_base + last) as u64)?;
        let original = self.map.decoded_utf8_range_to_byte_range(mapped).map_err(|_| ExtentViewError::InvalidRange)?;
        Ok(ReadingSelection { text, original_range: original, original_bytes: self.extent.range_bytes(original)?,
            contains_replacements: self.replacements_in(original) })
    }
    fn replacements_in(&self, range: ByteRange) -> bool {
        self.map.spans().iter().any(|span| span.raw_start < range.end().get() && span.raw_end() > range.start().get()
            && matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte))
    }
}
fn count_lines(prefix: &str) -> u64 {
    let mut count = 1u64;
    let mut after_cr = false;
    for ch in prefix.chars() {
        if ch == '\r' || (ch == '\n' && !after_cr) { count += 1; }
        after_cr = ch == '\r';
    }
    count
}
fn raw_range(start: u64, end: u64) -> Result<ByteRange, ExtentViewError> {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| ExtentViewError::InvalidRange)
}
fn decoded_range(start: u64, end: u64) -> Result<DecodedUtf8Range, ExtentViewError> {
    DecodedUtf8Range::new(DecodedUtf8Offset::new(start), DecodedUtf8Offset::new(end)).map_err(|_| ExtentViewError::InvalidRange)
}
fn unit(extent: &ObservedExtent, offset: u64, encoding: DetectedEncoding) -> Result<u16, ExtentViewError> {
    let end = offset.checked_add(2).ok_or(ExtentViewError::InvalidRange)?;
    let bytes = extent.range_bytes(raw_range(offset, end)?)?;
    Ok(if encoding == DetectedEncoding::Utf16Le { u16::from_le_bytes([bytes[0], bytes[1]]) }
        else { u16::from_be_bytes([bytes[0], bytes[1]]) })
}
fn floor_boundary(extent: &ObservedExtent, mut offset: u64, encoding: DetectedEncoding) -> Result<u64, ExtentViewError> {
    let available = extent.range();
    if offset == available.end().get() { return Ok(offset); }
    if encoding.is_utf16() {
        offset -= offset % 2;
        if offset < available.start().get() { return Err(ExtentViewError::ContextUnavailable); }
        if offset >= available.start().get().saturating_add(2) && available.end().get() - offset >= 2 {
            let previous = unit(extent, offset - 2, encoding)?;
            let current = unit(extent, offset, encoding)?;
            if ((0xd800..=0xdbff).contains(&previous) && (0xdc00..=0xdfff).contains(&current))
                || (previous == 13 && current == 10) { offset -= 2; }
        }
    } else {
        let base = available.start().get();
        let local = usize::try_from(offset - base).map_err(|_| ExtentViewError::InvalidRange)?;
        let bytes = extent.bytes();
        let mut boundary = local;
        for start in local.saturating_sub(3)..local {
            let width = match bytes[start] { 0xc2..=0xdf => 2, 0xe0..=0xef => 3, 0xf0..=0xf4 => 4, _ => 1 };
            if start + width > local && start + width <= bytes.len()
                && std::str::from_utf8(&bytes[start..start + width]).is_ok() { boundary = start; break; }
        }
        if boundary > 0 && bytes[boundary] == b'\n' && bytes[boundary - 1] == b'\r' { boundary -= 1; }
        offset = base + boundary as u64;
    }
    Ok(offset)
}

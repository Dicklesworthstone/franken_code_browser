#![forbid(unsafe_code)]

//! Optional readable source evidence beside authoritative original-byte hex.
//! Decoding and source-domain maps belong to `fcb-source`. These adapters only
//! qualify a selected range and stream its existing decoded text into JSON.
//! No replacement character or widened selection is presented as exact text.

use fcb::{ByteOffset, ByteRange};
use fcb::source::{CaptureEncodingMap, DetectedEncoding, SpanKind, StatefulChunkDecoder, detect_encoding};
use crate::output::{Output, OutputError};

const CHUNK_BYTES: usize = 4 * 1024;
/// Fixed fields, longest status/encoding labels, and a null text value. Reserve
/// this per entry before admitting optional text or any original-byte payload.
pub(crate) const FIELDS_RESERVE: usize = 192;
/// A caller reserves this once while preparing/writing selections. Per-scalar
/// maps never cover more than one chunk (plus the decoder's four-byte carry).
pub(crate) const DECODE_SCRATCH_BYTES: u64 = ((CHUNK_BYTES + 4) * 256 + 4096) as u64;

#[derive(Clone, Copy)]
pub(crate) struct TextSource<'a> {
    pub bytes: &'a [u8],
    pub original_start: u64,
    pub encoding: DetectedEncoding,
}
impl<'a> TextSource<'a> {
    pub(crate) fn complete(bytes: &'a [u8]) -> Self {
        Self { bytes, original_start: 0, encoding: detect_encoding(bytes) }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextStatus {
    Exact,
    MalformedSource,
    InvalidBoundary,
    OutputBudget,
    SourceUnavailable,
    NoSelection,
    UnsupportedEncoding,
}
impl TextStatus {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::MalformedSource => "malformed-source",
            Self::InvalidBoundary => "invalid-boundary",
            Self::OutputBudget => "output-budget",
            Self::SourceUnavailable => "source-unavailable",
            Self::NoSelection => "no-selection",
            Self::UnsupportedEncoding => "unsupported-encoding",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextError { Canceled, Decode, Output(OutputError) }
impl From<OutputError> for TextError {
    fn from(error: OutputError) -> Self { Self::Output(error) }
}

/// Borrowed, immutable evidence. The measured decoder is replayed at write time
/// so neither a whole-source map nor a second source-sized string is retained.
pub(crate) struct PreparedText<'a> {
    bytes: &'a [u8],
    raw_start: u64,
    encoding: DetectedEncoding,
    status: TextStatus,
    json_bytes: usize,
    json_scalars: usize,
    bom_omitted: bool,
}
impl PreparedText<'_> {
    /// Serialized text value including quotes; a null value measures four.
    pub(crate) const fn json_bytes(&self) -> usize { self.json_bytes }
    pub(crate) const fn json_scalars(&self) -> usize { self.json_scalars }
    pub(crate) const fn status(&self) -> TextStatus { self.status }
}

/// Measure exact decoded text under independent serialized JSON limits. The
/// caller reserves FIELDS_RESERVE separately, which includes the null fallback;
/// successful text consumes `json_bytes/scalars().saturating_sub(4)` extra.
/// Encoding comes from the complete source's prefix or a retained observation,
/// never by guessing from an arbitrary selected slice.
pub(crate) fn prepare<'a>(source: TextSource<'a>, range: ByteRange,
    max_json_bytes: usize, max_json_scalars: usize, canceled: &mut impl FnMut() -> bool)
    -> Result<PreparedText<'a>, TextError> {
    if canceled() { return Err(TextError::Canceled); }
    let mut prepared = PreparedText { bytes: &[], raw_start: range.start().get(),
        encoding: source.encoding, status: TextStatus::InvalidBoundary,
        json_bytes: 4, json_scalars: 4, bom_omitted: false };
    let Some(start) = range.start().get().checked_sub(source.original_start)
        .and_then(|value| usize::try_from(value).ok()) else { return Ok(prepared); };
    let Some(end) = range.end().get().checked_sub(source.original_start)
        .and_then(|value| usize::try_from(value).ok()) else { return Ok(prepared); };
    let Some(bytes) = source.bytes.get(start..end) else { return Ok(prepared); };
    if source.original_start.checked_add(source.bytes.len() as u64).is_none() { return Ok(prepared); }
    prepared.bytes = bytes;
    if source.encoding == DetectedEncoding::Unsupported {
        prepared.status = TextStatus::UnsupportedEncoding;
        return Ok(prepared);
    }
    if !boundary(source, start)? || !boundary(source, end)? { return Ok(prepared); }
    let mut json_bytes = 2usize;
    let mut json_scalars = 2usize;
    let mut status = TextStatus::Exact;
    decode(bytes, source.encoding, range.start().get(), canceled, |map| {
        if map.spans().iter().any(|span| matches!(span.kind,
            SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
            status = TextStatus::MalformedSource;
            return Ok(false);
        }
        for ch in map.decoded_text().chars() {
            let (bytes, scalars) = json_scalar_size(ch);
            let Some(next_bytes) = json_bytes.checked_add(bytes) else {
                status = TextStatus::OutputBudget; return Ok(false);
            };
            let Some(next_scalars) = json_scalars.checked_add(scalars) else {
                status = TextStatus::OutputBudget; return Ok(false);
            };
            if next_bytes > max_json_bytes || next_scalars > max_json_scalars {
                status = TextStatus::OutputBudget; return Ok(false);
            }
            json_bytes = next_bytes; json_scalars = next_scalars;
        }
        Ok(true)
    })?;
    if status == TextStatus::Exact && (json_bytes > max_json_bytes || json_scalars > max_json_scalars) {
        status = TextStatus::OutputBudget;
    }
    prepared.status = status;
    if status == TextStatus::Exact {
        prepared.json_bytes = json_bytes; prepared.json_scalars = json_scalars;
        prepared.bom_omitted = range.start().get() == 0 && initial_bom(source) > 0
            && range.end().get() >= initial_bom(source);
    }
    Ok(prepared)
}

/// Check each endpoint against a tiny contextual map. This distinguishes a
/// range inside a valid scalar from a complete but malformed source sequence,
/// without decoding the whole capture or borrowing excluded bytes for text.
fn boundary(source: TextSource<'_>, offset: usize) -> Result<bool, TextError> {
    let absolute = source.original_start + offset as u64;
    if absolute > 0 && absolute < initial_bom(source) { return Ok(false); }
    // A real source/window endpoint may contain an incomplete final scalar;
    // the selected decoder then reports malformed-source rather than inventing
    // bytes outside the retained evidence.
    if offset == source.bytes.len() { return Ok(true); }
    if source.encoding.is_utf16() && !absolute.is_multiple_of(2) { return Ok(false); }
    let mut begin = offset.saturating_sub(4);
    if source.encoding.is_utf16() && !(source.original_start + begin as u64).is_multiple_of(2) {
        begin += 1;
    }
    let end = offset.saturating_add(4).min(source.bytes.len());
    let map = CaptureEncodingMap::build_with_base_offset(&source.bytes[begin..end], source.encoding,
        source.original_start + begin as u64, 0, 0, 0).map_err(|_| TextError::Decode)?;
    Ok(map.byte_to_decoded_utf8_offset(ByteOffset::new(absolute)).is_ok())
}

fn initial_bom(source: TextSource<'_>) -> u64 {
    if source.original_start != 0 { return 0; }
    let bom: &[u8] = match source.encoding {
        DetectedEncoding::Utf8 { has_bom: true } => &[0xef, 0xbb, 0xbf],
        DetectedEncoding::Utf16Le => &[0xff, 0xfe],
        DetectedEncoding::Utf16Be => &[0xfe, 0xff],
        _ => return 0,
    };
    if source.bytes.starts_with(bom) { bom.len() as u64 } else { 0 }
}

fn json_scalar_size(ch: char) -> (usize, usize) {
    match ch {
        '"' | '\\' | '\n' | '\r' | '\t' => (2, 2),
        '\0'..='\u{1f}' => (6, 6),
        _ => (ch.len_utf8(), 1),
    }
}

fn decode(bytes: &[u8], encoding: DetectedEncoding, raw_start: u64,
    canceled: &mut impl FnMut() -> bool,
    mut accept: impl FnMut(&CaptureEncodingMap) -> Result<bool, TextError>) -> Result<(), TextError> {
    let mut decoder = StatefulChunkDecoder::with_raw_offset(encoding, raw_start);
    for chunk in bytes.chunks(CHUNK_BYTES) {
        if canceled() { return Err(TextError::Canceled); }
        let map = decoder.decode_chunk(chunk).map_err(|_| TextError::Decode)?;
        if !accept(&map)? { return Ok(()); }
    }
    if canceled() { return Err(TextError::Canceled); }
    if let Some(map) = decoder.finish().map_err(|_| TextError::Decode)? { accept(&map)?; }
    Ok(())
}

pub(crate) fn write_fields(out: &mut Output, text: &PreparedText<'_>,
    canceled: &mut impl FnMut() -> bool) -> Result<(), TextError> {
    if canceled() { return Err(TextError::Canceled); }
    out.literal(",\"selected_text\":")?;
    if text.status == TextStatus::Exact {
        out.literal("\"")?;
        decode(text.bytes, text.encoding, text.raw_start, canceled, |map| {
            out.quoted_fragment(map.decoded_text())?;
            Ok(true)
        })?;
        out.literal("\"")?;
    } else { out.literal("null")?; }
    out.literal(",\"selected_text_status\":")?; out.quoted(text.status.name())?;
    out.literal(",\"source_encoding\":")?; out.quoted(text.encoding.name())?;
    out.literal(",\"selected_text_bom_omitted\":")?; out.boolean(text.bom_omitted)?;
    Ok(())
}

pub(crate) fn write_unavailable_fields(out: &mut Output, status: TextStatus) -> Result<(), OutputError> {
    out.literal(",\"selected_text\":null,\"selected_text_status\":")?; out.quoted(status.name())?;
    out.literal(",\"source_encoding\":null,\"selected_text_bom_omitted\":false")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn output(limit: usize) -> Output {
        let owner = fcb::ArenaOwnerId::new(1).unwrap();
        let budget = fcb::search::ResourceBudget::new(owner, fcb::ByteLength::new(4 * 1024 * 1024)).unwrap();
        Output::new(owner, limit, &budget, fcb::search::ResourceAllocationId::new(1).unwrap()).unwrap()
    }
    fn range(start: u64, end: u64) -> ByteRange {
        ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
    }
    fn selected(bytes: &[u8], start: u64, end: u64) -> PreparedText<'_> {
        prepare(TextSource::complete(bytes), range(start, end), usize::MAX, usize::MAX, &mut || false).unwrap()
    }

    #[test]
    fn exact_text_distinguishes_json_bytes_from_scalars_and_escape_expansion() {
        let bytes = "a🦀\0\n\t\r\"\\".as_bytes();
        let text = selected(bytes, 0, bytes.len() as u64);
        assert_eq!(text.status(), TextStatus::Exact);
        assert_eq!(text.json_bytes(), 2 + 1 + 4 + 6 + 2 * 5);
        assert_eq!(text.json_scalars(), 2 + 1 + 1 + 6 + 2 * 5);
        assert!(!text.bom_omitted);
    }

    #[test]
    fn declared_utf16_encoding_and_initial_bom_are_preserved_as_metadata() {
        let bytes = [0xff, 0xfe, b'A', 0, 0x3e, 0xd8, 0x80, 0xdd, 0, 0];
        let whole = selected(&bytes, 0, bytes.len() as u64);
        assert_eq!(whole.status(), TextStatus::Exact);
        assert!(whole.bom_omitted);
        let part = selected(&bytes, 2, bytes.len() as u64);
        assert_eq!(part.status(), TextStatus::Exact);
        assert_eq!(part.json_bytes(), 2 + 1 + 4 + 6);
        assert!(!part.bom_omitted);
        assert_eq!(selected(&bytes, 4, 6).status(), TextStatus::InvalidBoundary);
        assert_eq!(selected(&bytes, 6, 8).status(), TextStatus::InvalidBoundary);
        assert_eq!(selected(&bytes, 3, 8).status(), TextStatus::InvalidBoundary);
    }

    #[test]
    fn invalid_scalar_boundaries_and_malformed_source_never_become_exact_text() {
        let bytes = "a🦀z".as_bytes();
        for (start, end) in [(2, 5), (1, 4), (3, 3)] {
            assert_eq!(selected(bytes, start, end).status(), TextStatus::InvalidBoundary);
        }
        assert_eq!(selected(b"a\xffz", 0, 3).status(), TextStatus::MalformedSource);
        assert_eq!(selected(b"a\xffz", 0, 1).status(), TextStatus::Exact);
        assert_eq!(selected("\u{fffd}".as_bytes(), 0, 3).status(), TextStatus::Exact);
        assert_eq!(selected(&[0xff, 0xfe, 0x00, 0xd8], 0, 4).status(), TextStatus::MalformedSource);
    }

    #[test]
    fn an_interior_bom_remains_text_and_partial_initial_bom_is_refused() {
        let bytes = "\u{feff}a\u{feff}z".as_bytes();
        let part = selected(bytes, 4, 7);
        assert_eq!(part.status(), TextStatus::Exact);
        assert_eq!(part.json_bytes(), 5);
        assert!(!part.bom_omitted);
        assert_eq!(selected(bytes, 0, 1).status(), TextStatus::InvalidBoundary);
        assert_eq!(selected(bytes, 1, 3).status(), TextStatus::InvalidBoundary);
    }

    #[test]
    fn observed_extent_uses_declared_encoding_and_absolute_source_grid() {
        let bytes = [0, b'A', 0xd8, 0x3e, 0xdd, 0x80];
        let source = TextSource { bytes: &bytes, original_start: 1024, encoding: DetectedEncoding::Utf16Be };
        let text = prepare(source, range(1026, 1030), 100, 100, &mut || false).unwrap();
        assert_eq!(text.status(), TextStatus::Exact);
        assert_eq!(text.json_bytes(), 6);
        assert_eq!(text.json_scalars(), 3);
        assert!(!text.bom_omitted);
        assert_eq!(prepare(source, range(1027, 1030), 100, 100, &mut || false).unwrap().status(), TextStatus::InvalidBoundary);
    }

    #[test]
    fn byte_and_scalar_limits_are_independent_and_never_truncate_text() {
        let source = TextSource::complete("🦀\0".as_bytes());
        assert_eq!(prepare(source, range(0, 5), 11, 9, &mut || false).unwrap().status(), TextStatus::OutputBudget);
        assert_eq!(prepare(source, range(0, 5), 12, 8, &mut || false).unwrap().status(), TextStatus::OutputBudget);
        let text = prepare(source, range(0, 5), 12, 9, &mut || false).unwrap();
        assert_eq!(text.status(), TextStatus::Exact);
        assert_eq!((text.json_bytes(), text.json_scalars()), (12, 9));
        let omitted = prepare(source, range(0, 5), 0, 0, &mut || false).unwrap();
        assert_eq!((omitted.json_bytes(), omitted.json_scalars()), (4, 4));
    }

    #[test]
    fn decoder_carry_and_cancellation_are_bounded_to_a_chunk() {
        let mut bytes = vec![b'a'; CHUNK_BYTES - 1];
        bytes.extend_from_slice("🦀".as_bytes());
        bytes.extend_from_slice(&vec![b'z'; CHUNK_BYTES]);
        let text = selected(&bytes, 0, bytes.len() as u64);
        assert_eq!(text.status(), TextStatus::Exact);
        assert_eq!(text.json_bytes(), bytes.len() + 2);
        let mut calls = 0;
        let result = prepare(TextSource::complete(&bytes), range(0, bytes.len() as u64),
            usize::MAX, usize::MAX, &mut || { calls += 1; calls == 3 });
        assert!(matches!(result, Err(TextError::Canceled)));
        assert_eq!(calls, 3);
    }

    #[test]
    fn streaming_json_matches_ordinary_escaping_and_fits_reserved_metadata() {
        let source = format!("{}🦀\0\n\t\r\"\\{}", "a".repeat(CHUNK_BYTES - 1), "汉".repeat(CHUNK_BYTES));
        let text = selected(source.as_bytes(), 0, source.len() as u64);
        let mut out = output(source.len() * 2 + FIELDS_RESERVE);
        write_fields(&mut out, &text, &mut || false).unwrap();
        let mut expected = output(source.len() * 2 + FIELDS_RESERVE);
        expected.literal(",\"selected_text\":").unwrap(); expected.quoted(&source).unwrap();
        expected.literal(",\"selected_text_status\":\"exact\",\"source_encoding\":\"UTF-8\",\"selected_text_bom_omitted\":false").unwrap();
        assert_eq!(out.as_bytes(), expected.as_bytes());
        assert!(out.as_bytes().len() <= FIELDS_RESERVE + text.json_bytes().saturating_sub(4));
        assert!(std::str::from_utf8(out.as_bytes()).unwrap().chars().count()
            <= FIELDS_RESERVE + text.json_scalars().saturating_sub(4));
    }
}

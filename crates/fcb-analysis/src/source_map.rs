#![forbid(unsafe_code)]

//! Bounded source decoding and original-byte evidence for analysis adapters.
//!
//! The shared decoder validates the complete capture once. Only decoded text
//! and one descriptor per input chunk survive that pass; there is no retained
//! scalar-sized encoding map. Evidence conversion rebuilds at most two bounded,
//! scalar-complete maps through the same source decoder. A chunk is never EOF.

use std::{cell::Cell, mem::size_of};
use fcb_core::{ByteOffset, ByteRange, DecodedUtf8Offset};
use fcb_source::{CaptureEncodingMap, DetectedEncoding, MappingSpan, SpanKind, StatefulChunkDecoder};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceMapError {
    SourceLimit, UnsupportedEncoding, InvalidEvidence, ResourceDenied, Canceled, ComplexityLimit,
}

const CHUNK_BYTES: usize = 4096;
const MAX_CHUNK_BYTES: usize = CHUNK_BYTES + 4;

#[derive(Clone, Copy)]
struct Chunk {
    raw_start: u64,
    raw_end: u64,
    decoded_start: u64,
    decoded_end: u64,
    utf16_start: u64,
    scalar_start: u64,
}

pub(crate) struct SourceMap<'source> {
    source: &'source [u8],
    encoding: DetectedEncoding,
    text: String,
    chunks: Vec<Chunk>,
    name_bytes: Cell<usize>,
    name_limit: usize,
}

impl<'source> SourceMap<'source> {
    pub(crate) fn decoded_capacity(source_bytes: usize, encoding: DetectedEncoding, decoded_limit: usize)
        -> Result<usize, SourceMapError> {
        // Valid UTF-16 has at most three UTF-8 bytes per two original bytes.
        // Malformed input is rejected before its replacement text is appended.
        let capacity = if encoding.is_utf16() {
            source_bytes.checked_mul(3).ok_or(SourceMapError::ResourceDenied)? / 2
        } else { source_bytes };
        Ok(capacity.min(decoded_limit))
    }

    fn chunk_capacity(source_bytes: usize) -> Result<usize, SourceMapError> {
        source_bytes.checked_add(CHUNK_BYTES - 1)
            .map(|bytes| bytes / CHUNK_BYTES)
            .and_then(|chunks| chunks.checked_add(1))
            .ok_or(SourceMapError::ResourceDenied)
    }

    pub(crate) fn workspace_bytes(source_bytes: usize, decoded_capacity: usize)
        -> Result<usize, SourceMapError> {
        // One decoder buffer and one encoding map are live at a time. A map
        // has at most one span per original byte; three capacities cover the
        // old/new overlap while its span Vec grows. Sixteen bytes per input
        // byte cover decoder input and malformed-text String growth, including
        // the replacement text that is rejected before publication.
        let decoder = MAX_CHUNK_BYTES.checked_mul(
            size_of::<MappingSpan>().checked_mul(3)
                .and_then(|bytes| bytes.checked_add(16))
                .ok_or(SourceMapError::ResourceDenied)?)
            .and_then(|bytes| bytes.checked_add(4096))
            .ok_or(SourceMapError::ResourceDenied)?;
        Self::chunk_capacity(source_bytes)?.checked_mul(size_of::<Chunk>())
            .and_then(|bytes| bytes.checked_add(decoded_capacity))
            .and_then(|bytes| bytes.checked_add(decoder))
            .and_then(|bytes| bytes.checked_add(size_of::<Self>()))
            .ok_or(SourceMapError::ResourceDenied)
    }

    pub(crate) fn build(source: &'source [u8], encoding: DetectedEncoding,
        source_limit: usize, decoded_limit: usize, canceled: &mut impl FnMut() -> bool) -> Result<Self, SourceMapError> {
        if source.len() > source_limit { return Err(SourceMapError::SourceLimit); }
        if encoding == DetectedEncoding::Unsupported { return Err(SourceMapError::UnsupportedEncoding); }
        let capacity = Self::decoded_capacity(source.len(), encoding, decoded_limit)?;
        let chunk_capacity = Self::chunk_capacity(source.len())?;
        let mut text = String::new();
        text.try_reserve_exact(capacity).map_err(|_| SourceMapError::ResourceDenied)?;
        let mut chunks = Vec::new();
        chunks.try_reserve_exact(chunk_capacity).map_err(|_| SourceMapError::ResourceDenied)?;
        if text.capacity() > capacity || chunks.capacity() > chunk_capacity {
            return Err(SourceMapError::ResourceDenied);
        }
        let mut result = Self { source, encoding, text, chunks,
            name_bytes: Cell::new(0), name_limit: 0 };
        let mut decoder = StatefulChunkDecoder::new(encoding);
        let mut raw_end = 0;
        for input in source.chunks(CHUNK_BYTES) {
            if canceled() { return Err(SourceMapError::Canceled); }
            let map = decoder.decode_chunk(input).map_err(|_| SourceMapError::UnsupportedEncoding)?;
            result.append(&map, &mut raw_end, capacity, decoded_limit, canceled)?;
            // Drop this map before decoding the next input or remapping ranges.
        }
        if canceled() { return Err(SourceMapError::Canceled); }
        if let Some(map) = decoder.finish().map_err(|_| SourceMapError::UnsupportedEncoding)? {
            result.append(&map, &mut raw_end, capacity, decoded_limit, canceled)?;
        }
        if raw_end != source.len() as u64 { return Err(SourceMapError::InvalidEvidence); }
        result.name_limit = result.text.len().checked_mul(2).ok_or(SourceMapError::ResourceDenied)?;
        if canceled() { return Err(SourceMapError::Canceled); }
        Ok(result)
    }

    fn append(&mut self, map: &CaptureEncodingMap, raw_end: &mut u64, capacity: usize,
        decoded_limit: usize, canceled: &mut impl FnMut() -> bool) -> Result<(), SourceMapError> {
        if canceled() { return Err(SourceMapError::Canceled); }
        if map.base_raw_offset() != *raw_end
            || map.base_decoded_offset() != self.text.len() as u64
            || map.total_raw_bytes().get() > MAX_CHUNK_BYTES as u64 {
            return Err(SourceMapError::InvalidEvidence);
        }
        for (index, span) in map.spans().iter().enumerate() {
            if index % 256 == 0 && canceled() { return Err(SourceMapError::Canceled); }
            if matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte) {
                return Err(SourceMapError::UnsupportedEncoding);
            }
        }
        let next_raw = raw_end.checked_add(map.total_raw_bytes().get())
            .ok_or(SourceMapError::InvalidEvidence)?;
        let next_decoded = self.text.len().checked_add(map.decoded_text().len())
            .ok_or(SourceMapError::SourceLimit)?;
        if next_raw > self.source.len() as u64 { return Err(SourceMapError::InvalidEvidence); }
        if next_decoded > decoded_limit { return Err(SourceMapError::SourceLimit); }
        if next_decoded > capacity || map.total_decoded_bytes() != map.decoded_text().len() as u64 {
            return Err(SourceMapError::InvalidEvidence);
        }
        if !map.decoded_text().is_empty() {
            if self.chunks.len() == self.chunks.capacity() { return Err(SourceMapError::ResourceDenied); }
            self.chunks.push(Chunk {
                raw_start: *raw_end, raw_end: next_raw,
                decoded_start: self.text.len() as u64, decoded_end: next_decoded as u64,
                utf16_start: map.base_utf16_offset(), scalar_start: map.base_scalar_offset(),
            });
            self.text.push_str(map.decoded_text());
        }
        *raw_end = next_raw;
        Ok(())
    }

    pub(crate) fn decoded_text(&self) -> &str { &self.text }

    /// Reserve an output name before allocation, independently of parser
    /// traversal. Exact identifiers and Rust's synthesized impl labels share a
    /// two-decoded-source-byte aggregate allowance; a recognizer cannot turn
    /// overlapping evidence into unbounded retained copies.
    pub(crate) fn name_buffer(&self, capacity: usize) -> Result<String, SourceMapError> {
        let next = self.name_bytes.get().checked_add(capacity).ok_or(SourceMapError::ResourceDenied)?;
        if next > self.name_limit { return Err(SourceMapError::ComplexityLimit); }
        let mut name = String::new();
        name.try_reserve_exact(capacity).map_err(|_| SourceMapError::ResourceDenied)?;
        if name.capacity() > capacity { return Err(SourceMapError::ResourceDenied); }
        self.name_bytes.set(next);
        Ok(name)
    }

    pub(crate) fn copy_name(&self, value: &str) -> Result<String, SourceMapError> {
        let mut name = self.name_buffer(value.len())?;
        name.push_str(value);
        Ok(name)
    }

    pub(crate) fn mapped(&self, start: u64, end: u64) -> Result<ByteRange, SourceMapError> {
        let start_index = usize::try_from(start).map_err(|_| SourceMapError::InvalidEvidence)?;
        let end_index = usize::try_from(end).map_err(|_| SourceMapError::InvalidEvidence)?;
        if start >= end || self.text.get(start_index..end_index).is_none() {
            return Err(SourceMapError::InvalidEvidence);
        }
        // Each conversion owns and drops one bounded map. Even a declaration
        // spanning the complete capture does not rebuild a full encoding map.
        ByteRange::new(self.original_offset(start)?, self.original_offset(end)?)
            .map_err(|_| SourceMapError::InvalidEvidence)
    }

    fn original_offset(&self, offset: u64) -> Result<ByteOffset, SourceMapError> {
        if offset == self.text.len() as u64 {
            return Ok(ByteOffset::new(self.source.len() as u64));
        }
        let index = self.chunks.partition_point(|chunk| chunk.decoded_end <= offset);
        let chunk = self.chunks.get(index).ok_or(SourceMapError::InvalidEvidence)?;
        if offset < chunk.decoded_start { return Err(SourceMapError::InvalidEvidence); }
        let start = usize::try_from(chunk.raw_start).map_err(|_| SourceMapError::InvalidEvidence)?;
        let end = usize::try_from(chunk.raw_end).map_err(|_| SourceMapError::InvalidEvidence)?;
        let raw = self.source.get(start..end).ok_or(SourceMapError::InvalidEvidence)?;
        if raw.len() > MAX_CHUNK_BYTES { return Err(SourceMapError::InvalidEvidence); }
        let map = CaptureEncodingMap::build_with_base_offset(raw, self.encoding, chunk.raw_start,
            chunk.decoded_start, chunk.utf16_start, chunk.scalar_start)
            .map_err(|_| SourceMapError::InvalidEvidence)?;
        map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(offset))
            .map_err(|_| SourceMapError::InvalidEvidence)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{DecodedUtf8Range, DecodedUtf8Offset};
    use fcb_source::detect_encoding;

    const TEST_SOURCE_LIMIT: usize = 1024 * 1024;

    fn build<'source>(source: &'source [u8], encoding: DetectedEncoding,
        canceled: &mut impl FnMut() -> bool) -> Result<SourceMap<'source>, SourceMapError> {
        SourceMap::build(source, encoding, TEST_SOURCE_LIMIT, TEST_SOURCE_LIMIT, canceled)
    }

    fn utf16(text: &str, little: bool) -> Vec<u8> {
        let mut raw = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in text.encode_utf16() {
            raw.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
        }
        raw
    }

    #[test]
    fn sparse_maps_match_the_full_decoder_across_scalar_and_chunk_boundaries() {
        let text = format!("{}aé中😀\r\n{}\u{feff}z",
            " ".repeat(4094), "aé中😀\r\n".repeat(700));
        let mut bom_utf8 = vec![0xef, 0xbb, 0xbf];
        bom_utf8.extend_from_slice(text.as_bytes());
        for raw in [text.as_bytes().to_vec(), bom_utf8, utf16(&text, true), utf16(&text, false)] {
            let encoding = detect_encoding(&raw);
            let sparse = build(&raw, encoding, &mut || false).unwrap();
            let full = CaptureEncodingMap::build_with_encoding(&raw, encoding).unwrap();
            assert_eq!(sparse.decoded_text(), full.decoded_text());
            assert!(sparse.chunks.len() <= raw.len().div_ceil(CHUNK_BYTES) + 1);
            // Sample scalar spans in every input region and every sparse chunk
            // endpoint, independently comparing the original full-map oracle.
            let mut offsets: Vec<_> = text.char_indices().map(|(offset, _)| offset)
                .step_by(97).collect();
            for chunk in &sparse.chunks {
                offsets.push(chunk.decoded_start as usize);
                if chunk.decoded_end < text.len() as u64 { offsets.push(chunk.decoded_end as usize); }
            }
            for start in offsets {
                let scalar = text[start..].chars().next().unwrap();
                let end = start + scalar.len_utf8();
                let expected = full.decoded_utf8_range_to_byte_range(
                    DecodedUtf8Range::new(DecodedUtf8Offset::new(start as u64),
                        DecodedUtf8Offset::new(end as u64)).unwrap()).unwrap();
                assert_eq!(sparse.mapped(start as u64, end as u64).unwrap(), expected);
            }
            let expected = full.decoded_utf8_range_to_byte_range(
                DecodedUtf8Range::new(DecodedUtf8Offset::new(0),
                    DecodedUtf8Offset::new(text.len() as u64)).unwrap()).unwrap();
            assert_eq!(sparse.mapped(0, text.len() as u64).unwrap(), expected);
        }
    }

    #[test]
    fn initial_bom_is_a_header_and_interior_bom_is_exact_text() {
        for little in [false, true] {
            let text = format!("{}\u{feff}name", "a".repeat(2047));
            let raw = utf16(&text, little);
            let map = build(&raw, detect_encoding(&raw), &mut || false).unwrap();
            assert_eq!(map.mapped(0, 1).unwrap().start().get(), 2);
            let interior = map.mapped(2047, 2050).unwrap();
            assert_eq!((interior.start().get(), interior.end().get()), (4096, 4098));
            assert_eq!(map.decoded_text(), text);
        }
        let text = format!("{}\u{feff}name", "a".repeat(4093));
        let mut raw = vec![0xef, 0xbb, 0xbf];
        raw.extend_from_slice(text.as_bytes());
        let map = build(&raw, detect_encoding(&raw), &mut || false).unwrap();
        assert_eq!(map.mapped(0, 1).unwrap().start().get(), 3);
        let interior = map.mapped(4093, 4096).unwrap();
        assert_eq!((interior.start().get(), interior.end().get()), (4096, 4099));
    }

    #[test]
    fn split_utf16_surrogates_keep_global_original_offsets() {
        for little in [false, true] {
            // BOM + 2046 ASCII units place the high surrogate at 4094.
            let text = format!("{}😀name", "a".repeat(2046));
            let raw = utf16(&text, little);
            let map = build(&raw, detect_encoding(&raw), &mut || false).unwrap();
            let emoji = map.mapped(2046, 2050).unwrap();
            assert_eq!((emoji.start().get(), emoji.end().get()), (4094, 4098));
            let name = map.mapped(2050, 2054).unwrap();
            assert_eq!((name.start().get(), name.end().get()), (4098, 4106));
            assert_eq!(map.mapped(2047, 2050), Err(SourceMapError::InvalidEvidence));
        }
    }

    #[test]
    fn empty_and_bom_only_captures_have_no_invented_text() {
        for raw in [&b""[..], &[0xef, 0xbb, 0xbf][..], &[0xff, 0xfe][..], &[0xfe, 0xff][..]] {
            let map = build(raw, detect_encoding(raw), &mut || false).unwrap();
            assert!(map.decoded_text().is_empty());
            assert!(map.chunks.is_empty());
            assert_eq!(map.mapped(0, 0), Err(SourceMapError::InvalidEvidence));
        }
    }

    #[test]
    fn malformed_terminal_input_and_decoded_expansion_do_not_publish() {
        let mut raw = b"a".repeat(8192);
        raw.push(0xf0);
        assert!(matches!(build(&raw, detect_encoding(&raw), &mut || false),
            Err(SourceMapError::UnsupportedEncoding)));
        for little in [false, true] {
            for tail in [vec![0x01], if little { vec![0x00, 0xd8] } else { vec![0xd8, 0x00] }] {
                let mut raw = utf16(&"a".repeat(4096), little);
                raw.extend_from_slice(&tail);
                assert!(matches!(build(&raw, detect_encoding(&raw), &mut || false),
                    Err(SourceMapError::UnsupportedEncoding)));
            }
            let expanded = utf16(&"中".repeat(TEST_SOURCE_LIMIT / 3 + 1), little);
            assert!(expanded.len() < TEST_SOURCE_LIMIT);
            assert!(matches!(build(&expanded, detect_encoding(&expanded), &mut || false),
                Err(SourceMapError::SourceLimit)));
        }
    }

    #[test]
    fn cancellation_interrupts_between_chunks_and_name_quota_precedes_allocation() {
        let raw = b"a".repeat(8192);
        let mut polls = 0;
        assert!(matches!(build(&raw, detect_encoding(&raw), &mut || {
            polls += 1; polls > 3
        }), Err(SourceMapError::Canceled)));
        assert_eq!(polls, 4);
        let map = build(b"name", detect_encoding(b"name"), &mut || false).unwrap();
        let first = map.name_buffer(7).unwrap();
        assert_eq!(first.capacity(), 7);
        assert_eq!(map.name_bytes.get(), 7);
        assert_eq!(map.name_buffer(2), Err(SourceMapError::ComplexityLimit));
        assert_eq!(map.name_bytes.get(), 7);
        assert_eq!(map.copy_name("x").unwrap(), "x");
        assert_eq!(map.name_bytes.get(), 8);
    }
}

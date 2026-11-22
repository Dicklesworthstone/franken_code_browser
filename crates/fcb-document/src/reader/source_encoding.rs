#![forbid(unsafe_code)]

//! Translate upstream UTF-8 source coordinates back to the immutable capture.
//! Decoding and scalar boundaries belong to fcb-source. This adapter neither
//! parses Markdown nor changes the capture's bytes or identity.

use std::{mem::size_of, sync::Arc};
use fcb_core::{ByteOffset, ByteRange, DecodedUtf8Offset};
use fcb_source::encoding::{CaptureEncodingMap, SpanKind, detect_encoding};
use crate::SourceSpan;
use super::DocumentReadError;

#[derive(Clone)]
pub(crate) struct DocumentSourceEncoding {
    source_base: usize,
    decoded: Option<Arc<CaptureEncodingMap>>,
    charge: usize,
}

impl DocumentSourceEncoding {
    /// Reserve before constructing a shared decoder. The conservative envelope
    /// includes map-vector growth and old/new allocation overlap, decoded text,
    /// and the independently retained Arc. UTF-8 keeps its existing direct map.
    pub(crate) fn workspace(bytes: &[u8]) -> Result<usize, DocumentReadError> {
        if !detect_encoding(bytes).is_utf16() { return Ok(0); }
        bytes.len().checked_add(1).and_then(|n| n.checked_mul(256))
            .and_then(|n| n.checked_add(size_of::<CaptureEncodingMap>() + 4096))
            .ok_or(DocumentReadError::ResourceDenied)
    }

    /// The caller holds workspace() admission before entering this bounded
    /// whole-source decode. Reject malformed source rather than inventing exact
    /// Markdown provenance for replacement characters.
    pub(crate) fn build(bytes: &[u8], max_bytes: usize) -> Result<Self, DocumentReadError> {
        let encoding = detect_encoding(bytes);
        let source_base = encoding.bom_bytes_len() as usize;
        let charge = Self::workspace(bytes)?;
        let decoded = if encoding.is_utf16() {
            let map = CaptureEncodingMap::build_with_encoding(bytes, encoding)
                .map_err(|_| DocumentReadError::InvalidUtf8)?;
            if map.spans().iter().any(|span| matches!(span.kind,
                SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
                return Err(DocumentReadError::InvalidUtf8);
            }
            if map.decoded_text().len() > max_bytes { return Err(DocumentReadError::SourceLimit); }
            Some(Arc::new(map))
        } else {
            std::str::from_utf8(bytes).map_err(|_| DocumentReadError::InvalidUtf8)?;
            None
        };
        Ok(Self { source_base, decoded, charge })
    }

    pub(crate) fn source_base(&self) -> usize { self.source_base }
    pub(crate) fn retained_charge(&self) -> usize { self.charge }

    pub(crate) fn text<'a>(&'a self, original: &'a [u8]) -> Result<&'a str, DocumentReadError> {
        if let Some(map) = &self.decoded { return Ok(map.decoded_text()); }
        std::str::from_utf8(original.get(self.source_base..)
            .ok_or(DocumentReadError::InvalidRange)?).map_err(|_| DocumentReadError::InvalidUtf8)
    }

    pub(crate) fn original_span(&self, original: &[u8], span: SourceSpan)
        -> Result<ByteRange, DocumentReadError> {
        if self.text(original)?.get(span.start..span.end).is_none() {
            return Err(DocumentReadError::InvalidOutput);
        }
        let (start, end) = if let Some(map) = &self.decoded {
            let start = map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(span.start as u64))
                .map_err(|_| DocumentReadError::InvalidOutput)?;
            let end = map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(span.end as u64))
                .map_err(|_| DocumentReadError::InvalidOutput)?;
            (start, end)
        } else {
            let start = span.start.checked_add(self.source_base).ok_or(DocumentReadError::InvalidRange)?;
            let end = span.end.checked_add(self.source_base).ok_or(DocumentReadError::InvalidRange)?;
            (ByteOffset::new(start as u64), ByteOffset::new(end as u64))
        };
        ByteRange::new(start, end).map_err(|_| DocumentReadError::InvalidRange)
    }

    /// BOM interiors, UTF-8 interiors, odd UTF-16 positions and the middle of a
    /// surrogate pair cannot become source-navigation anchors. Require a full
    /// round trip as well as the decoder's checked conversion.
    pub(crate) fn validate_original_offset(&self, original: &[u8], offset: usize)
        -> Result<(), DocumentReadError> {
        if offset < self.source_base || offset > original.len() {
            return Err(DocumentReadError::InvalidRange);
        }
        if let Some(map) = &self.decoded {
            let raw = ByteOffset::new(offset as u64);
            let decoded = map.byte_to_decoded_utf8_offset(raw)
                .map_err(|_| DocumentReadError::InvalidRange)?;
            if map.decoded_utf8_to_byte_offset(decoded).map_err(|_| DocumentReadError::InvalidRange)? != raw {
                return Err(DocumentReadError::InvalidRange);
            }
        } else if !self.text(original)?.is_char_boundary(offset - self.source_base) {
            return Err(DocumentReadError::InvalidRange);
        }
        Ok(())
    }
}

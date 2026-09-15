#![forbid(unsafe_code)]

//! Encoding detection, bidirectional decoding maps, and domain translations (FCB-012.B / fcb-37r.2).
//!
//! §10.6, §10.8:
//! Preserve original bytes. UTF-8 is the ordinary route; explicit supported BOM-marked
//! UTF-16 routes use a checked original-byte ↔ decoded-text mapping. Other encodings require
//! an explicit qualified decoder or show a labeled escaped/byte view. No statistical guess
//! becomes a promise of exact source text.
//!
//! [`ByteOffset`], [`DecodedUtf8Offset`], [`Utf16CodeUnitOffset`], [`ScalarIndex`],
//! [`GraphemeBoundary`], and [`VisualPosition`] are distinct domains. Native text/IME/accessibility
//! adapters validate their SDK range conventions and sentinel values before converting.
//! Do not cast an unsigned "not found" sentinel or UTF-16 range directly into a source-byte slice.
//! Original byte copy, Unicode text clipboard copy, and escaped display copy are different explicit operations.

use std::fmt::Write as _;

use fcb_core::{
    ByteLength, ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, ScalarIndex,
    Utf16CodeUnitOffset, NATIVE_NOT_FOUND,
};

use crate::SourceError;

/// The detected or declared encoding of a source capture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DetectedEncoding {
    /// UTF-8 encoding, indicating whether an explicit 3-byte BOM (`0xEF, 0xBB, 0xBF`) was present.
    Utf8 { has_bom: bool },
    /// UTF-16 Little Endian with 2-byte BOM (`0xFF, 0xFE`).
    Utf16Le,
    /// UTF-16 Big Endian with 2-byte BOM (`0xFE, 0xFF`).
    Utf16Be,
    /// Unsupported or arbitrary raw byte stream requiring labeled escaped byte view.
    Unsupported,
}

impl DetectedEncoding {
    /// Number of BOM header bytes at the start of the file for this encoding.
    pub const fn bom_bytes_len(self) -> u64 {
        match self {
            Self::Utf8 { has_bom: true } => 3,
            Self::Utf16Le | Self::Utf16Be => 2,
            Self::Utf8 { has_bom: false } | Self::Unsupported => 0,
        }
    }

    /// Human-readable encoding name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Utf8 { has_bom: false } => "UTF-8",
            Self::Utf8 { has_bom: true } => "UTF-8 (BOM)",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::Unsupported => "Unsupported",
        }
    }

    /// Whether this encoding is a UTF-16 variant.
    pub const fn is_utf16(self) -> bool {
        matches!(self, Self::Utf16Le | Self::Utf16Be)
    }

    /// Whether this encoding is UTF-8.
    pub const fn is_utf8(self) -> bool {
        matches!(self, Self::Utf8 { .. })
    }
}

/// Detect source encoding from initial bytes based strictly on standard BOM markers
/// and valid UTF-8 invariants. Never makes statistical guesses.
pub fn detect_encoding(prefix: &[u8]) -> DetectedEncoding {
    if prefix.starts_with(&[0xEF, 0xBB, 0xBF]) {
        DetectedEncoding::Utf8 { has_bom: true }
    } else if prefix.starts_with(&[0xFF, 0xFE]) {
        DetectedEncoding::Utf16Le
    } else if prefix.starts_with(&[0xFE, 0xFF]) {
        DetectedEncoding::Utf16Be
    } else {
        DetectedEncoding::Utf8 { has_bom: false }
    }
}

/// The classification of a contiguous mapping span between original bytes and decoded domains.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SpanKind {
    /// Initial BOM header bytes (stripped from decoded text representation).
    BomHeader,
    /// Contiguous run of single-byte ASCII characters (1:1 in all domains).
    AsciiRun,
    /// Single BMP character with multi-byte representation in one or both domains.
    BmpMultiByte,
    /// Surrogate pair in UTF-16 (2 code units, 4 raw bytes) representing 1 supplementary scalar (> U+FFFF).
    SurrogatePair,
    /// Malformed byte sequence replaced with Unicode replacement character U+FFFD.
    ReplacementMalformed,
    /// Escaped byte representation for unsupported encodings.
    EscapedByte,
}

/// A contiguous mapping span pairing raw source bytes with decoded UTF-8, UTF-16, and scalar positions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappingSpan {
    pub raw_start: u64,
    pub raw_len: u64,
    pub decoded_start: u64,
    pub decoded_len: u64,
    pub utf16_start: u64,
    pub utf16_len: u64,
    pub scalar_start: u64,
    pub scalar_len: u64,
    pub kind: SpanKind,
}

impl MappingSpan {
    pub const fn raw_end(&self) -> u64 {
        self.raw_start + self.raw_len
    }

    pub const fn decoded_end(&self) -> u64 {
        self.decoded_start + self.decoded_len
    }

    pub const fn utf16_end(&self) -> u64 {
        self.utf16_start + self.utf16_len
    }

    pub const fn scalar_end(&self) -> u64 {
        self.scalar_start + self.scalar_len
    }
}

/// Bidirectional decoding map between original capture bytes and decoded text domains.
///
/// Supports files with more than 2^32 bytes (`u64` offsets throughout) and guarantees
/// that search matches in decoded text can be resolved back to exact original bytes
/// without Apple framework dependencies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureEncodingMap {
    encoding: DetectedEncoding,
    base_raw_offset: u64,
    base_decoded_offset: u64,
    base_utf16_offset: u64,
    base_scalar_offset: u64,
    total_raw_bytes: u64,
    total_decoded_bytes: u64,
    total_utf16_units: u64,
    total_scalars: u64,
    decoded_text: String,
    spans: Vec<MappingSpan>,
}

impl CaptureEncodingMap {
    /// Automatically detect encoding from bytes and build the bidirectional decoding map.
    pub fn build(raw_bytes: &[u8]) -> Result<Self, SourceError> {
        let encoding = detect_encoding(raw_bytes);
        Self::build_with_encoding(raw_bytes, encoding)
    }

    /// Build the decoding map using a declared encoding.
    pub fn build_with_encoding(
        raw_bytes: &[u8],
        encoding: DetectedEncoding,
    ) -> Result<Self, SourceError> {
        Self::build_with_base_offset(raw_bytes, encoding, 0, 0, 0, 0)
    }

    /// Build the decoding map with explicit base offsets for chunked streaming or large offsets (> 2^32).
    pub fn build_with_base_offset(
        raw_bytes: &[u8],
        encoding: DetectedEncoding,
        base_raw: u64,
        base_decoded: u64,
        base_utf16: u64,
        base_scalar: u64,
    ) -> Result<Self, SourceError> {
        match encoding {
            DetectedEncoding::Utf8 { has_bom } => {
                Self::build_utf8(raw_bytes, has_bom, base_raw, base_decoded, base_utf16, base_scalar)
            }
            DetectedEncoding::Utf16Le => {
                Self::build_utf16(raw_bytes, true, base_raw, base_decoded, base_utf16, base_scalar)
            }
            DetectedEncoding::Utf16Be => {
                Self::build_utf16(raw_bytes, false, base_raw, base_decoded, base_utf16, base_scalar)
            }
            DetectedEncoding::Unsupported => {
                Self::build_escaped(raw_bytes, base_raw, base_decoded, base_utf16, base_scalar)
            }
        }
    }

    fn build_utf8(
        raw_bytes: &[u8],
        has_bom: bool,
        base_raw: u64,
        base_decoded: u64,
        base_utf16: u64,
        base_scalar: u64,
    ) -> Result<Self, SourceError> {
        let mut spans = Vec::new();
        let mut decoded_text = String::with_capacity(raw_bytes.len());

        let mut raw_pos = base_raw;
        let mut dec_pos = base_decoded;
        let mut u16_pos = base_utf16;
        let mut sca_pos = base_scalar;

        let mut byte_idx = 0;
        if has_bom && raw_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            spans.push(MappingSpan {
                raw_start: raw_pos,
                raw_len: 3,
                decoded_start: dec_pos,
                decoded_len: 0,
                utf16_start: u16_pos,
                utf16_len: 0,
                scalar_start: sca_pos,
                scalar_len: 0,
                kind: SpanKind::BomHeader,
            });
            raw_pos += 3;
            byte_idx = 3;
        }

        while byte_idx < raw_bytes.len() {
            let b = raw_bytes[byte_idx];
            if b < 0x80 {
                // Coalesce ASCII run
                let run_start = byte_idx;
                while byte_idx < raw_bytes.len() && raw_bytes[byte_idx] < 0x80 {
                    byte_idx += 1;
                }
                let run_len = (byte_idx - run_start) as u64;
                // Append ASCII slice safely
                if let Ok(ascii_str) = std::str::from_utf8(&raw_bytes[run_start..byte_idx]) {
                    decoded_text.push_str(ascii_str);
                }
                spans.push(MappingSpan {
                    raw_start: raw_pos,
                    raw_len: run_len,
                    decoded_start: dec_pos,
                    decoded_len: run_len,
                    utf16_start: u16_pos,
                    utf16_len: run_len,
                    scalar_start: sca_pos,
                    scalar_len: run_len,
                    kind: SpanKind::AsciiRun,
                });
                raw_pos += run_len;
                dec_pos += run_len;
                u16_pos += run_len;
                sca_pos += run_len;
            } else {
                // Determine sequence length and decode
                let (seq_len, valid_char) = Self::decode_utf8_char(&raw_bytes[byte_idx..]);
                if let Some(c) = valid_char {
                    let mut buf = [0u8; 4];
                    let enc = c.encode_utf8(&mut buf);
                    decoded_text.push_str(enc);

                    let c_u16_len = c.len_utf16() as u64;
                    let c_dec_len = enc.len() as u64;
                    let kind = if c_u16_len == 2 {
                        SpanKind::SurrogatePair
                    } else {
                        SpanKind::BmpMultiByte
                    };

                    spans.push(MappingSpan {
                        raw_start: raw_pos,
                        raw_len: seq_len as u64,
                        decoded_start: dec_pos,
                        decoded_len: c_dec_len,
                        utf16_start: u16_pos,
                        utf16_len: c_u16_len,
                        scalar_start: sca_pos,
                        scalar_len: 1,
                        kind,
                    });
                    raw_pos += seq_len as u64;
                    dec_pos += c_dec_len;
                    u16_pos += c_u16_len;
                    sca_pos += 1;
                    byte_idx += seq_len;
                } else {
                    // Malformed sequence replaced with U+FFFD
                    decoded_text.push('\u{FFFD}');
                    spans.push(MappingSpan {
                        raw_start: raw_pos,
                        raw_len: seq_len as u64,
                        decoded_start: dec_pos,
                        decoded_len: 3, // '\u{FFFD}' in UTF-8 is 3 bytes
                        utf16_start: u16_pos,
                        utf16_len: 1,
                        scalar_start: sca_pos,
                        scalar_len: 1,
                        kind: SpanKind::ReplacementMalformed,
                    });
                    raw_pos += seq_len as u64;
                    dec_pos += 3;
                    u16_pos += 1;
                    sca_pos += 1;
                    byte_idx += seq_len;
                }
            }
        }

        Ok(Self {
            encoding: DetectedEncoding::Utf8 { has_bom },
            base_raw_offset: base_raw,
            base_decoded_offset: base_decoded,
            base_utf16_offset: base_utf16,
            base_scalar_offset: base_scalar,
            total_raw_bytes: raw_pos - base_raw,
            total_decoded_bytes: dec_pos - base_decoded,
            total_utf16_units: u16_pos - base_utf16,
            total_scalars: sca_pos - base_scalar,
            decoded_text,
            spans,
        })
    }

    fn decode_utf8_char(slice: &[u8]) -> (usize, Option<char>) {
        if slice.is_empty() {
            return (0, None);
        }
        let b0 = slice[0];
        if (0xC2..=0xDF).contains(&b0) {
            if slice.len() >= 2 && (0x80..=0xBF).contains(&slice[1]) {
                let val = (((b0 & 0x1F) as u32) << 6) | ((slice[1] & 0x3F) as u32);
                return (2, char::from_u32(val));
            }
            return (1, None);
        }
        if (0xE0..=0xEF).contains(&b0) {
            if slice.len() >= 3 {
                let b1 = slice[1];
                let b2 = slice[2];
                let valid_b1 = match b0 {
                    0xE0 => (0xA0..=0xBF).contains(&b1),
                    0xED => (0x80..=0x9F).contains(&b1), // Exclude UTF-16 surrogates
                    _ => (0x80..=0xBF).contains(&b1),
                };
                if valid_b1 && (0x80..=0xBF).contains(&b2) {
                    let val = (((b0 & 0x0F) as u32) << 12)
                        | (((b1 & 0x3F) as u32) << 6)
                        | ((b2 & 0x3F) as u32);
                    return (3, char::from_u32(val));
                }
            }
            return (1, None);
        }
        if (0xF0..=0xF4).contains(&b0) {
            if slice.len() >= 4 {
                let b1 = slice[1];
                let b2 = slice[2];
                let b3 = slice[3];
                let valid_b1 = match b0 {
                    0xF0 => (0x90..=0xBF).contains(&b1),
                    0xF4 => (0x80..=0x8F).contains(&b1),
                    _ => (0x80..=0xBF).contains(&b1),
                };
                if valid_b1 && (0x80..=0xBF).contains(&b2) && (0x80..=0xBF).contains(&b3) {
                    let val = (((b0 & 0x07) as u32) << 18)
                        | (((b1 & 0x3F) as u32) << 12)
                        | (((b2 & 0x3F) as u32) << 6)
                        | ((b3 & 0x3F) as u32);
                    return (4, char::from_u32(val));
                }
            }
            return (1, None);
        }
        (1, None)
    }

    fn build_utf16(
        raw_bytes: &[u8],
        is_le: bool,
        base_raw: u64,
        base_decoded: u64,
        base_utf16: u64,
        base_scalar: u64,
    ) -> Result<Self, SourceError> {
        let mut spans = Vec::new();
        let mut decoded_text = String::with_capacity(raw_bytes.len());

        let mut raw_pos = base_raw;
        let mut dec_pos = base_decoded;
        let mut u16_pos = base_utf16;
        let mut sca_pos = base_scalar;

        let mut byte_idx = 0;
        let bom = if is_le { [0xFF, 0xFE] } else { [0xFE, 0xFF] };
        if raw_bytes.starts_with(&bom) {
            spans.push(MappingSpan {
                raw_start: raw_pos,
                raw_len: 2,
                decoded_start: dec_pos,
                decoded_len: 0,
                utf16_start: u16_pos,
                utf16_len: 0,
                scalar_start: sca_pos,
                scalar_len: 0,
                kind: SpanKind::BomHeader,
            });
            raw_pos += 2;
            byte_idx = 2;
        }

        while byte_idx < raw_bytes.len() {
            if byte_idx + 1 >= raw_bytes.len() {
                // Dangling odd byte at EOF
                decoded_text.push('\u{FFFD}');
                spans.push(MappingSpan {
                    raw_start: raw_pos,
                    raw_len: 1,
                    decoded_start: dec_pos,
                    decoded_len: 3,
                    utf16_start: u16_pos,
                    utf16_len: 1,
                    scalar_start: sca_pos,
                    scalar_len: 1,
                    kind: SpanKind::ReplacementMalformed,
                });
                raw_pos += 1;
                dec_pos += 3;
                u16_pos += 1;
                sca_pos += 1;
                break;
            }

            let u16_val = if is_le {
                u16::from_le_bytes([raw_bytes[byte_idx], raw_bytes[byte_idx + 1]])
            } else {
                u16::from_be_bytes([raw_bytes[byte_idx], raw_bytes[byte_idx + 1]])
            };

            if u16_val < 0x80 {
                // Coalesce ASCII run
                let run_start_idx = byte_idx;
                let mut ascii_chars = 0u64;
                while byte_idx + 1 < raw_bytes.len() {
                    let v = if is_le {
                        u16::from_le_bytes([raw_bytes[byte_idx], raw_bytes[byte_idx + 1]])
                    } else {
                        u16::from_be_bytes([raw_bytes[byte_idx], raw_bytes[byte_idx + 1]])
                    };
                    if v < 0x80 {
                        decoded_text.push(v as u8 as char);
                        ascii_chars += 1;
                        byte_idx += 2;
                    } else {
                        break;
                    }
                }
                let raw_run_bytes = (byte_idx - run_start_idx) as u64;
                spans.push(MappingSpan {
                    raw_start: raw_pos,
                    raw_len: raw_run_bytes,
                    decoded_start: dec_pos,
                    decoded_len: ascii_chars,
                    utf16_start: u16_pos,
                    utf16_len: ascii_chars,
                    scalar_start: sca_pos,
                    scalar_len: ascii_chars,
                    kind: SpanKind::AsciiRun,
                });
                raw_pos += raw_run_bytes;
                dec_pos += ascii_chars;
                u16_pos += ascii_chars;
                sca_pos += ascii_chars;
            } else if (0xD800..=0xDBFF).contains(&u16_val) {
                // High surrogate: check for low surrogate
                if byte_idx + 3 < raw_bytes.len() {
                    let next_u16 = if is_le {
                        u16::from_le_bytes([raw_bytes[byte_idx + 2], raw_bytes[byte_idx + 3]])
                    } else {
                        u16::from_be_bytes([raw_bytes[byte_idx + 2], raw_bytes[byte_idx + 3]])
                    };
                    if (0xDC00..=0xDFFF).contains(&next_u16) {
                        // Valid surrogate pair
                        let scalar_val = 0x10000
                            + (((u16_val - 0xD800) as u32) << 10)
                            + ((next_u16 - 0xDC00) as u32);
                        if let Some(c) = char::from_u32(scalar_val) {
                            let mut buf = [0u8; 4];
                            let enc = c.encode_utf8(&mut buf);
                            decoded_text.push_str(enc);

                            spans.push(MappingSpan {
                                raw_start: raw_pos,
                                raw_len: 4,
                                decoded_start: dec_pos,
                                decoded_len: enc.len() as u64,
                                utf16_start: u16_pos,
                                utf16_len: 2,
                                scalar_start: sca_pos,
                                scalar_len: 1,
                                kind: SpanKind::SurrogatePair,
                            });
                            raw_pos += 4;
                            dec_pos += enc.len() as u64;
                            u16_pos += 2;
                            sca_pos += 1;
                            byte_idx += 4;
                            continue;
                        }
                    }
                }
                // Unpaired high surrogate
                decoded_text.push('\u{FFFD}');
                spans.push(MappingSpan {
                    raw_start: raw_pos,
                    raw_len: 2,
                    decoded_start: dec_pos,
                    decoded_len: 3,
                    utf16_start: u16_pos,
                    utf16_len: 1,
                    scalar_start: sca_pos,
                    scalar_len: 1,
                    kind: SpanKind::ReplacementMalformed,
                });
                raw_pos += 2;
                dec_pos += 3;
                u16_pos += 1;
                sca_pos += 1;
                byte_idx += 2;
            } else if (0xDC00..=0xDFFF).contains(&u16_val) {
                // Isolated low surrogate
                decoded_text.push('\u{FFFD}');
                spans.push(MappingSpan {
                    raw_start: raw_pos,
                    raw_len: 2,
                    decoded_start: dec_pos,
                    decoded_len: 3,
                    utf16_start: u16_pos,
                    utf16_len: 1,
                    scalar_start: sca_pos,
                    scalar_len: 1,
                    kind: SpanKind::ReplacementMalformed,
                });
                raw_pos += 2;
                dec_pos += 3;
                u16_pos += 1;
                sca_pos += 1;
                byte_idx += 2;
            } else {
                // Valid BMP character
                if let Some(c) = char::from_u32(u16_val as u32) {
                    let mut buf = [0u8; 4];
                    let enc = c.encode_utf8(&mut buf);
                    decoded_text.push_str(enc);

                    spans.push(MappingSpan {
                        raw_start: raw_pos,
                        raw_len: 2,
                        decoded_start: dec_pos,
                        decoded_len: enc.len() as u64,
                        utf16_start: u16_pos,
                        utf16_len: 1,
                        scalar_start: sca_pos,
                        scalar_len: 1,
                        kind: SpanKind::BmpMultiByte,
                    });
                    raw_pos += 2;
                    dec_pos += enc.len() as u64;
                    u16_pos += 1;
                    sca_pos += 1;
                    byte_idx += 2;
                } else {
                    decoded_text.push('\u{FFFD}');
                    spans.push(MappingSpan {
                        raw_start: raw_pos,
                        raw_len: 2,
                        decoded_start: dec_pos,
                        decoded_len: 3,
                        utf16_start: u16_pos,
                        utf16_len: 1,
                        scalar_start: sca_pos,
                        scalar_len: 1,
                        kind: SpanKind::ReplacementMalformed,
                    });
                    raw_pos += 2;
                    dec_pos += 3;
                    u16_pos += 1;
                    sca_pos += 1;
                    byte_idx += 2;
                }
            }
        }

        let encoding = if is_le {
            DetectedEncoding::Utf16Le
        } else {
            DetectedEncoding::Utf16Be
        };

        Ok(Self {
            encoding,
            base_raw_offset: base_raw,
            base_decoded_offset: base_decoded,
            base_utf16_offset: base_utf16,
            base_scalar_offset: base_scalar,
            total_raw_bytes: raw_pos - base_raw,
            total_decoded_bytes: dec_pos - base_decoded,
            total_utf16_units: u16_pos - base_utf16,
            total_scalars: sca_pos - base_scalar,
            decoded_text,
            spans,
        })
    }

    fn build_escaped(
        raw_bytes: &[u8],
        base_raw: u64,
        base_decoded: u64,
        base_utf16: u64,
        base_scalar: u64,
    ) -> Result<Self, SourceError> {
        let mut spans = Vec::with_capacity(raw_bytes.len());
        let mut decoded_text = String::with_capacity(raw_bytes.len() * 4);

        let mut raw_pos = base_raw;
        let mut dec_pos = base_decoded;
        let mut u16_pos = base_utf16;
        let mut sca_pos = base_scalar;

        for &b in raw_bytes {
            let _ = write!(decoded_text, "\\x{:02X}", b);
            spans.push(MappingSpan {
                raw_start: raw_pos,
                raw_len: 1,
                decoded_start: dec_pos,
                decoded_len: 4,
                utf16_start: u16_pos,
                utf16_len: 4,
                scalar_start: sca_pos,
                scalar_len: 4,
                kind: SpanKind::EscapedByte,
            });
            raw_pos += 1;
            dec_pos += 4;
            u16_pos += 4;
            sca_pos += 4;
        }

        Ok(Self {
            encoding: DetectedEncoding::Unsupported,
            base_raw_offset: base_raw,
            base_decoded_offset: base_decoded,
            base_utf16_offset: base_utf16,
            base_scalar_offset: base_scalar,
            total_raw_bytes: raw_pos - base_raw,
            total_decoded_bytes: dec_pos - base_decoded,
            total_utf16_units: u16_pos - base_utf16,
            total_scalars: sca_pos - base_scalar,
            decoded_text,
            spans,
        })
    }

    pub const fn encoding(&self) -> DetectedEncoding {
        self.encoding
    }

    pub const fn base_raw_offset(&self) -> u64 {
        self.base_raw_offset
    }

    pub const fn base_decoded_offset(&self) -> u64 {
        self.base_decoded_offset
    }

    pub const fn base_utf16_offset(&self) -> u64 {
        self.base_utf16_offset
    }

    pub const fn base_scalar_offset(&self) -> u64 {
        self.base_scalar_offset
    }

    pub const fn total_raw_bytes(&self) -> ByteLength {
        ByteLength::new(self.total_raw_bytes)
    }

    pub const fn total_decoded_bytes(&self) -> u64 {
        self.total_decoded_bytes
    }

    pub const fn total_utf16_units(&self) -> u64 {
        self.total_utf16_units
    }

    pub const fn total_scalars(&self) -> u64 {
        self.total_scalars
    }

    pub fn decoded_text(&self) -> &str {
        &self.decoded_text
    }

    pub fn spans(&self) -> &[MappingSpan] {
        &self.spans
    }

    // =========================================================================
    // Domain Transformations
    // =========================================================================

    /// Convert a [`DecodedUtf8Offset`] to the exact [`ByteOffset`] in original capture.
    ///
    /// Rejects offsets that fall in the interior of a multi-byte decoded character
    /// with [`SourceError::InvalidUtf8Boundary`].
    pub fn decoded_utf8_to_byte_offset(
        &self,
        offset: DecodedUtf8Offset,
    ) -> Result<ByteOffset, SourceError> {
        let val = offset.get();
        if val < self.base_decoded_offset
            || val > self.base_decoded_offset + self.total_decoded_bytes
        {
            return Err(SourceError::RangeOutOfBounds);
        }
        if val == self.base_decoded_offset + self.total_decoded_bytes {
            return Ok(ByteOffset::new(self.base_raw_offset + self.total_raw_bytes));
        }

        let span = self
            .find_span_by_decoded(val)
            .ok_or(SourceError::RangeOutOfBounds)?;

        match span.kind {
            SpanKind::AsciiRun => {
                let delta = val - span.decoded_start;
                // In UTF-16 ASCII runs: 1 decoded char = 2 raw bytes
                if self.encoding.is_utf16() {
                    Ok(ByteOffset::new(span.raw_start + (delta * 2)))
                } else {
                    Ok(ByteOffset::new(span.raw_start + delta))
                }
            }
            SpanKind::BmpMultiByte | SpanKind::SurrogatePair | SpanKind::ReplacementMalformed => {
                if val == span.decoded_start {
                    Ok(ByteOffset::new(span.raw_start))
                } else if val == span.decoded_end() {
                    Ok(ByteOffset::new(span.raw_end()))
                } else {
                    Err(SourceError::InvalidUtf8Boundary)
                }
            }
            SpanKind::EscapedByte => {
                if (val - span.decoded_start).is_multiple_of(4) {
                    Ok(ByteOffset::new(span.raw_start))
                } else {
                    Err(SourceError::InvalidUtf8Boundary)
                }
            }
            SpanKind::BomHeader => Ok(ByteOffset::new(span.raw_end())),
        }
    }

    /// Convert an original [`ByteOffset`] to a [`DecodedUtf8Offset`].
    pub fn byte_to_decoded_utf8_offset(
        &self,
        offset: ByteOffset,
    ) -> Result<DecodedUtf8Offset, SourceError> {
        let val = offset.get();
        if val < self.base_raw_offset || val > self.base_raw_offset + self.total_raw_bytes {
            return Err(SourceError::RangeOutOfBounds);
        }
        if val == self.base_raw_offset + self.total_raw_bytes {
            return Ok(DecodedUtf8Offset::new(
                self.base_decoded_offset + self.total_decoded_bytes,
            ));
        }

        let span = self
            .find_span_by_raw(val)
            .ok_or(SourceError::RangeOutOfBounds)?;

        match span.kind {
            SpanKind::BomHeader => Ok(DecodedUtf8Offset::new(0)),
            SpanKind::AsciiRun => {
                let delta = val - span.raw_start;
                if self.encoding.is_utf16() {
                    if !delta.is_multiple_of(2) {
                        return Err(SourceError::InvalidUtf16);
                    }
                    Ok(DecodedUtf8Offset::new(span.decoded_start + (delta / 2)))
                } else {
                    Ok(DecodedUtf8Offset::new(span.decoded_start + delta))
                }
            }
            SpanKind::BmpMultiByte | SpanKind::SurrogatePair => {
                if val == span.raw_start {
                    Ok(DecodedUtf8Offset::new(span.decoded_start))
                } else if val == span.raw_end() {
                    Ok(DecodedUtf8Offset::new(span.decoded_end()))
                } else if self.encoding.is_utf16() {
                    Err(SourceError::InvalidUtf16)
                } else {
                    Err(SourceError::InvalidUtf8Boundary)
                }
            }
            SpanKind::ReplacementMalformed => {
                if val == span.raw_start {
                    Ok(DecodedUtf8Offset::new(span.decoded_start))
                } else if val == span.raw_end() {
                    Ok(DecodedUtf8Offset::new(span.decoded_end()))
                } else {
                    Err(SourceError::EncodingError)
                }
            }
            SpanKind::EscapedByte => {
                let delta = val - span.raw_start;
                Ok(DecodedUtf8Offset::new(span.decoded_start + (delta * 4)))
            }
        }
    }

    /// Convert a [`Utf16CodeUnitOffset`] (native Apple/Cocoa NSRange location) to the exact
    /// [`ByteOffset`] in original capture.
    ///
    /// Validates that the offset is not the native sentinel [`NATIVE_NOT_FOUND`] and does not
    /// fall into the interior of a surrogate pair with [`SourceError::InvalidUtf16`].
    pub fn utf16_to_byte_offset(
        &self,
        offset: Utf16CodeUnitOffset,
    ) -> Result<ByteOffset, SourceError> {
        let val = offset.get();
        if val == NATIVE_NOT_FOUND {
            return Err(SourceError::NativeSentinel);
        }
        if val < self.base_utf16_offset
            || val > self.base_utf16_offset + self.total_utf16_units
        {
            return Err(SourceError::RangeOutOfBounds);
        }
        if val == self.base_utf16_offset + self.total_utf16_units {
            return Ok(ByteOffset::new(self.base_raw_offset + self.total_raw_bytes));
        }

        let span = self
            .find_span_by_utf16(val)
            .ok_or(SourceError::RangeOutOfBounds)?;

        match span.kind {
            SpanKind::AsciiRun => {
                let delta = val - span.utf16_start;
                if self.encoding.is_utf16() {
                    Ok(ByteOffset::new(span.raw_start + (delta * 2)))
                } else {
                    Ok(ByteOffset::new(span.raw_start + delta))
                }
            }
            SpanKind::SurrogatePair => {
                if val == span.utf16_start {
                    Ok(ByteOffset::new(span.raw_start))
                } else if val == span.utf16_end() {
                    Ok(ByteOffset::new(span.raw_end()))
                } else {
                    // Interior of surrogate pair
                    Err(SourceError::InvalidUtf16)
                }
            }
            SpanKind::BmpMultiByte | SpanKind::ReplacementMalformed | SpanKind::EscapedByte => {
                if val == span.utf16_start {
                    Ok(ByteOffset::new(span.raw_start))
                } else if val == span.utf16_end() {
                    Ok(ByteOffset::new(span.raw_end()))
                } else {
                    Err(SourceError::InvalidUtf16)
                }
            }
            SpanKind::BomHeader => Ok(ByteOffset::new(span.raw_end())),
        }
    }

    /// Convert an original [`ByteOffset`] to [`Utf16CodeUnitOffset`].
    pub fn byte_to_utf16_offset(
        &self,
        offset: ByteOffset,
    ) -> Result<Utf16CodeUnitOffset, SourceError> {
        let val = offset.get();
        if val < self.base_raw_offset || val > self.base_raw_offset + self.total_raw_bytes {
            return Err(SourceError::RangeOutOfBounds);
        }
        if val == self.base_raw_offset + self.total_raw_bytes {
            return Ok(Utf16CodeUnitOffset::new(
                self.base_utf16_offset + self.total_utf16_units,
            ));
        }

        let span = self
            .find_span_by_raw(val)
            .ok_or(SourceError::RangeOutOfBounds)?;

        match span.kind {
            SpanKind::BomHeader => Ok(Utf16CodeUnitOffset::new(0)),
            SpanKind::AsciiRun => {
                let delta = val - span.raw_start;
                if self.encoding.is_utf16() {
                    if !delta.is_multiple_of(2) {
                        return Err(SourceError::InvalidUtf16);
                    }
                    Ok(Utf16CodeUnitOffset::new(span.utf16_start + (delta / 2)))
                } else {
                    Ok(Utf16CodeUnitOffset::new(span.utf16_start + delta))
                }
            }
            SpanKind::BmpMultiByte | SpanKind::SurrogatePair => {
                if val == span.raw_start {
                    Ok(Utf16CodeUnitOffset::new(span.utf16_start))
                } else if val == span.raw_end() {
                    Ok(Utf16CodeUnitOffset::new(span.utf16_end()))
                } else if self.encoding.is_utf16() {
                    Err(SourceError::InvalidUtf16)
                } else {
                    Err(SourceError::InvalidUtf8Boundary)
                }
            }
            SpanKind::ReplacementMalformed => {
                if val == span.raw_start {
                    Ok(Utf16CodeUnitOffset::new(span.utf16_start))
                } else if val == span.raw_end() {
                    Ok(Utf16CodeUnitOffset::new(span.utf16_end()))
                } else {
                    Err(SourceError::EncodingError)
                }
            }
            SpanKind::EscapedByte => {
                let delta = val - span.raw_start;
                Ok(Utf16CodeUnitOffset::new(span.utf16_start + (delta * 4)))
            }
        }
    }

    /// Convert a [`ScalarIndex`] to the exact [`ByteOffset`].
    pub fn scalar_to_byte_offset(&self, scalar: ScalarIndex) -> Result<ByteOffset, SourceError> {
        let val = scalar.get();
        if val < self.base_scalar_offset
            || val > self.base_scalar_offset + self.total_scalars
        {
            return Err(SourceError::RangeOutOfBounds);
        }
        if val == self.base_scalar_offset + self.total_scalars {
            return Ok(ByteOffset::new(self.base_raw_offset + self.total_raw_bytes));
        }

        let span = self
            .find_span_by_scalar(val)
            .ok_or(SourceError::RangeOutOfBounds)?;

        match span.kind {
            SpanKind::AsciiRun => {
                let delta = val - span.scalar_start;
                if self.encoding.is_utf16() {
                    Ok(ByteOffset::new(span.raw_start + (delta * 2)))
                } else {
                    Ok(ByteOffset::new(span.raw_start + delta))
                }
            }
            _ => {
                if val == span.scalar_start {
                    Ok(ByteOffset::new(span.raw_start))
                } else {
                    Ok(ByteOffset::new(span.raw_end()))
                }
            }
        }
    }

    /// Convert an original [`ByteOffset`] to a [`ScalarIndex`].
    pub fn byte_to_scalar_index(&self, offset: ByteOffset) -> Result<ScalarIndex, SourceError> {
        let val = offset.get();
        if val < self.base_raw_offset || val > self.base_raw_offset + self.total_raw_bytes {
            return Err(SourceError::RangeOutOfBounds);
        }
        if val == self.base_raw_offset + self.total_raw_bytes {
            return Ok(ScalarIndex::new(
                self.base_scalar_offset + self.total_scalars,
            ));
        }

        let span = self
            .find_span_by_raw(val)
            .ok_or(SourceError::RangeOutOfBounds)?;

        match span.kind {
            SpanKind::BomHeader => Ok(ScalarIndex::new(0)),
            SpanKind::AsciiRun => {
                let delta = val - span.raw_start;
                if self.encoding.is_utf16() {
                    if !delta.is_multiple_of(2) {
                        return Err(SourceError::InvalidUtf16);
                    }
                    Ok(ScalarIndex::new(span.scalar_start + (delta / 2)))
                } else {
                    Ok(ScalarIndex::new(span.scalar_start + delta))
                }
            }
            SpanKind::BmpMultiByte | SpanKind::SurrogatePair => {
                if val == span.raw_start {
                    Ok(ScalarIndex::new(span.scalar_start))
                } else if val == span.raw_end() {
                    Ok(ScalarIndex::new(span.scalar_end()))
                } else if self.encoding.is_utf16() {
                    Err(SourceError::InvalidUtf16)
                } else {
                    Err(SourceError::InvalidUtf8Boundary)
                }
            }
            SpanKind::ReplacementMalformed => {
                if val == span.raw_start {
                    Ok(ScalarIndex::new(span.scalar_start))
                } else if val == span.raw_end() {
                    Ok(ScalarIndex::new(span.scalar_end()))
                } else {
                    Err(SourceError::EncodingError)
                }
            }
            SpanKind::EscapedByte => {
                let delta = val - span.raw_start;
                Ok(ScalarIndex::new(span.scalar_start + (delta * 4)))
            }
        }
    }

    // =========================================================================
    // Range Translations & Search Correspondence
    // =========================================================================

    /// Map a search match range in decoded UTF-8 text to the exact [`ByteRange`] in original capture.
    pub fn decoded_utf8_range_to_byte_range(
        &self,
        range: DecodedUtf8Range,
    ) -> Result<ByteRange, SourceError> {
        let start = self.decoded_utf8_to_byte_offset(range.start())?;
        let end = self.decoded_utf8_to_byte_offset(range.end())?;
        ByteRange::new(start, end).map_err(Into::into)
    }

    /// Map an original [`ByteRange`] to a [`DecodedUtf8Range`].
    pub fn byte_range_to_decoded_utf8_range(
        &self,
        range: ByteRange,
    ) -> Result<DecodedUtf8Range, SourceError> {
        let start = self.byte_to_decoded_utf8_offset(range.start())?;
        let end = self.byte_to_decoded_utf8_offset(range.end())?;
        DecodedUtf8Range::new(start, end).map_err(Into::into)
    }

    /// Convert a native Apple NSRange (`location`, `length`) in UTF-16 code units to
    /// an exact [`ByteRange`] in the original source capture.
    ///
    /// Rejects `NATIVE_NOT_FOUND` sentinels, arithmetic overflow, and surrogate splits.
    pub fn native_nsrange_to_byte_range(
        &self,
        location: u64,
        length: u64,
    ) -> Result<ByteRange, SourceError> {
        if location == NATIVE_NOT_FOUND {
            return Err(SourceError::NativeSentinel);
        }
        let end_u16 = location
            .checked_add(length)
            .ok_or(SourceError::RangeOutOfBounds)?;

        let start_cu = Utf16CodeUnitOffset::from_native(location).map_err(Into::<SourceError>::into)?;
        let end_cu = Utf16CodeUnitOffset::from_native(end_u16).map_err(Into::<SourceError>::into)?;

        let start_byte = self.utf16_to_byte_offset(start_cu)?;
        let end_byte = self.utf16_to_byte_offset(end_cu)?;

        ByteRange::new(start_byte, end_byte).map_err(Into::into)
    }

    /// Convert an original [`ByteRange`] to native Apple NSRange (`location`, `length`) in UTF-16 code units.
    pub fn byte_range_to_native_nsrange(
        &self,
        range: ByteRange,
    ) -> Result<(u64, u64), SourceError> {
        let start_cu = self.byte_to_utf16_offset(range.start())?;
        let end_cu = self.byte_to_utf16_offset(range.end())?;
        let location = start_cu.to_native();
        let length = end_cu
            .get()
            .checked_sub(start_cu.get())
            .ok_or(SourceError::InvalidRange)?;
        Ok((location, length))
    }

    // =========================================================================
    // Explicit Copy Operations (§10.8)
    // =========================================================================

    /// Extract original captured bytes for exact source operations.
    pub fn copy_original_bytes<'a>(
        &self,
        raw_source: &'a [u8],
        range: ByteRange,
    ) -> Result<&'a [u8], SourceError> {
        let (start, end) = range.as_usize_bounds().map_err(Into::<SourceError>::into)?;
        raw_source.get(start..end).ok_or(SourceError::RangeOutOfBounds)
    }

    /// Extract decoded Unicode text for standard clipboard copy.
    pub fn copy_unicode_text(&self, range: DecodedUtf8Range) -> Result<String, SourceError> {
        let start = usize::try_from(range.start().get()).map_err(|_| SourceError::RangeOutOfBounds)?;
        let end = usize::try_from(range.end().get()).map_err(|_| SourceError::RangeOutOfBounds)?;
        if start > self.decoded_text.len() || end > self.decoded_text.len() || start > end {
            return Err(SourceError::RangeOutOfBounds);
        }
        if !self.decoded_text.is_char_boundary(start) || !self.decoded_text.is_char_boundary(end) {
            return Err(SourceError::InvalidUtf8Boundary);
        }
        Ok(self.decoded_text[start..end].to_string())
    }

    /// Extract labeled hex-escaped text for display copy of unsupported or raw byte views.
    pub fn copy_escaped_display(
        &self,
        raw_source: &[u8],
        range: ByteRange,
    ) -> Result<String, SourceError> {
        let bytes = self.copy_original_bytes(raw_source, range)?;
        let mut out = String::with_capacity(bytes.len() * 4);
        for &b in bytes {
            let _ = write!(out, "\\x{:02X}", b);
        }
        Ok(out)
    }

    // =========================================================================
    // Helper Binary Search Routines
    // =========================================================================

    fn find_span_by_decoded(&self, decoded_val: u64) -> Option<&MappingSpan> {
        if self.spans.is_empty() {
            return None;
        }
        let idx = self.spans.partition_point(|s| s.decoded_start <= decoded_val);
        if idx > 0 {
            let candidate = &self.spans[idx - 1];
            if decoded_val <= candidate.decoded_end() {
                return Some(candidate);
            }
        }
        if idx < self.spans.len() && self.spans[idx].decoded_start == decoded_val {
            return Some(&self.spans[idx]);
        }
        None
    }

    fn find_span_by_raw(&self, raw_val: u64) -> Option<&MappingSpan> {
        if self.spans.is_empty() {
            return None;
        }
        let idx = self.spans.partition_point(|s| s.raw_start <= raw_val);
        if idx > 0 {
            let candidate = &self.spans[idx - 1];
            if raw_val <= candidate.raw_end() {
                return Some(candidate);
            }
        }
        if idx < self.spans.len() && self.spans[idx].raw_start == raw_val {
            return Some(&self.spans[idx]);
        }
        None
    }

    fn find_span_by_utf16(&self, utf16_val: u64) -> Option<&MappingSpan> {
        if self.spans.is_empty() {
            return None;
        }
        let idx = self.spans.partition_point(|s| s.utf16_start <= utf16_val);
        if idx > 0 {
            let candidate = &self.spans[idx - 1];
            if utf16_val <= candidate.utf16_end() {
                return Some(candidate);
            }
        }
        if idx < self.spans.len() && self.spans[idx].utf16_start == utf16_val {
            return Some(&self.spans[idx]);
        }
        None
    }

    fn find_span_by_scalar(&self, scalar_val: u64) -> Option<&MappingSpan> {
        if self.spans.is_empty() {
            return None;
        }
        let idx = self.spans.partition_point(|s| s.scalar_start <= scalar_val);
        if idx > 0 {
            let candidate = &self.spans[idx - 1];
            if scalar_val <= candidate.scalar_end() {
                return Some(candidate);
            }
        }
        if idx < self.spans.len() && self.spans[idx].scalar_start == scalar_val {
            return Some(&self.spans[idx]);
        }
        None
    }
}

/// Stateful chunk decoder supporting streaming and split multi-byte characters
/// or CRLF sequences across chunk boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatefulChunkDecoder {
    encoding: DetectedEncoding,
    pending_bytes: [u8; 4],
    pending_len: usize,
    seen_bom: bool,
    current_raw_offset: u64,
    current_decoded_offset: u64,
    current_utf16_offset: u64,
    current_scalar_offset: u64,
}

impl StatefulChunkDecoder {
    pub fn new(encoding: DetectedEncoding) -> Self {
        Self {
            encoding,
            pending_bytes: [0u8; 4],
            pending_len: 0,
            seen_bom: false,
            current_raw_offset: 0,
            current_decoded_offset: 0,
            current_utf16_offset: 0,
            current_scalar_offset: 0,
        }
    }

    /// Feed a chunk of raw bytes, decoding complete characters and holding back partial
    /// boundary splits for the subsequent chunk.
    pub fn decode_chunk(&mut self, chunk: &[u8]) -> Result<CaptureEncodingMap, SourceError> {
        let mut buffer = Vec::with_capacity(self.pending_len + chunk.len());
        buffer.extend_from_slice(&self.pending_bytes[..self.pending_len]);
        buffer.extend_from_slice(chunk);
        self.pending_len = 0;

        if buffer.is_empty() {
            return CaptureEncodingMap::build_with_base_offset(
                &[],
                self.encoding,
                self.current_raw_offset,
                self.current_decoded_offset,
                self.current_utf16_offset,
                self.current_scalar_offset,
            );
        }

        let retain_len = if self.encoding.is_utf16() {
            buffer.len() % 2
        } else {
            Self::find_trailing_utf8_split(&buffer)
        };

        let process_len = buffer.len() - retain_len;
        if retain_len > 0 {
            self.pending_bytes[..retain_len].copy_from_slice(&buffer[process_len..]);
            self.pending_len = retain_len;
        }

        let map = CaptureEncodingMap::build_with_base_offset(
            &buffer[..process_len],
            self.encoding,
            self.current_raw_offset,
            self.current_decoded_offset,
            self.current_utf16_offset,
            self.current_scalar_offset,
        )?;

        self.current_raw_offset += map.total_raw_bytes;
        self.current_decoded_offset += map.total_decoded_bytes;
        self.current_utf16_offset += map.total_utf16_units;
        self.current_scalar_offset += map.total_scalars;

        Ok(map)
    }

    /// Flush any remaining partial bytes at EOF as malformed replacement characters.
    pub fn finish(&mut self) -> Result<Option<CaptureEncodingMap>, SourceError> {
        if self.pending_len == 0 {
            return Ok(None);
        }
        let pending = &self.pending_bytes[..self.pending_len];
        let map = CaptureEncodingMap::build_with_base_offset(
            pending,
            self.encoding,
            self.current_raw_offset,
            self.current_decoded_offset,
            self.current_utf16_offset,
            self.current_scalar_offset,
        )?;
        self.current_raw_offset += map.total_raw_bytes;
        self.current_decoded_offset += map.total_decoded_bytes;
        self.current_utf16_offset += map.total_utf16_units;
        self.current_scalar_offset += map.total_scalars;
        self.pending_len = 0;
        Ok(Some(map))
    }

    fn find_trailing_utf8_split(bytes: &[u8]) -> usize {
        let len = bytes.len();
        if len == 0 {
            return 0;
        }
        for i in 1..=4.min(len) {
            let b = bytes[len - i];
            if b < 0x80 {
                return 0;
            }
            if b >= 0xC0 {
                let needed = if (0xC2..=0xDF).contains(&b) {
                    2
                } else if (0xE0..=0xEF).contains(&b) {
                    3
                } else if (0xF0..=0xF4).contains(&b) {
                    4
                } else {
                    1
                };
                if i < needed {
                    return i;
                }
                return 0;
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_detection_bom_markers() {
        assert_eq!(
            detect_encoding(&[0xEF, 0xBB, 0xBF, b'a', b'b']),
            DetectedEncoding::Utf8 { has_bom: true }
        );
        assert_eq!(
            detect_encoding(&[0xFF, 0xFE, 0x61, 0x00]),
            DetectedEncoding::Utf16Le
        );
        assert_eq!(
            detect_encoding(&[0xFE, 0xFF, 0x00, 0x61]),
            DetectedEncoding::Utf16Be
        );
        assert_eq!(
            detect_encoding(b"Hello World"),
            DetectedEncoding::Utf8 { has_bom: false }
        );
    }

    #[test]
    fn utf8_plain_ascii_mapping() {
        let bytes = b"hello world";
        let map = CaptureEncodingMap::build(bytes).unwrap();
        assert_eq!(map.decoded_text(), "hello world");
        assert_eq!(map.total_raw_bytes().get(), 11);
        assert_eq!(map.total_decoded_bytes(), 11);
        assert_eq!(map.total_utf16_units(), 11);
        assert_eq!(map.total_scalars(), 11);

        assert_eq!(
            map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(6))
                .unwrap(),
            ByteOffset::new(6)
        );
        assert_eq!(
            map.byte_to_decoded_utf8_offset(ByteOffset::new(6)).unwrap(),
            DecodedUtf8Offset::new(6)
        );
    }

    #[test]
    fn utf8_with_bom_mapping() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hello");
        let map = CaptureEncodingMap::build(&bytes).unwrap();
        assert_eq!(map.decoded_text(), "hello");
        assert_eq!(map.total_raw_bytes().get(), 8);
        assert_eq!(map.total_decoded_bytes(), 5);

        // Decoded offset 0 maps to raw offset 3 (past BOM)
        assert_eq!(
            map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(0))
                .unwrap(),
            ByteOffset::new(3)
        );
        assert_eq!(
            map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(5))
                .unwrap(),
            ByteOffset::new(8)
        );
    }

    #[test]
    fn utf16le_with_bom_and_surrogate_pairs() {
        // UTF-16LE BOM [0xFF, 0xFE], 'A' [0x41, 0x00], Crab emoji U+1F980 [0x3E, 0xD8, 0x80, 0xDD], 'B' [0x42, 0x00]
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend_from_slice(&[0x41, 0x00]); // 'A' (2 bytes, 1 char)
        bytes.extend_from_slice(&[0x3E, 0xD8, 0x80, 0xDD]); // 🦀 (4 bytes, 2 code units, 1 char)
        bytes.extend_from_slice(&[0x42, 0x00]); // 'B' (2 bytes, 1 char)

        let map = CaptureEncodingMap::build(&bytes).unwrap();
        assert_eq!(map.decoded_text(), "A🦀B");
        assert_eq!(map.total_raw_bytes().get(), 10);
        // Decoded UTF-8: 'A' (1) + 🦀 (4) + 'B' (1) = 6 bytes
        assert_eq!(map.total_decoded_bytes(), 6);
        // UTF-16: 'A' (1) + 🦀 (2) + 'B' (1) = 4 code units
        assert_eq!(map.total_utf16_units(), 4);
        assert_eq!(map.total_scalars(), 3);

        // Map decoded 'B' (at decoded offset 5..6) to raw bytes
        let raw_b = map
            .decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(5))
            .unwrap();
        assert_eq!(raw_b, ByteOffset::new(8)); // 2(BOM) + 2(A) + 4(crab) = 8
        assert_eq!(&bytes[8..10], &[0x42, 0x00]);

        // Decoded hit search range for "🦀" (decoded 1..5)
        let dec_range = DecodedUtf8Range::new(
            DecodedUtf8Offset::new(1),
            DecodedUtf8Offset::new(5),
        )
        .unwrap();
        let raw_range = map.decoded_utf8_range_to_byte_range(dec_range).unwrap();
        assert_eq!(raw_range.start(), ByteOffset::new(4));
        assert_eq!(raw_range.end(), ByteOffset::new(8));
        assert_eq!(&bytes[4..8], &[0x3E, 0xD8, 0x80, 0xDD]);
    }

    #[test]
    fn utf16_interior_surrogate_boundary_rejected() {
        // UTF-16LE: Crab emoji U+1F980 [0x3E, 0xD8, 0x80, 0xDD]
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend_from_slice(&[0x3E, 0xD8, 0x80, 0xDD]);
        let map = CaptureEncodingMap::build(&bytes).unwrap();

        // Querying code unit 1 (second surrogate) must fail with InvalidUtf16
        let err = map.utf16_to_byte_offset(Utf16CodeUnitOffset::new(1));
        assert_eq!(err, Err(SourceError::InvalidUtf16));
    }

    #[test]
    fn native_sentinel_rejection() {
        let map = CaptureEncodingMap::build(b"abc").unwrap();
        let err = map.native_nsrange_to_byte_range(NATIVE_NOT_FOUND, 10);
        assert_eq!(err, Err(SourceError::NativeSentinel));
    }

    #[test]
    fn malformed_utf8_replacement_route() {
        // Malformed UTF-8: valid 'a', invalid 0xFF, valid 'b'
        let bytes = [b'a', 0xFF, b'b'];
        let map = CaptureEncodingMap::build(&bytes).unwrap();
        assert_eq!(map.decoded_text(), "a\u{FFFD}b");
        assert_eq!(map.total_raw_bytes().get(), 3);
        assert_eq!(map.total_decoded_bytes(), 5); // 1 + 3 + 1

        // Exact raw byte 1 maps to replacement char
        assert_eq!(
            map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(1))
                .unwrap(),
            ByteOffset::new(1)
        );
        // Interior of \u{FFFD} (decoded offset 2) is rejected
        assert_eq!(
            map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(2)),
            Err(SourceError::InvalidUtf8Boundary)
        );
    }
}

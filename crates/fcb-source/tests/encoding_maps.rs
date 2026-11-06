#![forbid(unsafe_code)]

//! Integration tests for encoding maps, domain translations, and capture decoders (FCB-012.B / fcb-37r.2).
//!
//! Verifies:
//! - UTF-8 and supported BOM UTF-16LE/BE decoder maps.
//! - Surrogate and malformed routes.
//! - Native/decoded/original domains remain distinct.
//! - Boundary-split CRLF, code points, and surrogates.
//! - Offsets exceeding 2^32 bytes without 32-bit truncation.
//! - Exact decoded hits map to original bytes without Apple framework dependencies.
//! - Intentional failing negative controls demonstrating oracle detection.

use fcb_core::{
    ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, ScalarIndex, Utf16CodeUnitOffset,
    NATIVE_NOT_FOUND,
};
use fcb_source::{
    detect_encoding, CaptureEncodingMap, DetectedEncoding, SourceError, StatefulChunkDecoder,
};

#[test]
fn utf8_bom_and_multibyte_mapping_exact_correspondence() {
    // UTF-8 with BOM: [0xEF, 0xBB, 0xBF]
    // Followed by: "fn main() {\n    let s = \"🦀\";\n}\n"
    let mut raw = vec![0xEF, 0xBB, 0xBF];
    let source_text = "fn main() {\n    let s = \"🦀\";\n}\n";
    raw.extend_from_slice(source_text.as_bytes());

    assert_eq!(detect_encoding(&raw), DetectedEncoding::Utf8 { has_bom: true });
    let map = CaptureEncodingMap::build(&raw).expect("build UTF-8 map");
    assert_eq!(map.encoding(), DetectedEncoding::Utf8 { has_bom: true });
    assert_eq!(map.decoded_text(), source_text);

    // BOM is raw bytes 0..3. Decoded text starts at raw byte 3.
    let dec_start = DecodedUtf8Offset::new(0);
    let raw_start = map.decoded_utf8_to_byte_offset(dec_start).unwrap();
    assert_eq!(raw_start, ByteOffset::new(3));

    // Search hit for crab emoji "🦀" in decoded text
    let crab_dec_start = source_text.find("🦀").expect("find crab in text") as u64;
    let crab_dec_end = crab_dec_start + "🦀".len() as u64;
    let dec_range = DecodedUtf8Range::new(
        DecodedUtf8Offset::new(crab_dec_start),
        DecodedUtf8Offset::new(crab_dec_end),
    )
    .unwrap();

    let raw_range = map.decoded_utf8_range_to_byte_range(dec_range).unwrap();
    let original_crab_bytes = map.copy_original_bytes(&raw, raw_range).unwrap();
    assert_eq!(original_crab_bytes, "🦀".as_bytes());
}

#[test]
fn utf16le_bom_and_surrogate_search_hit_to_original_bytes() {
    // UTF-16LE: BOM [0xFF, 0xFE]
    // Text: "fn main() { // 🦀\n}"
    let text = "fn main() { // 🦀\n}";
    let mut raw = vec![0xFF, 0xFE];
    for c in text.chars() {
        let mut buf = [0u16; 2];
        let encoded = c.encode_utf16(&mut buf);
        for &u in &*encoded {
            raw.extend_from_slice(&u.to_le_bytes());
        }
    }

    let map = CaptureEncodingMap::build(&raw).expect("build UTF-16LE map");
    assert_eq!(map.encoding(), DetectedEncoding::Utf16Le);
    assert_eq!(map.decoded_text(), text);

    // Search hit in decoded text for "🦀"
    let dec_pos = text.find("🦀").unwrap() as u64;
    let dec_len = "🦀".len() as u64; // 4 bytes in UTF-8
    let dec_range = DecodedUtf8Range::new(
        DecodedUtf8Offset::new(dec_pos),
        DecodedUtf8Offset::new(dec_pos + dec_len),
    )
    .unwrap();

    let raw_range = map.decoded_utf8_range_to_byte_range(dec_range).unwrap();
    assert_eq!(raw_range.len().get(), 4); // Crab in UTF-16 takes 4 bytes (2 code units)

    let raw_slice = map.copy_original_bytes(&raw, raw_range).unwrap();
    // Verify exact bytes in UTF-16LE: U+1F980 = high 0xD83E, low 0xDD80
    assert_eq!(raw_slice, &[0x3E, 0xD8, 0x80, 0xDD]);
}

#[test]
fn utf16be_bom_and_bmp_multibyte_mapping() {
    // UTF-16BE: BOM [0xFE, 0xFF]
    // Text: "Hello 世界" (World in Chinese: 世 U+4E16, 界 U+754C)
    let text = "Hello 世界";
    let mut raw = vec![0xFE, 0xFF];
    for c in text.chars() {
        let mut buf = [0u16; 2];
        let encoded = c.encode_utf16(&mut buf);
        for &u in &*encoded {
            raw.extend_from_slice(&u.to_be_bytes());
        }
    }

    let map = CaptureEncodingMap::build(&raw).expect("build UTF-16BE map");
    assert_eq!(map.encoding(), DetectedEncoding::Utf16Be);
    assert_eq!(map.decoded_text(), text);

    // Search for "世界" in decoded text
    let dec_pos = text.find("世界").unwrap() as u64;
    let dec_len = "世界".len() as u64; // 6 bytes in UTF-8
    let dec_range = DecodedUtf8Range::new(
        DecodedUtf8Offset::new(dec_pos),
        DecodedUtf8Offset::new(dec_pos + dec_len),
    )
    .unwrap();

    let raw_range = map.decoded_utf8_range_to_byte_range(dec_range).unwrap();
    // 2 chars in UTF-16BE = 4 bytes
    assert_eq!(raw_range.len().get(), 4);

    let raw_slice = map.copy_original_bytes(&raw, raw_range).unwrap();
    assert_eq!(raw_slice, &[0x4E, 0x16, 0x75, 0x4C]);
}

#[test]
fn large_offsets_exceeding_32_bits_without_truncation() {
    // Simulate a chunk situated past 5 GiB (5 * 1024^3 = 5_368_709_120)
    let base_raw = 5_368_709_120_u64;
    let base_dec = 5_368_709_120_u64;
    let base_u16 = 5_368_709_120_u64;
    let base_sca = 5_368_709_120_u64;

    let chunk_bytes = b"alpha beta gamma";
    let map = CaptureEncodingMap::build_with_base_offset(
        chunk_bytes,
        DetectedEncoding::Utf8 { has_bom: false },
        base_raw,
        base_dec,
        base_u16,
        base_sca,
    )
    .expect("build large offset map");

    let dec_offset = DecodedUtf8Offset::new(base_dec + 6);
    let raw_offset = map.decoded_utf8_to_byte_offset(dec_offset).unwrap();
    assert_eq!(raw_offset, ByteOffset::new(base_raw + 6));

    let u16_offset = Utf16CodeUnitOffset::new(base_u16 + 11);
    let raw_offset_u16 = map.utf16_to_byte_offset(u16_offset).unwrap();
    assert_eq!(raw_offset_u16, ByteOffset::new(base_raw + 11));
}

#[test]
fn native_nsrange_and_sentinel_handling() {
    let text = "Hello Apple NSRange";
    let map = CaptureEncodingMap::build(text.as_bytes()).unwrap();

    // NSRange { location: 6, length: 5 } -> "Apple"
    let range = map.native_nsrange_to_byte_range(6, 5).unwrap();
    assert_eq!(range.start(), ByteOffset::new(6));
    assert_eq!(range.end(), ByteOffset::new(11));

    let nsrange = map.byte_range_to_native_nsrange(range).unwrap();
    assert_eq!(nsrange, (6, 5));

    // Sentinel NATIVE_NOT_FOUND (u64::MAX) must be rejected with NativeSentinel
    let err = map.native_nsrange_to_byte_range(NATIVE_NOT_FOUND, 10);
    assert_eq!(err, Err(SourceError::NativeSentinel));
}

#[test]
fn distinct_copy_operations_contract() {
    let bytes = b"Line 1\r\nLine 2\r\n";
    let map = CaptureEncodingMap::build(bytes).unwrap();

    let byte_range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(6)).unwrap(); // "Line 1"
    let orig = map.copy_original_bytes(bytes, byte_range).unwrap();
    assert_eq!(orig, b"Line 1");

    let dec_range = DecodedUtf8Range::new(DecodedUtf8Offset::new(0), DecodedUtf8Offset::new(6)).unwrap();
    let text = map.copy_unicode_text(dec_range).unwrap();
    assert_eq!(text, "Line 1");

    let escaped = map.copy_escaped_display(bytes, byte_range).unwrap();
    assert_eq!(escaped, "\\x4C\\x69\\x6E\\x65\\x20\\x31");
}

#[test]
fn chunk_decoder_handles_boundary_split_crlf_and_multibyte() {
    let mut decoder = StatefulChunkDecoder::new(DetectedEncoding::Utf8 { has_bom: false });

    // Chunk 0 ends with CR (0x0D) and leading byte of 3-byte char U+4E2D (0xE4)
    let chunk0 = &[b'A', b'\r', 0xE4];
    let map0 = decoder.decode_chunk(chunk0).expect("chunk 0");
    // Only 'A' and '\r' were completed in chunk 0; 0xE4 was held back
    assert_eq!(map0.decoded_text(), "A\r");

    // Chunk 1 supplies continuation bytes (0xB8, 0xAD) and LF (0x0A)
    let chunk1 = &[0xB8, 0xAD, b'\n', b'B'];
    let map1 = decoder.decode_chunk(chunk1).expect("chunk 1");
    assert_eq!(map1.decoded_text(), "中\nB");

    let flush = decoder.finish().expect("finish");
    assert!(flush.is_none());
}

#[test]
fn negative_control_oracle_detects_all_boundary_defects() {
    // 1. Interior UTF-8 boundary defect
    let bytes = "🦀".as_bytes(); // 4-byte UTF-8
    let map = CaptureEncodingMap::build(bytes).unwrap();
    // Offset 1, 2, 3 are inside the multi-byte UTF-8 sequence
    assert_eq!(
        map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(1)),
        Err(SourceError::InvalidUtf8Boundary)
    );
    assert_eq!(
        map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(2)),
        Err(SourceError::InvalidUtf8Boundary)
    );

    // 2. Interior UTF-16 surrogate boundary defect
    let mut u16_bytes = vec![0xFF, 0xFE];
    u16_bytes.extend_from_slice(&[0x3E, 0xD8, 0x80, 0xDD]); // 🦀
    let map_u16 = CaptureEncodingMap::build(&u16_bytes).unwrap();
    assert_eq!(
        map_u16.utf16_to_byte_offset(Utf16CodeUnitOffset::new(1)),
        Err(SourceError::InvalidUtf16)
    );

    // 3. Out of bounds defect
    assert_eq!(
        map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(100)),
        Err(SourceError::RangeOutOfBounds)
    );

    // 4. Native sentinel defect
    assert_eq!(
        map.native_nsrange_to_byte_range(NATIVE_NOT_FOUND, 0),
        Err(SourceError::NativeSentinel)
    );
}

fn encode_units(text: &str, encoding: DetectedEncoding) -> Vec<u8> {
    if encoding.is_utf8() { return text.as_bytes().to_vec(); }
    text.encode_utf16().flat_map(|unit| if encoding == DetectedEncoding::Utf16Le {
        unit.to_le_bytes()
    } else { unit.to_be_bytes() }).collect()
}

fn assert_stream_equivalent(bytes: &[u8], encoding: DetectedEncoding, chunks: &[&[u8]]) {
    let whole = CaptureEncodingMap::build_with_encoding(bytes, encoding).unwrap();
    let mut decoder = StatefulChunkDecoder::new(encoding);
    let mut maps = Vec::new();
    for chunk in chunks { maps.push(decoder.decode_chunk(chunk).unwrap()); }
    if let Some(tail) = decoder.finish().unwrap() { maps.push(tail); }
    assert!(decoder.finish().unwrap().is_none());

    let mut text = String::new();
    let (mut raw, mut decoded, mut utf16, mut scalar) = (0, 0, 0, 0);
    for map in maps {
        assert_eq!(map.base_raw_offset(), raw);
        assert_eq!(map.base_decoded_offset(), decoded);
        assert_eq!(map.base_utf16_offset(), utf16);
        assert_eq!(map.base_scalar_offset(), scalar);
        for (local, ch) in map.decoded_text().char_indices() {
            let start = DecodedUtf8Offset::new(decoded + local as u64);
            let end = DecodedUtf8Offset::new(start.get() + ch.len_utf8() as u64);
            let range = DecodedUtf8Range::new(start, end).unwrap();
            let actual = map.decoded_utf8_range_to_byte_range(range).unwrap();
            let expected = whole.decoded_utf8_range_to_byte_range(range).unwrap();
            assert_eq!(actual, expected, "{encoding:?}: {ch:?} at {}", start.get());
            assert_eq!(map.byte_to_decoded_utf8_offset(actual.start()).unwrap(), start);
            assert_eq!(map.copy_unicode_text(range).unwrap(), ch.to_string());
        }
        text.push_str(map.decoded_text());
        raw += map.total_raw_bytes().get();
        decoded += map.total_decoded_bytes();
        utf16 += map.total_utf16_units();
        scalar += map.total_scalars();
    }
    assert_eq!(text, whole.decoded_text());
    assert_eq!(raw, bytes.len() as u64);
    assert_eq!(decoded, whole.total_decoded_bytes());
    assert_eq!(utf16, whole.total_utf16_units());
    assert_eq!(scalar, whole.total_scalars());
}

#[test]
fn every_stream_boundary_preserves_surrogates_interior_bom_and_copy_ranges() {
    for encoding in [DetectedEncoding::Utf8 { has_bom: false },
        DetectedEncoding::Utf8 { has_bom: true }, DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
        let body = "A🦀\r\n\u{FEFF}B🙂Z";
        let text = if encoding.bom_bytes_len() > 0 { format!("\u{FEFF}{body}") } else { body.to_owned() };
        let bytes = encode_units(&text, encoding);
        for split in 0..=bytes.len() {
            assert_stream_equivalent(&bytes, encoding, &[&bytes[..split], &bytes[split..]]);
        }
        for width in 1..=7 {
            let chunks: Vec<_> = bytes.chunks(width).collect();
            assert_stream_equivalent(&bytes, encoding, &chunks);
        }
    }
}

#[test]
fn utf16_high_surrogate_stays_pending_until_another_unit_or_real_eof() {
    for encoding in [DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
        let bytes = encode_units("🦀Z", encoding);
        for split in [2, 3] {
            let mut decoder = StatefulChunkDecoder::new(encoding);
            let prefix = decoder.decode_chunk(&bytes[..split]).unwrap();
            assert_eq!(prefix.decoded_text(), "");
            assert_eq!(prefix.total_raw_bytes().get(), 0);
            assert_eq!(decoder.decode_chunk(&bytes[split..]).unwrap().decoded_text(), "🦀Z");
            assert!(decoder.finish().unwrap().is_none());

            let mut truncated = StatefulChunkDecoder::new(encoding);
            truncated.decode_chunk(&bytes[..split]).unwrap();
            let tail = truncated.finish().unwrap().unwrap();
            assert_eq!(tail.total_raw_bytes().get(), split as u64);
            assert_eq!(tail.decoded_text(), if split == 2 { "\u{FFFD}" } else { "\u{FFFD}\u{FFFD}" });
            assert!(truncated.finish().unwrap().is_none());
        }
        let mut invalid = bytes[..2].to_vec();
        invalid.extend_from_slice(&encode_units("Z", encoding));
        assert_stream_equivalent(&invalid, encoding, &[&invalid[..2], &invalid[2..]]);
        assert_eq!(CaptureEncodingMap::build_with_encoding(&invalid, encoding).unwrap().decoded_text(), "\u{FFFD}Z");
    }
}

#[test]
fn nonzero_windows_keep_feff_and_absolute_text_domains() {
    let raw_base = (1_u64 << 32) + 64;
    let decoded_base = (1_u64 << 32) + 17;
    let utf16_base = (1_u64 << 32) + 9;
    let scalar_base = (1_u64 << 32) + 3;
    for encoding in [DetectedEncoding::Utf8 { has_bom: true }, DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
        let bytes = encode_units("\u{FEFF}🦀Z", encoding);
        let map = CaptureEncodingMap::build_with_base_offset(&bytes, encoding,
            raw_base, decoded_base, utf16_base, scalar_base).unwrap();
        assert_eq!(map.decoded_text(), "\u{FEFF}🦀Z");
        assert_eq!(map.byte_to_decoded_utf8_offset(ByteOffset::new(raw_base)).unwrap().get(), decoded_base);
        assert_eq!(map.byte_to_utf16_offset(ByteOffset::new(raw_base)).unwrap().get(), utf16_base);
        assert_eq!(map.byte_to_scalar_index(ByteOffset::new(raw_base)).unwrap().get(), scalar_base);
        let selected = DecodedUtf8Range::new(DecodedUtf8Offset::new(decoded_base),
            DecodedUtf8Offset::new(decoded_base + 7)).unwrap();
        assert_eq!(map.copy_unicode_text(selected).unwrap(), "\u{FEFF}🦀");
        let original = map.decoded_utf8_range_to_byte_range(selected).unwrap();
        assert_eq!(original.start().get(), raw_base);
        assert_eq!(original.len().get(), if encoding.is_utf16() { 6 } else { 7 });
        assert_eq!(map.native_nsrange_to_byte_range(utf16_base, 3).unwrap(), original);
        assert_eq!(map.scalar_to_byte_offset(ScalarIndex::new(scalar_base + 2)).unwrap(), original.end());
        let before = DecodedUtf8Range::new(DecodedUtf8Offset::new(0), DecodedUtf8Offset::new(1)).unwrap();
        assert_eq!(map.copy_unicode_text(before), Err(SourceError::RangeOutOfBounds));
        let interior = DecodedUtf8Range::new(DecodedUtf8Offset::new(decoded_base + 1),
            DecodedUtf8Offset::new(decoded_base + 3)).unwrap();
        assert_eq!(map.copy_unicode_text(interior), Err(SourceError::InvalidUtf8Boundary));
    }
}

#[test]
fn original_bom_maps_into_declared_decoded_domains() {
    for encoding in [DetectedEncoding::Utf8 { has_bom: true }, DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
        let bytes = encode_units("\u{FEFF}🦀Z", encoding);
        let map = CaptureEncodingMap::build_with_base_offset(&bytes, encoding, 0, 17, 23, 31).unwrap();
        assert_eq!(map.decoded_text(), "🦀Z");
        for raw in 0..encoding.bom_bytes_len() {
            assert_eq!(map.byte_to_decoded_utf8_offset(ByteOffset::new(raw)).unwrap().get(), 17);
            assert_eq!(map.byte_to_utf16_offset(ByteOffset::new(raw)).unwrap().get(), 23);
            assert_eq!(map.byte_to_scalar_index(ByteOffset::new(raw)).unwrap().get(), 31);
        }
        assert_eq!(map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(17)).unwrap().get(), encoding.bom_bytes_len());
    }
}

#[test]
fn every_map_domain_rejects_overflow_without_rejecting_the_exact_limit() {
    let samples: &[(&[u8], DetectedEncoding)] = &[
        (b"ab", DetectedEncoding::Utf8 { has_bom: false }),
        ("🦀".as_bytes(), DetectedEncoding::Utf8 { has_bom: false }),
        (&[0xff], DetectedEncoding::Utf8 { has_bom: false }),
        (&[0x3e, 0xd8, 0x80, 0xdd], DetectedEncoding::Utf16Le),
        (&[0xd8, 0x3e, 0xdd, 0x80], DetectedEncoding::Utf16Be),
        (&[0], DetectedEncoding::Utf16Le),
        (&[0xff], DetectedEncoding::Unsupported),
    ];
    for &(bytes, encoding) in samples {
        let original = CaptureEncodingMap::build_with_encoding(bytes, encoding).unwrap();
        let lengths = [original.total_raw_bytes().get(), original.total_decoded_bytes(),
            original.total_utf16_units(), original.total_scalars()];
        for domain in 0..4 {
            let mut bases = [0; 4];
            bases[domain] = u64::MAX - lengths[domain] + 1;
            assert_eq!(CaptureEncodingMap::build_with_base_offset(bytes, encoding,
                bases[0], bases[1], bases[2], bases[3]), Err(SourceError::RangeOutOfBounds),
                "{encoding:?}, domain {domain}");
            bases[domain] -= 1;
            let map = CaptureEncodingMap::build_with_base_offset(bytes, encoding,
                bases[0], bases[1], bases[2], bases[3]).unwrap();
            assert_eq!(map.decoded_text(), original.decoded_text());
            assert_eq!([map.total_raw_bytes().get(), map.total_decoded_bytes(),
                map.total_utf16_units(), map.total_scalars()], lengths);
        }
    }
}

#[test]
fn unsupported_bytes_are_escaped_without_utf8_carry() {
    let mut decoder = StatefulChunkDecoder::new(DetectedEncoding::Unsupported);
    let first = decoder.decode_chunk(&[0xf0, 0x9f]).unwrap();
    assert_eq!(first.decoded_text(), "\\xF0\\x9F");
    assert_eq!(first.total_raw_bytes().get(), 2);
    assert_eq!(decoder.decode_chunk(&[0xa6, 0x80]).unwrap().decoded_text(), "\\xA6\\x80");
    assert!(decoder.finish().unwrap().is_none());
}

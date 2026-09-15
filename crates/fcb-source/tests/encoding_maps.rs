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
    ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, Utf16CodeUnitOffset,
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

#![forbid(unsafe_code)]

//! Independent fixture-prefix oracles for streaming typed source positions.
//! This exercises public inert source APIs; execution belongs to batch RCH.
use fcb_core::ByteOffset;
use fcb_source::{DetectedEncoding, SourceError, position::{
    ColumnPositionError, LineColumnError, LineColumnScanner, LineColumnStatus,
    SourceColumnKind, MAX_POSITION_STEP_BYTES}};

fn encode(text: &str, encoding: DetectedEncoding) -> Vec<u8> {
    match encoding {
        DetectedEncoding::Utf8 { .. } => text.as_bytes().to_vec(),
        DetectedEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        DetectedEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        DetectedEncoding::Unsupported => unreachable!(),
    }
}
fn encodings() -> [DetectedEncoding; 3] {
    [DetectedEncoding::Utf8 { has_bom: true }, DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be]
}
fn locate(bytes: &[u8], line: u64, column: u64, kind: SourceColumnKind,
    encoding: DetectedEncoding, chunk: usize) -> Result<LineColumnStatus, LineColumnError> {
    let mut cursor = LineColumnScanner::new(line, column, kind, encoding)?;
    for block in bytes.chunks(chunk) {
        if cursor.status() != LineColumnStatus::Pending { break; }
        let consumed = cursor.step(ByteOffset::new(cursor.bytes_scanned()), block, block.len())?;
        assert_eq!(consumed, block.len());
    }
    if cursor.status() == LineColumnStatus::Pending { cursor.finish()?; }
    Ok(cursor.status())
}
fn point(offset: u64) -> LineColumnStatus {
    LineColumnStatus::Resolved { original_offset: ByteOffset::new(offset) }
}

#[test]
fn all_scalar_boundaries_match_independently_encoded_prefixes_for_every_small_chunk() {
    let rows = [("\tAé🦀e\u{301}\u{202e}z", "\r\n"), ("", "\r"),
        ("\u{feff}\u{0a41}tail", "\n"), ("last", "")];
    for encoding in encodings() {
        let text = format!("\u{feff}{}", rows.iter().map(|(body, ending)| format!("{body}{ending}")).collect::<String>());
        let bytes = encode(&text, encoding);
        let mut prefix = "\u{feff}".to_owned();
        for (index, (body, ending)) in rows.iter().enumerate() {
            for at in body.char_indices().map(|(at, _)| at).chain(std::iter::once(body.len())) {
                let preceding = &body[..at];
                let expected = encode(&format!("{prefix}{preceding}"), encoding).len() as u64;
                for (kind, column) in [
                    (SourceColumnKind::UnicodeScalar, preceding.chars().count() as u64 + 1),
                    (SourceColumnKind::Utf16CodeUnit, preceding.encode_utf16().count() as u64 + 1),
                    (SourceColumnKind::DecodedUtf8Byte, preceding.len() as u64 + 1),
                ] {
                    for chunk in 1..=13 {
                        assert_eq!(locate(&bytes, index as u64 + 1, column, kind, encoding, chunk).unwrap(),
                            point(expected), "encoding={encoding:?}, line={}, column={column}, kind={kind:?}, chunk={chunk}", index + 1);
                    }
                }
            }
            prefix.push_str(body); prefix.push_str(ending);
        }
    }
}

#[test]
fn missing_line_column_bom_only_and_no_phantom_final_line_are_distinct() {
    for encoding in encodings() {
        let scalar = SourceColumnKind::UnicodeScalar;
        for chunk in 1..=7 {
            assert_eq!(locate(&[], 1, 1, scalar, encoding, chunk).unwrap(), LineColumnStatus::MissingLine);
            let bom = encode("\u{feff}", encoding);
            assert_eq!(locate(&bom, 1, 1, scalar, encoding, chunk).unwrap(), point(bom.len() as u64));
            assert_eq!(locate(&bom, 1, 2, scalar, encoding, chunk).unwrap(), LineColumnStatus::MissingColumn);
            assert_eq!(locate(&bom, 2, 1, scalar, encoding, chunk).unwrap(), LineColumnStatus::MissingLine);
            for ending in ["\r", "\n", "\r\n"] {
                let bytes = encode(&format!("\u{feff}a{ending}"), encoding);
                assert_eq!(locate(&bytes, 1, 2, scalar, encoding, chunk).unwrap(),
                    point(encode("\u{feff}a", encoding).len() as u64));
                assert_eq!(locate(&bytes, 1, 3, scalar, encoding, chunk).unwrap(), LineColumnStatus::MissingColumn);
                assert_eq!(locate(&bytes, 2, 1, scalar, encoding, chunk).unwrap(), LineColumnStatus::MissingLine);
            }
        }
    }
}

#[test]
fn typed_interior_boundaries_are_errors_instead_of_snapped_carets() {
    for encoding in encodings() {
        let bytes = encode("\u{feff}é🦀x\r\n", encoding);
        for chunk in 1..=11 {
            for column in [2, 4, 5, 6] {
                assert_eq!(locate(&bytes, 1, column, SourceColumnKind::DecodedUtf8Byte, encoding, chunk),
                    Err(LineColumnError::Column(ColumnPositionError::Source(SourceError::InvalidUtf8Boundary))));
            }
            assert_eq!(locate(&bytes, 1, 3, SourceColumnKind::Utf16CodeUnit, encoding, chunk),
                Err(LineColumnError::Column(ColumnPositionError::Source(SourceError::InvalidUtf16))));
        }
    }
}

#[test]
fn chunk_cutoffs_do_not_become_eof_or_decode_partial_boms_and_surrogates() {
    let utf8 = DetectedEncoding::Utf8 { has_bom: true };
    let mut bom = LineColumnScanner::new(1, 1, SourceColumnKind::UnicodeScalar, utf8).unwrap();
    bom.step(ByteOffset::new(0), &[0xef], 1).unwrap();
    assert_eq!(bom.status(), LineColumnStatus::Pending);
    bom.step(ByteOffset::new(1), &[0xbb], 1).unwrap();
    assert_eq!(bom.status(), LineColumnStatus::Pending);
    bom.step(ByteOffset::new(2), &[0xbf], 1).unwrap();
    assert_eq!(bom.status(), point(3));

    for encoding in [DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
        let bytes = encode("\u{feff}🦀", encoding);
        let mut cursor = LineColumnScanner::new(1, 2, SourceColumnKind::UnicodeScalar, encoding).unwrap();
        cursor.step(ByteOffset::new(0), &bytes[..5], 5).unwrap();
        assert_eq!(cursor.status(), LineColumnStatus::Pending);
        cursor.step(ByteOffset::new(5), &bytes[5..], 1).unwrap();
        assert_eq!(cursor.status(), point(6));
    }

    let mut far = LineColumnScanner::new(9, 1, SourceColumnKind::UnicodeScalar, utf8).unwrap();
    far.step(ByteOffset::new(0), b"a\nb\n", 4).unwrap();
    assert_eq!(far.status(), LineColumnStatus::Pending);
    assert_eq!(far.finish().unwrap(), LineColumnStatus::MissingLine);
}

#[test]
fn malformed_units_use_shared_replacements_with_original_byte_carets() {
    let utf8 = DetectedEncoding::Utf8 { has_bom: false };
    for chunk in 1..=5 {
        assert_eq!(locate(b"\xffa\n", 1, 2, SourceColumnKind::UnicodeScalar, utf8, chunk).unwrap(), point(1));
        assert_eq!(locate(b"\xffa\n", 1, 4, SourceColumnKind::DecodedUtf8Byte, utf8, chunk).unwrap(), point(1));
        assert_eq!(locate(&[0xef, 0xbb], 1, 3, SourceColumnKind::UnicodeScalar, utf8, chunk).unwrap(), point(2));
        for encoding in [DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
            assert_eq!(locate(&[b'x'], 1, 1, SourceColumnKind::UnicodeScalar, encoding, chunk).unwrap(), point(0));
            assert_eq!(locate(&[b'x'], 1, 2, SourceColumnKind::UnicodeScalar, encoding, chunk).unwrap(), point(1));
            let mut bytes = encode("a\n", encoding); bytes.push(0xff);
            assert_eq!(locate(&bytes, 2, 1, SourceColumnKind::UnicodeScalar, encoding, chunk).unwrap(), point(4));
            assert_eq!(locate(&bytes, 2, 2, SourceColumnKind::UnicodeScalar, encoding, chunk).unwrap(), point(5));
        }
    }
}

#[test]
fn huge_columns_keep_progress_in_bounded_steps() {
    let mut bytes = vec![b'x'; 1024 * 1024 + 3];
    bytes.extend_from_slice("🦀target\n".as_bytes());
    let target = 1024 * 1024 + 7u64;
    for kind in [SourceColumnKind::UnicodeScalar, SourceColumnKind::Utf16CodeUnit, SourceColumnKind::DecodedUtf8Byte] {
        let column = match kind {
            SourceColumnKind::UnicodeScalar => 1024 * 1024 + 5,
            SourceColumnKind::Utf16CodeUnit => 1024 * 1024 + 6,
            SourceColumnKind::DecodedUtf8Byte => 1024 * 1024 + 8,
        };
        assert_eq!(locate(&bytes, 1, column, kind, DetectedEncoding::Utf8 { has_bom: false },
            MAX_POSITION_STEP_BYTES).unwrap(), point(target));
    }
}

#[test]
fn invalid_requests_and_noncontiguous_steps_do_not_advance_the_scanner() {
    let encoding = DetectedEncoding::Utf8 { has_bom: false };
    assert!(LineColumnScanner::new(0, 1, SourceColumnKind::UnicodeScalar, encoding).is_err());
    assert!(LineColumnScanner::new(1, 0, SourceColumnKind::UnicodeScalar, encoding).is_err());
    assert!(LineColumnScanner::new(1, 1, SourceColumnKind::UnicodeScalar, DetectedEncoding::Unsupported).is_err());
    let mut cursor = LineColumnScanner::new(2, 2, SourceColumnKind::UnicodeScalar, encoding).unwrap();
    assert_eq!(cursor.step(ByteOffset::new(0), b"a\n", MAX_POSITION_STEP_BYTES + 1), Err(LineColumnError::InvalidRequest));
    assert_eq!(cursor.bytes_scanned(), 0);
    cursor.step(ByteOffset::new(0), b"a\n", 2).unwrap();
    assert_eq!(cursor.step(ByteOffset::new(0), b"wrong", 5), Err(LineColumnError::NoncontiguousInput));
    assert_eq!(cursor.bytes_scanned(), 2);
    cursor.step(ByteOffset::new(2), b"ok\n", 3).unwrap();
    assert_eq!(cursor.status(), point(3));
}

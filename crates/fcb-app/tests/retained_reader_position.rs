#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real retained-host position requests. The oracle computes source offsets
//! from independent fixture prefixes, not the production line/column resolver.
//! Execution belongs to the repository's independent strict RCH build lane.
mod support;

use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, host::{HostResponse, reader::{ReaderColumnKind, ReaderSession,
    ReaderSessionError, MAX_READER_WINDOW_BYTES}}};
use support::{Json, parse};

fn source(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(9941).unwrap(), Path::new("position.txt"), bytes, || false).unwrap()
}
fn encode(text: &str, encoding: u8) -> Vec<u8> {
    match encoding {
        0 => text.as_bytes().to_vec(),
        1 => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        2 => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => unreachable!(),
    }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn file(bytes: &[u8]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-reader-position-{}-{now}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    let path = root.join("position.txt"); fs::write(&path, bytes).unwrap(); path
}

fn assert_position(response: &HostResponse, bytes: &[u8], line: u64, column: u64,
    kind: ReaderColumnKind, expected_offset: u64, page_bytes: usize) -> Json {
    assert_eq!(response.exit_code(), EXIT_OK);
    let page = json(response);
    assert_eq!(page.get("command").text(), "position");
    assert_eq!(page.get("selection"), &Json::Null, "a position carries no query selection authority");
    assert_eq!(page.get("first_physical_line"), &Json::Null, "the surrounding page has an independent line base");
    assert_eq!(page.get("additional_source_bytes_read").number(), 0);
    assert_eq!(page.get("captured_bytes").number(), bytes.len() as u64);
    assert!(!page.get("range_limited").flag());
    let position = page.get("position");
    assert_eq!(position.get("line").number(), line);
    assert_eq!(position.get("column").number(), column);
    assert_eq!(position.get("column_kind").text(), kind.code());
    assert_eq!(position.get("original_offset").number(), expected_offset);
    let before = (page_bytes as u64 - 4) / 2;
    let requested_start = expected_offset.saturating_sub(before);
    let requested_end = (requested_start + page_bytes as u64).min(bytes.len() as u64);
    assert_eq!(page.get("requested_original_range").get("start").number(), requested_start);
    assert_eq!(page.get("requested_original_range").get("end").number(), requested_end);
    let start = page.get("visible_range").get("start").number();
    let end = page.get("visible_range").get("end").number();
    assert!(start <= expected_offset && expected_offset <= end);
    assert!(end - start <= page_bytes as u64 + 8);
    assert_eq!(page.get("original_hex").text(), hex(&bytes[start as usize..end as usize]));
    let text = page.get("text").text();
    let decoded = position.get("window_utf8_offset").number() as usize;
    assert!(text.is_char_boundary(decoded));
    if !page.get("has_replacements").flag() {
        let prefix = &text[..decoded];
        let width = match page.get("encoding").text() {
            "utf8" => prefix.len(),
            "utf16le" | "utf16be" => prefix.encode_utf16().count() * 2,
            other => panic!("unexpected encoding {other}"),
        };
        assert_eq!(start + width as u64, expected_offset, "independent page-local caret oracle");
    }
    page
}

#[test]
fn typed_columns_distinguish_scalar_utf16_and_decoded_utf8_coordinates() {
    let prefix = "\u{feff}first\r\n";
    let line = "\tAé🦀e\u{301}\u{202e}z";
    for encoding in 0..3 {
        let bytes = encode(&format!("{prefix}{line}\r\nlast"), encoding);
        let mut reader = source(&bytes);
        let line_start = encode(prefix, encoding).len() as u64;
        for (scalar, offset) in line.char_indices().map(|(offset, _)| offset)
            .chain(std::iter::once(line.len())).enumerate() {
            let preceding = &line[..offset];
            let expected = line_start + encode(preceding, encoding).len() as u64;
            for (kind, column) in [
                (ReaderColumnKind::UnicodeScalar, scalar as u64 + 1),
                (ReaderColumnKind::Utf16CodeUnit, preceding.encode_utf16().count() as u64 + 1),
                (ReaderColumnKind::DecodedUtf8Byte, offset as u64 + 1),
            ] {
                let response = reader.read_position(2, column, kind, 96, || false).unwrap();
                assert_position(&response, &bytes, 2, column, kind, expected, 96);
            }
        }
        assert_eq!(reader.capture().bytes(), bytes);
    }
}

#[test]
fn physical_lines_exclude_terminators_and_never_create_a_phantom_eof_row() {
    let rows = [("a", "\r\n"), ("", "\r"), ("x", "\n"), ("y", "\n")];
    for encoding in 0..3 {
        let text = format!("\u{feff}{}", rows.iter().map(|(text, ending)| format!("{text}{ending}")).collect::<String>());
        let bytes = encode(&text, encoding);
        let mut reader = source(&bytes);
        let mut preceding = "\u{feff}".to_owned();
        for (index, (content, ending)) in rows.iter().enumerate() {
            let line = index as u64 + 1;
            let column = content.chars().count() as u64 + 1;
            let expected = encode(&format!("{preceding}{content}"), encoding).len() as u64;
            let response = reader.read_position(line, column, ReaderColumnKind::UnicodeScalar, 64, || false).unwrap();
            assert_position(&response, &bytes, line, column, ReaderColumnKind::UnicodeScalar, expected, 64);
            assert_eq!(reader.read_position(line, column + 1, ReaderColumnKind::UnicodeScalar, 64, || false).err(),
                Some(ReaderSessionError::MissingColumn));
            preceding.push_str(content); preceding.push_str(ending);
        }
        assert_eq!(reader.read_position(5, 1, ReaderColumnKind::UnicodeScalar, 64, || false).err(),
            Some(ReaderSessionError::MissingLine));
    }
    assert_eq!(source(b"").read_position(1, 1, ReaderColumnKind::UnicodeScalar, 4, || false).err(),
        Some(ReaderSessionError::MissingLine));
    let mut newline = source(b"\n");
    assert_position(&newline.read_position(1, 1, ReaderColumnKind::UnicodeScalar, 4, || false).unwrap(),
        b"\n", 1, 1, ReaderColumnKind::UnicodeScalar, 0, 4);
    assert_eq!(newline.read_position(2, 1, ReaderColumnKind::UnicodeScalar, 4, || false).err(),
        Some(ReaderSessionError::MissingLine));
}

#[test]
fn bom_is_not_a_column_and_interior_feff_remains_content() {
    for encoding in 0..3 {
        let bytes = encode("\u{feff}", encoding);
        let mut reader = source(&bytes);
        let response = reader.read_position(1, 1, ReaderColumnKind::UnicodeScalar, 4, || false).unwrap();
        let page = assert_position(&response, &bytes, 1, 1, ReaderColumnKind::UnicodeScalar, bytes.len() as u64, 4);
        assert_eq!(page.get("text").text(), "");
        assert_eq!(page.get("position").get("window_utf8_offset").number(), 0);
        assert_eq!(reader.read_position(1, 2, ReaderColumnKind::UnicodeScalar, 4, || false).err(),
            Some(ReaderSessionError::MissingColumn));

        let bytes = encode("\u{feff}a\n\u{feff}b", encoding);
        let mut reader = source(&bytes);
        let expected = encode("\u{feff}a\n\u{feff}", encoding).len() as u64;
        assert_position(&reader.read_position(2, 2, ReaderColumnKind::UnicodeScalar, 64, || false).unwrap(),
            &bytes, 2, 2, ReaderColumnKind::UnicodeScalar, expected, 64);
    }
}

#[test]
fn invalid_interior_code_units_are_refused_without_snapping() {
    for encoding in 0..3 {
        let bytes = encode("\u{feff}é🦀x\n", encoding);
        let mut reader = source(&bytes);
        // Byte 1 is inside é; decoded bytes 3, 4, 5 are inside the astral scalar.
        for column in [2, 4, 5, 6] {
            assert!(reader.read_position(1, column, ReaderColumnKind::DecodedUtf8Byte, 64, || false).is_err());
        }
        assert!(reader.read_position(1, 3, ReaderColumnKind::Utf16CodeUnit, 64, || false).is_err());
        let expected = encode("\u{feff}é🦀", encoding).len() as u64;
        assert_position(&reader.read_position(1, 3, ReaderColumnKind::UnicodeScalar, 64, || false).unwrap(),
            &bytes, 1, 3, ReaderColumnKind::UnicodeScalar, expected, 64);
        assert_eq!(reader.capture().bytes(), bytes);
    }
}

#[test]
fn minimum_pages_cover_astral_targets_and_allow_real_eof_carets() {
    for encoding in 0..3 {
        let bytes = encode("\u{feff}🦀", encoding);
        let mut reader = source(&bytes);
        let start = encode("\u{feff}", encoding).len() as u64;
        let first = reader.read_position(1, 1, ReaderColumnKind::UnicodeScalar, 4, || false).unwrap();
        let page = assert_position(&first, &bytes, 1, 1, ReaderColumnKind::UnicodeScalar, start, 4);
        assert_eq!(page.get("text").text(), "🦀");
        let eof = reader.read_position(1, 2, ReaderColumnKind::UnicodeScalar, 4, || false).unwrap();
        let page = assert_position(&eof, &bytes, 1, 2, ReaderColumnKind::UnicodeScalar, bytes.len() as u64, 4);
        assert_eq!(page.get("text").text(), "");
        assert_eq!(page.get("position").get("window_utf8_offset").number(), 0);
    }
}

#[test]
fn giant_line_far_columns_return_small_exact_windows_in_every_encoding() {
    let prefix = "\u{feff}first\r\n";
    let long = "x".repeat(256 * 1024);
    for encoding in 0..3 {
        let bytes = encode(&format!("{prefix}{long}🦀target\r\nlast"), encoding);
        let mut reader = source(&bytes);
        let column = long.len() as u64 + 2;
        let expected = encode(&format!("{prefix}{long}🦀"), encoding).len() as u64;
        let response = reader.read_position(2, column, ReaderColumnKind::UnicodeScalar, 16, || false).unwrap();
        let page = assert_position(&response, &bytes, 2, column, ReaderColumnKind::UnicodeScalar, expected, 16);
        assert!(page.get("visible_range").get("start").number() > 64 * 1024);
        assert!(response.as_str().len() < 2048, "a far column must not return the preceding giant line");
        assert_eq!(reader.capture().bytes(), bytes);
    }
}

#[test]
fn decoder_carry_preserves_scalar_positions_across_chunk_edges() {
    for encoding in 0..3 {
        // Place a scalar across the resolver's 4096-byte decoder boundary.
        let content = "x".repeat(if encoding == 0 { 4092 } else { 2046 });
        let prefix = format!("\u{feff}{content}");
        let bytes = encode(&format!("{prefix}🦀z"), encoding);
        let mut reader = source(&bytes);
        for after in [false, true] {
            let column = content.len() as u64 + 1 + u64::from(after);
            let expected = encode(&format!("{prefix}{}", if after { "🦀" } else { "" }), encoding).len() as u64;
            assert_position(&reader.read_position(1, column, ReaderColumnKind::UnicodeScalar, 16, || false).unwrap(),
                &bytes, 1, column, ReaderColumnKind::UnicodeScalar, expected, 16);
        }
    }
}

#[test]
fn line_checkpoint_carry_and_repeated_far_jumps_keep_exact_positions() {
    let mut bytes = vec![b'x'; 65_535];
    bytes.extend_from_slice(b"\r\nz\r\n");
    let mut reader = source(&bytes);
    for _ in 0..2 {
        assert_position(&reader.read_position(2, 1, ReaderColumnKind::UnicodeScalar, 16, || false).unwrap(),
            &bytes, 2, 1, ReaderColumnKind::UnicodeScalar, 65_537, 16);
    }
    let bytes = "pad\r\n".repeat(200_000).into_bytes();
    let mut reader = source(&bytes);
    let mut cold_checks = 0;
    let cold = reader.read_position(190_000, 2, ReaderColumnKind::UnicodeScalar, 64,
        || { cold_checks += 1; false }).unwrap();
    let mut warm_checks = 0;
    let warm = reader.read_position(190_000, 2, ReaderColumnKind::UnicodeScalar, 64,
        || { warm_checks += 1; false }).unwrap();
    assert_eq!(cold.as_str(), warm.as_str());
    assert!(cold_checks > warm_checks + 8, "cold={cold_checks}, warm={warm_checks}");
    assert_position(&warm, &bytes, 190_000, 2, ReaderColumnKind::UnicodeScalar, 189_999 * 5 + 1, 64);
}

#[test]
fn malformed_source_uses_declared_replacement_columns_and_exact_original_offsets() {
    let bytes = b"\xffa\r\nlast";
    let mut reader = source(bytes);
    for (kind, column) in [(ReaderColumnKind::UnicodeScalar, 2),
        (ReaderColumnKind::Utf16CodeUnit, 2), (ReaderColumnKind::DecodedUtf8Byte, 4)] {
        let response = reader.read_position(1, column, kind, 64, || false).unwrap();
        let page = assert_position(&response, bytes, 1, column, kind, 1, 64);
        assert!(page.get("has_replacements").flag());
        assert_eq!(page.get("position").get("window_utf8_offset").number(), 3);
    }
    for column in [2, 3] {
        assert!(reader.read_position(1, column, ReaderColumnKind::DecodedUtf8Byte, 64, || false).is_err());
    }
    let odd_utf16 = [0xff, 0xfe, b'a', 0, 0xff];
    let mut reader = source(&odd_utf16);
    assert_position(&reader.read_position(1, 3, ReaderColumnKind::UnicodeScalar, 64, || false).unwrap(),
        &odd_utf16, 1, 3, ReaderColumnKind::UnicodeScalar, 5, 64);
}

#[test]
fn invalid_limits_missing_coordinates_and_cancellation_preserve_accepted_queries() {
    let bytes = format!("old\n{}", "x".repeat(256 * 1024)).into_bytes();
    let mut reader = source(&bytes);
    reader.search(7, "old", 1, bytes.len() as u64, || false).unwrap();
    for (line, column, page_bytes) in [(0, 1, 64), (1, 0, 64), (u64::MAX, 1, 64),
        (1, u64::MAX, 64), (1, 1, 3), (1, 1, MAX_READER_WINDOW_BYTES + 1)] {
        assert!(reader.read_position(line, column, ReaderColumnKind::UnicodeScalar, page_bytes, || false).is_err());
    }
    assert_eq!(reader.read_position(2, 1, ReaderColumnKind::UnicodeScalar, 64, || true).err(),
        Some(ReaderSessionError::Canceled));
    let mut polls = 0;
    assert_eq!(reader.read_position(2, 200_000, ReaderColumnKind::UnicodeScalar, 64,
        || { polls += 1; polls == 8 }).err(), Some(ReaderSessionError::Canceled));
    assert_eq!(reader.accepted_generation(), Some(7));
    assert_eq!(reader.capture().bytes(), bytes);
    assert_eq!(json(&reader.copy_hit(7, 0, || false).unwrap()).get("original_hex").text(), "6f6c64");
    assert_position(&reader.read_position(2, 200_000, ReaderColumnKind::UnicodeScalar, 64, || false).unwrap(),
        &bytes, 2, 200_000, ReaderColumnKind::UnicodeScalar, 4 + 199_999, 64);
}

#[test]
fn canceled_far_line_lookup_can_resume_from_retained_checkpoints() {
    let bytes = "line\r\n".repeat(200_000).into_bytes();
    let mut reader = source(&bytes);
    let mut polls = 0;
    assert_eq!(reader.read_position(190_000, 1, ReaderColumnKind::UnicodeScalar, 64,
        || { polls += 1; polls == 8 }).err(), Some(ReaderSessionError::Canceled));
    assert_position(&reader.read_position(190_000, 1, ReaderColumnKind::UnicodeScalar, 64, || false).unwrap(),
        &bytes, 190_000, 1, ReaderColumnKind::UnicodeScalar, 189_999 * 6, 64);
}

#[test]
fn navigation_and_copy_use_retained_bytes_after_disk_replacement() {
    let bytes = "\u{feff}one\r\né🦀target\nlast".as_bytes();
    let path = file(bytes);
    let mut reader = ReaderSession::open(ArenaOwnerId::new(9942).unwrap(), &path, 4096, || false).unwrap();
    fs::write(&path, b"different current source").unwrap();
    let expected = "\u{feff}one\r\né🦀".len() as u64;
    let response = reader.read_position(2, 3, ReaderColumnKind::UnicodeScalar, 64, || false).unwrap();
    assert_position(&response, bytes, 2, 3, ReaderColumnKind::UnicodeScalar, expected, 64);
    assert_eq!(reader.capture().bytes(), bytes);
    let copied = reader.copy_range(expected, expected + 6, || false).unwrap();
    assert_eq!(json(&copied).get("original_hex").text(), "746172676574");
}

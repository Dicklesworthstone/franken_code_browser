#![forbid(unsafe_code)]
#![cfg(unix)]

//! Exact typed position navigation through the public native-host session on real opened
//! files. Expected ranges come from fixture construction, independent of the
//! production scanner. Execution belongs to the strict-RCH verification lane.
mod support;

use std::{fs::{self, OpenOptions}, os::unix::fs::FileExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{AppError, EXIT_PARTIAL, host::{HostResponse, observed_reader::{
    ObservedReaderEncoding, ObservedReaderError, ObservedReaderSession,
    MAX_OBSERVED_READER_LINE_SCAN_BYTES,
    MAX_OBSERVED_READER_WINDOW_BYTES}, reader::ReaderColumnKind}};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-observed-position-{}-{now}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("source"), bytes).unwrap();
        Self(root)
    }
    fn file(&self) -> fs::File {
        OpenOptions::new().read(true).write(true).open(self.0.join("source")).unwrap()
    }
    fn reader(&self) -> ObservedReaderSession {
        static NEXT: AtomicU64 = AtomicU64::new(93_000);
        ObservedReaderSession::open(ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap(),
            &self.0, b"source", || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(value: &Json) -> (u64, u64) { (value.get("start").number(), value.get("end").number()) }
fn ids(value: &Json) -> Vec<u64> {
    value.get("retained_observations").array().iter().map(Json::number).collect()
}
fn utf16(text: &str, encoding: ObservedReaderEncoding) -> Vec<u8> {
    text.encode_utf16().flat_map(|unit| if encoding == ObservedReaderEncoding::Utf16Le {
        unit.to_le_bytes()
    } else { unit.to_be_bytes() }).collect()
}


fn encode(text: &str, encoding: ObservedReaderEncoding) -> Vec<u8> {
    if encoding == ObservedReaderEncoding::Utf8 { text.as_bytes().to_vec() } else { utf16(text, encoding) }
}
fn encodings() -> [ObservedReaderEncoding; 3] {
    [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be]
}
fn assert_position(response: &HostResponse, bytes: &[u8], line: u64, column: u64,
    kind: ReaderColumnKind, expected: u64, max_bytes: usize) -> Json {
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let page = json(response);
    assert_eq!(page.get("command").text(), "position");
    assert_eq!(page.get("consistency").text(), "host-supplied-sequence");
    assert!(!page.get("whole_file_immutable").flag());
    assert!(!page.get("whole_file_observed").flag());
    let receipt = page.get("position_navigation");
    assert_eq!(receipt.get("metadata_consistency").text(), "unchanged-metadata-not-atomic");
    assert_eq!(receipt.get("line").number(), line);
    assert_eq!(receipt.get("column").number(), column);
    assert_eq!(receipt.get("column_kind").text(), kind.code());
    assert_eq!(receipt.get("original_offset").number(), expected);
    assert_eq!(receipt.get("max_selected_bytes").number(), max_bytes as u64);
    assert_eq!(receipt.get("payload_bytes_read"), page.get("observation_source_bytes_read"));
    assert_eq!(receipt.get("read_calls"), page.get("observation_read_calls"));
    let scanned = receipt.get("scanned_input_bytes").number();
    let payload = receipt.get("payload_bytes_read").number();
    assert!(expected <= scanned && scanned <= payload);
    assert!(payload <= receipt.get("max_scan_bytes").number());
    assert!(page.get("captured_bytes").number() <= max_bytes as u64 + 8);
    let (start, end) = bounds(page.get("visible_range"));
    assert!(start <= expected && expected <= end);
    assert!(end - start <= max_bytes as u64);
    assert!(bounds(page.get("captured_range")).1 <= payload);
    assert_eq!(page.get("original_hex").text(), hex(&bytes[start as usize..end as usize]));
    let decoded = receipt.get("window_utf8_offset").number() as usize;
    let text = page.get("text").text();
    assert!(text.is_char_boundary(decoded));
    if !page.get("has_replacements").flag() {
        let width = match page.get("encoding").text() {
            "utf8" => text[..decoded].len(),
            "utf16le" | "utf16be" => text[..decoded].encode_utf16().count() * 2,
            _ => unreachable!(),
        };
        assert_eq!(start + width as u64, expected);
    }
    if let Json::Object(fields) = &page {
        assert!(!fields.contains_key("line_navigation"));
        assert!(!fields.contains_key("find_hit"));
    }
    page
}

#[test]
fn typed_columns_and_content_end_carets_match_exact_source_prefixes() {
    let prefix = "\u{feff}first\r\n";
    let body = "\tAé🦀e\u{301}\u{202e}z";
    for encoding in encodings() {
        let bytes = encode(&format!("{prefix}{body}\r\nlast"), encoding);
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        for at in body.char_indices().map(|(at, _)| at).chain(std::iter::once(body.len())) {
            let preceding = &body[..at];
            let expected = encode(&format!("{prefix}{preceding}"), encoding).len() as u64;
            for (kind, column) in [
                (ReaderColumnKind::UnicodeScalar, preceding.chars().count() as u64 + 1),
                (ReaderColumnKind::Utf16CodeUnit, preceding.encode_utf16().count() as u64 + 1),
                (ReaderColumnKind::DecodedUtf8Byte, preceding.len() as u64 + 1),
            ] {
                let response = reader.read_position(2, column, kind, 64, 1024, encoding, || false).unwrap();
                assert_position(&response, &bytes, 2, column, kind, expected, 64);
            }
        }
    }
}

#[test]
fn giant_line_beyond_complete_capture_limit_keeps_only_small_same_pass_context() {
    let fixture = Fixture::new(b"");
    let file = fixture.file();
    let prefix = 32 * 1024 * 1024u64 + 3;
    let length = prefix + 64;
    file.set_len(length).unwrap();
    let tail = "🦀target\r\nlast".as_bytes();
    assert_eq!(file.write_at(tail, prefix).unwrap(), tail.len());
    let mut reader = fixture.reader();
    let response = reader.read_position(1, prefix + 2, ReaderColumnKind::UnicodeScalar, 16,
        64 * 1024 * 1024, ObservedReaderEncoding::Utf8, || false).unwrap();
    let page = json(&response);
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    assert!(response.as_str().len() < 4096);
    assert_eq!(page.get("position_navigation").get("original_offset").number(), prefix + 4);
    assert_eq!(page.get("position_navigation").get("window_utf8_offset").number(), 6);
    assert!(page.get("text").text().contains("🦀target"));
    assert_eq!(page.get("first_physical_line").number(), 1);
    assert!(page.get("captured_bytes").number() <= 24);
    assert_eq!(page.get("observed_length").number(), length);
    let receipt = page.get("position_navigation");
    assert!(receipt.get("scanned_input_bytes").number() >= prefix + 4);
    assert!(receipt.get("scanned_input_bytes").number() <= receipt.get("payload_bytes_read").number());
    let info = json(&reader.info(|| false).unwrap());
    assert_eq!(info.get("payload_bytes_read"), receipt.get("payload_bytes_read"));
    assert_eq!(info.get("read_calls"), receipt.get("read_calls"));
    assert_eq!(info.get("retained_payload_bytes"), page.get("captured_bytes"));
    assert_eq!(ids(&info), [page.get("observation").number()]);
}

#[test]
fn columns_far_into_utf16_lines_and_decoder_chunk_edges_keep_exact_small_pages() {
    for encoding in encodings() {
        for length in [16_383usize, 16_384, 16_386, 16_387, 256 * 1024 + 1] {
            let prefix = format!("\u{feff}first\r\n{}", "x".repeat(length));
            let bytes = encode(&format!("{prefix}🦀target\r\nlast"), encoding);
            let fixture = Fixture::new(&bytes);
            let mut reader = fixture.reader();
            for (after, page_bytes) in [(false, 4), (true, 4), (true, 8)] {
                let column = length as u64 + 1 + u64::from(after);
                let expected = encode(&format!("{prefix}{}", if after { "🦀" } else { "" }), encoding).len() as u64;
                let response = reader.read_position(2, column, ReaderColumnKind::UnicodeScalar, page_bytes,
                    2 * 1024 * 1024, encoding, || false).unwrap();
                let page = assert_position(&response, &bytes, 2, column, ReaderColumnKind::UnicodeScalar, expected, page_bytes);
                assert_eq!(page.get("first_physical_line").number(), 2);
            }
        }
    }
}

#[test]
fn physical_line_evidence_is_known_mid_line_and_unknown_across_unindexed_prefixes() {
    let bytes = b"header\nprior\nchosen tail\n";
    let fixture = Fixture::new(bytes);
    let mut reader = fixture.reader();
    let mid = json(&reader.read_position(3, 5, ReaderColumnKind::UnicodeScalar, 8, 1024,
        ObservedReaderEncoding::Utf8, || false).unwrap());
    assert_eq!(mid.get("first_physical_line").number(), 3);
    let previous = json(&reader.read_position(3, 1, ReaderColumnKind::UnicodeScalar, 12, 1024,
        ObservedReaderEncoding::Utf8, || false).unwrap());
    assert_eq!(previous.get("first_physical_line"), &Json::Null);
    let origin = json(&reader.read_position(3, 1, ReaderColumnKind::UnicodeScalar, 64, 1024,
        ObservedReaderEncoding::Utf8, || false).unwrap());
    assert_eq!(origin.get("first_physical_line").number(), 1);
}

#[test]
fn bom_only_empty_lines_and_eof_carets_follow_the_complete_reader_semantics() {
    for encoding in encodings() {
        for text in ["\u{feff}", "\u{feff}🦀"] {
            let bytes = encode(text, encoding);
            let fixture = Fixture::new(&bytes);
            let mut reader = fixture.reader();
            let column = if text.ends_with('🦀') { 2 } else { 1 };
            let response = reader.read_position(1, column, ReaderColumnKind::UnicodeScalar, 4,
                1024, encoding, || false).unwrap();
            let page = assert_position(&response, &bytes, 1, column, ReaderColumnKind::UnicodeScalar, bytes.len() as u64, 4);
            assert_eq!(page.get("text").text(), "");
            assert_eq!(page.get("position_navigation").get("window_utf8_offset").number(), 0);
        }
        let bytes = encode("\u{feff}a\r\n\r\nz\r", encoding);
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        let expected = encode("\u{feff}a\r\n", encoding).len() as u64;
        assert_position(&reader.read_position(2, 1, ReaderColumnKind::UnicodeScalar, 4, 1024,
            encoding, || false).unwrap(), &bytes, 2, 1, ReaderColumnKind::UnicodeScalar, expected, 4);
        assert_eq!(reader.read_position(4, 1, ReaderColumnKind::UnicodeScalar, 64, 1024,
            encoding, || false).err(), Some(ObservedReaderError::LineMissing));
    }
    let fixture = Fixture::new(b"");
    assert_eq!(fixture.reader().read_position(1, 1, ReaderColumnKind::UnicodeScalar, 4,
        1024, ObservedReaderEncoding::Utf8, || false).err(), Some(ObservedReaderError::LineMissing));
}

#[test]
fn malformed_original_bytes_and_invalid_unit_interiors_are_not_silently_rounded() {
    for encoding in encodings() {
        let bytes = encode("\u{feff}é🦀x\n", encoding);
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        for column in [2, 4, 5, 6] {
            assert_eq!(reader.read_position(1, column, ReaderColumnKind::DecodedUtf8Byte, 64,
                1024, encoding, || false).err(), Some(ObservedReaderError::InvalidRange));
        }
        assert_eq!(reader.read_position(1, 3, ReaderColumnKind::Utf16CodeUnit, 64,
            1024, encoding, || false).err(), Some(ObservedReaderError::InvalidRange));
    }
    for (bytes, encoding, expected) in [
        (b"\xffa\n".to_vec(), ObservedReaderEncoding::Utf8, 1),
        (vec![0xff, 0xfe, 0x00, 0xd8, b'a', 0], ObservedReaderEncoding::Utf16Le, 4),
        (vec![0xfe, 0xff, 0xd8, 0x00, 0, b'a'], ObservedReaderEncoding::Utf16Be, 4),
    ] {
        let fixture = Fixture::new(&bytes);
        let response = fixture.reader().read_position(1, 2, ReaderColumnKind::UnicodeScalar, 64,
            1024, encoding, || false).unwrap();
        let page = assert_position(&response, &bytes, 1, 2, ReaderColumnKind::UnicodeScalar, expected, 64);
        assert!(page.get("has_replacements").flag());
    }
}

#[test]
fn limit_failures_and_missing_coordinates_charge_reads_without_evicting_history() {
    let fixture = Fixture::new(b"first\nsecond\n");
    let mut reader = fixture.reader();
    let old = json(&reader.read_position(2, 2, ReaderColumnKind::UnicodeScalar, 8, 1024,
        ObservedReaderEncoding::Utf8, || false).unwrap());
    let before = json(&reader.info(|| false).unwrap());
    assert_eq!(reader.read_position(99, 1, ReaderColumnKind::UnicodeScalar, 64, 4,
        ObservedReaderEncoding::Utf8, || false).err(), Some(ObservedReaderError::PositionScanByteLimit));
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(after.get("payload_bytes_read").number(), before.get("payload_bytes_read").number() + 4);
    assert_eq!(after.get("read_calls").number(), before.get("read_calls").number() + 2);
    assert_eq!(reader.read_position(99, 1, ReaderColumnKind::UnicodeScalar, 64, 1024,
        ObservedReaderEncoding::Utf8, || false).err(), Some(ObservedReaderError::LineMissing));
    assert_eq!(reader.read_position(1, 7, ReaderColumnKind::UnicodeScalar, 64, 1024,
        ObservedReaderEncoding::Utf8, || false).err(), Some(ObservedReaderError::PositionColumnMissing));
    assert_eq!(ObservedReaderError::PositionReadCallLimit.to_string(), "OBSERVED_READER_POSITION_READ_CALL_LIMIT");
    assert_eq!(ObservedReaderError::PositionColumnMissing.to_string(), "OBSERVED_READER_COLUMN_MISSING");
    assert_eq!(ObservedReaderError::PositionScanByteLimit.to_string(), "OBSERVED_READER_POSITION_SCAN_BYTE_LIMIT");
    assert_eq!(ids(&json(&reader.info(|| false).unwrap())), [old.get("observation").number()]);
}

#[test]
fn invalid_limits_refuse_before_io_and_cancellation_preserves_the_accepted_observation() {
    let fixture = Fixture::new(b"first\nsecond\n");
    let mut reader = fixture.reader();
    let old = json(&reader.read_position(2, 2, ReaderColumnKind::UnicodeScalar, 8, 1024,
        ObservedReaderEncoding::Utf8, || false).unwrap());
    let before = json(&reader.info(|| false).unwrap());
    for (line, column, bytes, scan) in [
        (0, 1, 64, 1024), (1, 0, 64, 1024), (1, 1, 3, 1024),
        (1, 1, MAX_OBSERVED_READER_WINDOW_BYTES + 1, 1024), (1, 1, 64, 0),
        (1, 1, 64, MAX_OBSERVED_READER_LINE_SCAN_BYTES + 1),
    ] {
        assert_eq!(reader.read_position(line, column, ReaderColumnKind::UnicodeScalar, bytes, scan,
            ObservedReaderEncoding::Utf8, || false).err(), Some(ObservedReaderError::InvalidLimits));
    }
    let invalid = json(&reader.info(|| false).unwrap());
    assert_eq!(invalid.get("payload_bytes_read"), before.get("payload_bytes_read"));
    assert_eq!(invalid.get("read_calls"), before.get("read_calls"));
    let mut polls = 0;
    let error = reader.read_position(2, 2, ReaderColumnKind::UnicodeScalar, 64, 1024,
        ObservedReaderEncoding::Utf8, || { polls += 1; polls == 3 }).err().unwrap();
    assert!(error.is_canceled());
    let canceled = json(&reader.info(|| false).unwrap());
    assert_eq!(canceled.get("payload_bytes_read").number(), before.get("payload_bytes_read").number() + 3);
    assert_eq!(canceled.get("read_calls").number(), before.get("read_calls").number() + 1);
    assert_eq!(ids(&canceled), [old.get("observation").number()]);
}

#[test]
fn same_descriptor_metadata_change_is_refused_and_retained_receipts_never_read_live_paths() {
    let fixture = Fixture::new(b"first\noriginal\r\nlast");
    let file = fixture.file();
    let mut reader = fixture.reader();
    let old = json(&reader.read_position(2, 2, ReaderColumnKind::UnicodeScalar, 8, 1024,
        ObservedReaderEncoding::Utf8, || false).unwrap());
    fs::rename(fixture.0.join("source"), fixture.0.join("previous")).unwrap();
    fs::write(fixture.0.join("source"), b"unrelated replacement").unwrap();
    let same = json(&reader.read_position(2, 2, ReaderColumnKind::UnicodeScalar, 8, 1024,
        ObservedReaderEncoding::Utf8, || false).unwrap());
    assert_eq!(same.get("text"), old.get("text"));
    let mut polls = 0;
    let error = reader.read_position(2, 2, ReaderColumnKind::UnicodeScalar, 8, 1024,
        ObservedReaderEncoding::Utf8, || {
            polls += 1;
            if polls == 3 { file.set_len(25).unwrap(); }
            false
        }).err().unwrap();
    assert_eq!(error, ObservedReaderError::App(AppError::SourceChanged));
    assert_eq!(file.write_at(b"changed!", 6).unwrap(), 8);
    let before = json(&reader.info(|| false).unwrap());
    let replay = json(&reader.retained_window(old.get("observation").number(), || false).unwrap());
    assert_eq!(replay.get("text"), old.get("text"));
    assert_eq!(replay.get("original_hex"), old.get("original_hex"));
    assert_eq!(replay.get("position_navigation"), old.get("position_navigation"));
    assert_eq!(replay.get("additional_source_bytes_read").number(), 0);
    let copy = json(&reader.copy_range(old.get("observation").number(), 7, 9, || false).unwrap());
    assert_eq!(copy.get("original_hex").text(), hex(b"ri"));
    assert_eq!(copy.get("position_navigation"), old.get("position_navigation"),
        "page-local UTF-8 offset stays bound to the original page, not the copied subrange");
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(after.get("payload_bytes_read"), before.get("payload_bytes_read"));
    assert_eq!(after.get("read_calls"), before.get("read_calls"));
}

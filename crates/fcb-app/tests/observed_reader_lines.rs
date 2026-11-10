#![forbid(unsafe_code)]
#![cfg(unix)]

//! Exact line navigation through the public native-host session on real opened
//! files. Expected ranges come from fixture construction, independent of the
//! production scanner. Execution belongs to the strict-RCH verification lane.
mod support;

use std::{fs::{self, OpenOptions}, os::unix::fs::FileExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{AppError, EXIT_PARTIAL, host::{HostResponse, observed_reader::{
    ObservedReaderEncoding, ObservedReaderError, ObservedReaderSession,
    MAX_OBSERVED_READER_LINES, MAX_OBSERVED_READER_LINE_SCAN_BYTES,
    MAX_OBSERVED_READER_WINDOW_BYTES}}};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-observed-lines-{}-{now}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("source"), bytes).unwrap();
        Self(root)
    }
    fn file(&self) -> fs::File {
        OpenOptions::new().read(true).write(true).open(self.0.join("source")).unwrap()
    }
    fn reader(&self) -> ObservedReaderSession {
        static NEXT: AtomicU64 = AtomicU64::new(91_000);
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
fn line_page(reader: &mut ObservedReaderSession, first: u64, count: u64) -> Json {
    json(&reader.read_lines(first, count, 128, 1024 * 1024, ObservedReaderEncoding::Utf8, || false).unwrap())
}
fn utf16(text: &str, encoding: ObservedReaderEncoding) -> Vec<u8> {
    text.encode_utf16().flat_map(|unit| if encoding == ObservedReaderEncoding::Utf16Le {
        unit.to_le_bytes()
    } else { unit.to_be_bytes() }).collect()
}

#[test]
fn sparse_large_prefix_is_scanned_once_and_only_the_selected_line_is_retained() {
    let fixture = Fixture::new(b"");
    let file = fixture.file();
    let prefix = 32 * 1024 * 1024u64 + 1;
    let length = prefix + 64;
    file.set_len(length).unwrap();
    assert_eq!(file.write_at(b"\nchosen\r\nnext\n", prefix).unwrap(), 14);
    let mut reader = fixture.reader();
    let initial = json(&reader.info(|| false).unwrap());
    assert_eq!(initial.get("payload_bytes_read").number(), 0);
    let response = reader.read_lines(2, 1, 64, 64 * 1024 * 1024,
        ObservedReaderEncoding::Utf8, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL, "this is still a retained extent, not an immutable file");
    assert!(response.as_str().len() < 8192);
    let page = json(&response);
    assert_eq!(page.get("command").text(), "lines");
    assert_eq!(page.get("text").text(), "chosen\r\n");
    assert_eq!(page.get("original_hex").text(), hex(b"chosen\r\n"));
    assert_eq!(bounds(page.get("requested_visible_range")), (prefix + 1, prefix + 9));
    assert_eq!(bounds(page.get("visible_range")), (prefix + 1, prefix + 9));
    assert_eq!(page.get("requested_bytes").number(), 64);
    assert_eq!(page.get("first_physical_line").number(), 2);
    assert_eq!(page.get("consistency").text(), "host-supplied-sequence");
    assert!(!page.get("whole_file_immutable").flag());
    assert!(!page.get("whole_file_observed").flag());
    assert_eq!(page.get("observed_length").number(), length);
    assert_eq!(page.get("final_observed_length").number(), length);
    let receipt = page.get("line_navigation");
    assert_eq!(receipt.get("metadata_consistency").text(), "unchanged-metadata-not-atomic");
    assert_eq!(receipt.get("requested_first_line").number(), 2);
    assert_eq!(receipt.get("requested_lines").number(), 1);
    assert_eq!(receipt.get("first_line").number(), 2);
    assert_eq!(receipt.get("last_line").number(), 2);
    assert_eq!(receipt.get("max_scan_bytes").number(), 64 * 1024 * 1024);
    assert_eq!(receipt.get("max_selected_bytes").number(), 64);
    assert!(receipt.get("scanned_input_bytes").number() >= prefix + 9);
    assert!(receipt.get("scanned_input_bytes").number() <= receipt.get("payload_bytes_read").number());
    assert!(receipt.get("payload_bytes_read").number() > prefix);
    assert!(receipt.get("payload_bytes_read").number() <= length);
    assert!(receipt.get("read_calls").number() > 1);
    assert!(receipt.get("lines_seen").number() >= 2);
    assert!(page.get("captured_bytes").number() <= 16);
    assert_eq!(page.get("observation_source_bytes_read"), receipt.get("payload_bytes_read"));
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(after.get("payload_bytes_read"), receipt.get("payload_bytes_read"));
    assert_eq!(after.get("read_calls"), receipt.get("read_calls"));
    assert_eq!(after.get("retained_payload_bytes"), page.get("captured_bytes"));
    assert_eq!(ids(&after), [1]);
}

#[test]
fn genuine_eof_allows_short_windows_but_never_a_phantom_final_line() {
    for bytes in [b"first\n".as_slice(), b"first\r\n", b"first\r", b"first"] {
        let fixture = Fixture::new(bytes);
        let mut reader = fixture.reader();
        let page = line_page(&mut reader, 1, 20);
        assert_eq!(page.get("text").text().as_bytes(), bytes);
        assert_eq!(page.get("line_navigation").get("last_line").number(), 1);
        assert_eq!(page.get("line_navigation").get("requested_lines").number(), 20);
        let error = reader.read_lines(2, 1, 128, 1024, ObservedReaderEncoding::Utf8, || false).err().unwrap();
        assert_eq!(error, ObservedReaderError::LineMissing);
        assert_eq!(ids(&json(&reader.info(|| false).unwrap())), [1]);
    }
    let fixture = Fixture::new(b"");
    let mut reader = fixture.reader();
    assert_eq!(reader.read_lines(1, 1, 4, 64, ObservedReaderEncoding::Utf8, || false).err().unwrap(),
        ObservedReaderError::LineMissing);
    let empty = json(&reader.info(|| false).unwrap());
    assert!(ids(&empty).is_empty());
    assert_eq!(empty.get("payload_bytes_read").number(), 0);
    assert!(empty.get("read_calls").number() > 0);
    let fixture = Fixture::new(b"first\nx");
    let mut reader = fixture.reader();
    let page = json(&reader.read_lines(2, 20, 4, 64, ObservedReaderEncoding::Utf8, || false).unwrap());
    assert_eq!(page.get("text").text(), "x");
    assert_eq!(bounds(page.get("visible_range")), (6, 7));
    assert_eq!(page.get("requested_bytes").number(), 4);
    assert_eq!(page.get("line_navigation").get("last_line").number(), 2);
}

#[test]
fn first_line_boms_are_visible_boundary_adjustments_and_remain_exactly_copyable() {
    let cases = [
        (ObservedReaderEncoding::Utf8, b"\xef\xbb\xbffirst\r\nnext".to_vec(), 3u64, 10u64),
        (ObservedReaderEncoding::Utf16Le, utf16("\u{feff}first\r\nnext", ObservedReaderEncoding::Utf16Le), 2, 16),
        (ObservedReaderEncoding::Utf16Be, utf16("\u{feff}first\r\nnext", ObservedReaderEncoding::Utf16Be), 2, 16),
    ];
    for (encoding, bytes, header, end) in cases {
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        let page = json(&reader.read_lines(1, 1, 64, 1024, encoding, || false).unwrap());
        assert_eq!(page.get("text").text(), "first\r\n");
        assert_eq!(bounds(page.get("requested_visible_range")), (0, end));
        assert_eq!(bounds(page.get("visible_range")), (header, end));
        assert!(page.get("boundaries_adjusted").flag());
        assert_eq!(page.get("first_physical_line").number(), 1);
        assert_eq!(page.get("original_hex").text(), hex(&bytes[header as usize..end as usize]));
        let copied = json(&reader.copy_range(page.get("observation").number(), 0, end, || false).unwrap());
        assert_eq!(copied.get("original_hex").text(), hex(&bytes[..end as usize]));
        assert_eq!(copied.get("line_navigation"), page.get("line_navigation"));
        assert_eq!(copied.get("additional_source_bytes_read").number(), 0);
    }
    for (encoding, bom) in [
        (ObservedReaderEncoding::Utf8, b"\xef\xbb\xbf".as_slice()),
        (ObservedReaderEncoding::Utf16Le, b"\xff\xfe".as_slice()),
        (ObservedReaderEncoding::Utf16Be, b"\xfe\xff".as_slice()),
    ] {
        let fixture = Fixture::new(bom);
        let mut reader = fixture.reader();
        let page = json(&reader.read_lines(1, 20, 4, 64, encoding, || false).unwrap());
        assert_eq!(page.get("text").text(), "");
        assert_eq!(bounds(page.get("visible_range")), (bom.len() as u64, bom.len() as u64));
        assert_eq!(page.get("first_physical_line").number(), 1);
        assert_eq!(page.get("line_navigation").get("first_line").number(), 1);
        assert_eq!(page.get("line_navigation").get("last_line").number(), 1);
        let copied = json(&reader.copy_range(page.get("observation").number(), 0, bom.len() as u64, || false).unwrap());
        assert_eq!(copied.get("original_hex").text(), hex(bom));
    }
}

#[test]
fn utf16_surrogates_in_content_boms_and_dangling_bytes_keep_original_line_ranges() {
    for encoding in [ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let prefix = utf16("\u{feff}first\r\n", encoding);
        let selected = utf16("\u{feff}\u{1f980}\u{0a41}\r", encoding);
        let mut bytes = prefix.clone();
        bytes.extend_from_slice(&selected);
        bytes.push(0x41); // A final unmatched UTF-16 byte is content on line 3.
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        let page = json(&reader.read_lines(2, 1, 64, 1024, encoding, || false).unwrap());
        assert_eq!(page.get("text").text(), "\u{feff}\u{1f980}\u{0a41}\r");
        assert_eq!(page.get("first_physical_line").number(), 2);
        assert_eq!(page.get("original_hex").text(), hex(&selected));
        assert_eq!(bounds(page.get("visible_range")), (prefix.len() as u64, (prefix.len() + selected.len()) as u64));
        let final_page = json(&reader.read_lines(3, 20, 4, 1024, encoding, || false).unwrap());
        assert_eq!(final_page.get("text").text(), "\u{fffd}");
        assert!(final_page.get("has_replacements").flag());
        assert_eq!(final_page.get("original_hex").text(), "41");
        assert_eq!(final_page.get("first_physical_line").number(), 3);
        assert_eq!(final_page.get("line_navigation").get("last_line").number(), 3);
        assert_eq!(bounds(final_page.get("visible_range")), ((bytes.len() - 1) as u64, bytes.len() as u64));
    }
    let fixture = Fixture::new(b"first\n\xf0\x80bad\r\nlast");
    let mut reader = fixture.reader();
    let page = line_page(&mut reader, 2, 1);
    assert!(page.get("has_replacements").flag());
    assert!(page.get("text").text().ends_with("bad\r\n"));
    assert_eq!(page.get("original_hex").text(), hex(b"\xf0\x80bad\r\n"));
    assert_eq!(page.get("first_physical_line").number(), 2);
}

#[test]
fn missing_scan_and_output_limits_are_distinct_and_leave_the_last_page_available() {
    let fixture = Fixture::new(b"first\nsecond\n");
    let mut reader = fixture.reader();
    let accepted = line_page(&mut reader, 1, 1);
    let before = json(&reader.info(|| false).unwrap());
    let error = reader.read_lines(99, 1, 64, 4, ObservedReaderEncoding::Utf8, || false).err().unwrap();
    assert_eq!(error, ObservedReaderError::LineScanByteLimit);
    assert_eq!(error.to_string(), "OBSERVED_READER_LINE_SCAN_BYTE_LIMIT");
    let after_scan = json(&reader.info(|| false).unwrap());
    assert_eq!(after_scan.get("payload_bytes_read").number(), before.get("payload_bytes_read").number() + 4);
    assert_eq!(after_scan.get("read_calls").number(), before.get("read_calls").number() + 2);
    let error = reader.read_lines(1, 1, 4, 1024, ObservedReaderEncoding::Utf8, || false).err().unwrap();
    assert_eq!(error, ObservedReaderError::LineOutputByteLimit);
    assert_eq!(error.to_string(), "OBSERVED_READER_LINE_OUTPUT_BYTE_LIMIT");
    let error = reader.read_lines(99, 1, 64, 1024, ObservedReaderEncoding::Utf8, || false).err().unwrap();
    assert_eq!(error, ObservedReaderError::LineMissing);
    assert_eq!(error.to_string(), "OBSERVED_READER_LINE_MISSING");
    assert_eq!(ObservedReaderError::LineReadCallLimit.to_string(), "OBSERVED_READER_LINE_READ_CALL_LIMIT");
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(ids(&after), [accepted.get("observation").number()]);
    assert!(after.get("payload_bytes_read").number() > after_scan.get("payload_bytes_read").number());
    let replay = json(&reader.retained_window(accepted.get("observation").number(), || false).unwrap());
    assert_eq!(replay.get("text"), accepted.get("text"));
    assert_eq!(replay.get("line_navigation"), accepted.get("line_navigation"));
    assert_eq!(replay.get("additional_source_bytes_read").number(), 0);
}

#[test]
fn invalid_requests_refuse_before_payload_reads_and_do_not_evict_history() {
    let fixture = Fixture::new(b"first\nsecond");
    let mut reader = fixture.reader();
    let accepted = line_page(&mut reader, 1, 1);
    let before = json(&reader.info(|| false).unwrap());
    let invalid: [(u64, u64, usize, u64); 8] = [
        (0, 1, 64, 1024), (1, 0, 64, 1024),
        (1, MAX_OBSERVED_READER_LINES + 1, 64, 1024), (u64::MAX, 2, 64, 1024),
        (1, 1, 3, 1024), (1, 1, MAX_OBSERVED_READER_WINDOW_BYTES + 1, 1024),
        (1, 1, 64, 0), (1, 1, 64, MAX_OBSERVED_READER_LINE_SCAN_BYTES + 1),
    ];
    for (first, count, bytes, scan) in invalid {
        assert_eq!(reader.read_lines(first, count, bytes, scan, ObservedReaderEncoding::Utf8, || false).err().unwrap(),
            ObservedReaderError::InvalidLimits);
    }
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(after.get("payload_bytes_read"), before.get("payload_bytes_read"));
    assert_eq!(after.get("read_calls"), before.get("read_calls"));
    assert_eq!(ids(&after), [accepted.get("observation").number()]);
}

#[test]
fn cancellation_after_reading_and_metadata_change_charge_io_without_publishing() {
    let fixture = Fixture::new(b"first\nsecond\n");
    let mut reader = fixture.reader();
    let accepted = line_page(&mut reader, 1, 1);
    let before = json(&reader.info(|| false).unwrap());
    let mut polls = 0;
    let error = reader.read_lines(2, 1, 64, 1024, ObservedReaderEncoding::Utf8,
        || { polls += 1; polls == 3 }).err().unwrap();
    assert!(error.is_canceled());
    let canceled = json(&reader.info(|| false).unwrap());
    assert_eq!(canceled.get("payload_bytes_read").number(), before.get("payload_bytes_read").number() + 3);
    assert_eq!(canceled.get("read_calls").number(), before.get("read_calls").number() + 1);
    assert_eq!(ids(&canceled), [accepted.get("observation").number()]);
    let file = fixture.file();
    let mut polls = 0;
    let error = reader.read_lines(2, 1, 64, 1024, ObservedReaderEncoding::Utf8, || {
        polls += 1;
        if polls == 3 { file.set_len(14).unwrap(); }
        false
    }).err().unwrap();
    assert_eq!(error, ObservedReaderError::App(AppError::SourceChanged));
    let changed = json(&reader.info(|| false).unwrap());
    assert!(changed.get("payload_bytes_read").number() > canceled.get("payload_bytes_read").number());
    assert!(changed.get("read_calls").number() > canceled.get("read_calls").number());
    assert_eq!(ids(&changed), [accepted.get("observation").number()]);
    let replay = json(&reader.retained_window(accepted.get("observation").number(), || false).unwrap());
    assert_eq!(replay.get("text"), accepted.get("text"));
    assert_eq!(replay.get("original_hex"), accepted.get("original_hex"));
    assert_eq!(replay.get("line_navigation"), accepted.get("line_navigation"));
}

#[test]
fn opened_descriptor_and_retained_line_history_survive_path_replacement_and_live_writes() {
    let fixture = Fixture::new(b"first\noriginal\r\nlast");
    let file = fixture.file();
    let mut reader = fixture.reader();
    let old = line_page(&mut reader, 2, 1);
    fs::rename(fixture.0.join("source"), fixture.0.join("previous")).unwrap();
    fs::write(fixture.0.join("source"), b"first\nreplacement\r\nlast").unwrap();
    let same_opened = line_page(&mut reader, 2, 1);
    assert_eq!(same_opened.get("text").text(), "original\r\n");
    let mut reopened = fixture.reader();
    assert_eq!(line_page(&mut reopened, 2, 1).get("text").text(), "replacement\r\n");
    assert_eq!(file.write_at(b"changed!\r\n", 6).unwrap(), 10);
    let current = line_page(&mut reader, 2, 1);
    assert_eq!(current.get("text").text(), "changed!\r\n");
    let before_replay = json(&reader.info(|| false).unwrap());
    let replay = json(&reader.retained_window(old.get("observation").number(), || false).unwrap());
    assert_eq!(replay.get("text").text(), "original\r\n");
    assert_eq!(replay.get("line_navigation"), old.get("line_navigation"));
    assert_eq!(replay.get("first_physical_line").number(), 2);
    assert_eq!(replay.get("additional_source_bytes_read").number(), 0);
    let copied = json(&reader.copy_range(old.get("observation").number(), 6, 16, || false).unwrap());
    assert_eq!(copied.get("original_hex").text(), hex(b"original\r\n"));
    assert_eq!(copied.get("line_navigation"), old.get("line_navigation"));
    let after_replay = json(&reader.info(|| false).unwrap());
    assert_eq!(after_replay.get("payload_bytes_read"), before_replay.get("payload_bytes_read"));
    assert_eq!(after_replay.get("read_calls"), before_replay.get("read_calls"));
}

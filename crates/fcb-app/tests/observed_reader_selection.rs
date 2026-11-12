#![forbid(unsafe_code)]
#![cfg(unix)]

//! Selections over actual retained file observations. Expected source ranges
//! are calculated from fixture encodings, independently of the decoder map.
//! Execution belongs to the independent strict-RCH verification lane.
mod support;

use std::{fs::{self, OpenOptions}, os::unix::fs::FileExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, host::{HostResponse, observed_reader::{
    ObservedReaderEncoding, ObservedReaderError, ObservedReaderSession,
    MAX_OBSERVED_READER_OBSERVATIONS, MAX_OBSERVED_READER_WINDOW_BYTES}}};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-observed-selection-{}-{now}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("source"), bytes).unwrap();
        Self(root)
    }
    fn reader(&self) -> ObservedReaderSession {
        static NEXT: AtomicU64 = AtomicU64::new(131_000);
        ObservedReaderSession::open(ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap(),
            &self.0, b"source", || false).unwrap()
    }
    fn file(&self) -> fs::File {
        OpenOptions::new().read(true).write(true).open(self.0.join("source")).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(value: &Json) -> (u64, u64) { (value.get("start").number(), value.get("end").number()) }
fn encoded(text: &str, encoding: ObservedReaderEncoding) -> Vec<u8> {
    match encoding {
        ObservedReaderEncoding::Utf8 => text.as_bytes().to_vec(),
        ObservedReaderEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ObservedReaderEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
    }
}
fn page(reader: &mut ObservedReaderSession, offset: u64, bytes: usize,
    encoding: ObservedReaderEncoding) -> Json {
    json(&reader.read_window(offset, bytes, encoding, || false).unwrap())
}
fn assert_selection(response: &HostResponse, observed: &Json, decoded: (u64, u64),
    original: (u64, u64), text: &str, raw: &[u8], replacements: bool) -> Json {
    assert_eq!(response.exit_code(), EXIT_OK);
    let value = json(response);
    assert_eq!(value.get("schema").text(), "fcb.observed-reader/1");
    assert_eq!(value.get("command").text(), "resolve-selection");
    assert_eq!(value.get("selection_namespace").text(), "native-text");
    for key in ["owner", "file_id", "path", "observation", "encoding", "encoding_basis",
        "captured_range", "captured_bytes", "extent_sha256", "consistency",
        "observed_length", "final_observed_length", "line_navigation"] {
        assert_eq!(value.get(key), observed.get(key), "{key}");
    }
    assert_eq!(value.get("capture_kind").text(), "observed-extent");
    assert!(!value.get("whole_file_immutable").flag());
    assert!(!value.get("native_presented").flag());
    assert_eq!(value.get("window_original_range"), observed.get("visible_range"));
    assert_eq!(bounds(value.get("window_utf8_range")), decoded);
    assert_eq!(bounds(value.get("original_range")), original);
    assert_eq!(value.get("text").text().as_bytes(), text.as_bytes());
    assert_eq!(value.get("original_hex").text(), hex(raw));
    assert_eq!(value.get("contains_replacements").flag(), replacements);
    assert_eq!(value.get("additional_source_bytes_read").number(), 0);
    assert_eq!(value.get("additional_read_calls").number(), 0);
    value
}

#[test]
fn unicode_selection_preserves_encoding_bom_crlf_bidi_nul_and_source_order() {
    let prefix = "before ";
    let selected = "e\u{301} 🦀\r\nאב\t\0尾";
    let suffix = " \u{feff} after";
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let mut bytes = match encoding {
            ObservedReaderEncoding::Utf8 => vec![0xef, 0xbb, 0xbf],
            ObservedReaderEncoding::Utf16Le => vec![0xff, 0xfe],
            ObservedReaderEncoding::Utf16Be => vec![0xfe, 0xff],
        };
        let bom = bytes.len() as u64;
        bytes.extend(encoded(&format!("{prefix}{selected}{suffix}"), encoding));
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        let observed = page(&mut reader, 0, 512, encoding);
        let before = json(&reader.info(|| false).unwrap());
        let decoded = (prefix.len() as u64, (prefix.len() + selected.len()) as u64);
        let start = bom + encoded(prefix, encoding).len() as u64;
        let raw = encoded(selected, encoding);
        let response = reader.resolve_selection(observed.get("observation").number(), decoded.0, decoded.1, || false).unwrap();
        assert_selection(&response, &observed, decoded, (start, start + raw.len() as u64), selected, &raw, false);
        let after = json(&reader.info(|| false).unwrap());
        for key in ["payload_bytes_read", "read_calls", "retained_observations", "retained_payload_bytes"] {
            assert_eq!(before.get(key), after.get(key), "{key}");
        }
    }
}

#[test]
fn nonzero_window_and_adjusted_scalar_start_are_not_confused_with_file_offsets() {
    let mut bytes = vec![b'x'; 512];
    bytes.extend_from_slice("Aé🦀\r\nB same same tail padding".as_bytes());
    let fixture = Fixture::new(&bytes);
    let mut reader = fixture.reader();
    // The requested start is inside é; only the shared decoder adjusts it.
    let observed = page(&mut reader, 514, 20, ObservedReaderEncoding::Utf8);
    assert_eq!(observed.get("visible_range").get("start").number(), 513);
    assert!(observed.get("text").text().starts_with("é🦀"));
    let response = reader.resolve_selection(1, 0, 6, || false).unwrap();
    assert_selection(&response, &observed, (0, 6), (513, 519), "é🦀", "é🦀".as_bytes(), false);
}

#[test]
fn replacements_identify_only_selected_malformed_original_bytes() {
    for (bytes, encoding, original) in [
        (b"A\xffB".to_vec(), ObservedReaderEncoding::Utf8, (1, 2)),
        (vec![0xff, 0xfe, 0x41, 0, 0, 0xd8, 0x42, 0], ObservedReaderEncoding::Utf16Le, (4, 6)),
        (vec![0xfe, 0xff, 0, 0x41, 0xd8, 0, 0, 0x42], ObservedReaderEncoding::Utf16Be, (4, 6)),
    ] {
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        let observed = page(&mut reader, 0, 32, encoding);
        assert_eq!(observed.get("text").text(), "A\u{fffd}B");
        let response = reader.resolve_selection(1, 1, 4, || false).unwrap();
        assert_selection(&response, &observed, (1, 4), original, "\u{fffd}",
            &bytes[original.0 as usize..original.1 as usize], true);
        let ordinary = json(&reader.resolve_selection(1, 4, 5, || false).unwrap());
        assert_eq!(ordinary.get("text").text(), "B");
        assert!(!ordinary.get("contains_replacements").flag());
        assert!(reader.resolve_selection(1, 2, 4, || false).is_err());
    }
    let fixture = Fixture::new("A\u{fffd}B".as_bytes());
    let mut reader = fixture.reader();
    let observed = page(&mut reader, 0, 32, ObservedReaderEncoding::Utf8);
    let response = reader.resolve_selection(1, 1, 4, || false).unwrap();
    assert_selection(&response, &observed, (1, 4), (1, 4), "\u{fffd}", "\u{fffd}".as_bytes(), false);
}

#[test]
fn sparse_file_selection_preserves_full_width_offsets_without_prefix_reads() {
    let fixture = Fixture::new(b"");
    let file = fixture.file();
    let start = (1u64 << 32) + 128;
    file.set_len(start + 128).unwrap();
    file.write_at(b"same xx same yy", start).unwrap();
    let mut reader = fixture.reader();
    let observed = page(&mut reader, start, 15, ObservedReaderEncoding::Utf8);
    let before = json(&reader.info(|| false).unwrap());
    let response = reader.resolve_selection(1, 8, 12, || false).unwrap();
    assert_selection(&response, &observed, (8, 12), (start + 8, start + 12), "same", b"same", false);
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(before.get("payload_bytes_read"), after.get("payload_bytes_read"));
    assert!(after.get("payload_bytes_read").number() <= 31);
}

#[test]
fn retained_selection_survives_live_changes_and_explicit_path_replacement() {
    let fixture = Fixture::new(b"old selected source\n");
    let file = fixture.file();
    let mut reader = fixture.reader();
    let observed = page(&mut reader, 0, 64, ObservedReaderEncoding::Utf8);
    file.write_at(b"new modified source\n", 0).unwrap();
    fs::rename(fixture.0.join("source"), fixture.0.join("old-object")).unwrap();
    fs::write(fixture.0.join("source"), b"unrelated replacement").unwrap();
    let before = json(&reader.info(|| false).unwrap());
    let response = reader.resolve_selection(1, 4, 12, || false).unwrap();
    assert_selection(&response, &observed, (4, 12), (4, 12), "selected", b"selected", false);
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(before.get("payload_bytes_read"), after.get("payload_bytes_read"));
    assert_eq!(before.get("read_calls"), after.get("read_calls"));
    let current = page(&mut reader, 0, 64, ObservedReaderEncoding::Utf8);
    assert_eq!(current.get("observation").number(), 2, "selection must not allocate an observation");
    assert_eq!(current.get("text").text(), "new modified source\n");
    assert_ne!(current.get("extent_sha256"), observed.get("extent_sha256"));
    let response = reader.resolve_selection(1, 4, 12, || false).unwrap();
    assert_selection(&response, &observed, (4, 12), (4, 12), "selected", b"selected", false);
}

#[test]
fn line_scan_observation_keeps_its_physical_line_provenance() {
    let fixture = Fixture::new(b"first\r\nchosen selection\r\nlast");
    let mut reader = fixture.reader();
    let observed = json(&reader.read_lines(2, 1, 64, 1024, ObservedReaderEncoding::Utf8, || false).unwrap());
    assert_eq!(observed.get("text").text(), "chosen selection\r\n");
    let response = reader.resolve_selection(1, 7, 16, || false).unwrap();
    let selected = assert_selection(&response, &observed, (7, 16), (14, 23), "selection", b"selection", false);
    assert_eq!(selected.get("line_navigation").get("first_line").number(), 2);
}

#[test]
fn invalid_evicted_and_canceled_selections_preserve_observations_and_io() {
    let fixture = Fixture::new("A🦀B\r\n".as_bytes());
    let mut reader = fixture.reader();
    page(&mut reader, 0, 64, ObservedReaderEncoding::Utf8);
    let before = json(&reader.info(|| false).unwrap());
    for (start, end) in [(0, 0), (5, 1), (0, 100), (2, 5), (1, 3), (u64::MAX - 1, u64::MAX)] {
        assert!(reader.resolve_selection(1, start, end, || false).is_err(), "{start}..{end}");
    }
    assert_eq!(reader.resolve_selection(0, 0, 1, || false).err().unwrap(), ObservedReaderError::MissingObservation);
    for stop_at in 1..=6 {
        let mut polls = 0;
        let error = reader.resolve_selection(1, 1, 5, || {
            polls += 1; polls == stop_at
        }).err().unwrap();
        assert!(error.is_canceled(), "{stop_at}: {error:?}");
    }
    let after = json(&reader.info(|| false).unwrap());
    for key in ["payload_bytes_read", "read_calls", "retained_observations", "retained_payload_bytes"] {
        assert_eq!(before.get(key), after.get(key), "{key}");
    }
    assert_eq!(json(&reader.resolve_selection(1, 1, 5, || false).unwrap()).get("text").text(), "🦀");
    for _ in 0..MAX_OBSERVED_READER_OBSERVATIONS {
        page(&mut reader, 0, 64, ObservedReaderEncoding::Utf8);
    }
    let before_evicted = json(&reader.info(|| false).unwrap());
    assert_eq!(reader.resolve_selection(1, 0, 1, || false).err().unwrap(), ObservedReaderError::MissingObservation);
    let after_evicted = json(&reader.info(|| false).unwrap());
    assert_eq!(before_evicted.get("payload_bytes_read"), after_evicted.get("payload_bytes_read"));
}

#[test]
fn maximum_page_selection_is_complete_and_never_silently_truncated() {
    let bytes = vec![b'a'; MAX_OBSERVED_READER_WINDOW_BYTES];
    let fixture = Fixture::new(&bytes);
    let mut reader = fixture.reader();
    let observed = page(&mut reader, 0, bytes.len(), ObservedReaderEncoding::Utf8);
    let response = reader.resolve_selection(1, 0, bytes.len() as u64, || false).unwrap();
    assert_selection(&response, &observed, (0, bytes.len() as u64), (0, bytes.len() as u64),
        std::str::from_utf8(&bytes).unwrap(), &bytes, false);
    assert!(reader.resolve_selection(1, 0, bytes.len() as u64 + 1, || false).is_err());
}

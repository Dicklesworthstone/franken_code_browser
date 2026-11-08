#![forbid(unsafe_code)]
#![cfg(unix)]

//! Production host observations of real open files. Assertions use independently
//! constructed byte slices and filesystem changes, not whole-capture readers.
//! Execution belongs to the independent strict-RCH verification lane.
mod support;

use std::{ffi::OsString, fs::{self, OpenOptions}, os::unix::{ffi::{OsStrExt, OsStringExt}, fs::FileExt},
    path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::{HostResponse, observed_reader::{ObservedReaderEncoding,
    ObservedReaderError, ObservedReaderSession, MAX_OBSERVED_READER_WINDOW_BYTES,
    MAX_OBSERVED_READER_OBSERVATIONS}}};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-observed-host-{}-{now}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap(); Self(root)
    }
    fn write(&self, bytes: &[u8]) { fs::write(self.0.join("source"), bytes).unwrap(); }
    fn reader(&self) -> ObservedReaderSession {
        static NEXT: AtomicU64 = AtomicU64::new(61_000);
        ObservedReaderSession::open(ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap(),
            &self.0, b"source", || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn sha256(bytes: &[u8]) -> String {
    let mut digest = fcb::store::Sha256::new(); digest.update(bytes); digest.finalize().to_hex()
}
fn bounds(value: &Json) -> (u64, u64) { (value.get("start").number(), value.get("end").number()) }
fn ids(value: &Json) -> Vec<u64> { value.get("retained_observations").array().iter().map(Json::number).collect() }
fn read(reader: &mut ObservedReaderSession, offset: u64, bytes: usize) -> Json {
    json(&reader.read_window(offset, bytes, ObservedReaderEncoding::Utf8, || false).unwrap())
}

#[test]
fn sparse_file_above_four_gib_is_metadata_only_until_a_bounded_observation() {
    let fixture = Fixture::new();
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(fixture.0.join("source")).unwrap();
    let offset = (1u64 << 32) + 71;
    file.set_len(offset + 128).unwrap();
    assert_eq!(file.write_at(b"needle", offset).unwrap(), 6);
    let mut reader = fixture.reader();
    let initial = json(&reader.info(|| false).unwrap());
    assert_eq!(initial.get("initial_observed_length").number(), offset + 128);
    assert_eq!(initial.get("payload_bytes_read").number(), 0);
    assert_eq!(initial.get("read_calls").number(), 0);
    assert!(ids(&initial).is_empty());
    let response = reader.read_window(offset, 6, ObservedReaderEncoding::Utf8, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    assert!(response.as_str().len() < 8192);
    assert!(!response.as_str().contains("capture_sha256"));
    let page = json(&response);
    assert_eq!(bounds(page.get("visible_range")), (offset, offset + 6));
    assert_eq!(bounds(page.get("captured_range")), (offset - 8, offset + 14));
    assert_eq!(page.get("text").text(), "needle");
    assert_eq!(page.get("original_hex").text(), hex(b"needle"));
    assert_eq!(page.get("observation_source_bytes_read").number(), 22);
    let mut captured = vec![0; 8]; captured.extend_from_slice(b"needle"); captured.extend_from_slice(&[0; 8]);
    assert_eq!(page.get("extent_sha256").text(), sha256(&captured), "digest includes only captured context and visible bytes");
    assert_ne!(page.get("extent_sha256").text(), sha256(b"needle"));
    assert_eq!(page.get("additional_source_bytes_read").number(), 22);
    assert!(page.get("observation_read_calls").number() > 0);
    assert_eq!(page.get("first_physical_line"), &Json::Null);
    assert_eq!(page.get("next_offset").number(), offset + 6);
    assert!(page.get("requested_capture_filled").flag());
    assert!(!page.get("whole_file_observed").flag());
    assert!(!page.get("whole_file_immutable").flag());
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(after.get("payload_bytes_read").number(), 22);
    assert_eq!(after.get("retained_payload_bytes").number(), 22);
    assert_eq!(ids(&after), [1]);
}

#[test]
fn live_writes_refresh_only_new_observations_and_old_copy_never_reads_again() {
    let fixture = Fixture::new(); fixture.write(b"old source text\n");
    let mut reader = fixture.reader();
    let old = read(&mut reader, 0, 8);
    fixture.write(b"new source text\n");
    let new = read(&mut reader, 0, 8);
    assert_eq!(old.get("observation").number(), 1);
    assert_eq!(new.get("observation").number(), 2);
    assert_ne!(old.get("extent_sha256"), new.get("extent_sha256"));
    assert_eq!(old.get("text").text(), "old sour");
    assert_eq!(new.get("text").text(), "new sour");
    fs::remove_file(fixture.0.join("source")).unwrap();
    let before = json(&reader.info(|| false).unwrap());
    let replay = json(&reader.retained_window(1, || false).unwrap());
    for field in ["observation", "encoding", "extent_sha256", "captured_range", "visible_range", "text", "original_hex"] {
        assert_eq!(old.get(field), replay.get(field), "retained observation field {field}");
    }
    assert_eq!(replay.get("command").text(), "retained-window");
    assert_eq!(replay.get("additional_source_bytes_read").number(), 0);
    assert_eq!(replay.get("additional_read_calls").number(), 0);
    let copy = json(&reader.copy_range(1, 1, 7, || false).unwrap());
    assert_eq!(copy.get("original_hex").text(), hex(b"ld sou"));
    assert_eq!(copy.get("copy_domain").text(), "original-bytes");
    assert_eq!(copy.get("additional_source_bytes_read").number(), 0);
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(before.get("payload_bytes_read"), after.get("payload_bytes_read"));
    assert_eq!(before.get("read_calls"), after.get("read_calls"));
}

#[test]
fn pathname_replacement_requires_explicit_reopen_and_raw_identity_is_reversible() {
    let fixture = Fixture::new();
    let name = b"C:\\raw-\xff\n.txt";
    let path = fixture.0.join(OsString::from_vec(name.to_vec()));
    fs::write(&path, b"original").unwrap();
    let mut first = ObservedReaderSession::open(ArenaOwnerId::new(62_001).unwrap(), &fixture.0, name, || false).unwrap();
    let info = json(&first.info(|| false).unwrap());
    assert_eq!(info.get("path").get("encoding").text(), "unix-bytes");
    assert_eq!(info.get("path").get("hex").text(), hex(path.as_os_str().as_bytes()));
    fs::rename(&path, fixture.0.join("moved")).unwrap();
    fs::write(&path, b"replaced").unwrap();
    let still_opened = read(&mut first, 0, 8);
    assert_eq!(still_opened.get("text").text(), "original");
    let mut reopened = ObservedReaderSession::open(ArenaOwnerId::new(62_002).unwrap(), &fixture.0, name, || false).unwrap();
    let current = read(&mut reopened, 0, 8);
    assert_eq!(current.get("text").text(), "replaced");
    assert_ne!(still_opened.get("owner"), current.get("owner"));
    assert_eq!(still_opened.get("path"), current.get("path"));
}

#[test]
fn eviction_is_explicit_and_replay_or_failed_candidates_cannot_evict_or_refresh() {
    let fixture = Fixture::new(); fixture.write(b"retained source bytes");
    let mut reader = fixture.reader();
    for expected in 1..=MAX_OBSERVED_READER_OBSERVATIONS as u64 {
        assert_eq!(read(&mut reader, 0, 8).get("observation").number(), expected);
    }
    assert!(reader.retained_window(1, || false).is_ok());
    assert_eq!(reader.read_window(0, 3, ObservedReaderEncoding::Utf8, || false).err(), Some(ObservedReaderError::InvalidLimits));
    assert!(reader.read_window(u64::MAX, 8, ObservedReaderEncoding::Utf8, || false).is_err());
    assert!(reader.read_window(0, 8, ObservedReaderEncoding::Utf8, || true).err().unwrap().is_canceled());
    assert_eq!(ids(&json(&reader.info(|| false).unwrap())), (1..=8).collect::<Vec<_>>());
    assert!(reader.copy_range(1, 0, 8, || false).is_ok());
    let accepted = read(&mut reader, 0, 8);
    assert_eq!(accepted.get("observation").number(), 12, "failed attempts spend revisions");
    assert_eq!(reader.retained_window(1, || false).err(), Some(ObservedReaderError::MissingObservation));
    assert_eq!(reader.copy_range(1, 0, 8, || false).err(), Some(ObservedReaderError::MissingObservation));
    assert_eq!(ids(&json(&reader.info(|| false).unwrap())), [2, 3, 4, 5, 6, 7, 8, 12]);
}

#[test]
fn early_and_final_delivery_cancellation_preserve_every_accepted_page_and_spent_io() {
    let fixture = Fixture::new(); fixture.write(&vec![b'a'; 4096]);
    let seed = || {
        let mut reader = fixture.reader();
        for _ in 0..8 { read(&mut reader, 0, 8); }
        reader
    };
    let mut baseline = seed();
    let mut gates = 0;
    baseline.read_window(32, 64, ObservedReaderEncoding::Utf8, || { gates += 1; false }).unwrap();
    assert!(gates > 1);
    for stop in [1, gates] {
        let mut reader = seed();
        let before = json(&reader.info(|| false).unwrap());
        let mut seen = 0;
        let error = reader.read_window(32, 64, ObservedReaderEncoding::Utf8, || { seen += 1; seen == stop }).err().unwrap();
        assert!(error.is_canceled());
        assert_eq!(seen, stop);
        let after = json(&reader.info(|| false).unwrap());
        assert_eq!(ids(&before), ids(&after));
        assert_eq!(after.get("payload_bytes_read").number() - before.get("payload_bytes_read").number(),
            if stop == 1 { 0 } else { 80 });
        assert!(reader.copy_range(1, 0, 8, || false).is_ok());
        assert!(reader.retained_window(8, || false).is_ok());
        assert_eq!(read(&mut reader, 32, 64).get("observation").number(), 10);
    }
}

#[test]
fn declared_encoding_scalar_crlf_and_bom_boundaries_keep_exact_original_bytes() {
    let source = "\u{feff}A🦀B\r\né\u{feff}Z";
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let bytes: Vec<u8> = match encoding {
            ObservedReaderEncoding::Utf8 => source.as_bytes().to_vec(),
            ObservedReaderEncoding::Utf16Le => source.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            ObservedReaderEncoding::Utf16Be => source.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        };
        let fixture = Fixture::new(); fixture.write(&bytes);
        let mut reader = fixture.reader();
        for offset in 0..=bytes.len() {
            for width in [4, 7, 17] {
                let page = json(&reader.read_window(offset as u64, width, encoding, || false).unwrap());
                let (start, end) = bounds(page.get("visible_range"));
                assert!(end - start <= width as u64);
                let raw = &bytes[start as usize..end as usize];
                assert_eq!(page.get("original_hex").text(), hex(raw));
                let decoded = match encoding {
                    ObservedReaderEncoding::Utf8 => std::str::from_utf8(raw).unwrap().to_owned(),
                    ObservedReaderEncoding::Utf16Le | ObservedReaderEncoding::Utf16Be => {
                        assert_eq!(raw.len() % 2, 0);
                        let units: Vec<u16> = raw.chunks_exact(2).map(|pair| if encoding == ObservedReaderEncoding::Utf16Le {
                            u16::from_le_bytes([pair[0], pair[1]])
                        } else { u16::from_be_bytes([pair[0], pair[1]]) }).collect();
                        String::from_utf16(&units).unwrap()
                    }
                };
                assert_eq!(page.get("text").text(), decoded);
                assert!(!page.get("has_replacements").flag());
                assert_eq!(page.get("encoding_basis").text(), "explicit-for-this-observation");
                let copied = json(&reader.copy_range(page.get("observation").number(), start, end, || false).unwrap());
                assert_eq!(copied.get("original_hex"), page.get("original_hex"));
            }
        }
        let whole = json(&reader.read_window(0, 128, encoding, || false).unwrap());
        assert_eq!(whole.get("text").text(), "A🦀B\r\né\u{feff}Z", "only the real file-header BOM is omitted");
    }
}

#[test]
fn known_early_line_numbers_and_empty_eof_have_no_invented_far_line_index() {
    let fixture = Fixture::new(); fixture.write(b"a\nb\ncccccc\n");
    let mut reader = fixture.reader();
    assert_eq!(read(&mut reader, 2, 4).get("first_physical_line").number(), 2);
    assert_eq!(read(&mut reader, 4, 4).get("first_physical_line").number(), 3);
    assert_eq!(read(&mut reader, 9, 4).get("first_physical_line"), &Json::Null);
    let eof = read(&mut reader, 11, 4);
    assert_eq!(eof.get("text").text(), "");
    assert_eq!(eof.get("first_physical_line"), &Json::Null);
    assert_eq!(eof.get("next_offset"), &Json::Null);
    fixture.write(b"");
    let response = reader.read_window(0, 4, ObservedReaderEncoding::Utf8, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let empty = json(&response);
    assert_eq!(empty.get("captured_bytes").number(), 0);
    assert_eq!(empty.get("observation_read_calls").number(), 0);
    assert_eq!(empty.get("text").text(), "");
    assert_eq!(empty.get("first_physical_line"), &Json::Null);
    assert!(empty.get("whole_file_observed").flag());
    assert!(!empty.get("whole_file_immutable").flag());
    assert_eq!(empty.get("extent_sha256").text(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
}

#[test]
fn maximum_window_clips_expanded_scalar_start_and_copy_refuses_unknown_holes() {
    let fixture = Fixture::new();
    let mut bytes = "🦀".as_bytes().to_vec(); bytes.extend(std::iter::repeat_n(b'a', MAX_OBSERVED_READER_WINDOW_BYTES + 32));
    fixture.write(&bytes);
    let mut reader = fixture.reader();
    let page = read(&mut reader, 2, MAX_OBSERVED_READER_WINDOW_BYTES);
    let (start, end) = bounds(page.get("visible_range"));
    assert_eq!(start, 0);
    assert_eq!(end, MAX_OBSERVED_READER_WINDOW_BYTES as u64);
    assert!(page.get("boundaries_adjusted").flag());
    assert_eq!(page.get("original_hex").text(), hex(&bytes[..end as usize]));
    assert!(reader.copy_range(1, 0, MAX_OBSERVED_READER_WINDOW_BYTES as u64 + 1, || false).is_err());
    assert!(reader.copy_range(1, 9, 2, || false).is_err());
    let far = read(&mut reader, 100, 8);
    let id = far.get("observation").number();
    assert!(reader.copy_range(id, 0, 1, || false).is_err());
    assert!(reader.copy_range(id, 115, 117, || false).is_err());
    assert_eq!(reader.copy_range(u64::MAX, 0, 1, || false).err(), Some(ObservedReaderError::MissingObservation));
    let partial_scalar = json(&reader.copy_range(1, 1, 3, || false).unwrap());
    assert_eq!(partial_scalar.get("original_hex").text(), "9fa6");
}

#[test]
fn path_admission_rejects_traversal_and_symlinks_without_opening_an_ambient_source() {
    let fixture = Fixture::new(); fixture.write(b"source");
    let owner = ArenaOwnerId::new(63_001).unwrap();
    for relative in [b"".as_slice(), b"/source", b"../source", b"./source", b"a/../source", b"a//source", b"source/", b"source\0"] {
        assert_eq!(ObservedReaderSession::open(owner, &fixture.0, relative, || false).err(), Some(ObservedReaderError::InvalidPath));
    }
    assert_eq!(ObservedReaderSession::open(owner, Path::new("."), b"source", || false).err(), Some(ObservedReaderError::InvalidPath));
    let dotted_root = fixture.0.join(".");
    assert_eq!(ObservedReaderSession::open(owner, &dotted_root, b"source", || false).err(), Some(ObservedReaderError::InvalidPath));
    std::os::unix::fs::symlink(fixture.0.join("source"), fixture.0.join("link")).unwrap();
    assert!(ObservedReaderSession::open(owner, &fixture.0, b"link", || false).is_err());
    std::os::unix::fs::symlink(&fixture.0, fixture.0.join("linked-dir")).unwrap();
    assert!(ObservedReaderSession::open(owner, &fixture.0, b"linked-dir/source", || false).is_err());
    assert!(ObservedReaderSession::open(owner, &fixture.0, b"source", || true).err().unwrap().is_canceled());
}

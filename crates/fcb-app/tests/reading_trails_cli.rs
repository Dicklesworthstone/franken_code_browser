#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Read, Write}, path::PathBuf, process::Command,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceBudget, ResourceAllocationId};
use fcb::search::snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData, SnapshotLimits};
use fcb::search::trail::TrailView;
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-trail-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn archive(&self, name: &str, path: &[u8], bytes: &[u8]) -> PathBuf {
        let owner = ArenaOwnerId::new(1).unwrap(); let budget = ResourceBudget::new(owner, ByteLength::new(16 * 1024 * 1024)).unwrap();
        let encoded = SnapshotBytes::encode(owner, true, "trail-test-v1", &[SnapshotEntry { path,
            observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }], SnapshotLimits::default(),
            &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        let destination = self.0.join(name); fs::write(&destination, encoded.bytes()).unwrap(); destination
    }
    fn trail(&self) -> PathBuf { self.0.join("first.fcbt") }
    fn add(&self, archive: &PathBuf, start: u64, end: u64) -> Vec<OsString> {
        vec!["trail".into(), "add".into(), archive.as_os_str().to_owned(), "--member".into(), "a.rs".into(),
            "--start".into(), start.to_string().into(), "--end".into(), end.to_string().into(),
            "--note".into(), "User reasoning, not a compiler fact".into(), "--output".into(), self.trail().into_os_string(), "--json".into()]
    }
    fn read(&self, archive: &PathBuf) -> Vec<OsString> {
        vec!["trail".into(), "read".into(), self.trail().into_os_string(), "--archive".into(), archive.as_os_str().to_owned(), "--json".into()]
    }
    fn inspect(&self) -> Vec<OsString> { vec!["trail".into(), "inspect".into(), self.trail().into_os_string(), "--json".into()] }
    fn export(&self, archive: &PathBuf, destination: &PathBuf) -> Vec<OsString> {
        vec!["trail".into(), "export".into(), self.trail().into_os_string(), "--archive".into(),
            archive.as_os_str().to_owned(), "--output".into(), destination.as_os_str().to_owned(),
            "--format".into(), "json".into(), "--json".into()]
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NeverRead;
impl Read for NeverRead { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("trail commands must not read stdin") } }
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut out = Vec::new(); let mut err = Vec::new();
    let code = run(args, &mut NeverRead, &mut out, &mut err, canceled);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    (code, parse(&out).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out))))
}
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    bytes
}

#[test]
fn exact_utf16_note_reopens_after_archive_moves_without_source_payload_export() {
    let f = Fixture::new(); let archive = f.archive("old.fcbs", b"a.rs", &utf16("head\r\nbanana"));
    let (exit, saved) = invoke(&f.add(&archive, 16, 22), || false);
    assert_eq!(exit, EXIT_OK); assert!(!saved.get("contains_source_payloads").flag());
    assert!(saved.get("contains_user_text").flag()); assert_eq!(saved.get("items").number(), 1);
    let moved = f.0.join("moved.fcbs"); fs::rename(&archive, &moved).unwrap();
    let (exit, read) = invoke(&f.read(&moved), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(read.get("text").text(), "anana");
    assert_eq!(read.get("first_line").number(), 2);
    assert_eq!(read.get("selected_original_hex").text(), "61006e006100");
    assert_eq!(read.get("selection").get("rationale").text(), "User reasoning, not a compiler fact");
    assert_eq!(read.get("selection_window_utf8_range").get("end").number(), 3);
    assert!(!read.get("live_roots_accessed").flag());
}

#[test]
fn append_preserves_all_old_notes_and_digest_ancestry_without_overwrite() {
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"banana");
    let (exit, first) = invoke(&f.add(&archive, 1, 4), || false); assert_eq!(exit, EXIT_OK);
    let original = fs::read(f.trail()).unwrap(); let second = f.0.join("second.fcbt");
    let mut args = f.add(&archive, 3, 6);
    args[10] = "Revisit overlapping range".into(); args[12] = second.clone().into_os_string();
    args.extend([OsString::from("--from"), f.trail().into_os_string()]);
    let (exit, saved) = invoke(&args, || false); assert_eq!(exit, EXIT_OK);
    assert_eq!(saved.get("parent_digest").text(), first.get("trail_digest").text());
    assert_eq!(saved.get("items").number(), 2); assert_eq!(fs::read(f.trail()).unwrap(), original);
    let bytes = fs::read(second).unwrap(); let view = TrailView::open(&bytes, || false).unwrap();
    assert_eq!(view.entry(0).unwrap().rationale, "User reasoning, not a compiler fact");
    assert_eq!(view.entry(1).unwrap().rationale, "Revisit overlapping range");
    let mut inspect = f.inspect(); inspect.extend(["--limit".into(), "0".into()]);
    let (exit, listing) = invoke(&inspect, || false); assert_eq!(exit, EXIT_PARTIAL);
    assert!(listing.get("listing_truncated").flag()); assert!(listing.get("entries").array().is_empty());
}

#[test]
fn wrong_archive_same_native_path_refuses_note_reattachment() {
    let f = Fixture::new(); let old = f.archive("old.fcbs", b"a.rs", b"banana");
    assert_eq!(invoke(&f.add(&old, 1, 4), || false).0, EXIT_OK);
    let new = f.archive("new.fcbs", b"a.rs", b"changed text");
    let (exit, result) = invoke(&f.read(&new), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("error").get("code").text(), "TRAIL_OTHER_ARCHIVE");
    assert_eq!(result.get("effect").text(), "none");
    assert_eq!(invoke(&f.read(&old), || false).0, EXIT_OK);
}

#[test]
fn out_of_bounds_and_existing_destination_never_destroy_user_state() {
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"abc");
    assert_eq!(invoke(&f.add(&archive, 0, 4), || false).0, EXIT_ERROR); assert!(!f.trail().exists());
    assert_eq!(invoke(&f.add(&archive, 0, 3), || false).0, EXIT_OK);
    let before = fs::read(f.trail()).unwrap();
    let (exit, result) = invoke(&f.add(&archive, 1, 2), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("error").get("code").text(), "TRAIL_DESTINATION_EXISTS");
    assert_eq!(result.get("effect").text(), "none"); assert_eq!(fs::read(f.trail()).unwrap(), before);
}

#[test]
fn cancellation_before_after_creation_and_after_full_write_have_distinct_effects() {
    let before = Fixture::new(); let archive = before.archive("a.fcbs", b"a.rs", b"abc");
    let (exit, result) = invoke(&before.add(&archive, 0, 1), || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(result.get("effect").text(), "none"); assert!(!before.trail().exists());
    let created = Fixture::new(); let archive = created.archive("a.fcbs", b"a.rs", b"abc");
    let (exit, result) = invoke(&created.add(&archive, 0, 1), || created.trail().exists());
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(result.get("effect").text(), "destination-created-incomplete");
    assert_eq!(fs::metadata(created.trail()).unwrap().len(), 0);
    let written = Fixture::new(); let archive = written.archive("a.fcbs", b"a.rs", b"abc");
    let (exit, result) = invoke(&written.add(&archive, 0, 1), || fs::metadata(written.trail()).is_ok_and(|m| m.len() > 0));
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(result.get("effect").text(), "complete-file-sync-unconfirmed");
    assert!(TrailView::open(&fs::read(written.trail()).unwrap(), || false).is_ok());
}

#[test]
fn lost_stdout_cannot_undo_a_saved_trail() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::BrokenPipe.into()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"abc"); let mut err = Vec::new();
    assert_eq!(run(&f.add(&archive, 0, 1), &mut NeverRead, &mut Broken, &mut err, || false), EXIT_ERROR);
    assert!(String::from_utf8(err).unwrap().contains("effect=complete-file-sync-requested"));
    assert!(TrailView::open(&fs::read(f.trail()).unwrap(), || false).is_ok());
}

#[test]
fn raw_native_name_raw_selection_and_terminal_control_note_are_preserved_safely() {
    let f = Fixture::new(); let archive = f.archive("a.fcbs", &[255], b"a\xffz");
    let mut args = f.add(&archive, 1, 2); args[3] = "--member-hex".into(); args[4] = "ff".into();
    args[10] = "\u{001b}[31m\u{202e}user note".into();
    assert_eq!(invoke(&args, || false).0, EXIT_OK);
    let mut read = f.read(&archive); read.push("--raw".into());
    let (exit, result) = invoke(&read, || false); assert_eq!(exit, EXIT_OK);
    assert_eq!(result.get("selected_original_hex").text(), "ff");
    assert_eq!(result.get("selection").get("path").get("hex").text(), "ff");
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    assert_eq!(run(&["trail".into(), "inspect".into(), f.trail().into_os_string()], &mut NeverRead, &mut stdout, &mut stderr, || false), EXIT_OK);
    let text = String::from_utf8(stdout).unwrap(); assert!(!text.contains('\u{001b}')); assert!(!text.contains('\u{202e}'));
}

#[test]
fn bounded_preview_and_empty_file_bookmarks_are_not_silently_reinterpreted() {
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"abcdefghijkl");
    assert_eq!(invoke(&f.add(&archive, 0, 12), || false).0, EXIT_OK);
    let mut args = f.read(&archive); args.extend(["--raw".into(), "--bytes".into(), "4".into()]);
    let (exit, result) = invoke(&args, || false); assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(result.get("selected_original_hex").text(), "61626364"); assert!(result.get("selected_bytes_truncated").flag());
    let f = Fixture::new(); let archive = f.archive("empty.fcbs", b"a.rs", b"");
    assert_eq!(invoke(&f.add(&archive, 0, 0), || false).0, EXIT_OK);
    let (exit, result) = invoke(&f.read(&archive), || false); assert_eq!(exit, EXIT_OK);
    assert_eq!(result.get("text").text(), ""); assert_eq!(result.get("first_line").number(), 1);
}

#[test]
fn actual_binary_follows_a_saved_trail_and_rejects_corruption() {
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"banana");
    let save = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.add(&archive, 3, 6)).output().unwrap();
    assert_eq!(save.status.code(), Some(EXIT_OK as i32));
    let read = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.read(&archive)).output().unwrap();
    assert_eq!(read.status.code(), Some(EXIT_OK as i32));
    assert_eq!(parse(&read.stdout).unwrap().get("text").text(), "ana");
    let mut bad = fs::read(f.trail()).unwrap(); bad[30] ^= 1; fs::write(f.trail(), bad).unwrap();
    let rejected = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.read(&archive)).output().unwrap();
    assert_eq!(rejected.status.code(), Some(EXIT_ERROR as i32));
    assert_eq!(parse(&rejected.stdout).unwrap().get("status").text(), "error");
}

#[test]
fn exported_pack_preserves_original_bytes_native_path_and_full_provenance() {
    let f = Fixture::new();
    let archive = f.archive("raw.fcbs", &[255, b'.', b'r', b's'], b"\xff\xfea\0\x00\xd8z\0");
    let mut add = f.add(&archive, 3, 7);
    add[3] = "--member-hex".into(); add[4] = "ff2e7273".into();
    add[10] = "用户 note 😀\n\u{001b}[2J".into();
    let (exit, trail) = invoke(&add, || false); assert_eq!(exit, EXIT_OK);
    let destination = f.0.join("evidence.json");
    let (exit, receipt) = invoke(&f.export(&archive, &destination), || false);
    assert_eq!(exit, EXIT_OK); assert!(receipt.get("complete").flag());
    assert!(receipt.get("contains_source_payloads").flag()); assert!(!receipt.get("redacted").flag());
    let bytes = fs::read(&destination).unwrap(); let text = std::str::from_utf8(&bytes).unwrap();
    assert_eq!(receipt.get("export_bytes").number(), bytes.len() as u64);
    assert_eq!(receipt.get("export_characters").number(), text.chars().count() as u64);
    assert!(bytes.len() > text.chars().count(), "counts must distinguish non-ASCII metadata bytes from scalars");
    let pack = parse(&bytes).unwrap();
    assert_eq!(pack.get("schema").text(), "fcb.trail-source-pack/1");
    assert_eq!(pack.get("trail_digest").text(), trail.get("trail_digest").text());
    assert_eq!(pack.get("source_representation").text(), "original-bytes-lowercase-hex");
    assert!(!pack.get("live_roots_accessed").flag()); assert!(!pack.get("payload_truncated").flag());
    let row = &pack.get("entries").array()[0];
    assert_eq!(row.get("selected_original_hex").text(), "0000d87a");
    assert_eq!(row.get("readiness").text(), "digest-verified");
    let selection = row.get("selection");
    assert_eq!(selection.get("path").get("hex").text(), "ff2e7273");
    assert_eq!(selection.get("rationale").text(), "用户 note 😀\n\u{001b}[2J");
    assert_eq!(selection.get("original_range").get("start").number(), 3);
    assert_eq!(selection.get("original_range").get("end").number(), 7);
    assert_eq!(selection.get("source_length").number(), 8);
    assert_eq!(selection.get("archive_digest").text(), pack.get("archive").get("digest").text());
    assert_eq!(pack.get("coverage").get("source_bytes_read").number(), 8);
}

#[test]
fn export_wrong_archive_and_source_limit_produce_visible_partial_evidence() {
    let f = Fixture::new(); let archive = f.archive("old.fcbs", b"a.rs", b"banana");
    assert_eq!(invoke(&f.add(&archive, 1, 4), || false).0, EXIT_OK);
    let changed = f.archive("new.fcbs", b"a.rs", b"edited");
    let wrong = f.0.join("wrong.json");
    let (exit, receipt) = invoke(&f.export(&changed, &wrong), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!receipt.get("complete").flag());
    let pack = parse(&fs::read(&wrong).unwrap()).unwrap();
    let row = &pack.get("entries").array()[0];
    assert_eq!(row.get("reason").text(), "TRAIL_OTHER_ARCHIVE");
    assert!(matches!(row.get("selected_original_hex"), Json::Null));
    assert_eq!(pack.get("coverage").get("source_bytes_read").number(), 0);
    let limited = f.0.join("source-limited.json"); let mut args = f.export(&archive, &limited);
    args.extend(["--max-source-bytes".into(), "5".into()]);
    assert_eq!(invoke(&args, || false).0, EXIT_PARTIAL);
    let pack = parse(&fs::read(&limited).unwrap()).unwrap();
    assert_eq!(pack.get("entries").array()[0].get("reason").text(), "TRAIL_EXPORT_SOURCE_BYTE_LIMIT");
    assert_eq!(pack.get("coverage").get("source_bytes_read").number(), 0);
    let correct = f.0.join("correct.json");
    assert_eq!(invoke(&f.export(&archive, &correct), || false).0, EXIT_OK);
    assert_eq!(parse(&fs::read(correct).unwrap()).unwrap().get("entries").array()[0].get("selected_original_hex").text(), "616e61");
}

#[test]
fn export_byte_and_scalar_limits_include_metadata_and_omit_whole_payloads() {
    let f = Fixture::new(); let payload = vec![b'x'; 8000];
    let archive = f.archive("large.fcbs", b"a.rs", &payload);
    assert_eq!(invoke(&f.add(&archive, 0, payload.len() as u64), || false).0, EXIT_OK);
    for (name, option, reason) in [("bytes.json", "--max-output-bytes", "TRAIL_EXPORT_OUTPUT_BYTE_LIMIT"),
        ("characters.json", "--max-output-chars", "TRAIL_EXPORT_OUTPUT_CHARACTER_LIMIT")] {
        let destination = f.0.join(name); let mut args = f.export(&archive, &destination);
        args.extend([option.into(), "8192".into()]);
        let (exit, receipt) = invoke(&args, || false); assert_eq!(exit, EXIT_PARTIAL);
        let bytes = fs::read(&destination).unwrap(); let text = std::str::from_utf8(&bytes).unwrap();
        if option == "--max-output-bytes" { assert!(bytes.len() <= 8192); }
        else { assert!(text.chars().count() <= 8192); }
        assert_eq!(receipt.get("export_bytes").number(), bytes.len() as u64);
        let pack = parse(&bytes).unwrap(); let row = &pack.get("entries").array()[0];
        assert_eq!(row.get("reason").text(), reason);
        assert!(matches!(row.get("selected_original_hex"), Json::Null));
        assert_eq!(row.get("selection").get("rationale").text(), "User reasoning, not a compiler fact");
        assert_eq!(pack.get("coverage").get("source_bytes_read").number(), 0,
            "cannot spend member-load work on a payload that cannot be exported");
    }
    let rejected = f.0.join("too-small.json"); let mut args = f.export(&archive, &rejected);
    args.extend(["--max-output-bytes".into(), "64".into()]);
    let (exit, receipt) = invoke(&args, || false); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(receipt.get("effect").text(), "none"); assert!(!rejected.exists());
}

#[test]
fn zero_export_entry_limit_preserves_an_explicit_omitted_tail_without_payload_reads() {
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"abc");
    assert_eq!(invoke(&f.add(&archive, 0, 3), || false).0, EXIT_OK);
    let destination = f.0.join("limited.json"); let mut args = f.export(&archive, &destination);
    args.extend(["--limit".into(), "0".into()]);
    let (exit, receipt) = invoke(&args, || false); assert_eq!(exit, EXIT_PARTIAL);
    assert!(!receipt.get("contains_source_payloads").flag());
    let pack = parse(&fs::read(destination).unwrap()).unwrap();
    assert!(pack.get("entries").array().is_empty());
    assert_eq!(pack.get("omitted_tail").get("first_item").number(), 1);
    assert_eq!(pack.get("omitted_tail").get("count").number(), 1);
    assert_eq!(pack.get("coverage").get("source_bytes_read").number(), 0);
}

#[test]
fn export_never_overwrites_and_cancellation_preserves_the_reported_file_effect() {
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"abc");
    assert_eq!(invoke(&f.add(&archive, 0, 3), || false).0, EXIT_OK);
    let existing = f.0.join("existing.json"); fs::write(&existing, b"user state").unwrap();
    let (exit, receipt) = invoke(&f.export(&archive, &existing), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(receipt.get("error").get("code").text(), "TRAIL_DESTINATION_EXISTS");
    assert_eq!(fs::read(&existing).unwrap(), b"user state"); assert_eq!(receipt.get("effect").text(), "none");
    let untouched = f.0.join("untouched.json");
    let (exit, receipt) = invoke(&f.export(&archive, &untouched), || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(receipt.get("effect").text(), "none"); assert!(!untouched.exists());
    let created = f.0.join("created.json");
    let (exit, receipt) = invoke(&f.export(&archive, &created), || created.exists());
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(receipt.get("effect").text(), "destination-created-incomplete");
    assert_eq!(fs::metadata(&created).unwrap().len(), 0);
    let written = f.0.join("written.json");
    let (exit, receipt) = invoke(&f.export(&archive, &written), || fs::metadata(&written).is_ok_and(|m| m.len() > 0));
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(receipt.get("effect").text(), "complete-file-sync-unconfirmed");
    let pack = parse(&fs::read(&written).unwrap()).unwrap(); assert!(pack.get("complete").flag());
}

#[test]
fn lost_export_receipt_does_not_destroy_the_written_source_pack() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::BrokenPipe.into()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"abc");
    assert_eq!(invoke(&f.add(&archive, 0, 3), || false).0, EXIT_OK);
    let destination = f.0.join("retained.json"); let mut errors = Vec::new();
    assert_eq!(run(&f.export(&archive, &destination), &mut NeverRead, &mut Broken, &mut errors, || false), EXIT_ERROR);
    assert!(String::from_utf8(errors).unwrap().contains("effect=complete-file-sync-requested"));
    let pack = parse(&fs::read(destination).unwrap()).unwrap(); assert!(pack.get("complete").flag());
    assert_eq!(pack.get("entries").array()[0].get("selected_original_hex").text(), "616263");
}

#[test]
fn actual_binary_exports_saved_evidence_and_rejects_bad_format_and_corrupt_archive() {
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"banana");
    assert_eq!(invoke(&f.add(&archive, 3, 6), || false).0, EXIT_OK);
    let destination = f.0.join("binary.json");
    let exported = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.export(&archive, &destination)).output().unwrap();
    assert_eq!(exported.status.code(), Some(EXIT_OK as i32));
    assert_eq!(parse(&fs::read(destination).unwrap()).unwrap().get("entries").array()[0].get("selected_original_hex").text(), "616e61");
    let unsupported = f.0.join("not-markdown.md"); let mut args = f.export(&archive, &unsupported);
    args[8] = "markdown".into(); let (exit, receipt) = invoke(&args, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(receipt.get("error").get("code").text(), "TRAIL_EXPORT_UNSUPPORTED_FORMAT");
    assert!(!unsupported.exists());
    let mut bytes = fs::read(&archive).unwrap(); bytes[40] ^= 1; fs::write(&archive, bytes).unwrap();
    let rejected = f.0.join("corrupt.json");
    let (exit, receipt) = invoke(&f.export(&archive, &rejected), || false); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(receipt.get("effect").text(), "none"); assert!(!rejected.exists());
}

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
    fn append(&self, archive: &PathBuf, previous: &PathBuf, destination: &PathBuf,
        start: u64, end: u64, note: &str) {
        let mut args = self.add(archive, start, end);
        args[10] = note.into(); args[12] = destination.as_os_str().to_owned();
        args.extend(["--from".into(), previous.as_os_str().to_owned()]);
        assert_eq!(invoke(&args, || false).0, EXIT_OK);
    }
    fn export_many(&self, trail: &PathBuf, archives: &[&PathBuf], destination: &PathBuf) -> Vec<OsString> {
        let mut args = vec!["trail".into(), "export".into(), trail.as_os_str().to_owned(),
            "--output".into(), destination.as_os_str().to_owned(), "--json".into()];
        for archive in archives { args.extend(["--archive".into(), archive.as_os_str().to_owned()]); }
        args
    }
    fn edit(&self, trail: &PathBuf, destination: &PathBuf, action: &[&str]) -> Vec<OsString> {
        let mut args = vec!["trail".into(), "edit".into(), trail.as_os_str().to_owned(),
            "--output".into(), destination.as_os_str().to_owned(), "--json".into()];
        args.extend(action.iter().map(OsString::from)); args
    }
    fn mixed_trail(&self) -> (PathBuf, PathBuf, PathBuf) {
        let old = self.archive("old.fcbs", b"a.rs", b"old-one");
        let new = self.archive("new.fcbs", b"a.rs", b"new-two");
        assert_eq!(invoke(&self.add(&old, 0, 3), || false).0, EXIT_OK);
        let second = self.0.join("second.fcbt"); let third = self.0.join("third.fcbt");
        self.append(&new, &self.trail(), &second, 0, 3, "Inspect the changed capture");
        self.append(&old, &second, &third, 0, 3, "Return to the earlier exact selection");
        (old, new, third)
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
fn exported_selected_text_uses_capture_encoding_and_preserves_original_bytes() {
    let text = "A🦀\0\r\n\t\"\\\u{feff}Z";
    let mut big_endian = vec![0xfe, 0xff];
    for unit in text.encode_utf16() { big_endian.extend_from_slice(&unit.to_be_bytes()); }
    let mut utf8_bom = vec![0xef, 0xbb, 0xbf]; utf8_bom.extend_from_slice(text.as_bytes());
    for (source, encoding, bom_omitted) in [
        (text.as_bytes().to_vec(), "UTF-8", false),
        (utf8_bom, "UTF-8 (BOM)", true),
        (utf16(text), "UTF-16LE", true),
        (big_endian, "UTF-16BE", true),
    ] {
        let f = Fixture::new(); let archive = f.archive("source.fcbs", b"a.rs", &source);
        assert_eq!(invoke(&f.add(&archive, 0, source.len() as u64), || false).0, EXIT_OK);
        let destination = f.0.join("text.json");
        let (exit, receipt) = invoke(&f.export(&archive, &destination), || false);
        assert_eq!(exit, EXIT_OK);
        let bytes = fs::read(destination).unwrap(); let pack = parse(&bytes).unwrap();
        let row = &pack.get("entries").array()[0];
        assert_eq!(row.get("selected_text").text(), text);
        assert_eq!(row.get("selected_text_status").text(), "exact");
        assert_eq!(row.get("source_encoding").text(), encoding);
        assert_eq!(row.get("selected_text_bom_omitted").flag(), bom_omitted);
        let hex = source.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        assert_eq!(row.get("selected_original_hex").text(), hex);
        assert_eq!(receipt.get("export_bytes").number(), bytes.len() as u64);
        assert_eq!(receipt.get("export_characters").number(), std::str::from_utf8(&bytes).unwrap().chars().count() as u64);
        assert!(pack.get("complete").flag());
    }
}

#[test]
fn malformed_and_split_selections_keep_exact_hex_with_an_explicit_text_refusal() {
    for (source, start, end, status) in [
        (b"a\xffb".as_slice(), 0, 3, "malformed-source"),
        ("a🦀b".as_bytes(), 2, 5, "invalid-boundary"),
    ] {
        let f = Fixture::new(); let archive = f.archive("source.fcbs", b"a.rs", source);
        assert_eq!(invoke(&f.add(&archive, start, end), || false).0, EXIT_OK);
        let destination = f.0.join("raw.json");
        assert_eq!(invoke(&f.export(&archive, &destination), || false).0, EXIT_OK);
        let pack = parse(&fs::read(destination).unwrap()).unwrap();
        let row = &pack.get("entries").array()[0];
        assert!(matches!(row.get("selected_text"), Json::Null));
        assert_eq!(row.get("selected_text_status").text(), status);
        assert_eq!(row.get("selected_original_hex").text(), source[start as usize..end as usize]
            .iter().map(|byte| format!("{byte:02x}")).collect::<String>());
        assert!(pack.get("complete").flag(), "original-byte evidence is complete despite text refusal");
    }
}

#[test]
fn readable_text_budget_cannot_displace_later_original_evidence_or_notes() {
    let f = Fixture::new(); let source = vec![b'x'; 4000];
    let archive = f.archive("source.fcbs", b"a.rs", &source);
    assert_eq!(invoke(&f.add(&archive, 0, source.len() as u64), || false).0, EXIT_OK);
    let second = f.0.join("second.fcbt");
    f.append(&archive, &f.trail(), &second, 0, source.len() as u64, "Revisit the same exact evidence");
    let destination = f.0.join("bounded.json");
    let mut args = f.export_many(&second, &[&archive], &destination);
    args.extend(["--max-output-bytes".into(), "24000".into(), "--max-output-chars".into(), "24000".into()]);
    let (exit, receipt) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert!(receipt.get("complete").flag());
    let bytes = fs::read(destination).unwrap(); assert!(bytes.len() <= 24000);
    let pack = parse(&bytes).unwrap(); let rows = pack.get("entries").array();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row.get("selected_original_hex").text(), "78".repeat(source.len()));
        assert_eq!(row.get("readiness").text(), "digest-verified");
    }
    assert!(rows.iter().any(|row| row.get("selected_text_status").text() == "output-budget"));
    assert_eq!(rows[1].get("selection").get("rationale").text(), "Revisit the same exact evidence");
    assert_eq!(pack.get("coverage").get("selected_original_bytes").number(), 8000);
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

#[test]
fn mixed_archive_export_routes_by_digest_and_preserves_visit_order_and_repeats() {
    let f = Fixture::new(); let (old, new, trail) = f.mixed_trail();
    let destination = f.0.join("mixed.json");
    // Reverse input order, and use the same member path in both archives. A
    // position/path-based implementation would return the wrong source here.
    let (exit, receipt) = invoke(&f.export_many(&trail, &[&new, &old], &destination), || false);
    assert_eq!(exit, EXIT_OK); assert!(receipt.get("complete").flag());
    let pack = parse(&fs::read(destination).unwrap()).unwrap();
    assert!(matches!(pack.get("archive"), Json::Null));
    assert_eq!(pack.get("archives").array().len(), 2);
    let entries = pack.get("entries").array();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries.iter().map(|entry| entry.get("selected_original_hex").text()).collect::<Vec<_>>(),
        ["6f6c64", "6e6577", "6f6c64"]);
    assert_eq!(entries.iter().map(|entry| entry.get("selection").get("item").number()).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(entries[0].get("selection").get("archive_digest"), entries[2].get("selection").get("archive_digest"));
    assert_ne!(entries[0].get("selection").get("archive_digest"), entries[1].get("selection").get("archive_digest"));
    assert_eq!(entries[1].get("selection").get("rationale").text(), "Inspect the changed capture");
    assert_eq!(entries[2].get("selection").get("rationale").text(), "Return to the earlier exact selection");
    assert_eq!(pack.get("coverage").get("source_bytes_read").number(), 21);
    assert_eq!(pack.get("coverage").get("selected_original_bytes").number(), 9);
    let validation = pack.get("archive_validation");
    let envelope_bytes = fs::metadata(&old).unwrap().len() + fs::metadata(&new).unwrap().len();
    assert_eq!(validation.get("validation_bytes_read").number(), envelope_bytes);
    assert_eq!(validation.get("validation_bytes_admitted").number(), envelope_bytes + 2);
    assert_eq!(validation.get("distinct_archives").number(), 2);
    assert!(validation.get("retained_directory_bytes").number() <= validation.get("max_directory_bytes").number());
    assert!(validation.get("peak_directory_bytes").number() <= validation.get("max_directory_bytes").number());
    assert_eq!(receipt.get("archive_validation"), validation);
}

#[test]
fn omitted_archive_keeps_unavailable_visits_and_source_budget_spans_all_archives() {
    let f = Fixture::new(); let (old, new, trail) = f.mixed_trail();
    let missing = f.0.join("omitted.json");
    let (exit, receipt) = invoke(&f.export_many(&trail, &[&new], &missing), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!receipt.get("complete").flag());
    let pack = parse(&fs::read(missing).unwrap()).unwrap(); let entries = pack.get("entries").array();
    for index in [0, 2] {
        assert_eq!(entries[index].get("readiness").text(), "unavailable");
        assert_eq!(entries[index].get("reason").text(), "TRAIL_OTHER_ARCHIVE");
        assert!(matches!(entries[index].get("selected_original_hex"), Json::Null));
    }
    assert_eq!(entries[1].get("selected_original_hex").text(), "6e6577");
    assert_eq!(entries[2].get("selection").get("rationale").text(), "Return to the earlier exact selection");
    assert_eq!(pack.get("coverage").get("unavailable_entries").number(), 2);
    assert_eq!(pack.get("coverage").get("source_bytes_read").number(), 7);
    assert_eq!(pack.get("archive").get("digest"), pack.get("archives").array()[0].get("digest"));

    let limited = f.0.join("global-source-limit.json");
    let mut args = f.export_many(&trail, &[&old, &new], &limited);
    args.extend(["--max-source-bytes".into(), "14".into()]);
    assert_eq!(invoke(&args, || false).0, EXIT_PARTIAL);
    let pack = parse(&fs::read(limited).unwrap()).unwrap(); let entries = pack.get("entries").array();
    assert_eq!(entries[0].get("selected_original_hex").text(), "6f6c64");
    assert_eq!(entries[1].get("selected_original_hex").text(), "6e6577");
    assert_eq!(entries[2].get("reason").text(), "TRAIL_EXPORT_SOURCE_BYTE_LIMIT");
    assert_eq!(pack.get("coverage").get("verification_bytes_admitted").number(), 14);
    assert_eq!(pack.get("coverage").get("source_bytes_read").number(), 14);
}

#[test]
fn duplicate_archive_digests_share_a_route_but_every_supplied_file_is_validated() {
    let f = Fixture::new(); let archive = f.archive("first.fcbs", b"a.rs", b"abc");
    assert_eq!(invoke(&f.add(&archive, 0, 3), || false).0, EXIT_OK);
    let copy = f.0.join("renamed-copy.fcbs"); fs::copy(&archive, &copy).unwrap();
    let destination = f.0.join("duplicate.json");
    assert_eq!(invoke(&f.export_many(&f.trail(), &[&archive, &copy], &destination), || false).0, EXIT_OK);
    let pack = parse(&fs::read(destination).unwrap()).unwrap();
    assert_eq!(pack.get("archives").array().len(), 1);
    assert_eq!(pack.get("archive_validation").get("supplied_archives").number(), 2);
    assert_eq!(pack.get("archive_validation").get("duplicate_archives").number(), 1);
    assert_eq!(pack.get("archive_validation").get("validation_bytes_read").number(), fs::metadata(&archive).unwrap().len() * 2);
    assert_eq!(pack.get("coverage").get("source_bytes_read").number(), 3);

    let mut corrupt = fs::read(&copy).unwrap(); let last = corrupt.len() - 1; corrupt[last] ^= 1;
    fs::write(&copy, corrupt).unwrap();
    let rejected = f.0.join("corrupt-extra-archive.json");
    let (exit, receipt) = invoke(&f.export_many(&f.trail(), &[&archive, &copy], &rejected), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(receipt.get("effect").text(), "none");
    assert!(!rejected.exists(), "a bad explicit archive must not be treated as an omitted archive");
}

#[test]
fn aggregate_archive_validation_and_archive_count_refuse_before_output_creation() {
    let f = Fixture::new(); let (old, new, trail) = f.mixed_trail();
    let admitted = fs::metadata(&old).unwrap().len() + fs::metadata(&new).unwrap().len() + 2;
    let rejected = f.0.join("validation-over-limit.json");
    let mut args = f.export_many(&trail, &[&old, &new], &rejected);
    args.extend(["--max-validation-bytes".into(), (admitted - 1).to_string().into()]);
    let (exit, receipt) = invoke(&args, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(receipt.get("effect").text(), "none");
    assert_eq!(receipt.get("error").get("code").text(), "TRAIL_EXPORT_VALIDATION_BYTE_LIMIT");
    assert!(!rejected.exists());
    let exact = f.0.join("validation-exact-limit.json");
    let mut args = f.export_many(&trail, &[&old, &new], &exact);
    args.extend(["--max-validation-bytes".into(), admitted.to_string().into()]);
    assert_eq!(invoke(&args, || false).0, EXIT_OK);

    let too_many = f.0.join("too-many-archives.json");
    let (exit, receipt) = invoke(&f.export_many(&trail, &[&old; 9], &too_many), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(receipt.get("error").get("code").text(), "TRAIL_EXPORT_ARCHIVE_LIMIT");
    assert!(!too_many.exists());
}

#[test]
fn retained_archive_directories_share_one_admission_ceiling() {
    let f = Fixture::new();
    // Source payloads are empty; the pressure comes from retained membership
    // directories, which must not get a fresh 64 MiB allowance per archive.
    let paths: Vec<_> = (0..65_536).map(|ordinal| format!("{ordinal:05}.rs")).collect();
    let entries: Vec<_> = paths.iter().map(|path| SnapshotEntry { path: path.as_bytes(),
        observed_bytes: 0, data: SnapshotData::Captured(b"") }).collect();
    let owner = ArenaOwnerId::new(1).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(16 * 1024 * 1024)).unwrap();
    let mut archives = Vec::new();
    for ordinal in 0..4 {
        let encoded = SnapshotBytes::encode(owner, true, &format!("directory-fixture-{ordinal}"), &entries,
            SnapshotLimits::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        let destination = f.0.join(format!("directory-{ordinal}.fcbs"));
        fs::write(&destination, encoded.bytes()).unwrap(); archives.push(destination);
    }
    let mut add = f.add(&archives[0], 0, 0); add[4] = "00000.rs".into();
    assert_eq!(invoke(&add, || false).0, EXIT_OK);
    let permitted = f.0.join("three-directories.json");
    assert_eq!(invoke(&f.export_many(&f.trail(), &[&archives[0], &archives[1], &archives[2]], &permitted), || false).0, EXIT_OK);
    let pack = parse(&fs::read(permitted).unwrap()).unwrap();
    let counters = pack.get("archive_validation");
    assert_eq!(counters.get("distinct_archives").number(), 3);
    assert!(counters.get("peak_directory_bytes").number() <= counters.get("max_directory_bytes").number());
    let refused = f.0.join("four-directories.json");
    let (exit, receipt) = invoke(&f.export_many(&f.trail(),
        &[&archives[0], &archives[1], &archives[2], &archives[3]], &refused), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(receipt.get("effect").text(), "none");
    assert_eq!(receipt.get("error").get("code").text(), "SNAPSHOT_RESOURCE_DENIED");
    assert!(!refused.exists());
}

#[test]
fn actual_binary_exports_both_captures_and_multi_archive_cancellation_preserves_effects() {
    let f = Fixture::new(); let (old, new, trail) = f.mixed_trail();
    let destination = f.0.join("binary-mixed.json");
    let exported = Command::new(env!("CARGO_BIN_EXE_fcb"))
        .args(f.export_many(&trail, &[&new, &old], &destination)).output().unwrap();
    assert_eq!(exported.status.code(), Some(EXIT_OK as i32));
    let pack = parse(&fs::read(&destination).unwrap()).unwrap();
    assert!(pack.get("complete").flag());
    assert_eq!(pack.get("entries").array()[0].get("selected_original_hex").text(), "6f6c64");
    assert_eq!(pack.get("entries").array()[1].get("selected_original_hex").text(), "6e6577");
    let before = fs::read(&destination).unwrap();
    assert_eq!(invoke(&f.export_many(&trail, &[&old, &new], &destination), || false).0, EXIT_ERROR);
    assert_eq!(fs::read(destination).unwrap(), before);

    let untouched = f.0.join("mixed-untouched.json");
    let (exit, receipt) = invoke(&f.export_many(&trail, &[&old, &new], &untouched), || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(receipt.get("effect").text(), "none"); assert!(!untouched.exists());
    let created = f.0.join("mixed-created.json");
    let (exit, receipt) = invoke(&f.export_many(&trail, &[&old, &new], &created), || created.exists());
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(receipt.get("effect").text(), "destination-created-incomplete");
    assert_eq!(fs::metadata(created).unwrap().len(), 0);
    let written = f.0.join("mixed-written.json");
    let (exit, receipt) = invoke(&f.export_many(&trail, &[&old, &new], &written),
        || fs::metadata(&written).is_ok_and(|m| m.len() > 0));
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(receipt.get("effect").text(), "complete-file-sync-unconfirmed");
    assert!(parse(&fs::read(written).unwrap()).unwrap().get("complete").flag());
}

#[test]
fn offline_edits_preserve_mixed_archive_visits_repeats_and_exact_parent_chain() {
    let f = Fixture::new(); let (old, new, original) = f.mixed_trail();
    let before = fs::read(&original).unwrap(); let prior = TrailView::open(&before, || false).unwrap();
    // The supplied archive locations are no longer available. Edits have only
    // the trail reference file and an explicit new destination.
    fs::rename(&old, f.0.join("moved-old.fcbs")).unwrap();
    fs::rename(&new, f.0.join("moved-new.fcbs")).unwrap();
    let renamed = f.0.join("renamed.fcbt");
    let (code, receipt) = invoke(&f.edit(&original, &renamed, &["--title", "A deliberate reading order"]), || false);
    assert_eq!(code, EXIT_OK); assert_eq!(receipt.get("command").text(), "trail-edit");
    assert_eq!(receipt.get("edit").get("kind").text(), "rename");
    assert_eq!(receipt.get("edit").get("title").text(), "A deliberate reading order");
    assert_eq!(receipt.get("parent_digest").text(), prior.digest().to_hex());
    assert!(!receipt.get("source_verified").flag()); assert!(receipt.get("source_anchors_unchanged").flag());
    assert_eq!(receipt.get("archive_payload_bytes_read").number(), 0);
    let renamed_bytes = fs::read(&renamed).unwrap(); let renamed_view = TrailView::open(&renamed_bytes, || false).unwrap();
    assert_eq!(renamed_view.title(), "A deliberate reading order");
    assert_eq!(renamed_view.entries().collect::<Result<Vec<_>, _>>().unwrap(),
        prior.entries().collect::<Result<Vec<_>, _>>().unwrap());

    let annotated = f.0.join("annotated.fcbt"); let note = "Revisit \u{03bb}\n\u{001b}[31muser rationale";
    let (code, receipt) = invoke(&f.edit(&renamed, &annotated, &["--item", "3", "--note", note]), || false);
    assert_eq!(code, EXIT_OK); assert_eq!(receipt.get("edit").get("item").number(), 3);
    assert_eq!(receipt.get("edit").get("rationale").text(), note);
    let annotated_bytes = fs::read(&annotated).unwrap(); let annotated_view = TrailView::open(&annotated_bytes, || false).unwrap();
    assert_eq!(annotated_view.parent_digest(), Some(renamed_view.digest()));
    assert_eq!(annotated_view.entry(2).unwrap().rationale, note);
    assert_eq!(annotated_view.entry(0).unwrap(), prior.entry(0).unwrap());
    assert_eq!(annotated_view.entry(1).unwrap(), prior.entry(1).unwrap());
    assert_eq!(annotated_view.entry(2).unwrap().archive, prior.entry(2).unwrap().archive);
    assert_eq!(annotated_view.entry(2).unwrap().range, prior.entry(2).unwrap().range);

    let moved = f.0.join("moved.fcbt");
    let (code, receipt) = invoke(&f.edit(&annotated, &moved, &["--item", "1", "--to", "3"]), || false);
    assert_eq!(code, EXIT_OK); assert_eq!(receipt.get("edit").get("position").number(), 3);
    let moved_bytes = fs::read(&moved).unwrap(); let moved_view = TrailView::open(&moved_bytes, || false).unwrap();
    assert_eq!(moved_view.parent_digest(), Some(annotated_view.digest()));
    for (to, from) in [1, 2, 0].into_iter().enumerate() {
        assert_eq!(moved_view.entry(to).unwrap(), annotated_view.entry(from).unwrap());
    }
    let restored = f.0.join("restored.fcbt");
    assert_eq!(invoke(&f.edit(&moved, &restored, &["--item", "3", "--to", "1"]), || false).0, EXIT_OK);
    let restored_bytes = fs::read(&restored).unwrap(); let restored_view = TrailView::open(&restored_bytes, || false).unwrap();
    assert_eq!(restored_view.entries().collect::<Result<Vec<_>, _>>().unwrap(),
        annotated_view.entries().collect::<Result<Vec<_>, _>>().unwrap());

    let removed = f.0.join("removed.fcbt");
    let (code, receipt) = invoke(&f.edit(&restored, &removed, &["--item", "2", "--remove"]), || false);
    assert_eq!(code, EXIT_OK); assert_eq!(receipt.get("items").number(), 2);
    let removed_bytes = fs::read(&removed).unwrap(); let removed_view = TrailView::open(&removed_bytes, || false).unwrap();
    assert_eq!(removed_view.parent_digest(), Some(restored_view.digest()));
    assert_eq!(removed_view.entry(0).unwrap(), annotated_view.entry(0).unwrap());
    assert_eq!(removed_view.entry(1).unwrap(), annotated_view.entry(2).unwrap());
    assert_eq!(removed_view.reference_bytes(), 6);
    assert_eq!(fs::read(&original).unwrap(), before);
    assert_eq!(fs::read(&renamed).unwrap(), renamed_bytes);
    assert_eq!(fs::read(&annotated).unwrap(), annotated_bytes);
}

#[test]
fn editing_raw_path_notes_and_removing_the_final_visit_keeps_a_valid_empty_trail() {
    let f = Fixture::new(); let archive = f.archive("raw.fcbs", b"\xff.rs", b"abc");
    let mut add = f.add(&archive, 1, 3); add[3] = "--member-hex".into(); add[4] = "ff2e7273".into();
    assert_eq!(invoke(&add, || false).0, EXIT_OK);
    let source = fs::read(f.trail()).unwrap(); let prior = TrailView::open(&source, || false).unwrap();
    let annotated = f.0.join("empty-note.fcbt");
    assert_eq!(invoke(&f.edit(&f.trail(), &annotated, &["--item", "1", "--note", ""]), || false).0, EXIT_OK);
    let annotated_bytes = fs::read(&annotated).unwrap(); let view = TrailView::open(&annotated_bytes, || false).unwrap();
    let entry = view.entry(0).unwrap();
    assert_eq!(entry.path, b"\xff.rs"); assert_eq!(entry.rationale, "");
    assert_eq!(entry.source, prior.entry(0).unwrap().source);
    assert_eq!(entry.source_length, 3); assert_eq!(entry.range, prior.entry(0).unwrap().range);
    let empty = f.0.join("empty.fcbt");
    assert_eq!(invoke(&f.edit(&annotated, &empty, &["--item", "1", "--remove"]), || false).0, EXIT_OK);
    let empty_bytes = fs::read(&empty).unwrap(); let empty_view = TrailView::open(&empty_bytes, || false).unwrap();
    assert!(empty_view.is_empty()); assert_eq!(empty_view.reference_bytes(), 0);
    assert_eq!(empty_view.parent_digest(), Some(view.digest()));
    let titled = f.0.join("empty-title.fcbt");
    assert_eq!(invoke(&f.edit(&empty, &titled, &["--title", ""]), || false).0, EXIT_OK);
    let titled_bytes = fs::read(&titled).unwrap(); let titled_view = TrailView::open(&titled_bytes, || false).unwrap();
    assert!(titled_view.is_empty()); assert_eq!(titled_view.title(), "");
    assert_eq!(titled_view.parent_digest(), Some(empty_view.digest()));
    let mut append = add; append[12] = f.0.join("append-after-empty.fcbt").into_os_string();
    append.extend(["--from".into(), titled.into_os_string()]);
    assert_eq!(invoke(&append, || false).0, EXIT_OK);
    assert_eq!(fs::read(f.trail()).unwrap(), source);
}

#[test]
fn invalid_edits_and_existing_destinations_never_modify_the_parent_or_create_output() {
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"abc");
    assert_eq!(invoke(&f.add(&archive, 0, 3), || false).0, EXIT_OK);
    let before = fs::read(f.trail()).unwrap(); let destination = f.0.join("not-created.fcbt");
    for action in [
        vec!["--item", "2", "--remove"], vec!["--item", "1", "--to", "2"],
        vec!["--item", "2", "--note", "text"], vec!["--title", "new", "--item", "1", "--remove"],
        vec!["--title", "new", "--archive", "never-opened.fcbs"],
    ] {
        let (code, receipt) = invoke(&f.edit(&f.trail(), &destination, &action), || false);
        assert_eq!(code, EXIT_ERROR); assert_eq!(receipt.get("effect").text(), "none");
        assert!(!destination.exists()); assert_eq!(fs::read(f.trail()).unwrap(), before);
    }
    for (option, value) in [("--title", "\u{03bb}".repeat(129)), ("--note", "\u{03bb}".repeat(2049))] {
        let mut edit = f.edit(&f.trail(), &destination, &[]);
        if option == "--note" { edit.extend(["--item".into(), "1".into()]); }
        edit.extend([option.into(), value.into()]);
        assert_eq!(invoke(&edit, || false).0, EXIT_ERROR); assert!(!destination.exists());
    }
    let (code, receipt) = invoke(&f.edit(&f.trail(), &f.trail(), &["--title", "must not overwrite"]), || false);
    assert_eq!(code, EXIT_ERROR); assert_eq!(receipt.get("error").get("code").text(), "TRAIL_DESTINATION_EXISTS");
    assert_eq!(receipt.get("effect").text(), "none"); assert_eq!(fs::read(f.trail()).unwrap(), before);
}

#[test]
fn edited_trails_report_creation_cancellation_and_lost_response_without_rollback() {
    let f = Fixture::new(); let archive = f.archive("a.fcbs", b"a.rs", b"abc");
    assert_eq!(invoke(&f.add(&archive, 0, 3), || false).0, EXIT_OK);
    let before = fs::read(f.trail()).unwrap();
    for (phase, effect) in ["none", "destination-created-incomplete", "complete-file-sync-unconfirmed"].into_iter().enumerate() {
        let destination = f.0.join(format!("canceled-{phase}.fcbt"));
        let (code, receipt) = invoke(&f.edit(&f.trail(), &destination, &["--title", "new"]), || match phase {
            0 => true, 1 => destination.exists(), _ => fs::metadata(&destination).is_ok_and(|m| m.len() > 0),
        });
        assert_eq!(code, EXIT_CANCELED); assert_eq!(receipt.get("effect").text(), effect);
        match phase {
            0 => assert!(!destination.exists()),
            1 => assert_eq!(fs::metadata(&destination).unwrap().len(), 0),
            _ => assert!(TrailView::open(&fs::read(&destination).unwrap(), || false).is_ok()),
        }
        assert_eq!(fs::read(f.trail()).unwrap(), before);
    }
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::BrokenPipe.into()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let destination = f.0.join("saved-without-reply.fcbt"); let mut error = Vec::new();
    assert_eq!(run(&f.edit(&f.trail(), &destination, &["--title", "Saved revision"]),
        &mut NeverRead, &mut Broken, &mut error, || false), EXIT_ERROR);
    assert!(String::from_utf8(error).unwrap().contains("effect=complete-file-sync-requested"));
    let bytes = fs::read(destination).unwrap(); let saved = TrailView::open(&bytes, || false).unwrap();
    assert_eq!(saved.title(), "Saved revision"); assert_eq!(fs::read(f.trail()).unwrap(), before);
}

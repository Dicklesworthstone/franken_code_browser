#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Exercise explicit source-pack writes through the actual framed CLI boundary.
//! Fixtures remain available for diagnosis; execution belongs to strict RCH.
mod support;

use support::{Json, parse};
use std::{ffi::OsString, fs, io::{self, Write}, path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange, store::envelope::Sha256};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use fcb_app::host::{desk::{DeskSession, DeskLimits, DeskCommand},
    observed_reader::{ObservedReaderSession, ObservedReaderEncoding}};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-desk-pack-cli-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn args() -> Vec<OsString> { vec!["desk".into(), "--stdio".into()] }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn records(bytes: &[u8]) -> Vec<Json> {
    bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()).map(|line| {
        let row = parse(line).unwrap(); assert_eq!(row.get("schema").text(), "fcb.desk-stdio/1"); row
    }).collect()
}
fn invoke(script: &str) -> (u8, Vec<Json>, Vec<u8>) {
    let mut input = script.as_bytes(); let mut output = Vec::new(); let mut errors = Vec::new();
    let exit = run(&args(), &mut input, &mut output, &mut errors, || false);
    (exit, records(&output), errors)
}
fn annotated(source: &Path, end: usize, body: &str) -> String {
    format!("0\topen\t{}\n1\tselect\t1\t0\t{end}\n2\tannotate-hex\t1\t{}\t{}\n",
        source.display(), hex(b"Evidence"), hex(body.as_bytes()))
}
fn check_document(receipt: &Json, path: &Path) -> Json {
    let bytes = fs::read(path).unwrap();
    assert_eq!(receipt.get("bytes_written").number(), bytes.len() as u64);
    assert_eq!(receipt.get("export_bytes").number(), bytes.len() as u64);
    assert_eq!(receipt.get("export_scalars").number(), std::str::from_utf8(&bytes).unwrap().chars().count() as u64);
    assert_eq!(receipt.get("export_digest").text(), Sha256::digest(&bytes).to_hex());
    assert!(bytes.len() as u64 <= receipt.get("budgets").get("output_bytes").number());
    assert!(receipt.get("export_scalars").number() <= receipt.get("budgets").get("output_scalars").number());
    parse(&bytes).unwrap()
}

#[test]
fn export_keeps_original_selections_and_full_notes_after_live_replacement() {
    struct ReplaceAfterNote { bytes: Vec<u8>, flushes: usize, source: PathBuf }
    impl Write for ReplaceAfterNote {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 3 { fs::write(&self.source, b"replacement must never enter export")?; }
            Ok(())
        }
    }
    let root = fixture(); let source = root.join("live.rs"); let destination = root.join("pack.json");
    let selected = "old 🦀\r\n"; let body = "Why e\u{301}?\nFull\0note\t🙂";
    fs::write(&source, format!("prefix\n{selected}private tail")).unwrap();
    let script = format!("0\topen\t{}\n1\tselect\t1\t7\t{}\n2\tannotate-hex\t1\t{}\t{}\n3\texport\t{}\t{}\n3\tquit\n",
        source.display(), 7 + selected.len(), hex(b"Pinned evidence"), hex(body.as_bytes()),
        destination.display(), selected.len());
    let mut input = script.as_bytes(); let mut output = ReplaceAfterNote { bytes: Vec::new(), flushes: 0, source: source.clone() };
    let mut errors = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut output, &mut errors, || false), EXIT_OK);
    assert!(errors.is_empty()); let rows = records(&output.bytes); assert_eq!(rows.len(), 5);
    let exported = &rows[3]; let receipt = exported.get("result");
    assert_eq!(exported.get("effect").text(), "complete-file-and-parent-sync-requested");
    assert_eq!(exported.get("accepted_revision").number(), 3);
    assert_eq!(exported.get("last_attempt").number(), 3);
    assert_eq!(receipt.get("command").text(), "export"); assert_eq!(receipt.get("status").text(), "ok");
    assert_eq!(receipt.get("budgets").get("output_bytes").number(), 16 * 1024 * 1024);
    assert_eq!(receipt.get("budgets").get("output_scalars").number(), 16 * 1024 * 1024);
    assert!(!receipt.get("live_roots_accessed").flag()); assert!(!receipt.get("overwritten").flag());
    let pack = check_document(receipt, &destination);
    assert_eq!(pack.get("schema").text(), "fcb.desk-source-pack/1"); assert!(pack.get("complete").flag());
    let entry = &pack.get("entries").array()[0];
    assert_eq!(entry.get("selected_original_hex").text(), hex(selected.as_bytes()));
    assert_eq!(entry.get("selected_text").text(), selected);
    assert_eq!(entry.get("selected_text_status").text(), "exact");
    assert_eq!(entry.get("body").text(), body); assert_eq!(entry.get("label").text(), "Pinned evidence");
    let bytes = fs::read(&destination).unwrap(); let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("private tail")); assert!(!text.contains("replacement must never enter export"));
    assert_eq!(fs::read(&source).unwrap(), b"replacement must never enter export");
}

#[test]
fn restored_complete_and_observed_evidence_export_without_live_roots() {
    let root = fixture(); let source = root.join("complete.rs"); let observed = root.join("observed.txt");
    let checkpoint = root.join("desk.fcbk"); let destination = root.join("mixed.json");
    fs::write(&source, b"saved").unwrap();
    let encoded = "\u{feff}A🦀B\r\n".encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<_>>();
    fs::write(&observed, &encoded).unwrap();
    let mut desk = DeskSession::new(ArenaOwnerId::new(88101).unwrap(), DeskLimits::default()).unwrap();
    let pane = desk.open_file(0, 1, &source, || false).unwrap().active.unwrap();
    let range = |start, end| ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap();
    desk.apply(1, 2, DeskCommand::Navigate { pane, offset: 0, selection: Some(range(0, 5)) }, || false).unwrap();
    desk.apply(2, 3, DeskCommand::Annotate { pane, label: "Whole capture".into(), body: "First note".into() }, || false).unwrap();
    let mut reader = ObservedReaderSession::open_file(ArenaOwnerId::new(88102).unwrap(), &observed, || false).unwrap();
    let response = reader.read_window(0, encoded.len(), ObservedReaderEncoding::Utf16Be, || false).unwrap();
    let observation = parse(response.as_str().as_bytes()).unwrap().get("observation").number();
    let passage = desk.import_observation(3, 4, &mut reader, observation, range(4, 8), || false).unwrap().passage;
    desk.edit_observed_passage(4, 5, passage, 1, "Observed crab", "Second\nfull\0note", || false).unwrap();
    assert_eq!(desk.save_checkpoint(5, &checkpoint, 64, || false).error(), None);
    drop(reader); drop(desk);
    for path in [&source, &observed] {
        fs::rename(path, path.with_extension("moved")).unwrap(); fs::create_dir(path).unwrap();
    }
    let (exit, rows, errors) = invoke(&format!("0\trestore\t{}\n1\texport\t{}\t9\n1\tquit\n",
        checkpoint.display(), destination.display()));
    assert_eq!(exit, EXIT_OK); assert!(errors.is_empty()); assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get("result").get("initial_source_bytes_read").number(), 0);
    assert_eq!(rows[1].get("accepted_revision").number(), 1);
    let receipt = rows[1].get("result"); let pack = check_document(receipt, &destination);
    assert_eq!(receipt.get("pack_schema").text(), "fcb.desk-source-pack/2");
    assert!(receipt.get("observed_passages_included").flag()); assert!(!receipt.get("live_roots_accessed").flag());
    assert_eq!(receipt.get("coverage").get("selected_original_bytes").number(), 9);
    assert_eq!(receipt.get("coverage").get("exported_selections").number(), 2);
    let entries = pack.get("entries").array(); assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].get("capture_kind").text(), "complete-capture");
    assert_eq!(entries[0].get("selected_text").text(), "saved"); assert_eq!(entries[0].get("body").text(), "First note");
    assert_eq!(entries[1].get("capture_kind").text(), "observed-extent");
    assert_eq!(entries[1].get("payload_scope").text(), "selected-range-only");
    assert_eq!(entries[1].get("selected_original_hex").text(), "d83edd80");
    assert_eq!(entries[1].get("selected_text").text(), "🦀");
    assert_eq!(entries[1].get("body").text(), "Second\nfull\0note");
    assert_eq!(entries[1].get("observation").get("encoding").text(), "utf16be");
    assert!(!entries[1].get("observation").get("source_reopened").flag());
}

#[test]
fn raw_destination_paths_and_malformed_selected_bytes_remain_exact() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let root = fixture(); let source = root.join(OsString::from_vec(b"source\t\xff".to_vec()));
    let destination = root.join(OsString::from_vec(b"pack\n\t\xff.json".to_vec())); fs::write(&source, [0xff, b'x']).unwrap();
    let (exit, rows, errors) = invoke(&format!("0\topen-hex\t{}\n1\tselect\t1\t0\t2\n2\tbookmark\t1\tRaw evidence\n3\texport-hex\t{}\t2\t65536\t65536\n3\tquit\n",
        hex(source.as_os_str().as_bytes()), hex(destination.as_os_str().as_bytes())));
    assert_eq!(exit, EXIT_OK); assert!(errors.is_empty());
    let pack = check_document(rows[3].get("result"), &destination); let entry = &pack.get("entries").array()[0];
    assert_eq!(entry.get("selected_original_hex").text(), "ff78");
    assert_eq!(entry.get("selected_text"), &Json::Null);
    assert_eq!(entry.get("selected_text_status").text(), "malformed-source");
}

#[test]
fn explicit_source_and_serialized_budgets_keep_whole_notes_with_omission_reasons() {
    let root = fixture(); let source = root.join("source"); fs::write(&source, vec![0xff; 6000]).unwrap();
    let body = "é\0\n".repeat(100);
    for (ordinal, (source_limit, bytes, scalars, reason)) in [
        (0, 65536, 65536, "DESK_EXPORT_SOURCE_BYTE_LIMIT"),
        (6000, 12288, 65536, "DESK_EXPORT_OUTPUT_BYTE_LIMIT"),
        (6000, 65536, 12288, "DESK_EXPORT_OUTPUT_SCALAR_LIMIT"),
    ].into_iter().enumerate() {
        let destination = root.join(format!("bounded-{ordinal}.json"));
        let script = format!("{}3\texport\t{}\t{source_limit}\t{bytes}\t{scalars}\n3\tquit\n",
            annotated(&source, 6000, &body), destination.display());
        let (exit, rows, errors) = invoke(&script); assert_eq!(exit, EXIT_PARTIAL); assert!(errors.is_empty());
        assert_eq!(rows[3].get("exit_code").number(), u64::from(EXIT_PARTIAL));
        let receipt = rows[3].get("result"); assert_eq!(receipt.get("status").text(), "ok");
        assert!(!receipt.get("complete").flag()); assert!(!receipt.get("contains_source_payloads").flag());
        let pack = check_document(receipt, &destination); let entry = &pack.get("entries").array()[0];
        assert_eq!(entry.get("body").text(), body); assert_eq!(entry.get("reason").text(), reason);
        assert_eq!(entry.get("selected_original_hex"), &Json::Null); assert!(!pack.get("payload_truncated").flag());
    }
    let destination = root.join("metadata-does-not-fit.json");
    let (exit, rows, _) = invoke(&format!("{}3\texport\t{}\t0\t4096\t4096\n3\tcopy\t1\n3\tquit\n",
        annotated(&source, 6000, &"\0".repeat(8192)), destination.display()));
    assert_eq!(exit, EXIT_ERROR); assert!(!destination.exists()); assert_eq!(rows[3].get("effect").text(), "none");
    assert_eq!(rows[3].get("result").get("error").get("code").text(), "CLI_OUTPUT_LIMIT");
    assert_eq!(rows[4].get("result").get("original_hex").text(), "ff".repeat(6000));
    assert_eq!(rows[5].get("accepted_revision").number(), 3);
}

#[test]
fn malformed_requests_invalid_limits_stale_reviews_and_existing_paths_never_write() {
    let root = fixture(); let destination = root.join("never.json");
    for suffix in ["", "\t0\t4096", "\t0\t4096\t4096\textra", "\t-1", "\t+1", "\t01",
        "\t18446744073709551616", "\t0\t+4096\t4096"] {
        let (exit, rows, errors) = invoke(&format!("0\texport\t{}{suffix}\n0\tquit\n", destination.display()));
        assert_eq!(exit, EXIT_ERROR); assert!(errors.is_empty()); assert!(!destination.exists());
        assert_eq!(rows[0].get("effect").text(), "none"); assert_ne!(rows[0].get("error"), &Json::Null);
        assert_eq!(rows[1].get("accepted_revision").number(), 0);
    }
    for caps in ["0\t0\t4096", "0\t4096\t0", "0\t16777217\t4096", "0\t4096\t16777217"] {
        let (exit, rows, _) = invoke(&format!("0\texport\t{}\t{caps}\n0\tquit\n", destination.display()));
        assert_eq!(exit, EXIT_ERROR); assert!(!destination.exists());
        assert_eq!(rows[0].get("result").get("error").get("code").text(), "DESK_INVALID_LIMITS");
        assert_eq!(rows[0].get("effect").text(), "none");
    }
    let (exit, rows, _) = invoke(&format!("0\texport\t{}\t0", destination.display()));
    assert_eq!(exit, EXIT_ERROR); assert!(!destination.exists());
    assert_eq!(rows[0].get("error").get("code").text(), "DESK_FRAME_INCOMPLETE");
    let existing = root.join("existing.json"); fs::write(&existing, b"prior evidence").unwrap();
    let (exit, rows, _) = invoke(&format!("1\texport\t{}\t0\n0\texport\t{}\t0\n0\texport\t{}\t0\n0\texport\t{}\t0\n0\tquit\n",
        destination.display(), existing.display(), destination.display(), destination.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(fs::read(&existing).unwrap(), b"prior evidence");
    assert_eq!(rows[0].get("error").get("code").text(), "DESK_STALE_REVISION");
    for index in [1, 3] {
        assert_eq!(rows[index].get("result").get("error").get("code").text(), "DESK_SAVE_DESTINATION_EXISTS");
        assert_eq!(rows[index].get("effect").text(), "none");
    }
    check_document(rows[2].get("result"), &destination);
}

#[test]
fn cancellation_after_creation_or_a_written_prefix_reports_the_effect_and_stops() {
    for after_prefix in [false, true] {
        let root = fixture(); let source = root.join("source"); fs::write(&source, vec![b'a'; 40_000]).unwrap();
        let destination = root.join("interrupted.json"); let never = root.join("never.json");
        let script = format!("{}3\texport\t{}\t40000\n3\texport\t{}\t40000\n",
            annotated(&source, 40_000, "Exact evidence"), destination.display(), never.display());
        let mut input = script.as_bytes(); let mut output = Vec::new(); let mut errors = Vec::new();
        let exit = run(&args(), &mut input, &mut output, &mut errors, || {
            if after_prefix { fs::metadata(&destination).is_ok_and(|m| m.len() > 0) } else { destination.exists() }
        });
        assert_eq!(exit, EXIT_CANCELED); assert!(errors.is_empty()); assert!(!never.exists());
        let rows = records(&output); assert_eq!(rows.len(), 4); let receipt = rows[3].get("result");
        assert_eq!(rows[3].get("effect").text(), "destination-created-incomplete");
        assert_eq!(receipt.get("status").text(), "error");
        assert_eq!(receipt.get("error").get("code").text(), "DESK_CHECKPOINT_CANCELED");
        let written = receipt.get("bytes_written").number();
        assert_eq!(written, if after_prefix { 64 * 1024 } else { 0 });
        assert_eq!(written, fs::metadata(&destination).unwrap().len());
        assert!(receipt.get("export_bytes").number() > written);
        assert_eq!(rows[3].get("accepted_revision").number(), 3);
    }
}

#[test]
fn cancellation_racing_a_completed_export_preserves_its_receipt() {
    struct CancelOnExport<'a> { bytes: Vec<u8>, flag: &'a AtomicBool }
    impl Write for CancelOnExport<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            if self.bytes.windows(b"\"command\":\"export\"".len()).any(|part| part == b"\"command\":\"export\"") {
                self.flag.store(true, Ordering::Release);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let root = fixture(); let destination = root.join("complete.json"); let flag = AtomicBool::new(false);
    let script = format!("0\texport\t{}\t0\n0\tquit\n", destination.display()); let mut input = script.as_bytes();
    let mut output = CancelOnExport { bytes: Vec::new(), flag: &flag }; let mut errors = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut output, &mut errors, || flag.load(Ordering::Acquire)), EXIT_CANCELED);
    let rows = records(&output.bytes); assert_eq!(rows.len(), 2); assert!(errors.is_empty());
    assert_eq!(rows[0].get("exit_code").number(), 0);
    assert_eq!(rows[0].get("effect").text(), "complete-file-and-parent-sync-requested");
    assert_eq!(rows[0].get("result").get("status").text(), "ok");
    assert_eq!(rows[1].get("exit_code").number(), u64::from(EXIT_CANCELED));
    check_document(rows[0].get("result"), &destination);
}

#[test]
fn broken_stdout_after_export_keeps_the_file_and_reports_the_effect_without_replay() {
    struct BrokenAfterNote { flushes: usize }
    impl Write for BrokenAfterNote {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.flushes == 3 { Err(io::ErrorKind::BrokenPipe.into()) } else { Ok(bytes.len()) }
        }
        fn flush(&mut self) -> io::Result<()> { self.flushes += 1; Ok(()) }
    }
    let root = fixture(); let source = root.join("source"); fs::write(&source, b"saved evidence").unwrap();
    let destination = root.join("written.json"); let never = root.join("never.json");
    let script = format!("{}3\texport\t{}\t14\n3\texport\t{}\t14\n",
        annotated(&source, 14, "Private rationale"), destination.display(), never.display());
    let mut input = script.as_bytes(); let mut errors = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut BrokenAfterNote { flushes: 0 }, &mut errors, || false), EXIT_ERROR);
    let diagnostic = std::str::from_utf8(&errors).unwrap();
    assert!(diagnostic.contains("DESK_OUTPUT_INTERRUPTED"));
    assert!(diagnostic.contains("effect=complete-file-and-parent-sync-requested"));
    assert!(!diagnostic.contains("saved evidence")); assert!(!diagnostic.contains("Private rationale"));
    assert!(!never.exists()); let pack = parse(&fs::read(&destination).unwrap()).unwrap();
    assert_eq!(pack.get("entries").array()[0].get("selected_text").text(), "saved evidence");
    assert_eq!(pack.get("entries").array()[0].get("body").text(), "Private rationale");
}

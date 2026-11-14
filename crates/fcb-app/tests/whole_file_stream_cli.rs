#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{cell::Cell, ffi::OsString, fs, io::{self, Read, Write}, path::{Path, PathBuf}, rc::Rc,
    sync::atomic::{AtomicU64, Ordering}};
use fcb_app::{run, EXIT_CANCELED, EXIT_ERROR, EXIT_NO_MATCH, EXIT_OK, EXIT_PARTIAL};
use support::{Json, parse};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-ndjson-cli-{}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name); fs::write(&path, bytes).unwrap(); path
    }
}
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NoStdin;
impl Read for NoStdin { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("named search consumed stdin") } }
fn arguments(path: &Path, flags: &[&str], stream: bool) -> Vec<OsString> {
    let mut values = vec!["search".into(), path.as_os_str().to_owned(), "--whole-file".into(),
        if stream { "--stream".into() } else { "--json".into() }];
    values.extend(flags.iter().map(OsString::from)); values
}
fn lines(bytes: &[u8]) -> Vec<Json> {
    assert!(bytes.ends_with(b"\n"), "incomplete record");
    bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty())
        .map(|line| parse(line).unwrap_or_else(|error| panic!("{error}: {:?}", String::from_utf8_lossy(line)))).collect()
}
fn framed(bytes: &[u8]) -> Vec<Json> {
    let records = lines(bytes);
    for (sequence, record) in records.iter().enumerate() {
        assert_eq!(record.get("schema").text(), "fcb.search-stream/1");
        assert_eq!(record.get("sequence").number(), sequence as u64);
        assert_eq!(record.get("owner").text(), "1");
        assert_eq!(record.get("query_generation").text(), "1");
    }
    assert_eq!(records.iter().filter(|record| record.get("type").text() == "terminal").count(), 1);
    assert_eq!(records.last().unwrap().get("type").text(), "terminal");
    if records.len() > 1 {
        assert_eq!(records[0].get("type").text(), "start");
        assert_eq!(records[0].get("identity_scope").text(), "invocation-local");
    }
    records
}
fn invoke(path: &Path, flags: &[&str]) -> (u8, Vec<Json>) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(&arguments(path, flags, true), &mut NoStdin, &mut stdout, &mut stderr, || false);
    assert!(stderr.is_empty(), "{:?}", String::from_utf8_lossy(&stderr));
    (exit, framed(&stdout))
}
fn ordinary(path: &Path, flags: &[&str]) -> (u8, Json) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(&arguments(path, flags, false), &mut NoStdin, &mut stdout, &mut stderr, || false);
    assert!(stderr.is_empty());
    let json = parse(&stdout).unwrap();
    assert_eq!(json.get("schema").text(), "fcb.cli/1");
    (exit, json)
}
fn files(records: &[Json]) -> Vec<&Json> {
    records.iter().filter(|record| record.get("type").text() == "file")
        .map(|record| record.get("file")).collect()
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn progressive_workspace_records_match_the_existing_source_witness_contract() {
    let temp = Temp::new();
    let mut large = vec![b'x'; 16 * 1024 - 2];
    large.extend_from_slice(b"needle");
    large.extend(vec![b'x'; 5 * 1024 * 1024]);
    large.extend_from_slice(b"needle");
    temp.file("a.rs", &large);
    temp.file("b.rs", &utf16("prefix needle 😀", true));
    temp.file("c.rs", &utf16("needle\u{feff} needle", false));
    let flags = ["--workspace", "--text", "needle"];
    let (expected_exit, expected) = ordinary(&temp.0, &flags);
    let (exit, records) = invoke(&temp.0, &flags);
    assert_eq!(exit, expected_exit);
    let actual = files(&records);
    assert_eq!(actual.len(), 3);
    for (actual, expected) in actual.iter().zip(expected.get("files").array()) { assert_eq!(*actual, expected); }
    let terminal = records.last().unwrap();
    for name in ["payload_bytes_read", "read_calls", "matches_seen", "stored_hits", "files_examined",
        "incomplete_files", "unexamined_files", "matching_files", "truncated", "match_count_complete"] {
        assert_eq!(terminal.get(name), expected.get(name), "{name}");
    }
    assert!(terminal.get("scan_complete").flag());
    assert!(terminal.get("results_complete").flag());
    assert_eq!(terminal.get("file_records").number(), 3);
    assert!(records.iter().filter(|record| record.get("type").text() == "progress"
        && record.get("phase").text() == "scanning").count() > 3);
}

#[test]
fn raw_lowercase_identifier_and_expression_use_the_same_completed_reports() {
    let temp = Temp::new();
    let cases = [
        (b"AA\0\xffAA".to_vec(), vec!["--raw-hex", "4100ff"]),
        ("K K k İ i".as_bytes().to_vec(), vec!["--text", "k", "--ignore-case"]),
        (utf16("K K k", true), vec!["--text", "k", "--ignore-case"]),
        (utf16("cancel(); cancelled; \"cancel\";", false), vec!["--identifier", "cancel"]),
        (b"ana ana required".to_vec(), vec!["--query", "ana required -forbidden"]),
        (b"ana forbidden".to_vec(), vec!["--query", "ana -forbidden"]),
    ];
    for (ordinal, (bytes, flags)) in cases.iter().enumerate() {
        let path = temp.file(&format!("case-{ordinal}.rs"), bytes);
        let (expected_exit, expected) = ordinary(&path, flags);
        let (exit, records) = invoke(&path, flags);
        assert_eq!(exit, expected_exit);
        let file = files(&records)[0];
        for name in ["path", "file_id", "revision", "query_generation", "mode", "encoding", "state",
            "whole_file_complete", "count_all_matches", "truncated", "observed_length", "final_length",
            "payload_bytes_read", "scanned_input_bytes", "read_calls", "matches_seen", "stored_hits",
            "retained_witness_bytes", "literal_text", "literal_original_hex", "hits"] {
            assert_eq!(file.get(name), expected.get(name), "{ordinal}: {name}");
        }
    }
}

#[test]
fn partial_expression_never_publishes_candidate_hits_or_counts() {
    let temp = Temp::new();
    let path = temp.file("a.rs", format!("{}required", "ana ".repeat(100)).as_bytes());
    let (exit, records) = invoke(&path, &["--query", "ana required", "--max-scan-bytes", "64"]);
    assert_eq!(exit, EXIT_PARTIAL);
    for record in &records {
        match record.get("type").text() {
            "file" => {
                assert!(record.get("file").get("hits").array().is_empty());
                assert_eq!(record.get("file").get("expression_qualified"), &Json::Null);
                assert_eq!(record.get("file").get("matches_seen").number(), 0);
            }
            "progress" | "terminal" => assert_eq!(record.get("matches_seen").number(), 0),
            _ => {},
        }
    }
    assert!(!records.last().unwrap().get("match_count_complete").flag());
    let absent = temp.0.join("not-opened.rs");
    let (exit, records) = invoke(&absent, &["--query", "needle path:elsewhere"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert!(files(&records).is_empty());
    let terminal = records.last().unwrap();
    assert_eq!(terminal.get("metadata_excluded_files").number(), 1);
    assert_eq!(terminal.get("payload_bytes_read").number(), 0);
    assert!(terminal.get("scan_complete").flag());
}

#[test]
fn source_byte_and_hit_budgets_remain_global_and_exact_limit_is_not_truncation() {
    let temp = Temp::new();
    temp.file("a.rs", b"1234567890");
    temp.file("b.rs", b"1234567890");
    temp.file("c.rs", b"needle");
    let (exit, records) = invoke(&temp.0,
        &["--workspace", "--text", "needle", "--max-scan-bytes", "15"]);
    let terminal = records.last().unwrap();
    assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(terminal.get("payload_bytes_read").number(), 15);
    assert_eq!(terminal.get("files_examined").number(), 2);
    assert_eq!(terminal.get("unexamined_files").number(), 1);
    assert_eq!(terminal.get("stop_reason").text(), "byte-limit");
    assert!(!terminal.get("scan_complete").flag());

    let exact = temp.file("exact.txt", b"aa");
    let (exit, records) = invoke(&exact, &["--text", "a", "--limit", "2"]);
    assert_eq!(exit, EXIT_OK);
    assert!(!records.last().unwrap().get("truncated").flag());
    let excess = temp.file("excess.txt", b"aaa");
    let (exit, records) = invoke(&excess, &["--text", "a", "--limit", "2"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(records.last().unwrap().get("truncated").flag());

    let counted = Temp::new();
    counted.file("a.rs", b"aaaa"); counted.file("b.rs", b"aaa");
    let (exit, records) = invoke(&counted.0,
        &["--workspace", "--text", "a", "--count-all", "--limit", "2"]);
    let terminal = records.last().unwrap();
    assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(terminal.get("matches_seen").number(), 7);
    assert_eq!(terminal.get("stored_hits").number(), 2);
    assert_eq!(terminal.get("delivered_hits").number(), 2);
    assert!(terminal.get("match_count_complete").flag());
    assert!(!terminal.get("results_complete").flag());
    let (exit, records) = invoke(&counted.0,
        &["--workspace", "--text", "a", "--count-all", "--limit", "0"]);
    let terminal = records.last().unwrap();
    assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(terminal.get("matches_seen").number(), 7);
    assert_eq!(terminal.get("stored_hits").number(), 0);
    assert_eq!(terminal.get("delivered_hits").number(), 0);
    assert!(files(&records).iter().all(|file| file.get("hits").array().is_empty()));
    assert!(terminal.get("match_count_complete").flag());
}

#[derive(Clone, Copy)]
enum Action { CloseAfterFile, CancelAfterFile, CancelDuringScan, FailFlushAfterFile }
struct WatchingWriter {
    bytes: Vec<u8>,
    record_start: usize,
    action: Action,
    acted: bool,
    closed: bool,
    failed_offer: Vec<u8>,
    canceled: Rc<Cell<bool>>,
    broken: Rc<Cell<bool>>,
}
impl WatchingWriter {
    fn new(action: Action, canceled: Rc<Cell<bool>>, broken: Rc<Cell<bool>>) -> Self {
        Self { bytes: Vec::new(), record_start: 0, action, acted: false, closed: false,
            failed_offer: Vec::new(), canceled, broken }
    }
}
impl Write for WatchingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(!self.broken.get(), "retried a poisoned writer");
        if self.closed {
            self.failed_offer.extend_from_slice(bytes);
            self.broken.set(true);
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "test consumer closed"));
        }
        self.bytes.extend_from_slice(bytes); Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        let record = parse(&self.bytes[self.record_start..]).unwrap();
        self.record_start = self.bytes.len();
        if self.acted { return Ok(()); }
        let is_file = record.get("type").text() == "file";
        let is_scan = record.get("type").text() == "progress" && record.get("phase").text() == "scanning";
        match self.action {
            Action::CloseAfterFile if is_file => { self.acted = true; self.closed = true; }
            Action::CancelAfterFile if is_file => { self.acted = true; self.canceled.set(true); }
            Action::CancelDuringScan if is_scan => { self.acted = true; self.canceled.set(true); }
            Action::FailFlushAfterFile if is_file => {
                self.acted = true; self.broken.set(true);
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "test flush failure"));
            }
            _ => {},
        }
        Ok(())
    }
}

#[test]
fn a_closed_consumer_is_detected_before_the_next_source_is_opened() {
    let temp = Temp::new();
    temp.file("a.rs", b"needle"); temp.file("b.rs", &vec![b'x'; 1024 * 1024]);
    let canceled = Rc::new(Cell::new(false));
    let broken = Rc::new(Cell::new(false));
    let mut writer = WatchingWriter::new(Action::CloseAfterFile, canceled, broken.clone());
    let mut stderr = Vec::new();
    let exit = run(&arguments(&temp.0, &["--workspace", "--text", "needle"], true),
        &mut NoStdin, &mut writer, &mut stderr, || {
            assert!(!broken.get(), "continued control/scanning after failed delivery"); false
        });
    assert_eq!(exit, EXIT_ERROR);
    let records = lines(&writer.bytes);
    assert_eq!(files(&records).len(), 1);
    assert!(records.iter().all(|record| record.get("type").text() != "terminal"));
    let refused = parse(&writer.failed_offer).unwrap();
    assert_eq!(refused.get("phase").text(), "opening");
    assert_eq!(refused.get("payload_bytes_read").number(), 6);
    assert_eq!(refused.get("current_file").get("revision"), &Json::Null);
    assert!(String::from_utf8(stderr).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
}

#[test]
fn cancellation_between_records_preserves_earlier_results_and_a_truthful_terminal() {
    let temp = Temp::new();
    temp.file("a.rs", b"needle"); temp.file("b.rs", b"needle needle");
    let canceled = Rc::new(Cell::new(false));
    let mut writer = WatchingWriter::new(Action::CancelAfterFile, canceled.clone(), Rc::new(Cell::new(false)));
    let mut stderr = Vec::new();
    let exit = run(&arguments(&temp.0, &["--workspace", "--text", "needle"], true),
        &mut NoStdin, &mut writer, &mut stderr, || canceled.get());
    assert_eq!(exit, EXIT_CANCELED); assert!(stderr.is_empty());
    let records = framed(&writer.bytes);
    assert_eq!(files(&records).len(), 1);
    let terminal = records.last().unwrap();
    assert_eq!(terminal.get("status").text(), "canceled");
    assert_eq!(terminal.get("payload_bytes_read").number(), 6);
    assert_eq!(terminal.get("files_examined").number(), 1);
    assert_eq!(terminal.get("unexamined_files").number(), 1);
    assert!(!terminal.get("match_count_complete").flag());
}

#[test]
fn scan_progress_can_cancel_a_large_file_without_reading_its_tail_or_next_member() {
    let temp = Temp::new();
    temp.file("a.rs", &vec![b'x'; 1024 * 1024]); temp.file("b.rs", b"needle");
    let canceled = Rc::new(Cell::new(false));
    let mut writer = WatchingWriter::new(Action::CancelDuringScan, canceled.clone(), Rc::new(Cell::new(false)));
    let mut stderr = Vec::new();
    let exit = run(&arguments(&temp.0, &["--workspace", "--text", "needle"], true),
        &mut NoStdin, &mut writer, &mut stderr, || canceled.get());
    assert_eq!(exit, EXIT_CANCELED); assert!(stderr.is_empty());
    let records = framed(&writer.bytes);
    assert!(files(&records).is_empty());
    let terminal = records.last().unwrap();
    assert!((1..=16 * 1024).contains(&terminal.get("payload_bytes_read").number()));
    assert_eq!(terminal.get("files_examined").number(), 1);
    assert_eq!(terminal.get("incomplete_files").number(), 1);
    assert_eq!(terminal.get("unexamined_files").number(), 1);
    assert_eq!(terminal.get("matches_seen").number(), 0);
}

#[test]
fn a_failed_flush_after_a_complete_record_still_poisons_the_transport() {
    let temp = Temp::new(); let path = temp.file("a.rs", b"needle");
    let broken = Rc::new(Cell::new(false));
    let mut writer = WatchingWriter::new(Action::FailFlushAfterFile, Rc::new(Cell::new(false)), broken.clone());
    let mut stderr = Vec::new();
    let exit = run(&arguments(&path, &["--text", "needle"], true),
        &mut NoStdin, &mut writer, &mut stderr, || {
            assert!(!broken.get(), "attempted recovery after a failed flush"); false
        });
    assert_eq!(exit, EXIT_ERROR);
    let records = lines(&writer.bytes);
    assert_eq!(files(&records).len(), 1);
    assert!(records.iter().all(|record| record.get("type").text() != "terminal"));
    assert!(!stderr.is_empty());
}

struct PartialWriter { bytes: Vec<u8>, canceled: Rc<Cell<bool>>, cancel: bool, calls: usize }
impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.calls > 1 { return Err(io::Error::new(io::ErrorKind::BrokenPipe, "partial record")); }
        let count = bytes.len().min(7);
        self.bytes.extend_from_slice(&bytes[..count]);
        if self.cancel { self.canceled.set(true); }
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> { panic!("partial record must not be flushed") }
}
#[test]
fn partial_write_cancellation_and_failure_never_append_recovery_json() {
    let temp = Temp::new(); let absent = temp.0.join("not-opened.rs");
    for cancel in [false, true] {
        let canceled = Rc::new(Cell::new(false));
        let mut writer = PartialWriter { bytes: Vec::new(), canceled: canceled.clone(), cancel, calls: 0 };
        let mut stderr = Vec::new();
        let exit = run(&arguments(&absent, &["--text", "needle"], true),
            &mut NoStdin, &mut writer, &mut stderr, || canceled.get());
        assert_eq!(exit, if cancel { EXIT_CANCELED } else { EXIT_ERROR });
        assert_eq!(writer.bytes.len(), 7);
        assert!(!writer.bytes.contains(&b'\n'));
        assert_eq!(writer.calls, if cancel { 1 } else { 2 });
        assert!(String::from_utf8(stderr).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
    }
}

struct InterruptedWriter(usize);
impl Write for InterruptedWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        self.0 += 1; Err(io::Error::new(io::ErrorKind::Interrupted, "no progress"))
    }
    fn flush(&mut self) -> io::Result<()> { panic!("no record delivered") }
}
#[test]
fn interrupted_output_retries_are_bounded_before_source_admission() {
    let temp = Temp::new();
    let mut writer = InterruptedWriter(0); let mut stderr = Vec::new();
    let exit = run(&arguments(&temp.0.join("not-opened.rs"), &["--text", "needle"], true),
        &mut NoStdin, &mut writer, &mut stderr, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(writer.0, 4096);
    assert!(String::from_utf8(stderr).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
}

#[test]
fn an_oversized_file_record_preserves_progress_and_actual_work_in_a_partial_terminal() {
    let temp = Temp::new();
    let needle = "a".repeat(1024);
    let content = format!("{} ", "A".repeat(1024)).repeat(4096);
    let path = temp.file("large-witnesses.rs", content.as_bytes());
    let (exit, records) = invoke(&path,
        &["--text", &needle, "--ignore-case", "--count-all", "--limit", "4096"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(files(&records).is_empty());
    let terminal = records.last().unwrap();
    assert_eq!(terminal.get("stop_reason").text(), "output-limit");
    assert_eq!(terminal.get("error").get("code").text(), "CLI_OUTPUT_LIMIT");
    assert_eq!(terminal.get("payload_bytes_read").number(), content.len() as u64);
    assert_eq!(terminal.get("matches_seen").number(), 4096);
    assert_eq!(terminal.get("stored_hits").number(), 4096);
    assert_eq!(terminal.get("delivered_hits").number(), 0);
    assert!(!terminal.get("results_complete").flag());
    assert!(!terminal.get("match_count_complete").flag());
}

struct RecordCounter { pending: Vec<u8>, bytes: usize, count: u64, files: u64, terminal: Option<Json> }
impl Write for RecordCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(bytes.len() <= 16 * 1024);
        self.bytes += bytes.len(); self.pending.extend_from_slice(bytes); Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        assert!(self.pending.ends_with(b"\n"));
        let record = parse(&self.pending).unwrap();
        assert_eq!(record.get("sequence").number(), self.count);
        self.count += 1;
        match record.get("type").text() {
            "file" => self.files += 1,
            "terminal" => { assert!(self.terminal.is_none()); self.terminal = Some(record); }
            _ => {},
        }
        self.pending.clear(); Ok(())
    }
}
#[test]
fn many_files_can_deliver_more_than_one_ordinary_response_without_accumulating_it() {
    let temp = Temp::new();
    let prefix = "p".repeat(190);
    for ordinal in 0..2500 { temp.file(&format!("{prefix}-{ordinal:04}.rs"), b"x"); }
    let mut writer = RecordCounter { pending: Vec::new(), bytes: 0, count: 0, files: 0, terminal: None };
    let mut stderr = Vec::new();
    let exit = run(&arguments(&temp.0, &["--workspace", "--text", "needle"], true),
        &mut NoStdin, &mut writer, &mut stderr, || false);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(stderr.is_empty());
    assert_eq!(writer.files, 2500);
    assert!(writer.bytes > fcb_app::output::MAX_RESPONSE_BYTES);
    assert!(writer.pending.is_empty());
    let terminal = writer.terminal.unwrap();
    assert!(terminal.get("scan_complete").flag());
    assert_eq!(terminal.get("file_records").number(), 2500);
    assert_eq!(terminal.get("payload_bytes_read").number(), 2500);
}

#[test]
fn argument_errors_use_one_terminal_and_literal_flag_text_preserves_ordinary_json() {
    let temp = Temp::new(); let absent = temp.0.join("not-opened.rs");
    let mut flags = arguments(&absent, &["--text", "needle"], true);
    flags.push("--json".into());
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(&flags, &mut NoStdin, &mut stdout, &mut stderr, || false);
    assert_eq!(exit, EXIT_ERROR); assert!(stderr.is_empty());
    let records = framed(&stdout);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].get("error").get("code").text(), "CLI_INCOMPATIBLE_OPTIONS");
    assert_eq!(records[0].get("payload_bytes_read").number(), 0);
    let path = temp.file("literal.rs", b"--stream");
    let (exit, json) = ordinary(&path, &["--text", "--stream"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(json.get("matches_seen").number(), 1);
}

#[cfg(unix)]
#[test]
fn native_filename_bytes_and_newlines_remain_one_reversible_record() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let temp = Temp::new();
    let name = OsString::from_vec(vec![b'a', 0xff, b'\n', b'"', b'.', b'r', b's']);
    let path = temp.0.join(name); fs::write(&path, b"needle").unwrap();
    let (exit, records) = invoke(&path, &["--text", "needle"]);
    assert_eq!(exit, EXIT_OK);
    let expected: String = path.as_os_str().as_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(files(&records)[0].get("path").get("encoding").text(), "unix-bytes");
    assert_eq!(files(&records)[0].get("path").get("hex").text(), expected);
}

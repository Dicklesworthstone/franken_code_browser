#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{Json, parse};
use std::{ffi::OsString, fs, io::{self, Write}, path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_CANCELED};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-desk-checkpoint-cli-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn args() -> Vec<OsString> { vec!["desk".into(), "--stdio".into()] }
fn records(bytes: &[u8]) -> Vec<Json> {
    bytes.split(|b| *b == b'\n').filter(|s| !s.is_empty()).map(|line| {
        let json = parse(line).unwrap(); assert_eq!(json.get("schema").text(), "fcb.desk-stdio/1"); json
    }).collect()
}
fn invoke(input: &str) -> (u8, Vec<Json>, Vec<u8>) {
    let mut input = input.as_bytes(); let mut output = Vec::new(); let mut errors = Vec::new();
    let exit = run(&args(), &mut input, &mut output, &mut errors, || false);
    (exit, records(&output), errors)
}

#[test]
fn reopen_an_exact_pinned_reading_session_after_working_tree_replacement() {
    let root = fixture(); let source = root.join("live.rs"); let saved = root.join("saved.fcbk");
    fs::write(&source, b"old needle").unwrap();
    let script = format!("0\topen\t{}\n1\tpin\t1\n2\tfind\t1\t1\t10\t100\tneedle\n2\thit\t1\t1\t0\n4\tarrange\t1\t20.5\t30\t700\t500\n5\tbookmark\t1\toriginal evidence\n6\tsave\t{}\t33554432\n6\tquit\n", source.display(), saved.display());
    let (exit, first, errors) = invoke(&script);
    assert_eq!(exit, EXIT_OK); assert!(errors.is_empty()); assert_eq!(first.len(), 8);
    assert_eq!(first[6].get("accepted_revision").number(), 6);
    assert_eq!(first[6].get("effect").text(), "complete-file-and-parent-sync-requested");
    assert_eq!(first[6].get("result").get("source_bytes").number(), 10);
    fs::rename(&source, root.join("old.rs")).unwrap(); fs::write(&source, b"new bytes").unwrap();
    let (exit, second, errors) = invoke(&format!("0\trestore\t{}\n1\tview\t1\n1\tcopy\t1\n1\tstate\n1\tquit\n", saved.display()));
    assert_eq!(exit, EXIT_OK); assert!(errors.is_empty());
    assert_eq!(second[1].get("result").get("text").text(), "needle");
    assert_eq!(second[2].get("result").get("original_hex").text(), "6e6565646c65");
    let state = second[3].get("result");
    assert!(state.get("panes").array()[0].get("pinned").flag());
    assert_eq!(state.get("panes").array()[0].get("position").get("x").text(), "20.5");
    assert_eq!(state.get("panes").array()[0].get("size").get("width").text(), "700");
    assert_eq!(state.get("bookmarks").array()[0].get("label").text(), "original evidence");
    assert_eq!(state.get("initial_source_bytes_read").number(), 0);
    assert_eq!(fs::read(&source).unwrap(), b"new bytes");
}

#[test]
fn both_history_directions_survive_a_new_cli_session() {
    let root = fixture(); let a = root.join("a.rs"); let b = root.join("b.rs"); let saved = root.join("history.fcbk");
    fs::write(&a, b"A").unwrap(); fs::write(&b, b"B").unwrap();
    let (exit, _, _) = invoke(&format!("0\topen\t{}\n1\topen\t{}\n2\tback\n3\tsave\t{}\t2\n3\tquit\n", a.display(), b.display(), saved.display()));
    assert_eq!(exit, EXIT_OK);
    fs::rename(&a, root.join("a-moved.rs")).unwrap(); fs::rename(&b, root.join("b-moved.rs")).unwrap();
    let (exit, rows, _) = invoke(&format!("0\trestore\t{}\n1\tview\t1\n1\tforward\n3\tview\t1\n3\tback\n5\tview\t1\n5\tquit\n", saved.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[1].get("result").get("text").text(), "A");
    assert_eq!(rows[3].get("result").get("text").text(), "B");
    assert_eq!(rows[5].get("result").get("text").text(), "A");
    assert_eq!(rows[0].get("result").get("retained_sources").number(), 2);
}

#[test]
fn restore_rejects_old_pane_handles_and_search_generations_but_accepts_new_work() {
    let root = fixture(); let source = root.join("a.rs"); let saved = root.join("state.fcbk");
    fs::write(&source, b"needle").unwrap();
    let script = format!("0\topen\t{}\n1\tfind\t1\t7\t10\t100\tneedle\n1\tsave\t{}\t100\n1\trestore\t{}\n4\thit\t1\t7\t0\n4\thit\t2\t7\t0\n4\tfind\t2\t7\t10\t100\tneedle\n4\tfind\t2\t8\t10\t100\tneedle\n4\thit\t2\t8\t0\n9\tcopy\t2\n9\tquit\n", source.display(), saved.display(), saved.display());
    let (exit, rows, _) = invoke(&script); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[4].get("error").get("code").text(), "DESK_MISSING_PANE");
    assert_eq!(rows[5].get("error").get("code").text(), "DESK_NO_QUERY");
    assert_eq!(rows[6].get("error").get("code").text(), "DESK_STALE_QUERY");
    assert_eq!(rows[9].get("result").get("original_hex").text(), "6e6565646c65");
    assert_eq!(rows[10].get("accepted_revision").number(), 9);
}

#[test]
fn export_limits_and_repeated_save_never_truncate_or_overwrite_a_checkpoint() {
    let root = fixture(); let source = root.join("a.rs"); let saved = root.join("state.fcbk");
    fs::write(&source, b"private").unwrap();
    let (exit, rows, _) = invoke(&format!("0\topen\t{}\n1\tsave\t{}\t1\n1\tsave\t{}\t100\n1\tsave\t{}\t100\n1\tquit\n", source.display(), saved.display(), saved.display(), saved.display()));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[1].get("result").get("error").get("code").text(), "DESK_SAVE_SOURCE_EXPORT_LIMIT");
    assert_eq!(rows[1].get("effect").text(), "none");
    assert_eq!(rows[2].get("result").get("status").text(), "ok");
    assert_eq!(rows[3].get("result").get("error").get("code").text(), "DESK_SAVE_DESTINATION_EXISTS");
    assert_eq!(rows[3].get("effect").text(), "none");
    assert_eq!(rows[2].get("result").get("checkpoint_bytes").number(), fs::metadata(&saved).unwrap().len());
    let (exit, restored, _) = invoke(&format!("0\trestore\t{}\n1\tview\t1\n1\tquit\n", saved.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(restored[1].get("result").get("text").text(), "private");
}

#[test]
fn corrupt_checkpoint_does_not_discard_a_working_query_or_source() {
    let root = fixture(); let source = root.join("a.rs"); let bad = root.join("bad.fcbk");
    fs::write(&source, b"needle").unwrap(); fs::write(&bad, b"not a checkpoint").unwrap();
    let (exit, rows, _) = invoke(&format!("0\topen\t{}\n1\tfind\t1\t1\t10\t100\tneedle\n1\trestore\t{}\n1\thit\t1\t1\t0\n4\tcopy\t1\n4\tquit\n", source.display(), bad.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(rows[2].get("accepted_revision").number(), 1);
    assert_eq!(rows[2].get("error").get("code").text(), "DESK_CHECKPOINT_INVALID");
    assert_eq!(rows[2].get("accepted_query_generation").number(), 1);
    assert_eq!(rows[4].get("result").get("original_hex").text(), "6e6565646c65");
}

#[test]
fn broken_stdout_after_save_keeps_the_file_and_reports_its_effect() {
    struct BrokenAfterFirst { flushes: usize }
    impl Write for BrokenAfterFirst {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.flushes > 0 { Err(io::ErrorKind::BrokenPipe.into()) } else { Ok(bytes.len()) }
        }
        fn flush(&mut self) -> io::Result<()> { self.flushes += 1; Ok(()) }
    }
    let root = fixture(); let source = root.join("a.rs"); let saved = root.join("saved.fcbk"); let never = root.join("never.fcbk");
    fs::write(&source, b"saved").unwrap();
    let script = format!("0\topen\t{}\n1\tsave\t{}\t100\n1\tsave\t{}\t100\n", source.display(), saved.display(), never.display());
    let mut input = script.as_bytes(); let mut err = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut BrokenAfterFirst { flushes: 0 }, &mut err, || false), EXIT_ERROR);
    assert!(String::from_utf8(err).unwrap().contains("effect=complete-file-and-parent-sync-requested"));
    assert!(saved.exists()); assert!(!never.exists());
    let (exit, rows, _) = invoke(&format!("0\trestore\t{}\n1\tview\t1\n1\tquit\n", saved.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(rows[1].get("result").get("text").text(), "saved");
}

#[test]
fn cancellation_racing_save_delivery_does_not_erase_the_write_receipt() {
    struct CancelOnSave<'a> { bytes: Vec<u8>, flag: &'a AtomicBool }
    impl Write for CancelOnSave<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            if bytes.windows(b"\"command\":\"save\"".len()).any(|s| s == b"\"command\":\"save\"") { self.flag.store(true, Ordering::Release); }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let root = fixture(); let saved = root.join("empty.fcbk"); let flag = AtomicBool::new(false);
    let script = format!("0\tsave\t{}\t0\n0\tquit\n", saved.display()); let mut input = script.as_bytes();
    let mut output = CancelOnSave { bytes: Vec::new(), flag: &flag }; let mut errors = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut output, &mut errors, || flag.load(Ordering::Acquire)), EXIT_CANCELED);
    let rows = records(&output.bytes); assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("exit_code").number(), 0);
    assert_eq!(rows[0].get("effect").text(), "complete-file-and-parent-sync-requested");
    assert_eq!(rows[1].get("exit_code").number(), 130); assert!(errors.is_empty()); assert!(saved.exists());
}

#[test]
fn native_hex_checkpoint_paths_and_exact_malformed_bytes_round_trip() {
    use std::os::unix::ffi::{OsStringExt, OsStrExt};
    let root = fixture(); let source = root.join(OsString::from_vec(b"source\t\xff".to_vec()));
    let saved = root.join(OsString::from_vec(b"save\n\xff.fcbk".to_vec())); fs::write(&source, [b'a', 0, 0xff]).unwrap();
    let hex = |p: &std::path::Path| p.as_os_str().as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let (exit, _, _) = invoke(&format!("0\topen-hex\t{}\n1\tselect\t1\t1\t3\n2\tsave-hex\t{}\t3\n2\tquit\n", hex(&source), hex(&saved)));
    assert_eq!(exit, EXIT_OK);
    let (exit, rows, _) = invoke(&format!("0\trestore-hex\t{}\n1\tcopy\t1\n1\tquit\n", hex(&saved)));
    assert_eq!(exit, EXIT_OK); assert_eq!(rows[1].get("result").get("original_hex").text(), "00ff");
}

#[test]
fn invalid_arrangements_leave_the_reader_and_revision_unchanged() {
    let root = fixture(); let source = root.join("a.rs"); fs::write(&source, b"safe").unwrap();
    let (exit, rows, _) = invoke(&format!("0\topen\t{}\n1\tarrange\t1\tNaN\t0\t100\t100\n1\tarrange\t1\t0\t0\t0\t10\n1\tview\t1\n1\tquit\n", source.display()));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[1].get("error").get("code").text(), "DESK_INVALID_COORDINATE");
    assert_eq!(rows[2].get("error").get("code").text(), "DESK_INVALID_LOCATION");
    assert_eq!(rows[3].get("result").get("text").text(), "safe"); assert_eq!(rows[4].get("accepted_revision").number(), 1);
}

#[test]
fn incomplete_save_frame_and_missing_disclosure_cap_never_write() {
    let root = fixture(); let saved = root.join("never.fcbk");
    let (exit, rows, _) = invoke(&format!("0\tsave\t{}\n0\tquit\n", saved.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(rows[0].get("error").get("code").text(), "DESK_COMMAND_SYNTAX");
    assert!(!saved.exists());
    let (exit, rows, _) = invoke(&format!("0\tsave\t{}\t0", saved.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(rows[0].get("error").get("code").text(), "DESK_FRAME_INCOMPLETE");
    assert!(!saved.exists());
}

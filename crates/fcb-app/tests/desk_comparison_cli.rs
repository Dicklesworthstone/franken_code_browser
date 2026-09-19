#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{parse, Json};
use std::{ffi::OsString, fs, io::{self, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

fn fixture(a: &[u8], b: &[u8]) -> (PathBuf, PathBuf, PathBuf) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-compare-cli-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    let before = root.join("before.rs"); let after = root.join("after.rs");
    fs::write(&before, a).unwrap(); fs::write(&after, b).unwrap();
    (root, before, after)
}
fn args() -> Vec<OsString> { vec!["desk".into(), "--stdio".into()] }
fn records(bytes: &[u8]) -> Vec<Json> {
    bytes.split(|b| *b == b'\n').filter(|b| !b.is_empty()).map(|line| {
        let row = parse(line).unwrap(); assert_eq!(row.get("schema").text(), "fcb.desk-stdio/1"); row
    }).collect()
}
fn invoke(script: &str) -> (u8, Vec<Json>) {
    let mut input = script.as_bytes(); let mut out = Vec::new(); let mut err = Vec::new();
    let exit = run(&args(), &mut input, &mut out, &mut err, || false);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err)); (exit, records(&out))
}
fn prefix(a: &std::path::Path, b: &std::path::Path) -> String {
    format!("0\topen\t{}\n1\tpin\t1\n2\topen\t{}\n", a.display(), b.display())
}
fn result(row: &Json) -> &Json { row.get("result") }
fn error(row: &Json) -> &str { row.get("error").get("code").text() }

#[test]
fn compare_select_bookmark_and_restore_preserve_the_actual_source_difference() {
    let (root, a, b) = fixture(b"head tail", b"head INSERT tail"); let saved = root.join("reading.fcbk");
    let script = prefix(&a, &b) + &format!("3\tcompare-prepare\t1\t2\t1\n3\tcompare-window\t1\t1\t0\t0\t64\n3\tcompare-select\t1\t1\tafter\n6\tbookmark\t2\tinserted evidence\n7\tsave\t{}\t1024\n7\tquit\n", saved.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_OK);
    assert!(result(&rows[3]).get("comparison_complete").flag());
    assert_eq!(result(&rows[4]).get("before").get("original_hex").text(), "");
    assert_eq!(result(&rows[4]).get("after").get("exact_utf8_text").text(), "INSERT ");
    assert_eq!(rows[5].get("accepted_revision").number(), 6);
    assert_eq!(rows[8].get("comparison").get("generation").number(), 1);
    fs::rename(&a, root.join("old-moved.rs")).unwrap(); fs::rename(&b, root.join("new-moved.rs")).unwrap();
    let (exit, reopened) = invoke(&format!("0\trestore\t{}\n1\tcompare-prepare\t1\t2\t1\n1\tcopy\t2\n1\tquit\n", saved.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(reopened[0].get("comparison"), &Json::Null);
    assert_eq!(result(&reopened[2]).get("original_hex").text(), "494e5345525420");
    assert_eq!(result(&reopened[3]).get("bookmarks").array()[0].get("label").text(), "inserted evidence");
    assert_eq!(result(&reopened[3]).get("initial_source_bytes_read").number(), 0);
}

#[test]
fn historical_repository_hit_and_new_path_capture_compare_without_rebinding() {
    let (root, _, path) = fixture(b"unmatched", b"head old tail");
    struct Change { bytes: Vec<u8>, path: PathBuf, replies: usize }
    impl Write for Change {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.replies += 1; if self.replies == 5 { fs::write(&self.path, b"head NEW tail")?; } Ok(())
        }
    }
    // after.rs sorts before before.rs, hence its catalogued file ID is 1.
    let script = format!("0\trepo-open\t{}\n0\trepo-find\t1\t1\t10\t1024\thead\n0\trepo-hit\t1\t1\t1\n3\tpin\t1\n4\trepo-paths\t1\t1\texact\tsensitive\t10\tafter.rs\n4\trepo-path-open\t1\t1\t1\n6\tcompare-prepare\t1\t2\t1\n6\tcompare-window\t1\t1\t0\t0\t64\n6\trepo-close\t1\n6\tcompare-select\t1\t1\tafter\n10\tcopy\t2\n10\tquit\n", root.display());
    let mut input = script.as_bytes(); let mut out = Change { bytes: Vec::new(), path, replies: 0 }; let mut err = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut out, &mut err, || false), EXIT_OK); assert!(err.is_empty());
    let rows = records(&out.bytes);
    assert_eq!(result(&rows[7]).get("before").get("exact_utf8_text").text(), "old");
    assert_eq!(result(&rows[7]).get("after").get("exact_utf8_text").text(), "NEW");
    assert!(!result(&rows[7]).get("source_reopened").flag());
    assert_eq!(result(&rows[10]).get("original_hex").text(), "4e4557");
    assert_eq!(rows[11].get("repository_token"), &Json::Null);
    assert_eq!(rows[11].get("comparison").get("generation").number(), 1);
}

#[test]
fn failed_replacement_and_stale_clear_preserve_accepted_comparison() {
    let (_, a, b) = fixture(b"old", b"new");
    let script = prefix(&a, &b) + "3\tcompare-prepare\t1\t2\t1\n3\tcompare-prepare\t1\t2\t2\t513\t1000\n3\tcompare-page\t1\t0\t64\n3\tcompare-prepare\t1\t2\t2\n3\tcompare-prepare\t1\t2\t3\n3\tcompare-clear\t1\n3\tcompare-clear\t3\n3\tcompare-prepare\t1\t2\t3\n3\tcompare-prepare\t1\t2\t4\n3\tquit\n";
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(error(&rows[4]), "COMPARISON_LIMIT");
    assert_eq!(rows[4].get("comparison").get("generation").number(), 1);
    assert_eq!(rows[4].get("last_comparison_attempt").number(), 2);
    assert_eq!(error(&rows[6]), "COMPARISON_STALE"); assert_eq!(error(&rows[8]), "COMPARISON_STALE");
    assert_eq!(rows[9].get("comparison"), &Json::Null); assert_eq!(error(&rows[10]), "COMPARISON_STALE");
    assert_eq!(rows[12].get("comparison").get("generation").number(), 4);
    assert_eq!(rows[12].get("accepted_revision").number(), 3);
}

#[test]
fn retargeting_closing_and_restoring_cannot_reuse_stale_comparison_handles() {
    let (root, a, b) = fixture(b"old", b"new"); let third = root.join("third.rs"); fs::write(&third, b"other").unwrap();
    let script = prefix(&a, &b) + &format!("3\tcompare-prepare\t1\t2\t1\n3\topen\t{}\n5\tcompare-page\t1\t0\t64\n5\tback\n7\tcompare-prepare\t1\t2\t1\n7\tcompare-prepare\t1\t2\t2\n7\tclose\t1\n10\tquit\n", third.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[4].get("comparison"), &Json::Null); assert_eq!(error(&rows[5]), "DESK_NO_COMPARISON");
    assert_eq!(error(&rows[7]), "COMPARISON_STALE");
    assert_eq!(rows[8].get("comparison").get("generation").number(), 2);
    assert_eq!(rows[9].get("comparison"), &Json::Null);
}

#[test]
fn zero_work_comparison_is_partial_undetermined_even_for_identical_sources() {
    let (_, a, b) = fixture(b"same bytes", b"same bytes");
    let script = prefix(&a, &b) + "3\tcompare-prepare\t1\t2\t1\t256\t0\n3\tcompare-window\t1\t0\t0\t4\t3\n3\tcompare-page\t1\t1\t64\n3\tquit\n";
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(result(&rows[3]).get("relation").text(), "undetermined");
    assert_eq!(result(&rows[3]).get("comparison_quality").text(), "work-limit");
    assert_eq!(result(&rows[3]).get("changed_spans").number(), 0);
    assert_eq!(result(&rows[4]).get("before").get("exact_utf8_text").text(), "sam");
    assert_eq!(result(&rows[4]).get("after").get("exact_utf8_text").text(), " by");
    assert!(result(&rows[5]).get("spans").array().is_empty());
    assert!(!result(&rows[5]).get("comparison_complete").flag());
}

#[test]
fn raw_filename_and_malformed_source_windows_preserve_original_bytes() {
    use std::os::unix::ffi::{OsStringExt, OsStrExt};
    let (root, a, b) = fixture(&[0xff, 0, 0xfe, 0], &[0xff, 0, 0xfe, 0]);
    let raw = root.join(OsString::from_vec(b"new\t\xff.rs".to_vec())); fs::rename(&b, &raw).unwrap();
    let encoded = raw.as_os_str().as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let script = format!("0\topen\t{}\n1\tpin\t1\n2\topen-hex\t{}\n3\tcompare-prepare\t1\t2\t1\n3\tcompare-window\t1\t0\t0\t1\t3\n3\tcompare-select\t1\t0\tafter\n6\tcopy\t2\n6\tquit\n", a.display(), encoded);
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&rows[4]).get("before").get("original_hex").text(), "ff00fe");
    assert_eq!(result(&rows[4]).get("before").get("exact_utf8_text"), &Json::Null);
    assert_eq!(result(&rows[6]).get("original_hex").text(), "ff00fe00");
}

#[test]
fn comparison_does_not_advance_repository_work_or_replace_document_and_local_queries() {
    let (root, a, b) = fixture(b"# Old\n\nalpha\n", b"# New\n\nneedle\n");
    let script = prefix(&a, &b) + &format!("3\trepo-open\t{}\n3\trepo-begin\t4\t1\t10\t1024\tneedle\n3\tcompare-prepare\t1\t2\t1\n3\tdoc-prepare\t2\t1\t80\n3\tfind\t2\t1\t10\t1024\tneedle\n3\tcompare-page\t1\t0\t64\n3\trepo-progress\t4\t1\n3\trepo-cancel\t4\t1\n3\tquit\n", root.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[8].get("accepted_query_generation").number(), 1);
    assert_eq!(rows[8].get("documents").array().len(), 1);
    assert_eq!(rows[8].get("repository_pending_query_generation").number(), 1);
    assert_eq!(result(&rows[9]).get("step_count").number(), 0);
    assert_eq!(result(&rows[9]).get("source_bytes_read").number(), 0);
    assert!(rows[11].get("comparison").get("complete").flag());
}

#[test]
fn source_rename_after_open_cannot_change_comparison_or_trigger_live_reads() {
    let (root, a, b) = fixture(b"head old tail", b"head NEW tail");
    struct Move { bytes: Vec<u8>, a: PathBuf, b: PathBuf, root: PathBuf, replies: usize }
    impl Write for Move {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.replies += 1;
            if self.replies == 3 { fs::rename(&self.a, self.root.join("moved-a"))?; fs::rename(&self.b, self.root.join("moved-b"))?; }
            Ok(())
        }
    }
    let script = prefix(&a, &b) + "3\tcompare-prepare\t1\t2\t1\n3\tcompare-window\t1\t1\t0\t0\t64\n3\tquit\n";
    let mut input = script.as_bytes(); let mut out = Move { bytes: Vec::new(), a, b, root, replies: 0 }; let mut err = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut out, &mut err, || false), EXIT_OK); assert!(err.is_empty());
    let rows = records(&out.bytes);
    assert_eq!(result(&rows[4]).get("before").get("exact_utf8_text").text(), "old");
    assert_eq!(result(&rows[4]).get("after").get("exact_utf8_text").text(), "NEW");
    assert_eq!(result(&rows[4]).get("additional_source_bytes_read").number(), 0);
    assert!(!out.a.exists()); assert!(!out.b.exists());
}

#[test]
fn broken_comparison_delivery_stops_before_a_following_checkpoint_write() {
    struct Broken { replies: usize }
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.replies == 3 { Err(io::ErrorKind::BrokenPipe.into()) } else { Ok(bytes.len()) }
        }
        fn flush(&mut self) -> io::Result<()> { self.replies += 1; Ok(()) }
    }
    let (root, a, b) = fixture(b"old", b"new"); let saved = root.join("never.fcbk");
    let script = prefix(&a, &b) + &format!("3\tcompare-prepare\t1\t2\t1\n3\tsave\t{}\t100\n", saved.display());
    let mut input = script.as_bytes(); let mut err = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut Broken { replies: 0 }, &mut err, || false), EXIT_ERROR);
    assert!(String::from_utf8(err).unwrap().contains("DESK_OUTPUT_INTERRUPTED")); assert!(!saved.exists());
}

#[test]
fn stale_revision_same_pane_and_invalid_side_cannot_mutate_source_selection() {
    let (_, a, b) = fixture(b"old", b"new");
    let script = prefix(&a, &b) + "2\tcompare-prepare\t1\t2\t1\n3\tcompare-prepare\t1\t1\t1\n3\tcompare-prepare\t1\t2\t2\n3\tcompare-select\t2\t0\tunknown\n3\tcompare-select\t2\t9999\tafter\n3\tstate\n3\tquit\n";
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(error(&rows[3]), "DESK_STALE_REVISION"); assert_eq!(error(&rows[4]), "DESK_COMPARISON_SAME_PANE");
    assert_eq!(error(&rows[6]), "DESK_COMPARISON_SIDE"); assert_eq!(error(&rows[7]), "DESK_COMPARISON_NO_SPAN");
    assert_eq!(rows[9].get("accepted_revision").number(), 3);
    assert_eq!(result(&rows[8]).get("panes").array()[1].get("selection"), &Json::Null);
    assert_eq!(rows[9].get("comparison").get("generation").number(), 2);
}

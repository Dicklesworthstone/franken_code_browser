#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Stdio result windows keep count, row and capture identities distinct.
//! FCB-028 / fcb-qtb.2; code-first, independent batch verification pending.
mod support;
use support::{parse, Json};
use std::{ffi::OsString, fs, io::{self, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

fn fixture(files: &[(&str, &[u8])]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-window-cli-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&root).unwrap();
    for (name, source) in files {
        let path = root.join(name); fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    root
}
fn args() -> Vec<OsString> { vec!["desk".into(), "--stdio".into()] }
fn records(bytes: &[u8]) -> Vec<Json> {
    bytes.split(|&b| b == b'\n').filter(|s| !s.is_empty()).map(|s| parse(s).unwrap()).collect()
}
fn invoke(script: &str) -> (u8, Vec<Json>) {
    let mut input = script.as_bytes(); let mut output = Vec::new(); let mut errors = Vec::new();
    let exit = run(&args(), &mut input, &mut output, &mut errors, || false);
    assert!(errors.is_empty(), "{}", String::from_utf8_lossy(&errors));
    (exit, records(&output))
}
fn result(row: &Json) -> &Json { row.get("result") }
fn error(row: &Json) -> &str { row.get("error").get("code").text() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
#[derive(Default)]
struct Script { text: String, revision: u64, count: usize }
impl Script {
    fn push(&mut self, command: &str, mutation: bool) -> usize {
        let row = self.count; self.count += 1;
        self.text.push_str(&format!("{}\t{command}\n", self.revision));
        if mutation { self.revision = self.count as u64; }
        row
    }
}

#[test]
fn all_window_commands_reach_late_overlaps_and_open_the_original_indexed_capture() {
    struct Replace { bytes: Vec<u8>, flushes: usize, path: PathBuf }
    impl Write for Replace {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 3 { fs::write(&self.path, b"replacement ABSENT\n")?; }
            Ok(())
        }
    }
    let source = b"banana required\n".repeat(3000);
    let root = fixture(&[("main.rs", &source)]);
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-index-begin\t1\t1\t10\t100000\t1000", false);
    s.push("repo-index-step\t1\t1\t0", false);
    s.push("repo-begin-indexed-matching-window\t1\t1\t2\tsensitive\tbounded\t0\t1\t1000000\tworkspace\t-\tana", false);
    let first = s.push("repo-step\t1\t2\t0", false);
    let begin = s.push(&format!("repo-begin-indexed-expression-window-text-hex\t1\t1\t3\tsensitive\tcount-all\t4097\t1\t1000000\tworkspace\t-\t{}",
        hex(b"ana required -absent")), false);
    let late = s.push("repo-step\t1\t3\t0", false);
    let hit = s.push("repo-hit\t1\t3\t1", true);
    s.push("repo-begin-indexed-matching-window-text-hex\t1\t1\t4\tsensitive\tcount-all\t5999\t1\t1000000\tfile\t6d61696e2e7273\t616e61", false);
    let last = s.push("repo-step\t1\t4\t0", false);
    s.push("repo-begin-indexed-expression-window\t1\t1\t5\tsensitive\tcount-all\t0\t0\t1000000\tworkspace\t-\tana required -absent", false);
    let counted = s.push("repo-step\t1\t5\t0", false);
    s.push("repo-index-clear\t1\t6", false); s.push("repo-close\t1", false);
    let copy = s.push("copy\t1", false); s.push("quit", false);
    let mut input = s.text.as_bytes(); let mut errors = Vec::new();
    let mut output = Replace { bytes: Vec::new(), flushes: 0, path: root.join("main.rs") };
    assert_eq!(run(&args(), &mut input, &mut output, &mut errors, || false), EXIT_PARTIAL);
    assert!(errors.is_empty()); let rows = records(&output.bytes);
    assert_eq!(rows.len(), s.count);
    assert_eq!(result(&rows[first]).get("next_match_offset").number(), 1);
    assert!(!result(&rows[first]).get("match_count_complete").flag());
    assert!(rows[begin].get("repository_work_pending").flag());
    assert_eq!(rows[begin].get("repository_query_generation").number(), 2);
    for row in [late, last, counted] {
        let page = result(&rows[row]);
        assert_eq!(page.get("source_bytes_read").number(), 0);
        assert_eq!(page.get("read_calls").number(), 0);
        assert_eq!(page.get("matches_seen").number(), 6000);
        assert!(page.get("match_count_complete").flag());
    }
    let page = result(&rows[late]);
    assert_eq!(page.get("result_offset").number(), 4097);
    assert_eq!(page.get("skipped_matches").number(), 4097);
    assert_eq!(page.get("retained_hits").number(), 1);
    assert_eq!(page.get("next_match_offset").number(), 4098);
    assert_eq!(page.get("hits").array()[0].get("original_range").get("start").number(), 32771);
    assert_eq!(result(&rows[hit]).get("original_range").get("start").number(), 32771);
    assert_eq!(result(&rows[hit]).get("origin_source_revision").number(), 1);
    assert!(!result(&rows[hit]).get("source_reopened").flag());
    assert_eq!(result(&rows[last]).get("result_offset").number(), 5999);
    assert_eq!(result(&rows[last]).get("hits").array()[0].get("original_range").get("start").number(), 47987);
    assert_eq!(result(&rows[last]).get("next_match_offset"), &Json::Null);
    assert!(!result(&rows[last]).get("has_more_matches").flag());
    assert_eq!(result(&rows[counted]).get("retained_hits").number(), 0);
    assert_eq!(result(&rows[counted]).get("retained_source_bytes").number(), 0);
    assert_eq!(result(&rows[counted]).get("next_match_offset"), &Json::Null);
    assert_eq!(result(&rows[copy]).get("original_hex").text(), hex(b"ana"));
    assert_eq!(fs::read(root.join("main.rs")).unwrap(), b"replacement ABSENT\n");
}

#[test]
fn invalid_window_options_and_stale_work_keep_accepted_rows_and_legacy_frame_bounds() {
    let root = fixture(&[("a.rs", b"needle required"), ("b.rs", b"needle required")]);
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-index-begin\t1\t1\t10\t1000\t1000", false);
    s.push("repo-index-step\t1\t1\t0", false); s.push("repo-index-step\t1\t1\t1", false);
    s.push("repo-begin-indexed-matching-window\t1\t1\t2\tsensitive\tcount-all\t0\t2\t1000\tworkspace\t-\tneedle", false);
    s.push("repo-step\t1\t2\t0", false); s.push("repo-step\t1\t2\t1", false);
    let case = s.push("repo-begin-indexed-matching-window\t1\t1\t3\tfolded\tcount-all\t0\t1\t1000\tworkspace\t-\tneedle", false);
    let counting = s.push("repo-begin-indexed-matching-window\t1\t1\t3\tsensitive\tall\t0\t1\t1000\tworkspace\t-\tneedle", false);
    let scope = s.push("repo-begin-indexed-matching-window\t1\t1\t3\tsensitive\tcount-all\t0\t1\t1000\tworkspace\t737263\tneedle", false);
    let traversal = s.push("repo-begin-indexed-matching-window\t1\t1\t3\tsensitive\tcount-all\t0\t1\t1000\tdirectory\t2e2e\tneedle", false);
    let malformed = s.push("repo-begin-indexed-expression-window-text-hex\t1\t1\t3\tsensitive\tcount-all\t0\t1\t1000\tworkspace\t-\tff", false);
    let mut numbers = Vec::new();
    for offset in ["-1", "01", "18446744073709551616"] {
        numbers.push(s.push(&format!("repo-begin-indexed-matching-window\t1\t1\t3\tsensitive\tcount-all\t{offset}\t1\t1000\tworkspace\t-\tneedle"), false));
    }
    s.push("repo-begin-indexed-matching-window\t1\t1\t3\tsensitive\tcount-all\t1\t1\t1000\tworkspace\t-\tneedle", false);
    let stale = s.push("repo-step\t1\t3\t1", false);
    let canceled = s.push("repo-cancel\t1\t3", false);
    let zero = s.push("repo-begin-indexed-matching-window\t1\t1\t4\tsensitive\tbounded\t0\t0\t1000\tworkspace\t-\tneedle", false);
    let regex = s.push("repo-begin-indexed-expression-window\t1\t1\t5\tsensitive\tcount-all\t1\t1\t1000\tworkspace\t-\tre:needle", false);
    let syntax = s.push("repo-begin-indexed-expression-window\t1\t1\t6\tsensitive\tcount-all\t1\t1\t1000\tworkspace\t-\t\"needle", false);
    let page = s.push("repo-page\t1\t2\t0\t10", false);
    let extra = s.push("repo-begin-indexed-expression-window\t1\t1\t7\tsensitive\tcount-all\t0\t1\t1000\tworkspace\t-\tneedle\textra", false);
    let scoped_bound = s.push("repo-begin-indexed-matching\t1\t1\t7\tsensitive\tcount-all\t1\t1000\tworkspace\t-\tneedle\textra", false);
    let old_bound = s.push("state\t1\t2\t3\t4\t5\t6\t7\t8\t9", false);
    s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows.len(), s.count);
    assert_eq!(error(&rows[case]), "DESK_SEARCH_CASE");
    assert_eq!(error(&rows[counting]), "DESK_SEARCH_COUNT_MODE");
    assert_eq!(error(&rows[scope]), "DESK_SEARCH_SCOPE");
    assert_eq!(error(&rows[traversal]), "ATLAS_SEARCH_INVALID_SCOPE");
    assert_eq!(error(&rows[malformed]), "DESK_INVALID_TEXT");
    for row in numbers { assert_eq!(error(&rows[row]), "DESK_INVALID_NUMBER"); }
    assert_eq!(error(&rows[stale]), "DESK_REPOSITORY_STALE_STEP");
    assert!(!rows[canceled].get("repository_work_pending").flag());
    assert_eq!(error(&rows[zero]), "ATLAS_SEARCH_INVALID_LIMITS");
    assert!(error(&rows[regex]).contains("QUERY_REGEX_UNQUALIFIED"));
    assert!(error(&rows[syntax]).contains("QUERY_SYNTAX_ERROR"));
    assert_eq!(result(&rows[page]).get("retained_hits").number(), 2);
    assert_eq!(rows[page].get("repository_query_generation").number(), 2);
    for row in [extra, scoped_bound, old_bound] { assert_eq!(error(&rows[row]), "DESK_FIELD_LIMIT"); }
}

#[test]
fn full_width_offsets_partial_coverage_and_unfinished_windows_have_explicit_receipts() {
    let root = fixture(&[("main.rs", b"needle needle needle")]);
    let prefix = format!("0\trepo-open\t{}\n0\trepo-index-begin\t1\t1\t10\t1000\t1000\n0\trepo-index-step\t1\t1\t0\n",
        root.display());
    let begin = "0\trepo-begin-indexed-matching-window\t1\t1\t2\tsensitive\tcount-all\t18446744073709551615\t1\t1000\tworkspace\t-\tneedle\n";
    assert_eq!(invoke(&(prefix.clone() + begin)).0, EXIT_PARTIAL);
    assert_eq!(invoke(&(prefix.clone() + begin + "0\trepo-cancel\t1\t2\n0\tquit\n")).0, EXIT_OK);
    let tail = "0\trepo-step\t1\t2\t0\n0\trepo-begin-indexed-matching-window\t1\t1\t3\tsensitive\tcount-all\t1\t1\t0\tworkspace\t-\tneedle\n0\trepo-step\t1\t3\t0\n0\tquit\n";
    let (exit, rows) = invoke(&(prefix + begin + tail)); assert_eq!(exit, EXIT_PARTIAL);
    let beyond = result(&rows[4]);
    assert_eq!(beyond.get("result_offset").number(), u64::MAX);
    assert_eq!(beyond.get("skipped_matches").number(), 3);
    assert_eq!(beyond.get("matches_seen").number(), 3);
    assert_eq!(beyond.get("retained_hits").number(), 0);
    assert_eq!(beyond.get("retained_source_bytes").number(), 0);
    assert!(beyond.get("match_count_complete").flag());
    assert_eq!(beyond.get("next_match_offset"), &Json::Null);
    let limited = result(&rows[6]);
    assert_eq!(limited.get("result_offset").number(), 1);
    assert_eq!(limited.get("matches_seen").number(), 0);
    assert!(!limited.get("match_count_complete").flag());
    assert_eq!(limited.get("next_match_offset"), &Json::Null);
    assert_eq!(limited.get("stop_reason").text(), "verification-byte-limit");
}


#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Real stdio routing for scoped live/captured text queries (fcb-zebl.1).
//! Code-first cases; independent strict-RCH execution remains pending.
mod support;
use support::{parse, Json};
use std::{ffi::OsString, fs, io::{self, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

fn fixture(files: &[(&str, &[u8])]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-text-cli-{}-{stamp}-{}",
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
fn scoped_lowercase_counting_keeps_utf16_ranges_after_row_truncation() {
    for little in [true, false] {
        let source: Vec<u8> = "\u{feff}BANANA BANANA".encode_utf16()
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
        let root = fixture(&[("src/main.rs", &source), ("src-old/main.rs", b"banana banana")]);
        let mut s = Script::default();
        s.push(&format!("repo-open\t{}", root.display()), false);
        let begin = s.push("repo-begin-matching-text-hex\t1\t1\tunicode-lowercase\tcount-all\t1\t1000\tdirectory\t737263\t414e41", false);
        let progress = s.push("repo-progress\t1\t1", false);
        let done = s.push("repo-step\t1\t1\t0", false);
        let hit = s.push("repo-hit\t1\t1\t1", true);
        s.push("repo-close\t1", false);
        let copy = s.push("copy\t1", false); s.push("quit", false);
        let (exit, rows) = invoke(&s.text);
        assert_eq!(exit, EXIT_PARTIAL); // Rows omitted, even though the total is exact.
        assert_eq!(result(&rows[begin]).get("source_bytes_read").number(), 0);
        assert_eq!(result(&rows[progress]).get("step_count").number(), 0);
        let page = result(&rows[done]);
        assert_eq!(page.get("eligible_files").number(), 1);
        assert_eq!(page.get("excluded_files").number(), 1);
        assert!(!page.get("case_sensitive").flag());
        assert!(page.get("count_all_matches").flag());
        assert_eq!(page.get("matches_seen").number(), 4);
        assert_eq!(page.get("retained_hits").number(), 1);
        assert!(page.get("match_count_complete").flag());
        assert!(page.get("search_complete").flag());
        assert!(page.get("truncated").flag());
        assert_eq!(result(&rows[hit]).get("original_range").get("start").number(), 4);
        assert!(!result(&rows[hit]).get("source_reopened").flag());
        assert_eq!(result(&rows[copy]).get("original_hex").text(), hex(&source[4..10]));
    }
}

#[test]
fn expressions_are_opt_in_and_scope_filters_do_not_multiply_primary_matches() {
    let root = fixture(&[("src/good.rs", b"pub fn first Result\npub fn second Result\n"),
        ("src/blocked.rs", b"pub fn blocked Result unsafe\n"),
        ("src/README.md", b"pub fn Result\n"), ("src-old/other.rs", b"pub fn Result\n")]);
    let expression = "\"pub fn\" Result -unsafe lang:rust";
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push(&format!("repo-begin-matching\t1\t1\tsensitive\tbounded\t10\t1000\tfile\t{}\t{expression}",
        hex(b"src/good.rs")), false);
    let literal = s.push("repo-step\t1\t1\t0", false);
    s.push(&format!("repo-begin-expression\t1\t2\tsensitive\tbounded\t10\t1000\t1000\tdirectory\t737263\t{expression}"), false);
    s.push("repo-step\t1\t2\t0", false);
    let done = s.push("repo-step\t1\t2\t1", false);
    s.push("repo-hit\t1\t2\t2", true);
    let copy = s.push("copy\t1", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&rows[literal]).get("retained_hits").number(), 0);
    let page = result(&rows[done]);
    assert_eq!(page.get("mode").text(), "decoded-text-expression");
    assert_eq!(page.get("expression").text(), expression);
    assert_eq!(page.get("predicate_scope").text(), "same-complete-capture");
    assert_eq!(page.get("expression_scope_files").number(), 2);
    assert_eq!(page.get("expression_excluded_files").number(), 1);
    assert_eq!(page.get("retained_hits").number(), 2);
    assert_eq!(page.get("matching_files").number(), 1);
    assert!(page.get("search_complete").flag());
    assert_eq!(result(&rows[copy]).get("original_hex").text(), hex(b"pub fn"));
}

#[test]
fn indexed_literals_and_expressions_keep_captures_after_live_replacement_and_detachment() {
    struct Replace { bytes: Vec<u8>, flushes: usize, path: PathBuf }
    impl Write for Replace {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 3 { fs::write(&self.path, b"replacement ABSENT\n")?; }
            Ok(())
        }
    }
    let root = fixture(&[("main.rs", b"BANANA REQUIRED\n")]);
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-index-begin\t1\t1\t10\t1000\t1000", false);
    s.push("repo-index-step\t1\t1\t0", false);
    s.push("repo-begin-indexed-matching-text-hex\t1\t1\t2\tunicode-lowercase\tcount-all\t0\t1000\tworkspace\t-\t414e41", false);
    let counted = s.push("repo-step\t1\t2\t0", false);
    s.push(&format!("repo-begin-indexed-expression-text-hex\t1\t1\t3\tunicode-lowercase\tcount-all\t1\t1000\tworkspace\t-\t{}",
        hex(b"ANA required -absent")), false);
    let expression_count = s.push("repo-step\t1\t3\t0", false);
    s.push("repo-begin-indexed-expression\t1\t1\t4\tunicode-lowercase\tbounded\t10\t1000\tworkspace\t-\tANA required -absent", false);
    let expression = s.push("repo-step\t1\t4\t0", false);
    let hit = s.push("repo-hit\t1\t4\t2", true);
    s.push("repo-begin-indexed-matching\t1\t1\t5\tsensitive\tbounded\t10\t1000\tfile\t6d61696e2e7273\tANA", false);
    let literal = s.push("repo-step\t1\t5\t0", false);
    s.push("repo-index-clear\t1\t6", false); s.push("repo-close\t1", false);
    let view = s.push("view\t1", false); let copy = s.push("copy\t1", false); s.push("quit", false);
    let mut input = s.text.as_bytes(); let mut errors = Vec::new();
    let mut output = Replace { bytes: Vec::new(), flushes: 0, path: root.join("main.rs") };
    assert_eq!(run(&args(), &mut input, &mut output, &mut errors, || false), EXIT_PARTIAL);
    assert!(errors.is_empty()); let rows = records(&output.bytes);
    for row in [counted, expression_count, expression, literal] {
        let page = result(&rows[row]);
        assert_eq!(page.get("source_bytes_read").number(), 0);
        assert_eq!(page.get("read_calls").number(), 0);
        assert_eq!(page.get("matches_seen").number(), 2);
        assert!(page.get("match_count_complete").flag());
    }
    assert_eq!(result(&rows[counted]).get("retained_hits").number(), 0);
    assert_eq!(result(&rows[counted]).get("retained_source_bytes").number(), 0);
    assert_eq!(result(&rows[expression_count]).get("retained_hits").number(), 1);
    assert_eq!(result(&rows[expression]).get("retained_hits").number(), 2);
    assert_eq!(result(&rows[hit]).get("origin_source_revision").number(), 1);
    assert!(!result(&rows[hit]).get("source_reopened").flag());
    assert_eq!(result(&rows[view]).get("text").text(), "BANANA REQUIRED\n");
    assert_eq!(result(&rows[copy]).get("original_hex").text(), hex(b"ANA"));
}

#[test]
fn visible_raw_paths_empty_membership_and_missing_members_have_distinct_counts() {
    use std::os::unix::ffi::OsStringExt;
    let root = fixture(&[("ordinary.rs", b"needle")]);
    let raw = b"raw\xff.rs";
    fs::write(root.join(OsString::from_vec(raw.to_vec())), b"needle").unwrap();
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push(&format!("repo-begin-matching\t1\t1\tsensitive\tcount-all\t1\t1000\tvisible\t{}\tneedle", hex(raw)), false);
    let found = s.push("repo-step\t1\t1\t0", false);
    s.push("repo-begin-matching\t1\t2\tsensitive\tcount-all\t0\t1000\tvisible\t-\tneedle", false);
    let empty = s.push("repo-step\t1\t2\t0", false);
    s.push(&format!("repo-begin-matching\t1\t3\tsensitive\tcount-all\t0\t1000\tvisible\t{}\tneedle",
        hex(b"missing.rs")), false);
    let missing = s.push("repo-step\t1\t3\t0", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(result(&rows[found]).get("retained_hits").number(), 1);
    assert_eq!(result(&rows[found]).get("eligible_files").number(), 1);
    assert_eq!(result(&rows[found]).get("hits").array()[0].get("path").get("hex").text(), hex(raw));
    for row in [empty, missing] {
        assert_eq!(result(&rows[row]).get("matches_seen").number(), 0);
        assert_eq!(result(&rows[row]).get("source_bytes_read").number(), 0);
    }
    assert!(result(&rows[empty]).get("match_count_complete").flag());
    assert!(!result(&rows[empty]).get("truncated").flag());
    assert!(!result(&rows[missing]).get("match_count_complete").flag());
    assert_eq!(result(&rows[missing]).get("scope_region_missing_files").number(), 1);
}

#[test]
fn invalid_options_cancel_and_stale_frames_keep_accepted_results() {
    let root = fixture(&[("main.rs", b"needle required")]);
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-begin-matching\t1\t1\tsensitive\tbounded\t10\t1000\tworkspace\t-\tneedle", false);
    s.push("repo-step\t1\t1\t0", false);
    let case = s.push("repo-begin-matching\t1\t2\tfolded\tbounded\t10\t1000\tworkspace\t-\tneedle", false);
    let count = s.push("repo-begin-matching\t1\t2\tsensitive\tall\t10\t1000\tworkspace\t-\tneedle", false);
    let scope = s.push("repo-begin-matching\t1\t2\tsensitive\tbounded\t10\t1000\tworkspace\t737263\tneedle", false);
    let traversal = s.push("repo-begin-matching\t1\t2\tsensitive\tbounded\t10\t1000\tdirectory\t2e2e\tneedle", false);
    let malformed = s.push("repo-begin-expression-text-hex\t1\t2\tsensitive\tbounded\t10\t1000\t1000\tworkspace\t-\tff", false);
    s.push("repo-begin-expression\t1\t2\tsensitive\tcount-all\t0\t1000\t1000\tworkspace\t-\tneedle required", false);
    let stale = s.push("repo-step\t1\t2\t1", false);
    let canceled = s.push("repo-cancel\t1\t2", false);
    let zero = s.push("repo-begin-matching\t1\t3\tsensitive\tbounded\t0\t1000\tworkspace\t-\tneedle", false);
    let regex = s.push("repo-begin-expression\t1\t4\tsensitive\tbounded\t10\t1000\t1000\tworkspace\t-\tre:needle", false);
    let syntax = s.push("repo-begin-expression\t1\t5\tsensitive\tbounded\t10\t1000\t1000\tworkspace\t-\t\"needle", false);
    let page = s.push("repo-page\t1\t1\t0\t10", false);
    let extra = s.push("repo-begin-expression\t1\t6\tsensitive\tbounded\t10\t1000\t1000\tworkspace\t-\tneedle\textra", false);
    let old_bound = s.push("state\t1\t2\t3\t4\t5\t6\t7\t8\t9", false);
    s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(error(&rows[case]), "DESK_SEARCH_CASE");
    assert_eq!(error(&rows[count]), "DESK_SEARCH_COUNT_MODE");
    assert_eq!(error(&rows[scope]), "DESK_SEARCH_SCOPE");
    assert_eq!(error(&rows[traversal]), "ATLAS_SEARCH_INVALID_SCOPE");
    assert_eq!(error(&rows[malformed]), "DESK_INVALID_TEXT");
    assert_eq!(error(&rows[stale]), "DESK_REPOSITORY_STALE_STEP");
    assert!(!rows[canceled].get("repository_work_pending").flag());
    assert_eq!(error(&rows[zero]), "ATLAS_SEARCH_INVALID_LIMITS");
    assert!(error(&rows[regex]).contains("QUERY_REGEX_UNQUALIFIED"));
    assert!(error(&rows[syntax]).contains("QUERY_SYNTAX_ERROR"));
    assert_eq!(result(&rows[page]).get("retained_hits").number(), 1);
    assert_eq!(rows[page].get("repository_query_generation").number(), 1);
    assert_eq!(error(&rows[extra]), "DESK_FIELD_LIMIT");
    assert_eq!(error(&rows[old_bound]), "DESK_FIELD_LIMIT");
}

#[test]
fn count_only_work_obeys_verification_budgets_and_distinguishes_cancel_from_eof() {
    let root = fixture(&[("main.rs", b"needle required trailing marker")]);
    let open = format!("0\trepo-open\t{}\n", root.display());
    let begin = "0\trepo-begin-expression\t1\t1\tsensitive\tcount-all\t0\t1000\t1000\tworkspace\t-\tneedle required -absent\n";
    assert_eq!(invoke(&(open.clone() + begin)).0, EXIT_PARTIAL);
    assert_eq!(invoke(&(open.clone() + begin + "0\trepo-cancel\t1\t1\n0\tquit\n")).0, EXIT_OK);
    let (exit, rows) = invoke(&(open.clone() + begin + "0\trepo-step\t1\t1\t0\n0\tquit\n"));
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(result(&rows[2]).get("match_count_complete").flag());
    assert_eq!(result(&rows[2]).get("matches_seen").number(), 1);
    assert_eq!(result(&rows[2]).get("retained_hits").number(), 0);
    let zero = "0\trepo-begin-expression\t1\t1\tsensitive\tcount-all\t0\t1000\t0\tworkspace\t-\tneedle required -absent\n";
    let (exit, rows) = invoke(&(open + zero + "0\trepo-step\t1\t1\t0\n0\tquit\n"));
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!result(&rows[2]).get("match_count_complete").flag());
    assert_eq!(result(&rows[2]).get("source_bytes_read").number(), 0);
    assert_eq!(result(&rows[2]).get("matches_seen").number(), 0);
    assert_eq!(result(&rows[2]).get("stop_reason").text(), "verification-byte-limit");
}

#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{parse, Json};
use std::{ffi::OsString, fs, io::{self, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

fn fixture(files: &[(&str, &[u8])]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-structure-cli-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
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
fn directory_symbols_flow_through_progress_hit_copy_and_detached_reading() {
    let root = fixture(&[("src/main.rs", b"fn target() {}\n// target\n"),
        ("src-old/main.rs", b"fn target() {}\n")]);
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    let begin = s.push("repo-begin-structure\t1\t1\tsymbols\texact\t1\t1000\tdirectory\t737263\ttarget", false);
    let progress = s.push("repo-progress\t1\t1", false);
    let done = s.push("repo-step\t1\t1\t0", false);
    let hit = s.push("repo-hit\t1\t1\t1", true);
    s.push("repo-close\t1", false);
    let copy = s.push("copy\t1", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[begin].get("exit_code").number(), EXIT_PARTIAL as u64);
    assert_eq!(result(&rows[begin]).get("source_bytes_read").number(), 0);
    assert_eq!(result(&rows[progress]).get("step_count").number(), 0);
    assert_eq!(result(&rows[done]).get("requested_mode").text(), "symbols");
    assert_eq!(result(&rows[done]).get("eligible_files").number(), 1);
    assert_eq!(result(&rows[done]).get("excluded_files").number(), 1);
    assert!(result(&rows[done]).get("search_complete").flag());
    assert!(!result(&rows[hit]).get("source_reopened").flag());
    assert_eq!(result(&rows[copy]).get("original_hex").text(), hex(b"target"));
}

#[test]
fn headings_keep_punctuation_duplicate_slugs_and_exact_markdown_selection() {
    let root = fixture(&[("README.md", b"# Deploy!\n\nbody\n\n# Deploy!\n")]);
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-begin-structure-text-hex\t1\t1\theadings\texact\t10\t1000\tfile\t524541444d452e6d64\t4465706c6f7921", false);
    let done = s.push("repo-step\t1\t1\t0", false);
    s.push("repo-hit\t1\t1\t2", true);
    let copy = s.push("copy\t1", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_OK);
    let hits = result(&rows[done]).get("hits").array();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].get("structure").get("heading_slug").text(), "deploy");
    assert_eq!(hits[1].get("structure").get("heading_slug").text(), "deploy-2");
    assert!(result(&rows[copy]).get("original_hex").text().starts_with(&hex(b"# Deploy!")));
}

#[test]
fn occurrences_preserve_utf16_original_bytes_and_exact_token_boundaries() {
    let source: Vec<u8> = [0xff, 0xfe].into_iter().chain(
        "needle needle_suffix\n// needle\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
    let root = fixture(&[("source.txt", &source)]);
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-begin-structure-text-hex\t1\t1\toccurrences\texact\t10\t1000\tworkspace\t-\t6e6565646c65", false);
    let done = s.push("repo-step\t1\t1\t0", false);
    let hit = s.push("repo-hit\t1\t1\t1", true);
    let copy = s.push("copy\t1", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&rows[done]).get("retained_hits").number(), 2);
    assert!(result(&rows[done]).get("includes_comments_and_literals").flag());
    assert_eq!(result(&rows[hit]).get("original_range").get("start").number(), 2);
    assert_eq!(result(&rows[copy]).get("original_hex").text(), hex(&source[2..14]));
}

#[test]
fn visible_scope_handles_raw_native_names_and_empty_is_never_all_files() {
    use std::os::unix::ffi::OsStringExt;
    let root = fixture(&[("ordinary.rs", b"fn target() {}\n")]);
    let raw = b"raw\xff.rs";
    fs::write(root.join(OsString::from_vec(raw.to_vec())), b"fn target() {}\n").unwrap();
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push(&format!("repo-begin-structure\t1\t1\tsymbols\texact\t10\t1000\tvisible\t{}\ttarget", hex(raw)), false);
    let found = s.push("repo-step\t1\t1\t0", false);
    s.push("repo-begin-structure\t1\t2\tsymbols\texact\t10\t1000\tvisible\t-\ttarget", false);
    let empty = s.push("repo-step\t1\t2\t0", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&rows[found]).get("retained_hits").number(), 1);
    assert_eq!(result(&rows[found]).get("scope_region_catalogued_files").number(), 1);
    assert_eq!(result(&rows[empty]).get("eligible_files").number(), 0);
    assert_eq!(result(&rows[empty]).get("retained_hits").number(), 0);
    assert_eq!(result(&rows[empty]).get("source_bytes_read").number(), 0);
}

#[test]
fn indexed_structure_uses_captured_content_after_live_replacement_and_index_clear() {
    struct Replace { bytes: Vec<u8>, flushes: usize, path: PathBuf }
    impl Write for Replace {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 3 { fs::write(&self.path, b"fn replacement() {}\n")?; }
            Ok(())
        }
    }
    let root = fixture(&[("main.rs", b"fn target() {}\n")]);
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-index-begin\t1\t1\t10\t1000\t1000", false);
    s.push("repo-index-step\t1\t1\t0", false);
    s.push("repo-begin-indexed-structure-text-hex\t1\t1\t2\tsymbols\texact\t10\t1000\tworkspace\t-\t746172676574", false);
    let done = s.push("repo-step\t1\t2\t0", false);
    let hit = s.push("repo-hit\t1\t2\t1", true);
    s.push("repo-index-clear\t1\t3", false);
    s.push("repo-close\t1", false);
    let view = s.push("view\t1", false); let copy = s.push("copy\t1", false); s.push("quit", false);
    let mut input = s.text.as_bytes(); let mut errors = Vec::new();
    let mut output = Replace { bytes: Vec::new(), flushes: 0, path: root.join("main.rs") };
    assert_eq!(run(&args(), &mut input, &mut output, &mut errors, || false), EXIT_OK);
    assert!(errors.is_empty());
    let rows = records(&output.bytes);
    assert_eq!(result(&rows[done]).get("search_strategy").text(), "retained-capture-structure");
    assert_eq!(result(&rows[done]).get("source_bytes_read").number(), 0);
    assert_eq!(result(&rows[done]).get("read_calls").number(), 0);
    assert_eq!(result(&rows[hit]).get("origin_source_revision").number(), 1);
    assert_eq!(result(&rows[view]).get("text").text(), "fn target() {}\n");
    assert_eq!(result(&rows[copy]).get("original_hex").text(), hex(b"target"));
}

#[test]
fn invalid_modes_scope_and_stale_steps_preserve_the_accepted_query() {
    let root = fixture(&[("main.rs", b"fn target() {}\n")]);
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-begin-structure\t1\t1\tsymbols\texact\t10\t1000\tworkspace\t-\ttarget", false);
    s.push("repo-step\t1\t1\t0", false);
    let scope = s.push("repo-begin-structure\t1\t2\tsymbols\texact\t10\t1000\tworkspace\t737263\ttarget", false);
    let traversal = s.push("repo-begin-structure\t1\t2\tsymbols\texact\t10\t1000\tdirectory\t2e2e\ttarget", false);
    let mode = s.push("repo-begin-structure\t1\t2\tunknown\texact\t10\t1000\tworkspace\t-\ttarget", false);
    let invalid = s.push("repo-begin-structure\t1\t2\toccurrences\tprefix\t10\t1000\tworkspace\t-\ttarget", false);
    s.push("repo-begin-structure\t1\t3\tsymbols\texact\t10\t1000\tworkspace\t-\ttarget", false);
    let stale = s.push("repo-step\t1\t3\t1", false);
    let canceled = s.push("repo-cancel\t1\t3", false);
    let page = s.push("repo-page\t1\t1\t0\t10", false);
    let too_many = s.push("repo-begin-structure\t1\t4\tsymbols\texact\t10\t1000\tworkspace\t-\ttarget\textra\textra", false);
    let old_bound = s.push("state\t1\t2\t3\t4\t5\t6\t7\t8\t9", false);
    s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(error(&rows[scope]), "DESK_STRUCTURE_SCOPE");
    assert_eq!(error(&rows[traversal]), "ATLAS_SEARCH_INVALID_SCOPE");
    assert_eq!(error(&rows[mode]), "DESK_STRUCTURE_MODE");
    assert_eq!(error(&rows[invalid]), "ATLAS_SEARCH_INVALID_LIMITS");
    assert_eq!(error(&rows[stale]), "DESK_REPOSITORY_STALE_STEP");
    assert!(!rows[canceled].get("repository_work_pending").flag());
    assert_eq!(result(&rows[page]).get("retained_hits").number(), 1);
    assert_eq!(rows[page].get("repository_query_generation").number(), 1);
    assert_eq!(error(&rows[too_many]), "DESK_FIELD_LIMIT");
    assert_eq!(error(&rows[old_bound]), "DESK_FIELD_LIMIT");
}

#[test]
fn pending_structural_work_and_terminal_limits_keep_distinct_exit_states() {
    let root = fixture(&[("main.rs", b"fn target() {}\nfn targets() {}\n")]);
    let open = format!("0\trepo-open\t{}\n", root.display());
    let begin = "0\trepo-begin-structure\t1\t1\tsymbols\tprefix\t1\t1000\tworkspace\t-\ttarget\n";
    assert_eq!(invoke(&(open.clone() + begin)).0, EXIT_PARTIAL);
    assert_eq!(invoke(&(open.clone() + begin + "0\trepo-cancel\t1\t1\n0\tquit\n")).0, EXIT_OK);
    let (exit, rows) = invoke(&(open + begin + "0\trepo-step\t1\t1\t0\n0\tquit\n"));
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!result(&rows[2]).get("search_in_progress").flag());
    assert!(!result(&rows[2]).get("search_complete").flag());
    assert!(result(&rows[2]).get("truncated").flag());
}

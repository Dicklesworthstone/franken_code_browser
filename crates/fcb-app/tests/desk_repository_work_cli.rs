#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{parse, Json};
use std::{ffi::OsString, fs, io::{self, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-work-cli-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
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
#[derive(Default)]
struct Script { text: String, revision: u64, count: usize }
impl Script {
    fn push(&mut self, command: &str, mutation: bool) -> usize {
        let row = self.count; self.count += 1;
        self.text.push_str(&format!("{}\t{command}\n", self.revision));
        if mutation { self.revision = self.count as u64; } row
    }
}

#[test]
fn provisional_search_can_open_pin_copy_and_cancel_before_other_files_are_scanned() {
    let root = fixture(); for name in ["a.rs", "b.rs"] { fs::write(root.join(name), b"old needle").unwrap(); }
    let mut s = Script::default(); s.push(&format!("repo-open\t{}", root.display()), false);
    let begin = s.push("repo-begin\t1\t1\t10\t1000\tneedle", false);
    let step = s.push("repo-step\t1\t1\t0", false);
    let page = s.push("repo-page\t1\t1\t0\t10", false);
    s.push("repo-hit\t1\t1\t1", true); s.push("pin\t1", true);
    let cancel = s.push("repo-cancel\t1\t1", false);
    let copy = s.push("copy\t1", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[begin].get("exit_code").number(), EXIT_PARTIAL as u64);
    assert_eq!(rows[begin].get("result").get("source_bytes_read").number(), 0);
    assert_eq!(rows[step].get("result").get("examined_files").number(), 1);
    assert!(rows[page].get("result").get("search_in_progress").flag());
    assert_eq!(rows[page].get("result").get("step_count").number(), 1);
    assert_eq!(rows[page].get("repository_pending_query_generation").number(), 1);
    assert!(!rows[cancel].get("repository_work_pending").flag());
    assert_eq!(rows[copy].get("result").get("original_hex").text(), "6e6565646c65");
}

#[test]
fn running_partial_responses_do_not_poison_a_subsequently_complete_session() {
    let root = fixture(); for name in ["a.rs", "b.rs"] { fs::write(root.join(name), b"needle").unwrap(); }
    let mut s = Script::default(); s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-begin\t1\t1\t10\t1000\tneedle", false); s.push("repo-progress\t1\t1", false);
    s.push("repo-step\t1\t1\t0", false); let terminal = s.push("repo-step\t1\t1\t1", false);
    s.push("quit", false); let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_OK);
    assert!(rows[terminal].get("result").get("search_complete").flag());
    assert_eq!(rows[terminal].get("repository_query_generation").number(), 1);
    assert!(!rows[terminal].get("repository_work_pending").flag());
}

#[test]
fn duplicate_steps_and_stale_cancel_return_errors_without_advancing_the_cursor() {
    let root = fixture(); for name in ["a.rs", "b.rs"] { fs::write(root.join(name), b"needle").unwrap(); }
    let mut s = Script::default(); s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-begin\t1\t1\t10\t1000\tneedle", false); s.push("repo-step\t1\t1\t0", false);
    let duplicate = s.push("repo-step\t1\t1\t0", false); let progress = s.push("repo-progress\t1\t1", false);
    s.push("repo-begin\t1\t2\t10\t1000\tneedle", false);
    let stale = s.push("repo-cancel\t1\t1", false); s.push("repo-cancel\t1\t2", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[duplicate].get("error").get("code").text(), "DESK_REPOSITORY_STALE_STEP");
    assert_eq!(rows[progress].get("result").get("step_count").number(), 1);
    assert_eq!(rows[progress].get("result").get("examined_files").number(), 1);
    assert_eq!(rows[stale].get("error").get("code").text(), "ATLAS_SEARCH_STALE_QUERY");
    assert_eq!(rows[stale].get("repository_pending_query_generation").number(), 2);
}

#[test]
fn unfinished_work_at_eof_or_quit_is_partial_but_explicit_cancel_is_successful() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    for begin in ["repo-begin\t1\t1\t10\t1000\tneedle", "repo-index-begin\t1\t1\t10\t1000\t1000"] {
        let script = format!("0\trepo-open\t{}\n0\t{begin}\n", root.display());
        assert_eq!(invoke(&script).0, EXIT_PARTIAL);
        let (exit, rows) = invoke(&(script.clone() + "0\tquit\n")); assert_eq!(exit, EXIT_PARTIAL);
        assert_eq!(rows.last().unwrap().get("exit_code").number(), EXIT_PARTIAL as u64);
        let cancel = if begin.starts_with("repo-index") { "repo-index-cancel" } else { "repo-cancel" };
        assert_eq!(invoke(&(script + &format!("0\t{cancel}\t1\t1\n0\tquit\n"))).0, EXIT_OK);
    }
}

#[test]
fn terminal_partial_local_search_is_not_hidden_by_the_previous_running_repo_reply() {
    let root = fixture(); let path = root.join("a.rs"); fs::write(&path, b"a a a").unwrap();
    let mut s = Script::default(); s.push(&format!("repo-open\t{}", root.display()), false);
    s.push(&format!("open\t{}", path.display()), true);
    s.push("repo-begin\t1\t1\t10\t1000\ta", false);
    let local = s.push("find\t1\t1\t1\t100\ta", false);
    s.push("repo-cancel\t1\t1", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(rows[local].get("exit_code").number(), EXIT_PARTIAL as u64);
    assert!(!rows[local].get("result").get("search_complete").flag());
}

#[test]
fn indexed_queries_and_repeated_desk_imports_work_after_live_path_disappears() {
    struct Rename { bytes: Vec<u8>, flushes: usize, source: PathBuf, moved: PathBuf }
    impl Write for Rename {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1; if self.flushes == 3 { fs::rename(&self.source, &self.moved)?; } Ok(())
        }
    }
    let root = fixture(); let path = root.join("a.rs"); fs::write(&path, b"old needle").unwrap();
    let mut s = Script::default(); s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-index-begin\t1\t1\t10\t1000\t1000", false); s.push("repo-index-step\t1\t1\t0", false);
    s.push("repo-begin-indexed\t1\t1\t2\t10\t1000\tneedle", false);
    let query = s.push("repo-step\t1\t2\t0", false); s.push("repo-hit\t1\t2\t1", true);
    s.push("repo-find-indexed\t1\t1\t3\t10\t1000\told", false);
    let reuse = s.push("repo-hit\t1\t3\t1", true); s.push("repo-index-clear\t1\t4", false);
    let view = s.push("view\t1", false); s.push("quit", false);
    let mut output = Rename { bytes: Vec::new(), flushes: 0, source: path.clone(), moved: root.join("moved.rs") };
    let mut errors = Vec::new(); let mut input = s.text.as_bytes();
    assert_eq!(run(&args(), &mut input, &mut output, &mut errors, || false), EXIT_OK); assert!(errors.is_empty());
    let rows = records(&output.bytes);
    assert_eq!(rows[query].get("result").get("source_bytes_read").number(), 0);
    assert_eq!(rows[query].get("result").get("read_calls").number(), 0);
    assert_eq!(rows[query].get("result").get("search_strategy").text(), "retained-ephemeral-index");
    assert!(rows[reuse].get("result").get("reused_source").flag());
    assert_eq!(rows[view].get("result").get("text").text(), "old needle"); assert!(!path.exists());
}

#[test]
fn old_index_queries_continue_while_a_replacement_is_built_then_canceled() {
    let root = fixture(); for name in ["a.rs", "b.rs"] { fs::write(root.join(name), b"needle").unwrap(); }
    let mut s = Script::default(); s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-index-begin\t1\t1\t10\t1000\t1000", false);
    s.push("repo-index-step\t1\t1\t0", false); s.push("repo-index-step\t1\t1\t1", false);
    s.push("repo-index-begin\t1\t2\t10\t1000\t1000", false);
    s.push("repo-begin-indexed\t1\t1\t3\t10\t1000\tneedle", false); s.push("repo-step\t1\t3\t0", false);
    s.push("repo-index-step\t1\t2\t0", false); let cancel = s.push("repo-index-cancel\t1\t2", false);
    let done = s.push("repo-step\t1\t3\t1", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[cancel].get("repository_index_generation").number(), 1);
    assert_eq!(rows[cancel].get("repository_pending_query_generation").number(), 3);
    assert!(rows[done].get("result").get("search_complete").flag());
}

#[test]
fn stale_root_and_index_step_tokens_do_not_cancel_a_new_repository_build() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let mut s = Script::default(); s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-index-begin\t1\t1\t10\t1000\t1000", false);
    s.push(&format!("repo-open\t{}", root.display()), false); s.push("repo-index-begin\t3\t1\t10\t1000\t1000", false);
    let old = s.push("repo-index-cancel\t1\t1", false); let future = s.push("repo-index-step\t3\t1\t9", false);
    let info = s.push("repo-index-info\t3\t1", false); s.push("repo-index-step\t3\t1\t0", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[old].get("error").get("code").text(), "DESK_STALE_REPOSITORY");
    assert_eq!(rows[future].get("error").get("code").text(), "DESK_REPOSITORY_STALE_STEP");
    assert_eq!(rows[info].get("result").get("build_steps").number(), 0);
    assert_eq!(rows[info].get("repository_pending_index_generation").number(), 1);
}

#[test]
fn quota_stops_and_index_fallback_remain_explicit_terminal_partial_results() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle needle").unwrap();
    let mut s = Script::default(); s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-begin\t1\t1\t1\t1000\tneedle", false); let limit = s.push("repo-step\t1\t1\t0", false);
    s.push("repo-index-begin\t1\t2\t10\t1000\t0", false); let fallback = s.push("repo-index-step\t1\t2\t0", false);
    let exact = s.push("repo-find-indexed\t1\t2\t3\t10\t1000\tneedle", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_PARTIAL);
    assert!(!rows[limit].get("result").get("search_in_progress").flag());
    assert!(!rows[limit].get("result").get("search_complete").flag());
    assert!(rows[fallback].get("result").get("uncovered_files").number() > 0);
    assert!(rows[exact].get("result").get("search_complete").flag());
    assert_eq!(rows[exact].get("result").get("retained_hits").number(), 2);
}

#[test]
fn native_paths_and_hex_utf8_needles_select_exact_utf16_bytes_from_an_index() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let base = fixture(); let root = base.join(OsString::from_vec(b"root\t\xff".to_vec())); fs::create_dir(&root).unwrap();
    let mut bytes = vec![0xff, 0xfe]; for unit in "x\r\n😀".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(root.join("a.rs"), bytes).unwrap();
    let path_hex = root.as_os_str().as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let mut s = Script::default(); s.push(&format!("repo-open-hex\t{path_hex}"), false);
    s.push("repo-index-begin\t1\t1\t10\t1000\t1000", false); s.push("repo-index-step\t1\t1\t0", false);
    s.push("repo-begin-indexed-text-hex\t1\t1\t2\t10\t1000\tf09f9880", false);
    let completed = s.push("repo-step\t1\t2\t0", false); let hit = s.push("repo-hit\t1\t2\t1", true);
    let copy = s.push("copy\t1", false); s.push("quit", false);
    // UTF-16 is captured but uses exact-scan fallback, so index preparation
    // is explicitly partial even though the decoded literal query completes.
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(rows[completed].get("exit_code").number(), EXIT_OK as u64);
    assert!(rows[completed].get("result").get("search_complete").flag());
    assert_eq!(rows[completed].get("result").get("fallback_files").number(), 1);
    assert_eq!(rows[hit].get("result").get("original_range").get("start").number(), 8);
    assert_eq!(rows[copy].get("result").get("original_hex").text(), "3dd800de");
}

#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Real Desk stdio discovery/attachment routes for fcb-zebl.1.
//! Code-first cases; independent strict-RCH execution remains pending.
mod support;
use support::{parse, Json};
use std::{cell::Cell, ffi::OsString, fs, io::{self, Read, Write},
    os::unix::{ffi::{OsStrExt, OsStringExt}, fs::symlink}, path::{Path, PathBuf},
    rc::Rc, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};

fn fixture(files: &[(&str, &[u8])]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-desk-discovery-cli-{}-{}-{}",
        std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    for (name, bytes) in files { file(&root, name, bytes); }
    root
}
fn file(root: &Path, path: impl AsRef<Path>, bytes: &[u8]) {
    let path = root.join(path); fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
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
struct Replace { bytes: Vec<u8>, replies: usize, after: usize, path: PathBuf }
impl Write for Replace {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
    fn flush(&mut self) -> io::Result<()> {
        self.replies += 1;
        if self.replies == self.after { fs::write(&self.path, b"changed current source\n")?; }
        Ok(())
    }
}

#[test]
fn raw_root_and_directory_open_routes_search_metadata_and_detached_reader_to_original_bytes() {
    let parent = fixture(&[]);
    let root = parent.join(OsString::from_vec(b"root\xfe".to_vec())); fs::create_dir(&root).unwrap();
    let directory = b"C:\\raw\xff"; let native = OsString::from_vec(directory.to_vec());
    let path = root.join(&native).join("source.rs");
    file(&root, Path::new(&native).join("source.rs"), b"original NEEDLE\n");
    file(&root, "outside/source.rs", b"outside needle\n");
    let mut s = Script::default();
    let opened = s.push(&format!("repo-open-scoped-hex\t{}\tinclude-excluded\t1\tdirectory\t{}",
        hex(root.as_os_str().as_bytes()), hex(directory)), false);
    let info = s.push("repo-info\t1", false);
    let paths = s.push("repo-paths\t1\t1\texact\tsensitive\t10\tsource.rs", false);
    let begin = s.push("repo-begin-matching\t1\t1\tunicode-lowercase\tbounded\t10\t1000\tworkspace\t-\tneedle", false);
    let done = s.push("repo-step\t1\t1\t0", false);
    let hit = s.push("repo-hit\t1\t1\t1", true);
    let close = s.push("repo-close\t1", false);
    let copy = s.push("copy\t1", false); s.push("quit", false);
    let mut input = s.text.as_bytes();
    let mut output = Replace { bytes: Vec::new(), replies: 0, after: done + 1, path };
    let mut errors = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut output, &mut errors, || false), EXIT_OK);
    assert!(errors.is_empty());
    let rows = records(&output.bytes);
    let root_hex = hex(fs::canonicalize(&root).unwrap().as_os_str().as_bytes());
    let mut member = directory.to_vec(); member.extend_from_slice(b"/source.rs");
    for row in [opened, info, paths, begin, done] {
        let receipt = result(&rows[row]);
        assert_eq!(receipt.get("discovery_scope").text(), "directory");
        assert_eq!(receipt.get("discovery_directory").get("hex").text(), hex(directory));
        assert_eq!(receipt.get("path_basis").text(), "project-root");
        assert_eq!(rows[row].get("repository_token").number(), 1);
    }
    assert_eq!(result(&rows[opened]).get("root_path").get("hex").text(), root_hex);
    assert_eq!(result(&rows[opened]).get("catalogued_files").number(), 1);
    assert!(result(&rows[opened]).get("discovery_complete").flag());
    assert!(!result(&rows[paths]).get("source_payload_read").flag());
    assert_eq!(result(&rows[paths]).get("hits").array()[0].get("path").get("hex").text(), hex(&member));
    assert_eq!(result(&rows[begin]).get("source_bytes_read").number(), 0);
    assert!(result(&rows[done]).get("search_complete").flag());
    assert_eq!(result(&rows[done]).get("retained_hits").number(), 1);
    assert_eq!(result(&rows[done]).get("hits").array()[0].get("path").get("hex").text(), hex(&member));
    assert!(!result(&rows[hit]).get("source_reopened").flag());
    assert_eq!(rows[close].get("repository_token"), &Json::Null);
    assert_eq!(result(&rows[copy]).get("original_hex").text(), hex(b"NEEDLE"));
}

#[test]
fn workspace_defaults_and_inherited_directory_rules_share_explicit_policy_routes() {
    let root = fixture(&[(".gitignore", b"ignored/\n"), ("tracked.rs", b"visible needle"),
        ("ignored/selected/source.rs", b"private needle"), ("ignored/selected/.gitignore", b"!source.rs\n")]);
    let mut s = Script::default();
    let legacy = s.push(&format!("repo-open\t{}", root.display()), false);
    let legacy_hits = s.push("repo-find\t1\t1\t10\t1000\tneedle", false);
    let explicit = s.push(&format!("repo-open-scoped\t{}\trepository-rules\t4096\tworkspace\t-", root.display()), false);
    let explicit_token = explicit + 1;
    let explicit_hits = s.push(&format!("repo-find\t{explicit_token}\t1\t10\t1000\tneedle"), false);
    let all = s.push(&format!("repo-open-scoped\t{}\tinclude-excluded\t32\tworkspace\t-", root.display()), false);
    let all_token = all + 1;
    let all_hits = s.push(&format!("repo-find\t{all_token}\t1\t10\t1000\tneedle"), false);
    let blocked = s.push(&format!("repo-open-scoped\t{}\trepository-rules\t32\tdirectory\t{}",
        root.display(), hex(b"ignored/selected")), false);
    let blocked_token = blocked + 1;
    let blocked_hits = s.push(&format!("repo-find\t{blocked_token}\t1\t10\t1000\tprivate needle"), false);
    let focused = s.push(&format!("repo-open-scoped\t{}\tinclude-excluded\t32\tdirectory\t{}",
        root.display(), hex(b"ignored/selected")), false);
    let token = focused + 1;
    let focused_hits = s.push(&format!("repo-find\t{token}\t1\t10\t1000\tprivate needle"), false);
    s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(result(&rows[legacy]).get("discovery_policy"), result(&rows[explicit]).get("discovery_policy"));
    assert_eq!(result(&rows[legacy]).get("catalogued_files"), result(&rows[explicit]).get("catalogued_files"));
    for row in [legacy_hits, explicit_hits] { assert_eq!(result(&rows[row]).get("retained_hits").number(), 1); }
    assert_eq!(result(&rows[all_hits]).get("retained_hits").number(), 2);
    assert_eq!(result(&rows[blocked_hits]).get("retained_hits").number(), 0);
    assert!(!result(&rows[blocked_hits]).get("search_complete").flag());
    assert!(result(&rows[focused_hits]).get("search_complete").flag());
    assert_eq!(result(&rows[focused_hits]).get("retained_hits").number(), 1);
    for row in [all, focused] {
        assert!(!result(&rows[row]).get("rule_files_enabled").flag());
        assert_eq!(result(&rows[row]).get("rule_policy"), &Json::Null);
    }
    for row in [legacy, explicit, all, blocked, focused] {
        assert_eq!(rows[row].get("repository_token").number(), row as u64 + 1);
        assert_eq!(rows[row].get("accepted_revision").number(), 0);
    }
}

#[test]
fn failed_replacements_keep_the_previous_token_queries_and_reader_until_successful_attachment() {
    let root = fixture(&[("main.rs", b"original needle\n")]);
    symlink(root.join("main.rs"), root.join("alias")).unwrap();
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-find\t1\t1\t10\t1000\tneedle", false);
    s.push("repo-hit\t1\t1\t1", true);
    let mut invalid = Vec::new();
    for (tail, code) in [
        ("other\t1\tworkspace\t-", "DESK_REPOSITORY_POLICY"),
        ("repository-rules\t0\tworkspace\t-", "DESK_REPOSITORY_FILE_LIMIT"),
        ("repository-rules\t20001\tworkspace\t-", "DESK_REPOSITORY_FILE_LIMIT"),
        ("repository-rules\t1\tworkspace\t6d61696e", "DESK_REPOSITORY_SCOPE"),
        ("repository-rules\t1\tfile\t6d61696e2e7273", "DESK_REPOSITORY_SCOPE"),
        ("repository-rules\t1\tdirectory\t", "DESK_INVALID_HEX"),
        ("repository-rules\t1\tdirectory\tzz", "DESK_INVALID_HEX"),
        ("repository-rules\t1\tdirectory\t2e2e", "ATLAS_SESSION_INVALID_LIMITS"),
        ("repository-rules\t1\tdirectory\t00", "ATLAS_SESSION_INVALID_LIMITS"),
    ] {
        let row = s.push(&format!("repo-open-scoped\t{}\t{tail}", root.display()), false);
        invalid.push((row, code));
    }
    let mut unavailable = Vec::new();
    for directory in [b"missing".as_slice(), b"alias", b"main.rs"] {
        unavailable.push(s.push(&format!("repo-open-scoped\t{}\tinclude-excluded\t1\tdirectory\t{}",
            root.display(), hex(directory)), false));
    }
    let page = s.push("repo-page\t1\t1\t0\t10", false);
    let copied = s.push("copy\t1", false);
    let replacement = s.push(&format!("repo-open-scoped\t{}\tinclude-excluded\t20000\tworkspace\t-", root.display()), false);
    let token = replacement + 1;
    let stale = s.push("repo-info\t1", false);
    let info = s.push(&format!("repo-info\t{token}"), false);
    s.push(&format!("repo-close\t{token}"), false);
    let detached_copy = s.push("copy\t1", false); s.push("quit", false);
    let (exit, rows) = invoke(&s.text); assert_eq!(exit, EXIT_ERROR);
    for (row, code) in invalid {
        assert_eq!(error(&rows[row]), code);
        assert_eq!(rows[row].get("repository_token").number(), 1);
        assert_eq!(rows[row].get("repository_query_generation").number(), 1);
        assert_eq!(rows[row].get("accepted_revision").number(), 3);
    }
    for row in unavailable {
        assert!(!error(&rows[row]).is_empty());
        assert_eq!(rows[row].get("repository_token").number(), 1);
        assert_eq!(rows[row].get("accepted_revision").number(), 3);
    }
    assert_eq!(result(&rows[page]).get("retained_hits").number(), 1);
    assert_eq!(error(&rows[stale]), "DESK_STALE_REPOSITORY");
    assert_eq!(rows[replacement].get("repository_token").number(), token as u64);
    assert_eq!(rows[replacement].get("repository_query_generation"), &Json::Null);
    assert_eq!(result(&rows[info]).get("discovery_scope").text(), "workspace");
    for row in [copied, detached_copy] {
        assert_eq!(result(&rows[row]).get("original_hex").text(), hex(b"needle"));
        assert_eq!(rows[row].get("accepted_revision").number(), 3);
    }
}

struct ArmAfterLine<'a> { input: &'a [u8], lines: Rc<Cell<usize>> }
impl Read for ArmAfterLine<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() { return Ok(0); }
        // Desk buffers stdin; expose each newline only when read_frame consumes it.
        let count = self.input.read(&mut buffer[..1])?;
        self.lines.set(self.lines.get() + buffer[..count].iter().filter(|&&b| b == b'\n').count());
        Ok(count)
    }
}

#[test]
fn canceled_directory_candidate_reports_the_prior_attachment_and_accepted_reader_revision() {
    let root = fixture(&[("selected/source.rs", b"original needle\n")]);
    let mut s = Script::default();
    s.push(&format!("repo-open\t{}", root.display()), false);
    s.push("repo-find\t1\t1\t10\t1000\tneedle", false);
    s.push("repo-hit\t1\t1\t1", true);
    let copied = s.push("copy\t1", false);
    let canceled = s.push(&format!("repo-open-scoped\t{}\tinclude-excluded\t1\tdirectory\t73656c6563746564",
        root.display()), false);
    s.push("quit", false);
    let lines = Rc::new(Cell::new(0));
    let mut input = ArmAfterLine { input: s.text.as_bytes(), lines: lines.clone() };
    let mut output = Vec::new(); let mut errors = Vec::new(); let mut stops = 0;
    let exit = run(&args(), &mut input, &mut output, &mut errors, || {
        if lines.get() == canceled + 1 { stops += 1; true } else { false }
    });
    assert_eq!(exit, EXIT_CANCELED); assert!(errors.is_empty()); assert_eq!(stops, 1);
    let rows = records(&output); assert_eq!(rows.len(), canceled + 1);
    assert_eq!(result(&rows[copied]).get("original_hex").text(), hex(b"needle"));
    assert_eq!(error(&rows[canceled]), "DESK_REPOSITORY_CANCELED");
    assert_eq!(rows[canceled].get("repository_token").number(), 1);
    assert_eq!(rows[canceled].get("repository_query_generation").number(), 1);
    assert_eq!(rows[canceled].get("accepted_revision").number(), 3);
}

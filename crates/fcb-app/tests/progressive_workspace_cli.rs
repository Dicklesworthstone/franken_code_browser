#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Public CLI regressions for interleaved discovery and whole-file search.
//! Oracles are original bytes/counts, not another catalog search implementation.
mod support;
use std::{cell::Cell, ffi::OsString, fs, io::{self, Read, Write}, path::{Path, PathBuf}, rc::Rc,
    sync::atomic::{AtomicU64, Ordering}};
use fcb_app::{run, EXIT_CANCELED, EXIT_ERROR, EXIT_NO_MATCH, EXIT_OK, EXIT_PARTIAL};
use support::{Json, parse};

struct Tree(PathBuf);
impl Tree {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-progressive-cli-{label}-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, name: impl AsRef<Path>, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name); fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap(); path
    }
    // Preserve the fixture for the independent qualification runner.
}
struct NoStdin;
impl Read for NoStdin { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("workspace search consumed stdin") } }
fn arguments(root: &Path, flags: &[&str], output: Option<&str>) -> Vec<OsString> {
    let mut args = vec!["search".into(), root.as_os_str().to_owned(), "--workspace".into(),
        "--whole-file".into(), "--progressive".into(), "--respect-ignores".into()];
    args.extend(flags.iter().map(OsString::from));
    if let Some(output) = output { args.push(output.into()); }
    args
}
fn invoke(root: &Path, flags: &[&str]) -> (u8, Json) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&arguments(root, flags, Some("--json")), &mut NoStdin, &mut out, &mut err, || false);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    let json = parse(&out).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out)));
    assert_eq!(json.get("schema").text(), "fcb.progressive-search/1");
    (code, json)
}
fn records(bytes: &[u8]) -> Vec<Json> {
    assert!(bytes.ends_with(b"\n"), "incomplete record");
    let rows: Vec<_> = bytes.split(|b| *b == b'\n').filter(|row| !row.is_empty()).map(|row| parse(row).unwrap()).collect();
    for (sequence, row) in rows.iter().enumerate() {
        assert_eq!(row.get("schema").text(), "fcb.progressive-search-stream/1");
        assert_eq!(row.get("sequence").number(), sequence as u64);
        assert_eq!(row.get("owner").number(), 1); assert_eq!(row.get("query_generation").number(), 1);
    }
    assert_eq!(rows.iter().filter(|row| row.get("type").text() == "terminal").count(), 1);
    assert_eq!(rows.last().unwrap().get("type").text(), "terminal"); rows
}
fn stream(root: &Path, flags: &[&str]) -> (u8, Vec<Json>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&arguments(root, flags, Some("--stream")), &mut NoStdin, &mut out, &mut err, || false);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err)); (code, records(&out))
}
fn bounds(hit: &Json) -> (u64, u64) {
    (hit.get("original_range").get("start").number(), hit.get("original_range").get("end").number())
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    format!("\u{feff}{text}").encode_utf16().flat_map(|unit|
        if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}

#[test]
fn searches_beyond_the_old_cumulative_path_quota_with_bounded_retention() {
    let tree = Tree::new("path-quota");
    let mut path_bytes = 0usize;
    for index in 0..3000 {
        let name = format!("file-{index:04}-{}.rs", "p".repeat(190));
        path_bytes += name.len(); tree.file(&name, b"x");
    }
    assert!(path_bytes > 512 * 1024);
    let (code, result) = invoke(&tree.0, &["--text", "x", "--count-all", "--limit", "4"]);
    assert_eq!(code, EXIT_PARTIAL); // Counts complete; the retained rows are intentionally bounded.
    let summary = result.get("summary");
    assert!(summary.get("scan_complete").flag() && summary.get("match_count_complete").flag());
    assert_eq!(summary.get("catalogued_files").number(), 3000);
    assert_eq!(summary.get("files_examined").number(), 3000);
    assert_eq!(summary.get("matches_seen").number(), 3000);
    assert!(summary.get("retained_catalog_pages").number() <= 4);
    assert!(summary.get("retained_catalog_files").number() <= 1024);
    assert!(summary.get("discovery_pages").number() > 4);
    assert_eq!(result.get("aggregate_file_limit"), &Json::Null);
    assert_eq!(result.get("aggregate_path_byte_limit"), &Json::Null);
    assert_eq!(result.get("discovery_limits").get("max_path_bytes").number(), 2048);
    for (position, hit) in result.get("hits").array().iter().enumerate() {
        assert_eq!(hit.get("file_id").number(), position as u64 + 1);
        assert_eq!(bounds(hit), (0, 1)); assert_eq!(hit.get("original_hex").text(), "78");
    }
    // A later occurrence window reaches entries beyond the old path budget,
    // without depending on lexical/insertion order of a directory iterator.
    let (_, last) = invoke(&tree.0, &["--text", "x", "--count-all", "--limit", "4", "--skip-matches", "2996"]);
    assert_eq!(last.get("hits").array().iter().map(|hit| hit.get("file_id").number()).collect::<Vec<_>>(),
        [2997, 2998, 2999, 3000]);
    assert!(last.get("summary").get("scan_complete").flag());
    assert_eq!(last.get("summary").get("next_match_offset"), &Json::Null);
    // The existing explicit frozen route keeps its schema and finite catalog.
    let mut args = arguments(&tree.0, &["--text", "x", "--count-all", "--limit", "4", "--max-files", "65536"], Some("--json"));
    args.retain(|arg| arg != "--progressive");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run(&args, &mut NoStdin, &mut out, &mut err, || false), EXIT_PARTIAL);
    assert!(err.is_empty()); let old = parse(&out).unwrap();
    assert_eq!(old.get("schema").text(), "fcb.cli/1");
    assert!(!old.get("discovery_complete").flag());
    assert!(old.get("known_files").number() < 3000);
}

#[test]
fn utf16_lowercase_identifiers_and_raw_paths_keep_exact_witnesses() {
    use std::os::unix::ffi::OsStringExt;
    let tree = Tree::new("encoding");
    let raw_name = vec![b'a', 0xff, b'.', b'r', b's'];
    tree.file(OsString::from_vec(raw_name.clone()), &utf16("K K k", false));
    let (code, result) = invoke(&tree.0, &["--text", "k", "--ignore-case", "--count-all"]);
    assert_eq!(code, EXIT_OK);
    let hits = result.get("hits").array(); assert_eq!(hits.len(), 3);
    for ((hit, range), witness) in hits.iter().zip([(2, 4), (6, 8), (10, 12)]).zip(["212a", "004b", "006b"]) {
        assert_eq!(bounds(hit), range); assert_eq!(hit.get("original_hex").text(), witness);
        assert_eq!(hit.get("encoding").text(), "utf16be");
        assert_eq!(hit.get("path").get("hex").text(), "61ff2e7273");
    }
    assert_eq!(hits.iter().map(|hit| hit.get("matched_text").text()).collect::<Vec<_>>(), ["K", "K", "k"]);
    let (code, streamed) = stream(&tree.0, &["--text", "k", "--ignore-case", "--count-all"]);
    assert_eq!(code, EXIT_OK);
    let rows: Vec<_> = streamed.iter().filter(|row| row.get("type").text() == "match").map(|row| row.get("match")).collect();
    assert_eq!(rows, hits.iter().collect::<Vec<_>>());
    assert!(streamed.last().unwrap().get("results_complete").flag());

    let tokens = Tree::new("identifiers");
    tokens.file("source.rs", &utf16("foo foobar foo_ Foo foo", true));
    let (code, result) = invoke(&tokens.0, &["--identifier", "foo"]);
    assert_eq!(code, EXIT_OK);
    assert_eq!(result.get("hits").array().iter().map(bounds).collect::<Vec<_>>(), [(2, 8), (42, 48)]);
    assert!(!result.get("compiler_resolved").flag());
}

#[test]
fn expression_scope_ignores_and_incomplete_sources_never_create_false_negatives() {
    let tree = Tree::new("expression");
    tree.file(".gitignore", b"ignored.rs\n");
    tree.file("src/good.rs", &utf16("needle required needle", true));
    tree.file("src/ignored.rs", b"needle required");
    tree.file("src/forbidden.rs", b"needle required forbidden");
    tree.file("src/other.txt", b"needle required");
    tree.file("src/malformed.rs", b"needle required\xff");
    let flags = ["--query", "needle required -forbidden path:src lang:rust", "--limit", "2"];
    let (code, result) = invoke(&tree.0, &flags);
    assert_eq!(code, EXIT_PARTIAL);
    assert_eq!(result.get("hits").array().len(), 2);
    assert!(result.get("hits").array().iter().all(|hit| hit.get("path").get("display").text() == "src/good.rs"));
    assert_eq!(result.get("unsupported_files").number(), 1);
    assert_eq!(result.get("summary").get("metadata_excluded_files").number(), 2);
    assert!(result.get("count_all_matches").flag());
    assert!(!result.get("summary").get("scan_complete").flag());
    assert!(result.get("discovery").get("excluded_entries").number() > 0);
    assert_eq!(result.get("diagnostics").array().len(), 1);
    assert_eq!(result.get("diagnostics").array()[0].get("file_state").text(), "unsupported-text");

    let partial = Tree::new("partial-expression");
    partial.file("large.rs", format!("{}required", "needle ".repeat(100)).as_bytes());
    let (code, result) = invoke(&partial.0, &["--query", "needle required", "--max-scan-bytes", "64"]);
    assert_eq!(code, EXIT_PARTIAL); assert!(result.get("hits").array().is_empty());
    assert_eq!(result.get("matches_seen").number(), 0);
    assert_eq!(result.get("payload_bytes_read").number(), 64);
    assert_eq!(result.get("stop_reason").text(), "byte-limit");

    let bad_rules = Tree::new("bad-rules");
    bad_rules.file("bad/.gitignore", b"[unsupported"); bad_rules.file("bad/hidden.rs", b"needle");
    bad_rules.file("safe.rs", b"other");
    let (code, rows) = stream(&bad_rules.0, &["--text", "needle"]);
    assert_eq!(code, EXIT_PARTIAL);
    let terminal = rows.last().unwrap();
    assert_eq!(terminal.get("matches_seen").number(), 0);
    assert_eq!(terminal.get("failed_rule_files").number(), 1);
    assert!(!terminal.get("rule_policy_complete").flag() && !terminal.get("discovery_complete").flag());
}

#[test]
fn traversal_omissions_and_result_windows_remain_explicit() {
    let deep = Tree::new("depth");
    let nested = std::iter::repeat_n("d", 35).collect::<Vec<_>>().join("/");
    deep.file(format!("{nested}/hidden.rs"), b"needle");
    deep.file("safe.rs", b"other");
    let (code, result) = invoke(&deep.0, &["--text", "needle"]);
    assert_eq!(code, EXIT_PARTIAL); assert!(result.get("hits").array().is_empty());
    assert!(!result.get("summary").get("discovery_complete").flag());
    assert!(result.get("summary").get("discovery").get("depth_limited").number() > 0);
    let window = Tree::new("window"); window.file("source.rs", b"banana banana banana");
    let (code, result) = invoke(&window.0, &["--text", "ana", "--skip-matches", "3", "--limit", "2", "--count-all"]);
    assert_eq!(code, EXIT_PARTIAL);
    assert_eq!(result.get("hits").array().iter().map(bounds).collect::<Vec<_>>(), [(10, 13), (15, 18)]);
    assert_eq!(result.get("summary").get("matches_seen").number(), 6);
    assert_eq!(result.get("summary").get("next_match_offset").number(), 5);
    assert_eq!(result.get("retained_witness_bytes").number(), 3);
    assert!(result.get("summary").get("scan_complete").flag());
    let (code, absent) = invoke(&window.0, &["--text", "absent"]);
    assert_eq!(code, EXIT_NO_MATCH); assert!(absent.get("summary").get("results_complete").flag());
}

enum Action { CancelMatch, CancelActive, Rewrite(PathBuf), BreakMatch, FailFlush }
struct Observer {
    bytes: Vec<u8>, record_start: usize, acted: bool, poisoned: bool,
    canceled: Rc<Cell<bool>>, action: Action,
}
impl Write for Observer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(!self.poisoned, "continued delivery after the first transport failure");
        if matches!(self.action, Action::BreakMatch) && bytes.windows(b"\"type\":\"match\"".len()).any(|b| b == b"\"type\":\"match\"") {
            self.bytes.extend_from_slice(&bytes[..bytes.len().min(11)]);
            self.poisoned = true;
            // A writer can fail after an unknown partial transfer; no repair is valid.
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed consumer"));
        }
        self.bytes.extend_from_slice(bytes);
        if bytes.ends_with(b"\n") {
            let row = parse(&self.bytes[self.record_start..]).unwrap(); self.record_start = self.bytes.len();
            if !self.acted {
                let kind = row.get("type").text();
                match &self.action {
                    Action::CancelMatch if kind == "match" => { self.canceled.set(true); self.acted = true; }
                    Action::CancelActive if kind == "progress" && row.get("search").get("payload_bytes_read").number() > 0
                        && row.get("search").get("retained_hits").number() == 0 => { self.canceled.set(true); self.acted = true; }
                    Action::Rewrite(path) if kind == "match" => { fs::write(path, b"changed live bytes").unwrap(); self.acted = true; }
                    Action::FailFlush if kind == "match" => { self.acted = true; }
                    _ => {},
                }
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if matches!(self.action, Action::FailFlush) && self.acted {
            self.poisoned = true; return Err(io::Error::new(io::ErrorKind::BrokenPipe, "failed flush"));
        }
        Ok(())
    }
}
fn observed(root: &Path, flags: &[&str], action: Action) -> (u8, Observer, Vec<u8>) {
    let canceled = Rc::new(Cell::new(false));
    let mut out = Observer { bytes: Vec::new(), record_start: 0, acted: false, poisoned: false,
        canceled: Rc::clone(&canceled), action };
    let mut err = Vec::new();
    let code = run(&arguments(root, flags, Some("--stream")), &mut NoStdin, &mut out, &mut err, || canceled.get());
    (code, out, err)
}

#[test]
fn cancellation_preserves_only_finalized_counts_and_cleanly_delivered_witnesses() {
    let tree = Tree::new("cancel-finalized"); tree.file("source.rs", b"needle needle");
    let (code, out, err) = observed(&tree.0, &["--text", "needle"], Action::CancelMatch);
    assert_eq!(code, EXIT_CANCELED); assert!(out.acted && err.is_empty());
    let rows = records(&out.bytes); let terminal = rows.last().unwrap();
    assert_eq!(terminal.get("matches_seen").number(), 2);
    assert_eq!(terminal.get("retained_hits").number(), 2);
    assert_eq!(terminal.get("delivered_hits").number(), 1);
    assert!(!terminal.get("results_complete").flag());
    assert_eq!(terminal.get("stop_reason").text(), "canceled");

    let large = Tree::new("cancel-active");
    let mut bytes = b"needle ".to_vec(); bytes.extend(vec![b'x'; 6 * 1024 * 1024]); bytes.extend_from_slice(b" required");
    large.file("source.rs", &bytes);
    let (code, out, err) = observed(&large.0, &["--query", "needle required"], Action::CancelActive);
    assert_eq!(code, EXIT_CANCELED); assert!(out.acted && err.is_empty());
    let rows = records(&out.bytes); let terminal = rows.last().unwrap();
    assert!(terminal.get("payload_bytes_read").number() > 0);
    assert_eq!(terminal.get("matches_seen").number(), 0);
    assert_eq!(terminal.get("delivered_hits").number(), 0);
    assert_eq!(terminal.get("files_unexamined").number(), 1);
    assert!(!terminal.get("match_count_complete").flag());
}

#[test]
fn delivered_witnesses_survive_live_edit_and_poisoned_output_never_gets_a_terminal() {
    let tree = Tree::new("delivery"); let source = tree.file("source.rs", b"needle needle");
    let (code, out, err) = observed(&tree.0, &["--text", "needle"], Action::Rewrite(source.clone()));
    assert_eq!(code, EXIT_OK); assert!(out.acted && err.is_empty());
    let rows = records(&out.bytes);
    let hits: Vec<_> = rows.iter().filter(|row| row.get("type").text() == "match").map(|row| row.get("match")).collect();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|hit| hit.get("original_hex").text() == "6e6565646c65"));
    assert_eq!(fs::read(&source).unwrap(), b"changed live bytes");
    for action in [Action::BreakMatch, Action::FailFlush] {
        fs::write(&source, b"needle needle").unwrap();
        let (code, out, err) = observed(&tree.0, &["--text", "needle"], action);
        assert_eq!(code, EXIT_ERROR); assert!(out.poisoned);
        assert!(String::from_utf8_lossy(&err).contains("CLI_OUTPUT_INTERRUPTED"));
        assert!(!String::from_utf8_lossy(&out.bytes).contains("\"type\":\"terminal\""));
    }
}

#[test]
fn explicit_errors_and_human_output_use_the_new_route_without_source_side_effects() {
    let tree = Tree::new("arguments");
    let missing = tree.0.join("never-opened");
    let (code, result) = invoke(&missing, &["--text", "x", "--encoding", "auto"]);
    assert_eq!(code, EXIT_ERROR);
    assert_eq!(result.get("error").get("code").text(), "CLI_PROGRESSIVE_UNSUPPORTED_OPTIONS");
    let (code, rows) = stream(&missing, &["--raw-hex", "ff"]);
    assert_eq!(code, EXIT_ERROR); assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("payload_bytes_read").number(), 0);
    tree.file("control\u{1b}.rs", b"needle");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run(&arguments(&tree.0, &["--text", "needle"], None), &mut NoStdin, &mut out, &mut err, || false), EXIT_OK);
    assert!(err.is_empty() && !out.contains(&0x1b));
    let text = std::str::from_utf8(&out).unwrap();
    assert!(text.contains("bytes 0..6") && text.contains("1 files examined from 1 discovered files"));
}

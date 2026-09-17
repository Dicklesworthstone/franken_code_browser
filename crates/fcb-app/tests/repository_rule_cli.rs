#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Read}, path::{Path, PathBuf}, process::Command,
    sync::atomic::{AtomicU64, Ordering}};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use fcb::search::snapshot::{SnapshotLimits, SnapshotView};
use support::{Json, parse};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-rules-cli-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); fs::create_dir(path.join("root")).unwrap(); Self(path)
    }
    fn root(&self) -> PathBuf { self.0.join("root") }
    fn file(&self, relative: impl AsRef<Path>, bytes: &[u8]) {
        let path = self.root().join(relative); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
    fn workspace(&self, command: &str, flags: &[&str]) -> Vec<OsString> {
        let mut args = vec![command.into(), self.root().into_os_string(), "--workspace".into(), "--json".into()];
        args.extend(flags.iter().map(OsString::from)); args
    }
    fn save(&self, name: &str) -> Vec<OsString> {
        vec!["snapshot".into(), "save".into(), self.root().into_os_string(), "--output".into(),
            self.0.join(name).into_os_string(), "--respect-ignores".into(), "--json".into()]
    }
}
impl Drop for Tree { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NoStdin;
impl Read for NoStdin {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("workspace policy must not read stdin") }
}
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut out = Vec::new(); let mut err = Vec::new();
    let exit = run(args, &mut NoStdin, &mut out, &mut err, canceled);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    let json = parse(&out).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out)));
    assert_eq!(json.get("schema").text(), "fcb.cli/1"); (exit, json)
}
fn fixture() -> Tree {
    let tree = Tree::new();
    tree.file(".gitignore", b"private/\n*.tmp\n");
    tree.file("nested/.gitignore", b"!keep.tmp\n");
    tree.file("private/secret.rs", b"needle"); tree.file("drop.tmp", b"needle");
    tree.file("nested/keep.tmp", b"needle"); tree.file("safe.rs", b"needle");
    tree
}

#[test]
fn workspace_inspection_and_path_search_separate_configuration_from_source_payload() {
    let tree = fixture();
    let (exit, inspected) = invoke(&tree.workspace("inspect", &["--respect-ignores"]), || false);
    assert_eq!(exit, EXIT_OK); assert!(inspected.get("rule_files_enabled").flag());
    assert!(inspected.get("rule_policy").get("scope_complete").flag());
    assert_eq!(inspected.get("rule_policy").get("files_loaded").number(), 2);
    assert_eq!(inspected.get("rule_policy").get("configuration_bytes_read").number(), 25);
    assert_eq!(inspected.get("payload_bytes_read").number(), 0);
    assert_eq!(inspected.get("known_files").number(), 4);
    // Independently constructed with Python struct.pack and hashlib. The two
    // exact rule files are sorted by raw path, not their enumeration order.
    assert_eq!(inspected.get("policy").text(),
        "repository-rules-v1:bb71c15f4c99e4fe273924f042a85f29b709d9b2c7a9486ccbf8feb4aecf3ea9");
    let (exit, paths) = invoke(&tree.workspace("search", &["--respect-ignores", "--path", "keep"]), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(paths.get("hits").array().len(), 1);
    assert_eq!(paths.get("payload_bytes_read").number(), 0);
    assert_eq!(paths.get("policy").text(), inspected.get("policy").text());
}

#[test]
fn captured_and_whole_file_search_use_the_same_rule_filtered_universe() {
    let tree = fixture();
    for whole_file in [false, true] {
        let mut args = tree.workspace("search", &["--respect-ignores", "--text", "needle"]);
        if whole_file { args.push("--whole-file".into()); }
        let (exit, result) = invoke(&args, || false);
        assert_eq!(exit, EXIT_OK);
        if whole_file {
            assert_eq!(result.get("stored_hits").number(), 2);
            assert_eq!(result.get("files").array().iter().map(|file| file.get("hits").array().len()).sum::<usize>(), 2);
        } else { assert_eq!(result.get("hits").array().len(), 2); }
        assert!(result.get("rule_files_enabled").flag());
        assert_eq!(result.get("rule_policy").get("failed_files").number(), 0);
    }
    // Negative control: static scope includes the matching ignored files.
    let (exit, ordinary) = invoke(&tree.workspace("search", &["--text", "needle"]), || false);
    assert_eq!(exit, EXIT_OK); assert!(!ordinary.get("rule_files_enabled").flag());
    assert_eq!(ordinary.get("hits").array().len(), 4);
}

#[test]
fn malformed_rules_return_useful_partial_search_not_false_no_match() {
    let tree = Tree::new(); tree.file("bad/.gitignore", b"[broken"); tree.file("bad/secret.rs", b"needle");
    tree.file("safe.rs", b"other text");
    let (exit, result) = invoke(&tree.workspace("search", &["--respect-ignores", "--text", "needle"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!result.get("workspace_complete").flag());
    assert!(result.get("hits").array().is_empty());
    let policy = result.get("rule_policy");
    assert_eq!(policy.get("failed_files").number(), 1);
    assert_eq!(policy.get("diagnostics").array()[0].get("code").text(), "RULE_UNSUPPORTED_PATTERN");
    assert!(!result.get("discovery_complete").flag());
}

#[test]
fn snapshot_save_preserves_ignored_scope_and_offline_search_never_reopens_rules() {
    let tree = fixture();
    let (exit, saved) = invoke(&tree.save("before.fcbs"), || false);
    assert_eq!(exit, EXIT_OK); assert!(saved.get("rule_files_enabled").flag());
    assert_eq!(saved.get("rule_policy").get("configuration_bytes_read").number(), 25);
    let policy = saved.get("policy").text().to_owned();
    assert!(policy.starts_with("repository-rules-v1:"));
    let bytes = fs::read(tree.0.join("before.fcbs")).unwrap();
    let view = SnapshotView::open(&bytes, SnapshotLimits::default(), || false).unwrap();
    assert_eq!(view.policy(), policy);
    assert!(view.entries().all(|entry| {
        let entry = entry.unwrap(); !entry.path.starts_with(b"private/") && entry.path != b"drop.tmp"
    }));
    tree.file(".gitignore", b"*\n");
    let (exit, next) = invoke(&tree.save("after.fcbs"), || false);
    assert_eq!(exit, EXIT_OK); assert_ne!(next.get("policy").text(), policy);
    assert_eq!(next.get("known_files").number(), 0);
    let offline = vec!["snapshot".into(), "search".into(), tree.0.join("before.fcbs").into_os_string(),
        "--text".into(), "needle".into(), "--json".into()];
    let (exit, old) = invoke(&offline, || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(old.get("hits").array().len(), 2);
    assert!(!old.get("live_roots_accessed").flag());
}

#[test]
fn incomplete_rule_scope_remains_partial_after_save_and_reopen() {
    let tree = Tree::new(); tree.file("bad/.gitignore", b"[bad"); tree.file("bad/secret", b"needle"); tree.file("safe", b"x");
    let (exit, saved) = invoke(&tree.save("partial.fcbs"), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!saved.get("discovery_complete").flag());
    let offline = vec!["snapshot".into(), "search".into(), tree.0.join("partial.fcbs").into_os_string(),
        "--text".into(), "needle".into(), "--json".into()];
    let (exit, result) = invoke(&offline, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!result.get("workspace_complete").flag());
    assert!(result.get("hits").array().is_empty());
}

#[test]
fn byte_quotas_and_native_rule_symlinks_never_turn_into_ordinary_inclusion() {
    let tree = Tree::new();
    tree.file("large/.gitignore", &vec![b'#'; 16 * 1024 + 1]); tree.file("large/secret", b"needle");
    tree.file("outside", b"*.rs\n"); tree.file("linked/secret", b"needle");
    std::os::unix::fs::symlink(tree.root().join("outside"), tree.root().join("linked/.gitignore")).unwrap();
    let (exit, result) = invoke(&tree.workspace("search", &["--respect-ignores", "--text", "needle"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(result.get("hits").array().is_empty());
    let mut codes: Vec<_> = result.get("rule_policy").get("diagnostics").array().iter()
        .map(|d| d.get("code").text()).collect(); codes.sort_unstable();
    assert_eq!(codes, ["RULE_BYTE_LIMIT", "RULE_SPECIAL_OBJECT"]);
}

#[test]
fn conflicting_scope_cancellation_and_literal_values_are_resolved_before_io() {
    let tree = fixture();
    let (exit, error) = invoke(&tree.workspace("inspect", &["--respect-ignores", "--include-excluded"]), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error.get("error").get("code").text(), "CLI_INCOMPATIBLE_OPTIONS");
    assert_eq!(invoke(&tree.workspace("inspect", &["--respect-ignores"]), || true).0, EXIT_CANCELED);
    let (exit, literal) = invoke(&tree.workspace("search", &["--text", "--respect-ignores"]), || false);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(!literal.get("rule_files_enabled").flag());
}

#[test]
fn actual_binary_honors_rules_and_writes_only_the_selected_snapshot() {
    let tree = fixture();
    let result = Command::new(env!("CARGO_BIN_EXE_fcb")).args(tree.workspace("search", &["--respect-ignores", "--text", "needle"]))
        .output().unwrap();
    assert_eq!(result.status.code(), Some(EXIT_OK as i32));
    assert_eq!(parse(&result.stdout).unwrap().get("hits").array().len(), 2);
    let saved = Command::new(env!("CARGO_BIN_EXE_fcb")).args(tree.save("new.fcbs")).output().unwrap();
    assert_eq!(saved.status.code(), Some(EXIT_OK as i32));
    assert_eq!(parse(&saved.stdout).unwrap().get("known_files").number(), 4);
    assert_eq!(fs::read(tree.root().join("private/secret.rs")).unwrap(), b"needle");
    assert_eq!(fs::read(tree.root().join(".gitignore")).unwrap(), b"private/\n*.tmp\n");
}

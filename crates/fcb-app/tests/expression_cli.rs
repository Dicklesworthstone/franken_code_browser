#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Read}, path::PathBuf, process::Command,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-expression-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); fs::create_dir(path.join("root")).unwrap(); Self(path)
    }
    fn root(&self) -> PathBuf { self.0.join("root") }
    fn saved(&self) -> PathBuf { self.0.join("saved.fcbs") }
    fn file(&self, relative: &str, bytes: &[u8]) {
        let path = self.root().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
    fn live(&self, query: &str) -> Vec<OsString> {
        vec!["search".into(), self.root().into_os_string(), "--workspace".into(),
            "--query".into(), query.into(), "--json".into()]
    }
    fn offline(&self, query: &str) -> Vec<OsString> {
        vec!["snapshot".into(), "search".into(), self.saved().into_os_string(),
            "--query".into(), query.into(), "--json".into()]
    }
    fn save(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "save".into(), self.root().into_os_string(),
            "--output".into(), self.saved().into_os_string(), "--json".into()]
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NeverRead;
impl Read for NeverRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("expression command must not read stdin") }
}
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let exit = run(args, &mut NeverRead, &mut stdout, &mut stderr, canceled);
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    let json = parse(&stdout).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&stdout)));
    assert_eq!(json.get("schema").text(), "fcb.cli/1"); (exit, json)
}
fn add(args: &mut Vec<OsString>, flag: &str, value: &str) { args.extend([flag.into(), value.into()]); }
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    bytes
}
fn starts(document: &Json) -> Vec<u64> {
    document.get("hits").array().iter().map(|hit| hit.get("original_range").get("start").number()).collect()
}

#[test]
fn live_metadata_filters_prevent_out_of_scope_payload_reads_and_preserve_capture_allowance() {
    let fixture = Fixture::new();
    let included = b"banana\nrequired"; let rejected = b"banana required forbidden";
    fixture.file("src/a.rs", included); fixture.file("src/b.rs", rejected);
    fixture.file("before.py", &[b'x'; 1024]); fixture.file("src/c.py", &[b'x'; 1024]);
    let mut args = fixture.live("ana required -forbidden path:src/ lang:rust");
    add(&mut args, "--max-total-bytes", &(included.len() + rejected.len()).to_string());
    let (exit, result) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert!(result.get("workspace_complete").flag());
    assert_eq!(result.get("mode").text(), "decoded-text-expression");
    assert_eq!(result.get("scope_files").number(), 2);
    assert_eq!(result.get("metadata_excluded_files").number(), 2);
    assert_eq!(result.get("payload_bytes_read").number(), (included.len() + rejected.len()) as u64);
    assert!(result.get("unavailable_files").array().is_empty());
    assert_eq!(starts(&result), [1, 3]);
}

#[test]
fn literal_search_is_not_silently_reparsed_as_an_expression() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"needle -forbidden"); fixture.file("b.rs", b"needle");
    let mut literal = fixture.live("needle -forbidden"); literal[3] = "--text".into();
    let (exit, result) = invoke(&literal, || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("hits").array().len(), 1);
    assert_eq!(result.get("hits").array()[0].get("matched_text").text(), "needle -forbidden");
    let (exit, result) = invoke(&fixture.live("needle -forbidden"), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("hits").array().len(), 1);
    assert_eq!(result.get("hits").array()[0].get("path").get("hex").text(), "622e7273");
}

#[test]
fn snapshot_predicates_read_only_saved_utf16_source_after_live_namespace_replacement() {
    let fixture = Fixture::new();
    fixture.file("a.rs", &utf16("head\r\nbanana required"));
    fixture.file("b.rs", b"banana required forbidden");
    assert_eq!(invoke(&fixture.save(), || false).0, EXIT_OK);
    let original = fs::read(fixture.saved()).unwrap();
    fs::rename(fixture.root(), fixture.0.join("retired")).unwrap();
    fs::create_dir(fixture.root()).unwrap(); fixture.file("a.rs", b"new live bytes");
    let (exit, result) = invoke(&fixture.offline("ana required -forbidden type:rust"), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(starts(&result), [16, 20]);
    assert!(!result.get("live_roots_accessed").flag());
    assert_eq!(result.get("members_loaded").number(), 2);
    assert!(result.get("workspace_complete").flag());
    assert_eq!(fs::read(fixture.saved()).unwrap(), original, "search is read-only");
}

#[test]
fn missing_members_are_counted_in_the_selected_scope_not_the_whole_archive() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"needle"); fixture.file("b.py", &[b'x'; 40]);
    let mut save = fixture.save(); add(&mut save, "--max-file-bytes", "16");
    assert_eq!(invoke(&save, || false).0, EXIT_PARTIAL);
    let (exit, scoped) = invoke(&fixture.offline("needle lang:rs"), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(scoped.get("unavailable_files_count").number(), 0);
    assert!(scoped.get("workspace_complete").flag());
    let (exit, wide) = invoke(&fixture.offline("absent"), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(wide.get("unavailable_files_count").number(), 1);
    assert!(!wide.get("workspace_complete").flag()); assert!(wide.get("hits").array().is_empty());
}

#[test]
fn predicate_work_exhaustion_does_not_publish_unqualified_primary_hits() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"needle................forbidden");
    assert_eq!(invoke(&fixture.save(), || false).0, EXIT_OK);
    for mut args in [fixture.live("needle -forbidden"), fixture.offline("needle -forbidden")] {
        add(&mut args, "--max-scan-bytes", "8");
        let (exit, result) = invoke(&args, || false);
        assert_eq!(exit, EXIT_PARTIAL); assert!(result.get("work_limited").flag());
        assert!(!result.get("workspace_complete").flag()); assert!(result.get("hits").array().is_empty());
        assert!(result.get("scanned_input_bytes").number() <= 8);
    }
}

#[test]
fn limits_count_only_qualified_primary_hits_and_zero_limit_has_real_lookahead() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"needle required"); fixture.file("b.rs", b"needle");
    assert_eq!(invoke(&fixture.save(), || false).0, EXIT_OK);
    for offline in [false, true] {
        let route = |text| if offline { fixture.offline(text) } else { fixture.live(text) };
        let mut one = route("needle required"); add(&mut one, "--limit", "1");
        let (exit, result) = invoke(&one, || false);
        assert_eq!(exit, EXIT_OK); assert!(!result.get("truncated").flag()); assert_eq!(result.get("matches_seen").number(), 1);
        let mut zero = route("needle required"); add(&mut zero, "--limit", "0");
        let (exit, result) = invoke(&zero, || false);
        assert_eq!(exit, EXIT_PARTIAL); assert!(result.get("truncated").flag());
        assert_eq!(result.get("matches_seen").number(), 1); assert!(result.get("hits").array().is_empty());
        let mut absent = route("absent required"); add(&mut absent, "--limit", "0");
        let (exit, result) = invoke(&absent, || false);
        assert_eq!(exit, EXIT_NO_MATCH); assert!(result.get("workspace_complete").flag());
        assert!(!result.get("truncated").flag()); assert_eq!(result.get("matches_seen").number(), 0);
    }
}

#[test]
fn syntax_and_unsupported_regex_are_rejected_before_opening_nonexistent_source() {
    let fixture = Fixture::new();
    for (text, code) in [("\"unfinished", "QUERY_SYNTAX_ERROR"), ("regex:.*", "QUERY_REGEX_UNQUALIFIED"), ("-forbidden", "QUERY_EMPTY")] {
        let mut live = fixture.live(text); live[1] = fixture.0.join("does-not-exist").into_os_string();
        for args in [live, fixture.offline(text)] {
            let (exit, result) = invoke(&args, || false);
            assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("error").get("code").text(), code);
        }
    }
    assert!(!fixture.saved().exists());
}

#[test]
fn repository_policy_still_gates_expression_scope_and_unknown_subtrees_remain_partial() {
    let fixture = Fixture::new(); fixture.file(".gitignore", b"ignored/\n");
    fixture.file("a.rs", b"needle required"); fixture.file("ignored/secret.rs", b"needle required");
    fixture.file("unknown/.gitignore", b"[unsupported\n"); fixture.file("unknown/secret.rs", b"needle required");
    let mut args = fixture.live("needle required lang:rs"); args.push("--respect-ignores".into());
    let (exit, result) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!result.get("discovery_complete").flag());
    assert!(!result.get("workspace_complete").flag()); assert!(result.get("rule_files_enabled").flag());
    assert_eq!(result.get("hits").array().len(), 1);
    assert_eq!(result.get("payload_bytes_read").number(), b"needle required".len() as u64);
}

#[test]
fn trusted_catalog_expression_open_preserves_selective_member_loading() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"needle required"); fixture.file("b.py", &[b'x'; 1024]);
    assert_eq!(invoke(&fixture.save(), || false).0, EXIT_OK);
    let catalog = fixture.0.join("saved.fcbc");
    let build = vec!["snapshot".into(), "catalog".into(), fixture.saved().into_os_string(),
        "--output".into(), catalog.clone().into_os_string(), "--json".into()];
    let (exit, receipt) = invoke(&build, || false); assert_eq!(exit, EXIT_OK);
    let mut args = fixture.offline("needle required lang:rust");
    args.extend(["--catalog".into(), catalog.into_os_string()]);
    add(&mut args, "--catalog-digest", receipt.get("catalog_digest").text());
    let (exit, result) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("archive_validation_bytes").number(), 56);
    assert!(!result.get("archive_body_verified_on_open").flag());
    assert_eq!(result.get("members_loaded").number(), 1);
    assert_eq!(result.get("loaded_source_bytes").number(), b"needle required".len() as u64);
}

#[test]
fn raw_filenames_and_query_controls_are_reversible_without_terminal_control_output() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new(); let name = b"line\n\xff.rs";
    fs::write(fixture.root().join(OsString::from_vec(name.to_vec())), b"needle required").unwrap();
    let (exit, result) = invoke(&fixture.live("needle required lang:rs"), || false);
    assert_eq!(exit, EXIT_OK);
    let hex = name.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    assert_eq!(result.get("hits").array()[0].get("path").get("hex").text(), hex);
    let mut args = fixture.live("needle -\"\u{001b}[31m\""); args.pop();
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    assert_eq!(run(&args, &mut NeverRead, &mut stdout, &mut stderr, || false), EXIT_OK);
    assert!(stderr.is_empty()); assert!(!stdout.contains(&0x1b));
}

#[test]
fn cancellation_produces_one_terminal_error_document_and_no_artifact_writes() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"needle required");
    assert_eq!(invoke(&fixture.save(), || false).0, EXIT_OK);
    let original = fs::read(fixture.saved()).unwrap();
    for args in [fixture.live("needle required"), fixture.offline("needle required")] {
        let (exit, result) = invoke(&args, || true);
        assert_eq!(exit, EXIT_CANCELED); assert_eq!(result.get("status").text(), "error");
        assert!(!result.get("complete").flag());
    }
    assert_eq!(fs::read(fixture.saved()).unwrap(), original);
}

#[test]
fn actual_binary_executes_live_and_saved_expression_search_after_root_move() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"banana required"); fixture.file("b.rs", b"banana forbidden required");
    let binary = env!("CARGO_BIN_EXE_fcb");
    let live = Command::new(binary).args(fixture.live("ana required -forbidden lang:rust")).output().unwrap();
    assert_eq!(live.status.code(), Some(EXIT_OK as i32)); assert!(live.stderr.is_empty());
    assert_eq!(starts(&parse(&live.stdout).unwrap()), [1, 3]);
    let saved = Command::new(binary).args(fixture.save()).output().unwrap();
    assert_eq!(saved.status.code(), Some(EXIT_OK as i32));
    fs::rename(fixture.root(), fixture.0.join("gone")).unwrap();
    let offline = Command::new(binary).args(fixture.offline("ana required -forbidden lang:rust")).output().unwrap();
    assert_eq!(offline.status.code(), Some(EXIT_OK as i32)); assert!(offline.stderr.is_empty());
    let result = parse(&offline.stdout).unwrap(); assert_eq!(starts(&result), [1, 3]);
    assert!(!result.get("live_roots_accessed").flag());
}

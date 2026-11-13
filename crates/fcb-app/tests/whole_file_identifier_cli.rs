#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! CLI integration for the shared whole-token streaming matcher (FCB-057.A).
//! These tests exercise real files and the public app route, not a second matcher.

mod support;
use std::{ffi::OsString, fs, io::{self, Read}, path::{Path, PathBuf}, process::Command,
    sync::atomic::{AtomicU64, Ordering}};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use fcb_app::args::{self, ArgumentError, Needle};
use support::{Json, parse};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-identifier-cli-{}-{}-{}",
            std::process::id(), nonce, NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name); fs::write(&path, bytes).unwrap(); path
    }
}
// Preserve each unique input directory for post-run diagnosis.
struct NoStdin;
impl Read for NoStdin {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("identifier search must not consume stdin") }
}
fn arguments(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
fn command(path: &Path, flags: &[&str]) -> Vec<OsString> {
    let mut args = vec!["search".into(), path.as_os_str().to_owned(), "--json".into()];
    args.extend(flags.iter().map(OsString::from)); args
}
fn invoke(path: &Path, flags: &[&str]) -> (u8, Json) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = run(&command(path, flags), &mut NoStdin, &mut stdout, &mut stderr, || false);
    assert!(stderr.is_empty(), "stderr={:?}", String::from_utf8_lossy(&stderr));
    let json = parse(&stdout).unwrap();
    assert_eq!(json.get("schema").text(), "fcb.cli/1");
    (code, json)
}
fn bounds(hit: &Json) -> (u64, u64) {
    (hit.get("original_range").get("start").number(), hit.get("original_range").get("end").number())
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}
fn contract(json: &Json) {
    assert_eq!(json.get("mode").text(), "whole-token-occurrences");
    assert!(json.get("case_sensitive").flag());
    assert_eq!(json.get("normalization").text(), "exact");
    assert_eq!(json.get("evidence_level").text(), "whole-token-text-candidate");
    assert!(json.get("includes_comments_and_literals").flag());
    assert!(!json.get("compiler_resolved").flag());
    assert!(!json.get("semantic_complete").flag());
}

#[test]
fn identifier_admission_is_explicit_bounded_and_distinct_from_other_modes() {
    let parsed = args::parse(&arguments(&["search", "root", "--workspace", "--whole-file",
        "--identifier", "$item", "--count-all", "--limit", "0", "--respect-ignores"])).unwrap();
    assert_eq!(parsed.needle, Some(Needle::Identifier("$item".into())));
    assert!(parsed.whole_file && parsed.workspace && parsed.count_all && parsed.respect_ignores);
    assert_eq!(parsed.limit, 0);
    for name in ["", "1foo", "foo bar", "foo-bar", "foo\n", "foo\0"] {
        assert_eq!(args::parse(&arguments(&["search", "file", "--whole-file", "--identifier", name])),
            Err(ArgumentError::InvalidNeedle), "{name:?}");
    }
    let longest = "x".repeat(1024);
    assert!(args::parse(&arguments(&["search", "file", "--whole-file", "--identifier", &longest])).is_ok());
    let excessive = "x".repeat(1025);
    assert_eq!(args::parse(&arguments(&["search", "file", "--whole-file", "--identifier", &excessive])),
        Err(ArgumentError::InvalidNeedle));
    for flags in [
        vec!["search", "file", "--identifier", "foo"],
        vec!["search", "root", "--workspace", "--identifier", "foo"],
        vec!["search", "--stdin", "--whole-file", "--identifier", "foo"],
        vec!["read", "file", "--whole-file", "--identifier", "foo"],
        vec!["search", "file", "--whole-file", "--identifier", "foo", "--ignore-case"],
        vec!["search", "file", "--whole-file", "--identifier", "foo", "--identifier", "bar"],
        vec!["search", "file", "--whole-file", "--identifier", "foo", "--text", "foo"],
        vec!["search", "file", "--whole-file", "--text", "foo", "--identifier", "foo"],
        vec!["search", "file", "--whole-file", "--identifier", "foo", "--raw-hex", "66"],
        vec!["search", "file", "--whole-file", "--identifier", "foo", "--path", "foo"],
        vec!["search", "file", "--whole-file", "--identifier", "foo", "--query", "foo"],
        vec!["search", "file", "--whole-file", "--identifier", "foo", "--offset", "0"],
    ] { assert!(args::parse(&arguments(&flags)).is_err(), "{flags:?}"); }
    assert!(!args::json_requested(&arguments(&["search", "file", "--whole-file", "--identifier", "--json"])));
    assert!(args::json_requested(&arguments(&["search", "file", "--whole-file", "--identifier", "--json", "--json"])));
}

#[test]
fn whole_tokens_exclude_prefix_suffix_and_case_variants_but_include_comments_and_strings() {
    let temp = Temp::new();
    let bytes = b"foo foobar _foo foo_ $foo 1foo Foo foo(); // foo\n\"foo\"";
    let file = temp.file("tokens.rs", bytes);
    let (exit, json) = invoke(&file, &["--whole-file", "--identifier", "foo"]);
    assert_eq!(exit, EXIT_OK); contract(&json);
    assert!(json.get("whole_file_complete").flag());
    assert_eq!(json.get("matches_seen").number(), 4);
    assert_eq!(json.get("hits").array().iter().map(bounds).collect::<Vec<_>>(),
        [(0, 3), (35, 38), (45, 48), (50, 53)]);
    assert_eq!(json.get("literal_original_hex").text(), "666f6f");
    for hit in json.get("hits").array() {
        let (start, end) = bounds(hit);
        assert_eq!(&bytes[start as usize..end as usize], b"foo");
        assert_eq!(hit.get("decoded_range"), &Json::Null);
    }
    // Negative control: literal text deliberately finds embedded occurrences.
    // Accidentally dispatching identifiers to the literal matcher fails above.
    let (_, literal) = invoke(&file, &["--whole-file", "--text", "foo"]);
    assert!(literal.get("matches_seen").number() > json.get("matches_seen").number());
    let (exit, absent) = invoke(&file, &["--whole-file", "--identifier", "not_present"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert_eq!(absent.get("matches_seen").number(), 0);
}

#[test]
fn utf16_and_non_ascii_tokens_preserve_original_byte_witnesses() {
    let temp = Temp::new();
    for little in [true, false] {
        let file = temp.file(if little { "little.rs" } else { "big.rs" }, &utf16("😀 foo foobar foo", little));
        let (exit, json) = invoke(&file, &["--whole-file", "--identifier", "foo"]);
        assert_eq!(exit, EXIT_OK); contract(&json);
        assert_eq!(json.get("hits").array().iter().map(bounds).collect::<Vec<_>>(), [(8, 14), (30, 36)]);
        assert_eq!(json.get("literal_original_hex").text(), if little { "66006f006f00" } else { "0066006f006f" });
    }
    let file = temp.file("marks.rs", "e\u{301} e e\u{301}x e\u{301}".as_bytes());
    let (exit, json) = invoke(&file, &["--whole-file", "--identifier", "e\u{301}"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(json.get("matches_seen").number(), 2);
    assert_eq!(json.get("literal_original_hex").text(), "65cc81");
    let (exit, absent) = invoke(&file, &["--whole-file", "--identifier", "E\u{301}"]);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(absent.get("whole_file_complete").flag());
}

#[test]
fn tokens_cross_input_buffers_and_large_files_without_capture_quota() {
    let temp = Temp::new();
    let prefix = 1024 * 1024 + 16 * 1024 - 2;
    let mut bytes = vec![b' '; prefix];
    bytes.extend_from_slice(b"foo foobar foo");
    temp.file("large.rs", &bytes);
    let (exit, json) = invoke(&temp.0, &["--workspace", "--whole-file", "--identifier", "foo"]);
    assert_eq!(exit, EXIT_OK); contract(&json);
    assert!(!json.get("capture_limits_applied").flag());
    assert!(json.get("workspace_complete").flag());
    assert_eq!(json.get("payload_bytes_read").number(), bytes.len() as u64);
    let file = &json.get("files").array()[0]; contract(file);
    assert_eq!(file.get("hits").array().iter().map(bounds).collect::<Vec<_>>(),
        [(prefix as u64, prefix as u64 + 3), (prefix as u64 + 11, prefix as u64 + 14)]);
    assert!(file.get("peak_input_buffer_bytes").number() <= 16 * 1024);
}

#[test]
fn counts_continue_across_files_after_rows_fill_and_zero_retention_stays_exact() {
    let temp = Temp::new();
    temp.file("a.rs", b"foo foobar foo");
    temp.file("b.rs", &utf16("foo; foo", true));
    temp.file("c.rs", b"Foo _foo");
    for limit in ["1", "0"] {
        let (exit, json) = invoke(&temp.0, &["--workspace", "--whole-file", "--identifier", "foo",
            "--count-all", "--limit", limit]);
        assert_eq!(exit, EXIT_PARTIAL); contract(&json);
        assert!(json.get("match_count_complete").flag());
        assert!(json.get("workspace_complete").flag());
        assert!(json.get("truncated").flag());
        assert_eq!(json.get("matches_seen").number(), 4);
        assert_eq!(json.get("stored_hits").number(), if limit == "1" { 1 } else { 0 });
        assert_eq!(json.get("matching_files").number(), 2);
        assert_eq!(json.get("files_examined").number(), 3);
        assert_eq!(json.get("stop_reason"), &Json::Null);
        for file in json.get("files").array() {
            contract(file);
            if limit == "0" {
                assert_eq!(file.get("retained_witness_bytes").number(), 0);
                assert!(file.get("hits").array().is_empty());
            }
        }
    }
    let (exit, absent) = invoke(&temp.0, &["--workspace", "--whole-file", "--identifier", "missing",
        "--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert!(absent.get("match_count_complete").flag()); assert!(!absent.get("truncated").flag());
    let (exit, stopped) = invoke(&temp.0, &["--workspace", "--whole-file", "--identifier", "foo", "--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!stopped.get("match_count_complete").flag());
    assert_eq!(stopped.get("stop_reason").text(), "match-limit");
}

#[test]
fn a_scan_budget_is_not_a_token_boundary_and_workspace_io_remains_global() {
    let temp = Temp::new(); let file = temp.file("one.rs", b"foo bar");
    let (exit, partial) = invoke(&file, &["--whole-file", "--identifier", "foo", "--max-scan-bytes", "3"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(partial.get("matches_seen").number(), 0);
    assert_eq!(partial.get("state").text(), "byte-limit");
    let (_, delimited) = invoke(&file, &["--whole-file", "--identifier", "foo", "--max-scan-bytes", "4"]);
    assert_eq!(delimited.get("matches_seen").number(), 1);
    assert!(!delimited.get("match_count_complete").flag());
    let eof = temp.file("eof.rs", b"foo");
    let (exit, complete) = invoke(&eof, &["--whole-file", "--identifier", "foo", "--max-scan-bytes", "3"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(complete.get("matches_seen").number(), 1);
    let workspace = Temp::new();
    workspace.file("a.rs", b"foo "); workspace.file("b.rs", b"fooX "); workspace.file("c.rs", b"foo");
    let (exit, json) = invoke(&workspace.0, &["--workspace", "--whole-file", "--identifier", "foo",
        "--count-all", "--limit", "0", "--max-scan-bytes", "7"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(json.get("matches_seen").number(), 1);
    assert_eq!(json.get("payload_bytes_read").number(), 7);
    assert_eq!(json.get("unexamined_files").number(), 1);
    assert!(!json.get("match_count_complete").flag());
    assert_eq!(json.get("stop_reason").text(), "byte-limit");
}

#[test]
fn invalid_decoding_invalidates_only_its_file_and_raw_paths_remain_reversible() {
    use std::os::unix::ffi::OsStringExt;
    let temp = Temp::new();
    let raw = OsString::from_vec(b"a\xff.rs".to_vec());
    fs::write(temp.0.join(raw), b"foo").unwrap();
    temp.file("broken.rs", b"foo \xff");
    let (exit, json) = invoke(&temp.0, &["--workspace", "--whole-file", "--identifier", "foo", "--count-all"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(json.get("matches_seen").number(), 1);
    assert_eq!(json.get("incomplete_files").number(), 1);
    assert!(!json.get("match_count_complete").flag());
    let files = json.get("files").array();
    assert_eq!(files[0].get("path").get("hex").text(), "61ff2e7273");
    assert_eq!(files[1].get("state").text(), "unsupported-text");
    assert_eq!(files[1].get("matches_seen").number(), 0);
    assert!(files[1].get("hits").array().is_empty());
}

#[test]
fn cancellation_and_invalid_requests_publish_a_single_error_document() {
    let temp = Temp::new(); let file = temp.file("large.rs", &b"foo ".repeat(64 * 1024));
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut checks = 0;
    let code = run(&command(&file, &["--whole-file", "--identifier", "foo", "--count-all", "--limit", "1"]),
        &mut NoStdin, &mut stdout, &mut stderr, || { checks += 1; checks > 64 });
    assert_eq!(code, EXIT_CANCELED); assert!(stderr.is_empty());
    let json = parse(&stdout).unwrap();
    assert_eq!(json.get("error").get("code").text(), "CLI_CANCELED");
    let (exit, invalid) = invoke(&temp.0.join("does-not-exist"), &["--whole-file", "--identifier", "bad name"]);
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(invalid.get("error").get("code").text(), "CLI_INVALID_NEEDLE");
}

#[test]
fn binary_route_and_inert_capabilities_expose_identifier_mode() {
    let temp = Temp::new(); let file = temp.file("binary.rs", b"foo foobar foo");
    let output = Command::new(env!("CARGO_BIN_EXE_fcb"))
        .args(command(&file, &["--whole-file", "--identifier", "foo", "--count-all", "--limit", "0"]))
        .output().unwrap();
    assert_eq!(output.status.code(), Some(i32::from(EXIT_PARTIAL)));
    assert!(output.stderr.is_empty());
    let json = parse(&output.stdout).unwrap(); contract(&json);
    assert_eq!(json.get("matches_seen").number(), 2);
    assert!(json.get("match_count_complete").flag());
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(run(&arguments(&["capabilities", "--json"]), &mut NoStdin, &mut stdout, &mut stderr, || false), EXIT_OK);
    let capabilities = parse(&stdout).unwrap();
    assert!(!capabilities.get("source_scanned").flag());
    let whole = capabilities.get("whole_file_search");
    assert_eq!(whole.get("identifier_flag").text(), "--identifier");
    assert_eq!(whole.get("identifier_semantics").text(), "whole-token-text-candidate");
    assert!(whole.get("identifier_case_sensitive").flag());
}

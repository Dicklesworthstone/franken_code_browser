#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;

use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use fcb_app::{run, EXIT_CANCELED, EXIT_ERROR, EXIT_NO_MATCH, EXIT_OK, EXIT_PARTIAL};
use support::{parse, Json};

const INPUT_CHUNK: usize = 16 * 1024;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fcb-whole-expression-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn file(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

struct NoStdin;

impl Read for NoStdin {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("named-source expression search must not consume stdin")
    }
}

fn args(path: &Path, query: &str, flags: &[&str]) -> Vec<OsString> {
    let mut args = vec![
        "search".into(),
        path.as_os_str().to_owned(),
        "--whole-file".into(),
        "--query".into(),
        query.into(),
        "--json".into(),
    ];
    args.extend(flags.iter().map(OsString::from));
    args
}

fn invoke_args(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(args, &mut NoStdin, &mut stdout, &mut stderr, canceled);
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    let result = parse(&stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&stdout)));
    assert_eq!(result.get("schema").text(), "fcb.cli/1");
    (exit, result)
}

fn invoke(path: &Path, query: &str, flags: &[&str]) -> (u8, Json) {
    invoke_args(&args(path, query, flags), || false)
}

fn bounds(hit: &Json) -> (u64, u64) {
    (
        hit.get("original_range").get("start").number(),
        hit.get("original_range").get("end").number(),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn utf16(text: &str, little: bool, bom: bool) -> Vec<u8> {
    let mut bytes = if !bom {
        Vec::new()
    } else if little {
        vec![0xff, 0xfe]
    } else {
        vec![0xfe, 0xff]
    };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

fn assert_expression(result: &Json, query: &str) {
    assert_eq!(result.get("strategy").text(), "streaming-whole-file");
    assert_eq!(result.get("mode").text(), "decoded-text-expression");
    assert_eq!(result.get("expression").text(), query);
    assert_eq!(result.get("predicate_scope").text(), "same-observed-stream");
}

fn assert_no_published_hits(file: &Json) {
    assert_eq!(file.get("matches_seen").number(), 0);
    assert_eq!(file.get("stored_hits").number(), 0);
    assert!(file.get("hits").array().is_empty());
}

#[test]
fn large_file_predicates_share_one_observation_and_one_payload_allowance() {
    let fixture = Fixture::new();
    let mut source = b"required\n".to_vec();
    source.resize(2 * 1024 * 1024 + 7, b'x');
    let anchor = source.len() as u64;
    source.extend_from_slice(b"banana");
    let file = fixture.file("huge.rs", &source);
    let query = "ana required -forbidden";
    let allowance = source.len().to_string();
    let (exit, result) = invoke(&file, query, &["--max-scan-bytes", &allowance]);
    assert_eq!(exit, EXIT_OK);
    assert_expression(&result, query);
    assert_eq!(result.get("path_filter_domain").text(), "absolute-named-path-utf8-or-escaped-search-key");
    assert!(result.get("expression_qualified").flag());
    assert!(result.get("whole_file_complete").flag());
    assert!(result.get("match_count_complete").flag());
    assert_eq!(result.get("payload_bytes_read").number(), source.len() as u64);
    assert!(result.get("peak_input_buffer_bytes").number() <= INPUT_CHUNK as u64);
    assert_eq!(result.get("matches_seen").number(), 2);
    let hits = result.get("hits").array();
    assert_eq!(bounds(&hits[0]), (anchor + 1, anchor + 4));
    assert_eq!(bounds(&hits[1]), (anchor + 3, anchor + 6));
    assert_ne!(hits[0].get("occurrence_id"), hits[1].get("occurrence_id"));
    assert_eq!(hits[0].get("decoded_range"), &Json::Null);
    assert_eq!(result.get("literal_original_hex").text(), hex(b"ana"));
    assert_eq!(fs::read(&file).unwrap(), source);
}

#[test]
fn quoted_anchor_required_and_excluded_phrases_cross_distinct_input_boundaries() {
    let fixture = Fixture::new();
    let mut source = vec![b'x'; 3 * INPUT_CHUNK + 32];
    let anchor = INPUT_CHUNK - 3;
    let required = 2 * INPUT_CHUNK - 4;
    let excluded = 3 * INPUT_CHUNK - 5;
    source[anchor..anchor + 7].copy_from_slice(b"anc hor");
    source[required..required + 9].copy_from_slice(b"req uired");
    let query = "\"anc hor\" +\"req uired\" -\"for bidden\"";
    let file = fixture.file("qualified.rs", &source);
    let (exit, qualified) = invoke(&file, query, &[]);
    assert_eq!(exit, EXIT_OK);
    assert!(qualified.get("expression_qualified").flag());
    assert_eq!(qualified.get("matches_seen").number(), 1);
    assert_eq!(bounds(&qualified.get("hits").array()[0]), (anchor as u64, anchor as u64 + 7));
    assert_eq!(qualified.get("payload_bytes_read").number(), source.len() as u64);

    // The suffix exclusion must invalidate an already observed anchor, even
    // when zero retention would otherwise have established an omitted row.
    source[excluded..excluded + 10].copy_from_slice(b"for bidden");
    let rejected_file = fixture.file("rejected.rs", &source);
    let (exit, rejected) = invoke(&rejected_file, query, &["--limit", "0"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert!(!rejected.get("expression_qualified").flag());
    assert!(rejected.get("whole_file_complete").flag());
    assert!(rejected.get("match_count_complete").flag());
    assert!(!rejected.get("truncated").flag());
    assert_no_published_hits(&rejected);
    assert_eq!(rejected.get("payload_bytes_read").number(), source.len() as u64);
}

#[test]
fn workspace_metadata_filters_preserve_the_global_source_read_allowance() {
    let fixture = Fixture::new();
    let included = b"banana required";
    let rejected = b"banana required forbidden";
    fixture.file("src/a.rs", included);
    fixture.file("src/b.rs", rejected);
    let invalid = vec![0xff; 2 * INPUT_CHUNK];
    fixture.file("before.py", &invalid);
    fixture.file("src/c.py", &invalid);
    fixture.file("outside/d.rs", &invalid);
    fixture.file("src/skip.rs", &invalid);
    let query = "ana required -forbidden path:src/ -path:skip type:rust";
    let allowance = (included.len() + rejected.len()).to_string();
    let (exit, result) = invoke(&fixture.0, query, &["--workspace", "--max-scan-bytes", &allowance]);
    assert_eq!(exit, EXIT_OK);
    assert_expression(&result, query);
    assert_eq!(result.get("path_filter_domain").text(), "root-relative-utf8-or-escaped-search-key");
    assert!(result.get("workspace_complete").flag());
    assert!(result.get("match_count_complete").flag());
    assert!(!result.get("capture_limits_applied").flag());
    assert_eq!(result.get("scope_files").number(), 2);
    assert_eq!(result.get("metadata_excluded_files").number(), 4);
    assert_eq!(result.get("files_examined").number(), 2);
    assert_eq!(result.get("unexamined_files").number(), 0);
    assert_eq!(result.get("payload_bytes_read").number(), (included.len() + rejected.len()) as u64);
    assert_eq!(result.get("matches_seen").number(), 2);
    assert_eq!(result.get("matching_files").number(), 1);
    let files = result.get("files").array();
    assert_eq!(files.len(), 2);
    assert_eq!(bounds(&files[0].get("hits").array()[0]), (1, 4));
    assert_eq!(bounds(&files[0].get("hits").array()[1]), (3, 6));
    assert!(!files[1].get("expression_qualified").flag());
    assert_no_published_hits(&files[1]);
}

#[test]
fn named_metadata_exclusions_finish_without_opening_a_missing_source() {
    let fixture = Fixture::new();
    let missing = fixture.0.join("missing.rs");
    for query in ["ana lang:python", "ana path:__never_in_this_fixture__"] {
        let (exit, result) = invoke(&missing, query, &[]);
        assert_eq!(exit, EXIT_NO_MATCH);
        assert_expression(&result, query);
        assert_eq!(result.get("state").text(), "metadata-excluded");
        assert_eq!(result.get("path_filter_domain").text(), "absolute-named-path-utf8-or-escaped-search-key");
        assert!(result.get("query_complete").flag());
        assert!(result.get("match_count_complete").flag());
        assert!(!result.get("input_complete").flag());
        assert!(!result.get("whole_file_complete").flag());
        assert_eq!(result.get("payload_bytes_read").number(), 0);
        assert_eq!(result.get("read_calls").number(), 0);
        assert_no_published_hits(&result);
        assert_eq!(result.get("file_id"), &Json::Null);
        assert_eq!(result.get("revision"), &Json::Null);
        assert_eq!(result.get("observed_length"), &Json::Null);
        assert_eq!(result.get("final_length"), &Json::Null);
    }
    // The same nonexistent path is an error when its metadata is in scope.
    assert_eq!(invoke(&missing, "ana lang:rust", &[]).0, EXIT_ERROR);
}

#[test]
fn utf16_predicates_preserve_bom_and_surrogate_original_byte_offsets() {
    let fixture = Fixture::new();
    let prefix = "😀\r\n";
    let text = format!("{prefix}banana required");
    for little in [true, false] {
        let encoding = if little { "utf16le" } else { "utf16be" };
        for bom in [true, false] {
            let source = utf16(&text, little, bom);
            let file = fixture.file("encoded.rs", &source);
            let declaration = if bom { "auto" } else { encoding };
            let (exit, result) = invoke(&file, "ana required -forbidden", &["--encoding", declaration]);
            assert_eq!(exit, EXIT_OK);
            assert!(result.get("whole_file_complete").flag());
            assert_eq!(result.get("encoding").text(), encoding);
            assert_eq!(result.get("payload_bytes_read").number(), source.len() as u64);
            let base = (utf16(prefix, little, false).len() + if bom { 2 } else { 0 }) as u64;
            let hits = result.get("hits").array();
            assert_eq!(hits.len(), 2);
            assert_eq!(bounds(&hits[0]), (base + 2, base + 8));
            assert_eq!(bounds(&hits[1]), (base + 6, base + 12));
            assert_eq!(result.get("literal_original_hex").text(), hex(&utf16("ana", little, false)));
            assert!(hits.iter().all(|hit| hit.get("decoded_range") == &Json::Null));
        }
    }
}

#[test]
fn ignore_case_applies_to_every_predicate_and_retains_primary_source_spelling() {
    let fixture = Fixture::new();
    fixture.file("a.rs", "ÄPFEL REQUIRED".as_bytes());
    fixture.file("b.rs", &utf16("ÄPFEL REQUIRED FORBIDDEN", true, true));
    fixture.file("c.rs", "ÄPFEL unrelated".as_bytes());
    let query = "äpfel required -forbidden";
    let (exit, exact) = invoke(&fixture.0, query, &["--workspace"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert!(exact.get("case_sensitive").flag());

    let (exit, result) = invoke(&fixture.0, query, &["--workspace", "--ignore-case"]);
    assert_eq!(exit, EXIT_OK);
    assert_expression(&result, query);
    assert!(!result.get("case_sensitive").flag());
    assert_eq!(result.get("normalization").text(), "exact");
    assert!(result.get("workspace_complete").flag());
    assert_eq!(result.get("matches_seen").number(), 1);
    let files = result.get("files").array();
    let hit = &files[0].get("hits").array()[0];
    assert_eq!(bounds(hit), (0, 6));
    assert_eq!(hit.get("matched_text").text(), "ÄPFEL");
    assert_eq!(hit.get("original_hex").text(), hex("ÄPFEL".as_bytes()));
    assert_eq!(files[0].get("literal_original_hex"), &Json::Null);
    assert_no_published_hits(&files[1]);
    assert_no_published_hits(&files[2]);
}

#[test]
fn lowercase_expansion_retains_the_complete_original_scalar_without_full_folding() {
    let fixture = Fixture::new();
    let file = fixture.file("expansion.rs", "İ READY".as_bytes());
    let (exit, result) = invoke(&file, "i ready -forbidden", &["--ignore-case"]);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result.get("matches_seen").number(), 1);
    let hit = &result.get("hits").array()[0];
    assert_eq!(bounds(hit), (0, 2));
    assert_eq!(hit.get("matched_text").text(), "İ");
    assert_eq!(hit.get("original_hex").text(), "c4b0");
    let file = fixture.file("not-full-fold.rs", "ß READY".as_bytes());
    let (exit, result) = invoke(&file, "ss ready", &["--ignore-case"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert_no_published_hits(&result);
}

#[test]
fn malformed_suffix_invalidates_early_anchor_and_predicate_candidates() {
    let fixture = Fixture::new();
    let mut text = String::from("needle required");
    text.extend(std::iter::repeat_n('x', 2 * INPUT_CHUNK));
    let mut utf8 = text.as_bytes().to_vec();
    utf8.push(0xff);
    let mut utf16 = utf16(&text, true, true);
    utf16.extend_from_slice(&0xd800u16.to_le_bytes());
    for (name, source) in [("bad-utf8.rs", utf8), ("bad-utf16.rs", utf16)] {
        let file = fixture.file(name, &source);
        let (exit, result) = invoke(&file, "needle required -forbidden", &["--limit", "1"]);
        assert_eq!(exit, EXIT_PARTIAL);
        assert_eq!(result.get("state").text(), "unsupported-text");
        assert_eq!(result.get("expression_qualified"), &Json::Null);
        assert!(!result.get("whole_file_complete").flag());
        assert!(!result.get("match_count_complete").flag());
        assert_eq!(result.get("retained_witness_bytes").number(), 0);
        assert_no_published_hits(&result);
    }
}

#[test]
fn byte_budget_cannot_qualify_an_unobserved_suffix_even_after_all_positive_terms() {
    let fixture = Fixture::new();
    let mut source = b"needle required".to_vec();
    source.resize(2 * INPUT_CHUNK, b'x');
    source.extend_from_slice(b"forbidden");
    let file = fixture.file("budget.rs", &source);
    for query in ["needle required -forbidden", "needle required"] {
        let (exit, result) = invoke(&file, query, &["--max-scan-bytes", "16384", "--count-all"]);
        assert_eq!(exit, EXIT_PARTIAL);
        assert_eq!(result.get("state").text(), "byte-limit");
        assert_eq!(result.get("expression_qualified"), &Json::Null);
        assert!(!result.get("whole_file_complete").flag());
        assert!(!result.get("match_count_complete").flag());
        assert!(!result.get("truncated").flag());
        assert_eq!(result.get("payload_bytes_read").number(), INPUT_CHUNK as u64);
        assert_eq!(result.get("retained_witness_bytes").number(), 0);
        assert_no_published_hits(&result);
    }
}

#[test]
fn retention_limits_do_not_stop_expression_qualification_or_counting_later_files() {
    let fixture = Fixture::new();
    let a = b"banana required";
    let rejected = b"banana required forbidden";
    let c = utf16("banana banana required", false, true);
    let absent = b"absent required";
    fixture.file("a.rs", a);
    fixture.file("b.rs", rejected);
    fixture.file("c.rs", &c);
    fixture.file("d.rs", absent);
    fixture.file("e.rs", a);
    let payload = (2 * a.len() + rejected.len() + c.len() + absent.len()) as u64;
    for count_all in [false, true] {
        for limit in ["0", "1"] {
            let mut flags = vec!["--workspace", "--limit", limit];
            if count_all { flags.push("--count-all"); }
            let (exit, result) = invoke(&fixture.0, "ana required -forbidden", &flags);
            assert_eq!(exit, EXIT_PARTIAL);
            assert_eq!(result.get("count_all_matches").flag(), count_all);
            assert!(result.get("workspace_complete").flag());
            assert!(result.get("match_count_complete").flag());
            assert!(result.get("truncated").flag());
            assert_eq!(result.get("matches_seen").number(), 8);
            assert_eq!(result.get("stored_hits").number(), limit.parse::<u64>().unwrap());
            assert_eq!(result.get("matching_files").number(), 3);
            assert_eq!(result.get("files_examined").number(), 5);
            assert_eq!(result.get("unexamined_files").number(), 0);
            assert_eq!(result.get("payload_bytes_read").number(), payload);
            assert_eq!(result.get("stop_reason"), &Json::Null);
            let files = result.get("files").array();
            assert_eq!(files[2].get("matches_seen").number(), 4);
            assert!(files[2].get("hits").array().is_empty());
            assert_eq!(files[4].get("matches_seen").number(), 2);
            assert_no_published_hits(&files[1]);
            assert_no_published_hits(&files[3]);
            assert!(files.iter().all(|file| file.get("whole_file_complete").flag()));
        }
    }
}

#[test]
fn exact_full_and_zero_row_limits_distinguish_qualified_occurrences_from_rejected_files() {
    let fixture = Fixture::new();
    let file = fixture.file("one.rs", b"banana required");
    let (exit, exact) = invoke(&file, "ana required -forbidden", &["--limit", "2"]);
    assert_eq!(exit, EXIT_OK);
    assert!(!exact.get("truncated").flag());
    assert_eq!(exact.get("matches_seen").number(), 2);
    assert_eq!(exact.get("stored_hits").number(), 2);
    for count_all in [false, true] {
        let mut flags = vec!["--limit", "0"];
        if count_all { flags.push("--count-all"); }
        let (exit, present) = invoke(&file, "ana required -forbidden", &flags);
        assert_eq!(exit, EXIT_PARTIAL);
        assert!(present.get("whole_file_complete").flag());
        assert!(present.get("match_count_complete").flag());
        assert!(present.get("truncated").flag());
        assert_eq!(present.get("matches_seen").number(), 2);
        assert!(present.get("hits").array().is_empty());
        assert_eq!(present.get("literal_original_hex"), &Json::Null);
        assert_eq!(present.get("retained_witness_bytes").number(), 0);
        for query in ["missing required", "ana absent", "ana required -banana"] {
            let (exit, absent) = invoke(&file, query, &flags);
            assert_eq!(exit, EXIT_NO_MATCH);
            assert!(!absent.get("expression_qualified").flag());
            assert!(absent.get("whole_file_complete").flag());
            assert!(absent.get("match_count_complete").flag());
            assert!(!absent.get("truncated").flag());
            assert_no_published_hits(&absent);
        }
    }
}

#[test]
fn incomplete_workspace_preserves_only_completed_qualified_files() {
    let fixture = Fixture::new();
    let complete = b"needle required";
    fixture.file("a.rs", complete);
    fixture.file("b.rs", b"needle required........forbidden");
    fixture.file("c.rs", complete);
    let allowance = (complete.len() + 8).to_string();
    let (exit, result) = invoke(&fixture.0, "needle required -forbidden",
        &["--workspace", "--max-scan-bytes", &allowance]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!result.get("workspace_complete").flag());
    assert!(!result.get("match_count_complete").flag());
    assert_eq!(result.get("scope_files").number(), 3);
    assert_eq!(result.get("files_examined").number(), 2);
    assert_eq!(result.get("unexamined_files").number(), 1);
    assert_eq!(result.get("matches_seen").number(), 1);
    assert_eq!(result.get("stored_hits").number(), 1);
    assert_eq!(result.get("payload_bytes_read").number(), (complete.len() + 8) as u64);
    assert_eq!(result.get("stop_reason").text(), "byte-limit");
    let files = result.get("files").array();
    assert!(files[0].get("expression_qualified").flag());
    assert_eq!(files[1].get("expression_qualified"), &Json::Null);
    assert_no_published_hits(&files[1]);
}

#[test]
fn expression_syntax_and_conflicting_options_are_checked_before_source_io() {
    let fixture = Fixture::new();
    let missing = fixture.0.join("does-not-exist.rs");
    for (query, code) in [
        ("\"unfinished", "QUERY_SYNTAX_ERROR"),
        ("regex:.*", "QUERY_REGEX_UNQUALIFIED"),
        ("", "QUERY_EMPTY"),
        ("-forbidden", "QUERY_EMPTY"),
        ("path:src/", "QUERY_EMPTY"),
    ] {
        let (exit, error) = invoke(&missing, query, &[]);
        assert_eq!(exit, EXIT_ERROR);
        assert_eq!(error.get("error").get("code").text(), code);
    }
    for flags in [
        vec!["--text", "needle"],
        vec!["--raw-hex", "6e"],
        vec!["--identifier", "needle"],
        vec!["--offset", "0"],
        vec!["--bytes", "4"],
        vec!["--stdin"],
        vec!["--max-file-bytes", "16"],
        vec!["--max-total-bytes", "16"],
    ] {
        let (exit, error) = invoke(&missing, "needle required", &flags);
        assert_eq!(exit, EXIT_ERROR);
        assert_eq!(error.get("error").get("code").text(), "CLI_INCOMPATIBLE_OPTIONS");
    }
}

#[test]
fn literal_text_keeps_query_punctuation_as_source_data() {
    let fixture = Fixture::new();
    let file = fixture.file("literal.rs", b"needle -forbidden");
    let mut literal = args(&file, "needle -forbidden", &[]);
    literal[3] = "--text".into();
    let (exit, result) = invoke_args(&literal, || false);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result.get("mode").text(), "exact-decoded-text");
    assert_eq!(result.get("matches_seen").number(), 1);
    assert_eq!(bounds(&result.get("hits").array()[0]), (0, 17));
    let (exit, result) = invoke(&file, "needle -forbidden", &[]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert_no_published_hits(&result);
}

#[test]
fn cancellation_during_an_expression_observation_emits_one_terminal_error() {
    let fixture = Fixture::new();
    let mut source = b"needle required".to_vec();
    source.resize(2 * 1024 * 1024, b'x');
    let file = fixture.file("cancel.rs", &source);
    let mut checkpoints = 0;
    let (exit, error) = invoke_args(&args(&file, "needle required -forbidden", &[]), || {
        checkpoints += 1;
        checkpoints > 16
    });
    assert_eq!(exit, EXIT_CANCELED);
    assert_eq!(error.get("status").text(), "error");
    assert_eq!(error.get("error").get("code").text(), "CLI_CANCELED");
    assert_eq!(fs::read(&file).unwrap(), source);
}

#[test]
fn executable_routes_named_file_and_workspace_through_streaming_expressions() {
    let fixture = Fixture::new();
    let file = fixture.file("src/main.rs", b"banana required");
    fixture.file("src/excluded.py", &[0xff]);
    let query = "ana required -forbidden path:src/ lang:rust";
    for (path, flags) in [(&file, vec![]), (&fixture.0, vec!["--workspace"])] {
        let output = Command::new(env!("CARGO_BIN_EXE_fcb"))
            .args(args(path, query, &flags)).output().unwrap();
        assert_eq!(output.status.code(), Some(EXIT_OK as i32));
        assert!(output.stderr.is_empty());
        let result = parse(&output.stdout).unwrap();
        assert_expression(&result, query);
        assert_eq!(result.get("matches_seen").number(), 2);
        let report = if flags.contains(&"--workspace") {
            &result.get("files").array()[0]
        } else {
            &result
        };
        assert_eq!(bounds(&report.get("hits").array()[0]), (1, 4));
        assert_eq!(bounds(&report.get("hits").array()[1]), (3, 6));
    }
}

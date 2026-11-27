#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Public indexed-expression routes must preserve direct-source answers,
//! original witnesses and incomplete-scope honesty across all three layouts.

mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use std::{ffi::OsString, io::{self, Read}, path::PathBuf, process::Command};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};
use fixture::{entry, Fixture};
use support::{Json, parse};

struct NoInput;
impl Read for NoInput {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("saved expression must not read stdin") }
}
struct Index { path: PathBuf, pin: String, paged: bool }
fn invoke(args: &[OsString]) -> (u8, Json) {
    let mut out = Vec::new(); let mut err = Vec::new();
    let exit = run(args, &mut NoInput, &mut out, &mut err, || false);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    let json = parse(&out).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&out)));
    assert_eq!(json.get("schema").text(), "fcb.cli/1");
    (exit, json)
}
fn indexes(f: &Fixture, quota: Option<&str>) -> Vec<Index> {
    ["segments", "postings", "paged"].into_iter().map(|layout| {
        let path = f.root.join(format!("expression-{layout}-{}.index", quota.unwrap_or("default")));
        let mut args = vec!["snapshot".into(), "index".into(), "build".into(), f.path.clone().into_os_string(),
            "--output".into(), path.clone().into_os_string(), "--json".into()];
        if layout == "postings" { args.push("--inverted".into()); }
        if layout == "paged" { args.push("--paged".into()); }
        if let Some(quota) = quota { args.extend(["--max-grams".into(), quota.into()]); }
        let (exit, receipt) = invoke(&args);
        assert!(matches!(exit, EXIT_OK | EXIT_PARTIAL));
        Index { path, pin: receipt.get("index_digest").text().to_owned(), paged: layout == "paged" }
    }).collect()
}
fn args(f: &Fixture, index: Option<&Index>, query: &str, tail: &[&str]) -> Vec<OsString> {
    let mut args = vec!["snapshot".into()];
    if index.is_some() { args.push("index".into()); }
    args.extend(["search".into(), f.path.clone().into_os_string(), "--json".into(),
        "--query".into(), query.into()]);
    if let Some(index) = index {
        args.extend(["--index".into(), index.path.clone().into_os_string(),
            "--index-digest".into(), index.pin.clone().into()]);
        if index.paged { args.push("--paged".into()); }
    }
    args.extend(tail.iter().map(OsString::from)); args
}
fn parity(direct: &Json, indexed: &Json) {
    for field in ["mode", "case_sensitive", "normalization", "expression", "primary_needle",
        "scope", "scope_path_hex", "scope_known_members", "scope_captured_members",
        "scope_membership_complete", "scope_complete", "workspace_complete", "truncated",
        "count_all_matches", "skip_matches", "skipped_matches", "has_more_matches", "next_match_offset",
        "matches_seen_exact", "match_count_complete", "metadata_examined", "metadata_excluded_files",
        "scope_files", "unavailable_files_count", "matches_seen", "stored_hits", "matching_files",
        "retained_matching_files", "hits"] {
        assert_eq!(indexed.get(field), direct.get(field), "{field}");
    }
    assert!(indexed.get("index_used").flag()); assert!(!direct.get("index_used").flag());
    assert!(!indexed.get("live_roots_accessed").flag());
    assert_eq!(indexed.get("index_prefilter").text(), "mandatory-primary-literal-only");
}
fn closed_source_accounting(page: &Json) {
    assert_eq!(page.get("members_loaded").number() + page.get("index_eliminated_files").number(),
        page.get("scope_files").number() - page.get("unavailable_files_count").number());
}

#[test]
fn all_layouts_intersect_raw_scope_metadata_predicates_and_match_windows() {
    let f = Fixture::new(&[
        entry(b"docs/decoy.rs", b"needle required needle"),
        entry(b"src/a.rs", b"needle required needle"),
        entry(b"src/b.rs", b"filler required"),
        entry(b"src/c.rs", b"needle required forbidden"),
        entry(b"src/d.py", b"needle required"),
        entry(b"src/nested/\xff.rs", b"needle required needle"),
        SnapshotEntry { path: b"src-other/lost.rs", observed_bytes: 99, data: SnapshotData::Unavailable("missing") },
    ], true);
    let query = "needle required -forbidden lang:rust";
    let tail = ["--directory", "src", "--count-all", "--skip-matches", "1", "--limit", "2"];
    let (exit, direct) = invoke(&args(&f, None, query, &tail)); assert_eq!(exit, EXIT_OK);
    assert_eq!(direct.get("scope_known_members").number(), 5);
    assert_eq!(direct.get("scope_files").number(), 4);
    assert_eq!(direct.get("matches_seen").number(), 4);
    assert_eq!(direct.get("next_match_offset").number(), 3);
    assert_eq!(direct.get("members_loaded").number(), 4);
    for index in indexes(&f, None) {
        let (exit, page) = invoke(&args(&f, Some(&index), query, &tail)); assert_eq!(exit, EXIT_OK);
        parity(&direct, &page); closed_source_accounting(&page);
        assert_eq!(page.get("index_eliminated_files").number(), 1);
        assert_eq!(page.get("index_candidates").number(), 3);
        assert_eq!(page.get("index_fallback_files").number(), 0);
        assert_eq!(page.get("members_loaded").number(), 3);
        assert_eq!(page.get("predicate_rejections").number(), 1);
        assert!(page.get("loaded_source_bytes").number() < direct.get("loaded_source_bytes").number());
        assert_eq!(page.get("index_digest").text(), index.pin);
        if index.paged { assert!(page.get("index_page_loads").number() > 0); }
    }
}

#[test]
fn index_negatives_close_with_zero_scan_work_but_candidates_and_unknown_scopes_stay_partial() {
    let f = Fixture::new(&[entry(b"src/a.rs", b"needle required"), entry(b"src/b.rs", b"filler required"),
        SnapshotEntry { path: b"lost.rs", observed_bytes: 6, data: SnapshotData::Unavailable("missing") }], false);
    for index in indexes(&f, None) {
        let (exit, absent) = invoke(&args(&f, Some(&index), "needle required",
            &["--member", "src/b.rs", "--count-all", "--limit", "0", "--max-scan-bytes", "0"]));
        assert_eq!(exit, EXIT_NO_MATCH); assert!(absent.get("matches_seen_exact").flag());
        assert!(absent.get("scope_complete").flag()); assert!(!absent.get("workspace_complete").flag());
        assert_eq!(absent.get("members_loaded").number(), 0);
        assert_eq!(absent.get("scanned_input_bytes").number(), 0);
        assert_eq!(absent.get("index_eliminated_files").number(), 1); closed_source_accounting(&absent);
        for (scope, path) in [("--member", "src/a.rs"), ("--directory", "absent"),
            ("--member", "missing.rs"), ("--member", "lost.rs")] {
            let (exit, page) = invoke(&args(&f, Some(&index), "needle required",
                &[scope, path, "--count-all", "--limit", "0", "--max-scan-bytes", "0"]));
            assert_eq!(exit, EXIT_PARTIAL, "{scope} {path}");
            assert!(!page.get("matches_seen_exact").flag()); assert!(!page.get("scope_complete").flag());
            assert_eq!(page.get("matches_seen").number(), 0);
            assert_eq!(page.get("scanned_input_bytes").number(), 0);
        }
        let (exit, excluded) = invoke(&args(&f, Some(&index), "needle -path:lost",
            &["--member", "lost.rs", "--count-all", "--max-scan-bytes", "0"]));
        assert_eq!(exit, EXIT_NO_MATCH); assert_eq!(excluded.get("scope_files").number(), 0);
        assert_eq!(excluded.get("index_eliminated_files").number(), 0);
        assert_eq!(excluded.get("index_fallback_files").number(), 0);
        assert!(excluded.get("scope_complete").flag()); closed_source_accounting(&excluded);
    }
}

#[test]
fn unicode_utf16_short_terms_and_uncovered_indexes_keep_direct_expression_answers() {
    let wide: Vec<_> = "\u{feff}BANANA REQUIRED".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let f = Fixture::new(&[entry(b"src/a.rs", b"BANANA REQUIRED"),
        entry(b"src/b.rs", &wide), entry(b"src/c.rs", "KELVIN REQUIRED".as_bytes()),
        entry(b"src/d.rs", b"KELVIN REQUIRED"), entry(b"src/e.rs", b"irrelevant required")], true);
    let normal = indexes(&f, None); let uncovered = indexes(&f, Some("0"));
    for query in ["ana required -forbidden", "k required -forbidden", "kelvin required -forbidden"] {
        let tail = ["--directory", "src", "--ignore-case", "--count-all"];
        let (exit, direct) = invoke(&args(&f, None, query, &tail)); assert_eq!(exit, EXIT_OK);
        for index in normal.iter().chain(&uncovered) {
            let (exit, page) = invoke(&args(&f, Some(index), query, &tail)); assert_eq!(exit, EXIT_OK);
            parity(&direct, &page); closed_source_accounting(&page);
            assert_eq!(page.get("index_policy").text(), "ascii-case-variants-with-source-fallback");
            assert!(page.get("index_fallback_files").number() > 0);
        }
        for index in &uncovered {
            let (_, page) = invoke(&args(&f, Some(index), query, &tail));
            assert_eq!(page.get("index_eliminated_files").number(), 0);
            assert_eq!(page.get("index_fallback_files").number(), 5);
            assert_eq!(page.get("members_loaded").number(), 5);
        }
        if query.starts_with("ana") {
            let hits = direct.get("hits").array(); assert_eq!(hits.len(), 4);
            assert_eq!(hits[2].get("original_range").get("start").number(), 4);
            assert_eq!(hits[2].get("original_range").get("end").number(), 10);
            assert_eq!(hits[3].get("original_range").get("start").number(), 8);
            assert_eq!(hits[3].get("original_range").get("end").number(), 14);
            assert_eq!(hits[2].get("matched_text"), &Json::Null);
        }
    }
}

#[test]
fn counted_and_noncounted_later_windows_match_direct_exits_and_never_stall() {
    let f = Fixture::new(&[entry(b"a.rs", b"banana banana required"), entry(b"b.rs", b"banana required"),
        entry(b"negative.rs", b"unrelated required")], true);
    let indexes = indexes(&f, None);
    for tail in [
        vec!["--count-all", "--skip-matches", "3", "--limit", "2"],
        vec!["--count-all", "--skip-matches", "99", "--limit", "2"],
        vec!["--count-all", "--limit", "0"],
        vec!["--skip-matches", "3", "--limit", "2"],
        vec!["--skip-matches", "3", "--limit", "0"],
        vec!["--count-all", "--max-scan-bytes", "4", "--limit", "0"],
    ] {
        let (expected, direct) = invoke(&args(&f, None, "ana required -forbidden", &tail));
        for index in &indexes {
            let (exit, page) = invoke(&args(&f, Some(index), "ana required -forbidden", &tail));
            assert_eq!(exit, expected); parity(&direct, &page);
            assert_eq!(page.get("work_limited"), direct.get("work_limited"));
            if page.get("stored_hits").number() == 0 {
                assert_eq!(page.get("next_match_offset"), &Json::Null);
            }
        }
    }
}

#[test]
fn malformed_expressions_and_incompatible_flags_fail_before_opening_archive_or_index() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle")], true);
    let absent = Index { path: f.root.join("absent.index"), pin: "00".repeat(32), paged: true };
    for (query, tail, expected) in [
        ("\"unterminated", vec![], "QUERY_SYNTAX_ERROR"),
        ("regex:needle", vec![], "QUERY_REGEX_UNQUALIFIED"),
        ("needle", vec!["--text", "other"], "CLI_INCOMPATIBLE_OPTIONS"),
        ("needle", vec!["--raw-hex", "78"], "CLI_INCOMPATIBLE_OPTIONS"),
        ("needle", vec!["--query", "other"], "CLI_DUPLICATE_OPTION"),
        ("needle", vec!["--max-scan-bytes", "0", "--max-scan-bytes", "1"], "CLI_DUPLICATE_OPTION"),
        ("needle", vec!["--directory", "../src"], "SNAPSHOT_QUERY_INVALID_SCOPE"),
    ] {
        let mut input = args(&f, Some(&absent), query, &tail);
        input[3] = f.root.join("absent.fcbs").into_os_string();
        let (exit, error) = invoke(&input); assert_eq!(exit, EXIT_ERROR);
        assert_eq!(error.get("error").get("code").text(), expected);
    }
    let mut literal = args(&f, Some(&absent), "needle", &["--max-scan-bytes", "0"]);
    let query_flag = literal.iter().position(|value| value == "--query").unwrap();
    literal[query_flag] = "--text".into();
    let (exit, error) = invoke(&literal); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(error.get("error").get("code").text(), "CLI_INCOMPATIBLE_OPTIONS");
}

#[test]
fn trusted_index_pin_is_required_even_for_provable_absence_and_cancellation_is_terminal() {
    let f = Fixture::new(&[entry(b"a.rs", b"filler")], true);
    for mut index in indexes(&f, None) {
        index.pin = "00".repeat(32);
        let input = args(&f, Some(&index), "needle required", &["--count-all", "--max-scan-bytes", "0"]);
        let (exit, page) = invoke(&input); assert_eq!(exit, EXIT_ERROR);
        assert_eq!(page.get("error").get("code").text(), "SAVED_INDEX_PIN_MISMATCH");
        let mut out = Vec::new(); let mut err = Vec::new();
        assert_eq!(run(&input, &mut NoInput, &mut out, &mut err, || true), EXIT_CANCELED);
        assert!(err.is_empty()); let stopped = parse(&out).unwrap();
        assert_eq!(stopped.get("status").text(), "error");
        assert_eq!(stopped.get("error").get("code").text(), "SNAPSHOT_CANCELED");
    }
}

#[test]
fn standalone_binary_and_human_output_expose_indexed_predicate_results() {
    let f = Fixture::new(&[entry(b"src/a.rs", b"needle required"), entry(b"src/b.rs", b"filler required")], true);
    let mut indexes = indexes(&f, None); let index = indexes.pop().unwrap();
    let input = args(&f, Some(&index), "needle required -forbidden",
        &["--member", "src/a.rs", "--count-all"]);
    let result = Command::new(env!("CARGO_BIN_EXE_fcb")).args(&input).output().unwrap();
    assert_eq!(result.status.code(), Some(EXIT_OK as i32)); assert!(result.stderr.is_empty());
    let page = parse(&result.stdout).unwrap(); assert!(page.get("index_used").flag());
    assert_eq!(page.get("index_layout").text(), "demand-paged-postings-v1");
    assert_eq!(page.get("matches_seen").number(), 1);
    let human_args: Vec<_> = input.into_iter().filter(|value| value != "--json").collect();
    let mut out = Vec::new(); let mut err = Vec::new();
    assert_eq!(run(&human_args, &mut NoInput, &mut out, &mut err, || false), EXIT_OK);
    assert!(err.is_empty()); let human = String::from_utf8(out).unwrap();
    assert!(human.contains("Scope: saved-member"));
    assert!(human.contains("Pinned index filters only the mandatory primary literal"));
    assert!(human.contains("Primary matches seen: 1 (exact)"));
    assert!(human.contains("Index-pruned members:"));
}

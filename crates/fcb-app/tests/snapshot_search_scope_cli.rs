#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Raw saved-member selection applies before metadata predicates, index pruning,
//! source reads and match windows. This exercises the public command routes.

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use std::{ffi::OsString, io::{self, Read}, path::PathBuf, process::Command};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_ERROR};
use saved_fixture::{entry, Fixture};
use support::{Json, parse};

struct NoInput;
impl Read for NoInput {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("saved scope search must not consume stdin") }
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
fn indexes(f: &Fixture) -> Vec<Index> {
    ["segments", "postings", "paged"].into_iter().map(|layout| {
        let path = f.root.join(format!("scope-{layout}.index"));
        let mut args = vec!["snapshot".into(), "index".into(), "build".into(), f.path.clone().into_os_string(),
            "--output".into(), path.clone().into_os_string(), "--json".into()];
        if layout == "postings" { args.push("--inverted".into()); }
        if layout == "paged" { args.push("--paged".into()); }
        let (exit, receipt) = invoke(&args);
        assert!(matches!(exit, EXIT_OK | EXIT_PARTIAL));
        Index { path, pin: receipt.get("index_digest").text().to_owned(), paged: layout == "paged" }
    }).collect()
}
fn args(f: &Fixture, index: Option<&Index>, options: &[&str]) -> Vec<OsString> {
    let mut args = vec!["snapshot".into()];
    if index.is_some() { args.push("index".into()); }
    args.extend(["search".into(), f.path.clone().into_os_string(), "--json".into()]);
    if let Some(index) = index {
        args.extend(["--index".into(), index.path.clone().into_os_string(),
            "--index-digest".into(), index.pin.clone().into()]);
        if index.paged { args.push("--paged".into()); }
    }
    args.extend(options.iter().map(OsString::from)); args
}
fn scope(page: &Json, label: &str, path: &str, members: u64, captured: u64, membership: bool) {
    assert_eq!(page.get("scope").text(), label);
    assert_eq!(page.get("scope_path_hex").text(), path);
    assert_eq!(page.get("scope_known_members").number(), members);
    assert_eq!(page.get("scope_captured_members").number(), captured);
    assert_eq!(page.get("scope_membership_complete").flag(), membership);
    assert!(!page.get("workspace_complete").flag());
    assert!(!page.get("live_roots_accessed").flag());
}
fn range(hit: &Json) -> (u64, u64) {
    let range = hit.get("original_range");
    (range.get("start").number(), range.get("end").number())
}

#[test]
fn every_index_layout_scopes_before_counts_windows_and_source_loading() {
    let f = Fixture::new(&[entry(b"docs/decoy.rs", b"banana banana banana"),
        entry(b"src/a.rs", b"banana banana"), entry(b"src/nested/\xff.rs", b"banana"),
        entry(b"src-other/decoy.rs", b"banana"), entry(b"src\\decoy.rs", b"banana"),
        SnapshotEntry { path: b"unavailable.rs", observed_bytes: 99, data: SnapshotData::Unavailable("missing") }], true);
    let indexes = indexes(&f);
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        for (option, value) in [("--text", "ana"), ("--raw-hex", "616e61")] {
            let (exit, page) = invoke(&args(&f, index, &[option, value, "--directory", "src",
                "--skip-matches", "3", "--limit", "2", "--count-all"]));
            assert_eq!(exit, EXIT_OK);
            scope(&page, "saved-directory", "737263", 2, 2, true);
            assert!(page.get("scope_complete").flag()); assert!(page.get("matches_seen_exact").flag());
            assert_eq!(page.get("scope_unavailable_members").number(), 0);
            assert_eq!(page.get("member_payload_bytes_loaded").number(), 19);
            assert_eq!(page.get("matches_seen").number(), 6);
            assert_eq!(page.get("skipped_matches").number(), 3);
            assert_eq!(page.get("next_match_offset").number(), 5);
            let hits = page.get("hits").array(); assert_eq!(hits.len(), 2);
            assert_eq!(range(&hits[0]), (10, 13)); assert_eq!(range(&hits[1]), (1, 4));
            assert_eq!(hits[0].get("path").get("hex").text(), "7372632f612e7273");
            assert_eq!(hits[1].get("path").get("hex").text(), "7372632f6e65737465642fff2e7273");

            let (exit, exact) = invoke(&args(&f, index, &[option, value,
                "--member-hex", "7372632f6e65737465642fff2e7273", "--count-all"]));
            assert_eq!(exit, EXIT_OK);
            scope(&exact, "saved-member", "7372632f6e65737465642fff2e7273", 1, 1, true);
            assert_eq!(exact.get("scope_path"), exact.get("hits").array()[0].get("path"));
            assert_eq!(exact.get("matches_seen").number(), 2);
            assert_eq!(exact.get("member_payload_bytes_loaded").number(), 6);

            let (exit, empty) = invoke(&args(&f, index, &[option, value, "--directory", "absent", "--count-all"]));
            assert_eq!(exit, EXIT_NO_MATCH); assert_eq!(empty.get("scope_known_members").number(), 0);
            assert!(empty.get("scope_complete").flag()); assert_eq!(empty.get("member_payload_bytes_loaded").number(), 0);
        }
    }
}

#[test]
fn raw_scope_labels_preserve_utf16_case_insensitive_source_witnesses() {
    let body: Vec<_> = "\u{feff}BANANA".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let f = Fixture::new(&[entry(b"src/\xff/a.txt", &body), entry(b"src/\xff2/decoy.txt", b"BANANA"),
        entry(b"src2/decoy.txt", b"BANANA")], true);
    let indexes = indexes(&f);
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        let (exit, page) = invoke(&args(&f, index, &["--text", "ana", "--ignore-case",
            "--directory-hex", "7372632fff", "--count-all", "--skip-matches", "1", "--limit", "1"]));
        assert_eq!(exit, EXIT_OK);
        scope(&page, "saved-directory", "7372632fff", 1, 1, true);
        assert_eq!(page.get("matches_seen").number(), 2);
        let hit = &page.get("hits").array()[0];
        assert_eq!(range(hit), (8, 14));
        assert_eq!(hit.get("original_hex").text(), "41004e004100");
        assert_eq!(hit.get("matched_text").text(), "ANA");
    }
}

#[test]
fn expressions_intersect_exact_raw_scope_with_metadata_and_whole_member_predicates() {
    let f = Fixture::new(&[entry(b"docs/a.rs", b"needle required"),
        entry(b"src/a.rs", b"needle required needle"), entry(b"src/b.py", b"needle required"),
        entry(b"src/c.rs", b"needle required forbidden"),
        SnapshotEntry { path: b"src-other/lost.rs", observed_bytes: 99, data: SnapshotData::Unavailable("missing") }], true);
    let (exit, page) = invoke(&args(&f, None, &["--query", "needle required -forbidden lang:rust",
        "--directory", "src", "--count-all", "--skip-matches", "1", "--limit", "1"]));
    assert_eq!(exit, EXIT_OK);
    scope(&page, "saved-directory", "737263", 3, 3, true);
    assert_eq!(page.get("metadata_examined").number(), 3);
    assert_eq!(page.get("metadata_excluded_files").number(), 1);
    assert_eq!(page.get("scope_files").number(), 2);
    assert_eq!(page.get("unavailable_files_count").number(), 0);
    assert_eq!(page.get("matches_seen").number(), 2);
    assert_eq!(range(&page.get("hits").array()[0]), (16, 22));

    // Metadata can prove a selected unavailable member irrelevant; selected
    // membership remains explicit even when the expression has an empty scope.
    let (exit, excluded) = invoke(&args(&f, None, &["--query", "needle -path:lost",
        "--member", "src-other/lost.rs", "--count-all", "--limit", "0"]));
    assert_eq!(exit, EXIT_NO_MATCH);
    scope(&excluded, "saved-member", "7372632d6f746865722f6c6f73742e7273", 1, 0, true);
    assert!(excluded.get("scope_complete").flag());
    assert_eq!(excluded.get("scope_files").number(), 0);
}

#[test]
fn incomplete_discovery_closes_only_observed_members_and_never_a_missing_selection() {
    let f = Fixture::new(&[entry(b"src/a.rs", b"needle"),
        SnapshotEntry { path: b"lost.rs", observed_bytes: 6, data: SnapshotData::Unavailable("missing") }], false);
    let indexes = indexes(&f);
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        let queries = if index.is_none() { vec!["--text", "--query"] } else { vec!["--text"] };
        for query in queries {
            for (flag, path, expected, membership, members, captured) in [
                ("--member", "src/a.rs", EXIT_OK, true, 1, 1),
                ("--member", "lost.rs", EXIT_PARTIAL, true, 1, 0),
                ("--member", "unknown.rs", EXIT_PARTIAL, false, 0, 0),
                ("--directory", "src", EXIT_PARTIAL, false, 1, 1),
                ("--directory", "absent", EXIT_PARTIAL, false, 0, 0),
            ] {
                let (exit, page) = invoke(&args(&f, index, &[query, "needle", flag, path, "--count-all"]));
                assert_eq!(exit, expected, "{query} {flag} {path}");
                assert_eq!(page.get("scope_membership_complete").flag(), membership);
                assert_eq!(page.get("scope_known_members").number(), members);
                assert_eq!(page.get("scope_captured_members").number(), captured);
                assert_eq!(page.get("matches_seen_exact").flag(), expected == EXIT_OK);
                assert!(!page.get("discovery_complete").flag()); assert!(!page.get("workspace_complete").flag());
            }
        }
    }
}

#[test]
fn invalid_or_conflicting_scopes_are_rejected_before_archive_or_index_access() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle")], true);
    let absent = Index { path: f.root.join("missing.index"), pin: "00".repeat(32), paged: false };
    for index in [None, Some(&absent)] {
        for extra in [vec!["--member", "../a.rs"], vec!["--directory", "/src"],
            vec!["--directory", "src//a"], vec!["--member-hex", "610062"],
            vec!["--directory", "src", "--member", "src/a.rs"],
            vec!["--directory", "src", "--directory-hex", "737263"],
            vec!["--member", "a.rs", "--member-hex", "612e7273"]] {
            let mut options = vec!["--text", "needle"]; options.extend(extra);
            let mut input = args(&f, index, &options);
            input[if index.is_some() { 3 } else { 2 }] = f.root.join("missing.fcbs").into_os_string();
            let (exit, result) = invoke(&input);
            assert_eq!(exit, EXIT_ERROR);
            assert!(matches!(result.get("error").get("code").text(),
                "SNAPSHOT_QUERY_INVALID_SCOPE" | "CLI_INCOMPATIBLE_OPTIONS" | "CLI_DUPLICATE_OPTION" | "CLI_INVALID_MEMBER"));
        }
    }
}

#[test]
fn standalone_binary_and_human_receipts_show_the_selected_saved_scope() {
    let f = Fixture::new(&[entry(b"never-created/a.rs", b"needle"), entry(b"other.rs", b"needle")], true);
    let options = args(&f, None, &["--text", "needle", "--member", "never-created/a.rs", "--count-all"]);
    let output = Command::new(env!("CARGO_BIN_EXE_fcb")).args(&options).output().unwrap();
    assert_eq!(output.status.code(), Some(EXIT_OK as i32)); assert!(output.stderr.is_empty());
    let page = parse(&output.stdout).unwrap();
    scope(&page, "saved-member", "6e657665722d637265617465642f612e7273", 1, 1, true);
    let options: Vec<_> = options.into_iter().filter(|arg| arg != "--json").collect();
    let mut out = Vec::new(); let mut err = Vec::new();
    assert_eq!(run(&options, &mut NoInput, &mut out, &mut err, || false), EXIT_OK);
    let human = String::from_utf8(out).unwrap();
    assert!(human.contains("Scope: saved-member")); assert!(human.contains("known members: 1"));
    assert!(human.contains("Matches seen: 1 (exact)")); assert!(err.is_empty());
}

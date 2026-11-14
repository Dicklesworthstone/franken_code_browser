#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Public command coverage for bounded saved result windows. Expected ranges
//! are calculated from fixture bytes, independently of the production matcher.
//! Archive members are never created as live source paths.

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use std::{ffi::OsString, io::{self, Read}, path::PathBuf, process::Command};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};
use saved_fixture::{entry, Fixture};
use support::{Json, parse};

struct NoInput;
impl Read for NoInput {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("saved result windows must not consume stdin")
    }
}
#[derive(Clone, Copy)]
enum Layout { Segments, Postings, Paged }
struct Index { path: PathBuf, pin: String, paged: bool }
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut out = Vec::new(); let mut err = Vec::new();
    let exit = run(args, &mut NoInput, &mut out, &mut err, canceled);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    let json = parse(&out).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&out)));
    assert_eq!(json.get("schema").text(), "fcb.cli/1");
    (exit, json)
}
fn indexes(f: &Fixture) -> Vec<Index> {
    [Layout::Segments, Layout::Postings, Layout::Paged].into_iter().enumerate().map(|(i, layout)| {
        let path = f.root.join(format!("windows-{i}.index"));
        let mut args = vec!["snapshot".into(), "index".into(), "build".into(), f.path.clone().into_os_string(),
            "--output".into(), path.clone().into_os_string(), "--json".into()];
        match layout {
            Layout::Segments => {},
            Layout::Postings => args.push("--inverted".into()),
            Layout::Paged => args.push("--paged".into()),
        }
        let (exit, receipt) = invoke(&args, || false);
        assert!(matches!(exit, EXIT_OK | EXIT_PARTIAL));
        Index { path, pin: receipt.get("index_digest").text().to_owned(), paged: matches!(layout, Layout::Paged) }
    }).collect()
}
fn search_args(f: &Fixture, index: Option<&Index>, options: &[&str]) -> Vec<OsString> {
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
fn structure_args(f: &Fixture, mode: &str, name: &str, options: &[&str]) -> Vec<OsString> {
    let mut args = vec!["snapshot".into(), "structure".into(), f.path.clone().into_os_string(),
        "--mode".into(), mode.into(), "--name".into(), name.into(), "--json".into()];
    args.extend(options.iter().map(OsString::from)); args
}
fn range(hit: &Json) -> (u64, u64) {
    let value = hit.get("original_range");
    (value.get("start").number(), value.get("end").number())
}
fn window(result: &Json, offset: u64, skipped: u64, retained: usize, next: Option<u64>) {
    assert_eq!(result.get("result_offset").number(), offset);
    assert_eq!(result.get("skipped_matches").number(), skipped);
    assert_eq!(result.get("hits").array().len(), retained);
    match next {
        Some(value) => assert_eq!(result.get("next_match_offset").number(), value),
        None => assert_eq!(result.get("next_match_offset"), &Json::Null),
    }
    assert!(!result.get("live_roots_accessed").flag());
    assert_eq!(result.get("snapshot_digest").text().len(), 64);
}
fn utf16(text: &str, big_endian: bool) -> Vec<u8> {
    let mut bytes = if big_endian { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for word in text.encode_utf16() {
        bytes.extend_from_slice(&if big_endian { word.to_be_bytes() } else { word.to_le_bytes() });
    }
    bytes
}

#[test]
fn direct_and_every_index_layout_skip_verified_overlapping_matches_across_members() {
    let f = Fixture::new(&[entry(b"a.rs", b"banana"), entry(b"b.rs", b"unrelated"),
        entry(b"c.rs", b"banana banana")], true);
    let indexes = indexes(&f);
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        for (option, value) in [("--text", "ana"), ("--raw-hex", "616e61")] {
            let (exit, page) = invoke(&search_args(&f, index,
                &[option, value, "--skip-matches", "3", "--limit", "2"]), || false);
            assert_eq!(exit, EXIT_PARTIAL);
            window(&page, 3, 3, 2, Some(5));
            assert!(page.get("has_more_matches").flag());
            assert_eq!(page.get("matches_seen").number(), 6);
            for (hit, expected) in page.get("hits").array().iter().zip([(3, 6), (8, 11)]) {
                assert_eq!(range(hit), expected);
                assert_eq!(hit.get("path").get("hex").text(), "632e7273");
                assert_eq!(hit.get("original_hex").text(), "616e61");
            }
            let (exit, tail) = invoke(&search_args(&f, index,
                &[option, value, "--skip-matches", "5", "--limit", "2"]), || false);
            assert_eq!(exit, EXIT_PARTIAL, "earlier rows were intentionally omitted");
            window(&tail, 5, 5, 1, None);
            assert_eq!(range(&tail.get("hits").array()[0]), (10, 13));
            assert!(tail.get("workspace_complete").flag());
            assert!(tail.get("truncated").flag());
            assert!(!tail.get("has_more_matches").flag());
        }
    }
}

#[test]
fn all_index_layouts_reach_beyond_the_old_per_response_hit_limit() {
    let body = "target ".repeat(4100);
    let f = Fixture::new(&[entry(b"a.rs", body.as_bytes())], true);
    let indexes = indexes(&f);
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        let (exit, page) = invoke(&search_args(&f, index,
            &["--text", "target", "--skip-matches", "4096", "--limit", "2"]), || false);
        assert_eq!(exit, EXIT_PARTIAL);
        window(&page, 4096, 4096, 2, Some(4098));
        assert_eq!(range(&page.get("hits").array()[0]), (4096 * 7, 4096 * 7 + 6));
        assert_eq!(range(&page.get("hits").array()[1]), (4097 * 7, 4097 * 7 + 6));
        assert_eq!(page.get("max_matches").number(), 2);
        assert!(page.get("has_more_matches").flag());
        if index.is_some() { assert!(page.get("index_used").flag()); }
    }
}

#[test]
fn lowercase_index_fallback_keeps_offset_and_original_utf16_ranges() {
    let little = utf16("BaNaNa", false); let big = utf16("BANANA", true);
    let f = Fixture::new(&[entry(b"a.rs", &little), entry(b"b.rs", &big)], true);
    let indexes = indexes(&f);
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        let (exit, page) = invoke(&search_args(&f, index,
            &["--text", "ana", "--ignore-case", "--skip-matches", "2", "--limit", "1"]), || false);
        assert_eq!(exit, EXIT_PARTIAL);
        window(&page, 2, 2, 1, Some(3));
        let hit = &page.get("hits").array()[0];
        assert_eq!(range(hit), (4, 10));
        assert_eq!(hit.get("matched_text").text(), "ANA");
        assert_eq!(hit.get("original_hex").text(), "0041004e0041");
        assert!(!page.get("index_used").flag());
        if let Some(index) = index {
            assert_eq!(page.get("index_bypass_reason").text(), "incompatible-normalization");
            if index.paged { assert_eq!(page.get("index_page_loads").number(), 0); }
        }
    }
}

#[test]
fn count_only_and_full_width_empty_tails_preserve_global_totals_without_loops() {
    let f = Fixture::new(&[entry(b"a.rs", b"banana banana")], true);
    let indexes = indexes(&f);
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        let (exit, count) = invoke(&search_args(&f, index,
            &["--text", "ana", "--skip-matches", "1", "--limit", "0", "--count-all"]), || false);
        assert_eq!(exit, EXIT_OK); window(&count, 1, 1, 0, None);
        assert_eq!(count.get("matches_seen").number(), 4);
        assert!(count.get("matches_seen_exact").flag()); assert!(count.get("has_more_matches").flag());
        for counted in [false, true] {
            let mut options = vec!["--text", "ana", "--skip-matches", "18446744073709551615", "--limit", "2"];
            if counted { options.push("--count-all"); }
            let (exit, tail) = invoke(&search_args(&f, index, &options), || false);
            assert_eq!(exit, if counted { EXIT_OK } else { EXIT_PARTIAL });
            window(&tail, u64::MAX, 4, 0, None);
            assert!(!tail.get("has_more_matches").flag()); assert!(tail.get("workspace_complete").flag());
        }
        let (exit, absent) = invoke(&search_args(&f, index,
            &["--text", "absent", "--skip-matches", "18446744073709551615"]), || false);
        assert_eq!(exit, EXIT_NO_MATCH); window(&absent, u64::MAX, 0, 0, None);
        assert!(absent.get("workspace_complete").flag());
        let (exit, probe) = invoke(&search_args(&f, index,
            &["--text", "ana", "--skip-matches", "1", "--limit", "0"]), || false);
        // The legacy noncounting literal zero-cap route deliberately does no
        // matching work; it cannot claim it skipped or observed a match.
        assert_eq!(exit, EXIT_PARTIAL); window(&probe, 1, 0, 0, None);
        assert!(!probe.get("has_more_matches").flag());
        assert_eq!(probe.get("matches_seen").number(), 0);
        assert!(!probe.get("workspace_complete").flag());
    }
}

#[test]
fn expression_offsets_consume_only_qualified_primaries_and_keep_work_limits() {
    let f = Fixture::new(&[entry(b"a.rs", b"ana required"),
        entry(b"b.rs", b"ana ana required forbidden"), entry(b"c.rs", b"ana ana required"),
        entry(b"d.md", b"ana required")], true);
    let expression = "ana required -forbidden lang:rust";
    let (exit, page) = invoke(&search_args(&f, None,
        &["--query", expression, "--skip-matches", "1", "--limit", "1"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); window(&page, 1, 1, 1, Some(2));
    assert_eq!(range(&page.get("hits").array()[0]), (0, 3));
    assert_eq!(page.get("hits").array()[0].get("path").get("hex").text(), "632e7273");
    assert_eq!(page.get("predicate_rejections").number(), 1);
    assert_eq!(page.get("matches_seen").number(), 3);
    let (exit, counted) = invoke(&search_args(&f, None,
        &["--query", expression, "--skip-matches", "2", "--limit", "1", "--count-all"]), || false);
    assert_eq!(exit, EXIT_OK); window(&counted, 2, 2, 1, None);
    assert_eq!(range(&counted.get("hits").array()[0]), (4, 7));
    assert!(counted.get("matches_seen_exact").flag());
    let (exit, only_count) = invoke(&search_args(&f, None,
        &["--query", expression, "--skip-matches", "1", "--limit", "0", "--count-all"]), || false);
    assert_eq!(exit, EXIT_OK); window(&only_count, 1, 1, 0, None);
    assert!(only_count.get("has_more_matches").flag());
    let (exit, limited) = invoke(&search_args(&f, None,
        &["--query", expression, "--skip-matches", "2", "--max-scan-bytes", "0"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); window(&limited, 2, 0, 0, None);
    assert!(limited.get("work_limited").flag()); assert!(!limited.get("workspace_complete").flag());
}

#[test]
fn structural_occurrences_reach_later_tokens_inside_the_same_saved_member() {
    let body = "target ".repeat(4100);
    let f = Fixture::new(&[entry(b"a.rs", body.as_bytes())], true);
    let (exit, page) = invoke(&structure_args(&f, "occurrences", "target",
        &["--skip-matches", "4096", "--limit", "2"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); window(&page, 4096, 4096, 2, Some(4098));
    assert_eq!(range(&page.get("hits").array()[0]), (4096 * 7, 4096 * 7 + 6));
    let (exit, tail) = invoke(&structure_args(&f, "occurrences", "target",
        &["--skip-matches", "4098", "--limit", "2"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); window(&tail, 4098, 4098, 2, None);
    assert!(tail.get("workspace_complete").flag()); assert!(tail.get("truncated").flag());
    let (exit, probe) = invoke(&structure_args(&f, "occurrences", "target",
        &["--skip-matches", "4096", "--limit", "0"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); window(&probe, 4096, 4096, 0, None);
    assert!(probe.get("has_more_matches").flag());
}

#[test]
fn declarations_and_headings_use_the_same_bounded_window_contract() {
    let code = Fixture::new(&[entry(b"a.rs", b"fn target_a() {}\nfn target_b() {}\nfn target_c() {}\n")], true);
    let (exit, symbols) = invoke(&structure_args(&code, "symbols", "target",
        &["--name-mode", "prefix", "--skip-matches", "1", "--limit", "1"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); window(&symbols, 1, 1, 1, Some(2));
    assert_eq!(symbols.get("hits").array()[0].get("name").text(), "target_b");
    let docs = Fixture::new(&[entry(b"guide.md", b"# Target A\n\n# Target B\n\n# Target C\n")], true);
    let (exit, headings) = invoke(&structure_args(&docs, "headings", "Target",
        &["--name-mode", "prefix", "--skip-matches", "1", "--limit", "1"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); window(&headings, 1, 1, 1, Some(2));
    assert_eq!(headings.get("hits").array()[0].get("heading_slug").text(), "target-b");
}

#[test]
fn incomplete_archives_cannot_turn_an_empty_tail_into_an_exhaustive_negative() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle"),
        SnapshotEntry { path: b"b.rs", observed_bytes: 6, data: SnapshotData::Unavailable("not-captured") }], false);
    for options in [vec!["--text", "needle"], vec!["--query", "needle"]] {
        let mut options = options;
        options.extend(["--skip-matches", "10", "--count-all"]);
        let (exit, result) = invoke(&search_args(&f, None, &options), || false);
        assert_eq!(exit, EXIT_PARTIAL); window(&result, 10, 1, 0, None);
        assert!(!result.get("workspace_complete").flag()); assert!(!result.get("matches_seen_exact").flag());
    }
    let (exit, result) = invoke(&structure_args(&f, "occurrences", "needle",
        &["--skip-matches", "10"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); window(&result, 10, 1, 0, None);
    assert!(!result.get("workspace_complete").flag());
}

#[test]
fn invalid_offsets_are_rejected_before_archive_access_and_cancellation_publishes_no_page() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle needle")], true);
    for value in ["-1", "01", "18446744073709551616", "nope"] {
        let mut args = search_args(&f, None, &["--text", "needle", "--skip-matches", value]);
        args[2] = f.root.join("missing.fcbs").into_os_string();
        let (exit, result) = invoke(&args, || false);
        assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("status").text(), "error");
    }
    let (exit, canceled) = invoke(&search_args(&f, None,
        &["--text", "needle", "--skip-matches", "1"]), || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(canceled.get("status").text(), "error");
    if let Json::Object(fields) = canceled { assert!(!fields.contains_key("hits")); }
}

#[test]
fn actual_binary_and_human_output_expose_later_offline_matches() {
    let f = Fixture::new(&[entry(b"never-created/a.rs", b"banana banana")], true);
    let args = search_args(&f, None, &["--text", "ana", "--skip-matches", "1", "--limit", "1"]);
    let output = Command::new(env!("CARGO_BIN_EXE_fcb")).args(&args).output().unwrap();
    assert_eq!(output.status.code(), Some(EXIT_PARTIAL as i32)); assert!(output.stderr.is_empty());
    let result = parse(&output.stdout).unwrap(); window(&result, 1, 1, 1, Some(2));
    assert_eq!(range(&result.get("hits").array()[0]), (3, 6));
    let human: Vec<_> = args.into_iter().filter(|arg| arg != "--json").collect();
    let mut out = Vec::new(); let mut err = Vec::new();
    assert_eq!(run(&human, &mut NoInput, &mut out, &mut err, || false), EXIT_PARTIAL);
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Result offset: 1")); assert!(text.contains("--skip-matches 2"));
    assert!(err.is_empty());
}

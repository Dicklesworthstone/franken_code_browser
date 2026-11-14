#![forbid(unsafe_code)]
#![cfg(unix)]

//! Exercise the real one-shot CLI against captured archives. Counts describe
//! qualified primary occurrences, independently of the retained row allowance.
mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{entry, Fixture};
use support::{parse, Json};
use std::{ffi::OsString, fs, io};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};

fn args(f: &Fixture, expression: &str, tail: &[&str]) -> Vec<OsString> {
    let mut args = vec!["snapshot".into(), "search".into(), f.path.as_os_str().to_owned(),
        "--query".into(), expression.into()];
    args.extend(tail.iter().map(OsString::from)); args
}
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, String) {
    let mut out = Vec::new(); let mut errors = Vec::new();
    let exit = run(args, &mut io::empty(), &mut out, &mut errors, canceled);
    assert!(errors.is_empty(), "{}", String::from_utf8_lossy(&errors));
    (exit, String::from_utf8(out).unwrap())
}
fn search(f: &Fixture, expression: &str, tail: &[&str]) -> (u8, Json) {
    let mut args = args(f, expression, tail); args.push("--json".into());
    let (exit, out) = invoke(&args, || false);
    (exit, parse(out.as_bytes()).unwrap())
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn counted_predicates_cover_later_members_and_preserve_original_utf16_ranges() {
    for little in [false, true] {
        let wide = utf16("needle required needle needle", little);
        let f = Fixture::new(&[
            entry(b"docs/a.rs", b"needle required"),
            entry(b"src/a.rs", b"needle needle required"),
            entry(b"src/b.rs", &wide),
            entry(b"src/c.rs", b"needle needle required forbidden"),
            entry(b"src/d.py", b"needle required"),
            entry(b"src/\xff.rs", b"required needle"),
        ], true);
        let expression = "needle required -forbidden path:src/ lang:rust";
        let (exit, counted) = search(&f, expression, &["--count-all", "--limit", "2"]);
        assert_eq!(exit, EXIT_OK);
        assert_eq!(counted.get("matches_seen").number(), 6);
        assert_eq!(counted.get("matching_files").number(), 3);
        assert_eq!(counted.get("stored_hits").number(), 2);
        assert_eq!(counted.get("retained_matching_files").number(), 1);
        assert_eq!(counted.get("metadata_excluded_files").number(), 2);
        assert!(counted.get("count_all_matches").flag());
        assert!(counted.get("matches_seen_exact").flag());
        assert!(counted.get("match_count_complete").flag());
        assert!(counted.get("workspace_complete").flag());
        assert!(counted.get("truncated").flag());
        assert!(!counted.get("work_limited").flag());
        assert!(!counted.get("live_roots_accessed").flag());
        let (exit, full) = search(&f, expression, &["--count-all", "--limit", "10"]);
        assert_eq!(exit, EXIT_OK); assert!(!full.get("truncated").flag());
        let hits = full.get("hits").array(); assert_eq!(hits.len(), 6);
        assert_eq!(hits[2].get("original_range").get("start").number(), 2);
        assert_eq!(hits[2].get("original_range").get("end").number(), 14);
        assert_eq!(hits[5].get("path").get("hex").text(), "7372632fff2e7273");
        assert_eq!(fs::read(&f.path).unwrap(), f.bytes);
    }
}

#[test]
fn count_only_distinguishes_exact_totals_absence_and_legacy_lookahead() {
    let f = Fixture::new(&[entry(b"a.rs", b"banana banana required"),
        entry(b"b.rs", b"banana required")], true);
    let (exit, counted) = search(&f, "ana required", &["--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(counted.get("matches_seen").number(), 6);
    assert_eq!(counted.get("stored_hits").number(), 0); assert!(counted.get("hits").array().is_empty());
    assert!(counted.get("match_count_complete").flag()); assert!(counted.get("truncated").flag());
    let (exit, absent) = search(&f, "absent required", &["--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_NO_MATCH); assert_eq!(absent.get("matches_seen").number(), 0);
    assert!(absent.get("match_count_complete").flag()); assert!(!absent.get("truncated").flag());
    let (exit, bounded) = search(&f, "ana required", &["--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(bounded.get("stored_hits").number(), 1);
    assert!(!bounded.get("count_all_matches").flag()); assert!(!bounded.get("matches_seen_exact").flag());
    assert!(bounded.get("truncated").flag());
}

#[test]
fn negative_predicates_must_finish_before_primary_occurrences_can_count() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle needle required forbidden"),
        entry(b"b.rs", b"needle required")], true);
    let (exit, counted) = search(&f, "needle required -forbidden", &["--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(counted.get("matches_seen").number(), 1);
    assert_eq!(counted.get("predicate_rejections").number(), 1);
    for allowance in ["0", "4"] {
        let (exit, partial) = search(&f, "needle -forbidden", &["--count-all", "--limit", "0", "--max-scan-bytes", allowance]);
        assert_eq!(exit, EXIT_PARTIAL); assert!(partial.get("work_limited").flag());
        assert_eq!(partial.get("matches_seen").number(), 0);
        assert!(!partial.get("match_count_complete").flag());
        assert!(partial.get("scanned_input_bytes").number() <= allowance.parse::<u64>().unwrap());
    }
}

#[test]
fn unavailable_unsupported_and_incomplete_discovery_keep_counts_partial() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle needle"), entry(b"b.rs", &[0xff]),
        SnapshotEntry { path: b"c.rs", observed_bytes: 100, data: SnapshotData::Unavailable("READ_DENIED") }], true);
    let (exit, partial) = search(&f, "needle", &["--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(partial.get("matches_seen").number(), 2);
    assert_eq!(partial.get("unavailable_files_count").number(), 1);
    assert_eq!(partial.get("unsupported_text_files_count").number(), 1);
    assert!(!partial.get("matches_seen_exact").flag());
    let incomplete = Fixture::new(&[entry(b"a.rs", b"needle")], false);
    let (exit, absent) = search(&incomplete, "absent", &["--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(absent.get("matches_seen").number(), 0);
    assert!(!absent.get("match_count_complete").flag());
}

#[test]
fn cancellation_and_invalid_modes_do_not_publish_success_or_change_the_archive() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle required")], true);
    let args = args(&f, "needle required", &["--count-all", "--limit", "0", "--json"]);
    let (exit, out) = invoke(&args, || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(parse(out.as_bytes()).unwrap().get("status").text(), "error");
    for tail in [vec!["--count-all", "--ignore-case", "--ignore-case"], vec!["--count-all", "--count-all"],
        vec!["--count-all", "--text", "needle"]] {
        assert_eq!(search(&f, "needle", &tail).0, EXIT_ERROR);
    }
    assert_eq!(fs::read(&f.path).unwrap(), f.bytes);
}

#[test]
fn human_count_only_output_reports_totals_without_inventing_rows() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle needle required")], true);
    let (exit, out) = invoke(&args(&f, "needle required", &["--count-all", "--limit", "0"]), || false);
    assert_eq!(exit, EXIT_OK);
    assert!(out.contains("Primary matches seen: 2 (exact); retained hits: 0; matching files: 1."));
    assert!(!out.contains(" bytes 0.."));
}

#[test]
fn lowercase_expression_cli_applies_one_policy_and_preserves_source_byte_ranges() {
    let little = utf16("KELVIN REQUIRED", true);
    let big = utf16("KELVIN REQUIRED", false);
    let f = Fixture::new(&[entry(b"src/a.rs", "KELVIN REQUIRED".as_bytes()),
        entry(b"src/b.rs", &little), entry(b"src/c.rs", &big),
        entry(b"src/d.rs", b"KELVIN REQUIRED FoRbIdDeN"),
        entry(b"docs/a.rs", b"KELVIN REQUIRED")], true);
    let expression = "kelvin required -forbidden path:src/ lang:rust";
    let (exit, exact) = search(&f, expression, &[]);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(exact.get("case_sensitive").flag());
    let (exit, lower) = search(&f, expression, &["--ignore-case", "--count-all"]);
    assert_eq!(exit, EXIT_OK); assert!(!lower.get("case_sensitive").flag());
    assert_eq!(lower.get("mode").text(), "unicode-lowercase-expression");
    assert_eq!(lower.get("normalization").text(), "exact");
    assert_eq!(lower.get("primary_needle").text(), "kelvin");
    assert_eq!(lower.get("matches_seen").number(), 3);
    assert_eq!(lower.get("predicate_rejections").number(), 1);
    assert_eq!(lower.get("metadata_excluded_files").number(), 1);
    assert!(lower.get("matches_seen_exact").flag());
    assert!(!lower.get("live_roots_accessed").flag());
    for (hit, start, end) in lower.get("hits").array().iter().zip([0, 2, 2]).zip([8, 14, 14])
        .map(|((hit, start), end)| (hit, start, end)) {
        assert_eq!(hit.get("original_range").get("start").number(), start);
        assert_eq!(hit.get("original_range").get("end").number(), end);
        assert_eq!(*hit.get("matched_text"), Json::Null,
            "metadata-only results must not invent the source spelling from the needle");
    }
    let (exit, counted) = search(&f, expression, &["--ignore-case", "--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(counted.get("matches_seen").number(), 3);
    assert!(counted.get("match_count_complete").flag()); assert!(counted.get("hits").array().is_empty());
    assert_eq!(fs::read(&f.path).unwrap(), f.bytes);
}

#[test]
fn lowercase_cli_keeps_expansion_witnesses_and_incomplete_counts_honest() {
    let f = Fixture::new(&[entry(b"a.rs", "İ REQUIRED".as_bytes())], true);
    let (exit, found) = search(&f, "i required", &["--ignore-case"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(found.get("matches_seen").number(), 1);
    let range = found.get("hits").array()[0].get("original_range");
    assert_eq!(range.get("start").number(), 0); assert_eq!(range.get("end").number(), 2);
    let (exit, partial) = search(&f, "i -forbidden",
        &["--ignore-case", "--count-all", "--limit", "0", "--max-scan-bytes", "3"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!partial.get("match_count_complete").flag());
    assert_eq!(partial.get("matches_seen").number(), 0);
    assert!(partial.get("scanned_input_bytes").number() <= 3);
    let incomplete = Fixture::new(&[entry(b"a.rs", "İ REQUIRED".as_bytes()), SnapshotEntry {
        path: b"b.rs", observed_bytes: 100, data: SnapshotData::Unavailable("READ_DENIED") }], true);
    let (exit, partial) = search(&incomplete, "i required", &["--ignore-case", "--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(partial.get("matches_seen").number(), 1);
    assert!(!partial.get("matches_seen_exact").flag());
    assert_eq!(partial.get("unavailable_files_count").number(), 1);
}

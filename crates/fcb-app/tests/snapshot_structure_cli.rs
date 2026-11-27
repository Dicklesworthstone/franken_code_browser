#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use std::{ffi::OsString, fs, io::{self, Read}, process::Command};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use saved_fixture::{entry, Fixture};
use support::{parse, Json};

struct NoInput;
impl Read for NoInput {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("saved structural search must not consume stdin") }
}
fn args(f: &Fixture, mode: &str, name: &str, extra: &[&str]) -> Vec<OsString> {
    let mut args = vec!["snapshot".into(), "structure".into(), f.path.clone().into_os_string(),
        "--mode".into(), mode.into(), "--name".into(), name.into(), "--json".into()];
    args.extend(extra.iter().map(OsString::from)); args
}
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut output = Vec::new(); let mut errors = Vec::new();
    let exit = run(args, &mut NoInput, &mut output, &mut errors, canceled);
    assert!(errors.is_empty(), "{}", String::from_utf8_lossy(&errors));
    let result = parse(&output).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output)));
    assert_eq!(result.get("schema").text(), "fcb.cli/1");
    (exit, result)
}
fn with_catalog(mut args: Vec<OsString>, f: &Fixture, pin: &str) -> Vec<OsString> {
    args.extend(["--catalog".into(), f.root.join("saved.fcbc").into_os_string(),
        "--catalog-digest".into(), pin.into()]); args
}

#[test]
fn count_all_structure_reports_exact_totals_with_bounded_rows_and_stable_continuations() {
    for (mode, first, second, source) in [
        ("symbols", b"a.rs".as_slice(), b"b.rs".as_slice(), b"fn needle() {}\nfn needle() {}\nfn needle() {}\n".as_slice()),
        ("headings", b"a.md".as_slice(), b"b.md".as_slice(), b"# needle\n\n## needle\n\n### needle\n".as_slice()),
        ("occurrences", b"a.txt".as_slice(), b"b.txt".as_slice(), b"needle needle needle\n".as_slice()),
    ] {
        let f = Fixture::new(&[entry(first, source), entry(second, source)], true);
        let (exit, page) = invoke(&args(&f, mode, "needle", &["--limit", "1", "--skip-matches", "1", "--count-all"]), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(page.get("state").text(), "complete");
        assert!(page.get("count_all_matches").flag()); assert!(page.get("case_sensitive").flag());
        assert_eq!(page.get("name_case").text(), "sensitive");
        assert!(page.get("workspace_complete").flag()); assert!(page.get("matches_seen_exact").flag());
        assert_eq!(page.get("matches_seen").number(), 6); assert_eq!(page.get("retained_hits").number(), 1);
        assert!(page.get("truncated").flag()); assert!(page.get("has_more_matches").flag());
        assert_eq!(page.get("next_match_offset").number(), 2);
        let (exit, last) = invoke(&args(&f, mode, "needle", &["--limit", "1", "--skip-matches", "5", "--count-all"]), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(last.get("matches_seen").number(), 6);
        assert!(last.get("truncated").flag(), "CLI truncation includes an omitted prefix");
        assert!(!last.get("has_more_matches").flag()); assert_eq!(last.get("next_match_offset"), &Json::Null);
        let (exit, counted) = invoke(&args(&f, mode, "needle", &["--limit", "0", "--count-all"]), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(counted.get("matches_seen").number(), 6);
        assert!(counted.get("matches_seen_exact").flag()); assert!(counted.get("truncated").flag());
        assert_eq!(counted.get("retained_hits").number(), 0); assert!(counted.get("hits").array().is_empty());
        assert!(!counted.get("has_more_matches").flag()); assert_eq!(counted.get("next_match_offset"), &Json::Null);
        let (exit, beyond) = invoke(&args(&f, mode, "needle", &["--count-all", "--skip-matches", "18446744073709551615"]), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(beyond.get("matches_seen").number(), 6);
        assert_eq!(beyond.get("skipped_matches").number(), 6); assert!(beyond.get("hits").array().is_empty());
        assert!(!beyond.get("has_more_matches").flag()); assert_eq!(beyond.get("next_match_offset"), &Json::Null);
        let (exit, absent) = invoke(&args(&f, mode, "absent", &["--limit", "0", "--count-all"]), || false);
        assert_eq!(exit, EXIT_NO_MATCH); assert!(absent.get("matches_seen_exact").flag());
        assert_eq!(absent.get("matches_seen").number(), 0); assert!(!absent.get("truncated").flag());
    }
}

#[test]
fn count_all_exit_uses_exact_totals_when_only_result_metadata_is_omitted() {
    let long_name = format!("needle_{}", "x".repeat(4096));
    let text = format!("fn {long_name}() {{}}\nfn needle_a() {{}}\nfn needle_b() {{}}\n");
    let f = Fixture::new(&[entry(b"a.rs", text.as_bytes())], true);
    for (counted, expected_exit) in [(false, EXIT_PARTIAL), (true, EXIT_OK)] {
        let mut extra = vec!["--name-mode", "prefix", "--limit", "1"];
        if counted { extra.push("--count-all"); }
        let (exit, page) = invoke(&args(&f, "symbols", "needle_", &extra), || false);
        assert_eq!(exit, expected_exit);
        assert_eq!(page.get("matches_seen_exact").flag(), counted);
        assert_eq!(page.get("matches_seen").number(), 3);
        assert_eq!(page.get("metadata_omitted").number(), 1);
        assert!(!page.get("workspace_complete").flag());
        assert_eq!(page.get("retained_hits").number(), 1);
        assert_eq!(page.get("hits").array()[0].get("name").text(), "needle_a");
        assert_eq!(page.get("next_match_offset").number(), 2);
        assert!(!page.get("semantic_complete").flag());
    }
}

#[test]
fn insensitive_structure_keeps_original_names_and_utf16_source_bytes() {
    let text = "// 🚀\nfn Run() {}\nfn Runner() {}\nfn preRun() {}\n";
    let mut bytes = vec![0xff, 0xfe]; bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    let f = Fixture::new(&[entry(b"saved.rs", &bytes)], true);
    for (mode, count) in [("exact", 1), ("prefix", 2), ("contains", 3)] {
        let (exit, result) = invoke(&args(&f, "symbols", "rUN", &["--name-mode", mode, "--ignore-case", "--count-all"]), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(result.get("matches_seen").number(), count);
        assert_eq!(result.get("name_case").text(), "unicode-lowercase");
        assert!(!result.get("case_sensitive").flag()); assert!(result.get("count_all_matches").flag());
        assert!(result.get("matches_seen_exact").flag()); assert!(!result.get("live_roots_accessed").flag());
        for hit in result.get("hits").array() {
            let name = hit.get("name").text();
            let expected: Vec<_> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let range = hit.get("original_range");
            assert_eq!(&bytes[range.get("start").number() as usize..range.get("end").number() as usize], expected);
        }
    }
    let (exit, default) = invoke(&args(&f, "symbols", "run", &[]), || false);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(default.get("case_sensitive").flag());
    assert!(!default.get("count_all_matches").flag()); assert_eq!(default.get("name_case").text(), "sensitive");
    let headings = Fixture::new(&[entry(b"guide.md", "# İDEA\n\n# Straße\n\n# École\n".as_bytes())], true);
    for (mode, name, expected) in [("exact", "i\u{307}dea", 1), ("prefix", "i", 1),
        ("contains", "\u{307}d", 1), ("exact", "STRASSE", 0), ("exact", "e\u{301}cole", 0)] {
        let (exit, result) = invoke(&args(&headings, "headings", name,
            &["--name-mode", mode, "--ignore-case", "--count-all"]), || false);
        assert_eq!(exit, if expected == 0 { EXIT_NO_MATCH } else { EXIT_OK });
        assert_eq!(result.get("matches_seen").number(), expected); assert!(result.get("matches_seen_exact").flag());
        if expected > 0 { assert_eq!(result.get("hits").array()[0].get("name").text(), "İDEA"); }
    }
}

#[test]
fn count_all_never_upgrades_incomplete_coverage_and_occurrence_prefixes_fail_before_io() {
    let f = Fixture::new(&[entry(b"a.txt", b"needle"), entry(b"b.txt", b"needle")], true);
    let (exit, work) = invoke(&args(&f, "occurrences", "needle", &["--limit", "0", "--count-all", "--max-source-bytes", "6"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(work.get("state").text(), "work-limit");
    assert_eq!(work.get("matches_seen").number(), 1); assert!(!work.get("matches_seen_exact").flag());
    assert!(!work.get("workspace_complete").flag()); assert_eq!(work.get("next_match_offset"), &Json::Null);
    let partial = Fixture::new(&[entry(b"a.rs", b"fn needle() {}\n"),
        SnapshotEntry { path: b"b.rs", observed_bytes: 1, data: SnapshotData::Unavailable("missing") }], false);
    let (exit, result) = invoke(&args(&partial, "symbols", "needle", &["--limit", "0", "--count-all"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(result.get("matches_seen").number(), 1);
    assert!(!result.get("matches_seen_exact").flag()); assert_eq!(result.get("unavailable_files").number(), 1);
    let mut bad = args(&f, "occurrences", "needle", &["--name-mode", "prefix", "--ignore-case", "--count-all"]);
    bad[2] = f.root.join("missing.fcbs").into_os_string();
    let (exit, result) = invoke(&bad, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("error").get("code").text(), "CLI_INCOMPATIBLE_OPTIONS");
}

#[test]
fn saved_declarations_and_headings_share_name_modes_and_original_ranges() {
    let code = b"fn target() {}\nfn target_extra() {}\n";
    let f = Fixture::new(&[entry(b"src/source.rs", code)], true);
    for (matching, needle, count) in [("exact", "target", 1), ("prefix", "target", 2), ("contains", "arg", 2)] {
        let (exit, result) = invoke(&args(&f, "symbols", needle, &["--name-mode", matching]), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(result.get("command").text(), "snapshot-structure");
        assert_eq!(result.get("requested_mode").text(), "symbols");
        assert_eq!(result.get("realized_mode").text(), "heuristic-outline-candidate");
        assert_eq!(result.get("retained_hits").number(), count);
        assert!(result.get("workspace_complete").flag()); assert!(!result.get("semantic_complete").flag());
        assert!(!result.get("live_roots_accessed").flag()); assert_eq!(result.get("retained_source_payload_bytes").number(), 0);
        let hit = &result.get("hits").array()[0];
        let start = hit.get("original_range").get("start").number() as usize;
        let end = hit.get("original_range").get("end").number() as usize;
        assert_eq!(&code[start..end], b"target");
        assert!(hit.get("evidence_range").get("start").number() <= start as u64);
        assert!(hit.get("evidence_range").get("end").number() >= end as u64);
    }
    let document = b"# Target\n\n## Target Extra\n";
    let headings = Fixture::new(&[entry(b"docs/guide.md", document)], true);
    for (matching, count) in [("exact", 1), ("prefix", 2), ("contains", 2)] {
        let (exit, result) = invoke(&args(&headings, "headings", "Target", &["--name-mode", matching]), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(result.get("retained_hits").number(), count);
        assert_eq!(result.get("evidence_level").text(), "fmd-heading");
        let hit = &result.get("hits").array()[0];
        assert_eq!(hit.get("heading_slug").text(), "target");
        assert_eq!(hit.get("kind").text(), "heading"); assert_eq!(hit.get("language").text(), "markdown");
        assert_eq!(hit.get("original_range").get("start").number(), 0);
        let end = hit.get("original_range").get("end").number() as usize;
        assert!(document[..end].starts_with(b"# Target"));
    }
}

#[test]
fn utf16_occurrences_keep_byte_identity_and_include_comments_without_prefix_matches() {
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend("needle needle_suffix xneedle\r\n// needle\r\n".encode_utf16().flat_map(u16::to_le_bytes));
    let f = Fixture::new(&[entry(b"raw-\xff.rs", &bytes)], true);
    let (exit, result) = invoke(&args(&f, "occurrences", "needle", &[]), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("retained_hits").number(), 2);
    assert!(result.get("includes_comments_and_literals").flag()); assert!(!result.get("compiler_resolved").flag());
    assert_eq!(result.get("evidence_level").text(), "whole-token-text-candidate");
    assert_eq!(result.get("per_file_source_limit").number(), 32 * 1024 * 1024);
    let hits = result.get("hits").array();
    assert_eq!(hits[0].get("path").get("hex").text(), "7261772dff2e7273");
    assert_eq!(hits[0].get("original_range").get("start").number(), 2);
    assert_eq!(hits[0].get("original_range").get("end").number(), 14);
    assert_eq!(hits[1].get("hit_id").number(), 2);
    assert!(hits[1].get("original_range").get("start").number() > 14);
    for hit in hits {
        let start = hit.get("original_range").get("start").number() as usize;
        let end = hit.get("original_range").get("end").number() as usize;
        assert_eq!(&bytes[start..end], b"n\0e\0e\0d\0l\0e\0");
    }
}

#[test]
fn zero_capacity_work_limits_and_incomplete_coverage_never_report_exhaustive_negatives() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle"), entry(b"b.rs", b"needle")], true);
    let (exit, exact) = invoke(&args(&f, "occurrences", "needle", &["--limit", "2"]), || false);
    assert_eq!(exit, EXIT_OK); assert!(!exact.get("truncated").flag());
    let (exit, limited) = invoke(&args(&f, "occurrences", "needle", &["--limit", "1"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(limited.get("truncated").flag());
    assert_eq!(limited.get("retained_hits").number(), 1); assert_eq!(limited.get("matches_seen").number(), 2);
    let (exit, probe) = invoke(&args(&f, "occurrences", "needle", &["--limit", "0"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(probe.get("retained_hits").number(), 0);
    assert_eq!(probe.get("matches_seen").number(), 1); assert!(!probe.get("matches_seen_exact").flag());
    let (exit, absent) = invoke(&args(&f, "occurrences", "absent", &["--limit", "0"]), || false);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(absent.get("workspace_complete").flag());
    let (exit, work) = invoke(&args(&f, "occurrences", "absent", &["--max-source-bytes", "0"]), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(work.get("state").text(), "work-limit");
    assert_eq!(work.get("loaded_source_bytes").number(), 0); assert!(!work.get("matches_seen_exact").flag());
    let oversized = vec![b'x'; fcb::search::MAX_SYMBOL_SOURCE_BYTES + 1];
    let partial = Fixture::with_limits(&[entry(b"a.rs", &oversized), entry(b"b.bin", b"unsupported"),
        SnapshotEntry { path: b"c.rs", observed_bytes: 5, data: SnapshotData::Unavailable("not-captured") }], false,
        fcb::search::snapshot::SnapshotLimits { max_file_bytes: 2 * 1024 * 1024, ..Default::default() });
    let (exit, omitted) = invoke(&args(&partial, "symbols", "absent", &[]), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!omitted.get("discovery_complete").flag());
    assert_eq!(omitted.get("source_limited_files").number(), 1);
    assert_eq!(omitted.get("unsupported_files").number(), 1); assert_eq!(omitted.get("unavailable_files").number(), 1);
    assert!(!omitted.get("workspace_complete").flag()); assert!(!omitted.get("matches_seen_exact").flag());
}

#[test]
fn trusted_catalog_search_verifies_loaded_members_and_preserves_unread_integrity_boundary() {
    let code = b"fn target() {}\n"; let ignored = vec![b'z'; 4096];
    let f = Fixture::new(&[entry(b"a.rs", code), entry(b"z.bin", &ignored)], true);
    let catalog_args = vec!["snapshot".into(), "catalog".into(), f.path.clone().into_os_string(),
        "--output".into(), f.root.join("saved.fcbc").into_os_string(), "--json".into()];
    let (exit, built) = invoke(&catalog_args, || false); assert_eq!(exit, EXIT_OK);
    let pin = built.get("catalog_digest").text();
    let mut damaged = f.bytes.clone();
    let at = damaged.windows(ignored.len()).position(|window| window == ignored.as_slice()).unwrap();
    damaged[at] ^= 1; fs::write(&f.path, &damaged).unwrap();
    let (exit, skipped) = invoke(&with_catalog(args(&f, "symbols", "target", &[]), &f, pin), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(skipped.get("retained_hits").number(), 1);
    assert_eq!(skipped.get("unsupported_files").number(), 1);
    assert_eq!(skipped.get("archive_validation_bytes").number(), 56);
    assert_eq!(skipped.get("archive_open_validation").text(), "trusted-catalog-and-boundaries");
    assert!(!skipped.get("archive_body_verified_on_open").flag());
    assert_eq!(skipped.get("member_payload_bytes_loaded").number(), code.len() as u64);
    assert_eq!(invoke(&args(&f, "symbols", "target", &[]), || false).0, EXIT_ERROR);
    let at = damaged.windows(code.len()).position(|window| window == code).unwrap();
    damaged[at] ^= 1; fs::write(&f.path, &damaged).unwrap();
    let (exit, refused) = invoke(&with_catalog(args(&f, "symbols", "target", &[]), &f, pin), || false);
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(refused.get("error").get("code").text(), "SNAPSHOT_MEMBER_CHANGED");
}

#[test]
fn malformed_names_are_rejected_before_archive_access_and_cancel_never_publishes_rows() {
    let f = Fixture::new(&[entry(b"a.rs", b"fn target() {}\n")], true);
    let mut bad = args(&f, "occurrences", "private::target", &[]);
    bad[2] = f.root.join("missing.fcbs").into_os_string();
    let (exit, failure) = invoke(&bad, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(failure.get("error").get("code").text(), "REFERENCE_INVALID_NAME");
    let (exit, canceled) = invoke(&args(&f, "symbols", "target", &[]), || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(canceled.get("status").text(), "error");
    let mut output = Vec::new(); let mut errors = Vec::new();
    let mut human = args(&f, "symbols", "target", &[]); human.retain(|arg| arg != "--json");
    assert_eq!(run(&human, &mut NoInput, &mut output, &mut errors, || false), EXIT_OK);
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("heuristic-outline-candidate") && text.contains("source bytes loaded:"));
    assert!(errors.is_empty());
}

#[test]
fn actual_binary_searches_saved_headings_without_a_live_source_root() {
    let f = Fixture::new(&[entry(b"never-created/guide.md", b"# Offline Guide\n")], true);
    let output = Command::new(env!("CARGO_BIN_EXE_fcb"))
        .args(args(&f, "headings", "Offline", &["--name-mode", "prefix"])).output().unwrap();
    assert_eq!(output.status.code(), Some(EXIT_OK as i32)); assert!(output.stderr.is_empty());
    let result = parse(&output.stdout).unwrap();
    assert_eq!(result.get("retained_hits").number(), 1);
    assert_eq!(result.get("hits").array()[0].get("heading_slug").text(), "offline-guide");
    assert!(!result.get("live_roots_accessed").flag());
}


#[test]
fn saved_java_and_csharp_symbols_keep_capture_ranges_and_unsupported_context_coverage() {
    let java = b"class Browser { void open() {} }\n";
    let csharp: Vec<_> = "\u{feff}namespace Sample; class Browser { void open() {} }\n"
        .encode_utf16().flat_map(u16::to_le_bytes).collect();
    let f = Fixture::new(&[entry(b"src/Browser.JAVA", java), entry(b"src/Browser.CS", &csharp)], true);
    let (exit, result) = invoke(&args(&f, "symbols", "open", &["--name-mode", "exact"]), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("retained_hits").number(), 2);
    assert!(result.get("workspace_complete").flag()); assert!(!result.get("semantic_complete").flag());
    assert!(!result.get("live_roots_accessed").flag());
    let hits = result.get("hits").array();
    for (language, original, expected) in [("java", java.as_slice(), b"open".to_vec()),
        ("csharp", csharp.as_slice(), "open".encode_utf16().flat_map(u16::to_le_bytes).collect())] {
        let hit = hits.iter().find(|h| h.get("language").text() == language).unwrap();
        assert_eq!(hit.get("kind").text(), "method");
        let range = hit.get("original_range");
        assert_eq!(&original[range.get("start").number() as usize..range.get("end").number() as usize], expected);
    }
    let refused = Fixture::new(&[entry(b"Escaped.java", br"// \u000a class Hidden {}")], true);
    let (exit, result) = invoke(&args(&refused, "symbols", "Hidden", &[]), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(result.get("unsupported_files").number(), 1);
    assert!(!result.get("workspace_complete").flag()); assert_eq!(result.get("retained_hits").number(), 0);
}

#[test]
fn lowercase_occurrence_cli_keeps_original_encoded_names_in_counted_later_windows() {
    for little in [None, Some(true), Some(false)] {
        let text = "K K k Kx _K 1K K\u{307}";
        let bytes = match little {
            None => text.as_bytes().to_vec(),
            Some(little) => format!("\u{feff}{text}").encode_utf16()
                .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect(),
        };
        let f = Fixture::new(&[entry(b"a.txt", &bytes), entry(b"b.txt", &bytes)], true);
        let (exit, page) = invoke(&args(&f, "occurrences", "k",
            &["--ignore-case", "--count-all", "--limit", "1", "--skip-matches", "4"]), || false);
        assert_eq!(exit, EXIT_OK);
        assert!(!page.get("case_sensitive").flag()); assert_eq!(page.get("name_case").text(), "unicode-lowercase");
        assert_eq!(page.get("name").text(), "k"); assert_eq!(page.get("name_mode").text(), "exact");
        assert!(page.get("workspace_complete").flag()); assert!(page.get("matches_seen_exact").flag());
        assert_eq!(page.get("matches_seen").number(), 6); assert_eq!(page.get("retained_hits").number(), 1);
        assert_eq!(page.get("skipped_matches").number(), 4);
        assert_eq!(page.get("next_match_offset").number(), 5);
        let hit = &page.get("hits").array()[0];
        assert_eq!(hit.get("name").text(), "K"); assert_eq!(hit.get("member").number(), 1);
        let expected: Vec<_> = match little {
            None => "K".as_bytes().to_vec(),
            Some(little) => "K".encode_utf16()
                .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect(),
        };
        let range = hit.get("original_range");
        assert_eq!(&bytes[range.get("start").number() as usize..range.get("end").number() as usize], expected);
        assert!(!page.get("live_roots_accessed").flag());

        let (exit, count) = invoke(&args(&f, "occurrences", "k",
            &["--ignore-case", "--count-all", "--limit", "0"]), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(count.get("matches_seen").number(), 6);
        assert_eq!(count.get("retained_hits").number(), 0);
        assert!(count.get("matches_seen_exact").flag()); assert!(!count.get("has_more_matches").flag());
        assert_eq!(count.get("next_match_offset"), &Json::Null);
        let (exit, page) = invoke(&args(&f, "occurrences", "k",
            &["--ignore-case", "--limit", "1", "--skip-matches", "1"]), || false);
        assert_eq!(exit, EXIT_PARTIAL); assert_eq!(page.get("matches_seen").number(), 3);
        assert_eq!(page.get("hits").array()[0].get("name").text(), "K");
        assert_eq!(page.get("next_match_offset").number(), 2);
        assert!(!page.get("matches_seen_exact").flag());
        let (exit, legacy) = invoke(&args(&f, "occurrences", "K", &[]), || false);
        assert_eq!(exit, EXIT_OK); assert!(legacy.get("case_sensitive").flag());
        assert_eq!(legacy.get("matches_seen").number(), 2);
        assert!(legacy.get("hits").array().iter().all(|hit| hit.get("name").text() == "K"));
    }
}

#[test]
fn lowercase_occurrence_cli_rejects_partial_expansions_and_preserves_wider_source_spelling() {
    let text = "İ i\u{307} I\u{307} i I ß ẞ ss SS ΟΣ οσ ος É é e\u{301}";
    let f = Fixture::new(&[entry(b"tokens.txt", text.as_bytes())], true);
    for (query, expected) in [("i", vec!["i", "I"]),
        ("i\u{307}", vec!["İ", "i\u{307}", "I\u{307}"]),
        ("ß", vec!["ß", "ẞ"]), ("ss", vec!["ss", "SS"]),
        ("οσ", vec!["ΟΣ", "οσ"]), ("ος", vec!["ος"]), ("é", vec!["É", "é"])] {
        let (exit, report) = invoke(&args(&f, "occurrences", query, &["--ignore-case", "--count-all"]), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(report.get("matches_seen").number(), expected.len() as u64);
        assert_eq!(report.get("hits").array().iter().map(|hit| hit.get("name").text()).collect::<Vec<_>>(), expected);
        assert!(report.get("matches_seen_exact").flag());
    }
    let needle = "K".repeat(1024); let spelling = "K".repeat(1024);
    let wide = Fixture::new(&[entry(b"wide.txt", spelling.as_bytes())], true);
    let (exit, report) = invoke(&args(&wide, "occurrences", &needle,
        &["--ignore-case", "--count-all"]), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(report.get("name").text(), needle);
    assert_eq!(report.get("hits").array()[0].get("name").text(), spelling);
    assert_eq!(report.get("metadata_omitted").number(), 0);
    assert_eq!(report.get("metadata_bytes").number(), spelling.len() as u64);
}

#[test]
fn exact_and_folder_scopes_preserve_raw_labels_and_only_consume_selected_source() {
    let source = b"fn needle() {}\n";
    let f = Fixture::new(&[entry(b"a.rs", &[b'x'; 200]), entry(b"src-extra.rs", source),
        entry(b"src/nested/\xff.rs", source), entry(b"src/main.rs", source),
        entry(b"src2/decoy.rs", source), entry(b"src\\stray.rs", source)], true);
    let (exit, folder) = invoke(&args(&f, "symbols", "needle",
        &["--directory-hex", "737263", "--max-source-bytes", "30", "--limit", "1", "--skip-matches", "1", "--count-all"]), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(folder.get("scope").text(), "saved-directory");
    assert_eq!(folder.get("scope_path").get("hex").text(), "737263");
    assert!(folder.get("scope_complete").flag()); assert!(!folder.get("workspace_complete").flag());
    assert_eq!(folder.get("scope_known_members").number(), 2); assert_eq!(folder.get("matches_seen").number(), 2);
    assert_eq!(folder.get("members_visited").number(), 2); assert_eq!(folder.get("loaded_source_bytes").number(), 30);
    assert_eq!(folder.get("hits").array()[0].get("path").get("hex").text(), "7372632f6e65737465642fff2e7273");
    let (exit, member) = invoke(&args(&f, "symbols", "needle",
        &["--member-hex", "7372632f6e65737465642fff2e7273", "--max-source-bytes", "15"]), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(member.get("scope").text(), "saved-member");
    assert_eq!(member.get("scope_path"), member.get("hits").array()[0].get("path"));
    assert_eq!(member.get("members_visited").number(), 1);
    let (exit, absent) = invoke(&args(&f, "symbols", "needle", &["--directory", "absent"]), || false);
    assert_eq!(exit, EXIT_NO_MATCH); assert_eq!(absent.get("scope_known_members").number(), 0);
    assert!(absent.get("scope_complete").flag());
}

#[test]
fn partial_discovery_closes_only_an_observed_singleton_and_unavailable_members_remain_partial() {
    let f = Fixture::new(&[entry(b"src/a.md", b"# Heading\n"),
        SnapshotEntry { path: b"other.md", observed_bytes: 1, data: SnapshotData::Unavailable("missing") }], false);
    for (flag, path, exit, membership, unavailable) in [
        ("--member", "src/a.md", EXIT_OK, true, 0),
        ("--member", "other.md", EXIT_PARTIAL, true, 1),
        ("--member", "unknown.md", EXIT_PARTIAL, false, 0),
        ("--directory", "src", EXIT_PARTIAL, false, 0),
        ("--directory", "missing", EXIT_PARTIAL, false, 0),
    ] {
        let (observed, page) = invoke(&args(&f, "headings", "Heading", &[flag, path, "--count-all"]), || false);
        assert_eq!(observed, exit, "{flag} {path}");
        assert_eq!(page.get("scope_membership_complete").flag(), membership);
        assert_eq!(page.get("unavailable_files").number(), unavailable);
        assert_eq!(page.get("matches_seen_exact").flag(), observed == EXIT_OK);
        assert!(!page.get("discovery_complete").flag()); assert!(!page.get("workspace_complete").flag());
    }
    for extra in [vec!["--member", "../src/a.md"], vec!["--directory", "/src"],
        vec!["--directory", "a//b"], vec!["--member-hex", "610062"], vec!["--directory", "src", "--member", "src/a.md"]] {
        let mut input = args(&f, "headings", "Heading", &extra);
        input[2] = f.root.join("unopened.fcbs").into_os_string();
        let (exit, result) = invoke(&input, || false);
        assert_eq!(exit, EXIT_ERROR);
        assert!(matches!(result.get("error").get("code").text(), "STRUCTURE_INVALID_SCOPE" | "CLI_INCOMPATIBLE_OPTIONS"));
    }
}

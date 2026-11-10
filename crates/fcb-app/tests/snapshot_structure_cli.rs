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
    assert_eq!(result.get("per_file_source_limit").number(), 512 * 1024);
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
    let oversized = vec![b'x'; 64 * 1024 + 1];
    let partial = Fixture::new(&[entry(b"a.rs", &oversized), entry(b"b.bin", b"unsupported"),
        SnapshotEntry { path: b"c.rs", observed_bytes: 5, data: SnapshotData::Unavailable("not-captured") }], false);
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

#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Real directory -> public application -> bounded renderer-neutral atlas.
//! No native rendering is inferred from JSON geometry or these portable tests.
use std::{ffi::OsString, fs, io::{self, Read, Write}, path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};

struct NoStdin;
impl Read for NoStdin {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("atlas must never consume stdin") }
}
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("fcb-atlas-cli-{}-{now}-{id}", std::process::id()));
    fs::create_dir(&root).unwrap(); root
}
fn arguments(root: &Path, options: &[&str]) -> Vec<OsString> {
    let mut argv = vec![OsString::from("atlas"), root.as_os_str().to_owned()];
    argv.extend(options.iter().map(OsString::from)); argv
}
fn run(argv: &[OsString]) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = fcb_app::run(argv, &mut NoStdin, &mut out, &mut err, || false);
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}
fn fixture() -> PathBuf {
    let root = root();
    fs::create_dir(root.join("src")).unwrap(); fs::create_dir(root.join("docs")).unwrap();
    fs::create_dir(root.join("target")).unwrap(); fs::create_dir(root.join("empty")).unwrap();
    fs::write(root.join("src/a.rs"), b"PAYLOAD_MUST_NOT_BE_READ_OR_EMITTED").unwrap();
    fs::write(root.join("src/b.rs"), b"second file").unwrap();
    fs::write(root.join("docs/guide.md"), b"documentation content").unwrap();
    fs::write(root.join("target/build.bin"), b"excluded").unwrap(); root
}

#[test]
fn real_catalog_produces_finite_clipped_geometry_without_source_or_native_claims() {
    let root = fixture();
    let (code, out, err) = run(&arguments(&root, &["--json", "--detail-pixels", "0.001"]));
    assert_eq!(code, EXIT_OK, "{out} {err}"); assert!(err.is_empty());
    assert!(out.starts_with("{\"schema\":\"fcb.cli/1\"")); assert!(out.ends_with("}\n"));
    for field in ["\"command\":\"atlas\"", "\"atlas_schema\":\"fcb.atlas/1\"",
        "\"native_presented\":false", "\"payload_bytes_read\":\"0\"",
        "\"known_files\":\"3\"", "\"hierarchy_nodes\":\"6\"",
        "\"discovery_complete\":true", "\"plan_complete\":true", "\"detail_limited\":false"] {
        assert!(out.contains(field), "missing {field}: {out}");
    }
    assert!(!out.contains("PAYLOAD_MUST_NOT_BE_READ_OR_EMITTED"));
    assert!(!out.contains("build.bin")); assert!(!out.contains("\"display\":\"empty\""));
    assert_eq!(out.matches("\"detail\":\"file\"").count(), 3);
    for record in out.split("\"logical_rect\":{").skip(1) {
        let values: Vec<f64> = record.split('}').next().unwrap().split(',')
            .map(|part| part.split(':').nth(1).unwrap().parse::<f64>().unwrap()).collect();
        assert_eq!(values.len(), 4); assert!(values.iter().all(|n| n.is_finite()));
        assert!(values[0] >= 0.0 && values[1] >= 0.0 && values[2] > 0.0 && values[3] > 0.0);
        assert!(values[0] + values[2] <= 1024.0 + 1e-8);
        assert!(values[1] + values[3] <= 768.0 + 1e-8);
    }
}

#[test]
fn directory_focus_and_off_focus_selection_do_not_turn_into_other_file_hits() {
    let root = fixture();
    let (code, out, err) = run(&arguments(&root, &["--json", "--focus", "src", "--select", "docs/guide.md",
        "--detail-pixels", "0.001"]));
    assert_eq!(code, EXIT_OK, "{out} {err}");
    let selection = out.split("\"selection\":").nth(1).unwrap().split("\"parcels\":").next().unwrap();
    assert!(selection.contains("docs/guide.md")); assert!(selection.contains("\"logical_rect\":null"));
    let parcels = out.split("\"parcels\":").nth(1).unwrap();
    assert!(!parcels.contains("docs/guide.md")); assert_eq!(parcels.matches("\"detail\":\"file\"").count(), 2);
    assert!(parcels.contains("src/a.rs") && parcels.contains("src/b.rs"));
}

#[test]
fn aggregation_preserves_selection_without_fabricating_file_id_for_directory() {
    let root = fixture();
    let (code, out, err) = run(&arguments(&root, &["--json", "--limit", "1", "--select", "src/a.rs",
        "--detail-pixels", "0.001"]));
    assert_eq!(code, EXIT_PARTIAL, "{out} {err}");
    assert!(out.contains("\"detail_limited\":true"));
    let selection = out.split("\"selection\":").nth(1).unwrap().split("\"parcels\":").next().unwrap();
    assert!(selection.contains("src/a.rs")); assert!(!selection.contains("\"file_id\":null"));
    let parcels = out.split("\"parcels\":").nth(1).unwrap().split("\"traversal\":").next().unwrap();
    assert_eq!(parcels.matches("\"logical_rect\":").count(), 1);
    assert!(parcels.contains("\"file_id\":null")); assert!(!parcels.contains("\"detail\":\"file\""));
    let (code, out, _) = run(&arguments(&root, &["--json", "--max-visits", "1"]));
    assert_eq!(code, EXIT_PARTIAL); assert!(out.contains("\"visited_nodes\":\"1\""));
}

#[test]
fn empty_viewport_is_not_an_empty_repository_and_selection_remains_identified() {
    let root = fixture();
    let (code, out, err) = run(&arguments(&root, &["--json", "--pan-x", "1000000", "--select", "src/a.rs"]));
    assert_eq!(code, EXIT_OK, "{out} {err}");
    assert!(out.contains("\"parcels\":[]")); assert!(out.contains("\"known_files\":\"3\""));
    assert!(out.contains("\"logical_rect\":null")); assert!(out.contains("src/a.rs"));
}

#[test]
fn incomplete_discovery_and_empty_catalog_remain_distinct() {
    let root = fixture();
    let (code, out, _) = run(&arguments(&root, &["--json", "--max-files", "1"]));
    assert_eq!(code, EXIT_PARTIAL); assert!(out.contains("\"discovery_complete\":false"));
    assert!(out.contains("\"known_files\":\"1\""));
    let (code, out, err) = run(&arguments(&root.join("empty"), &["--json"]));
    assert_eq!(code, EXIT_OK, "{out} {err}"); assert!(out.contains("\"known_files\":\"0\""));
    assert!(out.contains("\"hierarchy_nodes\":\"1\"")); assert!(!out.contains("\"detail\":\"file\""));
}

#[test]
fn rule_files_require_the_explicit_permission_and_never_become_source_payload() {
    let root = root(); fs::write(root.join(".gitignore"), b"hidden.rs\n").unwrap();
    fs::write(root.join("hidden.rs"), b"hidden payload").unwrap(); fs::write(root.join("visible.rs"), b"visible payload").unwrap();
    let (code, default, err) = run(&arguments(&root, &["--json", "--detail-pixels", "0.001"]));
    assert_eq!(code, EXIT_OK, "{default} {err}"); assert!(default.contains("\"display\":\"hidden.rs\""));
    let (code, rules, err) = run(&arguments(&root, &["--json", "--respect-ignores", "--detail-pixels", "0.001"]));
    assert_eq!(code, EXIT_OK, "{rules} {err}");
    let parcels = rules.split("\"parcels\":").nth(1).unwrap();
    assert!(!parcels.contains("\"display\":\"hidden.rs\"")); assert!(parcels.contains("visible.rs"));
    assert!(rules.contains("\"payload_bytes_read\":\"0\"")); assert!(!rules.contains("hidden payload"));
}

#[test]
#[cfg(target_os = "linux")] // Filesystem fixture with arbitrary non-UTF-8 bytes.
fn native_filename_payload_survives_without_control_sequence_output() {
    use std::os::unix::ffi::OsStringExt;
    let root = root(); let name = OsString::from_vec(b"raw-\xff-\x1b.rs".to_vec());
    fs::write(root.join(&name), b"unused").unwrap();
    let mut argv = arguments(&root, &["--json", "--select"]); argv.push(name.clone());
    let (code, out, err) = run(&argv); assert_eq!(code, EXIT_OK, "{out} {err}");
    assert!(out.contains("7261772dff2d1b2e7273")); assert!(!out.contains('\u{1b}')); assert!(!out.contains('\u{fffd}'));
    let mut argv = arguments(&root, &["--select"]); argv.push(name);
    let (code, human, err) = run(&argv); assert_eq!(code, EXIT_OK, "{human} {err}");
    assert!(!human.contains('\u{1b}')); assert!(!human.contains('\u{fffd}'));
}

#[test]
fn invalid_focus_unavailable_sources_and_symlink_roots_have_no_success_geometry() {
    let root = fixture();
    for focus in ["../outside", "/absolute"] {
        let (code, out, _) = run(&arguments(&root, &["--json", "--focus", focus]));
        assert_eq!(code, EXIT_ERROR); assert!(!out.contains("\"parcels\":")); assert!(out.contains("CLI_INVALID_RANGE"));
    }
    let (code, out, _) = run(&arguments(&root, &["--json", "--focus", "not-there"]));
    assert_eq!(code, EXIT_ERROR); assert!(out.contains("CLI_ATLAS_FOCUS_NOT_CATALOGUED"));
    let (code, out, _) = run(&arguments(&root, &["--json", "--select", "src"]));
    assert_eq!(code, EXIT_ERROR); assert!(!out.contains("\"parcels\":"));
    let link = root.join("alias"); std::os::unix::fs::symlink(root.join("src"), &link).unwrap();
    let (code, out, _) = run(&arguments(&link, &["--json"]));
    assert_eq!(code, EXIT_ERROR); assert!(out.contains("CLI_SYMLINK_REFUSED"));
}

#[test]
fn cancellation_and_partial_writes_never_append_a_second_document() {
    let root = fixture(); let argv = arguments(&root, &["--json"]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(fcb_app::run(&argv, &mut NoStdin, &mut out, &mut err, || true), EXIT_CANCELED);
    let response = String::from_utf8(out).unwrap();
    assert!(response.contains("CLI_CANCELED")); assert_eq!(response.matches("\"schema\"").count(), 1);
    assert!(!response.contains("\"parcels\":"));
    struct Broken(Vec<u8>);
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0.len() == 11 { return Err(io::ErrorKind::BrokenPipe.into()); }
            let n = bytes.len().min(11 - self.0.len()); self.0.extend_from_slice(&bytes[..n]); Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> { panic!("flush belongs to host") }
    }
    let mut out = Broken(Vec::new()); let mut err = Vec::new();
    assert_eq!(fcb_app::run(&argv, &mut NoStdin, &mut out, &mut err, || false), EXIT_ERROR);
    assert_eq!(out.0.len(), 11); assert_eq!(&out.0, b"{\"schema\":\"");
    assert!(String::from_utf8(err).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
}

#[test]
fn help_is_available_without_any_root_or_native_startup() {
    let (code, out, err) = run(&[OsString::from("atlas"), OsString::from("--help"), OsString::from("--json")]);
    assert_eq!(code, EXIT_OK, "{out} {err}"); assert!(out.contains("atlas-help")); assert!(out.contains("--focus"));
}

#[test]
fn path_matches_use_the_same_files_and_never_repack_the_underlying_geometry() {
    let root = fixture();
    fs::write(root.join("src/parser.rs"), b"UNREAD_QUERY_PAYLOAD").unwrap();
    fs::write(root.join("docs/parser.md"), b"UNREAD_QUERY_PAYLOAD").unwrap();
    let base = run(&arguments(&root, &["--json", "--detail-pixels", "0.001"]));
    assert_eq!(base.0, EXIT_OK, "{} {}", base.1, base.2);
    let (code, out, err) = run(&arguments(&root, &["--json", "--detail-pixels", "0.001", "--path", "parser"]));
    assert_eq!(code, EXIT_OK, "{out} {err}");
    let geometry = |text: &str| -> Vec<String> {
        text.split("],\"traversal\":").next().unwrap().split("\"logical_rect\":{")
            .skip(1).map(|part| part.split('}').next().unwrap().to_owned()).collect()
    };
    assert_eq!(geometry(&base.1), geometry(&out));
    let hits = out.split("\"path_search\":").nth(1).unwrap();
    assert!(hits.contains("\"scan_complete\":true")); assert!(hits.contains("\"truncated\":false"));
    assert!(hits.contains("\"matches_seen\":\"2\"")); assert!(hits.contains("\"retained_matches\":\"2\""));
    assert!(hits.contains("src/parser.rs") && hits.contains("docs/parser.md"));
    assert!(!out.contains("UNREAD_QUERY_PAYLOAD")); assert!(out.contains("\"payload_bytes_read\":\"0\""));
    assert_eq!(out.matches("\"retained_path_matches\":\"1\"").count(), 2);
}

#[test]
fn top_k_match_rows_and_off_focus_matches_preserve_coverage_and_identity() {
    let root = fixture();
    let (code, out, err) = run(&arguments(&root, &["--json", "--path", ".rs", "--match-limit", "1"]));
    assert_eq!(code, EXIT_PARTIAL, "{out} {err}");
    let hits = out.split("\"path_search\":").nth(1).unwrap();
    assert!(hits.contains("\"scan_complete\":true")); assert!(hits.contains("\"truncated\":true"));
    assert!(hits.contains("\"matches_seen\":\"2\"")); assert!(hits.contains("\"retained_matches\":\"1\""));
    let (code, out, err) = run(&arguments(&root, &["--json", "--focus", "docs", "--path", ".rs"]));
    assert_eq!(code, EXIT_OK, "{out} {err}");
    let hits = out.split("\"path_search\":").nth(1).unwrap();
    assert!(hits.contains("src/a.rs") && hits.contains("src/b.rs"));
    assert_eq!(hits.matches("\"logical_rect\":null").count(), 2);
    let (code, out, _) = run(&arguments(&root, &["--json", "--path", ".rs", "--match-limit", "0"]));
    assert_eq!(code, EXIT_PARTIAL); assert!(out.contains("\"retained_matches\":\"0\""));
    assert!(out.contains("\"matches_seen\":\"2\""));
    let (code, out, _) = run(&arguments(&root, &["--json", "--path", "never-exists", "--max-files", "1"]));
    assert_eq!(code, EXIT_PARTIAL); assert!(out.contains("\"scan_complete\":false"));
    assert!(out.contains("\"matches_seen\":\"0\""));
}

#[test]
fn path_overlay_options_preserve_literal_values_and_reject_unpaired_limits() {
    let root = fixture();
    let (code, out, err) = run(&arguments(&root, &["--path", "--json"]));
    assert_eq!(code, EXIT_OK, "{out} {err}"); assert!(!out.starts_with('{'));
    for options in [vec!["--json", "--match-limit", "1"], vec!["--json", "--path", ""],
        vec!["--json", "--path", "a", "--path", "b"],
        vec!["--json", "--path", "a", "--match-limit", "4097"]] {
        let (code, out, _) = run(&arguments(&root, &options));
        assert_eq!(code, EXIT_ERROR); assert!(!out.contains("\"parcels\":"));
    }
}

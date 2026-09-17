#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Exercise the public CLI entry point against real directories and exact bytes.
use std::{ffi::OsString, fs, io::{self, Read, Write}, path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};

struct NoStdin;
impl Read for NoStdin { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("atlas cannot consume implicit stdin") } }
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-atlas-text-cli-{}-{now}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn arguments(root: &Path, tail: &[&str]) -> Vec<OsString> {
    let mut args = vec![OsString::from("atlas"), root.as_os_str().to_owned()];
    args.extend(tail.iter().map(OsString::from)); args
}
fn call(root: &Path, tail: &[&str]) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&arguments(root, tail), &mut NoStdin, &mut out, &mut err, || false);
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}
fn text_report(json: &str) -> &str {
    json.split_once(",\"text_search\":").expect("text-search field").1
}

#[test]
fn cli_maps_exact_source_hits_and_counts_without_modifying_repository_bytes() {
    let root = root(); fs::write(root.join("a.rs"), b"needle needle").unwrap(); fs::write(root.join("b.rs"), b"needle").unwrap();
    let (code, output, error) = call(&root, &["--text", "needle", "--detail-pixels", "0.001", "--json"]);
    assert_eq!(code, EXIT_OK, "{output} {error}"); assert!(error.is_empty());
    assert!(output.contains("\"native_presented\":false"));
    assert!(output.contains("\"payload_bytes_read\":\"19\""));
    assert!(output.contains("\"retained_text_matches\":{\"occurrences\":\"2\",\"files\":\"1\"}"));
    let report = text_report(&output);
    assert!(report.contains("\"workspace_complete\":true")); assert!(report.contains("\"counts_complete\":true"));
    assert!(report.contains("\"retained_matches\":\"3\""));
    assert_eq!(report.matches("\"matched_text\":\"needle\"").count(), 3);
    assert_eq!(report.matches("\"original_hex\":\"6e6565646c65\"").count(), 3);
    assert!(output.ends_with("}\n")); assert!(!output.contains("}{"));
    assert_eq!(fs::read(root.join("a.rs")).unwrap(), b"needle needle");
}

#[test]
fn text_search_does_not_conflate_focus_or_detail_admission_with_query_scope() {
    let root = root(); fs::create_dir(root.join("src")).unwrap(); fs::create_dir(root.join("other")).unwrap();
    fs::write(root.join("src/a.rs"), b"needle").unwrap(); fs::write(root.join("other/b.rs"), b"needle").unwrap();
    let (code, output, error) = call(&root, &["--text", "needle", "--focus", "src", "--json"]);
    assert_eq!(code, EXIT_OK, "{output} {error}");
    let report = text_report(&output);
    assert!(report.contains("\"retained_matches\":\"2\""));
    assert!(report.contains("\"scope\":\"catalogued-workspace-captures-not-camera-focus\""));
    assert!(report.contains("\"logical_rect\":null"));
    let (code, output, _) = call(&root, &["--text", "needle", "--limit", "1", "--json"]);
    assert_eq!(code, EXIT_PARTIAL);
    assert!(output.contains("\"detail_limited\":true"));
    assert!(text_report(&output).contains("\"workspace_complete\":true"));
}

#[test]
fn utf16_is_text_not_a_utf8_byte_prefilter_and_retains_original_ranges() {
    let root = root(); let mut bytes = vec![0xff, 0xfe];
    for unit in "head\r\nneedle😀\r\n".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(root.join("wide.txt"), &bytes).unwrap();
    let (code, output, error) = call(&root, &["--text", "needle", "--json"]);
    assert_eq!(code, EXIT_OK, "{output} {error}");
    let report = text_report(&output);
    assert!(report.contains("\"original_range\":{\"start\":\"14\",\"end\":\"26\"}"));
    assert!(report.contains("\"original_hex\":\"6e006500650064006c006500\""));
    assert!(report.contains("\"fallback_scans\":\"1\""));
    assert_eq!(fs::read(root.join("wide.txt")).unwrap(), bytes);
}

#[test]
fn file_total_verification_and_hit_limits_are_not_reported_as_absence() {
    let root = root(); fs::write(root.join("a.rs"), b"needle needle").unwrap();
    let (code, output, _) = call(&root, &["--text", "needle", "--match-limit", "1", "--json"]);
    assert_eq!(code, EXIT_PARTIAL);
    assert!(text_report(&output).contains("\"truncated\":true"));
    assert!(text_report(&output).contains("\"retained_matches\":\"1\""));
    let (code, output, _) = call(&root, &["--text", "needle", "--max-scan-bytes", "2", "--json"]);
    assert_eq!(code, EXIT_PARTIAL); assert!(text_report(&output).contains("\"byte_limited\":true"));
    for (option, reason) in [("--max-file-bytes", "WORKSPACE_FILE_BYTE_LIMIT"), ("--max-total-bytes", "WORKSPACE_TOTAL_SOURCE_LIMIT")] {
        let (code, output, error) = call(&root, &["--text", "needle", option, "4", "--json"]);
        assert_eq!(code, EXIT_PARTIAL, "{output} {error}");
        assert!(text_report(&output).contains(reason));
        assert!(text_report(&output).contains("\"workspace_complete\":false"));
        assert!(output.contains("\"payload_bytes_read\":\"0\""));
    }
    let (code, output, _) = call(&root, &["--text", "needle", "--match-limit", "0", "--json"]);
    assert_eq!(code, EXIT_PARTIAL);
    assert!(text_report(&output).contains("\"retained_matches\":\"0\""));
    assert!(text_report(&output).contains("\"counts_complete\":false"));
}

#[test]
fn metadata_and_filename_routes_still_read_no_source_payload() {
    let root = root(); fs::write(root.join("needle.bin"), [0xff; 8192]).unwrap();
    for tail in [vec!["--json"], vec!["--path", "needle", "--json"]] {
        let (code, output, error) = call(&root, &tail);
        assert_eq!(code, EXIT_OK, "{output} {error}");
        assert!(output.contains("\"payload_bytes_read\":\"0\""));
        assert_eq!(text_report(&output), "null}\n");
    }
    let (code, output, _) = call(&root, &["--text", "absent", "--max-files", "1", "--json"]);
    assert!(matches!(code, EXIT_OK | EXIT_PARTIAL), "{output}");
}

#[test]
#[cfg(target_os = "linux")]
fn non_utf8_source_paths_stay_reversible_in_text_results() {
    use std::os::unix::ffi::OsStringExt;
    let root = root(); let name = OsString::from_vec(b"raw-\xff.rs".to_vec());
    fs::write(root.join(name), b"needle").unwrap();
    let (code, output, error) = call(&root, &["--text", "needle", "--json"]);
    assert_eq!(code, EXIT_OK, "{output} {error}");
    assert!(text_report(&output).contains("\"hex\":\"7261772dff2e7273\""));
    assert!(!output.contains('\u{fffd}'));
}

#[test]
fn malformed_options_fail_before_source_io_and_human_text_escapes_controls() {
    let missing = PathBuf::from("/fcb-deliberately-missing-atlas-text-root");
    for tail in [vec!["--text", "needle", "--path", "a", "--json"],
        vec!["--max-total-bytes", "100", "--json"], vec!["--text", "", "--json"]] {
        let (code, output, _) = call(&missing, &tail);
        assert_eq!(code, EXIT_ERROR); assert!(output.contains("CLI_INCOMPATIBLE_OPTIONS") || output.contains("CLI_INVALID_NEEDLE"));
        assert!(!output.contains("CLI_SOURCE_IO"));
    }
    let root = root(); fs::write(root.join("a.rs"), b"needle\x1b[31m").unwrap();
    let (code, output, error) = call(&root, &["--text", "needle\u{1b}[31m"]);
    assert_eq!(code, EXIT_OK, "{output} {error}"); assert!(!output.contains('\u{1b}'));
    assert!(output.contains("\\u{1b}"));
}

#[test]
fn canceled_or_interrupted_delivery_never_appends_a_second_json_document() {
    let root = root(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let args = arguments(&root, &["--text", "needle", "--json"]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&args, &mut NoStdin, &mut out, &mut err, || true);
    assert_eq!(code, EXIT_CANCELED); assert!(String::from_utf8(out).unwrap().contains("CLI_CANCELED"));
    struct Broken(Vec<u8>);
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.0.is_empty() { return Err(io::ErrorKind::BrokenPipe.into()); }
            let n = bytes.len().min(7); self.0.extend_from_slice(&bytes[..n]); Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> { panic!("caller owns flush") }
    }
    let mut out = Broken(Vec::new()); let mut err = Vec::new();
    assert_eq!(run(&args, &mut NoStdin, &mut out, &mut err, || false), EXIT_ERROR);
    assert_eq!(out.0.len(), 7); assert_eq!(out.0.iter().filter(|&&b| b == b'{').count(), 1);
    assert!(String::from_utf8(err).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
}

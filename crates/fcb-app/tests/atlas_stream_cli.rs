#![forbid(unsafe_code)]
#![cfg(unix)]

//! End-to-end atlas command through production discovery, file streaming,
//! decoder/matcher, spatial projection and bounded output. No native GPU claim.
use std::{ffi::OsString, fs, io::{self, Read, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{EXIT_OK, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};

struct UnusedInput;
impl Read for UnusedInput {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("atlas cannot consume implicit stdin") }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-atlas-stream-cli-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.0.join(path); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
    fn argv(&self, extra: &[&str]) -> Vec<OsString> {
        let mut args = vec![OsString::from("atlas"), self.0.as_os_str().to_owned()];
        args.extend(extra.iter().map(OsString::from)); args
    }
    fn run(&self, extra: &[&str]) -> (u8, String, String) { run(&self.argv(extra), || false) }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn run(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, String, String) {
    let (mut output, mut error) = (Vec::new(), Vec::new());
    let exit = fcb_app::run(args, &mut UnusedInput, &mut output, &mut error, canceled);
    (exit, String::from_utf8(output).unwrap(), String::from_utf8(error).unwrap())
}
fn search(json: &str) -> &str { json.split_once("\"text_search\":").unwrap().1.split_once(",\"text_preview\":").unwrap().0 }
fn camera(json: &str) -> &str { json.split_once("\"camera\":").unwrap().1.split_once(",\"focus\":").unwrap().0 }

#[test]
fn large_file_matches_are_spatial_results_not_capture_limit_refusals() {
    let root = Fixture::new(); let mut source = vec![b'a'; 2 * 1024 * 1024 + 17];
    let start = source.len(); source.extend_from_slice(b"needle"); root.write("large.rs", &source);
    let (old_exit, old, _) = root.run(&["--text", "needle", "--json"]);
    assert_eq!(old_exit, EXIT_PARTIAL); assert!(old.contains("WORKSPACE_FILE_BYTE_LIMIT"));
    let (exit, json, error) = root.run(&["--whole-file", "--text", "needle", "--json"]);
    assert_eq!(exit, EXIT_OK, "{json} {error}"); assert!(error.is_empty());
    let result = search(&json);
    assert!(result.contains("\"strategy\":\"streaming-whole-file\""));
    assert!(result.contains("\"capture_limits_applied\":false,\"captured_bytes\":\"0\""));
    assert!(result.contains(&format!("\"original_range\":{{\"start\":\"{start}\",\"end\":\"{}\"}}", start + 6)));
    assert!(result.contains("\"original_hex\":\"6e6565646c65\""));
    assert!(result.contains("\"retained_witness_bytes\":\"6\""));
    assert!(result.contains(&format!("\"payload_bytes_read\":\"{}\"", source.len())));
    assert_eq!(camera(&json), camera(&old));
    assert!(json.contains("\"text_preview\":null")); assert!(json.contains("\"document_preview\":null"));
    assert!(json.contains("\"native_presented\":false")); assert!(json.ends_with("}\n"));
}

#[test]
fn file_and_directory_overlays_keep_distinct_file_and_occurrence_counts() {
    let root = Fixture::new(); root.write("src/a.rs", b"needle needle"); root.write("src/b.rs", b"needle");
    let (exit, json, _) = root.run(&["--whole-file", "--text", "needle", "--limit", "1", "--json"]);
    assert_eq!(exit, EXIT_PARTIAL, "aggregate detail must remain partial");
    assert!(search(&json).contains("\"workspace_complete\":true"));
    assert!(search(&json).contains("\"retained_matches\":\"3\""));
    assert!(json.contains("\"retained_text_matches\":{\"occurrences\":\"3\",\"files\":\"2\"}"));
}

#[test]
fn focus_does_not_silently_limit_the_search_to_visible_files() {
    let root = Fixture::new(); root.write("src/a.rs", b"unmatched"); root.write("other/b.rs", b"needle");
    let (exit, json, _) = root.run(&["--whole-file", "--text", "needle", "--focus", "src", "--json"]);
    assert_eq!(exit, EXIT_OK, "{json}");
    let hits = search(&json).split_once("\"hits\":[").unwrap().1.split_once("],\"files\":").unwrap().0;
    assert!(hits.contains("other/b.rs")); assert!(hits.contains("\"logical_rect\":null"));
}

#[test]
fn byte_and_call_caps_are_global_and_report_the_unexamined_suffix() {
    let root = Fixture::new(); root.write("a.rs", b"abc"); root.write("b.rs", b"needle"); root.write("c.rs", b"needle");
    let (exit, json, _) = root.run(&["--whole-file", "--text", "needle", "--max-scan-bytes", "4", "--json"]);
    assert_eq!(exit, EXIT_PARTIAL);
    let result = search(&json);
    assert!(result.contains("\"payload_bytes_read\":\"4\"")); assert!(result.contains("\"files_examined\":\"2\""));
    assert!(result.contains("\"unexamined_files\":\"1\"")); assert!(result.contains("\"stop_reason\":\"byte-limit\""));
    assert!(result.contains("\"workspace_complete\":false"));
    let (exit, json, _) = root.run(&["--whole-file", "--text", "needle", "--max-read-calls", "1", "--json"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(search(&json).contains("\"files_examined\":\"1\""));
    assert!(search(&json).contains("\"unexamined_files\":\"2\""));
    assert!(search(&json).contains("\"stop_reason\":\"read-call-limit\""));
}

#[test]
fn zero_io_admission_does_not_open_or_scan_source_files() {
    let root = Fixture::new(); root.write("a.rs", b"needle");
    for limit in ["--max-scan-bytes", "--max-read-calls"] {
        let (exit, json, _) = root.run(&["--whole-file", "--text", "needle", limit, "0", "--json"]);
        assert_eq!(exit, EXIT_PARTIAL); let result = search(&json);
        assert!(result.contains("\"files_examined\":\"0\"")); assert!(result.contains("\"read_calls\":\"0\""));
        assert!(result.contains("\"payload_bytes_read\":\"0\"")); assert!(result.contains("\"hits\":[]"));
    }
}

#[test]
fn match_limit_uses_lookahead_instead_of_declaring_every_full_buffer_truncated() {
    let root = Fixture::new(); root.write("a.rs", b"needle"); root.write("b.rs", b"nothing");
    let args = ["--whole-file", "--text", "needle", "--match-limit", "1", "--json"];
    let (exit, json, _) = root.run(&args); assert_eq!(exit, EXIT_OK, "{json}");
    assert!(search(&json).contains("\"truncated\":false"));
    root.write("b.rs", b"needle");
    let (exit, json, _) = root.run(&args); assert_eq!(exit, EXIT_PARTIAL);
    let result = search(&json); assert!(result.contains("\"matches_counted\":\"2\""));
    assert!(result.contains("\"retained_matches\":\"1\"")); assert!(result.contains("\"stop_reason\":\"match-limit\""));
}

#[test]
fn zero_rows_can_prove_absence_but_do_not_claim_an_exhaustive_positive_count() {
    let root = Fixture::new(); root.write("a.rs", b"nothing");
    let args = ["--whole-file", "--text", "needle", "--match-limit", "0", "--json"];
    let (exit, json, _) = root.run(&args); assert_eq!(exit, EXIT_OK, "{json}");
    assert!(search(&json).contains("\"workspace_complete\":true"));
    root.write("a.rs", b"needle needle");
    let (exit, json, _) = root.run(&args); assert_eq!(exit, EXIT_PARTIAL);
    assert!(search(&json).contains("\"matches_counted\":\"1\""));
    assert!(search(&json).contains("\"counts_complete\":false")); assert!(search(&json).contains("\"hits\":[]"));
}

#[test]
fn both_utf16_byte_orders_keep_native_byte_witnesses_in_json() {
    let root = Fixture::new();
    for (path, little) in [("a.rs", true), ("b.rs", false)] {
        let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in "head needle tail".encode_utf16() {
            bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
        }
        root.write(path, &bytes);
    }
    let (exit, json, _) = root.run(&["--whole-file", "--text", "needle", "--json"]);
    assert_eq!(exit, EXIT_OK, "{json}"); let result = search(&json);
    assert!(result.contains("\"retained_matches\":\"2\""));
    assert!(result.contains("\"original_hex\":\"6e006500650064006c006500\""));
    assert!(result.contains("\"original_hex\":\"006e006500650064006c0065\""));
    assert_eq!(result.matches("\"original_range\":{\"start\":\"12\",\"end\":\"24\"}").count(), 2);
}

#[test]
fn unsupported_text_is_explicit_and_later_files_still_search() {
    let root = Fixture::new(); root.write("a.rs", &[0xff, 0, 0xff]); root.write("b.rs", b"needle");
    let (exit, json, _) = root.run(&["--whole-file", "--text", "needle", "--json"]);
    assert_eq!(exit, EXIT_PARTIAL); let result = search(&json);
    assert!(result.contains("\"state\":\"unsupported-text\""));
    assert!(result.contains("\"retained_matches\":\"1\"")); assert!(result.contains("\"incomplete_files\":\"1\""));
}

#[test]
fn ignore_policy_is_separately_authorized_for_the_streaming_route() {
    let root = Fixture::new(); root.write(".gitignore", b"secret/\n");
    root.write("secret/a.rs", b"needle"); root.write("allowed/b.rs", b"needle");
    let (exit, json, _) = root.run(&["--whole-file", "--text", "needle", "--respect-ignores", "--json"]);
    assert_eq!(exit, EXIT_OK, "{json}"); assert!(search(&json).contains("\"retained_matches\":\"1\""));
    assert!(!search(&json).contains("secret/a.rs"));
}

#[test]
fn incompatible_capture_and_preview_requests_fail_before_root_io() {
    let root = Fixture::new();
    let missing = root.0.join("not-present");
    for extra in [vec!["--whole-file"], vec!["--whole-file", "--path", "needle"],
        vec!["--whole-file", "--text", "needle", "--max-file-bytes", "8"],
        vec!["--whole-file", "--text", "needle", "--max-total-bytes", "8"],
        vec!["--whole-file", "--text", "needle", "--preview-hit", "0"],
        vec!["--whole-file", "--text", "needle", "--markdown-hit", "0"],
        vec!["--text", "needle", "--max-read-calls", "1"]] {
        let mut args = vec![OsString::from("atlas"), missing.as_os_str().to_owned(), OsString::from("--json")];
        args.extend(extra.iter().map(OsString::from));
        let (exit, json, _) = run(&args, || false); assert_eq!(exit, EXIT_ERROR, "{json}");
        assert!(json.contains("CLI_INCOMPATIBLE_OPTIONS"), "{json}"); assert!(!json.contains("CLI_SOURCE_IO"));
    }
}

#[test]
#[cfg(target_os = "linux")]
fn raw_native_filename_bytes_are_preserved_in_streamed_hit_identity() {
    use std::os::unix::ffi::OsStringExt;
    let root = Fixture::new();
    fs::write(root.0.join(OsString::from_vec(b"raw-\xff.rs".to_vec())), b"needle").unwrap();
    let (exit, json, _) = root.run(&["--whole-file", "--text", "needle", "--json"]);
    assert_eq!(exit, EXIT_OK, "{json}");
    assert!(search(&json).contains("\"hex\":\"7261772dff2e7273\"")); assert!(!json.contains('\u{fffd}'));
}

#[test]
fn cancellation_and_broken_delivery_never_append_a_second_json_document() {
    let root = Fixture::new(); root.write("a.rs", b"needle");
    let args = root.argv(&["--whole-file", "--text", "needle", "--json"]);
    let (exit, json, _) = run(&args, || true); assert_eq!(exit, EXIT_CANCELED);
    assert_eq!(json.matches("\"schema\":").count(), 1); assert!(json.contains("\"status\":\"error\""));
    struct Broken(Vec<u8>);
    impl Write for Broken {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            if self.0.len() >= 32 { return Err(io::ErrorKind::BrokenPipe.into()); }
            let n = input.len().min(32 - self.0.len()); self.0.extend_from_slice(&input[..n]); Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> { panic!("not the output owner's flush") }
    }
    let mut output = Broken(Vec::new()); let mut error = Vec::new();
    assert_eq!(fcb_app::run(&args, &mut UnusedInput, &mut output, &mut error, || false), EXIT_ERROR);
    assert_eq!(output.0.len(), 32); assert!(String::from_utf8(error).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
}

#[test]
fn empty_scope_completes_and_human_literal_text_escapes_directional_controls() {
    let root = Fixture::new();
    let (exit, json, _) = root.run(&["--whole-file", "--text", "needle", "--json"]);
    assert_eq!(exit, EXIT_OK, "{json}"); assert!(search(&json).contains("\"workspace_complete\":true"));
    root.write("a.rs", "start\u{202e}end".as_bytes());
    let (exit, human, _) = root.run(&["--whole-file", "--text", "\u{202e}"]);
    assert_eq!(exit, EXIT_OK, "{human}"); assert!(!human.contains('\u{202e}')); assert!(human.contains("\\u{202e}"));
}

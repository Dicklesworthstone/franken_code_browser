#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Public atlas text search and exact captured-context preview composition.
use std::{ffi::OsString, fs, io::{self, Read}, path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};

struct NoStdin;
impl Read for NoStdin { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("no implicit stdin") } }
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-atlas-preview-cli-{}-{now}-{}",
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
fn preview(json: &str) -> &str { json.split_once(",\"text_preview\":").expect("preview must be present").1 }
fn first_decimal<'a>(json: &'a str, field: &str) -> &'a str {
    let marker = format!("\"{field}\":\"");
    json.split_once(&marker).unwrap().1.split_once('"').unwrap().0
}
fn parcels(json: &str) -> &str {
    json.split_once(",\"parcels\":").unwrap().1.split_once(",\"traversal\":").unwrap().0
}

#[test]
fn selecting_a_retained_occurrence_adds_context_without_source_io_or_repacking() {
    let root = root(); let bytes = b"first needle middle needle last";
    fs::write(root.join("a.rs"), bytes).unwrap();
    let (code, base, error) = call(&root, &["--text", "needle", "--json"]);
    assert_eq!(code, EXIT_OK, "{base} {error}");
    assert!(!base.contains("\"text_preview\""));
    let (code, output, error) = call(&root, &["--text", "needle", "--preview-hit", "1", "--json"]);
    assert_eq!(code, EXIT_OK, "{output} {error}"); assert!(error.is_empty());
    assert_eq!(first_decimal(&base, "payload_bytes_read"), "31");
    assert_eq!(first_decimal(&output, "payload_bytes_read"), first_decimal(&base, "payload_bytes_read"));
    assert_eq!(parcels(&output), parcels(&base));
    let context = preview(&output);
    assert!(context.contains("\"additional_source_bytes_read\":\"0\""));
    assert!(context.contains("\"hit_index\":\"1\""));
    assert!(context.contains("\"selected_original_range\":{\"start\":\"20\",\"end\":\"26\"}"));
    assert!(context.contains("\"selected_window_utf8_range\":{\"start\":\"20\",\"end\":\"26\"}"));
    assert!(context.contains("\"text\":\"first needle middle needle last\""));
    assert!(context.contains("\"has_replacements\":false"));
    assert_eq!(fs::read(root.join("a.rs")).unwrap(), bytes);
}

#[test]
fn utf16_preview_distinguishes_source_bytes_from_window_utf8_selection() {
    let logical = "head\r\nneedle😀tail";
    for little in [true, false] {
        let root = root(); let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in logical.encode_utf16() {
            let encoded = if little { unit.to_le_bytes() } else { unit.to_be_bytes() };
            bytes.extend_from_slice(&encoded);
        }
        fs::write(root.join("wide.txt"), &bytes).unwrap();
        let (code, output, error) = call(&root, &["--text", "needle", "--preview-hit", "0", "--json"]);
        assert_eq!(code, EXIT_OK, "{output} {error}");
        let context = preview(&output);
        assert!(context.contains("\"selected_original_range\":{\"start\":\"14\",\"end\":\"26\"}"));
        assert!(context.contains("\"selected_window_utf8_range\":{\"start\":\"6\",\"end\":\"12\"}"));
        assert!(context.contains("\"text\":\"head\\r\\nneedle😀tail\""));
        assert!(context.contains("\"first_line\":\"1\""));
        assert!(context.contains("\"has_replacements\":false"));
        assert_eq!(first_decimal(&output, "payload_bytes_read"), bytes.len().to_string());
        assert_eq!(fs::read(root.join("wide.txt")).unwrap(), bytes);
    }
}

#[test]
fn distant_context_is_bounded_without_fictional_global_line_numbers() {
    let root = root(); let mut original = vec![b'x'; 40 * 1024];
    original.extend_from_slice(b"needle"); original.extend_from_slice(&vec![b'y'; 40 * 1024]);
    fs::write(root.join("large.rs"), &original).unwrap();
    let (code, output, error) = call(&root, &["--text", "needle", "--preview-hit", "0", "--context-bytes", "16", "--json"]);
    assert_eq!(code, EXIT_OK, "{output} {error}");
    let context = preview(&output);
    assert!(context.contains("\"first_line\":null"));
    assert!(context.contains("\"prefix_bytes_omitted\":true"));
    assert!(context.contains("\"suffix_bytes_omitted\":true"));
    assert!(context.contains("\"selected_original_range\":{\"start\":\"40960\",\"end\":\"40966\"}"));
    assert!(context.contains("\"selected_window_utf8_range\":{\"start\":\"20\",\"end\":\"26\"}"));
    assert!(context.contains(&format!("\"text\":\"{}needle{}\"", "x".repeat(20), "y".repeat(20))));
    assert!(!context.contains(&"x".repeat(128))); assert!(!context.contains(&"y".repeat(128)));
    assert!(output.len() < 16 * 1024, "preview must not serialize the captured prefix");
}

#[test]
fn retained_hit_can_be_previewed_without_promoting_a_truncated_query_to_complete() {
    let root = root(); fs::write(root.join("a.rs"), b"needle needle").unwrap();
    let (code, output, error) = call(&root, &["--text", "needle", "--match-limit", "1", "--preview-hit", "0", "--json"]);
    assert_eq!(code, EXIT_PARTIAL, "{output} {error}");
    assert!(output.contains("\"counts_complete\":false")); assert!(output.contains("\"truncated\":true"));
    assert!(preview(&output).contains("\"selected_original_range\":{\"start\":\"0\",\"end\":\"6\"}"));
    assert!(preview(&output).contains("\"text\":\"needle needle\""));
}

#[test]
fn invalid_preview_requests_fail_before_io_or_return_one_exact_missing_hit_error() {
    let missing = Path::new("/fcb-missing-preview-root-for-argument-tests");
    for tail in [vec!["--preview-hit", "0", "--json"],
        vec!["--text", "x", "--context-bytes", "1", "--json"],
        vec!["--text", "x", "--preview-hit", "0", "--match-limit", "0", "--json"],
        vec!["--text", "x", "--preview-hit", "0", "--context-bytes", "16385", "--json"]] {
        let (code, output, _) = call(missing, &tail);
        assert_eq!(code, EXIT_ERROR);
        assert!(output.contains("CLI_INCOMPATIBLE_OPTIONS") || output.contains("CLI_ARGUMENT_LIMIT"));
        assert!(!output.contains("CLI_SOURCE_IO"));
    }
    let root = root(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let (code, output, error) = call(&root, &["--text", "needle", "--preview-hit", "1", "--json"]);
    assert_eq!(code, EXIT_ERROR, "{output} {error}");
    assert!(output.contains("ATLAS_TEXT_MISSING_HIT"));
    assert!(!output.contains("\"text_search\"")); assert!(!output.contains("\"text_preview\""));
    assert_eq!(output.lines().count(), 1);
}

#[test]
fn human_preview_escapes_context_controls_and_cancellation_keeps_one_terminal_response() {
    let root = root(); fs::write(root.join("a.rs"), b"\x1b[31mbefore needle after\x1b[0m").unwrap();
    let (code, output, error) = call(&root, &["--text", "needle", "--preview-hit", "0"]);
    assert_eq!(code, EXIT_OK, "{output} {error}");
    assert!(!output.contains('\u{1b}')); assert!(output.contains("\\u{1b}"));
    assert!(output.contains("before needle after")); assert!(output.contains("no additional file read"));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&arguments(&root, &["--text", "needle", "--preview-hit", "0", "--json"]),
        &mut NoStdin, &mut out, &mut err, || true);
    assert_eq!(code, EXIT_CANCELED);
    let output = String::from_utf8(out).unwrap();
    assert_eq!(output.lines().count(), 1); assert!(output.contains("CLI_CANCELED"));
    assert!(!output.contains("\"text_preview\""));
}

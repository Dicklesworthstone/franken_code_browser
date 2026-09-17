#![forbid(unsafe_code)]
#![cfg(any(
    target_os = "macos",
    all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))
))]
use std::{ffi::OsString, fs, io::{self, Read}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};
struct NoInput;
impl Read for NoInput { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("no implicit input") } }
fn root(source: &[u8]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-atlas-document-{}-{now}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); fs::create_dir(path.join("docs")).unwrap(); fs::create_dir(path.join("src")).unwrap();
    fs::write(path.join("docs/README.md"), source).unwrap(); fs::write(path.join("src/lib.rs"), b"fn untouched() {}\n").unwrap(); path
}
fn invoke(root: &PathBuf, extra: &[&str]) -> (u8, String) {
    let mut args = vec![OsString::from("atlas"), root.as_os_str().to_owned(), OsString::from("--text"), OsString::from("needle"), OsString::from("--json")];
    args.extend(extra.iter().map(OsString::from));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = run(&args, &mut NoInput, &mut out, &mut err, || false);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    (exit, String::from_utf8(out).unwrap())
}
fn parcels(output: &str) -> &str { output.split("\"parcels\":").nth(1).unwrap().split(",\"traversal\":").next().unwrap() }
fn payload(output: &str) -> &str { output.split("\"payload_bytes_read\":\"").nth(1).unwrap().split('"').next().unwrap() }

#[test]
fn exact_search_to_markdown_adds_no_source_reads_and_never_repacks_the_atlas() {
    let bytes = b"# Documentation\n\nRead the **needle** carefully.\n"; let root = root(bytes);
    let (base_exit, base) = invoke(&root, &[]); assert_eq!(base_exit, EXIT_OK, "{base}");
    let (exit, result) = invoke(&root, &["--markdown-hit", "0", "--preview-hit", "0"]);
    assert_eq!(exit, EXIT_OK, "{result}");
    assert_eq!(parcels(&result), parcels(&base)); assert_eq!(payload(&result), payload(&base));
    assert!(result.contains("\"document_preview\":{\"hit_index\":\"0\""));
    assert!(result.contains("\"document_schema\":\"fcb.document/1\""));
    assert!(result.contains("\"text\":\"Read the needle carefully.\""));
    assert!(result.contains("\"selected_original_hex\":\"6e6565646c65\""));
    assert!(result.contains("\"additional_source_bytes_read\":\"0\""));
    assert!(result.contains("\"rendered_hit_highlight\":false"));
    assert_eq!(fs::read(root.join("docs/README.md")).unwrap(), bytes);
}

#[test]
fn documents_outside_camera_focus_keep_the_same_captured_file_identity() {
    let root = root(b"# Documentation\n\nneedle\n");
    let (exit, result) = invoke(&root, &["--focus", "src", "--markdown-hit", "0"]);
    assert_eq!(exit, EXIT_OK, "{result}");
    assert!(result.contains("\"text\":\"Documentation\""));
    assert!(result.contains("docs/README.md"));
    assert!(result.contains("\"document_complete\":true"));
}

#[test]
fn successful_document_reading_does_not_promote_truncated_search_to_complete() {
    let root = root(b"# Reading\n\nneedle and another needle\n");
    let (exit, result) = invoke(&root, &["--match-limit", "1", "--markdown-hit", "0"]);
    assert_eq!(exit, EXIT_PARTIAL, "{result}");
    assert!(result.contains("\"document_complete\":true"));
    assert!(result.contains("\"truncated\":true"));
    assert!(result.contains("\"whole_document_visible\":true"));
}

#[test]
fn explicit_heading_navigation_and_missing_targets_have_distinct_results() {
    let root = root(b"# Overview\n\nneedle\n\n## Usage\n\nRead this section.\n");
    let (exit, result) = invoke(&root, &["--markdown-hit", "0", "--markdown-heading", "usage", "--markdown-lines", "1"]);
    assert_eq!(exit, EXIT_PARTIAL, "{result}"); assert!(result.contains("\"text\":\"Usage\""));
    assert!(!result.contains("\"text\":\"Overview\""));
    for (args, code) in [(vec!["--markdown-hit", "99"], "ATLAS_TEXT_MISSING_HIT"),
        (vec!["--markdown-hit", "0", "--markdown-heading", "unknown-private"], "DOCUMENT_READ_HEADING_NOT_FOUND")] {
        let (exit, result) = invoke(&root, &args); assert_eq!(exit, EXIT_ERROR, "{result}");
        assert!(result.contains(code)); assert!(!result.contains("unknown-private"));
        assert_eq!(result.matches("\"schema\"").count(), 1);
        assert!(!result.contains("\"parcels\""), "failed candidates must be discarded");
    }
}

#[test]
fn unsupported_utf16_document_is_not_silently_redecoded_or_opened_as_live_utf8() {
    let mut bytes = vec![0xff, 0xfe];
    for unit in "# Title\n\nneedle\n".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    let root = root(&bytes);
    let (exit, search) = invoke(&root, &[]); assert_eq!(exit, EXIT_OK, "{search}");
    let (exit, result) = invoke(&root, &["--markdown-hit", "0"]);
    assert_eq!(exit, EXIT_ERROR, "{result}"); assert!(result.contains("DOCUMENT_READ_INVALID_UTF8"));
    assert_eq!(fs::read(root.join("docs/README.md")).unwrap(), bytes);
}

#[test]
fn document_source_admission_does_not_turn_an_oversized_capture_into_a_prefix() {
    let mut bytes = b"# Title\n\nneedle\n\n".to_vec(); bytes.resize(65_537, b'a');
    let root = root(&bytes);
    let (exit, result) = invoke(&root, &["--markdown-hit", "0"]);
    assert_eq!(exit, EXIT_ERROR, "{result}"); assert!(result.contains("DOCUMENT_READ_SOURCE_LIMIT"));
    assert!(!result.contains("\"document_complete\":true"));
}

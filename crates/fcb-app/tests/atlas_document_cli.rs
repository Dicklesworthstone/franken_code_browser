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
    invoke_query(root, "needle", extra)
}
fn invoke_query(root: &PathBuf, needle: &str, extra: &[&str]) -> (u8, String) {
    let mut args = vec![OsString::from("atlas"), root.as_os_str().to_owned(), OsString::from("--text"), OsString::from(needle), OsString::from("--json")];
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
    assert_eq!(exit, EXIT_PARTIAL, "{result}");
    assert_eq!(parcels(&result), parcels(&base)); assert_eq!(payload(&result), payload(&base));
    assert!(result.contains("\"document_preview\":{\"hit_index\":\"0\""));
    assert!(result.contains("\"document_schema\":\"fcb.document/1\""));
    assert!(result.contains("\"text\":\"Read the needle carefully.\""));
    assert!(result.contains("\"selected_original_hex\":\"6e6565646c65\""));
    assert!(result.contains("\"additional_source_bytes_read\":\"0\""));
    assert!(result.contains("\"rendered_hit_highlight\":false"));
    assert!(result.contains("\"navigation\":\"source-region\""));
    assert!(result.contains("\"requested_location_resolved\":true"));
    assert!(!result.contains("\"text\":\"Documentation\""));
    assert_eq!(fs::read(root.join("docs/README.md")).unwrap(), bytes);
}

#[test]
fn documents_outside_camera_focus_keep_the_same_captured_file_identity() {
    let root = root(b"# Documentation\n\nneedle\n");
    let (exit, result) = invoke(&root, &["--focus", "src", "--markdown-hit", "0"]);
    assert_eq!(exit, EXIT_PARTIAL, "{result}");
    assert!(result.contains("\"text\":\"needle\""));
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
    assert!(result.contains("\"whole_document_visible\":false"));
}

#[test]
fn explicit_heading_navigation_and_missing_targets_have_distinct_results() {
    let root = root(b"# Overview\n\nneedle\n\n## Usage\n\nRead this section.\n");
    let (exit, result) = invoke(&root, &["--markdown-hit", "0", "--markdown-heading", "usage", "--markdown-lines", "1"]);
    assert_eq!(exit, EXIT_PARTIAL, "{result}"); assert!(result.contains("\"text\":\"Usage\""));
    assert!(!result.contains("\"text\":\"Overview\""));
    assert!(result.contains("\"navigation\":\"heading\""));
    assert!(result.contains("\"requested_original_anchor\":null"));
    for (args, code) in [(vec!["--markdown-hit", "99"], "ATLAS_TEXT_MISSING_HIT"),
        (vec!["--markdown-hit", "0", "--markdown-heading", "unknown-private"], "DOCUMENT_READ_HEADING_NOT_FOUND")] {
        let (exit, result) = invoke(&root, &args); assert_eq!(exit, EXIT_ERROR, "{result}");
        assert!(result.contains(code)); assert!(!result.contains("unknown-private"));
        assert_eq!(result.matches("\"schema\"").count(), 1);
        assert!(!result.contains("\"parcels\""), "failed candidates must be discarded");
    }
}

#[test]
fn captured_atlas_heading_refusal_preserves_render_only_and_exact_source_search() {
    let source = b"---\ntitle: metadata\n---\n\n# Target\n\nneedle body\n";
    let root = root(source);
    let (exit, rendered) = invoke(&root, &["--markdown-hit", "0"]);
    assert_eq!(exit, EXIT_PARTIAL, "{rendered}");
    assert!(rendered.contains("\"document_schema\":\"fcb.document/1\""));
    assert!(rendered.contains("Target") && rendered.contains("needle body"));
    assert!(rendered.contains("\"navigation\":\"document-start-unqualified-source\""));
    assert!(rendered.contains("\"requested_location_resolved\":false"));
    assert!(rendered.contains("\"source_mapping_qualified\":false"));
    assert!(rendered.contains("\"whole_document_visible\":true"));
    assert!(rendered.contains(&format!("\"requested_original_anchor\":\"{}\"",
        std::str::from_utf8(source).unwrap().find("needle").unwrap())));
    let (exit, refused) = invoke(&root, &["--markdown-hit", "0", "--markdown-heading", "target"]);
    assert_eq!(exit, EXIT_ERROR, "{refused}");
    assert!(refused.contains("DOCUMENT_READ_INVALID_PROVENANCE"));
    assert_eq!(refused.matches("\"schema\"").count(), 1);
    assert!(!refused.contains("\"parcels\"") && !refused.contains("\"document_preview\""));
    let (exit, source_result) = invoke(&root, &["--preview-hit", "0"]);
    assert_eq!(exit, EXIT_OK, "{source_result}");
    assert!(source_result.contains("\"selected_original_hex\":\"6e6565646c65\""));
    assert_eq!(fs::read(root.join("docs/README.md")).unwrap(), source);
}

#[test]
fn selected_occurrence_uses_original_bom_offset_and_never_highlights_rendered_text() {
    let source = "\u{feff}# Overview\n\nFirst needle paragraph.\n\n## Details\n\nSecond café needle paragraph.\n";
    let root = root(source.as_bytes());
    let (base_exit, base) = invoke(&root, &[]);
    assert_eq!(base_exit, EXIT_OK, "{base}");
    let (exit, result) = invoke(&root, &["--markdown-hit", "1", "--markdown-lines", "1"]);
    assert_eq!(exit, EXIT_PARTIAL, "{result}");
    assert_eq!(parcels(&result), parcels(&base));
    assert_eq!(payload(&result), payload(&base));
    let preview = result.split("\"document_preview\":").nth(1).unwrap();
    assert!(preview.contains("\"text\":\"Second café needle paragraph.\""));
    assert!(!preview.contains("\"text\":\"First needle paragraph.\""));
    assert!(!preview.contains("\"text\":\"Overview\""));
    assert!(preview.contains(&format!("\"requested_original_anchor\":\"{}\"", source.rfind("needle").unwrap())));
    assert!(preview.contains("\"bom_bytes\":\"3\""));
    assert!(preview.contains("\"navigation\":\"source-region\""));
    assert!(preview.contains("\"selected_original_hex\":\"6e6565646c65\""));
    assert!(preview.contains("\"rendered_hit_highlight\":false"));
    assert_eq!(fs::read(root.join("docs/README.md")).unwrap(), source.as_bytes());
}

#[test]
fn unmapped_whitespace_hit_keeps_an_explicit_incomplete_render_only_preview() {
    let root = root(b"   \n");
    let (exit, result) = invoke_query(&root, "   ", &["--markdown-hit", "0"]);
    assert_eq!(exit, EXIT_PARTIAL, "{result}");
    assert!(result.contains("\"document_complete\":true"));
    assert!(result.contains("\"navigation\":\"document-start-unmapped-source\""));
    assert!(result.contains("\"requested_original_anchor\":\"0\""));
    assert!(result.contains("\"requested_location_resolved\":false"));
    assert!(result.contains("\"source_mapping_qualified\":true"));
    assert!(result.contains("\"whole_document_visible\":true"));
    assert!(result.contains("\"selected_original_hex\":\"202020\""));
    assert!(result.contains("\"rendered_hit_highlight\":false"));
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

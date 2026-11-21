#![forbid(unsafe_code)]
#![cfg(any(
    target_os = "macos",
    all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))
))]

use std::{ffi::OsString, fs, io::{self, Read, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};

struct NoInput;
impl Read for NoInput { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("Markdown must not consume implicit stdin") } }
fn file(bytes: &[u8]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let directory = std::env::temp_dir().join(format!("fcb-markdown-{}-{now}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("read me.md"); fs::write(&path, bytes).unwrap(); path
}
fn arguments(path: &PathBuf, tail: &[&str]) -> Vec<OsString> {
    let mut args = vec![OsString::from("markdown"), path.as_os_str().to_owned()];
    args.extend(tail.iter().map(OsString::from)); args
}
fn invoke(args: &[OsString]) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = run(args, &mut NoInput, &mut out, &mut err, || false);
    (exit, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

#[test]
fn real_file_renders_markdown_and_exposes_heading_and_original_byte_provenance() {
    let source = b"# Introduction\n\nRead **this** first.\n\n## Details\n\nImplementation here.\n";
    let path = file(source);
    let (exit, out, err) = invoke(&arguments(&path, &["--json"]));
    assert_eq!(exit, EXIT_OK, "{out} {err}"); assert!(err.is_empty());
    assert!(out.contains("\"document_schema\":\"fcb.document/1\""));
    assert!(out.contains("\"slug\":\"details\""));
    assert!(out.contains("\"text\":\"Read this first.\""));
    assert!(out.contains("\"document_complete\":true"));
    assert!(out.contains("\"native_presented\":false"));
    assert!(out.contains(&format!("\"payload_bytes_read\":\"{}\"", source.len())));
    assert_eq!(fs::read(path).unwrap(), source);
}

#[test]
fn heading_jumps_and_pages_do_not_claim_the_entire_document_is_visible() {
    let path = file(b"# First\n\nFirst text.\n\n## Second\n\nSecond text.\n");
    let (exit, page, _) = invoke(&arguments(&path, &["--json", "--lines", "1"]));
    assert_eq!(exit, EXIT_PARTIAL, "{page}");
    assert!(page.contains("\"whole_document_visible\":false"));
    assert!(page.contains("\"next_rendered_line\":\"2\""));
    let (exit, page, _) = invoke(&arguments(&path, &["--json", "--heading", "second", "--lines", "1"]));
    assert_eq!(exit, EXIT_PARTIAL, "{page}");
    assert!(page.contains("\"text\":\"Second\""));
    assert!(!page.contains("\"text\":\"First\""));
    let (exit, error, _) = invoke(&arguments(&path, &["--json", "--heading", "private-unknown-heading"]));
    assert_eq!(exit, EXIT_ERROR); assert!(error.contains("DOCUMENT_READ_HEADING_NOT_FOUND"));
    assert!(!error.contains("private-unknown-heading"));
}

#[test]
fn original_source_anchor_opens_its_enclosing_region_across_reflow_and_bom() {
    let source = "\u{feff}# Introduction\n\nEarlier paragraph.\n\n## Details\n\nLocate café and **needle** here.\n";
    let path = file(source.as_bytes());
    let anchor = source.find("needle").unwrap().to_string();
    for width in ["24", "80"] {
        let (exit, page, err) = invoke(&arguments(&path,
            &["--json", "--source-byte", &anchor, "--lines", "1", "--width", width]));
        assert_eq!(exit, EXIT_PARTIAL, "{page} {err}");
        assert!(err.is_empty());
        assert!(page.contains("\"navigation\":\"source-region\""));
        assert!(page.contains(&format!("\"requested_original_anchor\":\"{anchor}\"")));
        assert!(page.contains("\"requested_location_resolved\":true"));
        assert!(page.contains("\"source_mapping_qualified\":true"));
        assert!(page.contains("\"bom_bytes\":\"3\""));
        assert!(page.contains("\"text\":\"Locate café"));
        assert!(!page.contains("\"text\":\"Introduction\""), "negative control: do not reset to document start");
        assert!(page.contains("\"whole_document_visible\":false"));
        assert!(page.contains(&format!("\"payload_bytes_read\":\"{}\"", source.len())));
        assert!(page.contains("\"additional_source_bytes_read\":\"0\""));
    }
    let (exit, text, err) = invoke(&arguments(&path, &["--source-byte", &anchor, "--lines", "1"]));
    assert_eq!(exit, EXIT_PARTIAL, "{text} {err}");
    assert!(text.contains(&format!("Original source byte {anchor}: opened the first row")));
    assert_eq!(fs::read(path).unwrap(), source.as_bytes());
}

#[test]
fn explicit_source_anchors_refuse_invalid_domains_and_unmapped_regions_but_allow_eof() {
    let source = "\u{feff}# Heading\n\nA café paragraph.\n";
    let path = file(source.as_bytes());
    for offset in [0, 1, 2, source.find('é').unwrap() + 1, source.len() + 1] {
        let (exit, out, err) = invoke(&arguments(&path, &["--json", "--source-byte", &offset.to_string()]));
        assert_eq!(exit, EXIT_ERROR, "{offset}: {out} {err}");
        assert!(err.is_empty() && out.contains("DOCUMENT_READ_INVALID_RANGE"));
        assert!(!out.contains("\"document\":"));
    }
    let (exit, out, err) = invoke(&arguments(&path, &["--json", "--source-byte", &source.len().to_string()]));
    assert_eq!(exit, EXIT_PARTIAL, "{out} {err}");
    assert!(out.contains("\"navigation\":\"source-end\""));
    assert!(out.contains("\"requested_location_resolved\":true"));
    assert!(out.contains("\"lines\":[]"));
    assert!(out.contains("\"next_rendered_line\":null"));
    for source in ["   \n", "# Heading\n\ncafé\n"] {
        let path = file(source.as_bytes());
        let offset = source.find('é').map_or(1, |at| at + 1).to_string();
        let (exit, out, _) = invoke(&arguments(&path, &["--json", "--source-byte", &offset]));
        assert_eq!(exit, EXIT_ERROR, "{out}");
        assert!(out.contains("DOCUMENT_READ_INVALID_RANGE"));
        assert!(!out.contains("document-start-unmapped-source"), "explicit anchors cannot silently fall back");
    }
}

#[test]
fn source_anchor_options_are_validated_before_io_and_values_cannot_switch_output_mode() {
    let path = file(b"# Present\n").with_file_name("missing.md");
    for options in [vec!["--json", "--source-byte", "0", "--line", "1"],
        vec!["--json", "--line", "1", "--source-byte", "0"],
        vec!["--json", "--source-byte", "0", "--heading", "present"],
        vec!["--json", "--heading", "present", "--source-byte", "0"]] {
        let (exit, out, err) = invoke(&arguments(&path, &options));
        assert_eq!(exit, EXIT_ERROR, "{out} {err}");
        assert!(err.is_empty() && out.contains("CLI_INCOMPATIBLE_OPTIONS"));
    }
    let (exit, out, err) = invoke(&arguments(&path, &["--source-byte", "--json"]));
    assert_eq!(exit, EXIT_ERROR);
    assert!(out.is_empty() && err.contains("CLI_INVALID_NUMBER"));
    assert!(!err.contains("\"schema\""), "an invalid value is not an output-mode flag");
}

#[test]
fn unqualified_heading_and_copy_actions_refuse_without_disabling_rendered_cli_reading() {
    let source = b"---\ntitle: metadata\n---\n\n# Target\n\nretained body\n";
    let path = file(source);
    let (exit, rendered, err) = invoke(&arguments(&path, &["--json"]));
    assert_eq!(exit, EXIT_OK, "{rendered} {err}");
    assert!(err.is_empty() && rendered.contains("Target") && rendered.contains("retained body"));
    for options in [
        vec!["--json", "--heading", "target"],
        vec!["--json", "--copy-start", "0", "--copy-end", "6"],
        vec!["--json", "--source-byte", "32"],
    ] {
        let (exit, refused, err) = invoke(&arguments(&path, &options));
        assert_eq!(exit, EXIT_ERROR, "{refused} {err}");
        assert!(err.is_empty() && refused.contains("DOCUMENT_READ_INVALID_PROVENANCE"));
        assert_eq!(refused.matches("\"schema\"").count(), 1);
        assert!(!refused.contains("enclosing_original_hex") && !refused.contains("\"document\":"));
    }
    let (exit, out, err) = invoke(&arguments(&path, &["--heading", "target"]));
    assert_eq!(exit, EXIT_ERROR);
    assert!(out.is_empty() && err.contains("DOCUMENT_READ_INVALID_PROVENANCE"));
    assert_eq!(fs::read(path).unwrap(), source);
}

#[test]
fn utf8_bom_is_preserved_in_source_and_not_mistaken_for_heading_syntax() {
    let path = file("\u{feff}# Café 😀\n\nUseful text.\n".as_bytes());
    let (exit, out, _) = invoke(&arguments(&path, &["--json"]));
    assert_eq!(exit, EXIT_OK, "{out}");
    assert!(out.contains("\"bom_bytes\":\"3\""));
    assert!(out.contains("\"title\":\"Café 😀\""));
    assert!(out.contains("\"original_range\":{\"start\":\"3\""));
    assert!(fs::read(path).unwrap().starts_with(&[0xef, 0xbb, 0xbf]));
}

#[test]
fn copy_domains_are_explicit_and_do_not_synthesize_literal_markdown() {
    let path = file(b"A **bold** word.\n");
    let (exit, out, _) = invoke(&arguments(&path, &["--json", "--copy-start", "2", "--copy-end", "6"]));
    assert_eq!(exit, EXIT_OK, "{out}"); assert!(out.contains("\"rendered_text\":\"bold\""));
    assert!(out.contains("enclosing-markdown-block-not-literal-rendered-selection"));
    assert!(out.contains("2a2a626f6c642a2a"));
    let (exit, out, _) = invoke(&arguments(&path, &["--json", "--copy-start", "999", "--copy-end", "1000"]));
    assert_eq!(exit, EXIT_ERROR); assert!(out.contains("DOCUMENT_READ_INVALID_RANGE"));
    assert_eq!(out.matches("\"schema\"").count(), 1);
}

#[test]
fn oversized_and_unsupported_inputs_are_errors_not_prefix_documents() {
    let path = file(b"# Too big for this declared admission\n");
    let (exit, out, _) = invoke(&arguments(&path, &["--json", "--max-source-bytes", "4"]));
    assert_eq!(exit, EXIT_ERROR); assert!(out.contains("DOCUMENT_READ_SOURCE_LIMIT"));
    assert!(!out.contains("\"document_complete\":true"));
    let path = file(&[0xff, 0xfe, b'#', 0, b' ', 0]);
    for options in [vec!["--json"], vec!["--json", "--source-byte", "2"]] {
        let (exit, out, _) = invoke(&arguments(&path, &options));
        assert_eq!(exit, EXIT_ERROR); assert!(out.contains("DOCUMENT_READ_INVALID_UTF8"));
        assert!(!out.contains("\"document\":"));
    }
}

#[test]
fn links_images_html_and_embedded_commands_have_no_external_effects() {
    let path = file(b"# Safe\n\n[remote](https://invalid.example/fetch)\n\n![image](missing.png)\n\n<script>never_execute()</script>\n\n```sh\ntouch should-not-exist\n```\n");
    let before = fs::read_dir(path.parent().unwrap()).unwrap().count();
    let (exit, out, err) = invoke(&arguments(&path, &["--json"]));
    assert_eq!(exit, EXIT_OK, "{out} {err}");
    assert!(out.contains("\"asset_policy\":\"no-external-access\""));
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), before);
    assert!(!path.parent().unwrap().join("should-not-exist").exists());
}

#[test]
fn symlinks_and_directories_are_not_implicitly_followed_or_scanned() {
    use std::os::unix::fs::symlink;
    let path = file(b"# Original\n");
    let link = path.with_file_name("link.md"); symlink(&path, &link).unwrap();
    let (exit, out, _) = invoke(&arguments(&link, &["--json"]));
    assert_eq!(exit, EXIT_ERROR); assert!(out.contains("CLI_SYMLINK_REFUSED"));
    let (exit, out, _) = invoke(&arguments(&path.parent().unwrap().to_path_buf(), &["--json"]));
    assert_eq!(exit, EXIT_ERROR); assert!(out.contains("CLI_DIRECTORY_SCOPE_UNAVAILABLE"));
}

#[test]
fn cancellation_and_broken_delivery_never_append_a_second_json_document() {
    let path = file(b"# Reader\n\nBounded document.\n"); let args = arguments(&path, &["--json"]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run(&args, &mut NoInput, &mut out, &mut err, || true), EXIT_CANCELED);
    assert_eq!(String::from_utf8(out).unwrap().matches("\"schema\"").count(), 1);
    struct Broken(Vec<u8>);
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.0.is_empty() { return Err(io::ErrorKind::BrokenPipe.into()); }
            self.0.extend_from_slice(&bytes[..5]); Ok(5)
        }
        fn flush(&mut self) -> io::Result<()> { panic!("host owns flushing") }
    }
    let mut output = Broken(Vec::new()); let mut error = Vec::new();
    assert_eq!(run(&args, &mut NoInput, &mut output, &mut error, || false), EXIT_ERROR);
    assert_eq!(output.0.len(), 5);
    assert!(String::from_utf8(error).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
}

#[test]
fn help_does_not_require_a_source_or_a_runtime() {
    let args = [OsString::from("markdown"), OsString::from("--help"), OsString::from("--json")];
    let (exit, out, err) = invoke(&args); assert_eq!(exit, EXIT_OK); assert!(err.is_empty());
    assert!(out.contains("fcb markdown FILE"));
    assert!(out.contains("--source-byte"));
}

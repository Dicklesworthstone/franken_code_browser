#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Read}, path::{Path, PathBuf}, process::Command,
    sync::atomic::{AtomicU64, Ordering}};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use support::{Json, parse};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-whole-cli-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name); fs::write(&path, bytes).unwrap(); path
    }
}
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NoStdin;
impl Read for NoStdin { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("file search must not consume stdin") } }
fn args(path: &Path, flags: &[&str]) -> Vec<OsString> {
    let mut args = vec!["search".into(), path.as_os_str().to_owned(), "--json".into()];
    args.extend(flags.iter().map(OsString::from)); args
}
fn invoke(path: &Path, flags: &[&str]) -> (u8, Json) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = run(&args(path, flags), &mut NoStdin, &mut stdout, &mut stderr, || false);
    assert!(stderr.is_empty(), "stderr={:?}", String::from_utf8_lossy(&stderr));
    let json = parse(&stdout).unwrap_or_else(|error| panic!("{error}: {:?}", String::from_utf8_lossy(&stdout)));
    assert_eq!(json.get("schema").text(), "fcb.cli/1");
    (code, json)
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}

#[test]
fn whole_file_mode_reaches_a_large_file_tail_without_weakening_the_window_route() {
    let temp = Temp::new();
    let mut bytes = vec![b'x'; 2 * 1024 * 1024]; bytes.extend_from_slice(b"needle");
    let file = temp.file("huge.rs", &bytes);
    let (window_exit, window) = invoke(&file, &["--text", "needle"]);
    assert_eq!(window_exit, EXIT_PARTIAL); assert!(window.get("hits").array().is_empty());
    assert!(!window.get("whole_file_complete").flag());
    let (exit, whole) = invoke(&file, &["--whole-file", "--text", "needle"]);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(whole.get("strategy").text(), "streaming-whole-file");
    assert!(whole.get("whole_file_complete").flag());
    assert_eq!(whole.get("hits").array()[0].get("original_range").get("start").number(), 2 * 1024 * 1024);
    assert_eq!(whole.get("payload_bytes_read").number(), bytes.len() as u64);
    assert!(whole.get("peak_input_buffer_bytes").number() <= 16 * 1024);
    assert_eq!(whole.get("literal_original_hex").text(), "6e6565646c65");
    assert_eq!(whole.get("retention").text(), "literal-witnesses-only");
    assert_eq!(whole.get("hits").array()[0].get("decoded_range"), &Json::Null);
}

#[test]
fn workspace_streams_large_utf8_and_utf16_files_without_the_capture_quota() {
    let temp = Temp::new();
    let mut large = vec![b'x'; 1024 * 1024 + 7]; large.extend_from_slice(b"needle");
    temp.file("a.rs", &large);
    let little = utf16("prefix needle 😀", true);
    let big = utf16("needle\u{feff} needle", false);
    temp.file("b.rs", &little); temp.file("c.rs", &big);
    let (exit, json) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle"]);
    assert_eq!(exit, EXIT_OK); assert!(json.get("workspace_complete").flag());
    assert!(!json.get("capture_limits_applied").flag());
    assert_eq!(json.get("matches_seen").number(), 4);
    assert_eq!(json.get("files_examined").number(), 3);
    assert_eq!(json.get("payload_bytes_read").number(), (large.len() + little.len() + big.len()) as u64);
    let files = json.get("files").array();
    assert_eq!(files[0].get("hits").array()[0].get("original_range").get("start").number(), 1024 * 1024 + 7);
    assert_eq!(files[1].get("hits").array()[0].get("original_range").get("start").number(), 16);
    assert_eq!(files[1].get("literal_original_hex").text(), "6e006500650064006c006500");
    assert_eq!(files[2].get("literal_original_hex").text(), "006e006500650064006c0065");
}

#[test]
fn byte_admission_is_global_across_files_and_unvisited_members_are_explicit() {
    let temp = Temp::new();
    temp.file("a.rs", b"1234567890"); temp.file("b.rs", b"1234567890"); temp.file("c.rs", b"needle");
    let (exit, json) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle", "--max-scan-bytes", "15"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!json.get("workspace_complete").flag());
    assert_eq!(json.get("payload_bytes_read").number(), 15);
    assert_eq!(json.get("files_examined").number(), 2);
    assert_eq!(json.get("unexamined_files").number(), 1);
    assert_eq!(json.get("stop_reason").text(), "byte-limit");
    assert_eq!(json.get("matches_seen").number(), 0);
    let files = json.get("files").array();
    assert!(files[0].get("whole_file_complete").flag());
    assert!(!files[1].get("whole_file_complete").flag());
    assert_eq!(files[1].get("payload_bytes_read").number(), 5);
}

#[test]
fn result_limit_is_shared_and_exactly_full_is_not_proof_of_truncation() {
    let temp = Temp::new();
    temp.file("a.rs", b"needle"); temp.file("b.rs", b"absent");
    let (exit, exact) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle", "--limit", "1"]);
    assert_eq!(exit, EXIT_OK); assert!(exact.get("workspace_complete").flag());
    assert!(!exact.get("truncated").flag()); assert_eq!(exact.get("stored_hits").number(), 1);
    temp.file("c.rs", b"needle");
    let (exit, extra) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle", "--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(extra.get("truncated").flag());
    assert_eq!(extra.get("stored_hits").number(), 1); assert_eq!(extra.get("matches_seen").number(), 2);
    assert!(extra.get("files").array()[2].get("hits").array().is_empty());
    let (exit, absent) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "notpresent", "--limit", "0"]);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(absent.get("workspace_complete").flag());
    assert_eq!(absent.get("stored_hits").number(), 0);
}

#[test]
fn unsupported_text_leaves_other_file_results_useful_and_raw_mode_can_search_it() {
    let temp = Temp::new(); temp.file("a.rs", b"needle"); temp.file("b.rs", b"bad\xff");
    let (exit, text) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle"]);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(text.get("stored_hits").number(), 1);
    assert_eq!(text.get("files").array()[1].get("state").text(), "unsupported-text");
    assert_eq!(text.get("incomplete_files").number(), 1);
    let (exit, raw) = invoke(&temp.0, &["--workspace", "--whole-file", "--raw-hex", "ff"]);
    assert_eq!(exit, EXIT_OK); assert!(raw.get("workspace_complete").flag());
    assert_eq!(raw.get("files").array()[1].get("hits").array()[0].get("original_range").get("start").number(), 3);
    assert_eq!(raw.get("files").array()[1].get("literal_original_hex").text(), "ff");
}

#[test]
fn workspace_policy_and_raw_unix_names_are_preserved_in_streaming_mode() {
    use std::os::unix::ffi::OsStringExt;
    let temp = Temp::new();
    let name = OsString::from_vec(b"x\n\\\xff.rs".to_vec());
    fs::write(temp.0.join(name), b"needle").unwrap();
    fs::create_dir(temp.0.join("target")).unwrap();
    fs::write(temp.0.join("target/hidden.rs"), b"needle").unwrap();
    let (exit, json) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(json.get("known_files").number(), 1);
    assert_eq!(json.get("files").array()[0].get("path").get("hex").text(), "780a5cff2e7273");
    let (exit, all) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle", "--include-excluded"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(all.get("stored_hits").number(), 2);
}

#[test]
fn whole_file_admission_errors_and_cancellation_publish_only_one_error_document() {
    let temp = Temp::new(); let file = temp.file("file.rs", b"needle");
    let (exit, error) = invoke(&file, &["--whole-file", "--text", "needle", "--offset", "0"]);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error.get("status").text(), "error");
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = run(&args(&file, &["--whole-file", "--text", "needle"]), &mut NoStdin,
        &mut stdout, &mut stderr, || true);
    assert_eq!(code, EXIT_CANCELED); assert!(stderr.is_empty());
    let document = parse(&stdout).unwrap();
    assert_eq!(document.get("status").text(), "error");
    assert_eq!(document.get("error").get("code").text(), "CLI_CANCELED");
}

#[test]
fn standalone_executable_delivers_whole_file_and_workspace_search() {
    let temp = Temp::new();
    let mut bytes = vec![b'x'; 1024 * 1024 + 1]; bytes.extend_from_slice(b"needle");
    let file = temp.file("huge.rs", &bytes);
    for (path, flags) in [(&file, vec!["--whole-file", "--text", "needle"]),
        (&temp.0, vec!["--whole-file", "--workspace", "--text", "needle"])] {
        let result = Command::new(env!("CARGO_BIN_EXE_fcb")).args(args(path, &flags)).output().unwrap();
        assert!(result.status.success(), "stderr={:?}", String::from_utf8_lossy(&result.stderr));
        assert!(result.stderr.is_empty());
        let json = parse(&result.stdout).unwrap();
        assert_eq!(json.get("strategy").text(), "streaming-whole-file");
    }
}

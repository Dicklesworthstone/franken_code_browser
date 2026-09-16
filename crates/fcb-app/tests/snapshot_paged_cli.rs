#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, path::PathBuf, process::Command, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_NO_MATCH};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-paged-cli-{}-{n}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap(); fs::create_dir(root.join("root")).unwrap(); Self(root)
    }
    fn source(&self) -> PathBuf { self.0.join("root") }
    fn archive(&self) -> PathBuf { self.0.join("saved.fcbs") }
    fn save(&self, extra: &[&str]) -> (u8, Json) {
        let mut args = vec!["snapshot".into(), "save".into(), self.source().into_os_string(), "--output".into(), self.archive().into_os_string(), "--json".into()];
        args.extend(extra.iter().map(OsString::from)); invoke(&args)
    }
    fn args(&self, command: &str, extra: &[&str]) -> Vec<OsString> {
        let mut args = vec!["snapshot".into(), command.into(), self.archive().into_os_string(), "--json".into()];
        args.extend(extra.iter().map(OsString::from)); args
    }
    fn call(&self, command: &str, extra: &[&str]) -> (u8, Json) { invoke(&self.args(command, extra)) }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn invoke(args: &[OsString]) -> (u8, Json) {
    let mut out = Vec::new(); let mut error = Vec::new();
    let exit = run(args, &mut &b""[..], &mut out, &mut error, || false);
    assert!(error.is_empty(), "{}", String::from_utf8_lossy(&error));
    let document = parse(&out).unwrap(); assert_eq!(document.get("schema").text(), "fcb.cli/1");
    (exit, document)
}
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); } bytes
}

#[test]
fn read_by_line_and_continuation_uses_saved_utf16_not_replaced_live_source() {
    let f = Fixture::new();
    let original = utf16("head\r\nbanana 😀\r\nlast");
    fs::write(f.source().join("file.rs"), &original).unwrap(); assert_eq!(f.save(&[]).0, EXIT_OK);
    fs::rename(f.source(), f.0.join("old-root")).unwrap(); fs::create_dir(f.source()).unwrap();
    fs::write(f.source().join("file.rs"), b"not the old content").unwrap();
    let (exit, read) = f.call("read", &["--member", "file.rs", "--line", "2", "--lines", "1"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(read.get("text").text(), "banana 😀\r\n");
    assert_eq!(read.get("original_range").get("start").number(), 14);
    assert_eq!(read.get("original_range").get("end").number(), 36);
    assert_eq!(read.get("next_offset").number(), 36);
    assert!(!read.get("live_roots_accessed").flag());
    assert_eq!(read.get("lines").array()[0].get("number").number(), 2);
    assert!(!read.get("lines").array()[0].get("continued_before").flag());
    let (exit, last) = f.call("read", &["--member", "file.rs", "--offset", "36"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(last.get("text").text(), "last"); assert!(last.get("reaches_eof").flag());
    assert_eq!(last.get("lines").array()[0].get("number").number(), 3);
    let (exit, error) = f.call("read", &["--member", "file.rs", "--line", "999"]);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error.get("error").get("code").text(), "SNAPSHOT_LINE_OUT_OF_BOUNDS");
}

#[test]
fn raw_member_identity_and_original_bytes_survive_utf8_errors_and_byte_order_marks() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    let name = b"literal\\\xff.rs";
    let native = OsString::from_vec(name.to_vec());
    fs::write(f.source().join(native), &[0xef, 0xbb, 0xbf, b'a', 0xff]).unwrap();
    assert_eq!(f.save(&[]).0, EXIT_OK);
    let hex = name.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let (exit, read) = f.call("read", &["--member-hex", &hex]);
    assert_eq!(exit, EXIT_OK); assert_eq!(read.get("text").text(), "a\u{fffd}");
    assert!(read.get("contains_replacements").flag());
    assert_eq!(read.get("path").get("hex").text(), hex);
    let (exit, raw) = f.call("read", &["--member-hex", &hex, "--raw", "--bytes", "4"]);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(raw.get("original_hex").text(), "efbbbf61");
    assert_eq!(raw.get("next_offset").number(), 4);
    let (exit, last) = f.call("read", &["--member-hex", &hex, "--raw", "--offset", "4"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(last.get("original_hex").text(), "ff");
    let (exit, search) = f.call("search", &["--raw-hex", "ff"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(search.get("hits").array().len(), 1);
    assert_eq!(search.get("hits").array()[0].get("original_range").get("start").number(), 4);
    assert_eq!(search.get("hits").array()[0].get("matched_raw_hex").text(), "ff");
    let (exit, text) = f.call("search", &["--text", "a"]);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(text.get("unsupported_text_files_count").number(), 1);
}

#[test]
fn inspect_retains_no_payload_and_read_loads_only_one_member_after_validation() {
    let f = Fixture::new();
    let payload = vec![b'x'; 128 * 1024];
    for path in ["a", "b", "c"] { fs::write(f.source().join(path), &payload).unwrap(); }
    assert_eq!(f.save(&[]).0, EXIT_OK);
    let (exit, inspect) = f.call("inspect", &[]);
    assert_eq!(exit, EXIT_OK); assert_eq!(inspect.get("member_payload_bytes_loaded").number(), 0);
    assert_eq!(inspect.get("archive_validation_bytes").number(), fs::metadata(f.archive()).unwrap().len());
    assert!(inspect.get("catalog_reserved_bytes").number() < payload.len() as u64);
    let (exit, read) = f.call("read", &["--member", "b", "--bytes", "16", "--lines", "1"]);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(read.get("member_payload_bytes_loaded").number(), payload.len() as u64);
    assert_eq!(read.get("text").text(), "xxxxxxxxxxxxxxxx");
    assert!(read.get("lines").array()[0].get("continued_after").flag());
    let (exit, search) = f.call("search", &["--text", "absent"]);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(search.get("workspace_complete").flag());
    assert_eq!(search.get("engine").text(), "bounded-stream-exact");
    assert_eq!(search.get("peak_source_bytes").number(), payload.len() as u64);
    assert_eq!(search.get("member_payload_bytes_loaded").number(), 3 * payload.len() as u64);
}

#[test]
fn exact_result_limit_does_not_invent_truncation_and_cross_file_lookahead_detects_it() {
    let f = Fixture::new(); fs::write(f.source().join("a"), b"needle").unwrap(); fs::write(f.source().join("b"), b"needle").unwrap();
    assert_eq!(f.save(&[]).0, EXIT_OK);
    let (exit, exact) = f.call("search", &["--text", "needle", "--limit", "2"]);
    assert_eq!(exit, EXIT_OK); assert!(!exact.get("truncated").flag()); assert_eq!(exact.get("hits").array().len(), 2);
    let (exit, truncated) = f.call("search", &["--text", "needle", "--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(truncated.get("truncated").flag()); assert_eq!(truncated.get("matches_seen").number(), 2);
    let (exit, zero) = f.call("search", &["--text", "absent", "--limit", "0"]);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(!zero.get("truncated").flag());
}

#[test]
fn unavailable_saved_members_and_arbitrary_member_paths_never_open_live_files() {
    let f = Fixture::new(); fs::write(f.source().join("a"), b"sensitive live content").unwrap();
    assert_eq!(f.save(&["--max-file-bytes", "0"]).0, EXIT_PARTIAL);
    let (exit, missing) = f.call("read", &["--member", "a"]);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(missing.get("error").get("code").text(), "SNAPSHOT_MEMBER_UNAVAILABLE");
    fs::write(f.0.join("outside"), b"must not be loaded").unwrap();
    for path in ["../outside", "../../outside", "/etc/passwd"] {
        let (exit, absent) = f.call("read", &["--member", path]);
        assert_eq!(exit, EXIT_ERROR); assert_eq!(absent.get("error").get("code").text(), "SNAPSHOT_MEMBER_NOT_FOUND");
    }
}

#[test]
fn source_controls_are_escaped_in_human_reading_and_preserved_in_json() {
    let f = Fixture::new(); fs::write(f.source().join("a"), "hello\u{001b}[31m\u{202e}world").unwrap();
    assert_eq!(f.save(&[]).0, EXIT_OK);
    let (_, read) = f.call("read", &["--member", "a"]);
    assert_eq!(read.get("text").text(), "hello\u{001b}[31m\u{202e}world");
    let args = vec!["snapshot".into(), "read".into(), f.archive().into_os_string(), "--member".into(), "a".into()];
    let mut out = Vec::new(); let mut err = Vec::new();
    assert_eq!(run(&args, &mut &b""[..], &mut out, &mut err, || false), EXIT_OK);
    let human = std::str::from_utf8(&out).unwrap();
    assert!(!human.contains('\u{001b}')); assert!(!human.contains('\u{202e}')); assert!(human.contains("\\u{1b}"));
}

#[test]
fn actual_binary_reads_and_searches_after_original_source_directory_moves() {
    let f = Fixture::new(); fs::write(f.source().join("a.rs"), b"first\nsecond needle\n").unwrap();
    assert_eq!(f.save(&[]).0, EXIT_OK); fs::rename(f.source(), f.0.join("gone-root")).unwrap();
    let read = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.args("read", &["--member", "a.rs", "--line", "2"])).output().unwrap();
    assert_eq!(read.status.code(), Some(EXIT_OK as i32)); assert!(read.stderr.is_empty());
    let document = parse(&read.stdout).unwrap(); assert_eq!(document.get("lines").array()[0].get("number").number(), 2);
    assert_eq!(document.get("text").text(), "second needle\n");
    let search = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.args("search", &["--text", "needle"])).output().unwrap();
    assert_eq!(search.status.code(), Some(EXIT_OK as i32));
    assert_eq!(parse(&search.stdout).unwrap().get("hits").array().len(), 1);
}

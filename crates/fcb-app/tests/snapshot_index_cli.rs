#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Read, Write}, path::PathBuf, process::Command,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-index-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); fs::create_dir(path.join("root")).unwrap(); Self(path)
    }
    fn root(&self) -> PathBuf { self.0.join("root") }
    fn saved(&self) -> PathBuf { self.0.join("saved.fcbs") }
    fn index(&self) -> PathBuf { self.0.join("saved.fcbi") }
    fn save(&self) {
        let args = vec!["snapshot".into(), "save".into(), self.root().into_os_string(), "--output".into(), self.saved().into_os_string(), "--json".into()];
        assert_eq!(invoke(&args, || false).0, EXIT_OK);
    }
    fn build_args(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "index".into(), "build".into(), self.saved().into_os_string(), "--output".into(), self.index().into_os_string(), "--json".into()]
    }
    fn inspect_args(&self, pin: &str) -> Vec<OsString> {
        vec!["snapshot".into(), "index".into(), "inspect".into(), self.saved().into_os_string(), "--index".into(),
            self.index().into_os_string(), "--index-digest".into(), pin.into(), "--json".into()]
    }
    fn search_args(&self, pin: &str, text: &str) -> Vec<OsString> {
        let mut args = self.inspect_args(pin); args[2] = "search".into();
        args.extend([OsString::from("--text"), OsString::from(text)]); args
    }
    fn build(&self) -> String {
        let (exit, result, stderr) = invoke(&self.build_args(), || false);
        assert_eq!(exit, EXIT_OK); assert!(stderr.is_empty());
        assert_eq!(result.get("effect").text(), "complete-file-sync-requested");
        result.get("index_digest").text().to_owned()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NoInput;
impl Read for NoInput {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("index commands cannot consume stdin") }
}
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json, Vec<u8>) {
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let exit = run(args, &mut NoInput, &mut stdout, &mut stderr, canceled);
    let value = parse(&stdout).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&stdout)));
    assert_eq!(value.get("schema").text(), "fcb.cli/1");
    (exit, value, stderr)
}

#[test]
fn separate_commands_reopen_saved_segments_without_rebuilding_or_loading_nonmatches() {
    let fixture = Fixture::new();
    fs::write(fixture.root().join("a.rs"), b"banana").unwrap();
    fs::write(fixture.root().join("b.rs"), b"unrelated").unwrap();
    fs::write(fixture.root().join("c.rs"), b"different").unwrap();
    fixture.save(); let pin = fixture.build();
    fs::rename(fixture.root(), fixture.0.join("old-root")).unwrap();
    let (exit, result, stderr) = invoke(&fixture.search_args(&pin, "ana"), || false);
    assert_eq!(exit, EXIT_OK); assert!(stderr.is_empty());
    assert!(result.get("workspace_complete").flag());
    assert_eq!(result.get("index_build_source_bytes").number(), 0);
    assert_eq!(result.get("index_eliminated_files").number(), 2);
    assert_eq!(result.get("member_payload_bytes_loaded").number(), 6);
    assert_eq!(result.get("loaded_members").number(), 1);
    assert!(result.get("archive_validation_bytes").number() > 6, "initial full validation must remain visible");
    assert_eq!(result.get("hits").array().len(), 2);
    assert!(!result.get("live_roots_accessed").flag());
    let (exit, inspected, _) = invoke(&fixture.inspect_args(&pin), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(inspected.get("indexed_files").number(), 3);
    assert_eq!(inspected.get("member_payload_bytes_loaded").number(), 0);
}

#[test]
fn missing_wrong_or_changed_pin_is_not_treated_as_a_complete_negative() {
    let fixture = Fixture::new(); fs::write(fixture.root().join("a"), b"needle").unwrap();
    fixture.save(); let pin = fixture.build();
    let mut missing = fixture.search_args(&pin, "needle"); missing.drain(6..8);
    assert_eq!(invoke(&missing, || false).0, EXIT_ERROR);
    let (exit, result, _) = invoke(&fixture.search_args(&"0".repeat(64), "needle"), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("error").get("code").text(), "SAVED_INDEX_PIN_MISMATCH");
    let mut bytes = fs::read(fixture.index()).unwrap(); let i = bytes.len() / 2; bytes[i] ^= 1;
    fs::write(fixture.index(), &bytes).unwrap();
    let (exit, result, _) = invoke(&fixture.search_args(&pin, "needle"), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("error").get("code").text(), "SAVED_INDEX_PIN_MISMATCH");
    assert!(!result.get("complete").flag());
    // The ordinary, unfiltered route remains usable without the bad sidecar.
    let normal = vec!["snapshot".into(), "search".into(), fixture.saved().into_os_string(), "--text".into(), "needle".into(), "--json".into()];
    assert_eq!(invoke(&normal, || false).0, EXIT_OK);
}

#[test]
fn exhausted_gram_budget_still_searches_every_captured_file() {
    let fixture = Fixture::new(); fs::write(fixture.root().join("a"), b"banana").unwrap();
    fs::write(fixture.root().join("b"), b"bandana").unwrap(); fixture.save();
    let mut args = fixture.build_args(); args.extend([OsString::from("--max-grams"), OsString::from("0")]);
    let (exit, result, _) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(result.get("uncovered_files").number(), 2);
    let pin = result.get("index_digest").text();
    let (exit, result, _) = invoke(&fixture.search_args(pin, "ana"), || false);
    assert_eq!(exit, EXIT_OK); assert!(result.get("workspace_complete").flag());
    assert_eq!(result.get("index_fallback_files").number(), 2);
    assert_eq!(result.get("hits").array().len(), 3);
}

#[test]
fn utf16_and_raw_byte_search_keep_the_same_original_ranges() {
    let fixture = Fixture::new();
    let mut utf16 = vec![0xff, 0xfe];
    for unit in "needle".encode_utf16() { utf16.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(fixture.root().join("a.rs"), &utf16).unwrap();
    fs::write(fixture.root().join("b.bin"), &[0, 0xff, 1, 2]).unwrap(); fixture.save();
    let pin = fixture.build();
    let (exit, text, _) = invoke(&fixture.search_args(&pin, "needle"), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(text.get("unsupported_text_files_count").number(), 1);
    let hit = &text.get("hits").array()[0];
    assert_eq!(hit.get("original_range").get("start").number(), 2);
    assert_eq!(hit.get("original_range").get("end").number(), 14);
    let mut args = fixture.inspect_args(&pin); args[2] = "search".into();
    args.extend([OsString::from("--raw-hex"), OsString::from("00ff01")]);
    let (exit, raw, _) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(raw.get("hits").array().len(), 1);
    assert_eq!(raw.get("index_eliminated_files").number(), 1);
    assert_eq!(raw.get("hits").array()[0].get("original_range").get("start").number(), 0);
}

#[test]
fn exact_display_limit_and_zero_limit_keep_cross_file_lookahead() {
    let fixture = Fixture::new(); fs::write(fixture.root().join("a"), b"needle").unwrap();
    fs::write(fixture.root().join("b"), b"no match").unwrap(); fixture.save(); let pin = fixture.build();
    let mut args = fixture.search_args(&pin, "needle"); args.extend([OsString::from("--limit"), OsString::from("1")]);
    let (exit, full, _) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert!(!full.get("truncated").flag());
    *args.last_mut().unwrap() = "0".into();
    let (exit, zero, _) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(zero.get("truncated").flag());
    assert_eq!(zero.get("matches_seen").number(), 1); assert!(zero.get("hits").array().is_empty());
}

#[test]
fn index_publication_never_overwrites_existing_files_or_source_snapshots() {
    let fixture = Fixture::new(); fixture.save();
    fs::write(fixture.index(), b"preserve this file").unwrap();
    let (exit, failed, _) = invoke(&fixture.build_args(), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(failed.get("effect").text(), "none");
    assert_eq!(fs::read(fixture.index()).unwrap(), b"preserve this file");
    let original = fs::read(fixture.saved()).unwrap();
    let mut args = fixture.build_args(); args[5] = fixture.saved().into_os_string();
    assert_eq!(invoke(&args, || false).0, EXIT_ERROR);
    assert_eq!(fs::read(fixture.saved()).unwrap(), original);
}

#[test]
fn cancellation_preserves_exact_destination_effect_state() {
    let fixture = Fixture::new(); fixture.save();
    let (exit, early, _) = invoke(&fixture.build_args(), || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(early.get("effect").text(), "none");
    assert!(!fixture.index().exists());
    let (exit, created, _) = invoke(&fixture.build_args(), || fixture.index().exists());
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(created.get("effect").text(), "destination-created-incomplete");
    assert_eq!(fs::metadata(fixture.index()).unwrap().len(), 0);
}

#[test]
fn lost_build_receipt_does_not_remove_the_written_index() {
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::BrokenPipe.into()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let fixture = Fixture::new(); fixture.save();
    let mut stderr = Vec::new();
    assert_eq!(run(&fixture.build_args(), &mut NoInput, &mut Closed, &mut stderr, || false), EXIT_ERROR);
    assert!(fs::metadata(fixture.index()).unwrap().len() > 0);
    assert!(String::from_utf8(stderr).unwrap().contains("effect=complete-file-sync-requested"));
}

#[test]
fn actual_binary_uses_saved_index_after_original_source_moves() {
    let fixture = Fixture::new(); fs::write(fixture.root().join("a"), b"banana").unwrap();
    fs::write(fixture.root().join("b"), b"unrelated").unwrap(); fixture.save();
    let output = Command::new(env!("CARGO_BIN_EXE_fcb")).args(fixture.build_args()).output().unwrap();
    assert_eq!(output.status.code(), Some(EXIT_OK as i32));
    let built = parse(&output.stdout).unwrap(); let pin = built.get("index_digest").text();
    fs::rename(fixture.root(), fixture.0.join("moved-root")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fcb")).args(fixture.search_args(pin, "ana")).output().unwrap();
    assert_eq!(output.status.code(), Some(EXIT_OK as i32));
    let result = parse(&output.stdout).unwrap();
    assert_eq!(result.get("hits").array().len(), 2); assert_eq!(result.get("index_eliminated_files").number(), 1);
    let absent = Command::new(env!("CARGO_BIN_EXE_fcb")).args(fixture.search_args(pin, "absent")).output().unwrap();
    assert_eq!(absent.status.code(), Some(EXIT_NO_MATCH as i32));
    assert_eq!(parse(&absent.stdout).unwrap().get("member_payload_bytes_loaded").number(), 0);
}

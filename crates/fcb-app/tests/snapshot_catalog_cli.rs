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
        let path = std::env::temp_dir().join(format!("fcb-catalog-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); fs::create_dir(path.join("root")).unwrap(); Self(path)
    }
    fn root(&self) -> PathBuf { self.0.join("root") }
    fn saved(&self) -> PathBuf { self.0.join("saved.fcbs") }
    fn catalog(&self) -> PathBuf { self.0.join("saved.fcbc") }
    fn index(&self) -> PathBuf { self.0.join("saved.fcbi") }
    fn save_args(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "save".into(), self.root().into_os_string(), "--output".into(), self.saved().into_os_string(), "--json".into()]
    }
    fn save(&self) { assert_eq!(invoke(&self.save_args(), || false).0, EXIT_OK); }
    fn catalog_args(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "catalog".into(), self.saved().into_os_string(), "--output".into(), self.catalog().into_os_string(), "--json".into()]
    }
    fn build_catalog(&self) -> String {
        let (exit, receipt, stderr) = invoke(&self.catalog_args(), || false);
        assert_eq!(exit, EXIT_OK); assert!(stderr.is_empty());
        assert!(receipt.get("archive_body_verified_on_open").flag());
        assert_eq!(receipt.get("archive_validation_bytes").number(), fs::metadata(self.saved()).unwrap().len());
        assert_eq!(receipt.get("effect").text(), "complete-file-sync-requested");
        assert!(!receipt.get("source_payload_in_catalog").flag());
        receipt.get("catalog_digest").text().to_owned()
    }
    fn with_catalog(&self, mut args: Vec<OsString>, pin: &str) -> Vec<OsString> {
        args.extend(["--catalog".into(), self.catalog().into_os_string(), "--catalog-digest".into(), pin.into()]); args
    }
    fn inspect_args(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "inspect".into(), self.saved().into_os_string(), "--json".into()]
    }
    fn read_args(&self, name: &str) -> Vec<OsString> {
        vec!["snapshot".into(), "read".into(), self.saved().into_os_string(), "--member".into(), name.into(), "--json".into()]
    }
    fn search_args(&self, text: &str) -> Vec<OsString> {
        vec!["snapshot".into(), "search".into(), self.saved().into_os_string(), "--text".into(), text.into(), "--json".into()]
    }
    fn build_index(&self, catalog_pin: Option<&str>) -> String {
        let mut args = vec!["snapshot".into(), "index".into(), "build".into(), self.saved().into_os_string(),
            "--output".into(), self.index().into_os_string(), "--json".into()];
        if let Some(pin) = catalog_pin { args = self.with_catalog(args, pin); }
        let (exit, result, _) = invoke(&args, || false);
        assert_eq!(exit, EXIT_OK);
        if catalog_pin.is_some() { assert_eq!(result.get("archive_validation_bytes").number(), 56); }
        result.get("index_digest").text().to_owned()
    }
    fn indexed_args(&self, pin: &str, text: &str) -> Vec<OsString> {
        vec!["snapshot".into(), "index".into(), "search".into(), self.saved().into_os_string(),
            "--index".into(), self.index().into_os_string(), "--index-digest".into(), pin.into(),
            "--text".into(), text.into(), "--json".into()]
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NoInput;
impl Read for NoInput {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("catalog commands may not consume stdin") }
}
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json, Vec<u8>) {
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let exit = run(args, &mut NoInput, &mut stdout, &mut stderr, canceled);
    let document = parse(&stdout).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&stdout)));
    assert_eq!(document.get("schema").text(), "fcb.cli/1");
    (exit, document, stderr)
}
fn cold_receipt(result: &Json) {
    assert_eq!(result.get("archive_validation_bytes").number(), 56);
    assert_eq!(result.get("archive_open_validation").text(), "trusted-catalog-and-boundaries");
    assert!(!result.get("archive_body_verified_on_open").flag());
    assert_eq!(result.get("member_verification").text(), "digest-before-publication");
    assert!(!result.get("live_roots_accessed").flag());
}

#[test]
fn catalog_reopening_is_opt_in_and_inspection_loads_no_saved_source() {
    let f = Fixture::new(); fs::write(f.root().join("a.rs"), vec![b'x'; 256 * 1024]).unwrap();
    f.save(); let pin = f.build_catalog();
    let (exit, result, _) = invoke(&f.with_catalog(f.inspect_args(), &pin), || false);
    assert_eq!(exit, EXIT_OK); cold_receipt(&result);
    assert_eq!(result.get("catalog_digest").text(), pin);
    assert_eq!(result.get("member_payload_bytes_loaded").number(), 0);
    let (exit, full, _) = invoke(&f.inspect_args(), || false);
    assert_eq!(exit, EXIT_OK); assert!(full.get("archive_body_verified_on_open").flag());
    assert_eq!(full.get("archive_validation_bytes").number(), fs::metadata(f.saved()).unwrap().len());
    assert_eq!(full.get("archive_open_validation").text(), "full-archive");
}

#[test]
fn indexed_cold_search_skips_validation_body_and_all_nonmatching_payloads() {
    let f = Fixture::new();
    fs::write(f.root().join("a.rs"), b"banana").unwrap();
    for i in 0..20 { fs::write(f.root().join(format!("b{i:02}.rs")), vec![b'x'; 4096]).unwrap(); }
    f.save(); let pin = f.build_catalog(); let index = f.build_index(Some(&pin));
    fs::rename(f.root(), f.0.join("moved-root")).unwrap();
    let (exit, result, stderr) = invoke(&f.with_catalog(f.indexed_args(&index, "ana"), &pin), || false);
    assert_eq!(exit, EXIT_OK); assert!(stderr.is_empty()); cold_receipt(&result);
    assert_eq!(result.get("hits").array().len(), 2);
    assert_eq!(result.get("index_eliminated_files").number(), 20);
    assert_eq!(result.get("member_payload_bytes_loaded").number(), 6);
    assert_eq!(result.get("index_build_source_bytes").number(), 0);
    let (exit, negative, _) = invoke(&f.with_catalog(f.indexed_args(&index, "not-present"), &pin), || false);
    assert_eq!(exit, EXIT_NO_MATCH); cold_receipt(&negative);
    assert!(negative.get("workspace_complete").flag());
    assert_eq!(negative.get("loaded_members").number(), 0);
    assert_eq!(negative.get("index_eliminated_files").number(), 21);
}

#[test]
fn cold_read_preserves_utf16_ranges_and_literal_native_filenames() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    let name = b"raw\\name\xff.rs";
    let mut bytes = vec![0xff, 0xfe];
    for unit in "head\r\nbanana".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(f.root().join(OsString::from_vec(name.to_vec())), &bytes).unwrap();
    f.save(); let pin = f.build_catalog();
    let hex = name.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let args = vec!["snapshot".into(), "read".into(), f.saved().into_os_string(), "--member-hex".into(), hex.clone().into(),
        "--line".into(), "2".into(), "--json".into()];
    let (exit, result, _) = invoke(&f.with_catalog(args, &pin), || false);
    assert_eq!(exit, EXIT_OK); cold_receipt(&result);
    assert_eq!(result.get("text").text(), "banana");
    assert_eq!(result.get("path").get("hex").text(), hex);
    assert_eq!(result.get("original_range").get("start").number(), 14);
    assert_eq!(result.get("member_payload_bytes_loaded").number(), bytes.len() as u64);
}

#[test]
fn wrong_or_corrupt_catalog_is_an_error_not_silent_full_scan_fallback() {
    let f = Fixture::new(); fs::write(f.root().join("a"), b"needle").unwrap();
    f.save(); let pin = f.build_catalog();
    let (exit, result, _) = invoke(&f.with_catalog(f.search_args("needle"), &"00".repeat(32)), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("error").get("code").text(), "CATALOG_PIN_MISMATCH");
    let original = fs::read(f.catalog()).unwrap();
    let mut changed = original.clone(); let end = changed.len()-1; changed[end] ^= 1;
    fs::write(f.catalog(), &changed).unwrap();
    let (exit, result, _) = invoke(&f.with_catalog(f.search_args("needle"), &pin), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("effect").text(), "none");
    assert_eq!(result.get("error").get("code").text(), "CATALOG_PIN_MISMATCH");
    // Ordinary commands remain explicit full-validation fallback; no automatic repair.
    assert_eq!(invoke(&f.search_args("needle"), || false).0, EXIT_OK);
    assert_eq!(fs::read(f.catalog()).unwrap(), changed);
}

#[test]
fn skipped_corruption_is_not_reported_as_verified_and_selected_corruption_is_rejected() {
    let f = Fixture::new(); fs::write(f.root().join("a"), b"needle").unwrap();
    fs::write(f.root().join("b"), b"unrelated").unwrap();
    f.save(); let pin = f.build_catalog(); let index = f.build_index(None);
    let mut bytes = fs::read(f.saved()).unwrap();
    let pos = bytes.windows(9).position(|window| window == b"unrelated").unwrap();
    bytes[pos] ^= 1; fs::write(f.saved(), &bytes).unwrap();
    let (exit, result, _) = invoke(&f.with_catalog(f.indexed_args(&index, "needle"), &pin), || false);
    assert_eq!(exit, EXIT_OK); cold_receipt(&result);
    assert!(result.get("workspace_complete").flag());
    assert_eq!(result.get("loaded_members").number(), 1);
    let (exit, bad, _) = invoke(&f.with_catalog(f.read_args("b"), &pin), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(bad.get("error").get("code").text(), "SNAPSHOT_MEMBER_CHANGED");
    assert_eq!(invoke(&f.inspect_args(), || false).0, EXIT_ERROR, "full validation must still detect unread corruption");
}

#[test]
fn catalog_writes_preserve_no_overwrite_and_interrupted_destination_effects() {
    let f = Fixture::new(); fs::write(f.root().join("a"), b"needle").unwrap(); f.save();
    let (exit, result, _) = invoke(&f.catalog_args(), || f.catalog().exists());
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(result.get("effect").text(), "destination-created-incomplete");
    assert_eq!(fs::metadata(f.catalog()).unwrap().len(), 0);
    let (exit, result, _) = invoke(&f.catalog_args(), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("error").get("code").text(), "SNAPSHOT_DESTINATION_EXISTS");
    assert_eq!(result.get("effect").text(), "none");
    assert_eq!(fs::metadata(f.catalog()).unwrap().len(), 0);
}

#[test]
fn lost_catalog_receipt_cannot_undo_a_completed_export() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::Error::from(io::ErrorKind::BrokenPipe)) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let f = Fixture::new(); f.save(); let mut stderr = Vec::new();
    assert_eq!(run(&f.catalog_args(), &mut NoInput, &mut Broken, &mut stderr, || false), EXIT_ERROR);
    assert!(String::from_utf8(stderr).unwrap().contains("effect=complete-file-sync-requested"));
    assert!(fs::metadata(f.catalog()).unwrap().len() > 56);
    // A missing pin must not be recovered by hashing that untrusted file.
    let args = vec!["snapshot".into(), "inspect".into(), f.saved().into_os_string(), "--catalog".into(), f.catalog().into_os_string(), "--json".into()];
    assert_eq!(invoke(&args, || false).0, EXIT_ERROR);
}

#[test]
fn empty_and_incomplete_saved_scopes_keep_their_meaning_after_catalog_reopen() {
    let f = Fixture::new(); f.save(); let pin = f.build_catalog();
    let (exit, result, _) = invoke(&f.with_catalog(f.search_args("needle"), &pin), || false);
    assert_eq!(exit, EXIT_NO_MATCH); cold_receipt(&result); assert!(result.get("workspace_complete").flag());
    let partial = Fixture::new();
    fs::write(partial.root().join("a"), b"needle").unwrap();
    fs::write(partial.root().join("b"), b"needle").unwrap();
    let mut args = partial.save_args(); args.extend(["--max-files".into(), "1".into()]);
    assert_eq!(invoke(&args, || false).0, EXIT_PARTIAL);
    let (exit, receipt, _) = invoke(&partial.catalog_args(), || false);
    assert_eq!(exit, EXIT_PARTIAL);
    let (exit, result, _) = invoke(&partial.with_catalog(partial.search_args("needle"), receipt.get("catalog_digest").text()), || false);
    assert_eq!(exit, EXIT_PARTIAL); cold_receipt(&result); assert!(!result.get("workspace_complete").flag());
}

#[test]
fn actual_binary_uses_catalog_and_index_after_live_repository_moves() {
    let f = Fixture::new(); fs::write(f.root().join("a.rs"), b"needle").unwrap();
    fs::write(f.root().join("b.rs"), b"nothing").unwrap(); f.save();
    let built = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.catalog_args()).output().unwrap();
    assert_eq!(built.status.code(), Some(EXIT_OK as i32));
    let receipt = parse(&built.stdout).unwrap();
    let pin = receipt.get("catalog_digest").text();
    let index = f.build_index(None);
    fs::rename(f.root(), f.0.join("retired-root")).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.with_catalog(f.indexed_args(&index, "needle"), pin)).output().unwrap();
    assert_eq!(result.status.code(), Some(EXIT_OK as i32));
    let result = parse(&result.stdout).unwrap(); cold_receipt(&result);
    assert_eq!(result.get("hits").array().len(), 1);
    assert_eq!(result.get("member_payload_bytes_loaded").number(), 6);
}

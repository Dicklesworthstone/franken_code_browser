#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Read, Write}, path::PathBuf, process::Command,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData, SnapshotLimits};
use fcb_app::{run, EXIT_OK, EXIT_PARTIAL, EXIT_NO_MATCH, EXIT_ERROR, EXIT_CANCELED};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-refresh-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn path(&self, name: &str) -> PathBuf { self.0.join(name) }
    fn snapshot(&self, name: &str, entries: &[SnapshotEntry<'_>], complete: bool) {
        let owner = ArenaOwnerId::new(1).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(128 * 1024 * 1024)).unwrap();
        let encoded = SnapshotBytes::encode(owner, complete, "test-v1", entries, SnapshotLimits::default(), &budget,
            ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        fs::write(self.path(name), encoded.bytes()).unwrap();
    }
    fn build(&self) -> String {
        let args = vec!["snapshot".into(), "index".into(), "build".into(), self.path("old.fcbs").into_os_string(),
            "--output".into(), self.path("old.fcbi").into_os_string(), "--json".into()];
        let (exit, result, err) = invoke(&args, || false);
        assert_eq!(exit, EXIT_OK); assert!(err.is_empty()); result.get("index_digest").text().to_owned()
    }
    fn refresh(&self, pin: &str) -> Vec<OsString> {
        vec!["snapshot".into(), "index".into(), "refresh".into(), self.path("new.fcbs").into_os_string(),
            "--base".into(), self.path("old.fcbs").into_os_string(), "--index".into(), self.path("old.fcbi").into_os_string(),
            "--index-digest".into(), pin.into(), "--output".into(), self.path("new.fcbi").into_os_string(), "--json".into()]
    }
    fn search(&self, pin: &str, needle: &str) -> Vec<OsString> {
        vec!["snapshot".into(), "index".into(), "search".into(), self.path("new.fcbs").into_os_string(),
            "--index".into(), self.path("new.fcbi").into_os_string(), "--index-digest".into(), pin.into(),
            "--text".into(), needle.into(), "--json".into()]
    }
    fn catalog(&self, archive: &str, target: &str) -> String {
        let args = vec!["snapshot".into(), "catalog".into(), self.path(archive).into_os_string(),
            "--output".into(), self.path(target).into_os_string(), "--json".into()];
        let (exit, result, _) = invoke(&args, || false); assert_eq!(exit, EXIT_OK);
        result.get("catalog_digest").text().to_owned()
    }
    fn populate(&self) {
        self.snapshot("old.fcbs", &[entry(b"a", b"banana"), entry(b"same", b"xxxxxx"), entry(b"vanished", b"retired")], true);
        self.snapshot("new.fcbs", &[entry(b"a", b"banana"), entry(b"copy", b"banana"), entry(b"same", b"needle")], true);
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NoInput;
impl Read for NoInput { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("refresh must not read stdin") } }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json, Vec<u8>) {
    let mut out = Vec::new(); let mut err = Vec::new();
    let exit = run(args, &mut NoInput, &mut out, &mut err, canceled);
    let result = parse(&out).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&out)));
    assert_eq!(result.get("schema").text(), "fcb.cli/1");
    (exit, result, err)
}

#[test]
fn refresh_reuses_unchanged_and_duplicated_content_but_rebuilds_an_equal_length_edit() {
    let f = Fixture::new(); f.populate(); let prior_pin = f.build();
    let prior = fs::read(f.path("old.fcbi")).unwrap();
    let old_source = fs::read(f.path("old.fcbs")).unwrap();
    let new_source = fs::read(f.path("new.fcbs")).unwrap();
    let (exit, refreshed, err) = invoke(&f.refresh(&prior_pin), || false);
    assert_eq!(exit, EXIT_OK); assert!(err.is_empty());
    assert_eq!(refreshed.get("command").text(), "snapshot-index-refresh");
    assert_eq!(refreshed.get("effect").text(), "complete-file-sync-requested");
    assert_eq!(refreshed.get("reused_files").number(), 2);
    assert_eq!(refreshed.get("reused_source_bytes").number(), 12);
    assert_eq!(refreshed.get("rebuilt_files").number(), 1);
    assert_eq!(refreshed.get("member_payload_bytes_loaded").number(), 6);
    assert!(!refreshed.get("rename_inferred").flag());
    assert!(!refreshed.get("live_roots_accessed").flag());
    let new_pin = refreshed.get("index_digest").text();
    let (exit, result, _) = invoke(&f.search(new_pin, "needle"), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("hits").array().len(), 1);
    assert!(result.get("workspace_complete").flag());
    assert_eq!(invoke(&f.search(new_pin, "retired"), || false).0, EXIT_NO_MATCH);
    assert_eq!(fs::read(f.path("old.fcbi")).unwrap(), prior);
    assert_eq!(fs::read(f.path("old.fcbs")).unwrap(), old_source);
    assert_eq!(fs::read(f.path("new.fcbs")).unwrap(), new_source);
}

#[test]
fn independent_catalogs_avoid_both_body_scans_and_refresh_loads_only_changed_source() {
    let f = Fixture::new(); f.populate(); let pin = f.build();
    let old_catalog = f.catalog("old.fcbs", "old.fcbc");
    let new_catalog = f.catalog("new.fcbs", "new.fcbc");
    let mut args = f.refresh(&pin);
    args.extend(["--base-catalog".into(), f.path("old.fcbc").into_os_string(), "--base-catalog-digest".into(), old_catalog.into(),
        "--catalog".into(), f.path("new.fcbc").into_os_string(), "--catalog-digest".into(), new_catalog.into()]);
    let (exit, result, _) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result.get("base_archive_validation_bytes").number(), 56);
    assert_eq!(result.get("archive_validation_bytes").number(), 56);
    assert!(!result.get("base_archive_body_verified_on_open").flag());
    assert!(!result.get("archive_body_verified_on_open").flag());
    assert_eq!(result.get("new_segment_source_bytes_loaded").number(), 6);
    assert_eq!(result.get("loaded_members").number(), 1);
}

#[test]
fn all_unchanged_content_refreshes_with_zero_fresh_source_budget_and_a_new_snapshot_identity() {
    let f = Fixture::new();
    f.snapshot("old.fcbs", &[entry(b"old", b"banana")], true);
    f.snapshot("new.fcbs", &[entry(b"moved", b"banana"), entry(b"second-copy", b"banana")], true);
    let pin = f.build(); let mut args = f.refresh(&pin);
    args.extend([OsString::from("--max-source-bytes"), OsString::from("0")]);
    let (exit, result, _) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("reused_files").number(), 2);
    assert_eq!(result.get("member_payload_bytes_loaded").number(), 0);
    assert_eq!(result.get("index_build_source_bytes").number(), 0);
    assert_ne!(result.get("snapshot_digest").text(), result.get("base_snapshot_digest").text());
    assert_eq!(invoke(&f.search(result.get("index_digest").text(), "ana"), || false).1.get("hits").array().len(), 4);
}

#[test]
fn shrinking_quota_publishes_uncovered_rows_that_still_take_exact_scan_fallback() {
    let f = Fixture::new(); f.populate(); let pin = f.build(); let mut args = f.refresh(&pin);
    args.extend([OsString::from("--max-grams"), OsString::from("0")]);
    let (exit, result, _) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(result.get("indexed_files").number(), 0);
    assert_eq!(result.get("uncovered_files").number(), 3);
    let (exit, searched, _) = invoke(&f.search(result.get("index_digest").text(), "ana"), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(searched.get("hits").array().len(), 4);
    assert_eq!(searched.get("index_fallback_files").number(), 3);
}

#[test]
fn unavailable_new_members_do_not_resurrect_old_captures_or_complete_incomplete_discovery() {
    let f = Fixture::new(); f.snapshot("old.fcbs", &[entry(b"a", b"needle")], true);
    f.snapshot("new.fcbs", &[SnapshotEntry { path: b"a", observed_bytes: 6, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }], false);
    let pin = f.build(); let (exit, result, _) = invoke(&f.refresh(&pin), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(result.get("reused_files").number(), 0);
    assert_eq!(result.get("unavailable_files_count").number(), 1);
    let (exit, searched, _) = invoke(&f.search(result.get("index_digest").text(), "needle"), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!searched.get("workspace_complete").flag());
    assert!(searched.get("hits").array().is_empty());
}

#[test]
fn wrong_pin_or_wrong_base_creates_nothing_and_preserves_inputs() {
    let f = Fixture::new(); f.populate(); let pin = f.build();
    let original = fs::read(f.path("old.fcbi")).unwrap();
    let (exit, error, _) = invoke(&f.refresh(&"00".repeat(32)), || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error.get("error").get("code").text(), "SAVED_INDEX_PIN_MISMATCH");
    assert_eq!(error.get("effect").text(), "none"); assert!(!f.path("new.fcbi").exists());
    let mut wrong_base = f.refresh(&pin); wrong_base[5] = f.path("new.fcbs").into_os_string();
    let (exit, error, _) = invoke(&wrong_base, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error.get("error").get("code").text(), "SAVED_INDEX_SOURCE_MISMATCH");
    assert!(!f.path("new.fcbi").exists()); assert_eq!(fs::read(f.path("old.fcbi")).unwrap(), original);
}

#[test]
fn destination_refusal_and_cancellation_keep_prior_index_and_report_exact_write_effects() {
    let f = Fixture::new(); f.populate(); let pin = f.build();
    let original = fs::read(f.path("old.fcbi")).unwrap();
    let mut overwrite = f.refresh(&pin); overwrite[11] = f.path("old.fcbi").into_os_string();
    let (exit, error, _) = invoke(&overwrite, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error.get("error").get("code").text(), "SNAPSHOT_DESTINATION_EXISTS");
    let (exit, error, _) = invoke(&f.refresh(&pin), || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(error.get("effect").text(), "none");
    assert!(!f.path("new.fcbi").exists());
    let (exit, error, _) = invoke(&f.refresh(&pin), || f.path("new.fcbi").exists());
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(error.get("effect").text(), "destination-created-incomplete");
    assert_eq!(fs::metadata(f.path("new.fcbi")).unwrap().len(), 0);
    assert_eq!(fs::read(f.path("old.fcbi")).unwrap(), original);
}

#[test]
fn failed_response_delivery_does_not_remove_a_successfully_published_refresh() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::Error::from(io::ErrorKind::BrokenPipe)) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let f = Fixture::new(); f.populate(); let pin = f.build(); let mut err = Vec::new();
    assert_eq!(run(&f.refresh(&pin), &mut NoInput, &mut Broken, &mut err, || false), EXIT_ERROR);
    assert!(String::from_utf8(err).unwrap().contains("effect=complete-file-sync-requested"));
    assert!(fs::metadata(f.path("new.fcbi")).unwrap().len() > 0);
    assert!(fs::metadata(f.path("old.fcbi")).unwrap().len() > 0);
}

#[test]
fn actual_binary_refreshes_and_searches_the_new_generation() {
    let f = Fixture::new(); f.populate(); let pin = f.build();
    let output = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.refresh(&pin)).output().unwrap();
    assert_eq!(output.status.code(), Some(EXIT_OK as i32)); assert!(output.stderr.is_empty());
    let receipt = parse(&output.stdout).unwrap();
    assert_eq!(receipt.get("reused_files").number(), 2);
    let result = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.search(receipt.get("index_digest").text(), "needle")).output().unwrap();
    assert_eq!(result.status.code(), Some(EXIT_OK as i32));
    let result = parse(&result.stdout).unwrap();
    assert!(result.get("workspace_complete").flag()); assert_eq!(result.get("hits").array().len(), 1);
}

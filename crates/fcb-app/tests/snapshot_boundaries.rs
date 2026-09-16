#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_CANCELED};
use fcb::search::snapshot::{SnapshotLimits, SnapshotView};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-snapshot-boundaries-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); fs::create_dir(path.join("root")).unwrap(); Self(path)
    }
    fn saved(&self) -> PathBuf { self.0.join("saved.fcbs") }
    fn save(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "save".into(), self.0.join("root").into_os_string(), "--output".into(), self.saved().into_os_string(), "--json".into()]
    }
    fn search(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "search".into(), self.saved().into_os_string(), "--text".into(), "needle".into(), "--json".into()]
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let result = run(args, &mut &b""[..], &mut stdout, &mut stderr, canceled);
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    (result, parse(&stdout).unwrap())
}

#[test]
fn empty_workspace_is_exportable_and_a_complete_saved_negative_is_distinct_from_partial() {
    let fixture = Fixture::new();
    let (exit, saved) = invoke(&fixture.save(), || false);
    assert_eq!(exit, EXIT_OK); assert!(saved.get("live_roots_accessed").flag());
    assert_eq!(saved.get("known_files").number(), 0); assert!(saved.get("discovery_complete").flag());
    let (exit, result) = invoke(&fixture.search(), || false);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(result.get("workspace_complete").flag());
    assert!(!result.get("live_roots_accessed").flag()); assert!(result.get("hits").array().is_empty());
    let inspect = vec!["snapshot".into(), "inspect".into(), fixture.saved().into_os_string(), "--json".into()];
    let (exit, inspected) = invoke(&inspect, || false);
    assert_eq!(exit, EXIT_OK); assert!(!inspected.get("live_roots_accessed").flag());
}

#[test]
fn a_saved_discovery_cutoff_survives_restore_even_when_every_retained_capture_is_present() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("root/a"), b"needle").unwrap();
    fs::write(fixture.0.join("root/b"), b"needle").unwrap();
    let mut args = fixture.save(); args.extend([OsString::from("--max-files"), OsString::from("1")]);
    let (exit, saved) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!saved.get("discovery_complete").flag());
    assert_eq!(saved.get("known_files").number(), 1); assert_eq!(saved.get("unavailable_files_count").number(), 0);
    let (exit, result) = invoke(&fixture.search(), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!result.get("workspace_complete").flag());
    assert_eq!(result.get("hits").array().len(), 1);
}

#[test]
fn cancel_after_final_write_reports_valid_unsynced_file_not_no_effect() {
    let fixture = Fixture::new(); fs::write(fixture.0.join("root/a"), b"needle").unwrap();
    let (exit, result) = invoke(&fixture.save(), || fs::metadata(fixture.saved()).is_ok_and(|metadata| metadata.len() > 0));
    assert_eq!(exit, EXIT_CANCELED);
    assert_eq!(result.get("effect").text(), "complete-file-sync-unconfirmed");
    let bytes = fs::read(fixture.saved()).unwrap();
    assert!(SnapshotView::open(&bytes, SnapshotLimits::default(), || false).is_ok());
    assert_eq!(invoke(&fixture.search(), || false).0, EXIT_OK);
}

#[test]
fn cancel_before_admission_creates_nothing() {
    let fixture = Fixture::new();
    let (exit, result) = invoke(&fixture.save(), || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(result.get("effect").text(), "none");
    assert!(!fixture.saved().exists());
}

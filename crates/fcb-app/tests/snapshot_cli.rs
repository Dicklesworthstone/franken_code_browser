#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Read, Write}, path::PathBuf, process::Command,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED, EXIT_NO_MATCH};
use fcb::search::snapshot::{SnapshotLimits, SnapshotView};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-snapshot-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("root")).unwrap();
        Self(path)
    }
    fn root(&self) -> PathBuf { self.0.join("root") }
    fn archive(&self) -> PathBuf { self.0.join("source.fcbs") }
    fn save_args(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "save".into(), self.root().into_os_string(), "--output".into(), self.archive().into_os_string(), "--json".into()]
    }
    fn search_args(&self, text: &str) -> Vec<OsString> {
        vec!["snapshot".into(), "search".into(), self.archive().into_os_string(), "--text".into(), text.into(), "--json".into()]
    }
    fn inspect_args(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "inspect".into(), self.archive().into_os_string(), "--json".into()]
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NeverRead;
impl Read for NeverRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("snapshot command must never read stdin") }
}
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json, Vec<u8>) {
    let mut out = Vec::new(); let mut err = Vec::new();
    let exit = run(args, &mut NeverRead, &mut out, &mut err, canceled);
    let document = parse(&out).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&out)));
    assert_eq!(document.get("schema").text(), "fcb.cli/1");
    (exit, document, err)
}
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    bytes
}

#[test]
fn save_inspect_and_search_keep_old_source_when_original_namespace_is_replaced() {
    let fixture = Fixture::new();
    fs::write(fixture.root().join("a.rs"), b"old needle").unwrap();
    fs::write(fixture.root().join("b.rs"), utf16("banana")).unwrap();
    let (exit, saved, stderr) = invoke(&fixture.save_args(), || false);
    assert_eq!(exit, EXIT_OK); assert!(stderr.is_empty());
    assert_eq!(saved.get("effect").text(), "complete-file-sync-requested");
    assert!(saved.get("source_contains_plaintext").flag());
    assert!(!saved.get("power_loss_qualified").flag());
    assert_eq!(saved.get("captured_files").number(), 2);
    let digest = saved.get("snapshot_digest").text().to_owned();
    fs::rename(fixture.root(), fixture.0.join("retired-root")).unwrap();
    fs::create_dir(fixture.root()).unwrap();
    fs::write(fixture.root().join("a.rs"), b"different live file").unwrap();
    let (exit, found, stderr) = invoke(&fixture.search_args("needle"), || false);
    assert_eq!(exit, EXIT_OK); assert!(stderr.is_empty());
    assert_eq!(found.get("snapshot_digest").text(), digest);
    assert!(!found.get("live_roots_accessed").flag());
    assert!(found.get("workspace_complete").flag());
    assert_eq!(found.get("hits").array().len(), 1);
    assert_eq!(found.get("hits").array()[0].get("matched_text").text(), "needle");
    let (exit, utf16, _) = invoke(&fixture.search_args("ana"), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(utf16.get("hits").array().len(), 2);
    assert_eq!(utf16.get("hits").array()[0].get("original_range").get("start").number(), 4);
    assert_eq!(utf16.get("hits").array()[1].get("original_range").get("start").number(), 8);
    let (exit, inspected, _) = invoke(&fixture.inspect_args(), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(inspected.get("files").array().len(), 2);
    assert_eq!(inspected.get("snapshot_digest").text(), digest);
}

#[test]
fn existing_destination_is_never_overwritten_even_with_source_path_as_output() {
    let fixture = Fixture::new();
    fs::write(fixture.root().join("a.rs"), b"source must survive").unwrap();
    fs::write(fixture.archive(), b"existing destination must survive").unwrap();
    let (exit, result, _) = invoke(&fixture.save_args(), || false);
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(result.get("effect").text(), "none");
    assert_eq!(result.get("error").get("code").text(), "SNAPSHOT_DESTINATION_EXISTS");
    assert_eq!(fs::read(fixture.archive()).unwrap(), b"existing destination must survive");
    let mut args = fixture.save_args(); args[4] = fixture.root().join("a.rs").into_os_string();
    assert_eq!(invoke(&args, || false).0, EXIT_ERROR);
    assert_eq!(fs::read(fixture.root().join("a.rs")).unwrap(), b"source must survive");
}

#[test]
fn unavailable_captures_survive_disk_roundtrip_and_block_false_negative_completeness() {
    let fixture = Fixture::new();
    fs::write(fixture.root().join("empty.rs"), b"").unwrap();
    fs::write(fixture.root().join("missing.rs"), b"needle").unwrap();
    let mut save = fixture.save_args(); save.extend([OsString::from("--max-total-bytes"), OsString::from("0")]);
    let (exit, saved, _) = invoke(&save, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(saved.get("captured_files").number(), 1);
    assert_eq!(saved.get("unavailable_files_count").number(), 1);
    let (exit, inspected, _) = invoke(&fixture.inspect_args(), || false);
    assert_eq!(exit, EXIT_PARTIAL);
    let missing = inspected.get("files").array().iter().find(|entry| !entry.get("captured").flag()).unwrap();
    assert_eq!(missing.get("unavailable_reason").text(), "WORKSPACE_TOTAL_SOURCE_LIMIT");
    let (exit, result, _) = invoke(&fixture.search_args("needle"), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!result.get("workspace_complete").flag());
    assert!(result.get("hits").array().is_empty());
    assert_eq!(result.get("unavailable_files_count").number(), 1);
}

#[test]
fn corruption_and_truncation_fail_before_returning_saved_hits() {
    let fixture = Fixture::new(); fs::write(fixture.root().join("a"), b"needle").unwrap();
    assert_eq!(invoke(&fixture.save_args(), || false).0, EXIT_OK);
    let original = fs::read(fixture.archive()).unwrap();
    for broken in [original[..original.len() - 1].to_vec(), {
        let mut corrupt = original.clone(); corrupt[30] ^= 1; corrupt
    }] {
        fs::write(fixture.archive(), &broken).unwrap();
        let (exit, result, _) = invoke(&fixture.search_args("needle"), || false);
        assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("status").text(), "error");
        assert_eq!(result.get("effect").text(), "none");
        assert!(!result.get("complete").flag());
    }
}

#[test]
fn cancellation_after_exclusive_creation_reports_and_preserves_incomplete_destination() {
    let fixture = Fixture::new(); fs::write(fixture.root().join("a"), b"needle").unwrap();
    let (exit, result, _) = invoke(&fixture.save_args(), || fixture.archive().exists());
    assert_eq!(exit, EXIT_CANCELED);
    assert_eq!(result.get("effect").text(), "destination-created-incomplete");
    assert_eq!(fs::metadata(fixture.archive()).unwrap().len(), 0);
    let bytes = fs::read(fixture.archive()).unwrap();
    assert!(SnapshotView::open(&bytes, SnapshotLimits::default(), || false).is_err());
    // A second invocation cannot silently overwrite the interrupted artifact.
    assert_eq!(invoke(&fixture.save_args(), || false).1.get("error").get("code").text(), "SNAPSHOT_DESTINATION_EXISTS");
}

#[test]
fn lost_response_does_not_erase_a_completed_export_or_claim_no_effect() {
    struct BrokenOutput;
    impl Write for BrokenOutput {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::Error::from(io::ErrorKind::BrokenPipe)) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let fixture = Fixture::new(); fs::write(fixture.root().join("a"), b"needle").unwrap();
    let mut stderr = Vec::new();
    assert_eq!(run(&fixture.save_args(), &mut NeverRead, &mut BrokenOutput, &mut stderr, || false), EXIT_ERROR);
    assert!(String::from_utf8(stderr).unwrap().contains("effect=complete-file-sync-requested"));
    let bytes = fs::read(fixture.archive()).unwrap();
    let view = SnapshotView::open(&bytes, SnapshotLimits::default(), || false).unwrap();
    assert_eq!(view.captured_files(), 1);
    assert_eq!(invoke(&fixture.search_args("needle"), || false).0, EXIT_OK);
}

#[test]
fn raw_paths_roundtrip_and_new_archives_start_owner_private() {
    use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
    let fixture = Fixture::new();
    let name = OsString::from_vec(b"literal\\name\xff\n.rs".to_vec());
    fs::write(fixture.root().join(name), b"old needle").unwrap();
    assert_eq!(invoke(&fixture.save_args(), || false).0, EXIT_OK);
    assert_eq!(fs::metadata(fixture.archive()).unwrap().permissions().mode() & 0o077, 0);
    let (exit, result, _) = invoke(&fixture.search_args("needle"), || false);
    assert_eq!(exit, EXIT_OK);
    let expected = b"literal\\name\xff\n.rs".iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    assert_eq!(result.get("hits").array()[0].get("path").get("hex").text(), expected);
}

#[test]
fn streaming_save_reopens_an_archive_beyond_the_old_contiguous_limit() {
    const MEMBER: usize = 16 * 1024 * 1024;
    const FILES: usize = 6;
    let fixture = Fixture::new();
    // Build 96 MiB of source without a fixture-sized allocation. The production
    // save must fit its unchanged 256 MiB admission budget while retaining only
    // one 16 MiB member, plus bounded extent and metadata scratch.
    for ordinal in 0..FILES {
        let mut file = fs::File::create(fixture.root().join(format!("file-{ordinal}"))).unwrap();
        let chunk = [b'a' + ordinal as u8; 64 * 1024];
        for _ in 0..MEMBER / chunk.len() { file.write_all(&chunk).unwrap(); }
    }
    let mut args = fixture.save_args();
    args.extend(["--max-file-bytes".into(), MEMBER.to_string().into(),
        "--max-total-bytes".into(), (MEMBER * FILES).to_string().into()]);
    let (exit, saved, errors) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert!(errors.is_empty());
    assert_eq!(saved.get("captured_files").number(), FILES as u64);
    assert_eq!(saved.get("captured_bytes").number(), (MEMBER * FILES) as u64);
    assert_eq!(saved.get("payload_bytes_read").number(), (MEMBER * FILES) as u64);
    assert_eq!(saved.get("largest_captured_member_bytes").number(), MEMBER as u64);
    assert_eq!(saved.get("payload_residency").text(), "one-captured-member");
    assert_eq!(saved.get("archive_bytes").number(), fs::metadata(fixture.archive()).unwrap().len());
    assert!(saved.get("archive_bytes").number() > 80 * 1024 * 1024);
    assert_eq!(saved.get("archive_hash_bytes_read").number() + 32, saved.get("archive_bytes").number());
    fs::rename(fixture.root(), fixture.0.join("old-root")).unwrap();
    fs::create_dir(fixture.root()).unwrap(); fs::write(fixture.root().join("file-5"), b"replacement").unwrap();
    let (exit, inspected, _) = invoke(&fixture.inspect_args(), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(inspected.get("snapshot_digest"), saved.get("snapshot_digest"));
    assert_eq!(inspected.get("member_payload_bytes_loaded").number(), 0);
    assert!(!inspected.get("live_roots_accessed").flag());
    let catalog = fixture.0.join("source.fcbc");
    let (exit, pinned, _) = invoke(&["snapshot".into(), "catalog".into(), fixture.archive().into_os_string(),
        "--output".into(), catalog.clone().into_os_string(), "--json".into()], || false);
    assert_eq!(exit, EXIT_OK);
    let (exit, read, _) = invoke(&["snapshot".into(), "read".into(), fixture.archive().into_os_string(),
        "--member".into(), "file-5".into(), "--raw".into(), "--offset".into(), (MEMBER - 16).to_string().into(),
        "--bytes".into(), "16".into(), "--catalog".into(), catalog.into_os_string(),
        "--catalog-digest".into(), pinned.get("catalog_digest").text().into(), "--json".into()], || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(read.get("original_hex").text(), "66".repeat(16));
    assert_eq!(read.get("member_payload_bytes_loaded").number(), MEMBER as u64);
    assert!(!read.get("archive_body_verified_on_open").flag());
    assert!(!read.get("live_roots_accessed").flag());
}

#[test]
fn streaming_source_quotas_keep_unavailable_members_and_later_empty_captures() {
    let fixture = Fixture::new();
    fs::write(fixture.root().join("a-large"), vec![b'a'; 3 * 1024 * 1024]).unwrap();
    fs::write(fixture.root().join("b-over-total"), vec![b'b'; 2 * 1024 * 1024]).unwrap();
    fs::write(fixture.root().join("z-empty"), []).unwrap();
    let mut args = fixture.save_args();
    args.extend(["--max-file-bytes".into(), "33554432".into(), "--max-total-bytes".into(), "4194304".into()]);
    let (exit, saved, _) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(saved.get("known_files").number(), 3);
    assert_eq!(saved.get("captured_files").number(), 2); assert_eq!(saved.get("unavailable_files_count").number(), 1);
    assert_eq!(saved.get("payload_bytes_read").number(), 3 * 1024 * 1024);
    let (exit, inspected, _) = invoke(&fixture.inspect_args(), || false);
    assert_eq!(exit, EXIT_PARTIAL); let members = inspected.get("files").array();
    assert!(members[0].get("captured").flag());
    assert_eq!(members[1].get("unavailable_reason").text(), "WORKSPACE_TOTAL_SOURCE_LIMIT");
    assert!(members[2].get("captured").flag()); assert_eq!(members[2].get("observed_bytes").number(), 0);
}

#[test]
fn a_member_changed_after_discovery_is_unavailable_without_losing_other_members() {
    let fixture = Fixture::new(); let source = fixture.root().join("a-changing");
    fs::write(&source, b"before").unwrap(); fs::write(fixture.root().join("b-stable"), b"stable").unwrap();
    let mut changed = false;
    let (exit, saved, _) = invoke(&fixture.save_args(), || {
        if !changed && fixture.archive().exists() {
            fs::write(&source, b"changed length after discovery").unwrap(); changed = true;
        }
        false
    });
    assert!(changed); assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(saved.get("known_files").number(), 2); assert_eq!(saved.get("captured_files").number(), 1);
    let (exit, inspected, _) = invoke(&fixture.inspect_args(), || false);
    assert_eq!(exit, EXIT_PARTIAL); let members = inspected.get("files").array();
    assert_eq!(members[0].get("unavailable_reason").text(), "SOURCE_METADATA_MISMATCH");
    assert!(members[1].get("captured").flag());
}

#[test]
fn actual_binary_can_search_a_snapshot_after_the_source_root_is_moved() {
    let fixture = Fixture::new(); fs::write(fixture.root().join("a.rs"), b"banana").unwrap();
    let saved = Command::new(env!("CARGO_BIN_EXE_fcb")).args(fixture.save_args()).output().unwrap();
    assert_eq!(saved.status.code(), Some(EXIT_OK as i32));
    assert_eq!(parse(&saved.stdout).unwrap().get("command").text(), "snapshot-save");
    fs::rename(fixture.root(), fixture.0.join("gone-root")).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fcb")).args(fixture.search_args("ana")).output().unwrap();
    assert_eq!(result.status.code(), Some(EXIT_OK as i32));
    assert_eq!(parse(&result.stdout).unwrap().get("hits").array().len(), 2);
    let absent = Command::new(env!("CARGO_BIN_EXE_fcb")).args(fixture.search_args("absent")).output().unwrap();
    assert_eq!(absent.status.code(), Some(EXIT_NO_MATCH as i32));
    assert!(parse(&absent.stdout).unwrap().get("workspace_complete").flag());
}

#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Write}, path::{Path, PathBuf}, process::Command,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-postings-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn path(&self, name: &str) -> PathBuf { self.0.join(name) }
    fn snapshot(&self, name: &str, entries: &[SnapshotEntry<'_>], closed: bool) -> PathBuf {
        let owner = ArenaOwnerId::new(1).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
        let bytes = SnapshotBytes::encode(owner, closed, "posting-cli-test-v1", entries, SnapshotLimits::default(),
            &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        let path = self.path(name); fs::write(&path, bytes.bytes()).unwrap(); path
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn build_args(archive: &Path, output: &Path, inverted: bool) -> Vec<OsString> {
    let mut args = vec!["snapshot".into(), "index".into(), "build".into(), archive.into(),
        "--output".into(), output.into(), "--json".into()];
    if inverted { args.push("--inverted".into()); } args
}
fn search_args(archive: &Path, index: &Path, pin: &str, pattern: &str) -> Vec<OsString> {
    vec!["snapshot".into(), "index".into(), "search".into(), archive.into(), "--index".into(), index.into(),
        "--index-digest".into(), pin.into(), "--text".into(), pattern.into(), "--json".into()]
}
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut out = Vec::new(); let mut err = Vec::new();
    let exit = run(args, &mut &b""[..], &mut out, &mut err, canceled);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    let json = parse(&out).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&out)));
    assert_eq!(json.get("schema").text(), "fcb.cli/1");
    (exit, json)
}
fn pin(result: &Json) -> String { result.get("index_digest").text().to_owned() }
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); } bytes
}

#[test]
fn build_inspect_search_dispatch_both_layouts_and_expose_actual_candidate_work() {
    let fixture = Fixture::new();
    let names: Vec<_> = (0..128).map(|i| format!("file-{i:03}")).collect();
    let entries: Vec<_> = names.iter().enumerate().map(|(i, name)| entry(name.as_bytes(),
        if i == 127 { b"aaaaZ9" } else { b"aaa common" })).collect();
    let archive = fixture.snapshot("saved.fcbs", &entries, true);
    let original = fs::read(&archive).unwrap();
    for inverted in [false, true] {
        let index = fixture.path(if inverted { "global.fcbo" } else { "legacy.fcbi" });
        let (exit, built) = invoke(&build_args(&archive, &index, inverted), || false);
        assert_eq!(exit, EXIT_OK);
        assert_eq!(built.get("index_layout").text(), if inverted { "global-postings-v1" } else { "per-member-grams-v1" });
        let pin = pin(&built);
        let (exit, found) = invoke(&search_args(&archive, &index, &pin, "aaaaZ9"), || false);
        assert_eq!(exit, EXIT_OK); assert!(found.get("workspace_complete").flag());
        assert_eq!(found.get("hits").array().len(), 1);
        assert_eq!(found.get("metadata_members_visited").number(), if inverted { 1 } else { 128 });
        assert_eq!(found.get("index_eliminated_files").number(), 127);
        assert_eq!(found.get("loaded_members").number(), 1);
        assert_eq!(found.get("member_payload_bytes_loaded").number(), 6);
        if inverted {
            assert_eq!(found.get("posting_entries_visited").number(), 1);
            assert_eq!(found.get("posting_list_lookups").number(), 3);
            assert!(found.get("posting_cursor_complete").flag());
        }
        let args = vec!["snapshot".into(), "index".into(), "inspect".into(), archive.as_os_str().to_owned(),
            "--index".into(), index.as_os_str().to_owned(), "--index-digest".into(), pin.into(), "--json".into()];
        let (exit, inspected) = invoke(&args, || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(inspected.get("indexed_files").number(), 128);
        assert_eq!(inspected.get("member_payload_bytes_loaded").number(), 0);
    }
    assert_eq!(fs::read(archive).unwrap(), original);
}

#[test]
fn catalog_and_postings_skip_unselected_payloads_but_reject_a_changed_selected_member() {
    let fixture = Fixture::new();
    let archive = fixture.snapshot("saved.fcbs", &[entry(b"a", b"UNRELATED_ARCHIVE_MEMBER"), entry(b"b", b"needle witness")], true);
    let index = fixture.path("global.fcbo");
    let index_pin = pin(&invoke(&build_args(&archive, &index, true), || false).1);
    let catalog = fixture.path("directory.fcbc");
    let (_, receipt) = invoke(&["snapshot".into(), "catalog".into(), archive.as_os_str().to_owned(),
        "--output".into(), catalog.as_os_str().to_owned(), "--json".into()], || false);
    let catalog_pin = receipt.get("catalog_digest").text().to_owned();
    let mut bytes = fs::read(&archive).unwrap();
    let unselected = bytes.windows(b"UNRELATED_ARCHIVE_MEMBER".len()).position(|s| s == b"UNRELATED_ARCHIVE_MEMBER").unwrap();
    bytes[unselected] ^= 1; fs::write(&archive, &bytes).unwrap();
    let mut args = search_args(&archive, &index, &index_pin, "needle");
    args.extend(["--catalog".into(), catalog.as_os_str().to_owned(), "--catalog-digest".into(), catalog_pin.into()]);
    let (exit, result) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("archive_validation_bytes").number(), 56);
    assert!(!result.get("archive_body_verified_on_open").flag());
    assert_eq!(result.get("metadata_members_visited").number(), 1);
    assert_eq!(result.get("loaded_members").number(), 1);
    let selected = bytes.windows(b"needle witness".len()).position(|s| s == b"needle witness").unwrap();
    bytes[selected] ^= 1; fs::write(&archive, &bytes).unwrap();
    let (exit, result) = invoke(&args, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("status").text(), "error");
}

#[test]
fn utf16_raw_mode_and_short_queries_keep_the_existing_fallback_and_range_contracts() {
    let fixture = Fixture::new(); let le = utf16("banana");
    let archive = fixture.snapshot("saved.fcbs", &[entry(b"a", b"banana"), entry(b"b", &le)], true);
    let index = fixture.path("global.fcbo");
    let pin = pin(&invoke(&build_args(&archive, &index, true), || false).1);
    let (exit, found) = invoke(&search_args(&archive, &index, &pin, "ana"), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(found.get("hits").array().len(), 4);
    assert_eq!(found.get("index_fallback_files").number(), 1);
    let hits = found.get("hits").array();
    assert_eq!(hits[0].get("original_range").get("start").number(), 1);
    assert_eq!(hits[2].get("original_range").get("start").number(), 4);
    let (exit, short) = invoke(&search_args(&archive, &index, &pin, "a"), || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(short.get("index_fallback_files").number(), 2);
    let args = vec!["snapshot".into(), "index".into(), "search".into(), archive.into_os_string(), "--index".into(), index.into_os_string(),
        "--index-digest".into(), pin.into(), "--raw-hex".into(), "61006e00".into(), "--json".into()];
    let (exit, raw) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(raw.get("hits").array().len(), 2);
    assert_eq!(raw.get("index_fallback_files").number(), 0);
}

#[test]
fn zero_gram_budget_keeps_all_members_searchable_and_reports_partial_index_not_partial_query() {
    let fixture = Fixture::new();
    let archive = fixture.snapshot("saved.fcbs", &[entry(b"a", b"needle"), entry(b"b", b"unrelated")], true);
    let index = fixture.path("empty-postings.fcbo");
    let mut args = build_args(&archive, &index, true); args.extend(["--max-grams".into(), "0".into()]);
    let (exit, built) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(built.get("uncovered_files").number(), 2);
    let (exit, found) = invoke(&search_args(&archive, &index, &pin(&built), "needle"), || false);
    assert_eq!(exit, EXIT_OK); assert!(found.get("workspace_complete").flag());
    assert_eq!(found.get("index_fallback_files").number(), 2); assert_eq!(found.get("hits").array().len(), 1);
}

#[test]
fn refresh_can_read_an_inverted_prior_and_publish_either_layout_without_overwriting_inputs() {
    let fixture = Fixture::new();
    let old = fixture.snapshot("old.fcbs", &[entry(b"a", b"needle old"), entry(b"b", b"banana")], true);
    let target = fixture.snapshot("target.fcbs", &[entry(b"b", b"banana"), entry(b"c", b"needle new")], true);
    let prior = fixture.path("prior.fcbo");
    let pin = pin(&invoke(&build_args(&old, &prior, true), || false).1);
    let original = fs::read(&prior).unwrap();
    for inverted in [false, true] {
        let output = fixture.path(if inverted { "new.fcbo" } else { "new.fcbi" });
        let mut args = vec!["snapshot".into(), "index".into(), "refresh".into(), target.as_os_str().to_owned(),
            "--base".into(), old.as_os_str().to_owned(), "--index".into(), prior.as_os_str().to_owned(),
            "--index-digest".into(), pin.clone().into(), "--output".into(), output.as_os_str().to_owned(), "--json".into()];
        if inverted { args.push("--inverted".into()); }
        let (exit, refreshed) = invoke(&args, || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(refreshed.get("reused_files").number(), 1);
        let new_pin = refreshed.get("index_digest").text();
        let (exit, found) = invoke(&search_args(&target, &output, new_pin, "needle"), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(found.get("hits").array().len(), 1);
        assert_eq!(found.get("hits").array()[0].get("file_id").number(), 2);
    }
    assert_eq!(fs::read(prior).unwrap(), original);
}

#[test]
fn forged_pins_corruption_and_missing_members_never_become_successful_negatives() {
    let fixture = Fixture::new();
    let archive = fixture.snapshot("saved.fcbs", &[entry(b"a", b"needle"), SnapshotEntry {
        path: b"missing", observed_bytes: 100, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }], true);
    let index = fixture.path("global.fcbo");
    let (exit, built) = invoke(&build_args(&archive, &index, true), || false);
    assert_eq!(exit, EXIT_PARTIAL);
    let pin = pin(&built);
    let (exit, result) = invoke(&search_args(&archive, &index, &pin, "absent"), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!result.get("workspace_complete").flag());
    assert_eq!(result.get("unavailable_files_count").number(), 1);
    assert_eq!(invoke(&search_args(&archive, &index, &"00".repeat(32), "absent"), || false).0, EXIT_ERROR);
    let mut bytes = fs::read(&index).unwrap(); bytes[80] ^= 1; fs::write(&index, bytes).unwrap();
    assert_eq!(invoke(&search_args(&archive, &index, &pin, "absent"), || false).0, EXIT_ERROR);
}

#[test]
fn publication_refuses_overwrite_and_preserves_interrupted_new_output() {
    let fixture = Fixture::new(); let archive = fixture.snapshot("saved.fcbs", &[entry(b"a", b"needle")], true);
    let output = fixture.path("global.fcbo"); let args = build_args(&archive, &output, true);
    let (exit, result) = invoke(&args, || output.exists());
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(result.get("effect").text(), "destination-created-incomplete");
    assert_eq!(fs::metadata(&output).unwrap().len(), 0);
    assert_eq!(invoke(&args, || false).0, EXIT_ERROR);
    assert_eq!(fs::metadata(&output).unwrap().len(), 0);
}

#[test]
fn lost_receipt_does_not_erase_a_complete_posting_artifact() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::BrokenPipe.into()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let fixture = Fixture::new(); let archive = fixture.snapshot("saved.fcbs", &[entry(b"a", b"needle")], true);
    let output = fixture.path("global.fcbo"); let args = build_args(&archive, &output, true);
    let mut err = Vec::new();
    assert_eq!(run(&args, &mut &b""[..], &mut Broken, &mut err, || false), EXIT_ERROR);
    assert!(String::from_utf8(err).unwrap().contains("effect=complete-file-sync-requested"));
    assert!(fs::read(output).unwrap().starts_with(b"FCBO"));
}

#[test]
fn actual_binary_builds_and_queries_a_global_index_after_the_source_tree_is_gone() {
    let fixture = Fixture::new(); let root = fixture.path("root"); fs::create_dir(&root).unwrap();
    fs::write(root.join("a.rs"), b"needle").unwrap();
    let saved = fixture.path("saved.fcbs");
    let save = Command::new(env!("CARGO_BIN_EXE_fcb")).args([OsString::from("snapshot"), "save".into(), root.as_os_str().to_owned(),
        "--output".into(), saved.as_os_str().to_owned(), "--json".into()]).output().unwrap();
    assert_eq!(save.status.code(), Some(EXIT_OK as i32));
    let index = fixture.path("global.fcbo");
    let built = Command::new(env!("CARGO_BIN_EXE_fcb")).args(build_args(&saved, &index, true)).output().unwrap();
    assert_eq!(built.status.code(), Some(EXIT_OK as i32));
    let pin = pin(&parse(&built.stdout).unwrap());
    fs::rename(root, fixture.path("retired-root")).unwrap();
    let found = Command::new(env!("CARGO_BIN_EXE_fcb")).args(search_args(&saved, &index, &pin, "needle")).output().unwrap();
    assert_eq!(found.status.code(), Some(EXIT_OK as i32));
    assert_eq!(parse(&found.stdout).unwrap().get("hits").array().len(), 1);
    let absent = Command::new(env!("CARGO_BIN_EXE_fcb")).args(search_args(&saved, &index, &pin, "absent")).output().unwrap();
    assert_eq!(absent.status.code(), Some(EXIT_NO_MATCH as i32));
    let document = parse(&absent.stdout).unwrap();
    assert_eq!(document.get("metadata_members_visited").number(), 0);
    assert_eq!(document.get("loaded_members").number(), 0);
}

#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Read, Write}, path::PathBuf, process::Command,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use support::{parse, Json};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-diff-{}-{nanos}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn before(&self) -> PathBuf { self.0.join("before.fcbs") }
    fn after(&self) -> PathBuf { self.0.join("after.fcbs") }
    fn write(&self, before: bool, entries: &[SnapshotEntry<'_>], complete: bool, policy: &str) {
        let owner = ArenaOwnerId::new(1).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(32 * 1024 * 1024)).unwrap();
        let encoded = SnapshotBytes::encode(owner, complete, policy, entries, SnapshotLimits::default(),
            &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        fs::write(if before { self.before() } else { self.after() }, encoded.bytes()).unwrap();
    }
    fn args(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "diff".into(), self.before().into_os_string(), self.after().into_os_string(), "--json".into()]
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
struct NoStdin;
impl Read for NoStdin { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("comparison cannot read stdin") } }
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let code = run(args, &mut NoStdin, &mut stdout, &mut stderr, canceled);
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    let document = parse(&stdout).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&stdout)));
    assert_eq!(document.get("schema").text(), "fcb.cli/1");
    (code, document)
}
fn extra(fixture: &Fixture, flags: &[&str]) -> Vec<OsString> {
    let mut args = fixture.args(); args.extend(flags.iter().map(OsString::from)); args
}

#[test]
fn real_comparison_reports_membership_and_byte_edits_without_live_paths() {
    let fixture = Fixture::new();
    fixture.write(true, &[entry(b"a.rs", b"same"), entry(b"b.rs", b"head old tail"), entry(b"deleted.rs", b"gone")], true, "same");
    fixture.write(false, &[entry(b"a.rs", b"same"), entry(b"b.rs", b"head new tail"), entry(b"created.rs", b"new")], true, "same");
    let before = fs::read(fixture.before()).unwrap(); let after = fs::read(fixture.after()).unwrap();
    let (code, result) = invoke(&fixture.args(), || false);
    assert_eq!(code, 1); assert!(result.get("complete").flag());
    assert!(!result.get("live_roots_accessed").flag()); assert!(!result.get("history_inferred").flag());
    assert_eq!(result.get("unchanged").number(), 1); assert_eq!(result.get("changed").number(), 1);
    assert_eq!(result.get("added").number(), 1); assert_eq!(result.get("removed").number(), 1);
    let changed = result.get("changes").array().iter().find(|change| change.get("kind").text() == "changed").unwrap();
    assert_eq!(changed.get("refinement").text(), "exact"); assert!(changed.get("edit_distance").number() > 0);
    assert!(changed.get("spans").array().iter().any(|span| span.get("kind").text() == "equal"));
    assert_eq!(fs::read(fixture.before()).unwrap(), before); assert_eq!(fs::read(fixture.after()).unwrap(), after);
}

#[test]
fn returned_spans_reconstruct_utf16_original_bytes_and_not_utf8_positions() {
    fn utf16(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xfe]; for unit in text.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); } bytes
    }
    let fixture = Fixture::new(); let old = utf16("header\r\nold 😀 tail"); let new = utf16("header\r\nnew 😀 tail");
    fixture.write(true, &[entry(b"a", &old)], true, "same"); fixture.write(false, &[entry(b"a", &new)], true, "same");
    let (code, result) = invoke(&fixture.args(), || false);
    assert_eq!(code, 1); assert_eq!(result.get("granularity").text(), "original-bytes");
    let change = &result.get("changes").array()[0];
    let (mut a, mut b) = (0usize, 0usize); let mut reconstructed = Vec::new();
    for span in change.get("spans").array() {
        let before = span.get("before"); let after = span.get("after");
        assert_eq!(before.get("start").number() as usize, a); assert_eq!(after.get("start").number() as usize, b);
        let end_a = before.get("end").number() as usize; let end_b = after.get("end").number() as usize;
        if span.get("kind").text() == "equal" { assert_eq!(&old[a..end_a], &new[b..end_b]); reconstructed.extend_from_slice(&old[a..end_a]); }
        else { reconstructed.extend_from_slice(&new[b..end_b]); }
        a = end_a; b = end_b;
    }
    assert_eq!((a, b), (old.len(), new.len())); assert_eq!(reconstructed, new);
}

#[test]
fn unchanged_and_empty_archives_need_no_member_payload_reload() {
    let fixture = Fixture::new();
    for entries in [vec![], vec![entry(b"empty", b""), entry(b"same", b"unchanged")]] {
        fixture.write(true, &entries, true, "same"); fixture.write(false, &entries, true, "same");
        let (code, result) = invoke(&fixture.args(), || false);
        assert_eq!(code, EXIT_OK); assert!(result.get("complete").flag());
        assert!(result.get("changes").array().is_empty()); assert_eq!(result.get("member_payload_bytes_loaded").number(), 0);
        assert!(result.get("archive_validation_bytes").number() > 0);
    }
}

#[test]
fn missing_capture_and_discovery_cutoff_are_not_deletion_or_empty_file_evidence() {
    let fixture = Fixture::new();
    fixture.write(true, &[SnapshotEntry { path: b"missing", observed_bytes: 0,
        data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }], false, "same");
    fixture.write(false, &[entry(b"added-or-undiscovered", b"x"), entry(b"missing", b"")], true, "same");
    let (code, result) = invoke(&fixture.args(), || false);
    assert_eq!(code, EXIT_PARTIAL); assert!(!result.get("content_evidence_complete").flag());
    assert_eq!(result.get("added").number(), 0); assert_eq!(result.get("only_after").number(), 1);
    assert_eq!(result.get("unavailable").number(), 1);
    assert_eq!(result.get("changes").array()[1].get("before").get("unavailable_reason").text(), "SOURCE_UNAVAILABLE");
}

#[test]
fn changed_exclusion_policy_keeps_single_sided_members_uncertain() {
    let fixture = Fixture::new(); fixture.write(true, &[entry(b"before", b"x")], true, "exclusions-v1");
    fixture.write(false, &[entry(b"after", b"x")], true, "include-all-v1");
    let (code, result) = invoke(&fixture.args(), || false);
    assert_eq!(code, EXIT_PARTIAL); assert!(!result.get("membership_comparable").flag());
    assert_eq!(result.get("added").number(), 0); assert_eq!(result.get("removed").number(), 0);
    assert_eq!(result.get("only_before").number(), 1); assert_eq!(result.get("only_after").number(), 1);
}

#[test]
fn byte_work_edit_and_output_limits_have_separate_truthful_outcomes() {
    let fixture = Fixture::new();
    fixture.write(true, &[entry(b"a", b"prefix old suffix"), entry(b"b", b"old")], true, "same");
    fixture.write(false, &[entry(b"a", b"prefix new suffix"), entry(b"b", b"new")], true, "same");
    let (code, result) = invoke(&extra(&fixture, &["--max-work", "0"]), || false);
    assert_eq!(code, EXIT_PARTIAL); assert!(result.get("content_evidence_complete").flag());
    assert!(!result.get("detail_complete").flag()); assert_eq!(result.get("diff_work_units").number(), 0);
    assert_eq!(result.get("member_payload_bytes_loaded").number(), 0);
    assert_eq!(result.get("changes").array()[0].get("spans").array()[0].get("kind").text(), "unresolved");
    let (code, result) = invoke(&extra(&fixture, &["--max-edits", "0"]), || false);
    assert_eq!(code, EXIT_PARTIAL); assert_eq!(result.get("changes").array()[0].get("refinement").text(), "edit-limit");
    let (code, result) = invoke(&extra(&fixture, &["--limit", "1"]), || false);
    assert_eq!(code, EXIT_PARTIAL); assert!(result.get("listing_truncated").flag());
    assert_eq!(result.get("changed").number(), 2); assert_eq!(result.get("omitted_changes").number(), 1);
    let (code, result) = invoke(&extra(&fixture, &["--hunks", "1"]), || false);
    assert_eq!(code, EXIT_PARTIAL); assert!(result.get("changes").array()[0].get("spans_truncated").flag());
}

#[test]
fn raw_path_identity_survives_json_and_equal_bytes_do_not_infer_a_rename() {
    let fixture = Fixture::new();
    fixture.write(true, &[entry(b"A", b"same"), entry(b"bad\xff\\name\n", b"old")], true, "same");
    fixture.write(false, &[entry(b"a", b"same"), entry(b"bad\xff\\name\n", b"new")], true, "same");
    let (code, result) = invoke(&fixture.args(), || false);
    assert_eq!(code, 1); assert_eq!(result.get("added").number(), 1); assert_eq!(result.get("removed").number(), 1);
    let changed = result.get("changes").array().iter().find(|c| c.get("kind").text() == "changed").unwrap();
    let expected = b"bad\xff\\name\n".iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(changed.get("path").get("hex").text(), expected);
    assert!(!changed.get("path").get("display").text().contains('\n'));
}

#[test]
fn corruption_and_cancellation_publish_one_error_and_no_source_edits() {
    let fixture = Fixture::new(); fixture.write(true, &[entry(b"a", b"old")], true, "same");
    fixture.write(false, &[entry(b"a", b"new")], true, "same");
    let before = fs::read(fixture.before()).unwrap(); let original_after = fs::read(fixture.after()).unwrap();
    let mut broken = original_after.clone(); let last = broken.len() - 1; broken[last] ^= 1;
    fs::write(fixture.after(), &broken).unwrap();
    let (code, result) = invoke(&fixture.args(), || false);
    assert_eq!(code, EXIT_ERROR); assert_eq!(result.get("status").text(), "error"); assert!(!result.get("complete").flag());
    fs::write(fixture.after(), &original_after).unwrap();
    let (code, result) = invoke(&fixture.args(), || true);
    assert_eq!(code, EXIT_CANCELED); assert_eq!(result.get("effect").text(), "none");
    assert_eq!(fs::read(fixture.before()).unwrap(), before); assert_eq!(fs::read(fixture.after()).unwrap(), original_after);
}

#[test]
fn parser_refuses_unbounded_or_duplicate_controls_before_opening_inputs() {
    let fixture = Fixture::new(); // No archive files exist: argument errors must win.
    for flags in [vec!["--max-edits", "513"], vec!["--max-work", "67108865"], vec!["--hunks", "0"],
        vec!["--limit", "4097"], vec!["--limit", "1", "--limit", "2"], vec!["--output", "would-be-write"]] {
        let (code, result) = invoke(&extra(&fixture, &flags), || false);
        assert_eq!(code, EXIT_ERROR); assert_eq!(result.get("effect").text(), "none");
        assert!(result.get("error").get("code").text().starts_with("CLI_"));
    }
}

#[test]
fn broken_delivery_does_not_append_another_document_or_modify_archives() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::BrokenPipe.into()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let fixture = Fixture::new(); fixture.write(true, &[entry(b"a", b"old")], true, "same");
    fixture.write(false, &[entry(b"a", b"new")], true, "same");
    let mut stderr = Vec::new();
    assert_eq!(run(&fixture.args(), &mut NoStdin, &mut Broken, &mut stderr, || false), EXIT_ERROR);
    assert_eq!(stderr, b"SNAPSHOT_DIFF_RESPONSE_INCOMPLETE effect=none\n");
}

#[test]
fn actual_binary_compares_captured_source_without_an_original_repository() {
    let fixture = Fixture::new(); fixture.write(true, &[entry(b"src/file.rs", b"fn before() {}")], true, "same");
    fixture.write(false, &[entry(b"src/file.rs", b"fn after() {}")], true, "same");
    let output = Command::new(env!("CARGO_BIN_EXE_fcb")).args(fixture.args()).output().unwrap();
    assert_eq!(output.status.code(), Some(1)); assert!(output.stderr.is_empty());
    let result = parse(&output.stdout).unwrap(); assert_eq!(result.get("command").text(), "snapshot-diff");
    assert!(result.get("complete").flag()); assert!(!result.get("live_roots_accessed").flag());
    assert_eq!(result.get("changes").array()[0].get("refinement").text(), "exact");
}

#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Read, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-references-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); fs::create_dir(path.join("root")).unwrap(); Self(path)
    }
    fn root(&self) -> PathBuf { self.0.join("root") }
    fn file(&self, path: &str, bytes: &[u8]) { fs::write(self.root().join(path), bytes).unwrap(); }
    fn file_args(&self, path: &str, name: &str) -> Vec<OsString> {
        vec!["symbols".into(), self.root().join(path).into_os_string(), "--references".into(), "--name".into(), name.into(), "--json".into()]
    }
    fn workspace_args(&self, name: &str) -> Vec<OsString> {
        vec!["symbols".into(), self.root().into_os_string(), "--workspace".into(), "--references".into(), "--name".into(), name.into(), "--json".into()]
    }
    fn archive(&self) -> PathBuf { self.0.join("source.fcbs") }
    fn save(&self) {
        let args = vec!["snapshot".into(), "save".into(), self.root().into_os_string(), "--output".into(), self.archive().into_os_string(), "--json".into()];
        assert_eq!(invoke(&args, || false).0, EXIT_OK);
    }
    fn saved_args(&self, name: &str) -> Vec<OsString> {
        vec!["symbols".into(), self.archive().into_os_string(), "--snapshot".into(), "--references".into(), "--name".into(), name.into(), "--json".into()]
    }
}
// Follow the neighboring integration fixtures: only this test's unique tree is
// ever cleaned up; repository or user-selected paths never participate.
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NeverRead;
impl Read for NeverRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("references must not implicitly read stdin") }
}
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let exit = run(args, &mut NeverRead, &mut stdout, &mut stderr, canceled);
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    let json = parse(&stdout).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&stdout)));
    assert_eq!(json.get("schema").text(), "fcb.cli/1");
    (exit, json)
}
fn values(args: &mut Vec<OsString>, values: &[&str]) { args.extend(values.iter().map(OsString::from)); }
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}

#[test]
fn references_are_exact_text_evidence_not_declaration_or_compiler_claims() {
    let fixture = Fixture::new();
    let bytes = b"foo(); foobar(); _foo; $foo;\n// foo\n\"foo\"\nfoo";
    fixture.file("source.rs", bytes);
    let (exit, json) = invoke(&fixture.file_args("source.rs", "foo"), || false);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(json.get("operation").text(), "references");
    assert!(json.get("text_candidates_complete").flag());
    assert!(json.get("candidate_count_complete").flag());
    assert!(json.get("comments_and_literals_included").flag());
    assert!(!json.get("compiler_resolved").flag());
    assert!(!json.get("reference_inventory_complete").flag());
    assert_eq!(json.get("candidates").array().len(), 4);
    for hit in json.get("candidates").array() {
        assert_eq!(hit.get("kind").text(), "reference-candidate");
        assert_eq!(hit.get("evidence").text(), "whole-token-text-candidate");
        let range = hit.get("original_range");
        assert_eq!(&bytes[range.get("start").number() as usize..range.get("end").number() as usize], b"foo");
    }
    assert_eq!(fs::read(fixture.root().join("source.rs")).unwrap(), bytes);
}

#[test]
fn all_text_extensions_participate_without_an_outline_language() {
    let fixture = Fixture::new();
    fixture.file("a.rs", b"fn work() {} work();");
    fixture.file("b.txt", b"work"); fixture.file("c.md", b"# work\n");
    let (exit, json) = invoke(&fixture.workspace_args("work"), || false);
    assert_eq!(exit, EXIT_OK);
    assert!(json.get("processing_complete").flag());
    assert_eq!(json.get("stats").get("unsupported_language_files").number(), 0);
    assert_eq!(json.get("stats").get("analyzed_files").number(), 3);
    assert_eq!(json.get("candidates").array().len(), 4);
    let files: Vec<_> = json.get("candidates").array().iter().map(|hit| hit.get("file_id").number()).collect();
    assert_eq!(files[0], files[1]); assert_ne!(files[1], files[2]); assert_ne!(files[2], files[3]);
}

#[test]
fn utf8_bom_and_utf16_hits_keep_original_byte_ranges() {
    let fixture = Fixture::new(); let text = "// 😀\r\nfoo foo_ foo";
    for (i, bytes) in [utf16(text, true), utf16(text, false), [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat()].into_iter().enumerate() {
        let path = format!("source-{i}.txt"); fixture.file(&path, &bytes);
        let (exit, json) = invoke(&fixture.file_args(&path, "foo"), || false);
        assert_eq!(exit, EXIT_OK); assert_eq!(json.get("candidates").array().len(), 2);
        for hit in json.get("candidates").array() {
            assert_eq!(hit.get("line").number(), 2);
            let range = hit.get("original_range");
            let expected = if i < 2 { utf16("foo", i == 0)[2..].to_vec() } else { b"foo".to_vec() };
            assert_eq!(&bytes[range.get("start").number() as usize..range.get("end").number() as usize], expected);
        }
    }
}

#[test]
fn declared_bomless_utf16_is_searched_under_the_requested_decoder() {
    let fixture = Fixture::new();
    let bytes: Vec<_> = "foo foo_ foo".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fixture.file("bomless.txt", &bytes);
    let mut args = fixture.file_args("bomless.txt", "foo"); values(&mut args, &["--encoding", "utf16le"]);
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert_eq!(json.get("candidates").array().len(), 2);
    assert_eq!(json.get("candidates").array()[0].get("original_range").get("start").number(), 0);
}

#[test]
fn exact_cap_is_complete_but_extra_occurrences_are_explicitly_partial() {
    let fixture = Fixture::new(); fixture.file("source.rs", b"foo foo");
    let mut args = fixture.file_args("source.rs", "foo"); values(&mut args, &["--limit", "2"]);
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert!(!json.get("listing_truncated").flag());
    fixture.file("source.rs", b"foo foo foo foo");
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(json.get("listing_truncated").flag());
    assert!(!json.get("candidate_count_complete").flag());
    assert!(!json.get("text_candidates_complete").flag());
    assert_eq!(json.get("candidates").array().len(), 2);
    assert_eq!(json.get("stats").get("matching_candidates_seen").number(), 3);
}

#[test]
fn global_cap_checks_later_files_instead_of_claiming_a_complete_prefix() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"foo"); fixture.file("b.rs", b"foobar");
    let mut args = fixture.workspace_args("foo"); values(&mut args, &["--limit", "1"]);
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert!(json.get("text_candidates_complete").flag());
    fixture.file("b.rs", b"foo foo");
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(json.get("candidates").array().len(), 1);
    assert_eq!(json.get("stats").get("matching_candidates_seen").number(), 2);
    assert_eq!(json.get("stats").get("visited_files").number(), 2);
}

#[test]
fn zero_output_budget_distinguishes_hits_from_complete_absence() {
    let fixture = Fixture::new(); fixture.file("source.txt", b"foobar");
    let mut args = fixture.file_args("source.txt", "foo"); values(&mut args, &["--limit", "0"]);
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(json.get("text_candidates_complete").flag());
    fixture.file("source.txt", b"foo");
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(json.get("candidates").array().is_empty());
    assert_eq!(json.get("stats").get("matching_candidates_seen").number(), 1);
}

#[test]
fn malformed_and_unread_sources_cannot_publish_complete_absence() {
    let fixture = Fixture::new(); fixture.file("bad.rs", b"foo\xff");
    let (exit, json) = invoke(&fixture.file_args("bad.rs", "foo"), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(json.get("candidates").array().is_empty());
    assert_eq!(json.get("notices").array()[0].get("reason").text(), "REFERENCE_UNSUPPORTED_ENCODING");
    fixture.file("source.txt", b"foo");
    let mut args = fixture.file_args("source.txt", "foo"); values(&mut args, &["--max-bytes", "0"]);
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!json.get("text_candidates_complete").flag());
    assert_eq!(json.get("stats").get("member_payload_bytes_read").number(), 0);
}

#[test]
fn reference_search_accepts_larger_files_without_relaxing_outline_limits() {
    let fixture = Fixture::new(); let mut bytes = vec![b' '; 128 * 1024]; bytes.extend_from_slice(b"foo");
    fixture.file("large.rs", &bytes);
    let (exit, json) = invoke(&fixture.file_args("large.rs", "foo"), || false);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(json.get("candidates").array()[0].get("original_range").get("start").number(), 128 * 1024);
    fixture.file("denied.txt", &vec![b' '; 512 * 1024 + 1]);
    let (exit, json) = invoke(&fixture.file_args("denied.txt", "foo"), || false);
    assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(json.get("notices").array()[0].get("reason").text(), "REFERENCE_SOURCE_LIMIT");
    assert_eq!(json.get("stats").get("member_payload_bytes_read").number(), 0);
}

#[test]
fn saved_references_are_offline_and_raw_member_paths_are_reversible() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new(); let name = b"raw\xff.txt";
    fs::write(fixture.root().join(OsString::from_vec(name.to_vec())), b"foo foo_").unwrap();
    fixture.save(); fs::rename(fixture.root(), fixture.0.join("retired-root")).unwrap();
    let hex = name.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let mut args = fixture.saved_args("foo"); values(&mut args, &["--member-hex", &hex]);
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert!(!json.get("live_roots_accessed").flag());
    assert_eq!(json.get("candidates").array().len(), 1);
    assert_eq!(json.get("candidates").array()[0].get("path").get("hex").text(), hex);
    assert!(json.get("stats").get("archive_validation_bytes").number() > 0);
}

#[test]
fn incomplete_saved_member_visits_cannot_claim_a_complete_query() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"foo"); fixture.file("b.rs", b"foo"); fixture.save();
    let mut args = fixture.saved_args("foo"); values(&mut args, &["--max-files", "1"]);
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!json.get("text_candidates_complete").flag());
    assert_eq!(json.get("stats").get("known_files").number(), 2);
    assert_eq!(json.get("stats").get("visited_files").number(), 1);
}

#[test]
fn invalid_reference_queries_are_rejected_before_source_access() {
    let fixture = Fixture::new();
    for name in ["foo.bar", "foo bar", "2foo", "--json"] {
        let (exit, json) = invoke(&fixture.file_args("does-not-exist.rs", name), || false);
        assert_eq!(exit, EXIT_ERROR);
        assert_eq!(json.get("error").get("code").text(), "REFERENCE_INVALID_NAME");
    }
    let mut args = fixture.file_args("does-not-exist.rs", "foo"); values(&mut args, &["--match", "prefix"]);
    let (exit, json) = invoke(&args, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(json.get("error").get("code").text(), "CLI_INCOMPATIBLE_OPTIONS");
}

#[test]
fn missing_names_duplicate_modes_and_precanceled_requests_fail_atomically() {
    let fixture = Fixture::new();
    let missing = vec!["symbols".into(), "absent.rs".into(), "--references".into(), "--json".into()];
    let (exit, json) = invoke(&missing, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(json.get("error").get("code").text(), "REFERENCE_INVALID_NAME");
    let mut duplicate = fixture.file_args("absent.rs", "foo"); duplicate.push("--references".into());
    assert_eq!(invoke(&duplicate, || false).0, EXIT_ERROR);
    assert_eq!(invoke(&fixture.file_args("absent.rs", "foo"), || true).0, EXIT_CANCELED);
}

#[test]
fn broken_delivery_is_not_followed_by_a_second_json_document() {
    let fixture = Fixture::new(); fixture.file("source.rs", b"foo");
    struct Broken { bytes: Vec<u8> }
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.bytes.is_empty() { return Err(io::ErrorKind::BrokenPipe.into()); }
            let count = bytes.len().min(23); self.bytes.extend_from_slice(&bytes[..count]); Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let mut broken = Broken { bytes: Vec::new() }; let mut stderr = Vec::new();
    let exit = run(&fixture.file_args("source.rs", "foo"), &mut NeverRead, &mut broken, &mut stderr, || false);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(broken.bytes.len(), 23);
    assert!(String::from_utf8(stderr).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
}

#[test]
fn human_output_and_help_expose_the_reference_policy() {
    let fixture = Fixture::new(); fixture.file("source.txt", b"foo");
    let mut args = fixture.file_args("source.txt", "foo"); args.pop();
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    assert_eq!(run(&args, &mut NeverRead, &mut stdout, &mut stderr, || false), EXIT_OK);
    let text = String::from_utf8(stdout).unwrap();
    assert!(text.contains("whole-token-text-candidate")); assert!(text.contains("bytes 0..3")); assert!(stderr.is_empty());
    let help: Vec<OsString> = ["symbols", "--help", "--json"].into_iter().map(OsString::from).collect();
    let (exit, json) = invoke(&help, || false); assert_eq!(exit, EXIT_OK);
    assert!(json.get("text").text().contains("--references --name IDENTIFIER"));
}

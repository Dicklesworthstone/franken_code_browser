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
        let path = std::env::temp_dir().join(format!("fcb-symbols-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); fs::create_dir(path.join("root")).unwrap(); Self(path)
    }
    fn root(&self) -> PathBuf { self.0.join("root") }
    fn file(&self, path: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root().join(path); fs::write(&path, bytes).unwrap(); path
    }
    fn file_args(&self, path: &str) -> Vec<OsString> { vec!["symbols".into(), self.root().join(path).into_os_string(), "--json".into()] }
    fn workspace_args(&self) -> Vec<OsString> { vec!["symbols".into(), self.root().into_os_string(), "--workspace".into(), "--json".into()] }
    fn archive(&self) -> PathBuf { self.0.join("source.fcbs") }
    fn save(&self) {
        let args = vec!["snapshot".into(), "save".into(), self.root().into_os_string(), "--output".into(), self.archive().into_os_string(), "--json".into()];
        assert_eq!(invoke(&args, || false).0, EXIT_OK);
    }
    fn saved_args(&self) -> Vec<OsString> { vec!["symbols".into(), self.archive().into_os_string(), "--snapshot".into(), "--json".into()] }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NeverRead;
impl Read for NeverRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("symbols must not implicitly read stdin") }
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
fn file_lookup_preserves_duplicate_names_parent_ids_and_original_name_ranges() {
    let fixture = Fixture::new();
    let bytes = b"// fn fake() {}\nimpl Thing { fn run(&self) {} }\nfn run() {}\nfn runner() {}\n";
    fixture.file("main.rs", bytes);
    let mut args = fixture.file_args("main.rs"); values(&mut args, &["--name", "run"]);
    let (exit, output) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert!(output.get("processing_complete").flag());
    assert!(!output.get("compiler_resolved").flag()); assert!(!output.get("symbol_inventory_complete").flag());
    let candidates = output.get("candidates").array(); assert_eq!(candidates.len(), 2);
    assert_ne!(candidates[0].get("candidate_id").number(), candidates[1].get("candidate_id").number());
    assert_eq!(candidates[0].get("kind").text(), "method");
    assert_eq!(candidates[1].get("kind").text(), "function");
    for candidate in candidates {
        let range = candidate.get("name_range");
        let start = range.get("start").number() as usize; let end = range.get("end").number() as usize;
        assert_eq!(&bytes[start..end], b"run");
    }
    assert_eq!(fs::read(fixture.root().join("main.rs")).unwrap(), bytes);
}

#[test]
fn utf16_and_utf8_bom_sources_return_original_not_decoded_offsets() {
    let fixture = Fixture::new();
    let text = "// 😀\r\nfn greet() {}\n";
    for (i, raw) in [utf16(text, true), utf16(text, false), [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat()].into_iter().enumerate() {
        let name = format!("file-{i}.rs"); fixture.file(&name, &raw);
        let mut args = fixture.file_args(&name); values(&mut args, &["--name", "greet"]);
        let (exit, output) = invoke(&args, || false); assert_eq!(exit, EXIT_OK);
        let candidate = &output.get("candidates").array()[0];
        assert_eq!(candidate.get("line").number(), 2);
        let start = candidate.get("name_range").get("start").number() as usize;
        let end = candidate.get("name_range").get("end").number() as usize;
        let expected = if i < 2 { utf16("greet", i == 0)[2..].to_vec() } else { b"greet".to_vec() };
        assert_eq!(&raw[start..end], expected);
        assert_eq!(start, if i < 2 { 22 } else { 15 });
    }
}

#[test]
fn live_workspace_uses_supported_routes_and_exposes_unsupported_files() {
    let fixture = Fixture::new();
    fixture.file("a.rs", b"fn work() {}\n");
    fixture.file("b.py", b"def work():\n    pass\n");
    fixture.file("c.ts", b"export function work() {}\n");
    fixture.file("README.md", b"# work\n");
    let mut args = fixture.workspace_args(); values(&mut args, &["--name", "work"]);
    let (exit, output) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(output.get("discovery_complete").flag());
    assert_eq!(output.get("candidates").array().len(), 3);
    assert_eq!(output.get("stats").get("analyzed_files").number(), 3);
    assert_eq!(output.get("stats").get("unsupported_language_files").number(), 1);
    assert_eq!(output.get("notices").array()[0].get("reason").text(), "SYMBOL_UNSUPPORTED_LANGUAGE");
    let files: Vec<_> = output.get("candidates").array().iter().map(|candidate| candidate.get("file_id").number()).collect();
    assert_ne!(files[0], files[1]); assert_ne!(files[1], files[2]);
}

#[test]
fn name_filtering_precedes_display_limits_and_exact_capacity_does_not_imply_truncation() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"fn alpha() {}\nfn beta() {}\nfn better() {}\n");
    let mut exact = fixture.file_args("a.rs"); values(&mut exact, &["--name", "beta", "--limit", "1"]);
    let (exit, output) = invoke(&exact, || false);
    assert_eq!(exit, EXIT_OK); assert!(!output.get("listing_truncated").flag());
    assert_eq!(output.get("candidates").array()[0].get("name").text(), "beta");
    let mut prefix = fixture.file_args("a.rs"); values(&mut prefix, &["--name", "bet", "--match", "prefix", "--limit", "1"]);
    let (exit, output) = invoke(&prefix, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(output.get("listing_truncated").flag());
    assert_eq!(output.get("stats").get("matching_candidates_seen").number(), 2);
    let mut zero = fixture.file_args("a.rs"); values(&mut zero, &["--limit", "0"]);
    let (exit, output) = invoke(&zero, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(output.get("candidates").array().is_empty());
    assert_eq!(output.get("stats").get("matching_candidates_seen").number(), 3);
}

#[test]
fn oversized_and_total_budget_refusals_do_not_read_prefixes_as_complete_files() {
    let fixture = Fixture::new(); fixture.file("big.rs", &vec![b'x'; 65537]);
    let (exit, output) = invoke(&fixture.file_args("big.rs"), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(output.get("stats").get("member_payload_bytes_read").number(), 0);
    assert_eq!(output.get("notices").array()[0].get("reason").text(), "SYMBOL_SOURCE_LIMIT");
    fixture.file("small.rs", b"fn actual() {}\n");
    let mut args = fixture.file_args("small.rs"); values(&mut args, &["--max-bytes", "0"]);
    let (exit, output) = invoke(&args, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!output.get("processing_complete").flag());
    assert_eq!(output.get("stats").get("member_payload_bytes_read").number(), 0);
    assert_eq!(output.get("notices").array()[0].get("reason").text(), "SYMBOL_TOTAL_SOURCE_LIMIT");
}

#[test]
fn malformed_and_recursive_source_do_not_escape_as_valid_outline_evidence() {
    let fixture = Fixture::new(); fixture.file("bad.rs", b"fn bad() {}\xff");
    let (exit, output) = invoke(&fixture.file_args("bad.rs"), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(output.get("candidates").array().is_empty());
    assert_eq!(output.get("notices").array()[0].get("reason").text(), "SYMBOL_UNSUPPORTED_ENCODING");
    fixture.file("deep.rs", &b"impl T {".repeat(129));
    let (exit, output) = invoke(&fixture.file_args("deep.rs"), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(output.get("notices").array()[0].get("reason").text(), "SYMBOL_COMPLEXITY_LIMIT");
}

#[test]
fn saved_symbol_lookup_is_offline_and_raw_member_names_are_reversible() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let raw_name = b"raw\\name\xff.rs";
    fs::write(fixture.root().join(OsString::from_vec(raw_name.to_vec())), b"fn archived() {}\n").unwrap();
    fixture.save();
    fs::rename(fixture.root(), fixture.0.join("retired-root")).unwrap();
    let hex = raw_name.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let mut args = fixture.saved_args(); values(&mut args, &["--member-hex", &hex, "--name", "archived"]);
    let (exit, output) = invoke(&args, || false);
    assert_eq!(exit, EXIT_OK); assert!(!output.get("live_roots_accessed").flag());
    assert_eq!(output.get("candidates").array()[0].get("path").get("hex").text(), hex);
    assert!(output.get("stats").get("archive_validation_bytes").number() > 0);
    assert_eq!(output.get("candidates").array()[0].get("name").text(), "archived");
    let mut absent = fixture.saved_args(); values(&mut absent, &["--member", "missing.rs"]);
    assert_eq!(invoke(&absent, || false).0, EXIT_ERROR);
}

#[test]
fn scoped_discovery_limits_and_notice_limits_are_independent_of_candidate_output() {
    let fixture = Fixture::new();
    for i in 0..70 { fixture.file(&format!("unknown-{i:03}.txt"), b"plain"); }
    let (exit, output) = invoke(&fixture.workspace_args(), || false);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(output.get("notices").array().len(), 64);
    assert_eq!(output.get("stats").get("notices_omitted").number(), 6);
    assert_eq!(output.get("stats").get("member_payload_bytes_read").number(), 0);
    let mut limited = fixture.workspace_args(); values(&mut limited, &["--max-files", "1"]);
    let (exit, output) = invoke(&limited, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!output.get("discovery_complete").flag());
    assert_eq!(output.get("stats").get("known_files").number(), 1);
}

#[test]
fn completed_candidate_processing_is_not_an_exhaustive_definition_claim() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"// no recognized declaration\n");
    let mut args = fixture.file_args("a.rs"); values(&mut args, &["--name", "missing"]);
    let (exit, output) = invoke(&args, || false);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(output.get("processing_complete").flag());
    assert!(!output.get("symbol_inventory_complete").flag()); assert!(!output.get("compiler_resolved").flag());
    assert_eq!(output.get("stats").get("files_without_recognized_declarations").number(), 1);
}

#[test]
fn cancellation_and_failed_output_never_mutate_sources_or_append_second_documents() {
    let fixture = Fixture::new(); let source = b"fn keep() {}\n"; fixture.file("a.rs", source);
    let (exit, error) = invoke(&fixture.file_args("a.rs"), || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(error.get("status").text(), "error");
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::BrokenPipe.into()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let mut stderr = Vec::new();
    assert_eq!(run(&fixture.file_args("a.rs"), &mut NeverRead, &mut Broken, &mut stderr, || false), EXIT_ERROR);
    assert!(String::from_utf8(stderr).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
    assert_eq!(fs::read(fixture.root().join("a.rs")).unwrap(), source);
}

#[test]
fn actual_binary_exercises_workspace_and_offline_symbol_routes() {
    let fixture = Fixture::new(); fixture.file("a.rs", b"fn execute() {}\n"); fixture.file("b.py", b"def execute():\n    pass\n");
    let mut live = fixture.workspace_args(); values(&mut live, &["--name", "execute"]);
    let result = Command::new(env!("CARGO_BIN_EXE_fcb")).args(live).output().unwrap();
    assert_eq!(result.status.code(), Some(EXIT_OK as i32));
    assert_eq!(parse(&result.stdout).unwrap().get("candidates").array().len(), 2);
    fixture.save(); fs::rename(fixture.root(), fixture.0.join("moved-root")).unwrap();
    let mut args = fixture.saved_args(); values(&mut args, &["--name", "exec", "--match", "prefix"]);
    let result = Command::new(env!("CARGO_BIN_EXE_fcb")).args(args).output().unwrap();
    assert_eq!(result.status.code(), Some(EXIT_OK as i32));
    let parsed = parse(&result.stdout).unwrap();
    assert_eq!(parsed.get("candidates").array().len(), 2); assert!(!parsed.get("live_roots_accessed").flag());
}

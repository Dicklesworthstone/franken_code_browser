#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Write}, path::PathBuf, process::Command,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_CANCELED};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-paged-postings-{}-{now}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); fs::create_dir(path.join("root")).unwrap(); Self(path)
    }
    fn snapshot(&self) -> PathBuf { self.0.join("source.fcbs") }
    fn index(&self) -> PathBuf { self.0.join("search.fcbd") }
    fn save(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "save".into(), self.0.join("root").into_os_string(), "--output".into(), self.snapshot().into_os_string(), "--json".into()]
    }
    fn build(&self) -> Vec<OsString> {
        vec!["snapshot".into(), "index".into(), "build".into(), self.snapshot().into_os_string(), "--output".into(), self.index().into_os_string(), "--paged".into(), "--json".into()]
    }
    fn inspect(&self, pin: &str) -> Vec<OsString> {
        vec!["snapshot".into(), "index".into(), "inspect".into(), self.snapshot().into_os_string(), "--index".into(), self.index().into_os_string(),
            "--index-digest".into(), pin.into(), "--paged".into(), "--json".into()]
    }
    fn search(&self, pin: &str, needle: &str) -> Vec<OsString> {
        let mut args = self.inspect(pin); args[2] = "search".into(); args.extend(["--text".into(), needle.into()]); args
    }
    fn prepare(&self) -> String {
        assert_eq!(invoke(&self.save(), || false).0, EXIT_OK);
        let (code, out) = invoke(&self.build(), || false); assert_eq!(code, EXIT_OK);
        assert_eq!(out.get("index_pin_scope").text(), "page-manifest-envelope-sha256");
        out.get("index_digest").text().to_owned()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut out = Vec::new(); let mut err = Vec::new();
    let code = run(args, &mut &b""[..], &mut out, &mut err, canceled);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    let json = parse(&out).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&out)));
    assert_eq!(json.get("schema").text(), "fcb.cli/1"); (code, json)
}
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe]; for word in text.encode_utf16() { bytes.extend_from_slice(&word.to_le_bytes()); } bytes
}

#[test]
fn publication_inspection_and_exact_search_preserve_encodings_and_read_only_effects() {
    let f = Fixture::new(); fs::write(f.0.join("root/a.rs"), b"banana needle").unwrap();
    fs::write(f.0.join("root/b.rs"), utf16("needle")).unwrap();
    let pin = f.prepare(); let before = fs::read(f.index()).unwrap();
    let (code, inspected) = invoke(&f.inspect(&pin), || false); assert_eq!(code, EXIT_OK);
    assert_eq!(inspected.get("index_layout").text(), "demand-paged-postings-v1");
    assert_eq!(inspected.get("index_page_bytes_read").number(), 0);
    assert!(!inspected.get("index_body_verified_on_open").flag());
    assert!(inspected.get("index_open_bytes_read").number() < inspected.get("index_bytes").number());
    let (code, found) = invoke(&f.search(&pin, "needle"), || false); assert_eq!(code, EXIT_OK);
    assert_eq!(found.get("hits").array().len(), 2); assert_eq!(found.get("index_fallback_files").number(), 1);
    assert!(found.get("workspace_complete").flag()); assert!(!found.get("live_roots_accessed").flag());
    assert_eq!(found.get("index_cache_capacity_bytes").number(), 65536);
    assert_eq!(found.get("hits").array()[1].get("original_range").get("start").number(), 2);
    assert_eq!(fs::read(f.index()).unwrap(), before);
}

#[test]
fn catalog_and_page_manifest_avoid_both_full_body_reads() {
    let f = Fixture::new();
    for i in 0..64 { fs::write(f.0.join(format!("root/file-{i:03}.rs")), format!("common body {i:05}" )).unwrap(); }
    fs::write(f.0.join("root/target.rs"), b"RARE_TOKEN").unwrap();
    let pin = f.prepare(); let catalog = f.0.join("source.fcbc");
    let create = vec!["snapshot".into(), "catalog".into(), f.snapshot().into_os_string(), "--output".into(), catalog.clone().into_os_string(), "--json".into()];
    let (code, receipt) = invoke(&create, || false); assert_eq!(code, EXIT_OK);
    let mut search = f.search(&pin, "RARE_TOKEN");
    search.extend(["--catalog".into(), catalog.into_os_string(), "--catalog-digest".into(), receipt.get("catalog_digest").text().into()]);
    let (code, result) = invoke(&search, || false); assert_eq!(code, EXIT_OK);
    assert_eq!(result.get("archive_validation_bytes").number(), 56);
    assert_eq!(result.get("member_payload_bytes_loaded").number(), 10);
    assert_eq!(result.get("loaded_members").number(), 1);
    assert!(!result.get("archive_body_verified_on_open").flag());
    assert_eq!(result.get("metadata_members_visited").number(), 1);
}

#[test]
fn tampered_page_is_unchecked_at_inspect_but_refused_before_matching() {
    let f = Fixture::new(); fs::write(f.0.join("root/a"), b"banana needle").unwrap(); let pin = f.prepare();
    let mut bytes = fs::read(f.index()).unwrap();
    let body = 32 + u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
    bytes[body] ^= 1; fs::write(f.index(), &bytes).unwrap();
    let (code, inspected) = invoke(&f.inspect(&pin), || false); assert_eq!(code, EXIT_OK);
    assert!(!inspected.get("index_body_verified_on_open").flag());
    let (code, result) = invoke(&f.search(&pin, "needle"), || false); assert_eq!(code, EXIT_ERROR);
    assert_eq!(result.get("error").get("code").text(), "PAGED_INDEX_CHANGED_PAGE");
    assert_eq!(result.get("effect").text(), "none");
}

#[test]
fn manifest_tampering_wrong_pins_and_format_confusion_never_silently_fallback() {
    let f = Fixture::new(); fs::write(f.0.join("root/a"), b"needle").unwrap(); let pin = f.prepare();
    assert_eq!(invoke(&f.inspect(&"00".repeat(32)), || false).0, EXIT_ERROR);
    let mut no_mode = f.search(&pin, "needle"); no_mode.retain(|arg| arg != "--paged");
    assert_eq!(invoke(&no_mode, || false).0, EXIT_ERROR);
    let mut bytes = fs::read(f.index()).unwrap(); bytes[70] ^= 1; fs::write(f.index(), bytes).unwrap();
    assert_eq!(invoke(&f.inspect(&pin), || false).0, EXIT_ERROR);
}

#[test]
fn zero_index_quota_falls_back_instead_of_losing_matches_and_lookahead() {
    let f = Fixture::new(); fs::write(f.0.join("root/a"), b"banana").unwrap(); fs::write(f.0.join("root/b"), b"ana").unwrap();
    assert_eq!(invoke(&f.save(), || false).0, EXIT_OK);
    let mut build = f.build(); build.extend(["--max-grams".into(), "0".into()]);
    let (code, built) = invoke(&build, || false); assert_eq!(code, EXIT_PARTIAL);
    let pin = built.get("index_digest").text();
    for limit in [0, 1, 2, 3] {
        let mut query = f.search(pin, "ana"); query.extend(["--limit".into(), limit.to_string().into()]);
        let (code, result) = invoke(&query, || false);
        assert_eq!(code, if limit < 3 { EXIT_PARTIAL } else { EXIT_OK });
        assert_eq!(result.get("hits").array().len(), limit);
        assert_eq!(result.get("truncated").flag(), limit < 3);
        assert_eq!(result.get("index_page_bytes_read").number(), 0);
    }
}

#[test]
fn canceled_or_repeated_publication_never_overwrites_a_destination() {
    let f = Fixture::new(); fs::write(f.0.join("root/a"), b"needle").unwrap();
    assert_eq!(invoke(&f.save(), || false).0, EXIT_OK);
    let (code, receipt) = invoke(&f.build(), || f.index().exists()); assert_eq!(code, EXIT_CANCELED);
    assert_eq!(receipt.get("effect").text(), "destination-created-incomplete");
    assert_eq!(fs::metadata(f.index()).unwrap().len(), 0);
    assert_eq!(invoke(&f.build(), || false).0, EXIT_ERROR);
}

#[test]
fn successful_publication_survives_a_broken_response_pipe() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::Error::from(io::ErrorKind::BrokenPipe)) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let f = Fixture::new(); fs::write(f.0.join("root/a"), b"needle").unwrap(); assert_eq!(invoke(&f.save(), || false).0, EXIT_OK);
    let mut stderr = Vec::new();
    assert_eq!(run(&f.build(), &mut &b""[..], &mut Broken, &mut stderr, || false), EXIT_ERROR);
    assert!(fs::metadata(f.index()).unwrap().len() > 32);
    assert!(String::from_utf8(stderr).unwrap().contains("complete-file-sync-requested"));
}

#[test]
fn actual_binary_reads_paged_index_without_the_original_workspace() {
    let f = Fixture::new(); fs::write(f.0.join("root/a.rs"), b"banana").unwrap();
    let saved = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.save()).output().unwrap(); assert_eq!(saved.status.code(), Some(0));
    let built = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.build()).output().unwrap(); assert_eq!(built.status.code(), Some(0));
    let receipt = parse(&built.stdout).unwrap(); let pin = receipt.get("index_digest").text();
    fs::rename(f.0.join("root"), f.0.join("gone")).unwrap();
    let found = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.search(pin, "ana")).output().unwrap();
    assert_eq!(found.status.code(), Some(0)); assert_eq!(parse(&found.stdout).unwrap().get("hits").array().len(), 2);
    let absent = Command::new(env!("CARGO_BIN_EXE_fcb")).args(f.search(pin, "not-there")).output().unwrap();
    assert_eq!(absent.status.code(), Some(EXIT_NO_MATCH as i32));
}

#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{Json, parse};
use std::{ffi::{OsStr, OsString}, fs, io, path::{Path, PathBuf}, process::Command,
    sync::atomic::{AtomicU64, Ordering}};
use fcb_app::{EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fcb-cli-workspace-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, name: impl AsRef<Path>, bytes: &[u8]) {
        let path = self.0.join(name); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
}
impl Drop for Tree { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn arguments(tree: &Tree, command: &str, options: &[&str]) -> Vec<OsString> {
    let mut args = vec![command.into(), tree.0.as_os_str().to_owned(), "--workspace".into(), "--json".into()];
    args.extend(options.iter().map(OsString::from)); args
}
fn run(tree: &Tree, command: &str, options: &[&str]) -> (u8, Json) {
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let exit = fcb_app::run(&arguments(tree, command, options), &mut io::empty(), &mut stdout, &mut stderr, || false);
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    (exit, parse(&stdout).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&stdout))))
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}
fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes().chunks_exact(2).map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()).collect()
}

#[test]
fn recursive_literal_search_verifies_mixed_encodings_and_original_byte_ranges() {
    use std::os::unix::ffi::OsStrExt;
    let tree = Tree::new(); tree.file("src/a.rs", b"prefix needle space suffix");
    tree.file("docs/b.txt", &utf16("needle space", true));
    tree.file("docs/c.txt", &utf16("needle space", false));
    tree.file("not-a-phrase", b"needle\nspace");
    tree.file("unrelated", b"nothing relevant");
    let (exit, result) = run(&tree, "search", &["--text", "needle space"]);
    assert_eq!(exit, EXIT_OK, "{result:?}");
    assert!(result.get("workspace_complete").flag()); assert!(result.get("discovery_complete").flag());
    assert_eq!(result.get("matches_seen").number(), 3);
    assert!(result.get("index_skipped_files").number() >= 1);
    assert_eq!(result.get("fallback_scans").number(), 2);
    for hit in result.get("hits").array() {
        let path = unhex(hit.get("path").get("hex").text());
        let bytes = fs::read(tree.0.join(OsStr::from_bytes(&path))).unwrap();
        let start = hit.get("original_range").get("start").number() as usize;
        let end = hit.get("original_range").get("end").number() as usize;
        assert_eq!(hit.get("capture_sha256").text(), fcb::store::Sha256::digest(&bytes).to_hex());
        assert_eq!(hit.get("capture_byte_length").number(), bytes.len() as u64);
        let selected = &bytes[start..end];
        let expected = if path.ends_with(b"a.rs") { b"needle space".to_vec() }
            else if path.ends_with(b"b.txt") { utf16("needle space", true)[2..].to_vec() }
            else { utf16("needle space", false)[2..].to_vec() };
        assert_eq!(selected, expected); assert_eq!(hit.get("matched_text").text(), "needle space");
    }
}

#[test]
fn filename_search_does_not_load_giant_or_malformed_payloads_and_preserves_raw_paths() {
    use std::os::unix::ffi::OsStrExt;
    let tree = Tree::new(); tree.file("src/Foo.rs", b"\xff malformed"); tree.file("src/foo.rs", b"lower");
    tree.file(Path::new(OsStr::from_bytes(b"foo-\xff.rs")), b"bytes");
    let sparse = fs::File::create(tree.0.join("huge-foo.rs")).unwrap(); sparse.set_len((1u64 << 32) + 19).unwrap();
    let (exit, result) = run(&tree, "search", &["--path", "foo", "--max-file-bytes", "0"]);
    assert_eq!(exit, EXIT_OK, "{result:?}"); assert_eq!(result.get("payload_bytes_read").number(), 0);
    assert_eq!(result.get("matches_seen").number(), 4); assert!(result.get("workspace_complete").flag());
    let paths: Vec<_> = result.get("hits").array().iter().map(|hit| unhex(hit.get("path").get("hex").text())).collect();
    assert!(paths.contains(&b"src/Foo.rs".to_vec())); assert!(paths.contains(&b"src/foo.rs".to_vec()));
    assert!(paths.contains(&b"foo-\xff.rs".to_vec()));
}

#[test]
fn inspect_workspace_lists_metadata_without_interpreting_ignore_file_contents() {
    let tree = Tree::new(); tree.file(".gitignore", b"*.rs\n"); tree.file("src/a.rs", b"visible");
    tree.file("target/generated.rs", b"excluded-by-default");
    let (exit, result) = run(&tree, "inspect", &[]);
    assert_eq!(exit, EXIT_OK, "{result:?}"); assert_eq!(result.get("payload_bytes_read").number(), 0);
    assert!(result.get("policy").text().contains("no-rule-files"));
    assert_eq!(result.get("known_files").number(), 2);
    assert_eq!(result.get("discovery").get("excluded_entries").number(), 1);
    let (exit, included) = run(&tree, "inspect", &["--include-excluded"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(included.get("known_files").number(), 3);
}

#[test]
fn oversized_and_unreadable_text_are_not_reported_as_complete_negatives() {
    let tree = Tree::new(); tree.file("a", b"needle"); tree.file("bad", b"text\xff");
    let sparse = fs::File::create(tree.0.join("huge")).unwrap(); sparse.set_len((1u64 << 32) + 1).unwrap();
    let (exit, result) = run(&tree, "search", &["--text", "needle"]);
    assert_eq!(exit, EXIT_PARTIAL, "{result:?}"); assert!(!result.get("workspace_complete").flag());
    assert_eq!(result.get("hits").array().len(), 1);
    assert_eq!(result.get("unavailable_files").array().len(), 1);
    assert_eq!(result.get("unsupported_text_files").array().len(), 1);
    assert_eq!(result.get("payload_bytes_read").number(), 11);
    let (exit, absent) = run(&tree, "search", &["--text", "absent"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(absent.get("hits").array().is_empty());
}

#[test]
fn result_limit_uses_real_lookahead_and_preserves_literal_spaces() {
    let tree = Tree::new(); tree.file("a", b"needle space"); tree.file("b", b"needle space");
    for (limit, expected, truncated) in [("1", EXIT_PARTIAL, true), ("2", EXIT_OK, false)] {
        let (exit, result) = run(&tree, "search", &["--text", "needle space", "--limit", limit]);
        assert_eq!(exit, expected, "{result:?}"); assert_eq!(result.get("truncated").flag(), truncated);
        assert_eq!(result.get("workspace_complete").flag(), !truncated);
    }
}

#[test]
fn total_io_and_retained_source_budget_are_shared_across_files() {
    let tree = Tree::new(); tree.file("a", b"needle"); tree.file("b", b"needle");
    let (exit, result) = run(&tree, "search", &["--text", "needle", "--max-total-bytes", "6"]);
    assert_eq!(exit, EXIT_PARTIAL, "{result:?}");
    assert_eq!(result.get("payload_bytes_read").number(), 6);
    assert_eq!(result.get("captured_bytes").number(), 6);
    assert_eq!(result.get("unavailable_files").array().len(), 1);
}

#[test]
fn discovery_and_display_truncation_are_separate_from_a_complete_negative() {
    let tree = Tree::new(); for name in ["foo-a", "foo-b", "foo-c"] { tree.file(name, b"source"); }
    let (exit, missing) = run(&tree, "search", &["--path", "no-match", "--max-files", "1"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!missing.get("discovery_complete").flag());
    assert!(missing.get("hits").array().is_empty());
    let (exit, limited) = run(&tree, "search", &["--path", "foo", "--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(limited.get("workspace_complete").flag());
    assert!(limited.get("truncated").flag()); assert_eq!(limited.get("matches_seen").number(), 3);
    let (exit, complete) = run(&tree, "search", &["--text", "no-match"]);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(complete.get("workspace_complete").flag());
}

#[test]
fn empty_workspace_is_an_actual_complete_empty_scope() {
    let tree = Tree::new();
    for options in [vec!["--path", "absent"], vec!["--text", "absent"]] {
        let (exit, result) = run(&tree, "search", &options);
        assert_eq!(exit, EXIT_NO_MATCH, "{result:?}"); assert!(result.get("workspace_complete").flag());
        assert!(result.get("hits").array().is_empty());
    }
}

#[test]
fn symlinks_and_special_objects_are_not_read_as_sources() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let tree = Tree::new(); let outside = Tree::new(); outside.file("private", b"secretneedle");
    tree.file("inside", b"unrelated"); symlink(outside.0.join("private"), tree.0.join("link")).unwrap();
    symlink(&tree.0, tree.0.join("cycle")).unwrap();
    let _socket = UnixListener::bind(tree.0.join("socket")).unwrap();
    let (exit, result) = run(&tree, "search", &["--text", "secretneedle"]);
    assert_eq!(exit, EXIT_NO_MATCH, "{result:?}");
    assert_eq!(result.get("discovery").get("symlinks_not_followed").number(), 2);
    assert_eq!(result.get("discovery").get("special_objects").number(), 1);
    assert_eq!(result.get("payload_bytes_read").number(), 9);
}

#[test]
fn cancellation_discards_private_partial_output_and_preserves_one_error_document() {
    let tree = Tree::new(); for i in 0..20 { tree.file(format!("file-{i:03}"), &b"needle\n".repeat(100)); }
    let mut stdout = Vec::new(); let mut stderr = Vec::new(); let mut polls = 0;
    let exit = fcb_app::run(&arguments(&tree, "search", &["--text", "needle"]), &mut io::empty(),
        &mut stdout, &mut stderr, || { polls += 1; polls >= 12 });
    assert_eq!(exit, EXIT_CANCELED); assert!(stderr.is_empty());
    let error = parse(&stdout).unwrap(); assert_eq!(error.get("status").text(), "error");
    assert!(!error.get("complete").flag());
}

#[test]
fn actual_binary_searches_a_real_workspace_without_companion_processes() {
    let tree = Tree::new(); tree.file("nested/file.rs", b"prefix needle suffix");
    let result = Command::new(env!("CARGO_BIN_EXE_fcb"))
        .args(arguments(&tree, "search", &["--text", "needle"]))
        .stdin(std::process::Stdio::null()).output().unwrap();
    assert_eq!(result.status.code(), Some(i32::from(EXIT_OK)), "{}", String::from_utf8_lossy(&result.stdout));
    assert!(result.stderr.is_empty()); let value = parse(&result.stdout).unwrap();
    assert!(value.get("workspace_complete").flag()); assert_eq!(value.get("hits").array().len(), 1);
    assert_eq!(value.get("hits").array()[0].get("original_range").get("start").number(), 7);
}

#[test]
fn binary_scope_expansion_is_explicit_and_directory_errors_remain_machine_readable() {
    let tree = Tree::new(); tree.file("a", b"needle");
    let result = Command::new(env!("CARGO_BIN_EXE_fcb")).arg("search").arg(&tree.0)
        .args(["--text", "needle", "--json"]).output().unwrap();
    assert_eq!(result.status.code(), Some(i32::from(EXIT_ERROR)));
    assert_eq!(parse(&result.stdout).unwrap().get("error").get("code").text(), "CLI_DIRECTORY_SCOPE_UNAVAILABLE");
}

#[test]
fn text_hit_identity_covers_the_entire_capture_not_only_matching_bytes() {
    let tree = Tree::new();
    let first = b"needle one needle";
    let second = b"needle two needle";
    tree.file("a.rs", first);
    let (exit, before) = run(&tree, "search", &["--text", "needle"]);
    assert_eq!(exit, EXIT_OK);
    tree.file("a.rs", second);
    let (exit, after) = run(&tree, "search", &["--text", "needle"]);
    assert_eq!(exit, EXIT_OK);
    let old_hits = before.get("hits").array();
    let new_hits = after.get("hits").array();
    assert_eq!(old_hits.len(), 2);
    assert_eq!(new_hits.len(), 2);
    for (i, (old, new)) in old_hits.iter().zip(new_hits).enumerate() {
        let start = if i == 0 { 0 } else { 11 };
        for (hit, bytes) in [(old, first), (new, second)] {
            assert_eq!(hit.get("original_range").get("start").number(), start);
            assert_eq!(hit.get("original_range").get("end").number(), start + 6);
            assert_eq!(hit.get("matched_text").text(), "needle");
            assert_eq!(hit.get("capture_byte_length").number(), bytes.len() as u64);
            assert_eq!(hit.get("capture_sha256").text(), fcb::store::Sha256::digest(bytes).to_hex());
        }
        assert_ne!(old.get("capture_sha256").text(), new.get("capture_sha256").text());
    }
    // Adding capture identity must not introduce another filesystem payload read.
    assert_eq!(before.get("payload_bytes_read").number(), first.len() as u64);
    assert_eq!(after.get("payload_bytes_read").number(), second.len() as u64);
}

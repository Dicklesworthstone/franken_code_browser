#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Real CLI/service composition over temporary source trees. Count completeness
//! stays independent of output rows, source eligibility and verification work.

mod support;
use support::{Json, parse};
use std::{ffi::OsString, fs, io, path::PathBuf, process::Command,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_CANCELED};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-counting-{}-{nonce}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
    fn args(&self, options: &[&str]) -> Vec<OsString> {
        let mut args = vec!["search".into(), self.0.as_os_str().to_owned(), "--workspace".into(), "--json".into()];
        args.extend(options.iter().map(OsString::from)); args
    }
    fn run(&self, options: &[&str]) -> (u8, Json) { invoke(&self.args(options), || false) }
}
impl Drop for Tree { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let exit = fcb_app::run(args, &mut io::empty(), &mut stdout, &mut stderr, canceled);
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    (exit, parse(&stdout).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&stdout))))
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn counts_overlap_across_utf8_and_utf16_after_zero_or_one_retained_rows() {
    let tree = Tree::new();
    tree.file("a.rs", b"banana banana");
    tree.file("b.rs", &utf16("banana", true));
    tree.file("c.rs", &utf16("banana", false));
    for (limit, retained) in [("0", 0), ("1", 1), ("8", 8)] {
        let (exit, result) = tree.run(&["--text", "ana", "--count-all", "--limit", limit]);
        assert_eq!(exit, if retained < 8 { EXIT_PARTIAL } else { EXIT_OK }, "{result:?}");
        assert!(result.get("count_all_matches").flag());
        assert!(result.get("match_count_complete").flag());
        assert_eq!(result.get("matches_seen").number(), 8);
        assert_eq!(result.get("stored_hits").number(), retained);
        assert_eq!(result.get("hits").array().len() as u64, retained);
        assert_eq!(result.get("truncated").flag(), retained < 8);
        assert_eq!(result.get("workspace_complete").flag(), retained == 8);
        assert_eq!(result.get("payload_bytes_read").number(), 41);
        for hit in result.get("hits").array() {
            assert_eq!(hit.get("matched_text").text(), "ana");
            let range = hit.get("original_range");
            let length = range.get("end").number() - range.get("start").number();
            assert_eq!(length, if hit.get("path").get("hex").text() == "612e7273" { 3 } else { 6 });
        }
    }
    // Negative control: the original bounded route must not acquire an exact
    // count or scan every occurrence merely because counting is now available.
    let (exit, limited) = tree.run(&["--text", "ana", "--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!limited.get("count_all_matches").flag());
    assert!(!limited.get("match_count_complete").flag());
    assert_eq!(limited.get("matches_seen").number(), 2);
}

#[test]
fn expression_counting_qualifies_predicates_before_counting_and_filters_before_io() {
    let tree = Tree::new();
    let included = b"banana banana REQUIRED";
    let wide = utf16("BANANA required", false);
    let excluded = b"banana required FORBIDDEN";
    tree.file("src/a.rs", included); tree.file("src/b.rs", &wide);
    tree.file("src/c.rs", excluded); tree.file("outside.py", &[b'x'; 1024]);
    let query = "ana required -forbidden path:src/ lang:rust";
    let (exit, result) = tree.run(&["--query", query, "--ignore-case", "--count-all", "--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL, "{result:?}");
    assert!(result.get("match_count_complete").flag());
    assert_eq!(result.get("matches_seen").number(), 6);
    assert_eq!(result.get("stored_hits").number(), 1);
    assert_eq!(result.get("scope_files").number(), 3);
    assert_eq!(result.get("metadata_excluded_files").number(), 1);
    assert_eq!(result.get("payload_bytes_read").number(), (included.len() + wide.len() + excluded.len()) as u64);
    assert!(result.get("unavailable_files").array().is_empty());
}

#[test]
fn unicode_lowercase_expansions_keep_complete_source_units_and_exact_counts() {
    let tree = Tree::new(); tree.file("a.rs", "İİİ".as_bytes());
    for mode in ["--text", "--query"] {
        let (exit, result) = tree.run(&[mode, "i", "--ignore-case", "--count-all", "--limit", "1"]);
        assert_eq!(exit, EXIT_PARTIAL, "{result:?}");
        assert!(result.get("match_count_complete").flag());
        assert_eq!(result.get("matches_seen").number(), 3);
        assert_eq!(result.get("stored_hits").number(), 1);
        let hit = &result.get("hits").array()[0];
        assert_eq!(hit.get("original_range").get("start").number(), 0);
        assert_eq!(hit.get("original_range").get("end").number(), 2);
        assert_eq!(hit.get("matched_text").text(), "İ");
    }
}

#[test]
fn work_and_source_limits_remain_partial_after_retained_rows_fill() {
    let tree = Tree::new(); tree.file("a.rs", b"aaaaaaaa");
    let (exit, limited) = tree.run(&["--text", "a", "--count-all", "--limit", "1", "--max-scan-bytes", "4"]);
    assert_eq!(exit, EXIT_PARTIAL, "{limited:?}");
    assert!(limited.get("work_limited").flag());
    assert_eq!(limited.get("scanned_input_bytes").number(), 4);
    assert_eq!(limited.get("matches_seen").number(), 4);
    assert_eq!(limited.get("stored_hits").number(), 1);
    assert!(!limited.get("match_count_complete").flag());
    tree.file("bad.rs", b"a\xff"); tree.file("huge.rs", &[b'a'; 40]);
    let (exit, incomplete) = tree.run(&["--text", "a", "--count-all", "--limit", "1", "--max-file-bytes", "16"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!incomplete.get("match_count_complete").flag());
    assert_eq!(incomplete.get("unavailable_files").array().len(), 1);
    assert_eq!(incomplete.get("unsupported_text_files").array().len(), 1);
    assert_eq!(incomplete.get("matches_seen").number(), 8);
}

#[test]
fn zero_row_complete_negatives_and_empty_scopes_are_distinct_from_discovery_limits() {
    let tree = Tree::new();
    for mode in ["--text", "--query"] {
        let (exit, empty) = tree.run(&[mode, "missing", "--count-all", "--limit", "0"]);
        assert_eq!(exit, EXIT_NO_MATCH); assert!(empty.get("match_count_complete").flag());
        assert_eq!(empty.get("matches_seen").number(), 0); assert!(!empty.get("truncated").flag());
    }
    tree.file("a.rs", b"source"); tree.file("b.rs", b"source");
    // Count-only queries retain the compatible gram-negative route. No source
    // verification work is needed to establish this indexed complete negative.
    let (exit, negative) = tree.run(&["--text", "missing", "--count-all", "--limit", "0", "--max-scan-bytes", "0"]);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(negative.get("match_count_complete").flag());
    assert_eq!(negative.get("scanned_input_bytes").number(), 0);
    assert_eq!(negative.get("index_skipped_files").number(), 2);
    let (exit, partial) = tree.run(&["--text", "missing", "--count-all", "--limit", "0", "--max-files", "1"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!partial.get("match_count_complete").flag());
    assert!(!partial.get("discovery_complete").flag());
}

#[test]
fn cancellation_and_predicate_budget_do_not_turn_partial_counting_into_success() {
    let tree = Tree::new(); tree.file("a.rs", b"needle................forbidden");
    let (exit, partial) = tree.run(&["--query", "needle -forbidden", "--count-all", "--limit", "0", "--max-scan-bytes", "8"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(partial.get("work_limited").flag());
    assert!(!partial.get("match_count_complete").flag());
    assert_eq!(partial.get("matches_seen").number(), 0);
    let (exit, canceled) = invoke(&tree.args(&["--text", "needle", "--count-all"]), || true);
    assert_eq!(exit, EXIT_CANCELED); assert_eq!(canceled.get("status").text(), "error");
    assert!(!canceled.get("complete").flag());
}

#[test]
fn binary_exposes_exact_counts_without_rows_and_human_output_names_the_count() {
    let tree = Tree::new(); tree.file("a.rs", b"banana banana");
    let result = Command::new(env!("CARGO_BIN_EXE_fcb"))
        .args(tree.args(&["--text", "ana", "--count-all", "--limit", "0"]))
        .stdin(std::process::Stdio::null()).output().unwrap();
    assert_eq!(result.status.code(), Some(i32::from(EXIT_PARTIAL)));
    assert!(result.stderr.is_empty()); let document = parse(&result.stdout).unwrap();
    assert!(document.get("match_count_complete").flag());
    assert_eq!(document.get("matches_seen").number(), 4);
    assert!(document.get("hits").array().is_empty());
    let mut args = tree.args(&["--query", "ana", "--count-all", "--limit", "1"]);
    args.retain(|arg| arg != "--json");
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let exit = fcb_app::run(&args, &mut io::empty(), &mut stdout, &mut stderr, || false);
    assert_eq!(exit, EXIT_PARTIAL); assert!(stderr.is_empty());
    assert!(String::from_utf8(stdout).unwrap().contains("Exact occurrences: 4; retained rows: 1"));
}

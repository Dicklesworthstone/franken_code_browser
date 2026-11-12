#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs, io::{self, Read}, path::{Path, PathBuf}, process::Command,
    sync::atomic::{AtomicU64, Ordering}};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use support::{Json, parse};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-whole-cli-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name); fs::write(&path, bytes).unwrap(); path
    }
}
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NoStdin;
impl Read for NoStdin { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("file search must not consume stdin") } }
fn args(path: &Path, flags: &[&str]) -> Vec<OsString> {
    let mut args = vec!["search".into(), path.as_os_str().to_owned(), "--json".into()];
    args.extend(flags.iter().map(OsString::from)); args
}
fn invoke(path: &Path, flags: &[&str]) -> (u8, Json) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = run(&args(path, flags), &mut NoStdin, &mut stdout, &mut stderr, || false);
    assert!(stderr.is_empty(), "stderr={:?}", String::from_utf8_lossy(&stderr));
    let json = parse(&stdout).unwrap_or_else(|error| panic!("{error}: {:?}", String::from_utf8_lossy(&stdout)));
    assert_eq!(json.get("schema").text(), "fcb.cli/1");
    (code, json)
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}

fn encoded(text: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "utf8" => text.as_bytes().to_vec(),
        "utf16le" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        "utf16be" => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => panic!("unexpected encoding"),
    }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(hit: &Json) -> (u64, u64) {
    (hit.get("original_range").get("start").number(), hit.get("original_range").get("end").number())
}

#[test]
fn whole_file_mode_reaches_a_large_file_tail_without_weakening_the_window_route() {
    let temp = Temp::new();
    let mut bytes = vec![b'x'; 2 * 1024 * 1024]; bytes.extend_from_slice(b"needle");
    let file = temp.file("huge.rs", &bytes);
    let (window_exit, window) = invoke(&file, &["--text", "needle"]);
    assert_eq!(window_exit, EXIT_PARTIAL); assert!(window.get("hits").array().is_empty());
    assert!(!window.get("whole_file_complete").flag());
    let (exit, whole) = invoke(&file, &["--whole-file", "--text", "needle"]);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(whole.get("strategy").text(), "streaming-whole-file");
    assert!(whole.get("whole_file_complete").flag());
    assert_eq!(whole.get("hits").array()[0].get("original_range").get("start").number(), 2 * 1024 * 1024);
    assert_eq!(whole.get("payload_bytes_read").number(), bytes.len() as u64);
    assert!(whole.get("peak_input_buffer_bytes").number() <= 16 * 1024);
    assert_eq!(whole.get("literal_original_hex").text(), "6e6565646c65");
    assert_eq!(whole.get("retention").text(), "literal-witnesses-only");
    assert_eq!(whole.get("hits").array()[0].get("decoded_range"), &Json::Null);
}

#[test]
fn workspace_streams_large_utf8_and_utf16_files_without_the_capture_quota() {
    let temp = Temp::new();
    let mut large = vec![b'x'; 1024 * 1024 + 7]; large.extend_from_slice(b"needle");
    temp.file("a.rs", &large);
    let little = utf16("prefix needle 😀", true);
    let big = utf16("needle\u{feff} needle", false);
    temp.file("b.rs", &little); temp.file("c.rs", &big);
    let (exit, json) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle"]);
    assert_eq!(exit, EXIT_OK); assert!(json.get("workspace_complete").flag());
    assert!(!json.get("capture_limits_applied").flag());
    assert_eq!(json.get("matches_seen").number(), 4);
    assert_eq!(json.get("files_examined").number(), 3);
    assert_eq!(json.get("payload_bytes_read").number(), (large.len() + little.len() + big.len()) as u64);
    let files = json.get("files").array();
    assert_eq!(files[0].get("hits").array()[0].get("original_range").get("start").number(), 1024 * 1024 + 7);
    assert_eq!(files[1].get("hits").array()[0].get("original_range").get("start").number(), 16);
    assert_eq!(files[1].get("literal_original_hex").text(), "6e006500650064006c006500");
    assert_eq!(files[2].get("literal_original_hex").text(), "006e006500650064006c0065");
}

#[test]
fn byte_admission_is_global_across_files_and_unvisited_members_are_explicit() {
    let temp = Temp::new();
    temp.file("a.rs", b"1234567890"); temp.file("b.rs", b"1234567890"); temp.file("c.rs", b"needle");
    let (exit, json) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle", "--max-scan-bytes", "15"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(!json.get("workspace_complete").flag());
    assert_eq!(json.get("payload_bytes_read").number(), 15);
    assert_eq!(json.get("files_examined").number(), 2);
    assert_eq!(json.get("unexamined_files").number(), 1);
    assert_eq!(json.get("stop_reason").text(), "byte-limit");
    assert_eq!(json.get("matches_seen").number(), 0);
    let files = json.get("files").array();
    assert!(files[0].get("whole_file_complete").flag());
    assert!(!files[1].get("whole_file_complete").flag());
    assert_eq!(files[1].get("payload_bytes_read").number(), 5);
}

#[test]
fn result_limit_is_shared_and_exactly_full_is_not_proof_of_truncation() {
    let temp = Temp::new();
    temp.file("a.rs", b"needle"); temp.file("b.rs", b"absent");
    let (exit, exact) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle", "--limit", "1"]);
    assert_eq!(exit, EXIT_OK); assert!(exact.get("workspace_complete").flag());
    assert!(!exact.get("truncated").flag()); assert_eq!(exact.get("stored_hits").number(), 1);
    temp.file("c.rs", b"needle");
    let (exit, extra) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle", "--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL); assert!(extra.get("truncated").flag());
    assert_eq!(extra.get("stored_hits").number(), 1); assert_eq!(extra.get("matches_seen").number(), 2);
    assert!(extra.get("files").array()[2].get("hits").array().is_empty());
    let (exit, absent) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "notpresent", "--limit", "0"]);
    assert_eq!(exit, EXIT_NO_MATCH); assert!(absent.get("workspace_complete").flag());
    assert_eq!(absent.get("stored_hits").number(), 0);
}

#[test]
fn count_all_separates_complete_counts_from_retained_rows_across_buffers() {
    let temp = Temp::new();
    let bytes = vec![b'a'; 32 * 1024 + 5];
    let file = temp.file("overlapping.rs", &bytes);
    // Negative control: the unchanged default must still stop at the first
    // excess witness, rather than accidentally opt every search into more I/O.
    let (exit, limited) = invoke(&file, &["--whole-file", "--text", "aa", "--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!limited.get("count_all_matches").flag());
    assert!(!limited.get("match_count_complete").flag());
    assert_eq!(limited.get("matches_seen").number(), 2);
    let (exit, counted) = invoke(&file, &["--whole-file", "--text", "aa", "--limit", "1", "--count-all"]);
    // Exit 3 continues to disclose omitted rows; the count has its own proof.
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(counted.get("count_all_matches").flag());
    assert!(counted.get("whole_file_complete").flag());
    assert!(counted.get("match_count_complete").flag());
    assert!(counted.get("truncated").flag());
    assert_eq!(counted.get("state").text(), "complete-observed-input");
    assert_eq!(counted.get("matches_seen").number(), (bytes.len() - 1) as u64);
    assert_eq!(counted.get("stored_hits").number(), 1);
    assert_eq!(counted.get("hits").array().len(), 1);
    assert_eq!(counted.get("literal_original_hex").text(), "6161");
    assert_eq!(counted.get("payload_bytes_read").number(), bytes.len() as u64);
    assert!(counted.get("peak_input_buffer_bytes").number() <= 16 * 1024);
}

#[test]
fn count_all_visits_later_files_and_counts_utf16_without_retaining_more_hits() {
    let temp = Temp::new();
    temp.file("a.rs", b"banana banana");
    temp.file("b.rs", &utf16("banana", true));
    temp.file("c.rs", &utf16("banana banana", false));
    temp.file("d.rs", b"absent");
    let (exit, counted) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "ana",
        "--limit", "1", "--count-all"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(counted.get("workspace_complete").flag());
    assert!(counted.get("match_count_complete").flag());
    assert!(counted.get("truncated").flag());
    assert_eq!(counted.get("matches_seen").number(), 10);
    assert_eq!(counted.get("stored_hits").number(), 1);
    assert_eq!(counted.get("matching_files").number(), 3);
    assert_eq!(counted.get("files_examined").number(), 4);
    assert_eq!(counted.get("unexamined_files").number(), 0);
    assert_eq!(counted.get("incomplete_files").number(), 0);
    assert_eq!(counted.get("stop_reason"), &Json::Null);
    let files = counted.get("files").array();
    assert_eq!(files[1].get("matches_seen").number(), 2);
    assert!(files[1].get("hits").array().is_empty());
    assert_eq!(files[1].get("literal_original_hex"), &Json::Null);
    assert_eq!(files[2].get("matches_seen").number(), 4);
    assert!(files.iter().all(|file| file.get("whole_file_complete").flag()));
}

#[test]
fn zero_retention_counting_distinguishes_presence_and_complete_absence() {
    let temp = Temp::new(); let file = temp.file("file.rs", b"banana");
    let (exit, counted) = invoke(&file, &["--whole-file", "--text", "ana", "--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(counted.get("match_count_complete").flag());
    assert_eq!(counted.get("matches_seen").number(), 2);
    assert!(counted.get("hits").array().is_empty());
    assert_eq!(counted.get("literal_original_hex"), &Json::Null);
    let (exit, absent) = invoke(&file, &["--whole-file", "--text", "missing", "--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert!(absent.get("match_count_complete").flag());
    assert!(!absent.get("truncated").flag());
    let (exit, exact) = invoke(&file, &["--whole-file", "--text", "ana", "--count-all", "--limit", "2"]);
    assert_eq!(exit, EXIT_OK);
    assert!(exact.get("match_count_complete").flag());
    assert!(!exact.get("truncated").flag());
}

#[test]
fn count_all_does_not_turn_a_scan_budget_into_an_exhaustive_total() {
    let temp = Temp::new();
    temp.file("a.rs", b"aaaaaaaaaa"); temp.file("b.rs", b"aaaaa");
    let (exit, counted) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "aa",
        "--count-all", "--limit", "1", "--max-scan-bytes", "5"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!counted.get("workspace_complete").flag());
    assert!(!counted.get("match_count_complete").flag());
    assert!(counted.get("truncated").flag());
    assert_eq!(counted.get("matches_seen").number(), 4);
    assert_eq!(counted.get("payload_bytes_read").number(), 5);
    assert_eq!(counted.get("unexamined_files").number(), 1);
    assert_eq!(counted.get("stop_reason").text(), "byte-limit");
}

#[test]
fn count_all_preserves_raw_byte_semantics_and_unsupported_text_coverage() {
    let temp = Temp::new();
    temp.file("a.rs", b"aaa"); temp.file("b.rs", b"a\xffa\xff"); temp.file("c.rs", b"aa");
    let (exit, text) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "a", "--count-all", "--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!text.get("match_count_complete").flag());
    assert_eq!(text.get("incomplete_files").number(), 1);
    assert_eq!(text.get("matches_seen").number(), 5);
    assert_eq!(text.get("files_examined").number(), 3);
    let (exit, raw) = invoke(&temp.0, &["--workspace", "--whole-file", "--raw-hex", "ff", "--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(raw.get("match_count_complete").flag());
    assert_eq!(raw.get("matches_seen").number(), 2);
    assert_eq!(raw.get("stored_hits").number(), 0);
}

#[test]
fn count_all_can_cancel_after_the_retained_buffer_has_filled() {
    let temp = Temp::new(); let file = temp.file("file.rs", &vec![b'a'; 128 * 1024]);
    for (query, ignore_case) in [("a", false), ("A", true)] {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let mut flags = vec!["--whole-file", "--text", query, "--count-all", "--limit", "1"];
        if ignore_case { flags.push("--ignore-case"); }
        let mut checks = 0;
        let code = run(&args(&file, &flags), &mut NoStdin, &mut stdout, &mut stderr,
            || { checks += 1; checks > 64 });
        assert_eq!(code, EXIT_CANCELED);
        assert!(stderr.is_empty());
        let error = parse(&stdout).unwrap();
        assert_eq!(error.get("status").text(), "error");
        assert_eq!(error.get("error").get("code").text(), "CLI_CANCELED");
    }
}

#[test]
fn unsupported_text_leaves_other_file_results_useful_and_raw_mode_can_search_it() {
    let temp = Temp::new(); temp.file("a.rs", b"needle"); temp.file("b.rs", b"bad\xff");
    let (exit, text) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle"]);
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(text.get("stored_hits").number(), 1);
    assert_eq!(text.get("files").array()[1].get("state").text(), "unsupported-text");
    assert_eq!(text.get("incomplete_files").number(), 1);
    let (exit, raw) = invoke(&temp.0, &["--workspace", "--whole-file", "--raw-hex", "ff"]);
    assert_eq!(exit, EXIT_OK); assert!(raw.get("workspace_complete").flag());
    assert_eq!(raw.get("files").array()[1].get("hits").array()[0].get("original_range").get("start").number(), 3);
    assert_eq!(raw.get("files").array()[1].get("literal_original_hex").text(), "ff");
}

#[test]
fn workspace_policy_and_raw_unix_names_are_preserved_in_streaming_mode() {
    use std::os::unix::ffi::OsStringExt;
    let temp = Temp::new();
    let name = OsString::from_vec(b"x\n\\\xff.rs".to_vec());
    fs::write(temp.0.join(name), b"needle").unwrap();
    fs::create_dir(temp.0.join("target")).unwrap();
    fs::write(temp.0.join("target/hidden.rs"), b"needle").unwrap();
    let (exit, json) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(json.get("known_files").number(), 1);
    assert_eq!(json.get("files").array()[0].get("path").get("hex").text(), "780a5cff2e7273");
    let (exit, all) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "needle", "--include-excluded"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(all.get("stored_hits").number(), 2);
}

#[test]
fn whole_file_admission_errors_and_cancellation_publish_only_one_error_document() {
    let temp = Temp::new(); let file = temp.file("file.rs", b"needle");
    let (exit, error) = invoke(&file, &["--whole-file", "--text", "needle", "--offset", "0"]);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error.get("status").text(), "error");
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = run(&args(&file, &["--whole-file", "--text", "needle"]), &mut NoStdin,
        &mut stdout, &mut stderr, || true);
    assert_eq!(code, EXIT_CANCELED); assert!(stderr.is_empty());
    let document = parse(&stdout).unwrap();
    assert_eq!(document.get("status").text(), "error");
    assert_eq!(document.get("error").get("code").text(), "CLI_CANCELED");
}

#[test]
fn standalone_executable_delivers_whole_file_and_workspace_search() {
    let temp = Temp::new();
    let mut bytes = vec![b'x'; 1024 * 1024 + 1]; bytes.extend_from_slice(b"needle");
    let file = temp.file("huge.rs", &bytes);
    for (path, flags) in [(&file, vec!["--whole-file", "--text", "needle"]),
        (&temp.0, vec!["--whole-file", "--workspace", "--text", "needle"]),
        (&file, vec!["--whole-file", "--text", "NEEDLE", "--ignore-case"]),
        (&temp.0, vec!["--whole-file", "--workspace", "--text", "NEEDLE", "--ignore-case"])] {
        let result = Command::new(env!("CARGO_BIN_EXE_fcb")).args(args(path, &flags)).output().unwrap();
        assert!(result.status.success(), "stderr={:?}", String::from_utf8_lossy(&result.stderr));
        assert!(result.stderr.is_empty());
        let json = parse(&result.stdout).unwrap();
        assert_eq!(json.get("strategy").text(), "streaming-whole-file");
        if flags.contains(&"--ignore-case") {
            assert_eq!(json.get("mode").text(), "unicode-lowercase-text");
            assert!(!json.get("case_sensitive").flag());
            let report = if flags.contains(&"--workspace") { &json.get("files").array()[0] } else { &json };
            let hit = &report.get("hits").array()[0];
            assert_eq!(hit.get("matched_text").text(), "needle");
            assert_eq!(hit.get("original_hex").text(), "6e6565646c65");
            assert_eq!(bounds(hit), (1024 * 1024 + 1, 1024 * 1024 + 7));
        }
    }
}

#[test]
fn lowercase_file_search_retains_each_original_spelling_in_utf8_and_utf16() {
    let temp = Temp::new();
    for encoding in ["utf8", "utf16le", "utf16be"] {
        let source = encoded("\u{feff}K K k", encoding);
        let file = temp.file("case.rs", &source);
        for declaration in ["auto", encoding] {
            let (exit, json) = invoke(&file, &["--whole-file", "--text", "k", "--ignore-case",
                "--encoding", declaration]);
            assert_eq!(exit, EXIT_OK);
            assert_eq!(json.get("mode").text(), "unicode-lowercase-text");
            assert!(!json.get("case_sensitive").flag());
            assert_eq!(json.get("normalization").text(), "exact");
            assert_eq!(json.get("encoding").text(), encoding);
            assert!(json.get("whole_file_complete").flag());
            assert_eq!(json.get("matches_seen").number(), 3);
            assert_eq!(json.get("literal_text").text(), "k");
            assert_eq!(json.get("literal_original_hex"), &Json::Null);
            let hits = json.get("hits").array();
            let mut offset = encoded("\u{feff}", encoding).len() as u64;
            let mut retained = 0u64;
            for (index, spelling) in ["K", "K", "k"].iter().enumerate() {
                let witness = encoded(spelling, encoding);
                assert_eq!(bounds(&hits[index]), (offset, offset + witness.len() as u64));
                assert_eq!(hits[index].get("matched_text").text(), *spelling);
                assert_eq!(hits[index].get("original_hex").text(), hex(&witness));
                assert_eq!(hits[index].get("decoded_range"), &Json::Null);
                offset += witness.len() as u64 + encoded(" ", encoding).len() as u64;
                retained += witness.len() as u64;
            }
            assert_eq!(json.get("retained_witness_bytes").number(), retained);
        }
        let (exit, exact) = invoke(&file, &["--whole-file", "--text", "k"]);
        assert_eq!(exit, EXIT_OK);
        assert_eq!(exact.get("mode").text(), "exact-decoded-text");
        assert!(exact.get("case_sensitive").flag());
        assert_eq!(exact.get("matches_seen").number(), 1);
        assert_eq!(exact.get("literal_original_hex").text(), hex(&encoded("k", encoding)));
        assert_eq!(bounds(&exact.get("hits").array()[0]),
            ((source.len() - encoded("k", encoding).len()) as u64, source.len() as u64));
        assert_eq!(fs::read(&file).unwrap(), source);
    }
}

#[test]
fn lowercase_scalar_expansions_keep_complete_source_units_without_full_case_folding() {
    let temp = Temp::new();
    let file = temp.file("expansion.rs", "İ i\u{0307} ß SS ss".as_bytes());
    let (exit, scalar) = invoke(&file, &["--whole-file", "--text", "i", "--ignore-case"]);
    assert_eq!(exit, EXIT_OK);
    let hits = scalar.get("hits").array();
    assert_eq!(hits.len(), 2);
    assert_eq!(bounds(&hits[0]), (0, 2));
    assert_eq!(hits[0].get("matched_text").text(), "İ");
    assert_eq!(hits[0].get("original_hex").text(), "c4b0");
    assert_eq!(bounds(&hits[1]), (3, 4));
    assert_eq!(hits[1].get("matched_text").text(), "i");
    assert_ne!(hits[0].get("occurrence_id"), hits[1].get("occurrence_id"));
    let (exit, expanded) = invoke(&file, &["--whole-file", "--text", "i\u{0307}", "--ignore-case"]);
    assert_eq!(exit, EXIT_OK);
    let hits = expanded.get("hits").array();
    assert_eq!(hits.len(), 2);
    assert_eq!(bounds(&hits[0]), (0, 2));
    assert_eq!(bounds(&hits[1]), (3, 6));
    assert_eq!(hits[1].get("matched_text").text(), "i\u{0307}");
    assert_eq!(hits[1].get("original_hex").text(), "69cc87");
    let (exit, exact) = invoke(&file, &["--whole-file", "--text", "i\u{0307}"]);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(exact.get("matches_seen").number(), 1);
    let (exit, lower) = invoke(&file, &["--whole-file", "--text", "ss", "--ignore-case"]);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(lower.get("matches_seen").number(), 2);
    assert_eq!(lower.get("hits").array()[0].get("matched_text").text(), "SS");
    assert_eq!(lower.get("hits").array()[1].get("matched_text").text(), "ss");
    let accents = temp.file("accents.rs", "Ä A\u{0308} ä".as_bytes());
    let (exit, exact_accents) = invoke(&accents, &["--whole-file", "--text", "ä", "--ignore-case"]);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(exact_accents.get("matches_seen").number(), 2);
    assert_eq!(bounds(&exact_accents.get("hits").array()[1]), (7, 9));
}

#[test]
fn lowercase_matches_survive_a_source_scalar_split_at_the_input_buffer_boundary() {
    let temp = Temp::new();
    let prefix = 16 * 1024 - 1;
    let mut source = vec![b'x'; prefix]; source.extend_from_slice("KKk".as_bytes());
    let file = temp.file("split.rs", &source);
    let (exit, json) = invoke(&file, &["--whole-file", "--text", "k", "--ignore-case"]);
    assert_eq!(exit, EXIT_OK);
    assert!(json.get("whole_file_complete").flag());
    assert_eq!(json.get("matches_seen").number(), 3);
    assert_eq!(bounds(&json.get("hits").array()[0]), (prefix as u64, prefix as u64 + 3));
    assert_eq!(json.get("hits").array()[0].get("original_hex").text(), "e284aa");
    assert_eq!(json.get("payload_bytes_read").number(), source.len() as u64);
    assert!(json.get("peak_input_buffer_bytes").number() <= 16 * 1024);
}

#[test]
fn lowercase_workspace_counting_remains_global_with_exact_per_hit_witnesses() {
    let temp = Temp::new();
    temp.file("a.rs", "K K k".as_bytes());
    temp.file("b.rs", &utf16("Kk", true));
    temp.file("c.rs", &utf16("KK", false));
    let (exit, limited) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "k",
        "--ignore-case", "--limit", "1"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!limited.get("match_count_complete").flag());
    assert_eq!(limited.get("matches_seen").number(), 2);
    assert_eq!(limited.get("unexamined_files").number(), 2);
    assert_eq!(limited.get("stop_reason").text(), "match-limit");
    for (limit, retained) in [("0", 0u64), ("1", 1), ("7", 7)] {
        let (exit, json) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "k",
            "--ignore-case", "--count-all", "--limit", limit]);
        assert_eq!(exit, if retained < 7 { EXIT_PARTIAL } else { EXIT_OK });
        assert_eq!(json.get("mode").text(), "unicode-lowercase-text");
        assert!(!json.get("case_sensitive").flag());
        assert_eq!(json.get("normalization").text(), "exact");
        assert!(json.get("workspace_complete").flag());
        assert!(json.get("match_count_complete").flag());
        assert_eq!(json.get("matches_seen").number(), 7);
        assert_eq!(json.get("stored_hits").number(), retained);
        assert_eq!(json.get("matching_files").number(), 3);
        assert_eq!(json.get("files_examined").number(), 3);
        assert_eq!(json.get("unexamined_files").number(), 0);
        assert_eq!(json.get("stop_reason"), &Json::Null);
        for file in json.get("files").array() {
            assert!(file.get("whole_file_complete").flag());
            assert_eq!(file.get("literal_original_hex"), &Json::Null);
            if retained == 0 {
                assert!(file.get("hits").array().is_empty());
                assert_eq!(file.get("retained_witness_bytes").number(), 0);
            }
        }
        if retained == 7 {
            let files = json.get("files").array();
            assert_eq!(files[1].get("hits").array()[0].get("original_hex").text(), "4b00");
            assert_eq!(files[2].get("hits").array()[0].get("original_hex").text(), "212a");
            assert_eq!(files[2].get("hits").array()[0].get("matched_text").text(), "K");
        }
    }
    let (exit, absent) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "absent",
        "--ignore-case", "--count-all", "--limit", "0"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert!(absent.get("match_count_complete").flag());
    assert_eq!(absent.get("matches_seen").number(), 0);
}

#[test]
fn lowercase_counting_preserves_partial_budget_and_malformed_source_reporting() {
    let temp = Temp::new();
    temp.file("a.rs", "K K k".as_bytes()); temp.file("b.rs", b"K");
    let (exit, limited) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "k",
        "--ignore-case", "--count-all", "--limit", "1", "--max-scan-bytes", "5"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!limited.get("match_count_complete").flag());
    assert_eq!(limited.get("payload_bytes_read").number(), 5);
    assert_eq!(limited.get("matches_seen").number(), 2);
    assert_eq!(limited.get("unexamined_files").number(), 1);
    assert_eq!(limited.get("stop_reason").text(), "byte-limit");
    temp.file("c.rs", b"K\xff");
    let (exit, malformed) = invoke(&temp.0, &["--workspace", "--whole-file", "--text", "k",
        "--ignore-case", "--count-all"]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!malformed.get("match_count_complete").flag());
    assert_eq!(malformed.get("matches_seen").number(), 4);
    assert_eq!(malformed.get("incomplete_files").number(), 1);
    assert_eq!(malformed.get("files_examined").number(), 3);
    let rejected = &malformed.get("files").array()[2];
    assert_eq!(rejected.get("state").text(), "unsupported-text");
    assert_eq!(rejected.get("matches_seen").number(), 0);
    assert_eq!(rejected.get("retained_witness_bytes").number(), 0);
    assert!(rejected.get("hits").array().is_empty());
}

#[test]
fn lowercase_whole_file_requests_reject_byte_and_window_options_before_io() {
    let temp = Temp::new(); let missing = temp.0.join("missing.rs");
    for flags in [
        vec!["--whole-file", "--raw-hex", "4b", "--ignore-case"],
        vec!["--workspace", "--whole-file", "--raw-hex", "4b", "--ignore-case"],
        vec!["--text", "k", "--ignore-case"],
        vec!["--whole-file", "--text", "k", "--ignore-case", "--bytes", "4"],
        vec!["--whole-file", "--text", "k", "--ignore-case", "--offset", "0"],
    ] {
        let (exit, error) = invoke(&missing, &flags);
        assert_eq!(exit, EXIT_ERROR);
        assert_eq!(error.get("error").get("code").text(), "CLI_INCOMPATIBLE_OPTIONS");
    }
    let file = temp.file("raw.rs", b"K k");
    let (exit, raw) = invoke(&file, &["--whole-file", "--raw-hex", "4b"]);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(raw.get("mode").text(), "original-bytes");
    assert_eq!(raw.get("case_sensitive"), &Json::Null);
    assert_eq!(raw.get("normalization"), &Json::Null);
    assert_eq!(raw.get("matches_seen").number(), 1);
    assert_eq!(raw.get("literal_original_hex").text(), "4b");
}

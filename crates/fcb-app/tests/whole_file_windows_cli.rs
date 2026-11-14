#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Public CLI match-offset navigation. Oracles use explicit original-byte
//! ranges and ordinary full-scan output, not another implementation of skipping.
mod support;
use std::{ffi::OsString, fs, io::{self, Read}, path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering}};
use fcb_app::{run, EXIT_NO_MATCH, EXIT_PARTIAL};
use support::{Json, parse};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-cli-match-windows-{}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name); fs::write(&path, bytes).unwrap(); path
    }
}
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct NoStdin;
impl Read for NoStdin {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("named-file query read stdin") }
}
fn invoke(path: &Path, flags: &[&str], stream: bool) -> (u8, Vec<Json>) {
    let mut args = vec![OsString::from("search"), path.as_os_str().to_owned(), "--whole-file".into()];
    args.extend(flags.iter().map(OsString::from));
    args.push(if stream { "--stream" } else { "--json" }.into());
    let (mut output, mut error) = (Vec::new(), Vec::new());
    let code = run(&args, &mut NoStdin, &mut output, &mut error, || false);
    assert!(error.is_empty(), "stderr={:?}", String::from_utf8_lossy(&error));
    let rows = if stream {
        output.split(|byte| *byte == b'\n').filter(|line| !line.is_empty())
            .map(|line| parse(line).unwrap()).collect()
    } else { vec![parse(&output).unwrap()] };
    (code, rows)
}
fn json(path: &Path, flags: &[&str]) -> (u8, Json) {
    let (code, mut values) = invoke(path, flags, false);
    (code, values.pop().unwrap())
}
fn bounds(hit: &Json) -> (u64, u64) {
    (hit.get("original_range").get("start").number(), hit.get("original_range").get("end").number())
}
fn window(result: &Json, requested: u64, skipped: u64, more: bool, next: Option<u64>) {
    assert_eq!(result.get("page_start_after").number(), requested);
    assert_eq!(result.get("skipped_matches").number(), skipped);
    assert_eq!(result.get("has_more_matches").flag(), more);
    if let Some(next) = next { assert_eq!(result.get("next_match_offset").number(), next); }
    else { assert_eq!(result.get("next_match_offset"), &Json::Null); }
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    format!("\u{feff}{text}").encode_utf16().flat_map(|unit|
        if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}
fn workspace_rows(result: &Json) -> Vec<(String, u64, u64)> {
    result.get("files").array().iter().flat_map(|file| file.get("hits").array().iter().map(|hit| {
        let (start, end) = bounds(hit);
        (file.get("path").get("hex").text().to_owned(), start, end)
    })).collect()
}

#[test]
fn text_windows_reach_beyond_the_retained_cap_without_larger_buffers() {
    let temp = Temp::new(); let source = "needle ".repeat(4_105);
    let file = temp.file("source.rs", source.as_bytes());
    for counted in [false, true] {
        let mut flags = vec!["--text", "needle", "--skip-matches", "4100", "--limit", "2"];
        if counted { flags.push("--count-all"); }
        let (code, report) = json(&file, &flags);
        assert_eq!(code, EXIT_PARTIAL);
        assert_eq!(report.get("hits").array().iter().map(bounds).collect::<Vec<_>>(),
            [(28_700, 28_706), (28_707, 28_713)]);
        assert_eq!(report.get("hits").array()[0].get("occurrence_id").number(), 4_101);
        assert_eq!(report.get("matches_seen").number(), if counted { 4_105 } else { 4_103 });
        assert_eq!(report.get("match_count_complete").flag(), counted);
        assert!(report.get("peak_input_buffer_bytes").number() <= 16 * 1024);
        assert_eq!(report.get("retained_witness_bytes").number(), 6);
        window(&report, 4_100, 4_100, true, Some(4_102));
    }
}

#[test]
fn final_beyond_end_and_count_only_windows_do_not_advertise_nonprogressing_pages() {
    let temp = Temp::new(); let file = temp.file("source", b"one one one");
    let (code, last) = json(&file, &["--text", "one", "--skip-matches", "2", "--limit", "2"]);
    assert_eq!(code, EXIT_PARTIAL); // Earlier intentional omissions remain explicit.
    assert!(last.get("whole_file_complete").flag() && last.get("truncated").flag());
    assert_eq!(last.get("hits").array().iter().map(bounds).collect::<Vec<_>>(), [(8, 11)]);
    window(&last, 2, 2, false, None);
    let (_, beyond) = json(&file, &["--text", "one", "--skip-matches", "18446744073709551615"]);
    assert!(beyond.get("whole_file_complete").flag() && beyond.get("hits").array().is_empty());
    assert_eq!(beyond.get("matches_seen").number(), 3);
    window(&beyond, u64::MAX, 3, false, None);
    let (_, counted) = json(&file, &["--text", "one", "--skip-matches", "2", "--limit", "0", "--count-all"]);
    assert!(counted.get("match_count_complete").flag());
    assert_eq!(counted.get("matches_seen").number(), 3);
    window(&counted, 2, 2, true, None);
    let (code, absent) = json(&file, &["--text", "absent", "--skip-matches", "2"]);
    assert_eq!(code, EXIT_NO_MATCH); window(&absent, 2, 0, false, None);
}

#[test]
fn byte_offsets_stay_original_for_utf16_overlaps_and_unicode_lowercase() {
    let temp = Temp::new();
    for little in [false, true] {
        let file = temp.file(if little { "le" } else { "be" }, &utf16("banana", little));
        let (_, report) = json(&file, &["--text", "ana", "--skip-matches", "1", "--limit", "1"]);
        assert_eq!(bounds(&report.get("hits").array()[0]), (8, 14));
        assert_eq!(report.get("hits").array()[0].get("occurrence_id").number(), 2);
        window(&report, 1, 1, false, None);
        let file = temp.file(if little { "lower-le" } else { "lower-be" }, &utf16("ÄPFEL äpfel", little));
        let (_, folded) = json(&file, &["--text", "ÄPFEL", "--ignore-case", "--skip-matches", "1", "--limit", "1"]);
        let hit = &folded.get("hits").array()[0];
        assert_eq!(bounds(hit), (14, 24)); assert_eq!(hit.get("matched_text").text(), "äpfel");
        assert_eq!(hit.get("original_hex").text(), if little { "e4007000660065006c00" } else { "00e4007000660065006c" });
    }
}

#[test]
fn raw_and_identifier_windows_keep_their_distinct_match_policies() {
    let temp = Temp::new(); let raw = temp.file("raw", &[0, 255, 0, 255, 0, 255]);
    let (_, bytes) = json(&raw, &["--raw-hex", "00ff", "--skip-matches", "1", "--limit", "1"]);
    assert_eq!(bounds(&bytes.get("hits").array()[0]), (2, 4));
    window(&bytes, 1, 1, true, Some(2));
    let file = temp.file("tokens.rs", b"needle2 needle _needle needle");
    let (_, tokens) = json(&file, &["--identifier", "needle", "--skip-matches", "1", "--limit", "1"]);
    assert_eq!(bounds(&tokens.get("hits").array()[0]), (23, 29));
    assert_eq!(tokens.get("matches_seen").number(), 2);
    window(&tokens, 1, 1, false, None);
}

#[test]
fn workspace_offsets_are_global_and_probe_later_files_after_an_exact_full_window() {
    let temp = Temp::new(); temp.file("a.rs", b"needle needle"); temp.file("b.rs", b"needle needle needle");
    let (_, all) = json(&temp.0, &["--workspace", "--text", "needle", "--count-all"]);
    let oracle = workspace_rows(&all);
    let (_, later) = json(&temp.0, &["--workspace", "--text", "needle", "--skip-matches", "3",
        "--limit", "2", "--count-all"]);
    assert_eq!(workspace_rows(&later), oracle[3..]);
    assert_eq!(later.get("matches_seen").number(), 5);
    assert!(later.get("workspace_complete").flag()); window(&later, 3, 3, false, None);
    let first_file_matches = all.get("files").array()[0].get("matches_seen").number();
    let count = first_file_matches.to_string();
    let (_, first) = json(&temp.0, &["--workspace", "--text", "needle", "--limit", &count]);
    assert_eq!(workspace_rows(&first), oracle[..first_file_matches as usize]);
    assert_eq!(first.get("files_examined").number(), 2);
    window(&first, 0, 0, true, Some(first_file_matches));
}

#[test]
fn excluded_expression_candidates_do_not_spend_the_global_match_offset() {
    let temp = Temp::new(); temp.file("a.rs", b"needle needle required forbidden");
    temp.file("b.rs", b"needle required needle needle"); temp.file("c.py", b"needle required");
    let (_, report) = json(&temp.0, &["--workspace", "--query", "needle required -forbidden lang:rust",
        "--skip-matches", "1", "--limit", "1"]);
    assert_eq!(report.get("matches_seen").number(), 3);
    assert_eq!(report.get("metadata_excluded_files").number(), 1);
    assert_eq!(workspace_rows(&report).iter().map(|(_, start, end)| (*start, *end)).collect::<Vec<_>>(), [(16, 22)]);
    window(&report, 1, 1, true, Some(2));
    let missing = temp.0.join("absent.py");
    let (code, excluded) = json(&missing, &["--query", "needle lang:rust", "--skip-matches", "9"]);
    assert_eq!(code, EXIT_NO_MATCH); assert_eq!(excluded.get("payload_bytes_read").number(), 0);
    window(&excluded, 9, 0, false, None);
}

#[test]
fn skipping_spends_the_original_work_budget_and_never_claims_absence_after_a_partial_scan() {
    let temp = Temp::new(); let file = temp.file("source", b"needle needle needle needle");
    let (_, report) = json(&file, &["--text", "needle", "--skip-matches", "3", "--max-scan-bytes", "8"]);
    assert!(report.get("hits").array().is_empty()); assert_eq!(report.get("payload_bytes_read").number(), 8);
    assert!(!report.get("match_count_complete").flag()); assert!(!report.get("whole_file_complete").flag());
    window(&report, 3, 1, false, None);
    let expression = temp.file("expression", b"needle required needle");
    let (_, partial) = json(&expression, &["--query", "needle required", "--skip-matches", "1", "--max-scan-bytes", "16"]);
    assert_eq!(partial.get("expression_qualified"), &Json::Null);
    assert_eq!(partial.get("matches_seen").number(), 0); window(&partial, 1, 0, false, None);
}

#[test]
fn ndjson_window_receipts_match_ordinary_results_and_keep_candidate_counts_private() {
    let temp = Temp::new(); temp.file("a.rs", b"needle required needle"); temp.file("b.rs", b"needle required needle");
    let flags = ["--workspace", "--query", "needle required", "--skip-matches", "1", "--limit", "2"];
    let (ordinary_code, ordinary) = json(&temp.0, &flags);
    let (code, records) = invoke(&temp.0, &flags, true);
    assert_eq!(code, ordinary_code);
    assert_eq!(records[0].get("type").text(), "start");
    assert_eq!(records[0].get("page_start_after").number(), 1);
    let files: Vec<&Json> = records.iter().filter(|row| row.get("type").text() == "file")
        .map(|row| row.get("file")).collect();
    assert_eq!(files, ordinary.get("files").array().iter().collect::<Vec<_>>());
    let terminal = records.last().unwrap(); assert_eq!(terminal.get("type").text(), "terminal");
    window(terminal, 1, 1, true, Some(3));
    assert!(terminal.get("scan_complete").flag()); assert_eq!(terminal.get("matches_seen").number(), 4);
    let mut finalized = 0;
    for record in &records {
        match record.get("type").text() {
            "file" => finalized += record.get("file").get("matches_seen").number(),
            "progress" => assert_eq!(record.get("matches_seen").number(), finalized),
            _ => {},
        }
    }
}

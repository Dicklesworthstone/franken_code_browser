#![forbid(unsafe_code)]

mod support;
use std::{ffi::OsString, io::{self, Read}};
use fcb_app::{run, EXIT_CANCELED, EXIT_ERROR, EXIT_NO_MATCH, EXIT_OK, EXIT_PARTIAL};
use support::{Json, parse};

fn args(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
fn invoke(values: &[&str], input: &[u8]) -> (u8, Json) {
    invoke_args(&args(values), &mut &input[..])
}
fn invoke_args(values: &[OsString], input: &mut impl Read) -> (u8, Json) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(values, input, &mut stdout, &mut stderr, || false);
    assert!(stderr.is_empty(), "{:?}", String::from_utf8_lossy(&stderr));
    let document = parse(&stdout).unwrap_or_else(|error| panic!("{error}: {:?}", String::from_utf8_lossy(&stdout)));
    assert_eq!(document.get("schema").text(), "fcb.cli/1");
    (exit, document)
}
struct NeverRead;
impl Read for NeverRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("this route must not read stdin") }
}
fn encoded(text: &str, kind: usize) -> Vec<u8> {
    match kind {
        0 => text.as_bytes().to_vec(),
        1 => { let mut bytes = vec![0xef, 0xbb, 0xbf]; bytes.extend_from_slice(text.as_bytes()); bytes }
        _ => {
            let little = kind == 2;
            let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            bytes
        }
    }
}

#[test]
fn stdin_lowercase_search_preserves_bom_utf16_and_expansion_source_coordinates() {
    let source = "İİ KeLViN\r\nBaNaNa😀";
    for kind in 0..4 {
        let bytes = encoded(source, kind);
        for (needle, expected) in [("i\u{307}i\u{307}", vec![(0, 4)]),
            ("\u{307}", vec![(0, 2), (2, 4)]), ("ANA", vec![(16, 19), (18, 21)])] {
            let (exit, result) = invoke(&["search", "--stdin", "--text", needle, "--ignore-case", "--count-all", "--json"], &bytes);
            assert_eq!(exit, EXIT_OK);
            assert!(!result.get("case_sensitive").flag());
            assert_eq!(result.get("normalization").text(), "exact");
            assert_eq!(result.get("case_insensitive_semantics").text(), "unicode-lowercase");
            assert!(result.get("count_all_matches").flag());
            assert!(result.get("scope_match_count_complete").flag());
            assert!(result.get("match_count_complete").flag());
            assert!(result.get("whole_file_complete").flag());
            assert_eq!(result.get("matches_seen").number(), expected.len() as u64);
            assert_eq!(result.get("scanned_input_bytes").number(), source.len() as u64);
            assert_eq!(result.get("input_domain").text(), "window-utf8");
            for (index, (hit, &(start, end))) in result.get("hits").array().iter().zip(&expected).enumerate() {
                let original_offset = |offset| match kind {
                    0 => offset as u64, 1 => offset as u64 + 3,
                    _ => 2 + (source[..offset].encode_utf16().count() * 2) as u64,
                };
                assert_eq!(hit.get("occurrence_id").number(), index as u64 + 1);
                assert_eq!(hit.get("original_range").get("start").number(), original_offset(start));
                assert_eq!(hit.get("original_range").get("end").number(), original_offset(end));
                assert_eq!(hit.get("window_utf8_range").get("start").number(), start as u64);
                assert_eq!(hit.get("window_utf8_range").get("end").number(), end as u64);
            }
        }
        // Existing case-sensitive text is a negative control for flag routing.
        let (exit, result) = invoke(&["search", "--stdin", "--text", "ANA", "--json"], &bytes);
        assert_eq!(exit, EXIT_NO_MATCH); assert_eq!(result.get("matches_seen").number(), 0);
        assert!(result.get("case_sensitive").flag());
        assert_eq!(result.get("case_insensitive_semantics"), &Json::Null);
    }
}

#[test]
fn count_all_is_complete_independently_of_retained_rows_and_zero_limit_exit_status() {
    for (source, insensitive) in [("bananana", false), ("BaNaNaNa", true)] {
        for limit in 0..=4 {
            let limit_text = limit.to_string();
            let mut values = vec!["search", "--stdin", "--text", "ana", "--count-all", "--limit", &limit_text, "--json"];
            if insensitive { values.push("--ignore-case"); }
            let (exit, result) = invoke(&values, source.as_bytes());
            assert_eq!(exit, if limit < 3 { EXIT_PARTIAL } else { EXIT_OK });
            assert_eq!(result.get("matches_seen").number(), 3);
            assert_eq!(result.get("stored_hits").number(), limit.min(3));
            assert_eq!(result.get("truncated").flag(), limit < 3);
            assert!(result.get("scope_complete").flag()); assert!(result.get("whole_file_complete").flag());
            assert!(result.get("scope_match_count_complete").flag()); assert!(result.get("match_count_complete").flag());
        }
        let mut values = vec!["search", "--stdin", "--text", "ana", "--limit", "1", "--json"];
        if insensitive { values.push("--ignore-case"); }
        let (exit, result) = invoke(&values, source.as_bytes());
        assert_eq!(exit, EXIT_PARTIAL); assert_eq!(result.get("matches_seen").number(), 2);
        assert!(!result.get("scope_complete").flag()); assert!(!result.get("match_count_complete").flag());
        assert!(!result.get("count_all_matches").flag());
    }
    for bytes in [&b"banana"[..], &[][..], &[0xef, 0xbb, 0xbf], &[0xff, 0xfe], &[0xfe, 0xff]] {
        let (exit, result) = invoke(&["search", "--stdin", "--text", "absent", "--ignore-case",
            "--count-all", "--limit", "0", "--json"], bytes);
        assert_eq!(exit, EXIT_NO_MATCH);
        assert_eq!(result.get("matches_seen").number(), 0);
        assert!(result.get("match_count_complete").flag()); assert!(!result.get("truncated").flag());
    }
}

#[test]
fn raw_counting_is_exact_and_does_not_silently_apply_text_options() {
    let (exit, result) = invoke(&["search", "--stdin", "--raw-hex", "ffff",
        "--count-all", "--limit", "0", "--json"], &[0xff, 0xff, 0xff, 0xff]);
    assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(result.get("matches_seen").number(), 3);
    assert_eq!(result.get("stored_hits").number(), 0);
    assert!(result.get("scope_complete").flag()); assert!(result.get("match_count_complete").flag());
    assert!(result.get("truncated").flag());
    assert_eq!(result.get("mode").text(), "original-bytes");
    assert_eq!(result.get("input_domain").text(), "original-bytes");
    assert_eq!(result.get("case_sensitive"), &Json::Null);
    assert_eq!(result.get("normalization"), &Json::Null);
    let (exit, result) = invoke(&["search", "--stdin", "--raw-hex", "ffff",
        "--count-all", "--limit", "4", "--json"], &[0xff, 0xff, 0xff, 0xff]);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result.get("hits").array().len(), 3);
    for (index, hit) in result.get("hits").array().iter().enumerate() {
        assert_eq!(hit.get("original_range").get("start").number(), index as u64);
        assert_eq!(hit.get("original_range").get("end").number(), index as u64 + 2);
        assert_eq!(hit.get("window_utf8_range"), &Json::Null);
    }
    for values in [
        vec!["search", "--stdin", "--raw-hex", "ff", "--ignore-case", "--json"],
        vec!["search", "--stdin", "--raw-hex", "ff", "--count-all", "--ignore-case", "--json"],
        vec!["search", "--stdin", "--query", "needle", "--ignore-case", "--json"],
        vec!["search", "file", "--query", "needle", "--count-all", "--json"],
        vec!["read", "--stdin", "--count-all", "--json"],
        vec!["search", "--stdin", "--text", "x", "--count-all", "--max-scan-bytes", "4", "--json"],
    ] {
        let (exit, result) = invoke_args(&args(&values), &mut NeverRead);
        assert_eq!(exit, EXIT_ERROR);
        assert_eq!(result.get("error").get("code").text(), "CLI_INCOMPATIBLE_OPTIONS");
    }
}

#[test]
fn malformed_decoding_and_cancellation_do_not_become_complete_no_matches() {
    for bytes in [&b"A\xffB"[..], &[0xff, 0xfe, 0x00, 0xd8], &[0xfe, 0xff, 0xd8, 0x00]] {
        let (exit, result) = invoke(&["search", "--stdin", "--text", "absent", "--ignore-case",
            "--count-all", "--limit", "0", "--json"], bytes);
        assert_eq!(exit, EXIT_ERROR);
        assert_eq!(result.get("error").get("code").text(), "EXTENT_QUERY_UNSUPPORTED_TEXT");
    }
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(run(&args(&["search", "--stdin", "--text", "x", "--ignore-case", "--count-all", "--json"]),
        &mut NeverRead, &mut stdout, &mut stderr, || true), EXIT_CANCELED);
    assert_eq!(parse(&stdout).unwrap().get("status").text(), "error");
    assert!(stderr.is_empty());
}

#[test]
fn flag_words_remain_literal_needles_and_human_counts_explain_omitted_rows() {
    for needle in ["--ignore-case", "--count-all"] {
        let (exit, result) = invoke(&["search", "--stdin", "--text", needle, "--json"], needle.as_bytes());
        assert_eq!(exit, EXIT_OK); assert_eq!(result.get("matches_seen").number(), 1);
        assert!(result.get("case_sensitive").flag()); assert!(!result.get("count_all_matches").flag());
    }
    for needle in ["STRASSE", "é"] {
        let (exit, result) = invoke(&["search", "--stdin", "--text", needle, "--ignore-case", "--json"], "Straße e\u{301}".as_bytes());
        assert_eq!(exit, EXIT_NO_MATCH); assert_eq!(result.get("matches_seen").number(), 0);
    }
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(run(&args(&["search", "--stdin", "--text", "ana", "--ignore-case", "--count-all", "--limit", "0"]),
        &mut &b"BaNaNaNa"[..], &mut stdout, &mut stderr, || false), EXIT_PARTIAL);
    let rendered = String::from_utf8(stdout).unwrap();
    assert!(rendered.contains("Matches seen: 3 (complete observed-file count)"));
    assert!(rendered.contains("Result rows omitted by --limit"));
    assert!(!rendered.contains("lookahead count"));
    assert!(stderr.is_empty());
}

#[cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
mod files {
    use super::*;
    use std::{fs::{self, File}, io::{Seek, SeekFrom, Write}, path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering}};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("fcb window options {} {}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&root).unwrap(); Self(root)
        }
        fn path(&self, name: &str) -> PathBuf { self.0.join(name) }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { fs::remove_dir_all(&self.0).expect("remove only this test's uniquely owned fixture"); }
    }
    fn search(path: &Path, options: &[&str]) -> (u8, Json) {
        let mut values = args(&["search"]); values.push(path.as_os_str().to_owned()); values.extend(args(options));
        invoke_args(&values, &mut NeverRead)
    }

    #[test]
    fn named_file_counts_agree_with_stdin_and_keep_source_read_only() {
        let fixture = Fixture::new(); let path = fixture.path("source ü.rs");
        let bytes = encoded("İİ\r\nBaNaNa", 3); fs::write(&path, &bytes).unwrap();
        let (exit, result) = search(&path, &["--text", "ANA", "--ignore-case", "--count-all", "--limit", "1", "--json"]);
        let (stdin_exit, stdin) = invoke(&["search", "--stdin", "--text", "ANA", "--ignore-case", "--count-all", "--limit", "1", "--json"], &bytes);
        assert_eq!(exit, EXIT_PARTIAL); assert_eq!(exit, stdin_exit);
        for field in ["hits", "matches_seen", "stored_hits", "scope_complete", "truncated",
            "whole_file_complete", "scope_match_count_complete", "match_count_complete"] {
            assert_eq!(result.get(field), stdin.get(field), "{field}");
        }
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
    }

    #[test]
    fn far_window_counts_never_claim_a_whole_file_total_or_search_read_ahead_context() {
        let fixture = Fixture::new(); let path = fixture.path("large.rs");
        let base = (1u64 << 32) + 33;
        let mut file = File::create(&path).unwrap();
        file.set_len(base + 80).unwrap();
        file.seek(SeekFrom::Start(base)).unwrap();
        file.write_all("KelVin\r\nBaNaNa".as_bytes()).unwrap(); drop(file);
        let offset = base.to_string();
        let (exit, result) = search(&path, &["--text", "ANA", "--ignore-case", "--count-all", "--limit", "1",
            "--offset", &offset, "--bytes", "16", "--encoding", "utf8", "--json"]);
        assert_eq!(exit, EXIT_PARTIAL);
        assert_eq!(result.get("matches_seen").number(), 2); assert_eq!(result.get("stored_hits").number(), 1);
        assert!(result.get("scope_complete").flag()); assert!(result.get("scope_match_count_complete").flag());
        assert!(!result.get("whole_file_complete").flag()); assert!(!result.get("match_count_complete").flag());
        assert!(result.get("truncated").flag());
        assert!(result.get("payload_bytes_read").number() <= 32, "16 visible bytes plus eight context bytes on each side");
        assert_eq!(result.get("hits").array()[0].get("original_range").get("start").number(), base + 11);
        assert_eq!(result.get("hits").array()[0].get("original_range").get("end").number(), base + 14);

        let (exit, result) = search(&path, &["--text", "ANA", "--ignore-case", "--count-all",
            "--offset", &offset, "--bytes", "12", "--encoding", "utf8", "--json"]);
        assert_eq!(exit, EXIT_PARTIAL);
        assert_eq!(result.get("matches_seen").number(), 0, "decoder halo bytes are not admitted search scope");
        assert!(result.get("scope_match_count_complete").flag()); assert!(!result.get("match_count_complete").flag());
        assert!(!result.get("truncated").flag());
    }
}

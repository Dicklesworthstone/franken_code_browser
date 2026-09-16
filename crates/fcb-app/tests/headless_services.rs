#![forbid(unsafe_code)]

mod support;
use std::{cell::Cell, ffi::OsString, io::{self, Read, Write}, rc::Rc};
use fcb_app::{run, EXIT_CANCELED, EXIT_ERROR, EXIT_NO_MATCH, EXIT_OK, EXIT_PARTIAL};
use support::{Json, parse};

fn args(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
fn invoke(values: &[&str], input: &[u8]) -> (u8, Json, Vec<u8>) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(&args(values), &mut &input[..], &mut stdout, &mut stderr, || false);
    let document = parse(&stdout).unwrap_or_else(|error| panic!("{error}: {:?}", String::from_utf8_lossy(&stdout)));
    assert_eq!(document.get("schema").text(), "fcb.cli/1");
    (exit, document, stderr)
}
struct NeverRead;
impl Read for NeverRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("this route must not read stdin") }
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}

#[test]
fn capabilities_and_static_doctor_are_real_inert_commands_not_gui_launches() {
    for values in [&["--json"][..], &["capabilities", "--json"], &["doctor", "--json"]] {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(run(&args(values), &mut NeverRead, &mut stdout, &mut stderr, || false), EXIT_OK);
        let document = parse(&stdout).unwrap();
        assert!(!document.get("native_ready").flag());
        assert!(!document.get("source_scanned").flag());
        let features = document.get("features").array();
        assert_eq!(features.len(), 12);
        assert!(features.iter().any(|row| row.get("name").text() == "exact-window-text-search"
            && row.get("state").text() == "implemented-unqualified"));
        assert!(features.iter().any(|row| row.get("name").text() == "native-gui"
            && row.get("state").text() == "unavailable"));
        assert!(stderr.is_empty());
    }
}

#[test]
fn all_three_decoders_read_real_bytes_and_map_overlapping_hits_exactly() {
    let text = "line\nbanana😀";
    for (bytes, offsets) in [(text.as_bytes().to_vec(), vec![(6, 9), (8, 11)]),
        (utf16(text, true), vec![(14, 20), (18, 24)]), (utf16(text, false), vec![(14, 20), (18, 24)])] {
        let (exit, read, stderr) = invoke(&["read", "--stdin", "--json"], &bytes);
        assert_eq!(exit, EXIT_OK); assert!(stderr.is_empty());
        assert_eq!(read.get("text").text(), text);
        assert!(read.get("whole_file_complete").flag());
        assert_eq!(read.get("observed_length").number(), bytes.len() as u64);
        assert!(!read.get("has_replacements").flag());
        let (exit, result, stderr) = invoke(&["search", "--stdin", "--text", "ana", "--json"], &bytes);
        assert_eq!(exit, EXIT_OK); assert!(stderr.is_empty());
        assert!(result.get("scope_complete").flag()); assert!(result.get("whole_file_complete").flag());
        assert_eq!(result.get("matches_seen").number(), 2);
        let hits = result.get("hits").array(); assert_eq!(hits.len(), 2);
        for (index, (hit, (start, end))) in hits.iter().zip(&offsets).enumerate() {
            assert_eq!(hit.get("occurrence_id").number(), index as u64 + 1);
            assert_eq!(hit.get("original_range").get("start").number(), *start);
            assert_eq!(hit.get("original_range").get("end").number(), *end);
            assert_eq!(hit.get("window_utf8_range").get("start").number(), [6, 8][index]);
        }
    }
}

#[test]
fn exact_limit_is_not_truncation_and_one_lookahead_is_not_an_exhaustive_count() {
    for (limit, expected_exit, seen, stored, truncated) in [("0", EXIT_PARTIAL, 1, 0, true),
        ("1", EXIT_PARTIAL, 2, 1, true), ("2", EXIT_OK, 2, 2, false), ("3", EXIT_OK, 2, 2, false)] {
        let (exit, result, _) = invoke(&["search", "--stdin", "--text", "ana", "--limit", limit, "--json"], b"banana");
        assert_eq!(exit, expected_exit);
        assert_eq!(result.get("matches_seen").number(), seen);
        assert_eq!(result.get("stored_hits").number(), stored);
        assert_eq!(result.get("truncated").flag(), truncated);
        assert_eq!(result.get("scope_complete").flag(), !truncated);
    }
    let (exit, result, _) = invoke(&["search", "--stdin", "--text", "absent", "--limit", "0", "--json"], b"banana");
    assert_eq!(exit, EXIT_NO_MATCH); assert!(result.get("whole_file_complete").flag());
    assert_eq!(result.get("matches_seen").number(), 0);
}

#[test]
fn malformed_text_is_not_false_no_match_and_raw_search_remains_exact() {
    let (exit, result, _) = invoke(&["search", "--stdin", "--text", "absent", "--json"], b"a\xffb");
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(result.get("error").get("code").text(), "EXTENT_QUERY_UNSUPPORTED_TEXT");
    let (exit, result, _) = invoke(&["search", "--stdin", "--raw-hex", "ff", "--json"], b"a\xffb");
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("hits").array().len(), 1);
    assert_eq!(result.get("hits").array()[0].get("original_range").get("start").number(), 1);
    assert_eq!(result.get("hits").array()[0].get("window_utf8_range"), &Json::Null);
    let (exit, result, _) = invoke(&["read", "--stdin", "--json"], b"a\xffb");
    assert_eq!(exit, EXIT_OK); assert!(result.get("has_replacements").flag());
    assert_eq!(result.get("original_hex").text(), "61ff62");
}

#[test]
fn empty_and_bom_only_observations_keep_exact_empty_text_semantics() {
    for bytes in [&[][..], &[0xef, 0xbb, 0xbf], &[0xff, 0xfe], &[0xfe, 0xff]] {
        let (exit, result, _) = invoke(&["read", "--stdin", "--json"], bytes);
        assert_eq!(exit, EXIT_OK); assert_eq!(result.get("text").text(), "");
        assert!(result.get("whole_file_complete").flag());
        let (exit, result, _) = invoke(&["search", "--stdin", "--text", "x", "--json"], bytes);
        assert_eq!(exit, EXIT_NO_MATCH); assert!(result.get("scope_complete").flag());
    }
}

#[test]
fn bounded_stdin_uses_eof_evidence_instead_of_lying_about_truncated_input() {
    let (exit, result, _) = invoke(&["read", "--stdin", "--bytes", "4", "--json"], b"abcd");
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("observed_length").number(), 4);
    let (exit, result, _) = invoke(&["read", "--stdin", "--bytes", "4", "--json"], b"abcde");
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("error").get("code").text(), "CLI_INPUT_LIMIT");
    assert!(!result.get("complete").flag());
}

#[test]
fn machine_strings_round_trip_controls_quotes_and_non_ascii_without_extra_documents() {
    let text = "\0\u{0001}\"\\\r\n\t café 😀 \u{202e}";
    let (exit, result, _) = invoke(&["read", "--stdin", "--json"], text.as_bytes());
    assert_eq!(exit, EXIT_OK); assert_eq!(result.get("text").text(), text);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(run(&args(&["read", "--stdin"]), &mut "a\u{001b}[2J\u{202e}".as_bytes(),
        &mut stdout, &mut stderr, || false), EXIT_OK);
    assert!(!stdout.contains(&0x1b));
    assert!(!String::from_utf8(stdout).unwrap().contains('\u{202e}'));
}

#[test]
fn rejected_arguments_never_touch_input_or_echo_secret_bearing_arguments() {
    for values in [vec!["search", "--stdin", "--json"], vec!["read", "--stdin", "--bytes", "999999", "--json"],
        vec!["cache", "clear", "PRIVATE_SECRET_PATH", "--json"]] {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(run(&args(&values), &mut NeverRead, &mut stdout, &mut stderr, || false), EXIT_ERROR);
        assert_eq!(parse(&stdout).unwrap().get("status").text(), "error");
        assert!(!String::from_utf8(stdout).unwrap().contains("PRIVATE_SECRET_PATH"));
        assert!(stderr.is_empty());
    }
}

#[test]
fn cancellation_before_and_during_input_returns_only_a_terminal_error() {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(run(&args(&["read", "--stdin", "--json"]), &mut NeverRead, &mut stdout, &mut stderr, || true), EXIT_CANCELED);
    assert_eq!(parse(&stdout).unwrap().get("error").get("code").text(), "CLI_CANCELED");
    struct CancelAfterRead(Rc<Cell<bool>>);
    impl Read for CancelAfterRead {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> { bytes[0] = b'x'; self.0.set(true); Ok(1) }
    }
    let flag = Rc::new(Cell::new(false));
    stdout.clear(); stderr.clear();
    assert_eq!(run(&args(&["read", "--stdin", "--json"]), &mut CancelAfterRead(flag.clone()),
        &mut stdout, &mut stderr, || flag.get()), EXIT_CANCELED);
    assert_eq!(parse(&stdout).unwrap().get("status").text(), "error");
}

#[test]
fn broken_stdout_does_not_append_a_second_json_error_document() {
    struct Broken { bytes: Vec<u8> }
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.bytes.is_empty() { return Err(io::ErrorKind::BrokenPipe.into()); }
            self.bytes.extend_from_slice(&bytes[..3]); Ok(3)
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let mut out = Broken { bytes: Vec::new() }; let mut err = Vec::new();
    assert_eq!(run(&args(&["capabilities", "--json"]), &mut NeverRead, &mut out, &mut err, || false), EXIT_ERROR);
    assert_eq!(out.bytes.len(), 3); assert!(parse(&out.bytes).is_err());
    assert!(String::from_utf8(err).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
}

#[test]
fn negative_controls_detect_duplicate_keys_truncation_and_unquoted_large_ids() {
    for broken in [b"{\"x\":true,\"x\":false}".as_slice(), b"{}{}", b"{\"x\":", b"{\"id\":9007199254740993}"] {
        assert!(parse(broken).is_err());
    }
    assert_eq!(parse(b"{\"id\":\"18446744073709551615\"}").unwrap().get("id").number(), u64::MAX);
}

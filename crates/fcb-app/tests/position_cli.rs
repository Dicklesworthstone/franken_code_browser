#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Real CLI routes and independently constructed original-byte expectations.
//! Execution belongs to the independent strict-RCH verification lane.
mod support;

use std::{ffi::OsString, fs, io::{self, Read, Write},
    os::unix::{ffi::OsStringExt, fs::FileExt}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{EXIT_OK, EXIT_NO_MATCH, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use support::{Json, parse};

struct NoStdin;
impl Read for NoStdin {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("named position reads never read stdin") }
}
struct Fixture { root: PathBuf, path: PathBuf }
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-cli-position-{}-{now}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        let path = root.join("source"); fs::write(&path, bytes).unwrap();
        Self { root, path }
    }
    fn argv(&self, command: &str, options: &[&str]) -> Vec<OsString> {
        let mut args = vec![OsString::from(command), self.path.clone().into_os_string()];
        args.extend(options.iter().map(OsString::from)); args
    }
    fn run(&self, command: &str, options: &[&str]) -> (u8, Json) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = fcb_app::run(&self.argv(command, options), &mut NoStdin, &mut out, &mut err, || false);
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        (exit, parse(&out).unwrap())
    }
}
fn encode(text: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "utf8" => text.as_bytes().to_vec(),
        "utf16le" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        "utf16be" => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => unreachable!(),
    }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(value: &Json) -> (usize, usize) {
    (value.get("start").number() as usize, value.get("end").number() as usize)
}
fn assert_page(page: &Json, bytes: &[u8], expected: usize, max_bytes: usize) {
    assert_eq!(page.get("schema").text(), "fcb.cli/1");
    assert_eq!(page.get("command").text(), "read-position");
    assert_eq!(page.get("scope").text(), "named-file");
    assert_eq!(page.get("state").text(), "resolved");
    assert!(page.get("position_complete").flag());
    assert!(!page.get("whole_file_immutable").flag());
    assert_eq!(page.get("original_offset").number() as usize, expected);
    let (start, end) = bounds(page.get("visible_range"));
    let (capture_start, capture_end) = bounds(page.get("captured_range"));
    assert!(capture_start <= start && start <= expected && expected <= end && end <= capture_end);
    assert!(end - start <= max_bytes && capture_end - capture_start <= max_bytes + 8);
    assert_eq!(page.get("original_hex").text(), hex(&bytes[start..end]));
    let text = page.get("text").text();
    assert!(text.is_char_boundary(page.get("window_utf8_offset").number() as usize));
    assert!(page.get("scanned_input_bytes").number() <= page.get("payload_bytes_read").number());
    assert!(page.get("payload_bytes_read").number() <= page.get("max_scan_bytes").number());
}

#[test]
fn typed_columns_round_trip_original_bytes_across_encodings_and_line_endings() {
    let prefix = "\u{feff}first\r\n";
    let body = "\tAé🦀e\u{301}\u{202e}z";
    for encoding in ["utf8", "utf16le", "utf16be"] {
        let bytes = encode(&format!("{prefix}{body}\rlast\n"), encoding);
        let fixture = Fixture::new(&bytes);
        for at in body.char_indices().map(|(at, _)| at).chain(std::iter::once(body.len())) {
            let preceding = &body[..at];
            let expected = encode(&format!("{prefix}{preceding}"), encoding).len();
            for (kind, column, wire_kind) in [
                ("scalar", preceding.chars().count() + 1, "unicode_scalar"),
                ("utf16", preceding.encode_utf16().count() + 1, "utf16_code_unit"),
                ("utf8", preceding.len() + 1, "decoded_utf8_byte"),
            ] {
                let (exit, page) = fixture.run("read", &["--line", "2", "--column", &column.to_string(),
                    "--column-kind", kind, "--bytes", "32", "--max-scan-bytes", "1024", "--json"]);
                assert_eq!(exit, EXIT_OK);
                assert_page(&page, &bytes, expected, 32);
                assert_eq!(page.get("encoding").text(), encoding);
                assert_eq!(page.get("column_kind").text(), wire_kind);
                assert_eq!(page.get("requested_column").number(), column as u64);
                assert_eq!(page.get("line_start").number() as usize, encode(prefix, encoding).len());
            }
        }
    }
}

#[test]
fn first_line_bom_columns_and_explicit_bomless_utf16_are_exact() {
    for encoding in ["utf8", "utf16le", "utf16be"] {
        let bytes = encode("\u{feff}🦀target", encoding);
        let fixture = Fixture::new(&bytes);
        let (exit, page) = fixture.run("read", &["--column", "1", "--bytes", "4", "--json"]);
        assert_eq!(exit, EXIT_OK);
        assert_page(&page, &bytes, encode("\u{feff}", encoding).len(), 4);
    }
    for encoding in ["utf16le", "utf16be"] {
        let bytes = encode("é🦀target", encoding);
        let fixture = Fixture::new(&bytes);
        let (exit, page) = fixture.run("open", &["--line", "1", "--column", "4", "--column-kind", "utf16",
            "--encoding", encoding, "--bytes", "8", "--json"]);
        assert_eq!(exit, EXIT_OK);
        assert_page(&page, &bytes, encode("é🦀", encoding).len(), 8);
    }
}

#[test]
fn huge_line_navigation_retains_small_context_beyond_full_capture_limits() {
    let fixture = Fixture::new(b"");
    let file = fs::OpenOptions::new().write(true).open(&fixture.path).unwrap();
    let prefix = 32 * 1024 * 1024u64 + 3;
    file.set_len(prefix + 64).unwrap();
    let tail = "🦀target\r\nlast".as_bytes();
    assert_eq!(file.write_at(tail, prefix).unwrap(), tail.len());
    drop(file);
    let column = (prefix + 2).to_string();
    let (exit, page) = fixture.run("read", &["--line", "1", "--column", &column,
        "--bytes", "16", "--max-scan-bytes", "67108864", "--json"]);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(page.get("original_offset").number(), prefix + 4);
    assert!(page.get("text").text().contains("🦀target"));
    let (start, end) = bounds(page.get("visible_range"));
    assert!(end - start <= 16);
    let (start, end) = bounds(page.get("captured_range"));
    assert!(end - start <= 24);
    assert!(page.get("payload_bytes_read").number() >= prefix + 4);
    assert!(page.get("payload_bytes_read").number() <= 67_108_864);
}

#[test]
fn missing_line_column_and_unavailable_scan_context_have_distinct_outcomes() {
    let fixture = Fixture::new(b"abc");
    for (options, exit, state) in [
        (vec!["--line", "2", "--column", "1"], EXIT_NO_MATCH, "missing-line"),
        (vec!["--line", "1", "--column", "5"], EXIT_NO_MATCH, "missing-column"),
        (vec!["--line", "1", "--column", "4", "--bytes", "4", "--max-scan-bytes", "3"],
            EXIT_PARTIAL, "scan-byte-limit"),
    ] {
        let mut options = options; options.push("--json");
        let (actual, page) = fixture.run("read", &options);
        assert_eq!(actual, exit);
        assert_eq!(page.get("state").text(), state);
        assert_eq!(page.get("position_complete").flag(), exit != EXIT_PARTIAL);
        assert_eq!(page.get("original_offset"), &Json::Null);
        assert_eq!(page.get("text"), &Json::Null);
    }
    let (exit, page) = fixture.run("read", &["--column", "4", "--bytes", "4",
        "--max-scan-bytes", "4", "--json"]);
    assert_eq!(exit, EXIT_OK); assert_page(&page, b"abc", 3, 4);
    assert!(page.get("scan_reached_eof").flag());
    fs::write(&fixture.path, b"abc\r\n").unwrap();
    let (exit, page) = fixture.run("read-lines", &["--line", "2", "--column", "1", "--json"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert_eq!(page.get("state").text(), "missing-line");
}

#[test]
fn interior_encoded_scalar_positions_are_refused_instead_of_rounded() {
    for encoding in ["utf8", "utf16le", "utf16be"] {
        let fixture = Fixture::new(&encode("🦀x", encoding));
        for kind in ["utf8", "utf16"] {
            let (exit, page) = fixture.run("read", &["--column", "2", "--column-kind", kind,
                "--encoding", encoding, "--json"]);
            assert_eq!(exit, EXIT_ERROR);
            assert_eq!(page.get("status").text(), "error");
        }
    }
}

#[test]
fn parser_rejects_ambiguous_or_unbounded_options_and_keeps_u64_coordinates() {
    let fixture = Fixture::new(b"source");
    for options in [
        vec!["--column", "0"], vec!["--column", "01"],
        vec!["--column", "18446744073709551616"], vec!["--column-kind", "utf16"],
        vec!["--column", "1", "--column-kind", "visual"],
        vec!["--column", "1", "--lines", "1"], vec!["--column", "1", "--bytes", "3"],
        vec!["--column", "1", "--column", "2"], vec!["--column", "1", "--offset", "0"],
        vec!["--column", "1", "--workspace"], vec!["--column", "1", "--stdin"],
    ] {
        let mut options = options; options.push("--json");
        assert_eq!(fixture.run("read", &options).0, EXIT_ERROR, "{options:?}");
    }
    let (exit, page) = fixture.run("read", &["--line", "18446744073709551615", "--column", "1", "--json"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert_eq!(page.get("requested_line").text(), "18446744073709551615");
    let (exit, page) = fixture.run("read", &["--column", "9007199254740993", "--json"]);
    assert_eq!(exit, EXIT_NO_MATCH);
    assert_eq!(page.get("requested_column").text(), "9007199254740993");
}

#[test]
fn raw_paths_and_escaped_human_output_preserve_selected_source() {
    let mut fixture = Fixture::new(b"unused");
    let name = OsString::from_vec(b"--column\n\xff".to_vec());
    fixture.path = fixture.root.join(name);
    fs::write(&fixture.path, "é\u{1b}\n").unwrap();
    let (exit, page) = fixture.run("read", &["--column", "2", "--json"]);
    assert_eq!(exit, EXIT_OK);
    assert_eq!(page.get("original_offset").number(), 2);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = fcb_app::run(&fixture.argv("read", &["--column", "2"]), &mut NoStdin,
        &mut out, &mut err, || false);
    assert_eq!(exit, EXIT_OK); assert!(err.is_empty());
    assert!(!out.contains(&0x1b));
    assert!(String::from_utf8(out).unwrap().contains("original byte 2"));
}

#[test]
fn cancellation_and_broken_transport_never_publish_a_second_document() {
    let fixture = Fixture::new(&vec![b'x'; 128 * 1024]);
    let args = fixture.argv("read", &["--column", "100000", "--bytes", "16", "--json"]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = fcb_app::run(&args, &mut NoStdin, &mut out, &mut err, || true);
    assert_eq!(exit, EXIT_CANCELED);
    assert_eq!(parse(&out).unwrap().get("status").text(), "error");

    struct Broken { bytes: Vec<u8>, remaining: usize }
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 { return Err(io::Error::from(io::ErrorKind::BrokenPipe)); }
            let count = bytes.len().min(self.remaining);
            self.bytes.extend_from_slice(&bytes[..count]); self.remaining -= count; Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let mut broken = Broken { bytes: Vec::new(), remaining: 11 };
    err.clear();
    let exit = fcb_app::run(&args, &mut NoStdin, &mut broken, &mut err, || false);
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(broken.bytes.len(), 11);
    assert!(parse(&broken.bytes).is_err());
    assert!(String::from_utf8(err).unwrap().contains("CLI_OUTPUT_INTERRUPTED"));
}

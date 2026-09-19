#![forbid(unsafe_code)]

mod support;
use std::{ffi::OsString, io::{self, Read, Write}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED};
use support::{Json, parse};

fn args(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
fn invoke(mut input: &[u8]) -> (u8, Vec<Json>, Vec<u8>) {
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let exit = run(&args(&["desk", "--stdio"]), &mut input, &mut stdout, &mut stderr, || false);
    let records = stdout.split(|b| *b == b'\n').filter(|line| !line.is_empty()).map(|line| {
        let json = parse(line).unwrap(); assert_eq!(json.get("schema").text(), "fcb.desk-stdio/1"); json
    }).collect();
    (exit, records, stderr)
}
struct NeverRead;
impl Read for NeverRead { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { panic!("unexpected stdin read") } }

#[test]
fn help_and_argument_errors_never_read_stdin() {
    let mut out = Vec::new(); let mut err = Vec::new();
    assert_eq!(run(&args(&["desk", "--help"]), &mut NeverRead, &mut out, &mut err, || false), EXIT_OK);
    assert!(String::from_utf8(out).unwrap().contains("EXPECTED_REV"));
    assert_eq!(run(&args(&["desk", "--stdio", "--other"]), &mut NeverRead, &mut Vec::new(), &mut err, || false), EXIT_ERROR);
}

#[test]
fn clean_eof_and_explicit_quit_do_not_start_source_work() {
    let (exit, empty, err) = invoke(b""); assert_eq!(exit, EXIT_OK); assert!(empty.is_empty()); assert!(err.is_empty());
    let (exit, records, err) = invoke(b"0\tstate\n0\tquit\n0\topen\tthis-must-not-be-opened\n");
    assert_eq!(exit, EXIT_OK); assert_eq!(records.len(), 2); assert!(err.is_empty());
    assert_eq!(records[0].get("result").get("initial_source_bytes_read").number(), 0);
    assert!(records[0].get("result").get("panes").array().is_empty());
}

#[test]
fn invalid_expected_revisions_and_numeric_domains_leave_the_session_alive() {
    let (exit, records, _) = invoke(b"1\topen\tnonexistent\n01\tstate\n18446744073709551616\tstate\n0\tquit\n");
    assert_eq!(exit, EXIT_ERROR); assert_eq!(records.len(), 4);
    assert_eq!(records[0].get("error").get("code").text(), "DESK_STALE_REVISION");
    assert_eq!(records[1].get("error").get("code").text(), "DESK_INVALID_NUMBER");
    assert_eq!(records[2].get("error").get("code").text(), "DESK_INVALID_NUMBER");
    assert_eq!(records[3].get("accepted_revision").number(), 0);
}

#[test]
fn unterminated_or_oversized_frames_never_execute_a_valid_prefix() {
    let (exit, records, _) = invoke(b"0\topen\tsome-file");
    assert_eq!(exit, EXIT_ERROR); assert_eq!(records.len(), 1);
    assert_eq!(records[0].get("error").get("code").text(), "DESK_FRAME_INCOMPLETE");
    assert_eq!(records[0].get("accepted_revision").number(), 0);
    let mut bytes = b"0\topen\t".to_vec(); bytes.extend(vec![b'a'; 65537]); bytes.extend_from_slice(b"\n0\tquit\n");
    let (exit, records, _) = invoke(&bytes);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(records.len(), 1);
    assert_eq!(records[0].get("error").get("code").text(), "DESK_FRAME_LIMIT");
}

#[test]
fn canceled_session_reports_cancellation_without_reading_input() {
    let mut out = Vec::new(); let mut err = Vec::new();
    assert_eq!(run(&args(&["desk", "--stdio"]), &mut NeverRead, &mut out, &mut err, || true), EXIT_CANCELED);
    let result = parse(&out).unwrap();
    assert_eq!(result.get("exit_code").number(), 130);
    assert_eq!(result.get("error").get("code").text(), "DESK_CANCELED"); assert!(err.is_empty());
}

#[test]
fn broken_output_stops_before_reading_the_next_request() {
    struct Frames { calls: usize }
    impl Read for Frames { fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.calls += 1; let frame = b"0\tstate\n"; bytes[..frame.len()].copy_from_slice(frame); Ok(frame.len())
    } }
    struct Broken;
    impl Write for Broken { fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::BrokenPipe.into()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) } }
    let mut input = Frames { calls: 0 }; let mut err = Vec::new();
    assert_eq!(run(&args(&["desk", "--stdio"]), &mut input, &mut Broken, &mut err, || false), EXIT_ERROR);
    assert_eq!(input.calls, 1); assert!(String::from_utf8(err).unwrap().contains("DESK_OUTPUT_INTERRUPTED"));
}

#[test]
fn malformed_commands_do_not_expand_tabs_quotes_or_shell_syntax() {
    let (exit, records, _) = invoke(b"0\tbogus\n0\topen\tpath\textra\n0\topen-hex\txyz\n0\tquit\r\n");
    assert_eq!(exit, EXIT_ERROR); assert_eq!(records.len(), 4);
    assert_eq!(records[0].get("error").get("code").text(), "DESK_COMMAND_SYNTAX");
    assert_eq!(records[1].get("error").get("code").text(), "DESK_COMMAND_SYNTAX");
    assert_eq!(records[2].get("error").get("code").text(), "DESK_INVALID_HEX");
    assert_eq!(records[3].get("result").get("initial_source_bytes_read").number(), 0);
}

#[cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
mod native {
    use super::*;
    use std::{fs, path::PathBuf, time::{SystemTime, UNIX_EPOCH}, sync::atomic::{AtomicU64, Ordering}};
    fn fixture() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("fcb-desk-cli-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&directory).unwrap(); directory
    }
    #[test]
    fn real_pinned_readers_find_copy_bookmark_close_and_recall_in_one_process() {
        let root = fixture(); let first = root.join("a.rs"); let second = root.join("b.rs");
        fs::write(&first, b"pinned source").unwrap(); fs::write(&second, b"prefix needle!").unwrap();
        let commands = format!("0\topen\t{}\n1\tpin\t1\n2\topen\t{}\n3\tfind\t2\t1\t50\t4194304\tneedle\n3\thit\t2\t1\t0\n5\tcopy\t2\n5\tbookmark\t2\timportant\n7\tclose\t2\n8\tclear-history\n9\trecall\t1\n10\tview\t3\n10\tquit\n", first.display(), second.display());
        let (exit, records, err) = invoke(commands.as_bytes());
        assert_eq!(exit, EXIT_OK); assert!(err.is_empty()); assert_eq!(records.len(), 12);
        assert_eq!(records[2].get("result").get("panes").array().len(), 2);
        assert_eq!(records[5].get("result").get("original_hex").text(), "6e6565646c65");
        assert_eq!(records[10].get("result").get("text").text(), "needle!");
        assert_eq!(records[11].get("result").get("panes").array().len(), 2);
        assert_eq!(records[11].get("result").get("bookmarks").array().len(), 1);
        assert_eq!(records[11].get("result").get("initial_source_bytes_read").number(), 27);
    }
    #[test]
    fn no_second_live_read_after_source_disappears_between_commands() {
        let root = fixture(); let file = root.join("source.rs"); fs::write(&file, b"original").unwrap();
        struct MovingOutput { bytes: Vec<u8>, file: PathBuf, moved: PathBuf, writes: usize }
        impl Write for MovingOutput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
            fn flush(&mut self) -> io::Result<()> {
                self.writes += 1; if self.writes == 1 { fs::rename(&self.file, &self.moved)?; } Ok(())
            }
        }
        let commands = format!("0\topen\t{}\n1\tview\t1\n1\tselect\t1\t0\t8\n3\tcopy\t1\n3\tquit\n", file.display());
        let mut input = commands.as_bytes();
        let mut out = MovingOutput { bytes: Vec::new(), file: file.clone(), moved: root.join("moved.rs"), writes: 0 };
        let mut err = Vec::new();
        assert_eq!(run(&args(&["desk", "--stdio"]), &mut input, &mut out, &mut err, || false), EXIT_OK);
        let records: Vec<_> = out.bytes.split(|b| *b == b'\n').filter(|s| !s.is_empty()).map(|s| parse(s).unwrap()).collect();
        assert_eq!(records[1].get("result").get("text").text(), "original");
        assert_eq!(records[3].get("result").get("original_hex").text(), "6f726967696e616c");
        assert!(!file.exists());
    }
    #[test]
    fn query_limits_and_stale_activation_are_machine_visible() {
        let root = fixture(); let path = root.join("many.rs"); fs::write(&path, b"a a a").unwrap();
        let commands = format!("0\topen\t{}\n1\tfind\t1\t1\t1\t100\ta\n1\thit\t1\t2\t0\n1\tquit\n", path.display());
        let (exit, records, _) = invoke(commands.as_bytes());
        assert_eq!(exit, EXIT_ERROR); assert_eq!(records[1].get("exit_code").number(), EXIT_PARTIAL as u64);
        assert!(!records[1].get("result").get("search_complete").flag());
        assert_eq!(records[2].get("error").get("code").text(), "DESK_STALE_QUERY");
    }
    #[test]
    fn hex_native_paths_and_multiline_needles_round_trip_without_shell_rules() {
        use std::os::unix::ffi::{OsStringExt, OsStrExt};
        let root = fixture(); let path = root.join(OsString::from_vec(b"tabs\tline\n\xff.rs".to_vec()));
        fs::write(&path, b"a\nb").unwrap();
        let path_hex = path.as_os_str().as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
        let commands = format!("0\topen-hex\t{path_hex}\n1\tfind-text-hex\t1\t1\t10\t100\t610a62\n1\thit\t1\t1\t0\n3\tcopy\t1\n3\tquit\n");
        let (exit, records, _) = invoke(commands.as_bytes());
        assert_eq!(exit, EXIT_OK); assert_eq!(records[3].get("result").get("original_hex").text(), "610a62");
    }
}

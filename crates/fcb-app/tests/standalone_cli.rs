#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::OsString, fs::{self, File, OpenOptions}, io::{Seek, SeekFrom, Write}, path::{Path, PathBuf},
    process::{Command, Output, Stdio}, sync::atomic::{AtomicU64, Ordering}};
use support::{Json, parse};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fcb cli source ü {} {}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap(); Self(root)
    }
    fn path(&self, name: impl AsRef<Path>) -> PathBuf { self.0.join(name) }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fcb"));
        command.current_dir(&self.0); command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { fs::remove_dir_all(&self.0).expect("remove only this test's uniquely owned fixture"); }
}
fn document(output: &Output, expected_exit: i32) -> Json {
    assert_eq!(output.status.code(), Some(expected_exit), "stderr={:?}, stdout={:?}",
        String::from_utf8_lossy(&output.stderr), String::from_utf8_lossy(&output.stdout));
    assert!(output.stderr.is_empty(), "{:?}", String::from_utf8_lossy(&output.stderr));
    let parsed = parse(&output.stdout).unwrap();
    assert_eq!(parsed.get("schema").text(), "fcb.cli/1"); parsed
}

#[test]
fn named_utf16_file_search_uses_source_byte_offsets_not_utf8_query_bytes() {
    let fixture = Fixture::new();
    let mut bytes = vec![0xfe, 0xff];
    for unit in "head\r\nbanana😀".encode_utf16() { bytes.extend_from_slice(&unit.to_be_bytes()); }
    fs::write(fixture.path("space ü.rs"), &bytes).unwrap();
    let output = fixture.command().args(["search", "space ü.rs", "--text", "ana", "--json"]).output().unwrap();
    let result = document(&output, 0);
    assert_eq!(result.get("mode").text(), "utf16be");
    assert_eq!(result.get("matches_seen").number(), 2);
    assert!(result.get("whole_file_complete").flag());
    let hits = result.get("hits").array();
    assert_eq!(hits[0].get("original_range").get("start").number(), 16);
    assert_eq!(hits[0].get("original_range").get("end").number(), 22);
    assert_eq!(hits[1].get("original_range").get("start").number(), 20);
    assert_eq!(fs::read(fixture.path("space ü.rs")).unwrap(), bytes);
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1, "no cache or other output created");
}

#[test]
fn sparse_file_inspection_and_far_range_search_do_not_load_the_whole_file() {
    let fixture = Fixture::new();
    let path = fixture.path("large source.rs");
    let base = (1u64 << 32) + 37;
    let mut file = File::create(&path).unwrap();
    file.set_len(base + 128).unwrap();
    file.seek(SeekFrom::Start(base)).unwrap(); file.write_all(b"needle\n").unwrap(); drop(file);
    let result = document(&fixture.command().args(["inspect", "large source.rs", "--json"]).output().unwrap(), 0);
    assert_eq!(result.get("observed_length").number(), base + 128);
    assert_eq!(result.get("payload_bytes_read").number(), 0);
    let output = fixture.command().args(["search", "large source.rs", "--text", "needle", "--encoding", "utf8",
        "--offset", &base.to_string(), "--bytes", "32", "--json"]).output().unwrap();
    let result = document(&output, 3);
    assert!(result.get("scope_complete").flag()); assert!(!result.get("whole_file_complete").flag());
    assert!(result.get("payload_bytes_read").number() <= 48);
    assert_eq!(result.get("hits").array()[0].get("original_range").get("start").number(), base);
    assert_eq!(result.get("hits").array()[0].get("original_range").get("end").number(), base + 6);
    assert!(output.stdout.len() < 8192);
}

#[test]
fn unsearched_tail_never_turns_a_window_no_match_into_a_whole_file_negative() {
    let fixture = Fixture::new();
    let mut file = File::create(fixture.path("tail.rs")).unwrap();
    file.set_len(1024 * 1024).unwrap(); file.seek(SeekFrom::End(-6)).unwrap(); file.write_all(b"needle").unwrap(); drop(file);
    let result = document(&fixture.command().args(["search", "tail.rs", "--text", "needle", "--bytes", "64", "--json"]).output().unwrap(), 3);
    assert_eq!(result.get("matches_seen").number(), 0);
    assert!(result.get("scope_complete").flag()); assert!(!result.get("whole_file_complete").flag());
    let result = document(&fixture.command().args(["search", "tail.rs", "--raw-hex", "6e6565646c65",
        "--offset", "1048570", "--bytes", "64", "--json"]).output().unwrap(), 3);
    assert_eq!(result.get("matches_seen").number(), 1);
    assert_eq!(result.get("hits").array()[0].get("original_range").get("start").number(), 1048570);
}

#[test]
fn native_non_utf8_and_control_filenames_round_trip_without_forged_output_rows() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let fixture = Fixture::new();
    let name = OsString::from_vec(b"--line\n\xff\".rs".to_vec());
    let path = fixture.path(&name);
    fs::write(&path, b"hello").unwrap();
    let result = document(&fixture.command().args(["read", "--json", "--"]).arg(&name).output().unwrap(), 0);
    let encoded = result.get("path");
    assert_eq!(encoded.get("encoding").text(), "unix-bytes");
    let bytes = encoded.get("hex").text().as_bytes().chunks_exact(2).map(|pair|
        u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()).collect::<Vec<_>>();
    assert_eq!(bytes, path.as_os_str().as_bytes());
    assert!(!encoded.get("display").text().contains('\n'));
    assert!(!encoded.get("display").text().contains('\u{fffd}'));
    assert_eq!(result.get("text").text(), "hello");
}

#[test]
fn directory_symlink_and_special_socket_inputs_refuse_before_payload_reading() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let fixture = Fixture::new();
    fs::write(fixture.path("real.rs"), b"real").unwrap();
    symlink("real.rs", fixture.path("alias.rs")).unwrap();
    fs::create_dir(fixture.path("directory")).unwrap();
    let socket = UnixListener::bind(fixture.path("socket")).unwrap();
    for (name, code) in [("alias.rs", "CLI_SYMLINK_REFUSED"), ("directory", "CLI_DIRECTORY_SCOPE_UNAVAILABLE"),
        ("socket", "CLI_SPECIAL_OBJECT_REFUSED")] {
        let result = document(&fixture.command().args(["read", name, "--json"]).output().unwrap(), 2);
        assert_eq!(result.get("error").get("code").text(), code);
        assert!(!result.get("complete").flag());
    }
    drop(socket);
}

#[test]
fn executable_runs_without_checkout_resources_or_a_companion_application() {
    let fixture = Fixture::new();
    let standalone = fixture.path("fcb standalone ü");
    fs::copy(env!("CARGO_BIN_EXE_fcb"), &standalone).unwrap();
    fs::write(fixture.path("data.rs"), b"exact bytes").unwrap();
    let run = |cmd: &mut Command| -> Output {
        for _ in 0..20 {
            match cmd.output() {
                Ok(out) => return out,
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy || e.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => panic!("failed to run standalone command: {e}"),
            }
        }
        cmd.output().expect("standalone command execution")
    };
    let result = document(&run(Command::new(&standalone).current_dir(&fixture.0).args(["open", "data.rs", "--json"])), 0);
    assert_eq!(result.get("text").text(), "exact bytes");
    let result = document(&run(Command::new(&standalone).current_dir(&fixture.0).arg("--json")), 0);
    assert!(!result.get("native_ready").flag());
    let human = run(Command::new(&standalone).current_dir(&fixture.0));
    assert_eq!(human.status.code(), Some(2)); assert!(human.stdout.is_empty());
    assert!(String::from_utf8(human.stderr).unwrap().contains("CLI_NATIVE_GUI_UNAVAILABLE"));
}

#[test]
fn actual_stdin_process_search_and_error_channels_have_complete_framing() {
    let fixture = Fixture::new();
    let mut child = fixture.command().args(["search", "--stdin", "--text", "ana", "--json"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"banana").unwrap();
    let result = document(&child.wait_with_output().unwrap(), 0);
    assert_eq!(result.get("matches_seen").number(), 2);
    let output = fixture.command().args(["read", "PRIVATE_DO_NOT_LOG", "--bytes", "01", "--json"]).output().unwrap();
    let result = document(&output, 2);
    assert_eq!(result.get("error").get("code").text(), "CLI_INVALID_NUMBER");
    assert!(!String::from_utf8(output.stdout).unwrap().contains("PRIVATE_DO_NOT_LOG"));
}

#[test]
fn read_only_named_file_operation_does_not_change_its_contents() {
    let fixture = Fixture::new();
    let path = fixture.path("unchanged.rs");
    let text = b"# source is data, not instructions\nrm -rf NEVER_EXECUTED\n";
    fs::write(&path, text).unwrap();
    let before = fs::metadata(&path).unwrap();
    let result = document(&fixture.command().args(["read", "unchanged.rs", "--json"]).output().unwrap(), 0);
    assert_eq!(result.get("text").text().as_bytes(), text);
    assert_eq!(fs::read(&path).unwrap(), text);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before.modified().unwrap());
    assert!(OpenOptions::new().read(true).open(path).is_ok());
}

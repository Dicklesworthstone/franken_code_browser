#![forbid(unsafe_code)]
#![cfg(unix)]

use std::{ffi::OsString, fs, io::Read, path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{host::{self, HostError, MAX_HOST_TEXT_BYTES}, AppError};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-host-{}-{now}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
struct ForbiddenInput;
impl Read for ForbiddenInput {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> { panic!("native services cannot read implicit stdin") }
}
fn cli(path: &Path, args: &[&str]) -> (u8, String) {
    let mut args: Vec<OsString> = args.iter().map(OsString::from).collect();
    args.extend(["--json".into(), "--".into(), path.as_os_str().to_owned()]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = fcb_app::run(&args, &mut ForbiddenInput, &mut out, &mut err, || false);
    assert!(err.is_empty()); (exit, String::from_utf8(out).unwrap())
}

#[test]
fn legacy_text_is_complete_original_utf8_including_bom_and_cross_extent_scalars() {
    let file = root().join("source.rs");
    let mut bytes = vec![b'a'; (1 << 20) - 1];
    bytes.extend_from_slice("😀\r\nbeta\rfinal\n".as_bytes());
    fs::write(&file, &bytes).unwrap();
    let result = host::read_text(&file, MAX_HOST_TEXT_BYTES, || false).unwrap();
    assert_eq!(result.as_str().as_bytes(), bytes);
    assert_eq!(result.source_bytes_read(), bytes.len());
    assert!(result.read_calls() > 16 && result.read_calls() <= host::MAX_HOST_READ_CALLS);
    fs::write(&file, "\u{feff}é\r\n").unwrap();
    let result = host::read_text(&file, 64, || false).unwrap();
    assert_eq!(result.as_str(), "\u{feff}é\r\n");
}
#[test]
fn full_legacy_allowance_is_available_but_one_extra_byte_is_never_truncated() {
    let file = root().join("bounded.txt");
    fs::write(&file, vec![b'x'; MAX_HOST_TEXT_BYTES]).unwrap();
    let result = host::read_text(&file, MAX_HOST_TEXT_BYTES, || false).unwrap();
    assert_eq!(result.as_str().len(), MAX_HOST_TEXT_BYTES);
    drop(result);
    fs::OpenOptions::new().write(true).open(&file).unwrap().set_len(MAX_HOST_TEXT_BYTES as u64 + 1).unwrap();
    assert!(matches!(host::read_text(&file, MAX_HOST_TEXT_BYTES, || false), Err(HostError::App(AppError::InputLimit))));
}
#[test]
fn invalid_utf8_nul_and_utf16_cannot_silently_change_into_different_source() {
    let file = root().join("text");
    for bytes in [b"left\xffright".as_slice(), &[0xff, 0xfe, b'x', 0]] {
        fs::write(&file, bytes).unwrap();
        assert!(matches!(host::read_text(&file, 64, || false), Err(HostError::InvalidUtf8)));
    }
    fs::write(&file, b"left\0right").unwrap();
    assert!(matches!(host::read_text(&file, 64, || false), Err(HostError::EmbeddedNul)));
}
#[test]
fn empty_source_returns_exact_empty_without_a_payload_read() {
    let file = root().join("empty"); fs::write(&file, b"").unwrap();
    let result = host::read_text(&file, 64, || false).unwrap();
    assert_eq!(result.as_str(), ""); assert_eq!(result.read_calls(), 0);
}
#[test]
fn named_source_policy_refuses_symlinks_directories_and_pre_io_cancellation() {
    let root = root(); let file = root.join("real"); fs::write(&file, b"content").unwrap();
    let link = root.join("link"); std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(matches!(host::read_text(&link, 64, || false), Err(HostError::App(AppError::Symlink))));
    assert!(matches!(host::read_text(&root, 64, || false), Err(HostError::App(AppError::Directory))));
    assert!(matches!(host::read_text(&root.join("absent"), 64, || true), Err(HostError::App(AppError::Canceled))));
    for limit in [0, MAX_HOST_TEXT_BYTES + 1] {
        assert!(matches!(host::read_text(&file, limit, || false), Err(HostError::App(AppError::InputLimit))));
    }
}
#[test]
fn growth_after_admission_cannot_escape_the_original_length_allowance() {
    let file = root().join("changing"); fs::write(&file, b"small").unwrap();
    let mut calls = 0;
    let result = host::read_text(&file, 64, || {
        calls += 1;
        if calls == 2 { fs::OpenOptions::new().write(true).open(&file).unwrap().set_len(1 << 30).unwrap(); }
        false
    });
    assert!(matches!(result, Err(HostError::App(AppError::SourceChanged))));
}
#[test]
fn byte_window_is_the_public_cli_result_and_does_not_need_full_file_admission() {
    let file = root().join("large"); fs::write(&file, b"head\nneedle\n").unwrap();
    fs::OpenOptions::new().write(true).open(&file).unwrap().set_len(8 << 20).unwrap();
    let expected = cli(&file, &["read", "--offset", "5", "--bytes", "8"]);
    let actual = host::read_window(&file, 5, 8, || false).unwrap();
    assert_eq!((actual.exit_code(), actual.as_str()), (expected.0, expected.1.as_str()));
    assert!(actual.as_str().contains("needle"));
}
#[test]
fn native_line_window_matches_cli_utf16_and_preserves_original_bytes() {
    let file = root().join("wide"); let mut bytes = vec![0xff, 0xfe];
    for unit in "alpha\r\nbeta😀\nlast".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(&file, bytes).unwrap();
    let expected = cli(&file, &["read-lines", "--line", "2", "--lines", "1"]);
    let actual = host::read_lines(&file, 2, 1, || false).unwrap();
    assert_eq!((actual.exit_code(), actual.as_str()), (expected.0, expected.1.as_str()));
    assert!(actual.as_str().contains("beta😀"));
}
#[test]
fn versioned_atlas_and_search_share_the_actual_workspace_policy_and_dispatcher() {
    let root = root(); fs::create_dir(root.join("src")).unwrap(); fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("src/main.rs"), b"needle").unwrap(); fs::write(root.join("target/ignored.rs"), b"needle").unwrap();
    let expected = cli(&root, &["atlas"]);
    let atlas = host::atlas_plan(&root, || false).unwrap();
    assert_eq!((atlas.exit_code(), atlas.as_str()), (expected.0, expected.1.as_str()));
    assert!(atlas.as_str().contains("\"payload_bytes_read\":\"0\""));
    let expected = cli(&root, &["search", "--workspace", "--text", "needle"]);
    let search = host::search_workspace(&root, "needle", || false).unwrap();
    assert_eq!((search.exit_code(), search.as_str()), (expected.0, expected.1.as_str()));
    assert!(!search.as_str().contains("ignored.rs"));
}
#[test]
fn structured_errors_remain_machine_responses_not_empty_successes_or_diagnostics() {
    let file = root().join("file"); fs::write(&file, b"abc").unwrap();
    let error = host::read_window(&file, 0, 0, || false).unwrap();
    assert_eq!(error.exit_code(), fcb_app::EXIT_ERROR);
    assert!(error.as_str().contains("\"status\":\"error\""));
    let canceled = host::read_window(&file, 0, 64, || true).unwrap();
    assert_eq!(canceled.exit_code(), fcb_app::EXIT_CANCELED);
    assert!(canceled.as_str().contains("CLI_CANCELED"));
}
#[test]
#[cfg(target_os = "linux")]
fn structured_rust_host_paths_keep_non_utf8_identity() {
    use std::os::unix::ffi::OsStringExt;
    let file = root().join(OsString::from_vec(b"source-\xff".to_vec()));
    fs::write(&file, b"native bytes").unwrap();
    let actual = host::read_window(&file, 0, 64, || false).unwrap();
    assert!(actual.as_str().contains("native bytes"));
    assert!(!actual.as_str().contains('\u{fffd}'));
}

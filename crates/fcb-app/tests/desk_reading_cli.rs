#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use support::{parse, Json};
use std::{ffi::OsString, fs, io::{self, Write}, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};
fn fixture(bytes: &[u8]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-reading-cli-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); let source = path.join("source.rs"); fs::write(&source, bytes).unwrap(); source
}
fn args() -> Vec<OsString> { vec!["desk".into(), "--stdio".into()] }
fn invoke(script: &str) -> (u8, Vec<Json>) {
    let mut input = script.as_bytes(); let mut out = Vec::new(); let mut err = Vec::new();
    let exit = run(&args(), &mut input, &mut out, &mut err, || false); assert!(err.is_empty());
    let rows = out.split(|b| *b == b'\n').filter(|s| !s.is_empty()).map(|s| parse(s).unwrap()).collect(); (exit, rows)
}
#[test]
fn long_jump_preserves_old_window_then_enters_history_and_reuses_index() {
    let mut bytes = b"x\n".repeat(1000); bytes.extend_from_slice(b"target\n"); let source = fixture(&bytes);
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\treader-prepare\t1\t1\n1\treader-line\t1\t1\t1\t1\n1\treader-line\t1\t1\t2\t1001\n1\treader-window\t1\t1\t1\t64\t1\n1\treader-step\t1\t1\t2\t0\t65536\n1\treader-go\t1\t1\t2\n7\tbookmark\t1\ttarget\n8\tback\n9\treader-line\t1\t1\t3\t1001\n9\tquit\n", source.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(rows.len(), 11);
    assert_eq!(rows[3].get("exit_code").number(), EXIT_PARTIAL as u64);
    assert_eq!(rows[4].get("result").get("text").text(), "x\n");
    assert_eq!(rows[6].get("accepted_revision").number(), 7);
    assert_eq!(rows[6].get("result").get("panes").array()[0].get("offset").number(), 2000);
    assert_eq!(rows[8].get("result").get("panes").array()[0].get("offset").number(), 0);
    assert_eq!(rows[9].get("result").get("request").get("scanned_bytes").number(), 0);
}
#[test]
fn pending_quit_is_partial_but_finished_or_canceled_progress_is_not() {
    let source = fixture(&b"x\n".repeat(100));
    let prefix = format!("0\topen\t{}\n1\treader-prepare\t1\t1\n1\treader-line\t1\t1\t1\t99\n", source.display());
    assert_eq!(invoke(&(prefix.clone() + "1\tquit\n")).0, EXIT_PARTIAL);
    assert_eq!(invoke(&prefix).0, EXIT_PARTIAL);
    assert_eq!(invoke(&(prefix.clone() + "1\treader-cancel\t1\t1\t1\n1\tquit\n")).0, EXIT_OK);
    assert_eq!(invoke(&(prefix + "1\treader-step\t1\t1\t1\t0\t65536\n1\tquit\n")).0, EXIT_OK);
}
#[test]
fn optional_partial_indexing_does_not_prevent_successful_session_exit() {
    let source = fixture(&b"x\n".repeat(1000));
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\treader-prepare\t1\t1\n1\treader-index\t1\t1\t0\t64\n1\treader-info\t1\t1\n1\tquit\n", source.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[3].get("result").get("indexed_through").number(), 64);
    assert_eq!(rows[3].get("readers").array()[0].get("index_steps").number(), 1);
    assert!(!rows[3].get("readers").array()[0].get("pending").flag());
}
#[test]
fn stale_steps_and_cancels_cannot_advance_or_destroy_current_work() {
    let source = fixture(&b"x\n".repeat(100));
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\treader-prepare\t1\t1\n1\treader-line\t1\t1\t2\t99\n1\treader-step\t1\t1\t2\t0\t4\n1\treader-step\t1\t1\t2\t0\t4\n1\treader-cancel\t1\t1\t1\n1\treader-cancel\t1\t1\t2\n1\tquit\n", source.display()));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[4].get("error").get("code").text(), "DESK_READER_STALE_STEP");
    assert_eq!(rows[4].get("readers").array()[0].get("seek_steps").number(), 1);
    assert_eq!(rows[5].get("error").get("code").text(), "DESK_READER_STALE_GENERATION");
    assert!(rows[5].get("readers").array()[0].get("pending").flag());
    assert!(!rows[6].get("readers").array()[0].get("pending").flag());
}
#[test]
fn continuation_offsets_are_qualified_and_do_not_rescan_the_source() {
    let source = fixture(b"a\r\nb\rc\n");
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\treader-prepare\t1\t1\n1\treader-line\t1\t1\t1\t1\n1\treader-window\t1\t1\t1\t32\t1\n1\treader-next\t1\t1\t1\t3\t32\t1\n1\treader-next\t1\t1\t1\t3\t32\t1\n1\treader-info\t1\t1\n1\tquit\n", source.display()));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[3].get("result").get("text").text(), "a\r\n");
    assert_eq!(rows[4].get("result").get("text").text(), "b\r");
    assert_eq!(rows[5].get("error").get("code").text(), "DESK_READER_NO_CONTINUATION");
    assert_eq!(rows[6].get("result").get("indexed_through").number(), 0);
}
#[test]
fn utf16_range_navigation_and_checkpoint_restore_keep_exact_selection() {
    let bytes: Vec<u8> = "\u{feff}a\r\nneedle".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let source = fixture(&bytes); let saved = source.with_extension("fcbk");
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\treader-prepare\t1\t1\n1\treader-range\t1\t1\t1\t8\t20\n1\treader-step\t1\t1\t1\t0\t64\n1\treader-go\t1\t1\t1\n5\tsave\t{}\t1024\n5\trestore\t{}\n7\tcopy\t2\n7\tquit\n", source.display(), saved.display(), saved.display()));
    assert_eq!(exit, EXIT_OK); assert!(rows[6].get("readers").array().is_empty());
    assert_eq!(rows[7].get("result").get("original_hex").text(), "6e006500650064006c006500");
}
#[test]
fn saved_archive_reading_survives_detachment_without_live_file_access() {
    let f = saved_fixture::Fixture::new(&[saved_fixture::entry(b"source.rs", b"first\nsecond\n")], true);
    let (exit, rows) = invoke(&format!("0\tsaved-open\t{}\n0\tsaved-member\t1\t0\n2\treader-prepare\t1\t1\n2\tsaved-close\t1\n2\treader-line\t1\t1\t1\t2\n2\treader-step\t1\t1\t1\t0\t64\n2\treader-window\t1\t1\t1\t64\t2\n2\tquit\n", f.path.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[6].get("result").get("text").text(), "second\n");
    assert!(!rows[6].get("result").get("source_reopened").flag());
    assert_eq!(rows[7].get("result").get("initial_source_bytes_read").number(), 0);
}
#[test]
fn failed_reader_replacement_preserves_old_index_and_consumes_generation() {
    let source = fixture(b"a\nb\n");
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\treader-prepare\t1\t1\n1\treader-index\t1\t1\t0\t64\n1\treader-prepare\t1\t2\t1\t64\n1\treader-prepare\t1\t2\n1\treader-line\t1\t1\t1\t2\n1\treader-step\t1\t1\t1\t0\t64\n1\treader-window\t1\t1\t1\t64\t1\n1\tquit\n", source.display()));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[3].get("readers").array()[0].get("generation").number(), 1);
    assert_eq!(rows[4].get("error").get("code").text(), "DESK_READER_STALE_GENERATION");
    assert_eq!(rows[7].get("result").get("text").text(), "b\n");
}
#[test]
fn reader_drop_does_not_drop_sources_and_one_panes_cancel_does_not_touch_another() {
    let source = fixture(&b"x\n".repeat(100));
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tduplicate\t1\n2\treader-prepare\t1\t1\n2\treader-prepare\t2\t2\n2\treader-line\t1\t1\t1\t99\n2\treader-line\t2\t2\t1\t1\n2\treader-clear\t1\t1\n2\treader-window\t2\t2\t1\t64\t1\n2\tstate\n2\tquit\n", source.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(rows[6].get("readers").array().len(), 1);
    assert_eq!(rows[7].get("result").get("text").text(), "x\n");
    assert_eq!(rows[8].get("result").get("panes").array().len(), 2);
    assert_eq!(rows[8].get("result").get("retained_sources").number(), 1);
}
#[test]
fn broken_progress_delivery_stops_before_later_effectful_commands() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.windows(b"reader-begin".len()).any(|s| s == b"reader-begin") { Err(io::ErrorKind::BrokenPipe.into()) } else { Ok(bytes.len()) }
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let source = fixture(b"a\nb\n"); let never = source.with_extension("never.fcbk");
    let script = format!("0\topen\t{}\n1\treader-prepare\t1\t1\n1\treader-line\t1\t1\t1\t2\n1\tsave\t{}\t1024\n", source.display(), never.display());
    let mut input = script.as_bytes(); let mut err = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut Broken, &mut err, || false), EXIT_ERROR);
    assert!(!never.exists()); assert!(String::from_utf8(err).unwrap().contains("DESK_OUTPUT_INTERRUPTED"));
}

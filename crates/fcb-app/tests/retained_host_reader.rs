#![forbid(unsafe_code)]
#![cfg(unix)]

use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::reader::{ReaderSession, ReaderSessionError, MAX_READER_WINDOW_BYTES};

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn source(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(owner(8801), Path::new("source.rs"), bytes, || false).unwrap()
}
fn file(bytes: &[u8]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-retained-reader-{}-{time}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    let path = root.join("source.rs"); fs::write(&path, bytes).unwrap(); path
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

#[test]
fn far_line_navigation_reuses_bounded_capture_checkpoints() {
    for utf16 in [false, true] {
        let text = "abcdef\r\n".repeat(200_000);
        let bytes = if utf16 {
            std::iter::once(0xfeff_u16).chain(text.encode_utf16())
                .flat_map(u16::to_le_bytes).collect::<Vec<_>>()
        } else { text.into_bytes() };
        let mut reader = source(&bytes);
        let mut cold_checks = 0;
        let cold = reader.read_lines(190_000, 2, 100, || { cold_checks += 1; false }).unwrap();
        let mut warm_checks = 0;
        let warm = reader.read_lines(190_000, 2, 100, || { warm_checks += 1; false }).unwrap();
        assert_eq!(cold.as_str(), warm.as_str(), "same captured source and coordinates");
        assert!(cold_checks > warm_checks + 15, "cold={cold_checks}, warm={warm_checks}");
        let backward = reader.read_lines(2, 1, 100, || false).unwrap();
        assert!(backward.as_str().contains("abcdef\\r\\n"));
        assert_eq!(reader.read_lines(190_001, 1, 100, || true).err(), Some(ReaderSessionError::Canceled));
        let next = reader.read_lines(190_001, 1, 100, || false).unwrap();
        assert!(next.as_str().contains("abcdef\\r\\n"));
    }
}

#[test]
fn live_replacement_cannot_change_search_context_or_exact_copy() {
    let path = file(b"before needle after\r\n");
    let mut reader = ReaderSession::open(owner(8802), &path, 4096, || false).unwrap();
    fs::write(&path, b"replacement source no old match").unwrap();
    let found = reader.search(1, "needle", 10, 4096, || false).unwrap();
    assert_eq!(found.exit_code(), EXIT_OK);
    assert!(found.as_str().contains("\"retained_hits\":\"1\""));
    assert_eq!(reader.hit_range(1, 0).unwrap().start().get(), 7);
    assert_eq!(reader.capture().bytes(), b"before needle after\r\n");
    let view = reader.hit_window(1, 0, 256, || false).unwrap();
    assert!(view.as_str().contains("before needle after\\r\\n"));
    assert!(view.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    let copied = reader.copy_hit(1, 0, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"6e6565646c65\""));
}

#[test]
fn independent_sessions_keep_their_own_captures_and_queries() {
    let mut a = ReaderSession::from_bytes(owner(8803), Path::new("same"), b"aaa x", || false).unwrap();
    let mut b = ReaderSession::from_bytes(owner(8804), Path::new("same"), b"x bbb", || false).unwrap();
    a.search(1, "x", 10, 100, || false).unwrap(); b.search(1, "x", 10, 100, || false).unwrap();
    assert_eq!(a.hit_range(1, 0).unwrap().start().get(), 4);
    assert_eq!(b.hit_range(1, 0).unwrap().start().get(), 0);
    assert_ne!(a.capture().request().file().owner(), b.capture().request().file().owner());
    drop(a);
    assert!(b.copy_hit(1, 0, || false).unwrap().as_str().contains("\"original_hex\":\"78\""));
}

#[test]
fn utf16_search_line_navigation_and_selection_preserve_original_coordinates() {
    let text = "\u{feff}left\r\n\u{1f980} needle\r\nlast";
    for little in [true, false] {
        let encode = |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(|u| if little { u.to_le_bytes() } else { u.to_be_bytes() }).collect() };
        let bytes = encode(text); let needle = encode("needle");
        let expected = bytes.windows(needle.len()).position(|p| p == needle).unwrap();
        let mut reader = source(&bytes);
        reader.search(1, "needle", 10, 4096, || false).unwrap();
        let selected = reader.hit_range(1, 0).unwrap();
        assert_eq!(selected.start().get(), expected as u64); assert_eq!(selected.len().get(), 12);
        let copy = reader.copy_hit(1, 0, || false).unwrap();
        assert!(copy.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&needle))));
        let window = reader.hit_window(1, 0, 0, || false).unwrap();
        assert!(window.as_str().contains("needle")); assert!(window.as_str().contains("window_utf8_range"));
        let lines = reader.read_lines(2, 1, 1024, || false).unwrap();
        assert!(lines.as_str().contains("🦀 needle\\r\\n"));
        assert!(lines.as_str().contains("\"first_physical_line\":\"2\""));
    }
}

#[test]
fn malformed_text_is_not_a_complete_negative_and_raw_copy_is_lossless() {
    let mut reader = source(b"ok\0\xffneedle");
    let result = reader.search(1, "needle", 10, 100, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL);
    assert!(result.as_str().contains("\"state\":\"unsupported-text\""));
    let raw = reader.copy_range(0, 10, || false).unwrap();
    assert!(raw.as_str().contains("\"original_hex\":\"6f6b00ff6e6565646c65\""));
    let view = reader.read_window(0, 100, || false).unwrap();
    assert!(view.as_str().contains("\"has_replacements\":true"));
    assert!(view.as_str().contains("\\u0000"));
}

#[test]
fn byte_copy_can_select_part_of_a_scalar_without_claiming_decoded_text() {
    let mut reader = source("é🦀".as_bytes());
    let out = reader.copy_range(1, 4, || false).unwrap();
    assert!(out.as_str().contains("\"copy_domain\":\"original-bytes\""));
    assert!(out.as_str().contains("\"original_hex\":\"a9f09f\""));
}

#[test]
fn canceled_and_failed_replacement_queries_preserve_the_accepted_generation() {
    let mut reader = source(b"aaa bbb");
    reader.search(1, "aaa", 10, 100, || false).unwrap();
    assert_eq!(reader.search(2, "bbb", 10, 100, || true).err(), Some(ReaderSessionError::Canceled));
    assert_eq!(reader.accepted_generation(), Some(1));
    assert!(reader.copy_hit(1, 0, || false).is_ok());
    assert_eq!(reader.search(2, "bbb", 10, 100, || false).err(), Some(ReaderSessionError::StaleQuery));
    reader.search(3, "bbb", 10, 100, || false).unwrap();
    assert_eq!(reader.copy_hit(1, 0, || false).err(), Some(ReaderSessionError::StaleQuery));
    assert_eq!(reader.hit_range(3, 0).unwrap().start().get(), 4);
}

#[test]
fn cancellation_inside_scan_does_not_replace_old_hits() {
    let mut bytes = b"old ".to_vec(); bytes.extend(std::iter::repeat_n(b'a', 200_000));
    let mut reader = source(&bytes);
    reader.search(1, "old", 1, bytes.len() as u64, || false).unwrap();
    let mut calls = 0;
    assert!(reader.search(2, "aa", 4096, bytes.len() as u64, || { calls += 1; calls > 15 }).is_err());
    assert_eq!(reader.accepted_generation(), Some(1));
    assert!(reader.copy_hit(1, 0, || false).is_ok());
}

#[test]
fn overlapping_hits_exact_limit_and_positive_lookahead_are_separate() {
    let mut reader = source(b"aaaa");
    let limited = reader.search(1, "aa", 2, 100, || false).unwrap();
    assert_eq!(limited.exit_code(), EXIT_PARTIAL);
    assert!(limited.as_str().contains("\"matches_seen\":\"3\""));
    assert_eq!(reader.hit_range(1, 1).unwrap().start().get(), 1);
    assert_eq!(reader.copy_hit(1, 2, || false).err(), Some(ReaderSessionError::MissingHit));
    let exact = reader.search(2, "aa", 3, 100, || false).unwrap();
    assert_eq!(exact.exit_code(), EXIT_OK);
    assert_eq!(reader.hit_range(2, 2).unwrap().start().get(), 2);
}

#[test]
fn byte_exhaustion_and_zero_retention_never_claim_exhaustive_counts() {
    let mut reader = source(b"a needle");
    let partial = reader.search(1, "needle", 10, 1, || false).unwrap();
    assert_eq!(partial.exit_code(), EXIT_PARTIAL);
    assert!(partial.as_str().contains("\"state\":\"byte-limit\""));
    let positive = reader.search(2, "needle", 0, 100, || false).unwrap();
    assert_eq!(positive.exit_code(), EXIT_PARTIAL);
    assert!(positive.as_str().contains("\"retained_hits\":\"0\""));
    assert_eq!(reader.search(3, "missing", 0, 100, || false).unwrap().exit_code(), EXIT_OK);
}

#[test]
fn giant_physical_line_gets_an_explicit_bounded_window_not_full_line_allocation() {
    let bytes = vec![b'a'; 2 * 1024 * 1024]; let mut reader = source(&bytes);
    let response = reader.read_lines(1, 1, 4096, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    assert!(response.as_str().contains("\"range_limited\":true"));
    assert!(response.as_str().contains("\"next_offset\":\"4096\""));
    assert!(response.as_str().len() < 20_000);
    assert!(reader.copy_range(0, MAX_READER_WINDOW_BYTES as u64 + 1, || false).is_err());
    assert!(reader.read_window(0, MAX_READER_WINDOW_BYTES + 1, || false).is_err());
}

#[test]
fn physical_lines_have_no_phantom_eof_row_and_empty_capture_remains_readable() {
    let mut reader = source(b"one\r\ntwo\rthree\n");
    let line = reader.read_lines(2, 1, 100, || false).unwrap();
    assert!(line.as_str().contains("\"text\":\"two\\r\""));
    assert_eq!(reader.read_lines(4, 1, 100, || false).err(), Some(ReaderSessionError::MissingLine));
    let mut empty = source(b"");
    assert!(empty.read_window(0, 4, || false).unwrap().as_str().contains("\"text\":\"\""));
    assert_eq!(empty.read_lines(1, 1, 4, || false).err(), Some(ReaderSessionError::MissingLine));
    assert_eq!(empty.search(1, "x", 0, 0, || false).unwrap().exit_code(), EXIT_OK);
}

#[test]
fn bom_and_crlf_matches_retain_their_exact_selected_bytes() {
    for needle in ["\r", "\n", "\r\n", "🦀"] {
        let mut reader = source("\u{feff}a🦀\r\nlast".as_bytes());
        reader.search(1, needle, 10, 1000, || false).unwrap();
        let hit = reader.hit_range(1, 0).unwrap();
        assert_eq!(&reader.capture().bytes()[hit.start().get() as usize..hit.end().get() as usize], needle.as_bytes());
        let view = reader.hit_window(1, 0, 0, || false).unwrap();
        assert!(view.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(needle.as_bytes()))));
    }
}

#[test]
fn retained_file_open_refuses_oversize_symlink_and_pre_cancellation() {
    let path = file(b"12345");
    assert!(ReaderSession::open(owner(8805), &path, 4, || false).is_err());
    assert!(ReaderSession::open(owner(8805), &path, 5, || false).is_ok());
    assert!(ReaderSession::open(owner(8805), &path, 5, || true).is_err());
    let link = path.with_file_name("link"); std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(ReaderSession::open(owner(8805), &link, 5, || false).is_err());
}

#[test]
fn byte_paths_and_control_bytes_survive_structured_responses() {
    use std::os::unix::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_vec(b"raw-\xff\n.rs".to_vec()));
    let mut reader = ReaderSession::from_bytes(owner(8806), &path, b"a\0b", || false).unwrap();
    let info = reader.info(|| false).unwrap();
    assert!(info.as_str().contains("7261772dff0a2e7273"));
    assert!(reader.read_window(0, 100, || false).unwrap().as_str().contains("a\\u0000b"));
}

#[test]
fn retained_response_buffers_coexist_without_allocation_id_aliasing() {
    let mut reader = source(b"needle");
    let first = reader.info(|| false).unwrap();
    let second = reader.read_window(0, 100, || false).unwrap();
    let third = reader.search(1, "needle", 10, 100, || false).unwrap();
    let fourth = reader.copy_hit(1, 0, || false).unwrap();
    assert_eq!([first.exit_code(), second.exit_code(), third.exit_code(), fourth.exit_code()], [0; 4]);
}

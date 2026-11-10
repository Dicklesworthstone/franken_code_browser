#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real captured-reader selections. Expected coordinates and bytes are built
//! directly from independent fixture prefixes, never the production decoder.
//! Executable qualification belongs to the independent strict-RCH verifier.
mod support;

use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, host::{HostResponse, reader::{ReaderSession,
    ReaderSessionError, MAX_READER_SELECTION_WINDOW_BYTES, MAX_READER_WINDOW_BYTES}}};
use support::{Json, parse};

fn source(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(9942).unwrap(), Path::new("selection.txt"), bytes, || false).unwrap()
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn encoded(text: &str, encoding: u8) -> Vec<u8> {
    match encoding {
        0 => text.as_bytes().to_vec(),
        1 => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        2 => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => unreachable!(),
    }
}
fn file(bytes: &[u8]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-reader-selection-{}-{now}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::write(&path, bytes).unwrap(); path
}
fn assert_range(value: &Json, start: u64, end: u64) {
    assert_eq!(value.get("start").number(), start);
    assert_eq!(value.get("end").number(), end);
}
fn assert_selection(
    response: &HostResponse, bytes: &[u8], window: (u64, u64), decoded: (u64, u64),
    original: (u64, u64), text: &str, replacements: bool,
) -> Json {
    assert_eq!(response.exit_code(), EXIT_OK);
    let value = json(response);
    assert_eq!(value.get("schema").text(), "fcb.reader-session/1");
    assert_eq!(value.get("status").text(), "ok");
    assert_eq!(value.get("command").text(), "resolve-selection");
    assert_eq!(value.get("selection_namespace").text(), "native-text");
    assert_eq!(value.get("additional_source_bytes_read").number(), 0);
    assert!(!value.get("native_presented").flag());
    assert_eq!(value.get("captured_bytes").number(), bytes.len() as u64);
    assert_range(value.get("window_original_range"), window.0, window.1);
    assert_range(value.get("window_utf8_range"), decoded.0, decoded.1);
    assert_range(value.get("original_range"), original.0, original.1);
    assert_eq!(value.get("original_hex").text(), hex(&bytes[original.0 as usize..original.1 as usize]));
    assert_eq!(value.get("text").text().as_bytes(), text.as_bytes());
    assert_eq!(value.get("contains_replacements").flag(), replacements);
    value
}

#[test]
fn selected_unicode_preserves_original_encoding_crlf_bidi_nul_and_normalization() {
    let prefix = "start ";
    let selected = "e\u{301} 🦀\r\nאב\t\0尾";
    let suffix = " \u{feff} end";
    let text = format!("{prefix}{selected}{suffix}");
    for encoding in 0..3 {
        let mut bytes = match encoding { 0 => vec![0xef, 0xbb, 0xbf],
            1 => vec![0xff, 0xfe], _ => vec![0xfe, 0xff] };
        let bom = bytes.len() as u64;
        bytes.extend_from_slice(&encoded(&text, encoding));
        let mut reader = source(&bytes);
        let page = json(&reader.read_window(0, 4096, || false).unwrap());
        assert_range(page.get("visible_range"), bom, bytes.len() as u64);
        assert_eq!(page.get("text").text().as_bytes(), text.as_bytes());
        let decoded = (prefix.len() as u64, (prefix.len() + selected.len()) as u64);
        let original_start = bom + encoded(prefix, encoding).len() as u64;
        let original = (original_start, original_start + encoded(selected, encoding).len() as u64);
        let receipt = reader.resolve_selection(bom, bytes.len() as u64, decoded.0, decoded.1, || false).unwrap();
        let value = assert_selection(&receipt, &bytes, (bom, bytes.len() as u64),
            decoded, original, selected, false);
        assert_eq!(value.get("encoding").text(), ["utf8", "utf16le", "utf16be"][encoding as usize]);
        assert_eq!(value.get("owner"), page.get("owner"));
        assert_eq!(value.get("file_id"), page.get("file_id"));
        assert_eq!(value.get("source_revision"), page.get("source_revision"));
        assert_eq!(value.get("path"), page.get("path"));
    }
}

#[test]
fn selected_replacements_remain_losslessly_bound_to_malformed_original_bytes() {
    let bytes = b"A\xffB";
    let mut reader = source(bytes);
    let page = json(&reader.read_window(0, 32, || false).unwrap());
    assert_eq!(page.get("text").text(), "A\u{fffd}B");
    let replaced = reader.resolve_selection(0, 3, 1, 4, || false).unwrap();
    assert_selection(&replaced, bytes, (0, 3), (1, 4), (1, 2), "\u{fffd}", true);
    let ordinary = reader.resolve_selection(0, 3, 4, 5, || false).unwrap();
    assert_selection(&ordinary, bytes, (0, 3), (4, 5), (2, 3), "B", false);
    assert!(reader.resolve_selection(0, 3, 2, 4, || false).is_err(),
        "a decoded replacement's UTF-8 interior is not an exact scalar boundary");
}

#[test]
fn real_replacement_scalar_is_not_reported_as_malformed() {
    let bytes = "A\u{fffd}B".as_bytes();
    let mut reader = source(bytes);
    let receipt = reader.resolve_selection(0, bytes.len() as u64, 1, 4, || false).unwrap();
    assert_selection(&receipt, bytes, (0, 5), (1, 4), (1, 4), "\u{fffd}", false);
}

#[test]
fn malformed_utf16_surrogates_resolve_to_their_actual_original_units() {
    for little in [true, false] {
        let units = [0xfeffu16, 0x0041, 0xd800, 0x0042];
        let bytes: Vec<u8> = units.into_iter().flat_map(|unit| if little {
            unit.to_le_bytes()
        } else { unit.to_be_bytes() }).collect();
        let mut reader = source(&bytes);
        let page = json(&reader.read_window(0, 32, || false).unwrap());
        assert_eq!(page.get("text").text(), "A\u{fffd}B");
        let receipt = reader.resolve_selection(2, 8, 1, 4, || false).unwrap();
        assert_selection(&receipt, &bytes, (2, 8), (1, 4), (4, 6), "\u{fffd}", true);
    }
}

#[test]
fn window_local_offsets_do_not_resolve_an_equal_token_elsewhere() {
    let bytes = b"same xx same yy same";
    let mut reader = source(bytes);
    let page = json(&reader.read_window(8, 7, || false).unwrap());
    assert_eq!(page.get("text").text(), "same yy");
    let receipt = reader.resolve_selection(8, 15, 0, 4, || false).unwrap();
    assert_selection(&receipt, bytes, (8, 15), (0, 4), (8, 12), "same", false);
}

#[test]
fn in_content_feff_is_retained_at_a_nonzero_window_start() {
    let prefix = "padding ";
    let selected = "\u{feff}source";
    for encoding in 0..3 {
        let mut bytes = match encoding { 0 => Vec::new(), 1 => vec![0xff, 0xfe], _ => vec![0xfe, 0xff] };
        bytes.extend_from_slice(&encoded(prefix, encoding));
        let start = bytes.len() as u64;
        bytes.extend_from_slice(&encoded(selected, encoding));
        let end = bytes.len() as u64;
        let mut reader = source(&bytes);
        let page = json(&reader.read_window(start, 64, || false).unwrap());
        assert_eq!(page.get("text").text(), selected);
        let receipt = reader.resolve_selection(start, end, 0, selected.len() as u64, || false).unwrap();
        assert_selection(&receipt, &bytes, (start, end), (0, selected.len() as u64),
            (start, end), selected, false);
    }
}

#[test]
fn empty_reversed_past_end_and_split_scalar_requests_are_refused() {
    let bytes = "a🦀b".as_bytes();
    let mut reader = source(bytes);
    for (first, last, start, end) in [
        (0, 6, 1, 1), (0, 6, 4, 1), (0, 6, 1, 7), (0, 6, 1, u64::MAX),
        (0, 6, 2, 5), (0, 6, 1, 3), (0, 0, 0, 1), (4, 3, 0, 1),
        (0, 7, 0, 1), (u64::MAX, u64::MAX, 0, 1), (2, 6, 0, 1), (0, 3, 0, 1),
    ] {
        assert!(reader.resolve_selection(first, last, start, end, || false).is_err(),
            "invalid page/selection {first}..{last}, {start}..{end} was accepted");
    }
    let receipt = reader.resolve_selection(0, 6, 1, 5, || false).unwrap();
    assert_selection(&receipt, bytes, (0, 6), (1, 5), (1, 5), "🦀", false);
}

#[test]
fn leading_bom_is_not_a_decoded_text_selection() {
    let bytes = b"\xef\xbb\xbftext";
    let mut reader = source(bytes);
    assert!(reader.resolve_selection(0, 7, 0, 4, || false).is_err(),
        "a fake visible page containing the decoder-stripped BOM was accepted");
    let receipt = reader.resolve_selection(3, 7, 0, 4, || false).unwrap();
    assert_selection(&receipt, bytes, (3, 7), (0, 4), (3, 7), "text", false);
}

#[test]
fn maximum_boundary_adjusted_page_can_resolve_a_bounded_selection() {
    let mut bytes = "🦀".as_bytes().to_vec();
    bytes.resize(MAX_READER_SELECTION_WINDOW_BYTES + 8, b'x');
    let mut reader = source(&bytes);
    let page = json(&reader.read_window(3, MAX_READER_WINDOW_BYTES, || false).unwrap());
    let end = MAX_READER_WINDOW_BYTES as u64 + 3;
    assert_range(page.get("visible_range"), 0, end);
    assert!(page.get("boundaries_adjusted").flag());
    let receipt = reader.resolve_selection(0, end, 0, 4, || false).unwrap();
    assert_selection(&receipt, &bytes, (0, end), (0, 4), (0, 4), "🦀", false);
    assert_eq!(reader.resolve_selection(0, end, 0, end, || false).err(),
        Some(ReaderSessionError::InvalidLimits), "oversized selected bytes must not truncate");
    let last = reader.resolve_selection(0, end, end - 1, end, || false).unwrap();
    assert_selection(&last, &bytes, (0, end), (end - 1, end), (end - 1, end), "x", false);
}

#[test]
fn selection_admission_rejects_wider_unproved_windows_and_over_budget_pages() {
    let bytes = vec![b'x'; MAX_READER_SELECTION_WINDOW_BYTES + 1];
    let mut reader = source(&bytes);
    assert_eq!(reader.resolve_selection(0, bytes.len() as u64, 0, 1, || false).err(),
        Some(ReaderSessionError::InvalidLimits));
    assert!(reader.resolve_selection(0, MAX_READER_WINDOW_BYTES as u64 + 1, 0, 1, || false).is_err(),
        "a widened ASCII window has no decoder boundary adjustment to prove it");
    let full = reader.resolve_selection(0, MAX_READER_WINDOW_BYTES as u64,
        0, MAX_READER_WINDOW_BYTES as u64, || false).unwrap();
    let value = json(&full);
    assert_eq!(value.get("original_hex").text().len(), 2 * MAX_READER_WINDOW_BYTES);
    assert_eq!(value.get("text").text().len(), MAX_READER_WINDOW_BYTES);
}

#[test]
fn canceled_or_refused_resolution_does_not_change_the_accepted_query() {
    let bytes = b"before needle after";
    let mut reader = source(bytes);
    reader.search(7, "needle", 8, bytes.len() as u64, || false).unwrap();
    for cancel_at in 0..10 {
        let mut checks = 0;
        let result = reader.resolve_selection(0, bytes.len() as u64, 7, 13, || {
            checks += 1;
            checks > cancel_at
        });
        if cancel_at == 0 { assert!(result.is_err()); }
        assert_eq!(reader.accepted_generation(), Some(7));
        let copy = json(&reader.copy_hit(7, 0, || false).unwrap());
        assert_eq!(copy.get("original_hex").text(), hex(b"needle"));
    }
    let receipt = reader.resolve_selection(0, bytes.len() as u64, 7, 13, || false).unwrap();
    assert_selection(&receipt, bytes, (0, bytes.len() as u64), (7, 13), (7, 13), "needle", false);
}

#[test]
fn changed_working_tree_cannot_retarget_a_retained_selection() {
    let bytes = b"original captured range";
    let path = file(bytes);
    let mut reader = ReaderSession::open(ArenaOwnerId::new(9943).unwrap(), &path, 4096, || false).unwrap();
    let page = json(&reader.read_window(0, 64, || false).unwrap());
    fs::write(&path, b"different current source").unwrap();
    let receipt = reader.resolve_selection(0, bytes.len() as u64, 9, 17, || false).unwrap();
    let selected = assert_selection(&receipt, bytes, (0, bytes.len() as u64),
        (9, 17), (9, 17), "captured", false);
    assert_eq!(selected.get("owner"), page.get("owner"));
    assert_eq!(selected.get("source_revision"), page.get("source_revision"));
}

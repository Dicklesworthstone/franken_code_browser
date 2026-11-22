#![forbid(unsafe_code)]

//! Public retained-reader delimiter navigation. Expected original offsets are
//! computed from fixture prefixes, independently of the production decoder.
//! Execution belongs to the repository's independent strict RCH build lane.
mod support;

use std::path::Path;
use fcb::{ArenaOwnerId, analysis::delimiters::{DelimiterError, MAX_DELIMITER_SOURCE_BYTES}};
use fcb_app::{EXIT_OK, host::reader::{ReaderSession, ReaderSessionError}};
use support::{Json, parse};

fn source(path: &str, bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(9954).unwrap(), Path::new(path), bytes, || false).unwrap()
}
fn encode(text: &str, encoding: u8) -> Vec<u8> {
    match encoding {
        0 => text.as_bytes().to_vec(),
        1 => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        2 => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => unreachable!(),
    }
}

#[test]
fn public_receipt_carries_exact_utf8_utf16_ranges_and_retained_identity() {
    let text = "\u{feff}// 😀 é\r\nfn run() { let text = \"}\"; ([1]) }\n";
    for encoding in 0..3 {
        let bytes = encode(text, encoding);
        let mut reader = source("/nonexistent/retained.rs", &bytes);
        reader.search(7, "run", 8, 1024, || false).unwrap();
        let source_digest = reader.capture_sha256();
        let open = encode(&text[..text.find('{').unwrap()], encoding).len() as u64;
        let close = encode(&text[..text.rfind('}').unwrap()], encoding).len() as u64;
        let width = if encoding == 0 { 1 } else { 2 };
        for (selected, expected) in [(open, close), (close, open)] {
            let response = reader.matching_delimiter(selected, selected + width, || false).unwrap();
            assert_eq!(response.exit_code(), EXIT_OK);
            let json = parse(response.as_str().as_bytes()).unwrap();
            assert_eq!(json.get("schema").text(), "fcb.reader-session/1");
            assert_eq!(json.get("command").text(), "matching-delimiter");
            assert_eq!(json.get("delimiter_state").text(), "matched");
            assert_eq!(json.get("selected_range").get("start").number(), selected);
            assert_eq!(json.get("selected_range").get("end").number(), selected + width);
            assert_eq!(json.get("matching_range").get("start").number(), expected);
            assert_eq!(json.get("matching_range").get("end").number(), expected + width);
            assert_eq!(json.get("language").text(), "rust");
            assert_eq!(json.get("evidence_level").text(), "lexical-delimiter-candidate");
            assert_eq!(json.get("source_context").text(), "complete-capture");
            assert!(!json.get("semantic_resolution").flag());
            assert_eq!(json.get("additional_source_bytes_read").number(), 0);
            assert_eq!(json.get("owner").number(), 9954);
            assert_eq!(json.get("source_revision").number(), reader.capture().request().revision().get());
        }
        assert_eq!(reader.accepted_generation(), Some(7));
        assert_eq!(reader.capture_sha256(), source_digest);
        assert_eq!(reader.capture().bytes(), bytes);
        assert!(reader.copy_hit(7, 0, || false).is_ok());
    }
}

#[test]
fn no_destination_states_and_invalid_requests_preserve_existing_search() {
    for (text, selected, state) in [
        ("([)]", 0, "mismatched"), ("(", 0, "unmatched"),
        ("\"(\"", 1, "not-code"), ("// (\n()", 3, "not-code"), ("x()", 0, "not-delimiter"),
    ] {
        let mut reader = source("file.rs", text.as_bytes());
        let response = reader.matching_delimiter(selected, selected + 1, || false).unwrap();
        let json = parse(response.as_str().as_bytes()).unwrap();
        assert_eq!(json.get("delimiter_state").text(), state);
        assert_eq!(json.get("matching_range"), &Json::Null);
    }
    let mut reader = source("file.rs", b"fn run() {}\xff");
    assert_eq!(reader.matching_delimiter(9, 10, || false).err(),
        Some(ReaderSessionError::Delimiter(DelimiterError::UnsupportedEncoding)));
    let mut reader = source("file.txt", b"()");
    assert_eq!(reader.matching_delimiter(0, 1, || false).err(),
        Some(ReaderSessionError::Delimiter(DelimiterError::UnsupportedLanguage)));
    let mut reader = source("file.rs", b"fn run() {}");
    reader.search(3, "run", 8, 128, || false).unwrap();
    assert_eq!(reader.matching_delimiter(6, 8, || false).err(),
        Some(ReaderSessionError::Delimiter(DelimiterError::InvalidSelection)));
    assert_eq!(reader.matching_delimiter(9, 10, || true).err(), Some(ReaderSessionError::Canceled));
    assert_eq!(reader.accepted_generation(), Some(3));
    assert!(reader.copy_hit(3, 0, || false).is_ok());
}

#[test]
fn explicit_analysis_limit_does_not_shorten_or_replace_the_reader_source() {
    let mut bytes = vec![b' '; MAX_DELIMITER_SOURCE_BYTES + 1];
    bytes[..2].copy_from_slice(b"()");
    let mut reader = source("large.rs", &bytes);
    assert_eq!(reader.matching_delimiter(0, 1, || false).err(),
        Some(ReaderSessionError::Delimiter(DelimiterError::SourceLimit)));
    assert_eq!(reader.capture().bytes(), bytes);
    assert!(reader.copy_range(0, 2, || false).unwrap().as_str().contains("2829"));
}

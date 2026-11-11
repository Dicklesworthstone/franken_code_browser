#![forbid(unsafe_code)]

//! Retained-source Find through the production shared scanner. Fixtures derive
//! original byte ranges independently from their encoded source prefixes.
//! Execution belongs to the independent strict-RCH verification lane.
mod support;

use std::path::Path;
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::{HostResponse, reader::{ReaderFindOptions,
    ReaderSession, ReaderSessionError, MAX_READER_MATCHES, MAX_READER_NEEDLE_BYTES}}};
use support::{Json, parse};

const LOWER: ReaderFindOptions = ReaderFindOptions { case_sensitive: false, count_all_matches: false };
const COUNT: ReaderFindOptions = ReaderFindOptions { case_sensitive: true, count_all_matches: true };
const LOWER_COUNT: ReaderFindOptions = ReaderFindOptions { case_sensitive: false, count_all_matches: true };

fn source(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(9942).unwrap(), Path::new("find.txt"), bytes, || false).unwrap()
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
fn assert_hit(found: &Json, index: usize, start: u64, end: u64) {
    let hit = &found.get("hits").array()[index];
    assert_eq!(hit.get("hit_index").number(), index as u64);
    assert_eq!(hit.get("occurrence_id").number(), index as u64 + 1);
    assert_eq!(hit.get("original_range").get("start").number(), start);
    assert_eq!(hit.get("original_range").get("end").number(), end);
}

#[test]
fn default_options_preserve_the_legacy_exact_find_wire_and_limit_semantics() {
    for limit in [0, 1, 4] {
        let bytes = b"banana BANANA\r\n";
        let mut legacy = source(bytes);
        let mut additive = source(bytes);
        let old = legacy.search(1, "ana", limit, 128, || false).unwrap();
        let new = additive.search_with_options(1, "ana", limit, 128,
            ReaderFindOptions::default(), || false).unwrap();
        assert_eq!(old.as_str(), new.as_str());
        assert_eq!(old.exit_code(), new.exit_code());
        assert_eq!(legacy.accepted_generation(), additive.accepted_generation());
    }
}

#[test]
fn full_count_is_independent_of_retained_rows_and_counts_overlapping_occurrences() {
    let mut reader = source(b"bananana");
    let response = reader.search_with_options(1, "ana", 1, 8, COUNT, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let found = json(&response);
    assert!(found.get("search_complete").flag());
    assert!(found.get("match_count_complete").flag());
    assert!(found.get("rows_truncated").flag());
    assert_eq!(found.get("matches_seen").number(), 3);
    assert_eq!(found.get("retained_hits").number(), 1);
    assert_eq!(found.get("retained_limit").number(), 1);
    assert_eq!(found.get("scanned_bytes").number(), 8);
    assert_eq!(found.get("literal_original_hex").text(), "616e61");
    assert_hit(&found, 0, 1, 4);
    assert_eq!(json(&reader.copy_hit(1, 0, || false).unwrap()).get("original_hex").text(), "616e61");
    let zero = json(&reader.search_with_options(2, "ana", 0, 8, COUNT, || false).unwrap());
    assert!(zero.get("search_complete").flag());
    assert!(zero.get("rows_truncated").flag());
    assert_eq!(zero.get("matches_seen").number(), 3);
    assert!(zero.get("hits").array().is_empty());
    assert_eq!(zero.get("literal_original_hex"), &Json::Null);
    assert_eq!(reader.hit_range(1, 0).err(), Some(ReaderSessionError::StaleQuery));
    assert_eq!(reader.hit_range(2, 0).err(), Some(ReaderSessionError::MissingHit));
}

#[test]
fn lowercase_uses_whole_original_scalars_in_utf8_and_both_utf16_orders() {
    let text = "\u{feff}K K k\r\nİ I i ß é e\u{301}";
    for encoding in 0..3 {
        let bytes = encoded(text, encoding);
        let mut reader = source(&bytes);
        for (generation, needle, originals) in [
            (1, "k", vec!["K", "K", "k"]),
            (2, "i", vec!["İ", "I", "i"]),
            // A match of one lowercase expansion unit still selects the whole İ.
            (3, "\u{307}", vec!["İ"]),
        ] {
            let found = json(&reader.search_with_options(generation, needle, 8,
                bytes.len() as u64, LOWER_COUNT, || false).unwrap());
            assert!(found.get("search_complete").flag());
            assert_eq!(found.get("mode").text(), "unicode-lowercase-literal");
            assert_eq!(found.get("literal_original_hex"), &Json::Null);
            assert_eq!(found.get("matches_seen").number(), originals.len() as u64);
            for (index, original) in originals.iter().enumerate() {
                let prefix = text.find(original).unwrap();
                let start = encoded(&text[..prefix], encoding).len() as u64;
                let witness = encoded(original, encoding);
                assert_hit(&found, index, start, start + witness.len() as u64);
                let hit = &found.get("hits").array()[index];
                assert_eq!(hit.get("matched_text").text(), *original);
                assert_eq!(hit.get("original_hex").text(), hex(&witness));
                let copy = json(&reader.copy_hit(generation, index, || false).unwrap());
                assert_eq!(copy.get("original_hex").text(), hex(&witness));
                let page = json(&reader.hit_window(generation, index, 64, || false).unwrap());
                let selection = page.get("selection");
                let from = selection.get("window_utf8_range").get("start").number() as usize;
                let to = selection.get("window_utf8_range").get("end").number() as usize;
                assert_eq!(&page.get("text").text()[from..to], *original);
                assert_eq!(selection.get("original_hex").text(), hex(&witness));
                assert_eq!(page.get("additional_source_bytes_read").number(), 0);
            }
        }
        let fold = json(&reader.search_with_options(4, "ss", 8, bytes.len() as u64, LOWER_COUNT, || false).unwrap());
        assert_eq!(fold.get("matches_seen").number(), 0, "lowercase must not silently become full folding");
        let canonical = json(&reader.search_with_options(5, "é", 8, bytes.len() as u64, LOWER_COUNT, || false).unwrap());
        assert_eq!(canonical.get("matches_seen").number(), 1, "canonical spellings remain distinct");
        assert_eq!(reader.capture().bytes(), bytes);
    }
}

#[test]
fn lowercase_crosses_decoder_windows_without_normalizing_crlf_or_source_spelling() {
    for encoding in 0..3 {
        let bom_bytes = encoded("\u{feff}", encoding).len();
        let unit_bytes = encoded("x", encoding).len();
        let prefix = format!("\u{feff}{}", "x".repeat((16_384 - bom_bytes - 1) / unit_bytes));
        let text = format!("{prefix}K\r\nK\r\nk\n");
        let bytes = encoded(&text, encoding);
        let mut reader = source(&bytes);
        let found = json(&reader.search_with_options(1, "k\r\n", 8, bytes.len() as u64,
            LOWER_COUNT, || false).unwrap());
        assert!(found.get("search_complete").flag());
        assert_eq!(found.get("matches_seen").number(), 2);
        let start = encoded(&prefix, encoding).len() as u64;
        let width = encoded("K\r\n", encoding).len() as u64;
        assert_hit(&found, 0, start, start + width);
        assert_eq!(found.get("hits").array()[0].get("matched_text").text(), "K\r\n");
        assert_eq!(json(&reader.copy_hit(1, 0, || false).unwrap()).get("original_hex").text(),
            hex(&encoded("K\r\n", encoding)));
        assert_eq!(reader.capture().bytes(), bytes);
    }
}

#[test]
fn bounded_rows_full_counts_scan_budget_and_unsupported_text_remain_distinct() {
    let mut reader = source(b"aAaAa");
    let bounded_response = reader.search_with_options(1, "A", 1, 5, LOWER, || false).unwrap();
    assert_eq!(bounded_response.exit_code(), EXIT_PARTIAL);
    let bounded = json(&bounded_response);
    assert_eq!(bounded.get("state").text(), "match-limit");
    assert_eq!(bounded.get("matches_seen").number(), 2);
    assert_eq!(bounded.get("retained_hits").number(), 1);
    assert!(!bounded.get("match_count_complete").flag());
    assert!(bounded.get("rows_truncated").flag());
    let counted = json(&reader.search_with_options(2, "A", 1, 5, LOWER_COUNT, || false).unwrap());
    assert!(counted.get("search_complete").flag());
    assert_eq!(counted.get("matches_seen").number(), 5);
    let limited = json(&reader.search_with_options(3, "A", 1, 3, LOWER_COUNT, || false).unwrap());
    assert_eq!(limited.get("state").text(), "byte-limit");
    assert_eq!(limited.get("matches_seen").number(), 3);
    assert_eq!(limited.get("scanned_bytes").number(), 3);
    assert!(!limited.get("match_count_complete").flag());
    assert!(limited.get("rows_truncated").flag());
    let absent = json(&reader.search_with_options(4, "z", 0, 5, LOWER_COUNT, || false).unwrap());
    assert!(absent.get("search_complete").flag());
    assert!(!absent.get("rows_truncated").flag());
    assert_eq!(absent.get("matches_seen").number(), 0);

    let mut scalar = source("ééé".as_bytes());
    let cut = json(&scalar.search_with_options(1, "É", 8, 1, LOWER_COUNT, || false).unwrap());
    assert_eq!(cut.get("state").text(), "byte-limit");
    assert_eq!(cut.get("matches_seen").number(), 0);
    assert_eq!(cut.get("unsupported_at"), &Json::Null);
    let mut malformed = vec![b'a'; 16_385];
    malformed[16_384] = 0xff;
    let mut bad = source(&malformed);
    let unavailable = json(&bad.search_with_options(1, "A", 1, malformed.len() as u64,
        LOWER_COUNT, || false).unwrap());
    assert_eq!(unavailable.get("state").text(), "unsupported-text");
    assert!(!unavailable.get("search_complete").flag());
    assert!(!unavailable.get("match_count_complete").flag());
    assert_eq!(unavailable.get("matches_seen").number(), 0);
    assert!(unavailable.get("hits").array().is_empty());
}

#[test]
fn canceled_replacement_consumes_identity_but_preserves_previously_accepted_hits() {
    let bytes = vec![b'A'; 256 * 1024];
    let mut reader = source(&bytes);
    reader.search(1, "A", 1, 8, || false).unwrap();
    let witness = reader.hit_range(1, 0).unwrap();
    let mut checks = 0;
    let canceled = reader.search_with_options(2, "a", 1, bytes.len() as u64, LOWER_COUNT, || {
        checks += 1;
        checks > 5
    });
    assert_eq!(canceled.err(), Some(ReaderSessionError::Canceled));
    assert_eq!(reader.accepted_generation(), Some(1));
    assert_eq!(reader.hit_range(1, 0).unwrap(), witness);
    assert_eq!(reader.search_with_options(2, "a", 1, 8, LOWER_COUNT, || false).err(),
        Some(ReaderSessionError::StaleQuery));
    reader.search_with_options(3, "a", 1, bytes.len() as u64, LOWER_COUNT, || false).unwrap();
    assert_eq!(reader.hit_range(1, 0).err(), Some(ReaderSessionError::StaleQuery));
    assert_eq!(reader.hit_range(3, 0).unwrap(), witness);
    assert_eq!(reader.capture().bytes(), bytes);
}

#[test]
fn long_query_witnesses_have_an_explicit_bounded_row_envelope_without_capping_counts() {
    let needle = "k".repeat(MAX_READER_NEEDLE_BYTES);
    let original = "K".repeat(MAX_READER_NEEDLE_BYTES + 100);
    let mut reader = source(original.as_bytes());
    let response = reader.search_with_options(1, &needle, MAX_READER_MATCHES,
        original.len() as u64, LOWER_COUNT, || false).unwrap();
    assert!(response.as_str().len() < 4 * 1024 * 1024);
    let found = json(&response);
    assert!(found.get("search_complete").flag());
    assert!(found.get("rows_truncated").flag());
    assert_eq!(found.get("matches_seen").number(), 101);
    let retained = found.get("retained_hits").number();
    assert!(retained > 0 && retained < 101);
    assert_eq!(retained, found.get("retained_limit").number());
    assert_eq!(found.get("hits").array()[0].get("matched_text").text(), "K".repeat(MAX_READER_NEEDLE_BYTES));
    assert_hit(&found, 0, 0, (MAX_READER_NEEDLE_BYTES * 3) as u64);
    let copy = json(&reader.copy_hit(1, 0, || false).unwrap());
    assert_eq!(copy.get("original_hex").text(), hex(&original.as_bytes()[..MAX_READER_NEEDLE_BYTES * 3]));
    assert_eq!(reader.search_with_options(2, "a", 0, 8, LOWER, || false).err(),
        Some(ReaderSessionError::InvalidLimits));
    assert_eq!(reader.accepted_generation(), Some(1));
}

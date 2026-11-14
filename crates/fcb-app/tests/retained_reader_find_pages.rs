#![forbid(unsafe_code)]

//! Production retained Find paging. Original byte positions come from fixture
//! encodings, independently of the streaming search implementation.
mod support;
use std::path::Path;
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::{HostResponse,
    reader::{ReaderFindOptions, ReaderSession, ReaderSessionError, MAX_READER_MATCHES}}};
use support::{Json, parse};

fn source(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(9943).unwrap(),
        Path::new("pages.txt"), bytes, || false).unwrap()
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
const COUNT: ReaderFindOptions = ReaderFindOptions { case_sensitive: true, count_all_matches: true };
const LOWER_COUNT: ReaderFindOptions = ReaderFindOptions { case_sensitive: false, count_all_matches: true };

#[test]
fn every_occurrence_beyond_the_old_hit_cap_is_navigable_without_growing_the_table() {
    let count = MAX_READER_MATCHES + 11;
    let bytes = vec![b'a'; count];
    let mut reader = source(&bytes);
    let first_response = reader.search_page(1, "a", 0, MAX_READER_MATCHES,
        count as u64, ReaderFindOptions::default(), || false).unwrap();
    assert_eq!(first_response.exit_code(), EXIT_PARTIAL);
    let first = json(&first_response);
    assert_eq!(first.get("page_start_after").number(), 0);
    assert_eq!(first.get("skipped_matches").number(), 0);
    assert!(first.get("has_more_matches").flag());
    assert_eq!(first.get("retained_hits").number(), MAX_READER_MATCHES as u64);
    let second_response = reader.search_page(2, "a", MAX_READER_MATCHES as u64,
        MAX_READER_MATCHES, count as u64, ReaderFindOptions::default(), || false).unwrap();
    assert_eq!(second_response.exit_code(), EXIT_OK);
    let second = json(&second_response);
    assert_eq!(second.get("matches_seen").number(), count as u64);
    assert_eq!(second.get("skipped_matches").number(), MAX_READER_MATCHES as u64);
    assert!(second.get("search_complete").flag());
    assert!(second.get("rows_truncated").flag(), "earlier rows remain deliberately omitted");
    assert!(!second.get("has_more_matches").flag(), "earlier omissions are not future rows");
    assert_eq!(second.get("retained_hits").number(), 11);
    for (index, hit) in second.get("hits").array().iter().enumerate() {
        let start = MAX_READER_MATCHES as u64 + index as u64;
        assert_eq!(hit.get("occurrence_id").number(), start + 1);
        assert_eq!(hit.get("original_range").get("start").number(), start);
        assert_eq!(json(&reader.copy_hit(2, index, || false).unwrap()).get("original_hex").text(), "61");
    }
    assert_eq!(reader.hit_range(1, 0).err(), Some(ReaderSessionError::StaleQuery));
    // Previous-page replay has the same original occurrence and source bytes.
    let previous = json(&reader.search_page(3, "a", 0, MAX_READER_MATCHES,
        count as u64, ReaderFindOptions::default(), || false).unwrap());
    assert_eq!(previous.get("hits"), first.get("hits"));
    assert_eq!(reader.capture().bytes(), bytes);
}

#[test]
fn overlapping_utf8_and_utf16_pages_share_exact_global_occurrences() {
    for encoding in 0..3 {
        let bytes = encoded("\u{feff}bananana", encoding);
        let mut reader = source(&bytes);
        let mut previous_generation = 0;
        for start_after in 0..3u64 {
            let generation = start_after + 1;
            let found = json(&reader.search_page(generation, "ana", start_after, 1,
                bytes.len() as u64, COUNT, || false).unwrap());
            assert!(found.get("match_count_complete").flag());
            assert_eq!(found.get("matches_seen").number(), 3);
            assert_eq!(found.get("has_more_matches").flag(), start_after < 2);
            let row = &found.get("hits").array()[0];
            assert_eq!(row.get("occurrence_id").number(), start_after + 1);
            let prefix = format!("\u{feff}b{}", "an".repeat(start_after as usize));
            let start = encoded(&prefix, encoding).len() as u64;
            assert_eq!(row.get("original_range").get("start").number(), start);
            let copy = json(&reader.copy_hit(generation, 0, || false).unwrap());
            assert_eq!(copy.get("original_hex").text(), hex(&encoded("ana", encoding)));
            if previous_generation > 0 {
                assert_eq!(reader.hit_range(previous_generation, 0).err(), Some(ReaderSessionError::StaleQuery));
            }
            previous_generation = generation;
        }
        let empty = json(&reader.search_page(4, "ana", 3, 1,
            bytes.len() as u64, COUNT, || false).unwrap());
        assert!(empty.get("search_complete").flag());
        assert!(!empty.get("has_more_matches").flag());
        assert!(empty.get("hits").array().is_empty());
    }
}

#[test]
fn lowercase_pages_keep_each_original_spelling_and_expansion_source_unit() {
    let text = "\u{feff}K K k İ i İ";
    for encoding in 0..3 {
        let bytes = encoded(text, encoding);
        let mut reader = source(&bytes);
        for (start_after, original) in ["K", "K", "k"].iter().enumerate() {
            let found = json(&reader.search_page(start_after as u64 + 1, "k",
                start_after as u64, 1, bytes.len() as u64, LOWER_COUNT, || false).unwrap());
            let row = &found.get("hits").array()[0];
            assert_eq!(row.get("occurrence_id").number(), start_after as u64 + 1);
            assert_eq!(row.get("matched_text").text(), *original);
            assert_eq!(row.get("original_hex").text(), hex(&encoded(original, encoding)));
            let page = json(&reader.hit_window(start_after as u64 + 1, 0, 32, || false).unwrap());
            assert_eq!(page.get("selection").get("original_hex").text(), hex(&encoded(original, encoding)));
            assert_eq!(page.get("additional_source_bytes_read").number(), 0);
        }
        let expansion = json(&reader.search_page(4, "\u{307}", 1, 1,
            bytes.len() as u64, LOWER_COUNT, || false).unwrap());
        let row = &expansion.get("hits").array()[0];
        assert_eq!(row.get("occurrence_id").number(), 2);
        assert_eq!(row.get("matched_text").text(), "İ");
        assert_eq!(row.get("original_hex").text(), hex(&encoded("İ", encoding)));
    }
}

#[test]
fn replay_budgets_count_the_prefix_and_cancellation_preserves_the_accepted_page() {
    let bytes = vec![b'a'; 128 * 1024];
    let mut reader = source(&bytes);
    reader.search_page(1, "a", 64, 2, bytes.len() as u64,
        ReaderFindOptions::default(), || false).unwrap();
    let original = reader.hit_range(1, 0).unwrap();
    let mut polls = 0;
    let canceled = reader.search_page(2, "a", 65_536, 2, bytes.len() as u64, COUNT, || {
        polls += 1; polls > 8
    });
    assert_eq!(canceled.err(), Some(ReaderSessionError::Canceled));
    assert_eq!(reader.hit_range(1, 0).unwrap(), original);
    assert_eq!(reader.search_page(2, "a", 0, 2, 8, COUNT, || false).err(),
        Some(ReaderSessionError::StaleQuery));
    let short = json(&reader.search_page(3, "a", 64, 2, 16, COUNT, || false).unwrap());
    assert_eq!(short.get("state").text(), "byte-limit");
    assert_eq!(short.get("scanned_bytes").number(), 16);
    assert_eq!(short.get("matches_seen").number(), 16);
    assert_eq!(short.get("skipped_matches").number(), 16);
    assert!(!short.get("has_more_matches").flag());
    assert!(!short.get("search_complete").flag(), "no retained rows is not complete no-match");
    assert!(short.get("hits").array().is_empty());
}

#[test]
fn invalid_page_limits_do_not_evict_accepted_hits_and_zero_row_counting_is_explicit() {
    let mut reader = source(b"aaaa");
    reader.search_page(1, "a", 1, 1, 4, COUNT, || false).unwrap();
    assert_eq!(reader.search_page(2, "a", 5, 1, 4, COUNT, || false).err(),
        Some(ReaderSessionError::InvalidLimits));
    assert_eq!(reader.search_page(2, "a", 0, 0, 4,
        ReaderFindOptions::default(), || false).err(), Some(ReaderSessionError::InvalidLimits));
    assert_eq!(reader.accepted_generation(), Some(1));
    let count = json(&reader.search_page(2, "a", 2, 0, 4, COUNT, || false).unwrap());
    assert!(count.get("search_complete").flag());
    assert_eq!(count.get("matches_seen").number(), 4);
    assert_eq!(count.get("skipped_matches").number(), 2);
    assert!(count.get("has_more_matches").flag());
    assert!(count.get("hits").array().is_empty());
}

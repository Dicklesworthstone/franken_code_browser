#![forbid(unsafe_code)]
#![cfg(unix)]

//! Native Find aliases and witness-verified fresh hit context on real opened
//! files. This extends the original search/search_hit suite without replacing
//! it. Expected offsets and bytes come from independent fixture construction.
//! Execution belongs to the independent strict-RCH verification lane.
mod support;

use std::{fs::{self, OpenOptions}, os::unix::fs::FileExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::{HostResponse, observed_reader::{ObservedReaderEncoding,
    ObservedReaderError, ObservedReaderSession, MAX_OBSERVED_READER_OBSERVATIONS,
    MAX_OBSERVED_READER_MATCHES, MAX_OBSERVED_READER_NEEDLE_BYTES,
    MAX_OBSERVED_READER_SCAN_BYTES}}};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-observed-find-{}-{now}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("source"), bytes).unwrap();
        Self(root)
    }
    fn file(&self) -> fs::File { OpenOptions::new().read(true).write(true).open(self.0.join("source")).unwrap() }
    fn reader(&self) -> ObservedReaderSession {
        static NEXT: AtomicU64 = AtomicU64::new(71_000);
        ObservedReaderSession::open(ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap(),
            &self.0, b"source", || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(value: &Json) -> (u64, u64) { (value.get("start").number(), value.get("end").number()) }
fn ids(value: &Json) -> Vec<u64> { value.get("retained_observations").array().iter().map(Json::number).collect() }
fn find(reader: &mut ObservedReaderSession, generation: u64, needle: &str, max_hits: usize, max_bytes: u64) -> HostResponse {
    reader.find(generation, needle, ObservedReaderEncoding::Utf8, max_hits, max_bytes, || false).unwrap()
}
fn selected<'a>(page: &'a Json) -> &'a str {
    let (first, last) = bounds(page.get("find_hit").get("decoded_utf8_range"));
    &page.get("text").text()[first as usize..last as usize]
}

#[test]
fn large_file_find_crosses_read_chunks_without_retaining_the_source() {
    let fixture = Fixture::new(b"");
    let file = fixture.file();
    let across_chunk = 16 * 1024 - 2;
    let beyond_capture_limit = 32 * 1024 * 1024 + 17;
    let length = beyond_capture_limit + 512;
    file.set_len(length).unwrap();
    assert_eq!(file.write_at(b"needle", across_chunk).unwrap(), 6);
    assert_eq!(file.write_at(b"needle", beyond_capture_limit).unwrap(), 6);
    let mut reader = fixture.reader();
    let initial = json(&reader.info(|| false).unwrap());
    assert_eq!(initial.get("payload_bytes_read").number(), 0);
    let first = json(&reader.read_window(0, 16, ObservedReaderEncoding::Utf8, || false).unwrap());
    let response = find(&mut reader, 7, "needle", 32, 64 * 1024 * 1024);
    assert_eq!(response.exit_code(), EXIT_OK);
    assert!(response.as_str().len() < 8192);
    let found = json(&response);
    assert_eq!(found.get("query_generation").number(), 7);
    assert_eq!(found.get("search_observation").number(), 2);
    assert_eq!(found.get("search_kind").text(), "continuous-open-file-observation");
    assert_eq!(found.get("state").text(), "complete-observed-input");
    assert!(found.get("search_complete").flag());
    assert!(found.get("input_complete").flag());
    assert!(!found.get("whole_file_immutable").flag());
    assert_eq!(found.get("source_bytes_read").number(), length);
    assert_eq!(found.get("scanned_bytes").number(), length);
    assert_eq!(found.get("source_bytes_read"), found.get("payload_bytes_read"));
    assert_eq!(found.get("scanned_bytes"), found.get("scanned_input_bytes"));
    assert_eq!(found.get("needle"), found.get("literal_text"));
    assert_eq!(found.get("matches_seen").number(), 2);
    assert_eq!(found.get("retained_hits").number(), 2);
    assert_eq!(found.get("literal_original_hex").text(), hex(b"needle"));
    for (index, offset) in [across_chunk, beyond_capture_limit].into_iter().enumerate() {
        let hit = &found.get("hits").array()[index];
        assert_eq!(hit.get("hit_index").number(), index as u64);
        assert_eq!(hit.get("hit_index"), hit.get("index"));
        assert_eq!(hit.get("occurrence_id").number(), index as u64 + 1);
        assert_eq!(bounds(hit.get("original_range")), (offset, offset + 6));
    }
    let after_find = json(&reader.info(|| false).unwrap());
    assert_eq!(ids(&after_find), [1], "a search observation is not a retained range observation");
    assert_eq!(after_find.get("retained_payload_bytes"), first.get("captured_bytes"));
    assert_eq!(after_find.get("retained_search_witness_bytes").number(), 6);
    assert_eq!(after_find.get("payload_bytes_read").number(), length + first.get("captured_bytes").number());
    let page = json(&reader.observe_hit(7, 1, 32, 128, || false).unwrap());
    assert_eq!(page.get("observation").number(), 3);
    assert_eq!(page.get("find_hit").get("search_observation").number(), 2);
    assert_eq!(bounds(page.get("find_hit").get("original_range")), (beyond_capture_limit, beyond_capture_limit + 6));
    assert_eq!(selected(&page), "needle");
    assert!(page.get("find_hit").get("witness_verified").flag());
    assert!(page.get("additional_source_bytes_read").number() <= 144);
}

#[test]
fn overlapping_matches_and_one_lookahead_have_explicit_count_semantics() {
    let fixture = Fixture::new(b"banana");
    let mut reader = fixture.reader();
    let complete = json(&find(&mut reader, 1, "ana", 2, 64));
    assert_eq!(complete.get("state").text(), "complete-observed-input");
    assert_eq!(complete.get("matches_seen").number(), 2);
    assert_eq!(bounds(complete.get("hits").array()[0].get("original_range")), (1, 4));
    assert_eq!(bounds(complete.get("hits").array()[1].get("original_range")), (3, 6));
    let limited = find(&mut reader, 2, "ana", 1, 64);
    assert_eq!(limited.exit_code(), EXIT_PARTIAL);
    let limited = json(&limited);
    assert_eq!(limited.get("state").text(), "match-limit");
    assert_eq!(limited.get("matches_seen").number(), 2);
    assert_eq!(limited.get("retained_hits").number(), 1);
    assert!(!limited.get("input_complete").flag());
    let absent = json(&find(&mut reader, 3, "missing", 1, 64));
    assert_eq!(absent.get("state").text(), "complete-observed-input");
    assert_eq!(absent.get("matches_seen").number(), 0);
    assert_eq!(absent.get("literal_original_hex"), &Json::Null);
}

#[test]
fn byte_allowance_is_partial_coverage_and_partial_scalars_are_not_malformed() {
    let fixture = Fixture::new(b"0123456789needle");
    let mut reader = fixture.reader();
    let response = find(&mut reader, 1, "needle", 4, 12);
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let report = json(&response);
    assert_eq!(report.get("state").text(), "byte-limit");
    assert_eq!(report.get("source_bytes_read").number(), 12);
    assert_eq!(report.get("scanned_bytes").number(), 12);
    assert_eq!(report.get("matches_seen").number(), 0);
    assert!(!report.get("search_complete").flag());
    assert_eq!(report.get("unsupported_at"), &Json::Null);

    let bom = Fixture::new(b"\xef\xbb\xbfneedle");
    let report = json(&find(&mut bom.reader(), 1, "needle", 4, 2));
    assert_eq!(report.get("state").text(), "byte-limit");
    assert_eq!(report.get("source_bytes_read").number(), 2);
    assert_eq!(report.get("scanned_bytes").number(), 0);
    assert_eq!(report.get("unsupported_at"), &Json::Null);
}

fn encoded(text: &str, encoding: ObservedReaderEncoding, bom: bool) -> Vec<u8> {
    match encoding {
        ObservedReaderEncoding::Utf8 => {
            let mut bytes = if bom { vec![0xef, 0xbb, 0xbf] } else { Vec::new() };
            bytes.extend_from_slice(text.as_bytes()); bytes
        }
        ObservedReaderEncoding::Utf16Le | ObservedReaderEncoding::Utf16Be => {
            let little = encoding == ObservedReaderEncoding::Utf16Le;
            let mut bytes = if bom { if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] } } else { Vec::new() };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            bytes
        }
    }
}

#[test]
fn utf8_utf16_chunk_surrogates_boms_and_page_local_selection_share_exact_coordinates() {
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let prefix = if encoding == ObservedReaderEncoding::Utf8 { 16 * 1024 - 5 } else { (16 * 1024 - 4) / 2 };
        let text = format!("{}🦀ana\r\n\u{feff}tail", "x".repeat(prefix));
        let bytes = encoded(&text, encoding, true);
        let literal = encoded("🦀ana", encoding, false);
        let start = 16 * 1024 - 2;
        assert_eq!(&bytes[start..start + literal.len()], literal.as_slice());
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        let found = json(&reader.find(1, "🦀ana", encoding, 8, 64 * 1024, || false).unwrap());
        assert_eq!(found.get("retained_hits").number(), 1);
        assert_eq!(bounds(found.get("hits").array()[0].get("original_range")), (start as u64, (start + literal.len()) as u64));
        assert_eq!(found.get("literal_original_hex").text(), hex(&literal));
        let page = json(&reader.observe_hit(1, 0, 3, 64, || false).unwrap());
        assert_eq!(page.get("encoding").text(), encoding.code());
        assert_eq!(selected(&page), "🦀ana");
        assert!(!page.get("has_replacements").flag());
        let copy = json(&reader.copy_range(page.get("observation").number(), start as u64,
            (start + literal.len()) as u64, || false).unwrap());
        assert_eq!(copy.get("original_hex").text(), hex(&literal));
        let interior_bom = json(&reader.find(2, "\u{feff}", encoding, 8, 64 * 1024, || false).unwrap());
        assert_eq!(interior_bom.get("retained_hits").number(), 1, "the initial BOM is not searchable content");
        let interior = json(&reader.observe_hit(2, 0, 3, 32, || false).unwrap());
        assert_eq!(selected(&interior), "\u{feff}");
    }
}

#[test]
fn malformed_text_is_distinct_from_a_completed_search_with_no_matches() {
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let mut bytes = encoded("needle ", encoding, true);
        let bad_offset = bytes.len() as u64;
        bytes.push(0xff);
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        let response = reader.find(1, "needle", encoding, 8, 64 * 1024, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let report = json(&response);
        assert_eq!(report.get("state").text(), "unsupported-text");
        assert_eq!(report.get("unsupported_at").number(), bad_offset);
        assert_eq!(report.get("retained_hits").number(), 0);
        assert_eq!(report.get("matches_seen").number(), 0);
        assert_eq!(report.get("literal_original_hex"), &Json::Null);
        assert!(!report.get("search_complete").flag());
        assert_eq!(reader.observe_hit(1, 0, 8, 64, || false).err().unwrap(), ObservedReaderError::MissingHit);
    }
}

#[test]
fn changed_witness_cannot_evict_history_and_unchanged_witness_gets_a_new_observation() {
    let fixture = Fixture::new(b"prefix needle suffix");
    let file = fixture.file();
    let mut reader = fixture.reader();
    for _ in 0..MAX_OBSERVED_READER_OBSERVATIONS {
        reader.read_window(0, 16, ObservedReaderEncoding::Utf8, || false).unwrap();
    }
    let found = json(&find(&mut reader, 10, "needle", 8, 64));
    assert_eq!(found.get("search_observation").number(), 9);
    let before = json(&reader.info(|| false).unwrap());
    assert_eq!(file.write_at(b"Noodle", 7).unwrap(), 6);
    assert_eq!(reader.observe_hit(10, 0, 8, 32, || false).err().unwrap(), ObservedReaderError::SearchHitChanged);
    let failed = json(&reader.info(|| false).unwrap());
    assert_eq!(ids(&before), ids(&failed));
    assert!(failed.get("payload_bytes_read").number() > before.get("payload_bytes_read").number());
    let old = json(&reader.copy_range(1, 7, 13, || false).unwrap());
    assert_eq!(old.get("original_hex").text(), hex(b"needle"));
    let retained_witness = json(&reader.search_hit(10, 0, || false).unwrap());
    assert_eq!(retained_witness.get("original_hex").text(), hex(b"needle"));
    assert_eq!(retained_witness.get("additional_source_bytes_read").number(), 0);
    assert_eq!(file.write_at(b"PREFIX needle", 0).unwrap(), 13);
    let new_page = json(&reader.observe_hit(10, 0, 8, 32, || false).unwrap());
    assert_eq!(new_page.get("observation").number(), 11, "the failed hit read spent revision 10");
    assert_eq!(new_page.get("text").text(), "PREFIX needle suffix");
    assert_eq!(selected(&new_page), "needle");
    assert_eq!(new_page.get("find_hit").get("search_observation").number(), 9);
    let _ = find(&mut reader, 11, "suffix", 8, 64);
    let retained = json(&reader.retained_window(11, || false).unwrap());
    assert_eq!(retained.get("find_hit"), new_page.get("find_hit"));
    assert_eq!(retained.get("text"), new_page.get("text"));
    assert_eq!(retained.get("additional_source_bytes_read").number(), 0);
}

#[test]
fn replacement_glyphs_do_not_substitute_for_a_literal_witness_or_poison_other_matches() {
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let bytes = encoded("xxx needle \u{fffd} end", encoding, true);
        let fixture = Fixture::new(&bytes);
        let file = fixture.file();
        let mut reader = fixture.reader();
        let _ = reader.find(1, "needle", encoding, 8, 64 * 1024, || false).unwrap();
        let first = if encoding == ObservedReaderEncoding::Utf8 { 3 } else { 2 };
        let malformed = match encoding {
            ObservedReaderEncoding::Utf8 => vec![0xff],
            ObservedReaderEncoding::Utf16Le => vec![0x00, 0xd8],
            ObservedReaderEncoding::Utf16Be => vec![0xd8, 0x00],
        };
        assert_eq!(file.write_at(&malformed, first).unwrap(), malformed.len());
        let page = json(&reader.observe_hit(1, 0, 32, 64, || false).unwrap());
        assert!(page.get("has_replacements").flag(), "a newly malformed neighbor is visibly disclosed");
        assert_eq!(selected(&page), "needle", "an exact unaffected match remains navigable");
        assert_eq!(file.write_at(&bytes, 0).unwrap(), bytes.len());
        let literal = json(&reader.find(2, "\u{fffd}", encoding, 8, 64 * 1024, || false).unwrap());
        let (start, _) = bounds(literal.get("hits").array()[0].get("original_range"));
        assert_eq!(file.write_at(&malformed, start).unwrap(), malformed.len());
        let before = json(&reader.info(|| false).unwrap());
        assert_eq!(reader.observe_hit(2, 0, 32, 64, || false).err().unwrap(), ObservedReaderError::SearchHitChanged);
        let after = json(&reader.info(|| false).unwrap());
        assert_eq!(ids(&after), ids(&before));
        let retained = json(&reader.retained_window(page.get("observation").number(), || false).unwrap());
        assert_eq!(retained.get("find_hit"), page.get("find_hit"));
        assert_eq!(retained.get("text"), page.get("text"));
    }
}

#[test]
fn invalid_and_canceled_queries_preserve_accepted_search_and_charge_spent_io() {
    let fixture = Fixture::new(b"needle");
    fixture.file().set_len(64 * 1024).unwrap();
    let mut reader = fixture.reader();
    reader.read_window(0, 16, ObservedReaderEncoding::Utf8, || false).unwrap();
    let _ = find(&mut reader, 1, "needle", 8, 128 * 1024);
    let before = json(&reader.info(|| false).unwrap());
    assert_eq!(reader.find(2, "", ObservedReaderEncoding::Utf8, 8, 64, || false).err().unwrap(), ObservedReaderError::InvalidLimits);
    assert_eq!(reader.find(2, "needle", ObservedReaderEncoding::Utf8, 8, 64, || false).err().unwrap(), ObservedReaderError::StaleQuery);
    let mut calls = 0;
    let canceled = reader.find(3, "absent", ObservedReaderEncoding::Utf8, 8, 128 * 1024,
        || { calls += 1; calls >= 8 });
    assert!(canceled.err().unwrap().is_canceled());
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(reader.find(3, "needle", ObservedReaderEncoding::Utf8, 8, 64, || false).err(), Some(ObservedReaderError::StaleQuery));
    assert_eq!(after.get("accepted_search_generation").number(), 1);
    assert_eq!(after.get("accepted_search_observation").number(), 2);
    assert_eq!(ids(&after), [1]);
    assert!(after.get("payload_bytes_read").number() > before.get("payload_bytes_read").number());
    assert!(after.get("read_calls").number() > before.get("read_calls").number());
    let selected = json(&reader.observe_hit(1, 0, 8, 32, || false).unwrap());
    assert_eq!(selected.get("observation").number(), 5);
    assert_eq!(selected.get("find_hit").get("query_generation").number(), 1);
}

#[test]
fn final_delivery_cancellation_preserves_previous_find_after_full_io() {
    let fixture = Fixture::new(b"prefix needle suffix");
    let mut reader = fixture.reader();
    let mut gates = 0;
    reader.find(1, "needle", ObservedReaderEncoding::Utf8, 8, 64, || { gates += 1; false }).unwrap();
    let before = json(&reader.info(|| false).unwrap());
    let mut calls = 0;
    let canceled = reader.find(2, "needle", ObservedReaderEncoding::Utf8, 8, 64,
        || { calls += 1; calls == gates });
    assert!(canceled.err().unwrap().is_canceled());
    assert_eq!(calls, gates);
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(after.get("accepted_search_generation").number(), 1);
    assert_eq!(reader.find(2, "needle", ObservedReaderEncoding::Utf8, 8, 64, || false).err(), Some(ObservedReaderError::StaleQuery));
    assert_eq!(after.get("payload_bytes_read").number(), before.get("payload_bytes_read").number() * 2);
    assert_eq!(selected(&json(&reader.observe_hit(1, 0, 8, 32, || false).unwrap())), "needle");
}

#[test]
fn pathname_replacement_does_not_redirect_find_or_hit_navigation() {
    let fixture = Fixture::new(b"old needle source");
    let mut reader = fixture.reader();
    fs::rename(fixture.0.join("source"), fixture.0.join("original")).unwrap();
    fs::write(fixture.0.join("source"), b"new replacement source").unwrap();
    let old = json(&find(&mut reader, 1, "needle", 8, 64));
    assert_eq!(old.get("retained_hits").number(), 1);
    let page = json(&reader.observe_hit(1, 0, 8, 32, || false).unwrap());
    assert_eq!(page.get("text").text(), "old needle source");
    let new = json(&find(&mut fixture.reader(), 1, "needle", 8, 64));
    assert_eq!(new.get("retained_hits").number(), 0);
}

#[test]
fn changed_metadata_is_partial_even_when_the_declared_sequence_was_fully_scanned() {
    let fixture = Fixture::new(b"needle");
    let file = fixture.file();
    file.set_len(64 * 1024).unwrap();
    let mut reader = fixture.reader();
    let mut calls = 0;
    let response = reader.find(1, "needle", ObservedReaderEncoding::Utf8, 8, 128 * 1024,
        || { calls += 1; if calls == 5 { file.set_len(64 * 1024 + 1).unwrap(); } false }).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let report = json(&response);
    assert_eq!(report.get("state").text(), "complete-observed-input");
    assert!(report.get("input_complete").flag());
    assert!(!report.get("search_complete").flag());
    assert_eq!(report.get("consistency").text(), "changed-during-read");
    assert_eq!(report.get("observed_length").number(), 64 * 1024);
    assert_eq!(report.get("final_observed_length").number(), 64 * 1024 + 1);
}

#[test]
fn limits_empty_input_and_stale_hit_requests_do_not_publish_pages() {
    let fixture = Fixture::new(b"");
    let mut reader = fixture.reader();
    let empty = json(&find(&mut reader, 1, "needle", 8, 64));
    assert_eq!(empty.get("state").text(), "complete-observed-input");
    assert_eq!(empty.get("source_bytes_read").number(), 0);
    assert_eq!(empty.get("read_calls").number(), 0);
    assert_eq!(reader.observe_hit(1, 0, 8, 32, || false).err().unwrap(), ObservedReaderError::MissingHit);
    assert_eq!(reader.observe_hit(2, 0, 8, 32, || false).err().unwrap(), ObservedReaderError::StaleQuery);
    for (generation, needle, hits, bytes) in [(2, "n", MAX_OBSERVED_READER_MATCHES + 1, 64),
        (3, "n", 8, MAX_OBSERVED_READER_SCAN_BYTES + 1)] {
        assert_eq!(reader.find(generation, needle, ObservedReaderEncoding::Utf8, hits, bytes, || false).err().unwrap(), ObservedReaderError::InvalidLimits);
    }
    let oversized = "n".repeat(MAX_OBSERVED_READER_NEEDLE_BYTES + 1);
    assert_eq!(reader.find(4, &oversized, ObservedReaderEncoding::Utf8, 8, 64, || false).err().unwrap(), ObservedReaderError::InvalidLimits);
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(after.get("accepted_search_generation").number(), 1);
    assert_eq!(after.get("payload_bytes_read").number(), 0);
    assert!(ids(&after).is_empty());
}

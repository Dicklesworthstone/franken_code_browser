#![forbid(unsafe_code)]
#![cfg(unix)]

//! Actual open-file searches, independent source-byte oracles and retained hit
//! activation. These are production-path tests, not whole-capture substitutes.
//! Execution belongs to the independent strict-RCH verification lane.
mod support;

use std::{fs::{self, OpenOptions}, os::unix::fs::FileExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::{HostResponse, observed_reader::{ObservedReaderEncoding,
    ObservedReaderError, ObservedReaderSearchKind, ObservedReaderSession, MAX_OBSERVED_READER_MATCHES,
    MAX_OBSERVED_READER_NEEDLE_BYTES, MAX_OBSERVED_READER_SCAN_BYTES, MAX_OBSERVED_READER_SEARCH_CALLS}}};
use support::{Json, parse};

const BUFFER_BYTES: usize = 16 * 1024;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-observed-search-{}-{now}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap(); Self(root)
    }
    fn write(&self, bytes: &[u8]) { fs::write(self.0.join("source"), bytes).unwrap(); }
    fn reader(&self) -> ObservedReaderSession {
        static NEXT: AtomicU64 = AtomicU64::new(65_000);
        ObservedReaderSession::open(ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap(),
            &self.0, b"source", || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn encoded(text: &str, encoding: ObservedReaderEncoding) -> Vec<u8> {
    match encoding {
        ObservedReaderEncoding::Utf8 => text.as_bytes().to_vec(),
        ObservedReaderEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ObservedReaderEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
    }
}
fn bounds(value: &Json) -> (u64, u64) { (value.get("start").number(), value.get("end").number()) }
fn find(reader: &mut ObservedReaderSession, generation: u64, needle: &str) -> Json {
    json(&reader.search(generation, needle, ObservedReaderEncoding::Utf8, 8,
        MAX_OBSERVED_READER_SCAN_BYTES, || false).unwrap())
}
fn assert_hit(value: &Json, index: usize, start: u64, end: u64) {
    let hit = &value.get("hits").array()[index];
    assert_eq!(hit.get("index").number(), index as u64);
    assert_eq!(hit.get("occurrence_id").number(), index as u64 + 1);
    assert_eq!(bounds(hit.get("original_range")), (start, end));
}

#[test]
fn sparse_source_above_the_complete_capture_limit_has_bounded_search_and_exact_activation() {
    let fixture = Fixture::new();
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(fixture.0.join("source")).unwrap();
    let offset = 33 * 1024 * 1024 - 3; // Crosses a 16 KiB input buffer above 32 MiB.
    let length = offset + 37;
    file.set_len(length).unwrap();
    assert_eq!(file.write_at(b"needle\n", offset).unwrap(), 7);
    let mut reader = fixture.reader();
    let initial = json(&reader.info(|| false).unwrap());
    assert_eq!(initial.get("payload_bytes_read").number(), 0);
    assert_eq!(initial.get("initial_observed_length").number(), length);
    let response = reader.search(1, "needle\n", ObservedReaderEncoding::Utf8, 8, length, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let found = json(&response);
    assert!(found.get("search_complete").flag());
    assert!(found.get("input_complete").flag());
    assert!(!found.get("whole_file_immutable").flag());
    assert_eq!(found.get("source_observation").text(), "open-file-ranges-not-atomic");
    assert_eq!(found.get("retention").text(), "literal-witnesses-only");
    assert_eq!(found.get("observed_length").number(), length);
    assert_eq!(found.get("final_observed_length").number(), length);
    assert_eq!(found.get("payload_bytes_read").number(), length);
    assert_eq!(found.get("scanned_input_bytes").number(), length);
    assert_eq!(found.get("peak_input_buffer_bytes").number(), BUFFER_BYTES as u64);
    assert_eq!(found.get("max_read_calls").number(), MAX_OBSERVED_READER_SEARCH_CALLS);
    assert!(found.get("read_calls").number() <= MAX_OBSERVED_READER_SEARCH_CALLS);
    assert_eq!(found.get("matches_seen").number(), 1);
    assert_hit(&found, 0, offset, offset + 7);
    let before = json(&reader.info(|| false).unwrap());
    assert_eq!(before.get("retained_payload_bytes").number(), 0, "search does not fill the page cache");
    assert!(before.get("retained_observations").array().is_empty());
    let witness = json(&reader.search_hit(1, 0, || false).unwrap());
    assert_eq!(witness.get("text").text(), "needle\n");
    assert_eq!(witness.get("original_hex").text(), "6e6565646c650a");
    assert_eq!(witness.get("witness_sha256"), found.get("literal_sha256"));
    assert_eq!(witness.get("additional_source_bytes_read").number(), 0);
    assert_eq!(witness.get("additional_read_calls").number(), 0);
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(before, after, "activation cannot even stat or read the live source");
    assert_eq!(reader.copy_range(found.get("search_observation").number(), offset, offset + 7, || false).err(),
        Some(ObservedReaderError::MissingObservation), "a witness is not a retained arbitrary page");
}

#[test]
fn utf8_utf16_scalars_and_overlapping_literals_cross_buffers_with_original_byte_ranges() {
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let prefix_count = if encoding == ObservedReaderEncoding::Utf8 { BUFFER_BYTES - 4 } else { (BUFFER_BYTES - 4) / 2 };
        let prefix = format!("\u{feff}{}", "x".repeat(prefix_count));
        let source = format!("{prefix}🦀a🦀a🦀\r\n");
        let bytes = encoded(&source, encoding);
        let literal = encoded("🦀a🦀", encoding);
        let start = encoded(&prefix, encoding).len() as u64;
        let stride = encoded("🦀a", encoding).len() as u64;
        let fixture = Fixture::new(); fixture.write(&bytes);
        let mut reader = fixture.reader();
        let result = reader.search(7, "🦀a🦀", encoding, 8, bytes.len() as u64, || false).unwrap();
        assert_eq!(result.exit_code(), EXIT_OK);
        let found = json(&result);
        assert_eq!(found.get("encoding").text(), encoding.code());
        assert_eq!(found.get("encoding_basis").text(), "explicit-for-this-search");
        assert_eq!(found.get("matches_seen").number(), 2);
        assert_eq!(found.get("literal_original_hex").text(), hex(&literal));
        for index in 0..2 {
            let from = start + index as u64 * stride;
            assert_hit(&found, index, from, from + literal.len() as u64);
            assert_eq!(&bytes[from as usize..from as usize + literal.len()], literal.as_slice());
            let witness = json(&reader.search_hit(7, index, || false).unwrap());
            assert_eq!(witness.get("original_hex").text(), hex(&literal));
            assert_eq!(witness.get("text").text(), "🦀a🦀");
        }
    }
}

#[test]
fn initial_bom_is_not_a_text_occurrence_but_later_feff_remains_exact() {
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let bytes = encoded("\u{feff}x\u{feff}y", encoding);
        let prefix = encoded("\u{feff}x", encoding).len() as u64;
        let literal = encoded("\u{feff}", encoding);
        let fixture = Fixture::new(); fixture.write(&bytes);
        let mut reader = fixture.reader();
        let found = json(&reader.search(1, "\u{feff}", encoding, 8, 64, || false).unwrap());
        assert!(found.get("search_complete").flag());
        assert_eq!(found.get("retained_hits").number(), 1);
        assert_hit(&found, 0, prefix, prefix + literal.len() as u64);
        assert_eq!(found.get("literal_original_hex").text(), hex(&literal));
    }
}

#[test]
fn zero_row_limit_probes_real_hits_and_keeps_count_truncation_and_scan_budget_distinct() {
    let fixture = Fixture::new(); fixture.write(b"banana");
    let mut reader = fixture.reader();
    let absent = reader.search(1, "zzz", ObservedReaderEncoding::Utf8, 0, 64, || false).unwrap();
    assert_eq!(absent.exit_code(), EXIT_OK);
    let absent = json(&absent);
    assert!(absent.get("search_complete").flag());
    assert!(!absent.get("truncated").flag());
    assert_eq!(absent.get("matches_seen").number(), 0);
    assert_eq!(absent.get("literal_original_hex"), &Json::Null);
    assert_eq!(absent.get("literal_sha256"), &Json::Null);
    let present = reader.search(2, "ana", ObservedReaderEncoding::Utf8, 0, 64, || false).unwrap();
    assert_eq!(present.exit_code(), EXIT_PARTIAL);
    let present = json(&present);
    assert_eq!(present.get("state").text(), "match-limit");
    assert!(present.get("truncated").flag());
    assert!(!present.get("search_complete").flag());
    assert_eq!(present.get("matches_seen").number(), 1);
    assert_eq!(present.get("retained_hits").number(), 0);
    assert_eq!(present.get("literal_original_hex"), &Json::Null);
    assert!(present.get("hits").array().is_empty());
    let limited = json(&reader.search(3, "ana", ObservedReaderEncoding::Utf8, 1, 64, || false).unwrap());
    assert_eq!(limited.get("matches_seen").number(), 2);
    assert_eq!(limited.get("retained_hits").number(), 1);
    assert!(limited.get("truncated").flag());
    assert_hit(&limited, 0, 1, 4);
    let exactly_full = json(&reader.search(4, "ban", ObservedReaderEncoding::Utf8, 1, 64, || false).unwrap());
    assert!(exactly_full.get("search_complete").flag());
    assert!(!exactly_full.get("truncated").flag(), "a full result table does not imply an omitted match");
    let unscanned = json(&reader.search(5, "ana", ObservedReaderEncoding::Utf8, 8, 0, || false).unwrap());
    assert_eq!(unscanned.get("state").text(), "byte-limit");
    assert!(!unscanned.get("truncated").flag());
    assert!(!unscanned.get("search_complete").flag());
    assert_eq!(unscanned.get("payload_bytes_read").number(), 0);
    assert_eq!(unscanned.get("read_calls").number(), 0);
    fixture.write(b"");
    let empty = json(&reader.search(6, "ana", ObservedReaderEncoding::Utf8, 0, 0, || false).unwrap());
    assert!(empty.get("search_complete").flag());
    assert_eq!(empty.get("payload_bytes_read").number(), 0);
    assert_eq!(empty.get("read_calls").number(), 0);
}

#[test]
fn byte_limit_is_not_eof_and_partial_scalars_do_not_become_replacement_matches() {
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let bytes = encoded("a🦀b", encoding);
        let start = encoded("a", encoding).len() as u64;
        let end = encoded("a🦀", encoding).len() as u64;
        let fixture = Fixture::new(); fixture.write(&bytes);
        let mut reader = fixture.reader();
        let cut = json(&reader.search(1, "🦀", encoding, 8, start + 1, || false).unwrap());
        assert_eq!(cut.get("state").text(), "byte-limit");
        assert_eq!(cut.get("payload_bytes_read").number(), start + 1);
        assert_eq!(cut.get("retained_hits").number(), 0);
        assert_eq!(cut.get("unsupported_at"), &Json::Null);
        let prefix = json(&reader.search(2, "🦀", encoding, 8, end, || false).unwrap());
        assert_eq!(prefix.get("state").text(), "byte-limit");
        assert!(!prefix.get("search_complete").flag());
        assert!(!prefix.get("truncated").flag());
        assert_hit(&prefix, 0, start, end);
        let complete = json(&reader.search(3, "🦀", encoding, 8, bytes.len() as u64, || false).unwrap());
        assert!(complete.get("search_complete").flag());
        assert_hit(&complete, 0, start, end);
    }
}

#[test]
fn malformed_later_input_invalidates_earlier_exact_text_hits_without_claiming_absence() {
    let mut bytes = vec![b'x'; BUFFER_BYTES + 1];
    bytes[..6].copy_from_slice(b"needle"); bytes[BUFFER_BYTES] = 0xff;
    let fixture = Fixture::new(); fixture.write(&bytes);
    let mut reader = fixture.reader();
    let response = reader.search(1, "needle", ObservedReaderEncoding::Utf8, 8, 64 * 1024, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let found = json(&response);
    assert_eq!(found.get("state").text(), "unsupported-text");
    assert_eq!(found.get("unsupported_at").number(), BUFFER_BYTES as u64);
    assert_eq!(found.get("matches_seen").number(), 0);
    assert!(found.get("hits").array().is_empty());
    assert!(!found.get("search_complete").flag());
    assert!(!found.get("input_complete").flag());
    assert!(!found.get("truncated").flag());
    assert_eq!(found.get("literal_original_hex"), &Json::Null);
    assert_eq!(reader.search_hit(1, 0, || false).err(), Some(ObservedReaderError::MissingHit));
}

#[test]
fn pathname_replacement_never_changes_the_scanned_open_object() {
    let fixture = Fixture::new(); fixture.write(b"old needle");
    let mut reader = fixture.reader();
    fs::rename(fixture.0.join("source"), fixture.0.join("moved")).unwrap();
    fixture.write(b"new source");
    let found = find(&mut reader, 1, "old");
    assert_hit(&found, 0, 0, 3);
    assert_eq!(json(&reader.search_hit(1, 0, || false).unwrap()).get("text").text(), "old");
    assert!(find(&mut reader, 2, "new").get("hits").array().is_empty());
    let mut reopened = fixture.reader();
    let current = find(&mut reopened, 1, "new");
    assert_hit(&current, 0, 0, 3);
    assert_ne!(current.get("owner"), found.get("owner"));
    assert_eq!(current.get("path"), found.get("path"));
}

#[test]
fn live_writes_cannot_replace_witness_bytes_and_current_context_gets_a_fresh_observation() {
    let fixture = Fixture::new(); fixture.write(b"banana");
    let mut reader = fixture.reader();
    let found = find(&mut reader, 1, "ana");
    fixture.write(b"xxxxxx");
    let before = json(&reader.info(|| false).unwrap());
    let witness = json(&reader.search_hit(1, 0, || false).unwrap());
    assert_eq!(witness.get("text").text(), "ana");
    assert_eq!(witness.get("original_hex").text(), "616e61");
    assert_eq!(bounds(witness.get("original_range")), (1, 4));
    assert_eq!(witness.get("witness_sha256"), found.get("literal_sha256"));
    assert_eq!(witness.get("search_observation"), found.get("search_observation"));
    assert_eq!(before, json(&reader.info(|| false).unwrap()));
    let current = json(&reader.read_window(1, 4, ObservedReaderEncoding::Utf8, || false).unwrap());
    assert_eq!(current.get("text").text(), "xxxx");
    assert!(current.get("observation").number() > found.get("search_observation").number());
    let old_again = json(&reader.search_hit(1, 0, || false).unwrap());
    assert_eq!(old_again, witness, "explicit current context cannot mutate the accepted search");
    assert!(find(&mut reader, 2, "ana").get("hits").array().is_empty());
    assert_eq!(reader.search_hit(1, 0, || false).err(), Some(ObservedReaderError::StaleQuery));
}

#[test]
fn failed_attempts_consume_both_identity_domains_and_preserve_the_accepted_query_and_pages() {
    let fixture = Fixture::new(); fixture.write(b"banana");
    let mut reader = fixture.reader();
    let page = json(&reader.read_window(0, 6, ObservedReaderEncoding::Utf8, || false).unwrap());
    let accepted = find(&mut reader, 3, "ana");
    assert_eq!(page.get("observation").number(), 1);
    assert_eq!(accepted.get("query_generation").number(), 3);
    assert_eq!(accepted.get("search_observation").number(), 2);
    assert_eq!(reader.search(3, "ana", ObservedReaderEncoding::Utf8, 8, 64, || false).err(), Some(ObservedReaderError::StaleQuery));
    assert_eq!(reader.search(4, "", ObservedReaderEncoding::Utf8, 8, 64, || false).err(), Some(ObservedReaderError::InvalidLimits));
    assert!(reader.search(5, "ana", ObservedReaderEncoding::Utf8, 8, 64, || true).err().unwrap().is_canceled());
    assert_eq!(reader.search(5, "ana", ObservedReaderEncoding::Utf8, 8, 64, || false).err(), Some(ObservedReaderError::StaleQuery));
    assert_eq!(json(&reader.search_hit(3, 0, || false).unwrap()).get("text").text(), "ana");
    assert_eq!(reader.search_hit(3, 2, || false).err(), Some(ObservedReaderError::MissingHit));
    assert!(reader.retained_window(1, || false).is_ok());
    let current = json(&reader.read_window(0, 6, ObservedReaderEncoding::Utf8, || false).unwrap());
    assert_eq!(current.get("observation").number(), 5);
    let replacement = find(&mut reader, 6, "ban");
    assert_eq!(replacement.get("search_observation").number(), 6);
    assert_eq!(reader.search_hit(3, 0, || false).err(), Some(ObservedReaderError::StaleQuery));
    assert_eq!(json(&reader.search_hit(6, 0, || false).unwrap()).get("text").text(), "ban");
}

#[test]
fn early_mid_scan_and_final_delivery_cancellation_keep_old_hits_and_charge_actual_io() {
    let mut bytes = vec![b'x'; 256 * 1024]; bytes[..6].copy_from_slice(b"needle");
    let fixture = Fixture::new(); fixture.write(&bytes);
    let seed = || {
        let mut reader = fixture.reader();
        reader.search(1, "needle", ObservedReaderEncoding::Utf8, 8, 6, || false).unwrap();
        reader.read_window(0, 8, ObservedReaderEncoding::Utf8, || false).unwrap();
        reader
    };
    let mut baseline = seed();
    let mut gates = 0;
    baseline.search(2, "absent", ObservedReaderEncoding::Utf8, 8, bytes.len() as u64,
        || { gates += 1; false }).unwrap();
    assert!(gates > 16);
    for stop in [1, gates / 2, gates] {
        let mut reader = seed();
        let before = json(&reader.info(|| false).unwrap());
        let mut polls = 0;
        let error = reader.search(2, "absent", ObservedReaderEncoding::Utf8, 8, bytes.len() as u64,
            || { polls += 1; polls == stop }).err().unwrap();
        assert!(error.is_canceled());
        // The engine may observe cancellation again at the end of its bounded
        // step; that extra poll must not resurrect or publish the canceled job.
        assert!(polls >= stop);
        let after = json(&reader.info(|| false).unwrap());
        assert_eq!(before.get("retained_observations"), after.get("retained_observations"));
        let spent = after.get("payload_bytes_read").number() - before.get("payload_bytes_read").number();
        let calls = after.get("read_calls").number() - before.get("read_calls").number();
        if stop == 1 { assert_eq!((spent, calls), (0, 0)); }
        else if stop == gates { assert_eq!(spent, bytes.len() as u64); assert!(calls > 0); }
        else { assert!(spent > 0 && spent < bytes.len() as u64); assert!(calls > 0); }
        assert_eq!(json(&reader.search_hit(1, 0, || false).unwrap()).get("text").text(), "needle");
        assert!(reader.search_hit(2, 0, || false).is_err());
        assert!(reader.retained_window(2, || false).is_ok());
        assert_eq!(reader.search(2, "needle", ObservedReaderEncoding::Utf8, 8, 6, || false).err(), Some(ObservedReaderError::StaleQuery));
        let page = json(&reader.read_window(0, 8, ObservedReaderEncoding::Utf8, || false).unwrap());
        assert_eq!(page.get("observation").number(), 4);
    }
}

#[test]
fn oversized_requests_fail_before_payload_reads_and_cannot_replace_a_good_query() {
    let fixture = Fixture::new(); fixture.write(b"needle");
    let mut reader = fixture.reader();
    find(&mut reader, 1, "needle");
    let before = json(&reader.info(|| false).unwrap());
    let long = "x".repeat(MAX_OBSERVED_READER_NEEDLE_BYTES + 1);
    for (generation, needle, matches, bytes) in [(2, long.as_str(), 8, 64),
        (3, "needle", MAX_OBSERVED_READER_MATCHES + 1, 64),
        (4, "needle", 8, MAX_OBSERVED_READER_SCAN_BYTES + 1)] {
        assert_eq!(reader.search(generation, needle, ObservedReaderEncoding::Utf8, matches, bytes, || false).err(),
            Some(ObservedReaderError::InvalidLimits));
    }
    assert_eq!(before, json(&reader.info(|| false).unwrap()));
    assert_eq!(json(&reader.search_hit(1, 0, || false).unwrap()).get("text").text(), "needle");
    assert_eq!(reader.search(4, "needle", ObservedReaderEncoding::Utf8, 8, 64, || false).err(), Some(ObservedReaderError::StaleQuery));
}


#[test]
fn counted_find_keeps_rows_bounded_but_finishes_exact_overlapping_count() {
    let fixture = Fixture::new(); fixture.write(b"banana banana");
    let mut reader = fixture.reader();
    let legacy = json(&reader.search(1, "ana", ObservedReaderEncoding::Utf8, 1, 64, || false).unwrap());
    assert_eq!(legacy.get("state").text(), "match-limit");
    assert_eq!(legacy.get("matches_seen").number(), 2);
    assert!(!legacy.get("count_all_matches").flag());
    assert!(!legacy.get("match_count_complete").flag());

    let response = reader.search_counted(2, "ana", ObservedReaderEncoding::Utf8, 1, 64, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL, "omitted rows remain explicit");
    let counted = json(&response);
    assert_eq!(counted.get("state").text(), "complete-observed-input");
    assert!(counted.get("search_complete").flag());
    assert!(counted.get("match_count_complete").flag());
    assert!(counted.get("count_all_matches").flag());
    assert!(counted.get("truncated").flag());
    assert_eq!(counted.get("matches_seen").number(), 4);
    assert_eq!(counted.get("retained_hits").number(), 1);
    assert_eq!(counted.get("payload_bytes_read").number(), 13);
    assert_hit(&counted, 0, 1, 4);
    let witness = json(&reader.search_hit(2, 0, || false).unwrap());
    assert!(witness.get("count_all_matches").flag());
    assert!(witness.get("match_count_complete").flag());
    assert_eq!(witness.get("original_hex").text(), "616e61");
    assert_eq!(reader.search_hit(2, 1, || false).err(), Some(ObservedReaderError::MissingHit));
}

#[test]
fn counted_find_reports_partial_count_when_scan_budget_ends() {
    let fixture = Fixture::new(); fixture.write(b"banana banana");
    let mut reader = fixture.reader();
    let response = reader.search_counted(1, "ana", ObservedReaderEncoding::Utf8, 1, 6, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let counted = json(&response);
    assert!(counted.get("count_all_matches").flag());
    assert!(!counted.get("search_complete").flag());
    assert!(!counted.get("match_count_complete").flag());
    assert_eq!(counted.get("state").text(), "byte-limit");
    assert_eq!(counted.get("matches_seen").number(), 2);
    assert_eq!(counted.get("retained_hits").number(), 1);
    assert!(counted.get("truncated").flag());
    assert_eq!(counted.get("payload_bytes_read").number(), 6);
}

#[test]
fn lowercase_find_keeps_per_hit_spelling_ranges_and_hashes_in_every_explicit_encoding() {
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let fixture = Fixture::new();
        let bytes = encoded("\u{feff}K K k", encoding);
        fixture.write(&bytes);
        let mut reader = fixture.reader();
        let found = json(&reader.find_matching(1, "k", encoding, false, true, 8, bytes.len() as u64, || false).unwrap());
        assert_eq!(found.get("mode").text(), "unicode-lowercase-text");
        assert!(!found.get("case_sensitive").flag());
        assert!(found.get("search_complete").flag());
        assert!(found.get("match_count_complete").flag());
        assert!(found.get("count_all_matches").flag());
        assert_eq!(found.get("matches_seen").number(), 3);
        assert_eq!(found.get("literal_original_hex"), &Json::Null);
        assert_eq!(found.get("literal_sha256"), &Json::Null);
        let before = json(&reader.info(|| false).unwrap());
        let mut offset = encoded("\u{feff}", encoding).len() as u64;
        let mut retained_bytes = 0;
        for (position, spelling) in ["K", "K", "k"].into_iter().enumerate() {
            let original = encoded(spelling, encoding);
            let row = &found.get("hits").array()[position];
            assert_eq!(bounds(row.get("original_range")), (offset, offset + original.len() as u64));
            let witness = json(&reader.search_hit(1, position, || false).unwrap());
            assert_eq!(witness.get("mode"), found.get("mode"));
            assert_eq!(witness.get("case_sensitive"), found.get("case_sensitive"));
            assert_eq!(witness.get("text").text(), spelling);
            assert_eq!(witness.get("original_hex").text(), hex(&original));
            let mut digest = fcb::store::Sha256::new(); digest.update(&original);
            assert_eq!(row.get("witness_sha256").text(), digest.finalize().to_hex());
            assert_eq!(witness.get("witness_sha256"), row.get("witness_sha256"));
            assert_eq!(witness.get("additional_source_bytes_read").number(), 0);
            retained_bytes += original.len() as u64;
            offset += original.len() as u64 + encoded(" ", encoding).len() as u64;
        }
        assert_eq!(found.get("retained_witness_bytes").number(), retained_bytes);
        assert_eq!(before.get("retained_search_witness_bytes").number(), retained_bytes);
        assert_eq!(before, json(&reader.info(|| false).unwrap()));
        let exact = json(&reader.search(2, "k", encoding, 8, bytes.len() as u64, || false).unwrap());
        assert!(exact.get("case_sensitive").flag());
        assert_eq!(exact.get("mode").text(), "exact-decoded-text");
        assert_eq!(exact.get("matches_seen").number(), 1);
        assert_eq!(exact.get("literal_original_hex").text(), hex(&encoded("k", encoding)));
    }
}

#[test]
fn lowercase_find_counts_without_rows_and_preserves_partial_and_canceled_policy() {
    let fixture = Fixture::new(); fixture.write("KKkK".as_bytes());
    let mut reader = fixture.reader();
    let counted = json(&reader.find_matching(1, "k", ObservedReaderEncoding::Utf8, false, true, 0, 1024, || false).unwrap());
    assert_eq!(counted.get("matches_seen").number(), 4);
    assert!(counted.get("match_count_complete").flag());
    assert!(counted.get("truncated").flag());
    assert_eq!(counted.get("retained_hits").number(), 0);
    assert_eq!(counted.get("retained_witness_bytes").number(), 0);
    assert!(reader.search_hit(1, 0, || false).is_err());

    let limited = json(&reader.find_matching(2, "k", ObservedReaderEncoding::Utf8, false, false, 1, 1024, || false).unwrap());
    assert_eq!(limited.get("state").text(), "match-limit");
    assert_eq!(limited.get("matches_seen").number(), 2);
    assert!(!limited.get("match_count_complete").flag());
    assert_eq!(limited.get("retained_witness_bytes").number(), 3);
    let witness = json(&reader.search_hit(2, 0, || false).unwrap());
    assert_eq!(witness.get("text").text(), "K");
    let io = json(&reader.info(|| false).unwrap());
    assert_eq!(reader.find_matching(3, "K", ObservedReaderEncoding::Utf8, false, true, 8, 1024, || true).err(),
        Some(ObservedReaderError::Canceled));
    assert_eq!(json(&reader.search_hit(2, 0, || false).unwrap()), witness);
    assert_eq!(json(&reader.info(|| false).unwrap()).get("payload_bytes_read"), io.get("payload_bytes_read"));
    let partial = json(&reader.find_matching(4, "k", ObservedReaderEncoding::Utf8, false, true, 8, 4, || false).unwrap());
    assert_eq!(partial.get("state").text(), "byte-limit");
    assert_eq!(partial.get("matches_seen").number(), 2);
    assert!(!partial.get("match_count_complete").flag());
    assert_eq!(partial.get("payload_bytes_read").number(), 4);
}

#[test]
fn lowercase_current_context_requires_original_spelling_and_retained_history_survives_change() {
    let fixture = Fixture::new(); fixture.write("K K k".as_bytes());
    let mut reader = fixture.reader();
    reader.find_matching(1, "k", ObservedReaderEncoding::Utf8, false, true, 8, 1024, || false).unwrap();
    let retained = json(&reader.search_hit(1, 0, || false).unwrap());
    let page = json(&reader.observe_hit(1, 0, 0, 64, || false).unwrap());
    let observation = page.get("observation").number();
    assert!(page.get("find_hit").get("witness_verified").flag());
    let current = OpenOptions::new().write(true).open(fixture.0.join("source")).unwrap();
    current.write_at(b"K  ", 0).unwrap();
    assert_eq!(reader.observe_hit(1, 0, 0, 64, || false).err(), Some(ObservedReaderError::SearchHitChanged));
    assert_eq!(json(&reader.search_hit(1, 0, || false).unwrap()), retained);
    let replay = json(&reader.retained_window(observation, || false).unwrap());
    assert_eq!(replay.get("find_hit"), page.get("find_hit"));
    assert!(replay.get("text").text().starts_with("K"));
}

#[test]
fn malformed_late_lowercase_source_revokes_tentative_counts_and_witnesses() {
    let fixture = Fixture::new();
    let mut bytes = "K K".as_bytes().to_vec(); bytes.resize(BUFFER_BYTES, b' '); bytes.push(0xff);
    fixture.write(&bytes);
    let mut reader = fixture.reader();
    let report = json(&reader.find_matching(1, "k", ObservedReaderEncoding::Utf8, false, true, 8,
        bytes.len() as u64, || false).unwrap());
    assert_eq!(report.get("state").text(), "unsupported-text");
    assert!(!report.get("match_count_complete").flag());
    assert_eq!(report.get("matches_seen").number(), 0);
    assert_eq!(report.get("retained_witness_bytes").number(), 0);
    assert_eq!(report.get("unsupported_at").number(), BUFFER_BYTES as u64);
    assert!(report.get("hits").array().is_empty());
}

#[test]
fn later_result_pages_reach_matches_beyond_256_and_preserve_exact_activation_history() {
    let fixture = Fixture::new(); fixture.write(&b"cat\n".repeat(300));
    let mut reader = fixture.reader();
    let first = json(&reader.search(1, "cat", ObservedReaderEncoding::Utf8, 256, 4096, || false).unwrap());
    assert_eq!(first.get("retained_hits").number(), 256);
    assert_eq!(first.get("matches_seen").number(), 257);
    assert_eq!(first.get("page_start_after").number(), 0);
    assert_eq!(first.get("skipped_matches").number(), 0);
    assert!(first.get("page_has_more_matches").flag());
    assert!(!first.get("search_complete").flag());

    let response = reader.find_page(2, "cat", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Text, true, false, 256, 256, 4096, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL, "skipped matches are not retained on this page");
    let later = json(&response);
    assert_eq!(later.get("page_start_after").number(), 256);
    assert_eq!(later.get("skipped_matches").number(), 256);
    assert_eq!(later.get("retained_hits").number(), 44);
    assert_eq!(later.get("matches_seen").number(), 300);
    assert!(later.get("truncated").flag());
    assert!(later.get("match_count_complete").flag());
    assert!(!later.get("page_has_more_matches").flag());
    assert!(later.get("search_observation").number() > first.get("search_observation").number());
    for (index, hit) in later.get("hits").array().iter().enumerate() {
        assert_eq!(hit.get("index").number(), index as u64);
        assert_eq!(hit.get("hit_index").number(), index as u64);
        assert_eq!(hit.get("occurrence_id").number(), 257 + index as u64);
        let start = (256 + index as u64) * 4;
        assert_eq!(bounds(hit.get("original_range")), (start, start + 3));
    }
    let before = json(&reader.info(|| false).unwrap());
    let witness = json(&reader.search_hit(2, 43, || false).unwrap());
    assert_eq!(witness.get("page_start_after").number(), 256);
    assert_eq!(witness.get("occurrence_id").number(), 300);
    assert_eq!(witness.get("original_hex").text(), "636174");
    assert_eq!(json(&reader.info(|| false).unwrap()), before, "retained selection performs no source I/O");
    let page = json(&reader.observe_hit(2, 43, 4, 64, || false).unwrap());
    let observation = page.get("observation").number();
    assert!(observation > later.get("search_observation").number());
    let selected = page.get("find_hit");
    assert_eq!(selected.get("page_start_after").number(), 256);
    assert_eq!(selected.get("hit_index").number(), 43);
    assert_eq!(selected.get("occurrence_id").number(), 300);
    assert_eq!(bounds(selected.get("original_range")), (1196, 1199));

    // A rescan sees current bytes and assigns a new identity. It cannot make
    // occurrence 257 in this new scan authoritative for the previous scan.
    let writer = OpenOptions::new().write(true).open(fixture.0.join("source")).unwrap();
    writer.write_at(b"dog", 0).unwrap();
    let changed = json(&reader.find_page(3, "cat", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Text, true, false, 256, 256, 4096, || false).unwrap());
    assert_eq!(changed.get("matches_seen").number(), 299);
    assert_eq!(changed.get("retained_hits").number(), 43);
    assert_eq!(bounds(changed.get("hits").array()[0].get("original_range")), (1028, 1031));
    assert!(changed.get("search_observation").number() > observation);
    assert_eq!(reader.search_hit(2, 43, || false).err(), Some(ObservedReaderError::StaleQuery));
    let replay = json(&reader.retained_window(observation, || false).unwrap());
    assert_eq!(replay.get("find_hit"), selected);
    assert_eq!(replay.get("additional_source_bytes_read").number(), 0);
}

#[test]
fn page_skips_counting_and_exhaustion_keep_independent_work_and_retention_limits() {
    let fixture = Fixture::new(); fixture.write(b"banana banana");
    let mut reader = fixture.reader();
    let counted = json(&reader.find_page(1, "ana", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Text, true, true, 2, 1, 64, || false).unwrap());
    assert!(counted.get("search_complete").flag());
    assert!(counted.get("count_all_matches").flag());
    assert!(counted.get("page_has_more_matches").flag());
    assert_eq!(counted.get("matches_seen").number(), 4);
    assert_eq!(counted.get("skipped_matches").number(), 2);
    assert_eq!(counted.get("retained_hits").number(), 1);
    assert_eq!(counted.get("hits").array()[0].get("occurrence_id").number(), 3);
    assert_eq!(bounds(counted.get("hits").array()[0].get("original_range")), (8, 11));

    let partial = json(&reader.find_page(2, "ana", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Text, true, true, 3, 1, 6, || false).unwrap());
    assert_eq!(partial.get("state").text(), "byte-limit");
    assert_eq!(partial.get("payload_bytes_read").number(), 6);
    assert_eq!(partial.get("page_start_after").number(), 3);
    assert_eq!(partial.get("skipped_matches").number(), 2);
    assert_eq!(partial.get("retained_hits").number(), 0);
    assert!(!partial.get("page_has_more_matches").flag());
    assert!(!partial.get("match_count_complete").flag(), "no rows before a budget stop is not EOF");

    let beyond = json(&reader.find_page(3, "ana", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Text, true, false, u64::MAX, 256, 64, || false).unwrap());
    assert_eq!(beyond.get("page_start_after").number(), u64::MAX);
    assert_eq!(beyond.get("matches_seen").number(), 4);
    assert_eq!(beyond.get("skipped_matches").number(), 4);
    assert_eq!(beyond.get("retained_hits").number(), 0);
    assert!(beyond.get("search_complete").flag());
    assert!(!beyond.get("page_has_more_matches").flag());

    let count_only = json(&reader.find_page(4, "ana", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Text, true, true, 2, 0, 64, || false).unwrap());
    assert!(count_only.get("match_count_complete").flag());
    assert!(count_only.get("page_has_more_matches").flag());
    assert_eq!(count_only.get("matches_seen").number(), 4);
    assert_eq!(count_only.get("retained_hits").number(), 0);
    assert_eq!(count_only.get("retained_witness_bytes").number(), 0);
}

#[test]
fn page_replays_preserve_utf16_lowercase_expansion_and_whole_token_witnesses() {
    let source = "\u{feff}K K k İ i\u{307} cat bobcat cat2 cat";
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let fixture = Fixture::new(); fixture.write(&encoded(source, encoding));
        let mut reader = fixture.reader();
        let lower = json(&reader.find_page(1, "k", encoding, ObservedReaderSearchKind::Text,
            false, true, 1, 1, 1024, || false).unwrap());
        assert_eq!(lower.get("matches_seen").number(), 3);
        assert_eq!(lower.get("hits").array()[0].get("occurrence_id").number(), 2);
        assert!(lower.get("page_has_more_matches").flag());
        let witness = json(&reader.search_hit(1, 0, || false).unwrap());
        assert_eq!(witness.get("text").text(), "K");
        assert_eq!(witness.get("original_hex").text(), hex(&encoded("K", encoding)));
        let start = encoded("\u{feff}K ", encoding).len() as u64;
        assert_eq!(bounds(witness.get("original_range")), (start, start + encoded("K", encoding).len() as u64));

        // The first dot comes from lowering İ; the next is an original scalar.
        // Skipping by occurrence must retain the latter's exact bytes only.
        let expansion = json(&reader.find_page(2, "\u{307}", encoding, ObservedReaderSearchKind::Text,
            false, true, 1, 1, 1024, || false).unwrap());
        assert_eq!(expansion.get("matches_seen").number(), 2);
        assert!(!expansion.get("page_has_more_matches").flag());
        let witness = json(&reader.search_hit(2, 0, || false).unwrap());
        assert_eq!(witness.get("text").text(), "\u{307}");
        assert_eq!(witness.get("original_hex").text(), hex(&encoded("\u{307}", encoding)));

        let tokens = json(&reader.find_page(3, "cat", encoding, ObservedReaderSearchKind::Occurrences,
            true, true, 1, 1, 1024, || false).unwrap());
        assert_eq!(tokens.get("matches_seen").number(), 2);
        assert_eq!(tokens.get("hits").array()[0].get("occurrence_id").number(), 2);
        let start = encoded(&source[..source.rfind("cat").unwrap()], encoding).len() as u64;
        assert_eq!(bounds(tokens.get("hits").array()[0].get("original_range")),
            (start, start + encoded("cat", encoding).len() as u64));
        let page = json(&reader.observe_hit(3, 0, 0, 64, || false).unwrap());
        assert!(page.get("find_hit").get("token_boundaries_verified").flag());
        assert_eq!(page.get("find_hit").get("page_start_after").number(), 1);
        assert_eq!(page.get("find_hit").get("occurrence_id").number(), 2);
    }
}

#[test]
fn failed_and_canceled_page_attempts_preserve_the_accepted_page_and_consume_generations() {
    let fixture = Fixture::new(); fixture.write(b"cat cat cat");
    let mut reader = fixture.reader();
    reader.find_page(1, "cat", ObservedReaderEncoding::Utf8, ObservedReaderSearchKind::Text,
        true, true, 1, 1, 64, || false).unwrap();
    let witness = json(&reader.search_hit(1, 0, || false).unwrap());
    let info = json(&reader.info(|| false).unwrap());
    assert_eq!(reader.find_page(2, "cat", ObservedReaderEncoding::Utf8, ObservedReaderSearchKind::Text,
        true, false, 2, 257, 64, || false).err(), Some(ObservedReaderError::InvalidLimits));
    assert_eq!(reader.find_page(2, "cat", ObservedReaderEncoding::Utf8, ObservedReaderSearchKind::Text,
        true, false, 2, 1, 64, || false).err(), Some(ObservedReaderError::StaleQuery));
    assert_eq!(reader.find_page(3, "cat", ObservedReaderEncoding::Utf8, ObservedReaderSearchKind::Text,
        true, false, 2, 1, 64, || true).err(), Some(ObservedReaderError::Canceled));
    assert_eq!(reader.find_page(3, "cat", ObservedReaderEncoding::Utf8, ObservedReaderSearchKind::Text,
        true, false, 2, 1, 64, || false).err(), Some(ObservedReaderError::StaleQuery));
    assert_eq!(json(&reader.search_hit(1, 0, || false).unwrap()), witness);
    assert_eq!(json(&reader.info(|| false).unwrap()), info);
    let next = json(&reader.find_page(4, "cat", ObservedReaderEncoding::Utf8, ObservedReaderSearchKind::Text,
        true, false, 2, 1, 64, || false).unwrap());
    assert!(next.get("search_observation").number() >= witness.get("search_observation").number() + 3);
    assert_eq!(next.get("hits").array()[0].get("occurrence_id").number(), 3);
}

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
    ObservedReaderError, ObservedReaderSession, MAX_OBSERVED_READER_MATCHES,
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

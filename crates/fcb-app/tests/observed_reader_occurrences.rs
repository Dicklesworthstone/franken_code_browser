#![forbid(unsafe_code)]
#![cfg(unix)]

//! Whole-token Find through the actual open-file host, not a complete-capture
//! substitute. Independent fixture ranges distinguish literal substring hits
//! from token occurrences, including UTF-16, read limits and current edits.
//! Code-first, independent strict-RCH batch verification pending.
mod support;

use std::{fs::{self, OpenOptions}, os::unix::fs::FileExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::{HostResponse, observed_reader::{
    ObservedReaderEncoding, ObservedReaderError, ObservedReaderSearchKind,
    ObservedReaderSession, MAX_OBSERVED_READER_OCCURRENCE_BYTES}}};
use support::{Json, parse};

const BUFFER_BYTES: usize = 16 * 1024;
struct Fixture(PathBuf);
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-observed-occurrences-{}-{now}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("source"), bytes).unwrap();
        Self(root)
    }
    fn file(&self) -> fs::File {
        OpenOptions::new().read(true).write(true).open(self.0.join("source")).unwrap()
    }
    fn reader(&self) -> ObservedReaderSession {
        static NEXT: AtomicU64 = AtomicU64::new(94_000);
        ObservedReaderSession::open(ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap(),
            &self.0, b"source", || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(value: &Json) -> (u64, u64) { (value.get("start").number(), value.get("end").number()) }
fn encoded(text: &str, encoding: ObservedReaderEncoding) -> Vec<u8> {
    match encoding {
        ObservedReaderEncoding::Utf8 => text.as_bytes().to_vec(),
        ObservedReaderEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ObservedReaderEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
    }
}
fn occurrences(reader: &mut ObservedReaderSession, generation: u64, name: &str,
    encoding: ObservedReaderEncoding, count_all: bool, rows: usize, bytes: u64) -> HostResponse {
    reader.find_with_kind(generation, name, encoding, ObservedReaderSearchKind::Occurrences,
        true, count_all, rows, bytes, || false).unwrap()
}
fn assert_mode(value: &Json) {
    assert_eq!(value.get("mode").text(), "whole-token-occurrences");
    assert_eq!(value.get("requested_mode").text(), "occurrences");
    assert_eq!(value.get("realized_mode"), value.get("mode"));
    assert!(value.get("case_sensitive").flag());
    assert_eq!(value.get("evidence_level").text(), "whole-token-text-candidate");
    assert!(value.get("includes_comments_and_literals").flag());
    assert!(!value.get("compiler_resolved").flag());
    assert!(!value.get("semantic_complete").flag());
}
fn hit_ranges(value: &Json) -> Vec<(u64, u64)> {
    value.get("hits").array().iter().map(|hit| bounds(hit.get("original_range"))).collect()
}

#[test]
fn whole_token_mode_has_independent_fixture_ranges_and_literal_negative_controls() {
    // Positive rows are specified by the fixture, not by the production cursor.
    // Comments and quoted strings intentionally remain text candidates.
    let cases = [
        ("", "", true), ("", "bar", false), ("_", "", false),
        ("", "_", false), ("$", "", false), ("1", "", false),
        ("", "()", true), ("// ", "", true), ("\"", "\"", true),
        ("é", "", false), ("", "\u{301}", false), ("", "—other", false),
        ("", "\u{a0}", true), ("other.", "", true),
    ];
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le,
        ObservedReaderEncoding::Utf16Be] {
        let mut source = String::from("\u{feff}");
        let mut expected = Vec::new();
        for (prefix, suffix, accepted) in cases {
            source.push_str(prefix);
            let start = encoded(&source, encoding).len() as u64;
            source.push_str("foo");
            let end = encoded(&source, encoding).len() as u64;
            if accepted { expected.push((start, end)); }
            source.push_str(suffix);
            source.push('\n');
        }
        source.push_str("Foo\nfoo"); // Case mismatch, then a true EOF boundary.
        let end = encoded(&source, encoding).len() as u64;
        expected.push((end - encoded("foo", encoding).len() as u64, end));
        let bytes = encoded(&source, encoding);
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        let response = occurrences(&mut reader, 1, "foo", encoding, false, 64, bytes.len() as u64);
        assert_eq!(response.exit_code(), EXIT_OK);
        let found = json(&response);
        assert_mode(&found);
        assert!(found.get("search_complete").flag());
        assert!(found.get("match_count_complete").flag());
        assert_eq!(hit_ranges(&found), expected);
        assert_eq!(found.get("matches_seen").number(), expected.len() as u64);
        assert_eq!(found.get("retained_witness_bytes").number(), encoded("foo", encoding).len() as u64,
            "all exact token occurrences share one literal witness");
        for (index, &(start, end)) in expected.iter().enumerate() {
            let hit = json(&reader.search_hit(1, index, || false).unwrap());
            assert_mode(&hit);
            assert_eq!(hit.get("text").text(), "foo");
            assert_eq!(hit.get("original_hex").text(), hex(&bytes[start as usize..end as usize]));
            assert_eq!(hit.get("additional_source_bytes_read").number(), 0);
            assert_eq!(hit.get("witness_sha256"), found.get("literal_sha256"));
        }
        // A literal route would falsely include every embedded substring.
        let literal = json(&reader.find(2, "foo", encoding, 64, bytes.len() as u64, || false).unwrap());
        assert_eq!(literal.get("mode").text(), "exact-decoded-text");
        assert!(literal.get("matches_seen").number() > expected.len() as u64);
        assert_ne!(hit_ranges(&literal), expected);
    }
}

#[test]
fn large_sparse_file_crosses_input_chunks_and_retains_only_one_exact_witness() {
    let fixture = Fixture::new(b"");
    let file = fixture.file();
    let cross_chunk = BUFFER_BYTES as u64 - 2;
    let distant = 33 * 1024 * 1024 + 17;
    let length = distant + 32;
    file.set_len(length).unwrap();
    assert_eq!(file.write_at(b"needle ", cross_chunk).unwrap(), 7);
    assert_eq!(file.write_at(b"needle ", distant).unwrap(), 7);
    let mut reader = fixture.reader();
    let response = occurrences(&mut reader, 7, "needle", ObservedReaderEncoding::Utf8, false, 4, length);
    assert_eq!(response.exit_code(), EXIT_OK);
    let found = json(&response);
    assert_eq!(hit_ranges(&found), [(cross_chunk, cross_chunk + 6), (distant, distant + 6)]);
    assert_eq!(found.get("payload_bytes_read").number(), length);
    assert_eq!(found.get("peak_input_buffer_bytes").number(), BUFFER_BYTES as u64);
    assert_eq!(found.get("retained_witness_bytes").number(), 6);
    assert!(!found.get("whole_file_immutable").flag());
    let before = json(&reader.info(|| false).unwrap());
    assert_eq!(before.get("retained_payload_bytes").number(), 0);
    assert!(before.get("retained_observations").array().is_empty());
    let witness = json(&reader.search_hit(7, 1, || false).unwrap());
    assert_mode(&witness);
    assert_eq!(witness.get("text").text(), "needle");
    assert_eq!(json(&reader.info(|| false).unwrap()), before,
        "retained activation performs no source or metadata I/O");
}

#[test]
fn unicode_names_cross_decoder_chunks_and_map_back_to_utf16_original_units() {
    let name = "e\u{301}🦀";
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le,
        ObservedReaderEncoding::Utf16Be] {
        let bom = encoded("\u{feff}", encoding).len();
        let unit = encoded(" ", encoding).len();
        let prefix = format!("\u{feff}{}", " ".repeat((BUFFER_BYTES - bom - unit) / unit));
        let source = format!("{prefix}{name} {name}x {name}");
        let bytes = encoded(&source, encoding);
        let first = encoded(&prefix, encoding).len() as u64;
        let last = encoded(&format!("{prefix}{name} {name}x "), encoding).len() as u64;
        let name_bytes = encoded(name, encoding);
        let fixture = Fixture::new(&bytes);
        let mut reader = fixture.reader();
        let report = json(&occurrences(&mut reader, 1, name, encoding, false, 4, bytes.len() as u64));
        assert_mode(&report);
        assert_eq!(hit_ranges(&report), [(first, first + name_bytes.len() as u64),
            (last, last + name_bytes.len() as u64)]);
        assert_eq!(report.get("literal_original_hex").text(), hex(&name_bytes));
        for index in 0..2 {
            let page = json(&reader.observe_hit(1, index, 0, 64, || false).unwrap());
            let selected = page.get("find_hit");
            assert_mode(selected);
            assert!(selected.get("token_boundaries_verified").flag());
            let (from, to) = bounds(selected.get("decoded_utf8_range"));
            assert_eq!(&page.get("text").text()[from as usize..to as usize], name);
        }
    }
}

#[test]
fn byte_limits_do_not_supply_a_token_boundary_and_count_only_keeps_exact_totals_separate() {
    let fixture = Fixture::new(b"foo bar foo");
    let mut reader = fixture.reader();
    let prefix = json(&occurrences(&mut reader, 1, "foo", ObservedReaderEncoding::Utf8, false, 8, 3));
    assert_eq!(prefix.get("state").text(), "byte-limit");
    assert_eq!(prefix.get("matches_seen").number(), 0, "the unread suffix may continue the token");
    assert!(!prefix.get("search_complete").flag());
    let delimiter = json(&occurrences(&mut reader, 2, "foo", ObservedReaderEncoding::Utf8, false, 8, 4));
    assert_eq!(delimiter.get("state").text(), "byte-limit");
    assert_eq!(hit_ranges(&delimiter), [(0, 3)]);
    assert!(!delimiter.get("match_count_complete").flag());
    let counted = occurrences(&mut reader, 3, "foo", ObservedReaderEncoding::Utf8, true, 0, 11);
    assert_eq!(counted.exit_code(), EXIT_PARTIAL, "an exact count does not make omitted rows a complete response");
    let counted = json(&counted);
    assert_mode(&counted);
    assert!(counted.get("count_all_matches").flag());
    assert!(counted.get("match_count_complete").flag());
    assert!(counted.get("search_complete").flag());
    assert!(counted.get("truncated").flag());
    assert_eq!(counted.get("matches_seen").number(), 2);
    assert_eq!(counted.get("retained_witness_bytes").number(), 0);
    assert!(counted.get("hits").array().is_empty());
    assert_eq!(counted.get("literal_sha256"), &Json::Null);
    let exact_full = json(&occurrences(&mut reader, 4, "foo", ObservedReaderEncoding::Utf8, false, 2, 11));
    assert!(exact_full.get("search_complete").flag());
    assert!(!exact_full.get("truncated").flag());
    let limited = json(&occurrences(&mut reader, 5, "foo", ObservedReaderEncoding::Utf8, false, 1, 11));
    assert_eq!(limited.get("matches_seen").number(), 2);
    assert_eq!(limited.get("retained_hits").number(), 1);
    assert!(limited.get("truncated").flag());
}

#[test]
fn invalid_names_and_incompatible_case_policy_spend_no_source_io_or_accepted_query() {
    let fixture = Fixture::new(b"foo Foo foobar");
    let mut reader = fixture.reader();
    let accepted = json(&occurrences(&mut reader, 1, "foo", ObservedReaderEncoding::Utf8, false, 8, 64));
    let before = json(&reader.info(|| false).unwrap());
    let mut generation = 1;
    let oversized = "x".repeat(MAX_OBSERVED_READER_OCCURRENCE_BYTES + 1);
    for name in ["1foo", "foo bar", "foo-bar", "foo\n", "foo\0", oversized.as_str()] {
        generation += 1;
        let error = reader.find_with_kind(generation, name, ObservedReaderEncoding::Utf8,
            ObservedReaderSearchKind::Occurrences, true, false, 8, 64, || false).err().unwrap();
        assert_eq!(error.to_string(), "STREAM_READ_INVALID_IDENTIFIER");
    }
    generation += 1;
    assert_eq!(reader.find_with_kind(generation, "foo", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Occurrences, false, false, 8, 64, || false).err(),
        Some(ObservedReaderError::InvalidLimits));
    generation += 1;
    assert!(reader.find_with_kind(generation, "foo", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Occurrences, true, false, 8, 64, || true).err().unwrap().is_canceled());
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(after.get("payload_bytes_read"), before.get("payload_bytes_read"));
    assert_eq!(after.get("read_calls"), before.get("read_calls"));
    assert_eq!(after.get("accepted_search_generation"), before.get("accepted_search_generation"));
    let retained = json(&reader.search_hit(1, 0, || false).unwrap());
    assert_mode(&retained);
    assert_eq!(retained.get("witness_sha256"), accepted.get("literal_sha256"));
    assert_eq!(reader.find_with_kind(generation, "foo", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Occurrences, true, false, 8, 64, || false).err(),
        Some(ObservedReaderError::StaleQuery), "failed/canceled attempts still consume their generation");
    let lower = json(&reader.find_matching(generation + 1, "foo", ObservedReaderEncoding::Utf8,
        false, true, 8, 64, || false).unwrap());
    assert_eq!(lower.get("mode").text(), "unicode-lowercase-text");
    assert_eq!(lower.get("requested_mode").text(), "text");
    assert_eq!(lower.get("matches_seen").number(), 3, "legacy lowercase text keeps substring semantics");
}

#[test]
fn current_neighbor_edits_cannot_relabel_retained_witnesses_as_current_whole_tokens() {
    for (original, changed, expected_start) in [
        ("cat ", "catx", 0u64), (" cat ", "xcat ", 1u64),
    ] {
        for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le,
            ObservedReaderEncoding::Utf16Be] {
            let source = encoded(original, encoding);
            let replacement = encoded(changed, encoding);
            assert_eq!(source.len(), replacement.len());
            let fixture = Fixture::new(&source);
            let mut reader = fixture.reader();
            let found = json(&occurrences(&mut reader, 1, "cat", encoding, false, 4, source.len() as u64));
            let start = expected_start * encoded(" ", encoding).len() as u64;
            assert_eq!(hit_ranges(&found), [(start, start + encoded("cat", encoding).len() as u64)]);
            let initial = json(&reader.observe_hit(1, 0, 0, 64, || false).unwrap());
            assert_mode(initial.get("find_hit"));
            assert!(initial.get("find_hit").get("token_boundaries_verified").flag());
            let retained_id = initial.get("observation").number();
            assert_eq!(fixture.file().write_at(&replacement, 0).unwrap(), replacement.len());
            let before = json(&reader.info(|| false).unwrap());
            assert_eq!(reader.observe_hit(1, 0, 0, 64, || false).err(), Some(ObservedReaderError::SearchHitChanged));
            let after = json(&reader.info(|| false).unwrap());
            assert_eq!(after.get("retained_observations"), before.get("retained_observations"));
            assert!(after.get("payload_bytes_read").number() > before.get("payload_bytes_read").number());
            let witness = json(&reader.search_hit(1, 0, || false).unwrap());
            assert_mode(&witness);
            assert_eq!(witness.get("text").text(), "cat", "the old exact witness still exists");
            // Literal activation is intentionally a different permitted case.
            let literal = json(&reader.find(2, "cat", encoding, 4, replacement.len() as u64, || false).unwrap());
            assert_eq!(literal.get("matches_seen").number(), 1);
            let current = json(&reader.observe_hit(2, 0, 0, 64, || false).unwrap());
            assert_eq!(current.get("find_hit").get("mode").text(), "exact-decoded-text");
            assert!(!current.get("find_hit").get("token_boundaries_verified").flag());
            // An independent accepted observation keeps its original search
            // mode after the active query changes, with no live source reread.
            let replay = json(&reader.retained_window(retained_id, || false).unwrap());
            assert_eq!(replay.get("find_hit"), initial.get("find_hit"));
            assert_eq!(replay.get("text"), initial.get("text"));
            assert_eq!(replay.get("additional_source_bytes_read").number(), 0);
        }
    }
}

#[test]
fn true_input_boundaries_and_minimum_context_work_with_boms_and_crlf() {
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le,
        ObservedReaderEncoding::Utf16Be] {
        for text in ["cat", "\u{feff}cat", "\u{feff}cat\r\ncat"] {
            let bytes = encoded(text, encoding);
            let fixture = Fixture::new(&bytes);
            let mut reader = fixture.reader();
            let report = json(&occurrences(&mut reader, 1, "cat", encoding, false, 8, bytes.len() as u64));
            for index in 0..report.get("hits").array().len() {
                let page = json(&reader.observe_hit(1, index, 0, encoded("cat", encoding).len() + 24, || false).unwrap());
                assert_mode(page.get("find_hit"));
                assert!(page.get("find_hit").get("token_boundaries_verified").flag());
            }
        }
    }
    let fixture = Fixture::new(b"cat ");
    let mut reader = fixture.reader();
    occurrences(&mut reader, 1, "cat", ObservedReaderEncoding::Utf8, false, 8, 4);
    let before = json(&reader.info(|| false).unwrap());
    assert_eq!(reader.observe_hit(1, 0, 0, 26, || false).err(), Some(ObservedReaderError::InvalidLimits));
    let after = json(&reader.info(|| false).unwrap());
    assert_eq!(before.get("payload_bytes_read"), after.get("payload_bytes_read"));
}

#[test]
fn malformed_suffix_invalidates_token_results_and_cancellation_preserves_an_accepted_query() {
    let mut bytes = vec![b' '; BUFFER_BYTES * 64];
    bytes[..4].copy_from_slice(b"foo ");
    let fixture = Fixture::new(&bytes);
    let mut reader = fixture.reader();
    let initial = json(&occurrences(&mut reader, 1, "foo", ObservedReaderEncoding::Utf8, false, 8, bytes.len() as u64));
    let witness = json(&reader.search_hit(1, 0, || false).unwrap());
    let mut checks = 0usize;
    let canceled = reader.find_with_kind(2, "missing", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Occurrences, true, true, 8, bytes.len() as u64, || {
            checks += 1; checks > 20
        });
    assert!(canceled.err().unwrap().is_canceled());
    assert_eq!(json(&reader.search_hit(1, 0, || false).unwrap()), witness);
    assert_eq!(json(&reader.info(|| false).unwrap()).get("accepted_search_generation").number(), 1);
    assert_eq!(initial.get("matches_seen").number(), 1);

    assert_eq!(fixture.file().write_at(&[0xff], BUFFER_BYTES as u64).unwrap(), 1);
    let response = occurrences(&mut reader, 3, "foo", ObservedReaderEncoding::Utf8, false, 8, bytes.len() as u64);
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let invalid = json(&response);
    assert_mode(&invalid);
    assert_eq!(invalid.get("state").text(), "unsupported-text");
    assert_eq!(invalid.get("matches_seen").number(), 0);
    assert_eq!(invalid.get("retained_witness_bytes").number(), 0);
    assert!(invalid.get("hits").array().is_empty());
    assert!(!invalid.get("search_complete").flag());
}

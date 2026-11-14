#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real opened-file expression searches: whole-observation qualification,
//! exact primary witnesses and immutable provenance after context/query changes.
mod support;

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::{HostResponse, observed_reader::{
    ObservedReaderEncoding, ObservedReaderError, ObservedReaderSearchKind,
    ObservedReaderSession}}};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-observed-expression-{}-{now}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        let fixture = Self(path); fixture.write(bytes); fixture
    }
    fn write(&self, bytes: &[u8]) { fs::write(self.0.join("source"), bytes).unwrap(); }
    fn reader(&self) -> ObservedReaderSession {
        static NEXT: AtomicU64 = AtomicU64::new(86_000);
        ObservedReaderSession::open(ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap(),
            &self.0, b"source", || false).unwrap()
    }
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn expression(reader: &mut ObservedReaderSession, generation: u64, raw: &str,
    count_all: bool, max_matches: usize, max_bytes: u64) -> Result<HostResponse, ObservedReaderError> {
    reader.find_with_kind(generation, raw, ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Expression, true, count_all, max_matches, max_bytes, || false)
}
fn encoded(text: &str, encoding: ObservedReaderEncoding) -> Vec<u8> {
    match encoding {
        ObservedReaderEncoding::Utf8 => text.as_bytes().to_vec(),
        ObservedReaderEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ObservedReaderEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
    }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(value: &Json) -> (u64, u64) { (value.get("start").number(), value.get("end").number()) }
fn metadata(value: &Json, raw: &str, primary: &str, scope: &str) {
    assert_eq!(value.get("mode").text(), "decoded-text-expression");
    assert_eq!(value.get("requested_mode").text(), "expression");
    assert_eq!(value.get("realized_mode").text(), "decoded-text-expression");
    assert_eq!(value.get("expression").text(), raw);
    assert_eq!(value.get("primary_needle").text(), primary);
    assert_eq!(value.get("query_syntax").text(), "fcb-bounded-expression/1");
    assert_eq!(value.get("predicate_scope").text(), scope);
}

#[test]
fn expression_qualifies_every_predicate_before_exposing_exact_primary_witnesses() {
    let bytes = b"banana permit\n";
    let fixture = Fixture::new(bytes); let mut reader = fixture.reader();
    let raw = "\"ana\" permit -blocked";
    let response = expression(&mut reader, 1, raw, false, 1, 1024).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL, "one qualified primary row was omitted");
    let found = json(&response);
    metadata(&found, raw, "ana", "same-observed-stream");
    assert_eq!(found.get("needle").text(), raw);
    assert_eq!(found.get("literal_text").text(), "ana");
    assert!(found.get("expression_qualified").flag());
    assert!(found.get("input_complete").flag()); assert!(found.get("search_complete").flag());
    assert!(found.get("match_count_complete").flag());
    assert!(!found.get("count_all_matches").flag(), "frozen caller policy is echoed");
    assert!(found.get("truncated").flag()); assert!(found.get("page_has_more_matches").flag());
    assert_eq!(found.get("matches_seen").number(), 2);
    assert_eq!(found.get("retained_hits").number(), 1);
    assert_eq!(found.get("payload_bytes_read").number(), bytes.len() as u64);
    assert_eq!(found.get("retained_witness_bytes").number(), 3);
    assert_eq!(found.get("literal_original_hex").text(), "616e61");
    assert_eq!(bounds(found.get("hits").array()[0].get("original_range")), (1, 4));
    let before = json(&reader.info(|| false).unwrap());
    let selected = json(&reader.search_hit(1, 0, || false).unwrap());
    metadata(&selected, raw, "ana", "same-observed-stream");
    assert!(selected.get("expression_qualified").flag());
    assert_eq!(selected.get("text").text(), "ana");
    assert_eq!(selected.get("original_hex").text(), "616e61");
    assert_eq!(selected.get("witness_sha256"), found.get("literal_sha256"));
    assert_eq!(selected.get("additional_source_bytes_read").number(), 0);
    assert_eq!(json(&reader.info(|| false).unwrap()), before);
    assert!(before.get("retained_observations").array().is_empty(), "search never captures the whole file");
}

#[test]
fn late_exclusions_missing_conjunctions_and_missing_primary_publish_no_candidates() {
    let mut bytes = b"banana permit ".to_vec();
    bytes.resize(33 * 1024, b' '); bytes.extend_from_slice(b"blocked");
    let fixture = Fixture::new(&bytes); let mut reader = fixture.reader();
    for (offset, raw) in ["ana permit -blocked", "ana missing", "absent permit"].into_iter().enumerate() {
        let response = expression(&mut reader, offset as u64 + 1, raw, false, 1, bytes.len() as u64).unwrap();
        assert_eq!(response.exit_code(), EXIT_OK);
        let found = json(&response);
        assert!(!found.get("expression_qualified").flag());
        assert!(found.get("search_complete").flag()); assert!(found.get("match_count_complete").flag());
        assert_eq!(found.get("matches_seen").number(), 0);
        assert_eq!(found.get("retained_hits").number(), 0);
        assert_eq!(found.get("retained_witness_bytes").number(), 0);
        assert_eq!(found.get("literal_original_hex"), &Json::Null);
        assert_eq!(found.get("literal_sha256"), &Json::Null);
        assert_eq!(found.get("payload_bytes_read").number(), bytes.len() as u64);
        assert!(found.get("hits").array().is_empty());
        assert_eq!(reader.search_hit(offset as u64 + 1, 0, || false).err(), Some(ObservedReaderError::MissingHit));
    }
    let empty = Fixture::new(b""); let mut reader = empty.reader();
    let found = json(&expression(&mut reader, 1, "ana", false, 0, 0).unwrap());
    assert!(found.get("search_complete").flag()); assert!(!found.get("expression_qualified").flag());
}

#[test]
fn incomplete_and_malformed_observations_never_release_tentative_counts_or_hits() {
    let mut bytes = b"banana permit ".to_vec(); bytes.resize(33 * 1024, b' ');
    let fixture = Fixture::new(&bytes); let mut reader = fixture.reader();
    let response = expression(&mut reader, 1, "ana permit -blocked", false, 1, 64).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let partial = json(&response);
    assert_eq!(partial.get("state").text(), "byte-limit");
    assert_eq!(partial.get("expression_qualified"), &Json::Null);
    assert!(!partial.get("input_complete").flag()); assert!(!partial.get("match_count_complete").flag());
    assert_eq!(partial.get("matches_seen").number(), 0);
    assert_eq!(partial.get("retained_witness_bytes").number(), 0);
    assert_eq!(partial.get("payload_bytes_read").number(), 64);
    assert!(partial.get("hits").array().is_empty());
    bytes.push(0xff); fixture.write(&bytes);
    let malformed = json(&expression(&mut reader, 2, "ana permit", true, 8, bytes.len() as u64).unwrap());
    assert_eq!(malformed.get("state").text(), "unsupported-text");
    assert_eq!(malformed.get("expression_qualified"), &Json::Null);
    assert_eq!(malformed.get("matches_seen").number(), 0);
    assert_eq!(malformed.get("retained_hits").number(), 0);
    assert_eq!(malformed.get("unsupported_at").number(), bytes.len() as u64 - 1);
    assert!(!malformed.get("search_complete").flag());
}

#[test]
fn exact_and_lowercase_expression_witnesses_preserve_bom_utf16_and_original_spelling() {
    for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
        let bytes = encoded("\u{feff}🦀a🦀a🦀 permit", encoding);
        let fixture = Fixture::new(&bytes); let mut reader = fixture.reader();
        let raw = "\"🦀a🦀\" permit -blocked";
        let found = json(&reader.find_with_kind(1, raw, encoding, ObservedReaderSearchKind::Expression,
            true, false, 8, bytes.len() as u64, || false).unwrap());
        metadata(&found, raw, "🦀a🦀", "same-observed-stream");
        assert!(found.get("expression_qualified").flag());
        assert_eq!(found.get("matches_seen").number(), 2);
        let literal = encoded("🦀a🦀", encoding);
        assert_eq!(found.get("literal_original_hex").text(), hex(&literal));
        let start = encoded("\u{feff}", encoding).len() as u64;
        let stride = encoded("🦀a", encoding).len() as u64;
        for index in 0..2 {
            let from = start + index as u64 * stride;
            let selected = json(&reader.search_hit(1, index, || false).unwrap());
            assert_eq!(bounds(selected.get("original_range")), (from, from + literal.len() as u64));
            assert_eq!(selected.get("original_hex").text(), hex(&bytes[from as usize..from as usize + literal.len()]));
        }

        let bytes = encoded("\u{feff}K K k Permit", encoding); fixture.write(&bytes);
        let raw = "\"k\" permit -blocked";
        let lower = json(&reader.find_with_kind(2, raw, encoding, ObservedReaderSearchKind::Expression,
            false, true, 8, bytes.len() as u64, || false).unwrap());
        metadata(&lower, raw, "k", "same-observed-stream");
        assert!(!lower.get("case_sensitive").flag()); assert!(lower.get("expression_qualified").flag());
        assert_eq!(lower.get("matches_seen").number(), 3);
        assert_eq!(lower.get("literal_original_hex"), &Json::Null);
        assert_eq!(lower.get("literal_sha256"), &Json::Null);
        let mut offset = encoded("\u{feff}", encoding).len() as u64;
        for (index, spelling) in ["K", "K", "k"].into_iter().enumerate() {
            let original = encoded(spelling, encoding);
            let selected = json(&reader.search_hit(2, index, || false).unwrap());
            metadata(&selected, raw, "k", "same-observed-stream");
            assert_eq!(selected.get("text").text(), spelling);
            assert_eq!(selected.get("original_hex").text(), hex(&original));
            assert_eq!(bounds(selected.get("original_range")), (offset, offset + original.len() as u64));
            let mut digest = fcb::store::Sha256::new(); digest.update(&original);
            assert_eq!(selected.get("witness_sha256").text(), digest.finalize().to_hex());
            assert_eq!(selected.get("witness_sha256"), lower.get("hits").array()[index].get("witness_sha256"));
            offset += original.len() as u64 + encoded(" ", encoding).len() as u64;
        }
    }
}

#[test]
fn invalid_expressions_and_scope_filters_fail_before_io_and_preserve_the_accepted_query() {
    let fixture = Fixture::new(b"needle permit path:src"); let mut reader = fixture.reader();
    expression(&mut reader, 1, "needle permit", true, 8, 1024).unwrap();
    let selected = json(&reader.search_hit(1, 0, || false).unwrap());
    let before = json(&reader.info(|| false).unwrap());
    let cases = [
        ("".to_owned(), "QUERY_EMPTY"), (" \t ".to_owned(), "QUERY_EMPTY"),
        ("-blocked".to_owned(), "QUERY_EMPTY"), ("\"unterminated".to_owned(), "QUERY_SYNTAX_ERROR"),
        ("re:needle".to_owned(), "QUERY_REGEX_UNQUALIFIED"),
        ("x".repeat(1025), "QUERY_NEEDLE_TOO_LONG"), ("x ".repeat(65), "QUERY_LIMIT_EXCEEDED"),
        ("needle path:src".to_owned(), "OBSERVED_READER_EXPRESSION_SCOPE_UNSUPPORTED"),
        ("needle -path:src".to_owned(), "OBSERVED_READER_EXPRESSION_SCOPE_UNSUPPORTED"),
        ("needle lang:rust".to_owned(), "OBSERVED_READER_EXPRESSION_SCOPE_UNSUPPORTED"),
        ("needle -type:js".to_owned(), "OBSERVED_READER_EXPRESSION_SCOPE_UNSUPPORTED"),
    ];
    for (index, (raw, code)) in cases.into_iter().enumerate() {
        let generation = index as u64 + 2;
        let error = expression(&mut reader, generation, &raw, true, 8, 1024).err().unwrap();
        assert_eq!(error.to_string(), code, "{raw}");
        assert_eq!(expression(&mut reader, generation, "needle", true, 8, 1024).err(), Some(ObservedReaderError::StaleQuery));
        assert_eq!(json(&reader.search_hit(1, 0, || false).unwrap()), selected);
        assert_eq!(json(&reader.info(|| false).unwrap()), before);
    }
    let literal = json(&expression(&mut reader, 20, "\"path:src\"", true, 8, 1024).unwrap());
    assert!(literal.get("expression_qualified").flag());
    assert_eq!(literal.get("primary_needle").text(), "path:src");
    let boundary = expression(&mut reader, 21, &"x".repeat(1024), true, 8, 1024).unwrap();
    assert!(!json(&boundary).get("expression_qualified").flag());
}

#[test]
fn expression_pages_rescan_predicates_and_preserve_whole_input_occurrence_identity() {
    let mut bytes = b"ana ".repeat(300); bytes.extend_from_slice(b"permit");
    let fixture = Fixture::new(&bytes); let mut reader = fixture.reader();
    let first = json(&expression(&mut reader, 1, "ana permit", false, 256, 4096).unwrap());
    assert_eq!(first.get("matches_seen").number(), 300);
    assert_eq!(first.get("retained_hits").number(), 256);
    assert!(first.get("search_complete").flag()); assert!(first.get("page_has_more_matches").flag());
    let later = json(&reader.find_page(2, "ana permit", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Expression, true, false, 256, 16, 4096, || false).unwrap());
    assert!(later.get("expression_qualified").flag()); assert!(later.get("page_has_more_matches").flag());
    assert!(later.get("match_count_complete").flag());
    assert_eq!(later.get("matches_seen").number(), 300);
    assert_eq!(later.get("skipped_matches").number(), 256);
    assert!(later.get("search_observation").number() > first.get("search_observation").number());
    for (index, hit) in later.get("hits").array().iter().enumerate() {
        assert_eq!(hit.get("index").number(), index as u64);
        assert_eq!(hit.get("occurrence_id").number(), 257 + index as u64);
        let start = (256 + index as u64) * 4;
        assert_eq!(bounds(hit.get("original_range")), (start, start + 3));
    }
    let selected = json(&reader.search_hit(2, 15, || false).unwrap());
    assert_eq!(selected.get("occurrence_id").number(), 272);
    assert_eq!(selected.get("page_start_after").number(), 256);
    let beyond = json(&reader.find_page(3, "ana permit", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Expression, true, false, u64::MAX, 16, 4096, || false).unwrap());
    assert!(beyond.get("expression_qualified").flag());
    assert_eq!(beyond.get("skipped_matches").number(), 300);
    assert_eq!(beyond.get("retained_hits").number(), 0); assert!(!beyond.get("page_has_more_matches").flag());
    let count = json(&expression(&mut reader, 4, "ana permit", false, 0, 4096).unwrap());
    assert_eq!(count.get("matches_seen").number(), 300); assert_eq!(count.get("retained_witness_bytes").number(), 0);
    assert!(count.get("expression_qualified").flag()); assert!(count.get("match_count_complete").flag());
    // An exclusion introduced after page 1 must be evaluated by the new scan.
    bytes.extend_from_slice(b" blocked"); fixture.write(&bytes);
    let changed = json(&reader.find_page(5, "ana permit -blocked", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Expression, true, true, 256, 16, 4096, || false).unwrap());
    assert!(!changed.get("expression_qualified").flag());
    assert_eq!(changed.get("matches_seen").number(), 0); assert!(changed.get("hits").array().is_empty());
}

#[test]
fn fresh_context_verifies_only_primary_and_history_keeps_original_expression_provenance() {
    let fixture = Fixture::new(b"banana permit\n"); let mut reader = fixture.reader();
    let raw = "\"ana\" permit -blocked";
    let found = json(&expression(&mut reader, 1, raw, true, 8, 1024).unwrap());
    // The exact primary is unchanged, but the whole-file conjunction no longer
    // holds. Context activation must not claim to have rerun those predicates.
    fixture.write(b"banana denied\n");
    let page = json(&reader.observe_hit(1, 0, 0, 64, || false).unwrap());
    let observation = page.get("observation").number();
    assert!(observation > found.get("search_observation").number());
    assert!(page.get("text").text().contains("denied"));
    let proof = page.get("find_hit");
    metadata(proof, raw, "ana", "original-search-observation");
    assert!(proof.get("expression_qualified").flag());
    assert!(!proof.get("expression_revalidated").flag()); assert!(proof.get("witness_verified").flag());
    assert!(!proof.get("token_boundaries_verified").flag());
    assert_eq!(proof.get("search_observation"), found.get("search_observation"));
    fixture.write(b"bonono denied\n");
    assert_eq!(reader.observe_hit(1, 0, 0, 64, || false).err(), Some(ObservedReaderError::SearchHitChanged));
    reader.search(2, "denied", ObservedReaderEncoding::Utf8, 8, 1024, || false).unwrap();
    let before = json(&reader.info(|| false).unwrap());
    let replay = json(&reader.retained_window(observation, || false).unwrap());
    assert_eq!(replay.get("find_hit"), proof);
    assert_eq!(replay.get("text"), page.get("text"));
    assert_eq!(replay.get("additional_source_bytes_read").number(), 0);
    assert_eq!(json(&reader.info(|| false).unwrap()), before);
    let literal = reader.search_hit(2, 0, || false).unwrap();
    assert!(!literal.as_str().contains("\"expression\""));
    assert!(!literal.as_str().contains("\"expression_qualified\""));
}

#[test]
fn canceled_expression_attempts_preserve_frozen_query_and_account_actual_reads() {
    let mut bytes = b"needle permit ".to_vec(); bytes.resize(256 * 1024, b' ');
    let fixture = Fixture::new(&bytes);
    let seed = || {
        let mut reader = fixture.reader();
        expression(&mut reader, 1, "needle permit", true, 8, bytes.len() as u64).unwrap();
        reader
    };
    let mut baseline = seed(); let mut gates = 0;
    baseline.find_with_kind(2, "needle permit -blocked", ObservedReaderEncoding::Utf8,
        ObservedReaderSearchKind::Expression, true, false, 8, bytes.len() as u64,
        || { gates += 1; false }).unwrap();
    assert!(gates > 16);
    for stop in [1, gates / 2, gates] {
        let mut reader = seed();
        let selected = json(&reader.search_hit(1, 0, || false).unwrap());
        let before = json(&reader.info(|| false).unwrap());
        let mut polls = 0;
        let error = reader.find_with_kind(2, "needle permit -blocked", ObservedReaderEncoding::Utf8,
            ObservedReaderSearchKind::Expression, true, false, 8, bytes.len() as u64,
            || { polls += 1; polls == stop }).err().unwrap();
        assert!(error.is_canceled()); assert!(polls >= stop);
        let after = json(&reader.info(|| false).unwrap());
        let spent = after.get("payload_bytes_read").number() - before.get("payload_bytes_read").number();
        let calls = after.get("read_calls").number() - before.get("read_calls").number();
        if stop == 1 { assert_eq!((spent, calls), (0, 0)); }
        else if stop == gates { assert_eq!(spent, bytes.len() as u64); assert!(calls > 0); }
        else { assert!(spent > 0 && spent < bytes.len() as u64); assert!(calls > 0); }
        assert_eq!(json(&reader.search_hit(1, 0, || false).unwrap()), selected);
        assert_eq!(reader.search_hit(2, 0, || false).err(), Some(ObservedReaderError::StaleQuery));
        assert_eq!(expression(&mut reader, 2, "needle", true, 8, 1024).err(), Some(ObservedReaderError::StaleQuery));
    }
}

#![forbid(unsafe_code)]

//! Public retained-reader expression searches. Expected ranges and witnesses
//! come from independently encoded fixtures, not the production matcher.
mod support;

use std::path::Path;
use fcb::ArenaOwnerId;
use fcb::search::QueryError;
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::{HostResponse, reader::{
    ReaderFindOptions, ReaderSession, ReaderSessionError, MAX_READER_EXPRESSION_BYTES}}};
use support::{Json, parse};

const COUNT: ReaderFindOptions = ReaderFindOptions {
    case_sensitive: true, count_all_matches: true,
};
const LOWER: ReaderFindOptions = ReaderFindOptions {
    case_sensitive: false, count_all_matches: true,
};
fn source(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(9971).unwrap(),
        Path::new("retained-expression.txt"), bytes, || false).unwrap()
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
fn metadata(value: &Json, raw: &str, primary: &str) {
    assert_eq!(value.get("mode").text(), "decoded-text-expression");
    assert_eq!(value.get("requested_mode").text(), "expression");
    assert_eq!(value.get("realized_mode").text(), "decoded-text-expression");
    assert_eq!(value.get("needle").text(), raw);
    assert_eq!(value.get("expression").text(), raw);
    assert_eq!(value.get("primary_needle").text(), primary);
    assert_eq!(value.get("query_syntax").text(), "fcb-bounded-expression/1");
    assert_eq!(value.get("predicate_scope").text(), "retained-source-capture");
    assert_eq!(value.get("additional_source_bytes_read").number(), 0);
}

#[test]
fn qualified_expression_pages_expose_primary_occurrences_and_immutable_exact_copies() {
    for encoding in 0..3 {
        let bytes = encoded("\u{feff}bananana permit\r\n", encoding);
        let mut reader = source(&bytes);
        let capture = json(&reader.info(|| false).unwrap());
        let raw = "\"ana\" permit -blocked";
        for start_after in 0..3u64 {
            let generation = start_after + 1;
            let response = reader.search_expression_page(generation, raw, start_after, 1,
                bytes.len() as u64, ReaderFindOptions::default(), || false).unwrap();
            assert_eq!(response.exit_code(), EXIT_OK, "complete count and omitted rows are separate");
            let found = json(&response);
            metadata(&found, raw, "ana");
            assert!(found.get("expression_qualified").flag());
            assert!(found.get("search_complete").flag());
            assert!(found.get("match_count_complete").flag());
            assert!(!found.get("count_all_matches").flag(), "echo requested policy");
            assert!(found.get("rows_truncated").flag());
            assert_eq!(found.get("matches_seen").number(), 3);
            assert_eq!(found.get("retained_hits").number(), 1);
            assert_eq!(found.get("page_start_after").number(), start_after);
            assert_eq!(found.get("skipped_matches").number(), start_after);
            assert_eq!(found.get("has_more_matches").flag(), start_after < 2);
            let row = &found.get("hits").array()[0];
            let prefix = format!("\u{feff}b{}", "an".repeat(start_after as usize));
            let start = encoded(&prefix, encoding).len() as u64;
            assert_eq!(row.get("occurrence_id").number(), generation);
            assert_eq!(row.get("original_range").get("start").number(), start);
            assert_eq!(row.get("original_range").get("end").number(),
                start + encoded("ana", encoding).len() as u64);
            let witness = hex(&encoded("ana", encoding));
            assert_eq!(found.get("literal_original_hex").text(), witness);
            assert_eq!(json(&reader.copy_hit(generation, 0, || false).unwrap())
                .get("original_hex").text(), witness);
            let page = json(&reader.hit_window(generation, 0, 64, || false).unwrap());
            assert_eq!(page.get("selection").get("original_hex").text(), witness);
            for field in ["owner", "file_id", "source_revision", "captured_bytes", "encoding"] {
                assert_eq!(page.get(field), capture.get(field));
            }
            assert_eq!(reader.capture_sha256().to_hex(), capture.get("capture_sha256").text());
            assert_eq!(page.get("additional_source_bytes_read").number(), 0);
            if generation > 1 {
                assert_eq!(reader.hit_range(generation - 1, 0).err(),
                    Some(ReaderSessionError::StaleQuery));
            }
        }
        let end = json(&reader.search_expression_page(4, raw, 3, 1,
            bytes.len() as u64, COUNT, || false).unwrap());
        assert!(end.get("expression_qualified").flag());
        assert!(end.get("search_complete").flag());
        assert_eq!(end.get("matches_seen").number(), 3);
        assert!(!end.get("has_more_matches").flag());
        assert!(end.get("hits").array().is_empty());
        assert_eq!(reader.capture().bytes(), bytes);
    }
}

#[test]
fn late_predicates_and_incomplete_scans_never_publish_tentative_primary_hits() {
    let mut bytes = b"banana permit ".to_vec();
    bytes.resize(33 * 1024, b' '); bytes.extend_from_slice(b"blocked");
    let mut reader = source(&bytes);
    for (index, raw) in ["ana permit -blocked", "ana missing", "missing permit"].into_iter().enumerate() {
        let response = reader.search_expression_page(index as u64 + 1, raw, 0, 1,
            bytes.len() as u64, ReaderFindOptions::default(), || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_OK);
        let found = json(&response);
        assert_eq!(found.get("expression_qualified"), &Json::Bool(false));
        assert!(found.get("search_complete").flag());
        assert_eq!(found.get("matches_seen").number(), 0);
        assert_eq!(found.get("retained_hits").number(), 0);
        assert_eq!(found.get("literal_original_hex"), &Json::Null);
        assert!(!found.get("has_more_matches").flag());
    }
    let response = reader.search_expression_page(4, "ana permit -blocked", 1, 1,
        16, COUNT, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let partial = json(&response);
    assert_eq!(partial.get("state").text(), "byte-limit");
    assert_eq!(partial.get("expression_qualified"), &Json::Null);
    assert!(!partial.get("search_complete").flag());
    assert!(!partial.get("match_count_complete").flag());
    assert_eq!(partial.get("matches_seen").number(), 0);
    assert_eq!(partial.get("skipped_matches").number(), 0);
    assert!(!partial.get("has_more_matches").flag());
    assert!(partial.get("hits").array().is_empty());
    assert!(partial.get("scanned_bytes").number() <= 16);
    assert_eq!(reader.hit_range(4, 0).err(), Some(ReaderSessionError::MissingHit));

    let mut malformed = source(b"banana permit\xff");
    let bad = json(&malformed.search_expression_page(1, "ana permit", 0, 1,
        1024, COUNT, || false).unwrap());
    assert_eq!(bad.get("state").text(), "unsupported-text");
    assert_eq!(bad.get("expression_qualified"), &Json::Null);
    assert!(!bad.get("search_complete").flag());
    assert_eq!(bad.get("matches_seen").number(), 0);
    assert!(bad.get("hits").array().is_empty());
}

#[test]
fn lowercase_expressions_keep_original_spellings_expansions_and_predicate_case_policy() {
    for encoding in 0..3 {
        let bytes = encoded("\u{feff}K K k İ PERMIT", encoding);
        let mut reader = source(&bytes);
        for (start_after, original) in ["K", "K", "k"].into_iter().enumerate() {
            let generation = start_after as u64 + 1;
            let found = json(&reader.search_expression_page(generation, "k permit -blocked",
                start_after as u64, 1, bytes.len() as u64, LOWER, || false).unwrap());
            assert!(found.get("expression_qualified").flag());
            assert_eq!(found.get("matches_seen").number(), 3);
            assert_eq!(found.get("literal_original_hex"), &Json::Null);
            let row = &found.get("hits").array()[0];
            assert_eq!(row.get("matched_text").text(), original);
            assert_eq!(row.get("original_hex").text(), hex(&encoded(original, encoding)));
            assert_eq!(json(&reader.copy_hit(generation, 0, || false).unwrap())
                .get("original_hex").text(), hex(&encoded(original, encoding)));
        }
        let expansion = json(&reader.search_expression_page(4, "\u{307} permit",
            0, 1, bytes.len() as u64, LOWER, || false).unwrap());
        let row = &expansion.get("hits").array()[0];
        assert_eq!(row.get("matched_text").text(), "İ");
        assert_eq!(row.get("original_hex").text(), hex(&encoded("İ", encoding)));
        let rejected = json(&reader.search_expression_page(5, "k permit",
            0, 1, bytes.len() as u64, COUNT, || false).unwrap());
        assert_eq!(rejected.get("expression_qualified"), &Json::Bool(false));
        let excluded = json(&reader.search_expression_page(6, "k -permit",
            0, 1, bytes.len() as u64, LOWER, || false).unwrap());
        assert_eq!(excluded.get("expression_qualified"), &Json::Bool(false));
    }
}

#[test]
fn expression_errors_preserve_accepted_literal_page_and_consume_attempted_generation() {
    let mut reader = source(b"banana permit");
    reader.search_page(1, "ana", 0, 1, 32, COUNT, || false).unwrap();
    let accepted = reader.hit_range(1, 0).unwrap();
    let too_long = "x".repeat(MAX_READER_EXPRESSION_BYTES + 1);
    let too_many = "a ".repeat(65);
    let cases = [
        ("", ReaderSessionError::Query(QueryError::EmptyNeedle)),
        ("\"ana", ReaderSessionError::Query(QueryError::SyntaxError)),
        ("regex:a.*", ReaderSessionError::Query(QueryError::RegexUnqualified)),
        (too_long.as_str(), ReaderSessionError::Query(QueryError::NeedleTooLong)),
        (too_many.as_str(), ReaderSessionError::Query(QueryError::LimitExceeded)),
        ("ana path:src/", ReaderSessionError::ExpressionScopeUnsupported),
        ("ana -path:generated/", ReaderSessionError::ExpressionScopeUnsupported),
        ("ana lang:rust", ReaderSessionError::ExpressionScopeUnsupported),
        ("ana type:txt", ReaderSessionError::ExpressionScopeUnsupported),
    ];
    for (index, (raw, expected)) in cases.into_iter().enumerate() {
        let generation = index as u64 + 2;
        assert_eq!(reader.search_expression_page(generation, raw, 0, 1, 32, COUNT,
            || false).err(), Some(expected));
        assert_eq!(reader.accepted_generation(), Some(1));
        assert_eq!(reader.hit_range(1, 0).unwrap(), accepted);
        assert_eq!(reader.search_expression_page(generation, "ana permit", 0, 1, 32,
            COUNT, || false).err(), Some(ReaderSessionError::StaleQuery));
    }
    assert_eq!(json(&reader.copy_hit(1, 0, || false).unwrap()).get("original_hex").text(), "616e61");
}

#[test]
fn literal_syntax_is_never_reinterpreted_and_quoted_operators_remain_literal_expression_terms() {
    let mut reader = source(b"path:src/ -blocked permit");
    let raw = "path:src/ -blocked";
    let literal = json(&reader.search_page(1, raw, 0, 4, 128, COUNT, || false).unwrap());
    assert_eq!(literal.get("mode").text(), "exact-decoded-literal");
    assert_eq!(literal.get("matches_seen").number(), 1);
    assert_eq!(literal.get("needle").text(), raw);
    let expression = "\"path:src/\" \"-blocked\" permit";
    let found = json(&reader.search_expression_page(2, expression, 0, 4, 128,
        COUNT, || false).unwrap());
    metadata(&found, expression, "path:src/");
    assert!(found.get("expression_qualified").flag());
    assert_eq!(found.get("matches_seen").number(), 1);
}

#[test]
fn zero_row_counting_and_empty_capture_have_explicit_qualification() {
    let mut reader = source(b"banana permit");
    let counted = json(&reader.search_expression_page(1, "ana permit", 1, 0, 64,
        COUNT, || false).unwrap());
    assert!(counted.get("expression_qualified").flag());
    assert_eq!(counted.get("matches_seen").number(), 2);
    assert_eq!(counted.get("skipped_matches").number(), 1);
    assert!(counted.get("has_more_matches").flag());
    assert!(counted.get("hits").array().is_empty());
    assert_eq!(reader.search_expression_page(2, "ana permit", 0, 0, 64,
        ReaderFindOptions::default(), || false).err(), Some(ReaderSessionError::InvalidLimits));
    assert_eq!(reader.search_expression_page(2, "ana permit", u64::MAX, 1, 64,
        COUNT, || false).err(), Some(ReaderSessionError::InvalidLimits));
    let mut empty = source(b"");
    let found = json(&empty.search_expression_page(1, "ana -blocked", 0, 1, 0,
        COUNT, || false).unwrap());
    assert!(found.get("search_complete").flag());
    assert_eq!(found.get("expression_qualified"), &Json::Bool(false));
    assert_eq!(found.get("matches_seen").number(), 0);
}

#[test]
fn cancellation_during_scan_and_at_final_publication_preserves_previous_hits() {
    let mut bytes = b"banana permit ".to_vec();
    bytes.resize(128 * 1024, b' ');
    let mut oracle = source(&bytes);
    oracle.search_page(1, "ana", 0, 1, bytes.len() as u64, COUNT, || false).unwrap();
    let mut complete_polls = 0;
    oracle.search_expression_page(2, "ana permit -blocked", 0, 1, bytes.len() as u64,
        COUNT, || { complete_polls += 1; false }).unwrap();
    assert!(complete_polls > 8);
    for cancel_at in [8, complete_polls] {
        let mut reader = source(&bytes);
        reader.search_page(1, "ana", 0, 1, bytes.len() as u64, COUNT, || false).unwrap();
        let original = reader.hit_range(1, 0).unwrap();
        let mut polls = 0;
        let canceled = reader.search_expression_page(2, "ana permit -blocked", 0, 1,
            bytes.len() as u64, COUNT, || { polls += 1; polls >= cancel_at });
        assert_eq!(canceled.err(), Some(ReaderSessionError::Canceled));
        assert_eq!(reader.accepted_generation(), Some(1));
        assert_eq!(reader.hit_range(1, 0).unwrap(), original);
        assert_eq!(reader.search_expression_page(2, "ana permit", 0, 1, bytes.len() as u64,
            COUNT, || false).err(), Some(ReaderSessionError::StaleQuery));
    }
}

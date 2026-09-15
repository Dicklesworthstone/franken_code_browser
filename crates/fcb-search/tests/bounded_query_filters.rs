#![forbid(unsafe_code)]

//! Integration tests for bounded query filters and reference scan oracle (FCB-026.B / fcb-8ii.2).
//!
//! Verifies:
//! - Declared escape/phrase/conjunction/exclusion/language/path semantics.
//! - Conjunction and exclusion filters determine document eligibility WITHOUT multiplying anchor occurrences.
//! - Invalid query errors: unclosed quotes (`QUERY_SYNTAX_ERROR`), empty query (`QUERY_EMPTY`), query too long (`QUERY_NEEDLE_TOO_LONG`).
//! - Rejection of unqualified regex queries (`QUERY_REGEX_UNQUALIFIED`).
//! - Reference scan oracle collection scanning and oracle verification (`verify_oracle_match`).
//! - Negative controls demonstrating oracle detection of anchor multiplication, filter leakage, and regex bypass.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, SourceRevision};
use fcb_search::{
    oracle::{ReferenceScanOracle, SearchDocument},
    query::{LangFilterKind, ParsedQuery, PathFilterKind, MAX_QUERY_LEN},
    QueryError, QueryOptions,
};
use fcb_source::{CaptureRequest, CompleteCapture};

fn setup_identities(file_num: u64) -> (FileId, SourceRevision, QueryGeneration) {
    let owner = ArenaOwnerId::new(1).unwrap();
    let file = FileId::new(owner, file_num).unwrap();
    let rev = SourceRevision::new(owner, 100).unwrap();
    let generation = QueryGeneration::new(owner, 1).unwrap();
    (file, rev, generation)
}

fn make_capture(file: FileId, rev: SourceRevision, bytes: &[u8]) -> CompleteCapture {
    let req = CaptureRequest::new(file, rev).unwrap();
    let len = ByteLength::new(bytes.len() as u64);
    CompleteCapture::new(req, len, Arc::from(bytes)).unwrap()
}

#[test]
fn query_parser_phrase_and_field_filters() {
    let query_str = r#""fn main" path:src/ -path:vendor/ lang:rust -test"#;
    let parsed = ParsedQuery::parse(query_str).expect("parse valid query");

    assert_eq!(parsed.primary_needle, "fn main");
    assert!(parsed.is_phrase);
    assert_eq!(
        parsed.path_filters,
        vec![
            PathFilterKind::Include("src/".to_string()),
            PathFilterKind::Exclude("vendor/".to_string()),
        ]
    );
    assert_eq!(
        parsed.lang_filters,
        vec![LangFilterKind::Include("rust".to_string())]
    );
    assert_eq!(parsed.exclusion_terms, vec!["test".to_string()]);
}

#[test]
fn query_parser_escaped_quotes_in_phrase() {
    let query_str = r#""escaped \" quote and \\ slash""#;
    let parsed = ParsedQuery::parse(query_str).expect("parse escaped phrase");

    assert_eq!(parsed.primary_needle, r#"escaped " quote and \ slash"#);
    assert!(parsed.is_phrase);
}

#[test]
fn query_parser_syntax_error_on_unclosed_quote() {
    let query_str = r#""unclosed quote without end"#;
    let err = ParsedQuery::parse(query_str).expect_err("should fail on unclosed quote");

    assert_eq!(err, QueryError::SyntaxError);
    assert_eq!(err.code(), "QUERY_SYNTAX_ERROR");
}

#[test]
fn query_parser_rejects_unqualified_regex() {
    let query_re1 = "re:.*foo.*";
    let err1 = ParsedQuery::parse(query_re1).expect_err("regex re: should fail");
    assert_eq!(err1, QueryError::RegexUnqualified);
    assert_eq!(err1.code(), "QUERY_REGEX_UNQUALIFIED");

    let query_re2 = "regex:^[a-z]+";
    let err2 = ParsedQuery::parse(query_re2).expect_err("regex regex: should fail");
    assert_eq!(err2, QueryError::RegexUnqualified);
    assert_eq!(err2.code(), "QUERY_REGEX_UNQUALIFIED");

    let query_re3 = "target re:[0-9]+";
    let err3 = ParsedQuery::parse(query_re3).expect_err("token re: should fail");
    assert_eq!(err3, QueryError::RegexUnqualified);
    assert_eq!(err3.code(), "QUERY_REGEX_UNQUALIFIED");
}

#[test]
fn query_parser_rejects_empty_and_oversized_queries() {
    let err_empty = ParsedQuery::parse("   ").expect_err("empty query");
    assert_eq!(err_empty, QueryError::EmptyNeedle);
    assert_eq!(err_empty.code(), "QUERY_EMPTY");

    let huge_query = "a".repeat(MAX_QUERY_LEN + 1);
    let err_huge = ParsedQuery::parse(&huge_query).expect_err("huge query");
    assert_eq!(err_huge, QueryError::NeedleTooLong);
    assert_eq!(err_huge.code(), "QUERY_NEEDLE_TOO_LONG");
}

#[test]
fn conjunction_and_exclusion_filters_do_not_multiply_anchor_occurrences() {
    let (file, rev, generation) = setup_identities(1);
    let options = QueryOptions::new(generation);

    // Document text contains "apple" twice and "banana" twice
    // "apple banana banana apple"
    let text = "apple banana banana apple";
    let capture = make_capture(file, rev, text.as_bytes());
    let doc = SearchDocument::new(file, "src/fruit.rs", &capture);

    // Query is "banana apple" -> primary needle is "banana", conjunction is "apple"
    let parsed = ParsedQuery::parse("banana apple").unwrap();
    assert_eq!(parsed.primary_needle, "banana");
    assert_eq!(parsed.conjunction_terms, vec!["apple".to_string()]);

    let res = ReferenceScanOracle::scan_document(&doc, &parsed, &options)
        .expect("scan document with conjunction");

    // INVARIANT: Conjunction filter verifies that "apple" exists, but ONLY returns occurrences of "banana"
    // There are 2 bananas, NOT 4 combined matches!
    assert_eq!(res.match_count(), 2);
    assert_eq!(res.matches[0].matched_text, "banana");
    assert_eq!(res.matches[0].original_byte_range.start().get(), 6);
    assert_eq!(res.matches[0].original_byte_range.end().get(), 12);
    assert_eq!(res.matches[1].matched_text, "banana");
    assert_eq!(res.matches[1].original_byte_range.start().get(), 13);
    assert_eq!(res.matches[1].original_byte_range.end().get(), 19);
}

#[test]
fn exclusion_filters_reject_matching_documents() {
    let (file, rev, generation) = setup_identities(2);
    let options = QueryOptions::new(generation);

    // Document contains "banana" and "poison"
    let text = "banana with poison";
    let capture = make_capture(file, rev, text.as_bytes());
    let doc = SearchDocument::new(file, "src/snack.rs", &capture);

    // Query: "banana -poison"
    let parsed = ParsedQuery::parse("banana -poison").unwrap();
    let res = ReferenceScanOracle::scan_document(&doc, &parsed, &options)
        .expect("scan document with exclusion");

    // Document contains "poison", so it is excluded: 0 matches!
    assert_eq!(res.match_count(), 0);
    assert!(res.is_empty());
}

#[test]
fn path_and_language_filters_constrain_document_selection() {
    let (file1, rev1, generation) = setup_identities(10);
    let (file2, rev2, _) = setup_identities(20);
    let (file3, rev3, _) = setup_identities(30);
    let options = QueryOptions::new(generation);

    let text = "struct BrowserCore;";
    let cap1 = make_capture(file1, rev1, text.as_bytes());
    let cap2 = make_capture(file2, rev2, text.as_bytes());
    let cap3 = make_capture(file3, rev3, text.as_bytes());

    let doc_src_rust = SearchDocument::new(file1, "crates/fcb-core/src/lib.rs", &cap1);
    let doc_vendor_rust = SearchDocument::new(file2, "vendor/other/lib.rs", &cap2);
    let doc_src_python = SearchDocument::new(file3, "crates/fcb-core/src/tool.py", &cap3);

    let docs = vec![doc_src_rust, doc_vendor_rust, doc_src_python];

    // Query: "BrowserCore path:crates/ lang:rust -path:vendor/"
    let parsed = ParsedQuery::parse("BrowserCore path:crates/ lang:rust -path:vendor/").unwrap();

    let res = ReferenceScanOracle::scan_collection(&docs, &parsed, &options)
        .expect("scan collection with path and lang filters");

    // Only doc_src_rust satisfies path:crates/, lang:rust, and -path:vendor/
    assert_eq!(res.match_count(), 1);
    assert_eq!(res.matches[0].file_id, file1);
    assert_eq!(res.matches[0].matched_text, "BrowserCore");
}

#[test]
fn reference_oracle_verification_matches_identical_results_and_detects_mismatches() {
    let (file, rev, generation) = setup_identities(100);
    let options = QueryOptions::new(generation);

    let text = "alpha beta alpha";
    let cap = make_capture(file, rev, text.as_bytes());
    let doc = SearchDocument::new(file, "test.txt", &cap);
    let parsed = ParsedQuery::parse("alpha").unwrap();

    let oracle_res = ReferenceScanOracle::scan_document(&doc, &parsed, &options).unwrap();
    assert_eq!(oracle_res.match_count(), 2);

    // Candidate identical to oracle: verify succeeds
    let candidate_ok = oracle_res.clone();
    assert!(ReferenceScanOracle::verify_oracle_match(&candidate_ok, &oracle_res).is_ok());

    // Candidate with missing match: verify detects defect
    let mut candidate_bad = oracle_res.clone();
    candidate_bad.matches.pop();
    candidate_bad.total_matches_counted = 1;
    let mismatch_err = ReferenceScanOracle::verify_oracle_match(&candidate_bad, &oracle_res)
        .expect_err("should detect mismatch in match count");
    assert!(mismatch_err.reason.contains("Match count mismatch"));
}

#[test]
fn negative_control_oracle_detects_anchor_multiplication_defect() {
    let (file, rev, generation) = setup_identities(200);
    let options = QueryOptions::new(generation);

    let text = "foo bar foo";
    let cap = make_capture(file, rev, text.as_bytes());
    let doc = SearchDocument::new(file, "test.rs", &cap);
    let parsed = ParsedQuery::parse("foo bar").unwrap();

    let oracle_res = ReferenceScanOracle::scan_document(&doc, &parsed, &options).unwrap();
    // In oracle truth, only "foo" occurrences are returned (2 hits: offsets 0..3 and 8..11)
    assert_eq!(oracle_res.match_count(), 2);

    // Simulate defective search engine that multiplied anchor occurrences by including "bar"
    let mut defective_res = oracle_res.clone();
    let defective_bar_hit = fcb_search::SearchMatch {
        occurrence_id: 3,
        file_id: file,
        revision: rev,
        decoded_range: None,
        original_byte_range: fcb_core::ByteRange::new(
            fcb_core::ByteOffset::new(4),
            fcb_core::ByteOffset::new(7),
        )
        .unwrap(),
        matched_text: "bar".to_string(),
        multiplicity: 1,
    };
    defective_res.matches.push(defective_bar_hit);
    defective_res.total_matches_counted = 3;

    // DEFECT PROOF: Reference oracle detects the illegal occurrence multiplication!
    let err = ReferenceScanOracle::verify_oracle_match(&defective_res, &oracle_res)
        .expect_err("oracle must detect anchor multiplication defect");
    assert!(err.reason.contains("Match count mismatch"));
}

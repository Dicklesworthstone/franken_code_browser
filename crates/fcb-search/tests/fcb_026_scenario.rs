#![forbid(unsafe_code)]

//! FCB-026.V production verification scenario campaign (FCB-026.V / fcb-8ii.3).
//!
//! Exercises:
//! - Exact decoded-text and byte search engine (FCB-026.A).
//! - Bounded query filters and reference scan oracle (FCB-026.B).
//! - All required routes executed through the scenario driver with bounded redacted receipts.
//! - Named wrong-match negative control detecting deliberate oracle corruption.

use std::sync::Arc;

use fcb_core::{
    ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, QueryGeneration, SourceRevision,
};
use fcb_search::{
    oracle::{ReferenceScanOracle, SearchDocument},
    query::ParsedQuery,
    DirectSourceScanner, QueryOptions, SearchMode, UnicodeNormalization,
};
use fcb_source::{
    CaptureRequest, ChunkSize, ChunkedCapture, CompleteCapture, SourceChunk,
};
use fcb_test_support::receipts::Redactor;
use fcb_test_support::scenario::{
    validate_results, BodyResult, RequiredOutcome, RequiredScenario, ScenarioContext,
    ScenarioDriver, ScenarioRoute, ScenarioSpec, ScenarioVerdict,
};

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

// ---------------------------------------------------------------------------
// Scenario 1: Exact decoded text and UTF-16 BOM verification
// ---------------------------------------------------------------------------
fn scenario_exact_decoded_text_and_utf16_bom_verification(context: &ScenarioContext) -> BodyResult {
    let (file, rev, generation) = setup_identities(101);
    let options = QueryOptions::new(generation);

    // UTF-16LE with BOM: "hello world"
    let mut raw = vec![0xFF, 0xFE];
    for c in "hello world".chars() {
        let mut buf = [0u16; 2];
        let enc = c.encode_utf16(&mut buf);
        for &u in &*enc {
            raw.extend_from_slice(&u.to_le_bytes());
        }
    }

    let capture = make_capture(file, rev, &raw);
    let res = match DirectSourceScanner::scan_complete_capture(&capture, "world", &options) {
        Ok(res) => res,
        Err(err) => {
            return BodyResult::Fail {
                reason: format!("UTF-16LE search failed: {err}"),
            }
        }
    };

    if res.match_count() != 1 {
        return BodyResult::Fail {
            reason: format!("expected 1 match, got {}", res.match_count()),
        };
    }

    let m = &res.matches[0];
    if m.matched_text != "world" || m.original_byte_range.start().get() != 14 {
        return BodyResult::Fail {
            reason: format!("mismatched match data: {:?}", m),
        };
    }

    let _ = context.write_fixture("utf16_world.bin", &raw);
    BodyResult::Pass
}

// ---------------------------------------------------------------------------
// Scenario 2: Cross-chunk boundary and overlapping literal scans
// ---------------------------------------------------------------------------
fn scenario_cross_chunk_and_overlapping_literal_scans(context: &ScenarioContext) -> BodyResult {
    let (file, rev, generation) = setup_identities(102);

    // 1. Cross-chunk boundary test
    let mut data = Vec::new();
    data.extend_from_slice(b"0123456789ABCDBO"); // 16 bytes, ends with "BO"
    data.extend_from_slice(b"UNDARY_NEEDLE123"); // 16 bytes, begins with "UNDARY_NEEDLE"
    data.extend_from_slice(b"extra tail chunk"); // 16 bytes

    let chunk_size = ChunkSize::bounded(16).unwrap();
    let c0 = SourceChunk::new(
        0,
        ByteRange::new(ByteOffset::new(0), ByteOffset::new(16)).unwrap(),
        Arc::from(&data[0..16]),
    )
    .unwrap();
    let c1 = SourceChunk::new(
        1,
        ByteRange::new(ByteOffset::new(16), ByteOffset::new(32)).unwrap(),
        Arc::from(&data[16..32]),
    )
    .unwrap();
    let c2 = SourceChunk::new(
        2,
        ByteRange::new(ByteOffset::new(32), ByteOffset::new(48)).unwrap(),
        Arc::from(&data[32..48]),
    )
    .unwrap();

    let req = CaptureRequest::new(file, rev).unwrap();
    let capture = ChunkedCapture::new(
        req,
        ByteLength::new(48),
        chunk_size,
        vec![c0, c1, c2],
    )
    .unwrap();

    let raw_options = QueryOptions::new(generation).with_mode(SearchMode::RawBytes);
    let chunk_res = match DirectSourceScanner::scan_chunked_capture(&capture, "BOUNDARY_NEEDLE", &raw_options) {
        Ok(res) => res,
        Err(err) => {
            return BodyResult::Fail {
                reason: format!("cross-chunk search failed: {err}"),
            }
        }
    };

    if chunk_res.match_count() != 1 {
        return BodyResult::Fail {
            reason: format!("expected 1 boundary match, got {}", chunk_res.match_count()),
        };
    }

    // 2. Overlapping substring test: "ana" in "banana"
    let banana_cap = make_capture(file, rev, b"banana");
    let text_options = QueryOptions::new(generation);
    let banana_res = match DirectSourceScanner::scan_complete_capture(&banana_cap, "ana", &text_options) {
        Ok(res) => res,
        Err(err) => {
            return BodyResult::Fail {
                reason: format!("overlapping search failed: {err}"),
            }
        }
    };

    if banana_res.match_count() != 2 {
        return BodyResult::Fail {
            reason: format!("expected 2 overlapping matches, got {}", banana_res.match_count()),
        };
    }

    let _ = context.write_fixture("cross_chunk.bin", &data);
    BodyResult::Pass
}

// ---------------------------------------------------------------------------
// Scenario 3: Unicode normalization and expansion multiplicity
// ---------------------------------------------------------------------------
fn scenario_unicode_normalization_and_expansion_multiplicity(context: &ScenarioContext) -> BodyResult {
    let (file, rev, generation) = setup_identities(103);

    // 1. Canonical equivalence: precomposed "café" query vs decomposed "cafe\u{0301}" source
    let canonical_options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
        case_sensitive: true,
        normalization: UnicodeNormalization::Canonical,
    });
    let decomp_cap = make_capture(file, rev, "cafe\u{0301}".as_bytes());
    let canon_res = match DirectSourceScanner::scan_complete_capture(&decomp_cap, "café", &canonical_options) {
        Ok(res) => res,
        Err(err) => {
            return BodyResult::Fail {
                reason: format!("canonical search failed: {err}"),
            }
        }
    };
    if canon_res.match_count() != 1 || canon_res.matches[0].original_byte_range.end().get() != 6 {
        return BodyResult::Fail {
            reason: format!("canonical match error: {:?}", canon_res),
        };
    }

    // 2. German 'ß' expansion to 'ss' (multiplicity 1) and 's' (multiplicity 2)
    let fold_options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
        case_sensitive: false,
        normalization: UnicodeNormalization::CaseFold,
    });
    let strasse_cap = make_capture(file, rev, "Straße".as_bytes());
    let ss_res = DirectSourceScanner::scan_complete_capture(&strasse_cap, "ss", &fold_options).unwrap();
    if ss_res.match_count() != 1 || ss_res.matches[0].multiplicity != 1 {
        return BodyResult::Fail {
            reason: format!("Straße 'ss' match error: {:?}", ss_res),
        };
    }

    let s_res = DirectSourceScanner::scan_complete_capture(&strasse_cap, "s", &fold_options).unwrap();
    if s_res.match_count() != 2 || s_res.matches[1].multiplicity != 2 {
        return BodyResult::Fail {
            reason: format!("Straße 's' multiplicity error: {:?}", s_res),
        };
    }

    let _ = context.write_fixture("normalization.txt", "Straße cafe\u{0301}".as_bytes());
    BodyResult::Pass
}

// ---------------------------------------------------------------------------
// Scenario 4: Query parser filters and conjunction eligibility
// ---------------------------------------------------------------------------
fn scenario_query_parser_filters_and_conjunction_eligibility(context: &ScenarioContext) -> BodyResult {
    let (file1, rev1, generation) = setup_identities(104);
    let (file2, rev2, _) = setup_identities(105);
    let options = QueryOptions::new(generation);

    // doc 1: matching path, satisfies conjunction "apple", contains 2 "banana"s
    let text1 = "apple banana banana apple";
    let cap1 = make_capture(file1, rev1, text1.as_bytes());
    let doc1 = SearchDocument::new(file1, "src/fruit.rs", &cap1);

    // doc 2: contains "banana" and "poison" (excluded)
    let text2 = "banana with poison";
    let cap2 = make_capture(file2, rev2, text2.as_bytes());
    let doc2 = SearchDocument::new(file2, "src/poison.rs", &cap2);

    let docs = vec![doc1, doc2];

    // Query: "banana apple -poison path:src/ lang:rust"
    let parsed = match ParsedQuery::parse("banana apple -poison path:src/ lang:rust") {
        Ok(q) => q,
        Err(err) => {
            return BodyResult::Fail {
                reason: format!("query parse failed: {err}"),
            }
        }
    };

    let coll_res = match ReferenceScanOracle::scan_collection(&docs, &parsed, &options) {
        Ok(res) => res,
        Err(err) => {
            return BodyResult::Fail {
                reason: format!("collection scan failed: {err}"),
            }
        }
    };

    // INVARIANT: Exactly 2 matches from doc1 (both for "banana"). doc2 is excluded by "-poison".
    // Anchor occurrences are not multiplied!
    if coll_res.match_count() != 2 {
        return BodyResult::Fail {
            reason: format!("expected 2 matches, got {}", coll_res.match_count()),
        };
    }

    let _ = context.write_fixture("query_docs.txt", text1.as_bytes());
    BodyResult::Pass
}

// ---------------------------------------------------------------------------
// Scenario 5: Honest coverage for unsupported encoding
// ---------------------------------------------------------------------------
fn scenario_honest_coverage_unsupported_encoding(context: &ScenarioContext) -> BodyResult {
    let (file, rev, generation) = setup_identities(106);
    let options = QueryOptions::new(generation);

    let binary_data = vec![0x80, 0x81, 0xFF, 0x00, 0xFE, 0x01];
    let capture = make_capture(file, rev, &binary_data);

    let res = match DirectSourceScanner::scan_complete_capture(&capture, "needle", &options) {
        Ok(res) => res,
        Err(err) => {
            return BodyResult::Fail {
                reason: format!("scan unexpected error: {err}"),
            }
        }
    };

    if res.match_count() != 0 || res.unsupported_files != vec![file] {
        return BodyResult::Fail {
            reason: format!("honest coverage violated: {:?}", res),
        };
    }

    let _ = context.write_fixture("binary.bin", &binary_data);
    BodyResult::Pass
}

// ---------------------------------------------------------------------------
// Scenario 6: Negative control detecting deliberate oracle corruption
// ---------------------------------------------------------------------------
fn scenario_negative_control_wrong_match_oracle_corruption(_context: &ScenarioContext) -> BodyResult {
    let (file, rev, generation) = setup_identities(107);
    let options = QueryOptions::new(generation);

    let text = "target content";
    let cap = make_capture(file, rev, text.as_bytes());
    let doc = SearchDocument::new(file, "src/lib.rs", &cap);
    let parsed = ParsedQuery::parse("target").unwrap();

    let oracle_res = ReferenceScanOracle::scan_document(&doc, &parsed, &options).unwrap();
    assert_eq!(oracle_res.match_count(), 1);

    // Deliberately corrupt candidate result by altering the matched byte range
    let mut corrupted = oracle_res.clone();
    corrupted.matches[0].original_byte_range = ByteRange::new(ByteOffset::new(99), ByteOffset::new(105)).unwrap();

    // Verify against oracle: MUST detect corruption!
    match ReferenceScanOracle::verify_oracle_match(&corrupted, &oracle_res) {
        Ok(()) => BodyResult::Fail {
            reason: "oracle failed to detect deliberate match range corruption".to_string(),
        },
        Err(err) => BodyResult::ExpectedFailure {
            reason: format!("correctly detected match corruption: {}", err.reason),
        },
    }
}

fn spec(
    name: &'static str,
    negative_control: bool,
    body: fn(&ScenarioContext) -> BodyResult,
) -> ScenarioSpec {
    ScenarioSpec {
        name,
        route: ScenarioRoute::Headless,
        timeout: std::time::Duration::from_secs(30),
        negative_control,
        seed: 26,
        body,
    }
}

#[test]
fn fcb_026_production_verification_campaign() {
    let mut driver = ScenarioDriver::new(6, false).expect("create scenario driver");

    driver
        .submit(spec(
            "scenario_exact_decoded_text_and_utf16_bom_verification",
            false,
            scenario_exact_decoded_text_and_utf16_bom_verification,
        ))
        .unwrap();

    driver
        .submit(spec(
            "scenario_cross_chunk_and_overlapping_literal_scans",
            false,
            scenario_cross_chunk_and_overlapping_literal_scans,
        ))
        .unwrap();

    driver
        .submit(spec(
            "scenario_unicode_normalization_and_expansion_multiplicity",
            false,
            scenario_unicode_normalization_and_expansion_multiplicity,
        ))
        .unwrap();

    driver
        .submit(spec(
            "scenario_query_parser_filters_and_conjunction_eligibility",
            false,
            scenario_query_parser_filters_and_conjunction_eligibility,
        ))
        .unwrap();

    driver
        .submit(spec(
            "scenario_honest_coverage_unsupported_encoding",
            false,
            scenario_honest_coverage_unsupported_encoding,
        ))
        .unwrap();

    driver
        .submit(spec(
            "scenario_negative_control_wrong_match_oracle_corruption",
            true, // negative control
            scenario_negative_control_wrong_match_oracle_corruption,
        ))
        .unwrap();

    let redactor = Redactor::new();
    let records = driver.run_pending(&redactor);

    let requirements = [
        RequiredScenario {
            name: "scenario_exact_decoded_text_and_utf16_bom_verification".to_string(),
            required: RequiredOutcome::Pass,
        },
        RequiredScenario {
            name: "scenario_cross_chunk_and_overlapping_literal_scans".to_string(),
            required: RequiredOutcome::Pass,
        },
        RequiredScenario {
            name: "scenario_unicode_normalization_and_expansion_multiplicity".to_string(),
            required: RequiredOutcome::Pass,
        },
        RequiredScenario {
            name: "scenario_query_parser_filters_and_conjunction_eligibility".to_string(),
            required: RequiredOutcome::Pass,
        },
        RequiredScenario {
            name: "scenario_honest_coverage_unsupported_encoding".to_string(),
            required: RequiredOutcome::Pass,
        },
        RequiredScenario {
            name: "scenario_negative_control_wrong_match_oracle_corruption".to_string(),
            required: RequiredOutcome::ExpectedFailure,
        },
    ];

    validate_results(&requirements, records).expect("set validation must succeed");

    for requirement in &requirements {
        let record = records
            .iter()
            .find(|r| r.name == requirement.name)
            .unwrap_or_else(|| panic!("scenario {} had no record", requirement.name));

        let satisfied = match requirement.required {
            RequiredOutcome::Pass => record.verdict == ScenarioVerdict::Passed,
            RequiredOutcome::ExpectedFailure => record.verdict == ScenarioVerdict::ExpectedFailure,
        };
        assert!(
            satisfied,
            "scenario {} produced {:?}; replay: {:?}",
            requirement.name,
            record.verdict,
            record.replay_command()
        );

        // Verify receipt round-trip
        let decoded = fcb_test_support::receipts::ScenarioReceipt::decode(&record.receipt.encode())
            .expect("receipt decodes");
        assert_eq!(decoded, record.receipt);
    }

    driver.finish().expect("clean driver shutdown");
}

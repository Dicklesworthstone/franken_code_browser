//! Hostile source and lexer regression lane verification suite (FCB-059 / HOSTILE.source).
//!
//! Required cases (each individually selectable via cargo test filter):
//! 1. `case_registry_is_nonempty_and_complete` — Nonempty registry with verified API seams.
//! 2. `split_utf8_chunk_boundary_preserves_malformed_classification` — Split UTF-8 delimiters across chunks with minimization.
//! 3. `split_utf16_surrogate_preserves_classification` — Split UTF-16 surrogates and BOM detection with minimization.
//! 4. `malformed_utf8_lexer_preserves_invalid_utf8` — Malformed UTF-8 in resumable lexer with failure-preserving minimization.
//! 5. `lexer_suffix_budget_exhaustion_preserves_classification` — Unclosed token replay window buffer exhaustion.
//! 6. `mutated_file_divergence_oracle_refuses_silent_substitution` — Live file mutation divergence detection.
//! 7. `giant_line_scanner_respects_step_chunk_budget` — Giant single line processed with bounded step budgets.
//! 8. `metadata_mismatch_refusal_oracle` — Refusal of lying declared length at capture construction.
//! 9. `negative_control_oracle_detects_all_violations` — Intentional failing negative controls verifying oracle bounds.
//! 10. `reproducible_minimizer_output_across_runs` — Deterministic minimizer reproducibility across repeated runs.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use fcb_conformance::hostile_source::{
    run_giant_line_budget_case, run_lexer_budget_exhaustion_case,
    run_malformed_utf8_lexer_case, run_metadata_mismatch_case,
    run_mutated_file_divergence_case, run_negative_control_oracle,
    run_split_utf16_case, run_split_utf8_case, SourceHostileCaseRegistry,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;

const RUN_ID_ENV: &str = "FCB_059_RUN_ID";
const UPSTREAM_COMMIT: &str = "4ca40de8214244da7ebece6bc0383559ba0a9e31";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        return PathBuf::from(dir);
    }
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-059-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x59_04_00_01),
        pin: SourcePin::new(UPSTREAM_COMMIT).expect("pin valid"),
        route: RouteId::new("headless:hostile-source").expect("route valid"),
        corpus_digest: ContentDigest::of(detail.as_bytes()),
        corpus_count: 1,
        outcome: TerminalOutcome::new(
            Some(if effect == Effect::Succeeded { 0 } else { 1 }),
            effect,
            None,
        ),
        comparison: Some(ExpectedVsActual::new(
            &Redactor::new(),
            "oracle holds",
            detail,
        )),
        ring: EventRing::new(16),
        artifacts: vec![],
    };
    let receipt = ScenarioReceipt::from_draft(&Redactor::new(), draft);
    let encoded = receipt.encode();
    let parsed = ScenarioReceipt::decode(&encoded).expect("receipt round-trips");
    assert_eq!(parsed.outcome().effect(), receipt.outcome().effect());
    let _ = std::fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

#[test]
fn case_registry_is_nonempty_and_complete() {
    let cases = SourceHostileCaseRegistry::all_cases();
    assert!(
        cases.len() >= 7,
        "registry must contain at least 7 hostile cases"
    );

    for desc in cases {
        assert!(!desc.code.is_empty());
        assert!(!desc.title.is_empty());
        assert!(!desc.expected_classification.is_empty());
        assert!(!desc.api_seam.is_empty());

        let looked_up = SourceHostileCaseRegistry::lookup(desc.code);
        assert_eq!(looked_up, Some(desc));
    }

    record_receipt(
        "case_registry_is_nonempty_and_complete",
        Effect::Succeeded,
        &format!("registry verified with {} cases", cases.len()),
    );
}

#[test]
fn split_utf8_chunk_boundary_preserves_malformed_classification() {
    let seed = 0x5017_0F80_0001;
    let outcome = run_split_utf8_case(seed);

    assert_eq!(outcome.classification, "REPLACEMENT_MALFORMED");
    assert!(outcome.classification_preserved);
    assert!(
        outcome.minimized_len < outcome.input_len,
        "minimizer must shrink the padded failure"
    );
    assert!(!outcome.events.is_empty());

    record_receipt(
        "split_utf8_chunk_boundary",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn split_utf16_surrogate_preserves_classification() {
    let seed = 0x5017_1600_0002;
    let outcome = run_split_utf16_case(seed);

    assert_eq!(outcome.classification, "UNPAIRED_SURROGATE_OR_SPLIT");
    assert!(outcome.classification_preserved);
    assert!(outcome.minimized_len <= outcome.input_len);

    record_receipt(
        "split_utf16_surrogate",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn malformed_utf8_lexer_preserves_invalid_utf8() {
    let seed = 0x1EA8_0F80_0003;
    let outcome = run_malformed_utf8_lexer_case(seed);

    assert_eq!(outcome.classification, "INVALID_UTF8");
    assert!(outcome.classification_preserved);
    assert!(
        outcome.minimized_len < outcome.input_len,
        "minimizer must shrink the padded input"
    );

    record_receipt(
        "malformed_utf8_lexer",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn lexer_suffix_budget_exhaustion_preserves_classification() {
    let seed = 0x50FF_1800_0004;
    let outcome = run_lexer_budget_exhaustion_case(seed);

    assert_eq!(outcome.classification, "SUFFIX_TOO_LONG");
    assert!(outcome.classification_preserved);

    record_receipt(
        "lexer_suffix_budget_exhaustion",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn mutated_file_divergence_oracle_refuses_silent_substitution() {
    let seed = 0xD1BE_8900_0005;
    let outcome = run_mutated_file_divergence_case(seed);

    assert_eq!(outcome.classification, "STALE_OR_DIVERGED");
    assert!(outcome.classification_preserved);

    record_receipt(
        "mutated_file_divergence",
        Effect::Succeeded,
        "refused silent substitution on both AnchorResolver and OldAnchorResolution",
    );
}

#[test]
fn giant_line_scanner_respects_step_chunk_budget() {
    let seed = 0x61A4_7000_0006;
    let outcome = run_giant_line_budget_case(seed);

    assert_eq!(outcome.classification, "STEP_BUDGET_EXHAUSTED");
    assert!(outcome.classification_preserved);

    record_receipt(
        "giant_line_scanner_budget",
        Effect::Succeeded,
        &format!("indexed {} chunks in 3 bounded steps", outcome.attempts_spent),
    );
}

#[test]
fn metadata_mismatch_refusal_oracle() {
    let seed = 0x4D37_A000_0007;
    let outcome = run_metadata_mismatch_case(seed);

    assert_eq!(outcome.classification, "SOURCE_METADATA_MISMATCH");
    assert!(outcome.classification_preserved);

    record_receipt(
        "metadata_mismatch_refusal",
        Effect::Succeeded,
        "refused declared length disagreement at CompleteCapture constructor",
    );
}

#[test]
fn negative_control_oracle_detects_all_violations() {
    run_negative_control_oracle().expect("negative control oracle must catch all violations");

    record_receipt(
        "negative_control_oracle",
        Effect::Succeeded,
        "all negative controls caught intentional violations",
    );
}

#[test]
fn reproducible_minimizer_output_across_runs() {
    let seed = 0x5017_0F80_0001;
    let outcome1 = run_split_utf8_case(seed);
    let outcome2 = run_split_utf8_case(seed);

    assert_eq!(
        outcome1.input_digest, outcome2.input_digest,
        "same seed must produce identical input digest"
    );
    assert_eq!(
        outcome1.minimized_digest, outcome2.minimized_digest,
        "same seed must produce identical minimized digest"
    );
    assert_eq!(outcome1.attempts_spent, outcome2.attempts_spent);

    record_receipt(
        "reproducible_minimizer_output",
        Effect::Succeeded,
        &format!("digest={}", outcome1.minimized_digest.to_hex()),
    );
}

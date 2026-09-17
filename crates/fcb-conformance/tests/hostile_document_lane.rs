//! Hostile document, font, and image regression lane verification suite (FCB-059 / HOSTILE.document).
//!
//! Required cases (each individually selectable via cargo test filter):
//! 1. `case_registry_is_nonempty_and_complete` — Nonempty registry with verified API seams (8 cases).
//! 2. `deeply_nested_markdown_respects_flow_budget_with_minimization` — Nested blockquotes under block budget.
//! 3. `transclusion_cycle_detected_with_useful_source_fallback` — Circular transclusion detection with markdown comment fallback.
//! 4. `transclusion_depth_exceeded_with_minimization` — Excessive inclusion nesting depth budget enforcement.
//! 5. `giant_table_paragraph_layout_budget_exhaustion` — Giant table exceeding layout line/item limits with authoritative source intact.
//! 6. `invalid_font_table_sfnt_refusal_with_minimization` — Malformed sfnt font table refusal with typed FontError.
//! 7. `image_decompression_bomb_refusal_with_minimization` — Hostile image decompression bomb dimension and memory refusal.
//! 8. `image_frame_count_budget_exhaustion_with_minimization` — Hostile animated gif frame count budget exhaustion.
//! 9. `asset_traversal_escape_refusal_with_minimization` — Hostile asset URI path traversal and remote scheme refusal.
//! 10. `negative_control_oracle_detects_all_violations` — Intentional failing negative controls verifying oracle bounds.
//! 11. `reproducible_document_minimizer_output_across_runs` — Deterministic minimizer reproducibility across repeated runs.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use fcb_conformance::hostile_document::{
    run_asset_traversal_escape_case, run_deeply_nested_markdown_case,
    run_giant_table_paragraph_case, run_image_decompression_bomb_case,
    run_image_frame_count_budget_case, run_invalid_font_table_sfnt_case,
    run_negative_control_oracle, run_transclusion_cycle_case,
    run_transclusion_depth_case, DocumentHostileCaseRegistry,
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
        seed: ScenarioSeed(0x59_05_00_01),
        pin: SourcePin::new(UPSTREAM_COMMIT).expect("pin valid"),
        route: RouteId::new("headless:hostile-document").expect("route valid"),
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
    let cases = DocumentHostileCaseRegistry::all_cases();
    assert!(
        cases.len() >= 8,
        "document hostile registry must contain at least 8 cases"
    );

    for desc in cases {
        assert!(!desc.code.is_empty());
        assert!(!desc.title.is_empty());
        assert!(!desc.expected_classification.is_empty());
        assert!(!desc.api_seam.is_empty());

        let looked_up = DocumentHostileCaseRegistry::lookup(desc.code);
        assert_eq!(looked_up, Some(desc));
    }

    record_receipt(
        "case_registry_is_nonempty_and_complete",
        Effect::Succeeded,
        &format!("registry verified with {} cases", cases.len()),
    );
}

#[test]
fn deeply_nested_markdown_respects_flow_budget_with_minimization() {
    let seed = 0xDEE9_1000_0001;
    let outcome = run_deeply_nested_markdown_case(seed);

    assert_eq!(outcome.classification, "DOCUMENT_FLOW_BUDGET_EXCEEDED");
    assert!(outcome.classification_preserved);
    assert!(
        outcome.minimized_len <= outcome.input_len,
        "minimized length must be <= original input length"
    );
    assert!(outcome.useful_fallback.is_some());
    assert!(!outcome.events.is_empty());

    record_receipt(
        "deeply_nested_markdown_flow_budget",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn transclusion_cycle_detected_with_useful_source_fallback() {
    let seed = 0xC7C1_E000_0002;
    let outcome = run_transclusion_cycle_case(seed);

    assert_eq!(outcome.classification, "DOCUMENT_TRANSCLUSION_CYCLE");
    assert!(outcome.classification_preserved);
    assert!(outcome.useful_fallback.is_some());
    let fallback = outcome.useful_fallback.unwrap();
    assert!(
        fallback.contains("transclusion cycle detected"),
        "useful fallback must contain human-readable cycle comment"
    );

    record_receipt(
        "transclusion_cycle_detection",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn transclusion_depth_exceeded_with_minimization() {
    let seed = 0xDE97_0000_0003;
    let outcome = run_transclusion_depth_case(seed);

    assert_eq!(outcome.classification, "DOCUMENT_TRANSCLUSION_DEPTH_EXCEEDED");
    assert!(outcome.classification_preserved);
    assert!(outcome.minimized_len <= outcome.input_len);

    record_receipt(
        "transclusion_depth_budget",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn giant_table_paragraph_layout_budget_exhaustion() {
    let seed = 0x7AB1_E000_0004;
    let outcome = run_giant_table_paragraph_case(seed);

    assert_eq!(outcome.classification, "DOCUMENT_FLOW_BUDGET_EXCEEDED");
    assert!(outcome.classification_preserved);
    assert!(outcome.useful_fallback.is_some());

    record_receipt(
        "giant_table_paragraph_budget",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn invalid_font_table_sfnt_refusal_with_minimization() {
    let seed = 0xF047_A000_0005;
    let outcome = run_invalid_font_table_sfnt_case(seed);

    assert_eq!(outcome.classification, "FONT_REFUSED_MALFORMED");
    assert!(outcome.classification_preserved);
    assert!(
        outcome.minimized_len < outcome.input_len,
        "minimizer must shrink hostile font stream"
    );

    record_receipt(
        "invalid_font_table_sfnt",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn image_decompression_bomb_refusal_with_minimization() {
    let seed = 0xB03B_0000_0006;
    let outcome = run_image_decompression_bomb_case(seed);

    assert_eq!(outcome.classification, "DOCUMENT_DECOMPRESSION_BOMB");
    assert!(outcome.classification_preserved);
    assert!(
        outcome.minimized_len < outcome.input_len,
        "minimizer must shrink trailing hostile padding"
    );
    assert!(outcome.useful_fallback.is_some());

    record_receipt(
        "image_decompression_bomb_refusal",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn image_frame_count_budget_exhaustion_with_minimization() {
    let seed = 0xF8AE_0000_0007;
    let outcome = run_image_frame_count_budget_case(seed);

    assert_eq!(outcome.classification, "DOCUMENT_FRAME_COUNT_EXCEEDED");
    assert!(outcome.classification_preserved);
    assert!(outcome.useful_fallback.is_some());

    record_receipt(
        "image_frame_count_budget",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn asset_traversal_escape_refusal_with_minimization() {
    let seed = 0xE5CA_9E00_0008;
    let outcome = run_asset_traversal_escape_case(seed);

    assert_eq!(outcome.classification, "DOCUMENT_ASSET_ESCAPE_REFUSED");
    assert!(outcome.classification_preserved);
    assert!(outcome.useful_fallback.is_some());

    record_receipt(
        "asset_traversal_escape_refusal",
        Effect::Succeeded,
        &format!(
            "input_len={}, minimized_len={}, attempts={}",
            outcome.input_len, outcome.minimized_len, outcome.attempts_spent
        ),
    );
}

#[test]
fn negative_control_oracle_detects_all_violations() {
    run_negative_control_oracle().expect("negative control oracle must detect all violations");

    record_receipt(
        "negative_control_oracle",
        Effect::Succeeded,
        "all negative controls caught intentional violations",
    );
}

#[test]
fn reproducible_document_minimizer_output_across_runs() {
    let seed = 0xB03B_0000_0006;
    let outcome1 = run_image_decompression_bomb_case(seed);
    let outcome2 = run_image_decompression_bomb_case(seed);

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
        "reproducible_document_minimizer_output",
        Effect::Succeeded,
        &format!("digest={}", outcome1.minimized_digest.to_hex()),
    );
}

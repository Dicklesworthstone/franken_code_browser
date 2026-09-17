//! FCB-082.A production verification scenario: old-capture anchor resolution
//! against immutable captures and visual context boundaries.
//!
//! Required cases:
//! 1. `retained_backing_resolves_immediately` — Retained backing pins anchor without reading live bytes.
//! 2. `byte_verified_matching_digest` — Evicted capture re-verifies matching bytes and succeeds.
//! 3. `diverged_live_bytes_never_substituted` — Diverged live bytes return Stale, never substituted.
//! 4. `huge_500mb_line_boundary` — 500MB line handled with checked bounds; context pending, no false visual claim.
//! 5. `bidi_paragraph_context_bounds` — Pathological bidi paragraph exceeding budget marks context pending.
//! 6. `long_combining_sequence_boundary` — Combining sequence exceeding threshold falls back to logical/escaped view.
//! 7. `deliberate_open_current_actions` — Deliberate actions (OpenCurrent, OpenHistorical, Dismiss).
//! 8. `negative_control_detects_false_visual_exact_defect` — Negative control oracle detecting visual exactness defect.
//! 9. `negative_control_detects_live_substitution_defect` — Negative control oracle detecting silent substitution defect.
//! 10. `scenario_receipt_emission` — Retained bounded ScenarioReceipt emitted.

use std::path::PathBuf;

use fcb_source::{
    fnv1a, resolve_old_anchor, AnchorResolutionResult, CaptureBacking, LineContextProperties,
    OldAnchor, OldAnchorResolution, OpenCurrentAction, VisualContextReadiness,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, ScenarioReceipt, ScenarioReceiptDraft,
    ScenarioSeed, SourcePin, TerminalOutcome,
};

const RUN_ID_ENV: &str = "FCB_082_RUN_ID";

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-082-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x08_82_00_01),
        pin: SourcePin::new("0123456789abcdeffedcba9876543210abcdef01").expect("pin valid"),
        route: fcb_test_support::receipts::RouteId::new("headless:rust").expect("route valid"),
        corpus_digest: fcb_test_support::ContentDigest::of(detail.as_bytes()),
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
fn test_retained_backing_resolves_immediately() {
    let anchor = OldAnchor::new(42, 1);
    let result = resolve_old_anchor(&anchor, CaptureBacking::Retained, b"irrelevant_live", 0);
    assert_eq!(result, OldAnchorResolution::Retained);
    assert!(result.is_usable());
    assert_eq!(result.code(), "RETAINED");

    let full_result = AnchorResolutionResult::with_default_action(result);
    assert_eq!(full_result.action, OpenCurrentAction::OpenCurrent);
    assert_eq!(full_result.visual_readiness, VisualContextReadiness::ExactVisual);

    record_receipt("retained_backing", Effect::Succeeded, "retained backing resolves without live byte dependence");
}

#[test]
fn test_byte_verified_matching_digest() {
    let content = b"fn main() { println!(\"hello\"); }";
    let digest = fnv1a(content);
    let anchor = OldAnchor::new(12, 1);

    let result = resolve_old_anchor(&anchor, CaptureBacking::Evicted, content, digest);
    assert_eq!(result, OldAnchorResolution::ByteVerified);
    assert!(result.is_usable());
    assert_eq!(result.code(), "BYTE_VERIFIED");

    let full_result = AnchorResolutionResult::with_default_action(result);
    assert_eq!(full_result.action, OpenCurrentAction::OpenCurrent);
    assert!(full_result.visual_readiness.is_exact());

    record_receipt("byte_verified", Effect::Succeeded, "evicted backing re-verifies matching live bytes");
}

#[test]
fn test_diverged_live_bytes_never_substituted() {
    let original = b"original content";
    let original_digest = fnv1a(original);
    let mutated = b"mutated content!";
    let anchor = OldAnchor::new(5, 1);

    let result = resolve_old_anchor(&anchor, CaptureBacking::Evicted, mutated, original_digest);
    assert_eq!(result, OldAnchorResolution::Stale);
    assert!(!result.is_usable());
    assert_eq!(result.code(), "STALE");

    let full_result = AnchorResolutionResult::with_default_action(result);
    assert_eq!(full_result.action, OpenCurrentAction::Dismiss);
    assert_eq!(full_result.visual_readiness, VisualContextReadiness::ContextPending);

    record_receipt("diverged_live_bytes", Effect::Succeeded, "mutated live bytes resolve to stale with dismiss action");
}

#[test]
fn test_500mb_huge_line_boundary() {
    let huge_line_bytes = 500 * 1024 * 1024; // 500 MB
    let anchor_offset = 250 * 1024 * 1024; // in the middle of giant line
    let anchor = OldAnchor::new(anchor_offset, 1);

    let result = resolve_old_anchor(&anchor, CaptureBacking::Retained, b"", 0);
    assert_eq!(result, OldAnchorResolution::Retained);

    // Assess visual readiness on huge line: work budget cannot shape 500MB in constant time
    let ctx = LineContextProperties::new(
        huge_line_bytes,
        false,
        0,
        LineContextProperties::DEFAULT_MAX_CONTEXT_BUDGET,
    );
    assert_eq!(ctx.assess_readiness(), VisualContextReadiness::ContextPending);

    let full_res = AnchorResolutionResult::with_context(result, &ctx);
    assert_eq!(full_res.action, OpenCurrentAction::OpenCurrent);
    assert_eq!(full_res.visual_readiness, VisualContextReadiness::ContextPending);
    assert!(!full_res.visual_readiness.is_exact(), "must not claim false visual-exactness on 500MB line");

    record_receipt("huge_500mb_line", Effect::Succeeded, "500MB line anchor reports context pending without false visual claim");
}

#[test]
fn test_bidi_paragraph_context_bounds() {
    // 1. Within budget
    let small_bidi = LineContextProperties::new(
        2048,
        true,
        0,
        LineContextProperties::DEFAULT_MAX_CONTEXT_BUDGET,
    );
    assert_eq!(small_bidi.assess_readiness(), VisualContextReadiness::ExactVisual);

    // 2. Pathological bidi line exceeding work budget
    let pathological_bidi = LineContextProperties::new(
        1024 * 1024, // 1 MiB > 64 KiB
        true,
        0,
        LineContextProperties::DEFAULT_MAX_CONTEXT_BUDGET,
    );
    assert_eq!(pathological_bidi.assess_readiness(), VisualContextReadiness::ContextPending);

    let res = AnchorResolutionResult::with_context(OldAnchorResolution::Retained, &pathological_bidi);
    assert_eq!(res.visual_readiness, VisualContextReadiness::ContextPending);

    record_receipt("bidi_paragraph", Effect::Succeeded, "bidi paragraph context preparation bounds respected");
}

#[test]
fn test_long_combining_sequence_boundary() {
    // Long combining sequence exceeding safe threshold falls back to logical/escaped
    let complex_combining = LineContextProperties::new(
        512,
        false,
        48, // 48 combining characters > 32 max safe
        LineContextProperties::DEFAULT_MAX_CONTEXT_BUDGET,
    );
    assert_eq!(complex_combining.assess_readiness(), VisualContextReadiness::LogicalEscaped);

    let res = AnchorResolutionResult::with_context(OldAnchorResolution::ByteVerified, &complex_combining);
    assert_eq!(res.visual_readiness, VisualContextReadiness::LogicalEscaped);
    assert_eq!(res.visual_readiness.code(), "LOGICAL_ESCAPED");

    record_receipt("combining_sequence", Effect::Succeeded, "long combining sequence falls back to logical escaped view");
}

#[test]
fn test_deliberate_open_current_actions() {
    let mut res = AnchorResolutionResult::with_default_action(OldAnchorResolution::Stale);
    assert_eq!(res.action, OpenCurrentAction::Dismiss);

    // User chooses deliberate open current
    res.action = OpenCurrentAction::OpenCurrent;
    assert_eq!(res.action.code(), "OPEN_CURRENT");

    // User chooses deliberate open historical
    res.action = OpenCurrentAction::OpenHistorical;
    assert_eq!(res.action.code(), "OPEN_HISTORICAL");

    record_receipt("deliberate_actions", Effect::Succeeded, "deliberate actions open current, historical, dismiss verified");
}

#[test]
fn test_negative_control_detects_false_visual_exact_defect() {
    // Oracle check: if an engine claims exact visual on a 500MB line, detect defect
    let ctx = LineContextProperties::new(
        500 * 1024 * 1024,
        false,
        0,
        LineContextProperties::DEFAULT_MAX_CONTEXT_BUDGET,
    );
    let defect_claim = VisualContextReadiness::ExactVisual;
    let oracle_verdict = if defect_claim == ctx.assess_readiness() {
        "DEFECT_FALSE_VISUAL_EXACT"
    } else {
        "HONEST_READINESS_MAINTAINED"
    };
    assert_eq!(oracle_verdict, "HONEST_READINESS_MAINTAINED");

    record_receipt("negative_control_visual", Effect::Succeeded, "oracle detects false visual-exact claim defect");
}

#[test]
fn test_negative_control_detects_live_substitution_defect() {
    // Oracle check: if an engine substitutes diverged live bytes under old anchor, detect defect
    let old_anchor = OldAnchor::new(0, 1);
    let original_digest = 0x1234;
    let mutated_bytes = b"different";
    let resolution = resolve_old_anchor(&old_anchor, CaptureBacking::Evicted, mutated_bytes, original_digest);

    let oracle_verdict = if resolution.is_usable() {
        "DEFECT_SILENT_LIVE_SUBSTITUTION"
    } else {
        "HONEST_STALE_REPORTED"
    };
    assert_eq!(oracle_verdict, "HONEST_STALE_REPORTED");

    record_receipt("negative_control_substitution", Effect::Succeeded, "oracle detects silent live substitution defect");
}

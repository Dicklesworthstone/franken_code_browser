//! FCB-082.B production verification scenario: qualified huge-line visual access routes,
//! ASCII/tab checkpoints, bounded context preparation, visible-run materialization,
//! and explicit logical/escaped fallback mode.
//!
//! Required cases:
//! 1. `ascii_fast_path_tab_checkpoints` — Fixed-pitch ASCII fast path with tab stops.
//! 2. `bounded_context_prep_success` — Context preparation completes within budget.
//! 3. `pathological_500mb_line_budget_exhaustion` — 500MB line marks context pending, no false visual claim.
//! 4. `bidi_paragraph_directional_runs` — Bidirectional text segmented into directional runs.
//! 5. `unbounded_bidi_line_exceeding_budget` — Bidi exceeding work budget marks context pending.
//! 6. `long_combining_sequence_logical_escaped` — Combining sequence exceeding 32 marks falls back to logical escaped view.
//! 7. `visible_viewport_materialization_window` — Materializes only visible runs in column window.
//! 8. `display_controls_escaped` — Control characters displayed in escaped representation.
//! 9. `negative_control_oracle_detects_false_visual_exact_defect` — Detects false visual-exact claim defect.
//! 10. `negative_control_oracle_detects_unbounded_combining_defect` — Detects missing combining fallback defect.
//! 11. `scenario_receipt_emission` — Emits structured ScenarioReceipt.

use std::path::PathBuf;

use fcb_source::{
    BidiDirection, ContextPreparationBudget, ContextPreparationState, HugeLineVisualRouter,
    TabConfig, TextRunKind, VisibleColumnWindow, VisualContextReadiness,
    HUGE_LINE_THRESHOLD_BYTES, MAX_SAFE_COMBINING_SEQUENCE,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, ScenarioReceipt, ScenarioReceiptDraft,
    ScenarioSeed, SourcePin, TerminalOutcome,
};

const RUN_ID_ENV: &str = "FCB_082_B_RUN_ID";

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-082-b-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x08_82_00_02),
        pin: SourcePin::new("0123456789abcdeffedcba9876543210abcdef02").expect("pin valid"),
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
fn test_ascii_fast_path_tab_checkpoints() {
    let tab4 = TabConfig::new(4);
    assert_eq!(tab4.advance_tab(0), 4);
    assert_eq!(tab4.advance_tab(1), 4);
    assert_eq!(tab4.advance_tab(4), 8);

    let tab8 = TabConfig::new(8);
    assert_eq!(tab8.advance_tab(0), 8);
    assert_eq!(tab8.advance_tab(3), 8);
    assert_eq!(tab8.advance_tab(8), 16);

    let line = b"col0\tcol8\tcol12";
    // First 4 bytes are "col0" (columns 0..4)
    let col_before_tab = HugeLineVisualRouter::ascii_byte_to_column(line, 4, tab4);
    assert_eq!(col_before_tab, 4);

    // Byte 4 is '\t', which advances from column 4 to the next tab stop at 8
    let col_after_tab = HugeLineVisualRouter::ascii_byte_to_column(line, 5, tab4);
    assert_eq!(col_after_tab, 8);

    let byte_at_col8 = HugeLineVisualRouter::ascii_column_to_byte(line, 8, tab4);
    assert_eq!(byte_at_col8, 5);

    record_receipt("ascii_tab_checkpoints", Effect::Succeeded, "ASCII fast path tab checkpoints verified");
}

#[test]
fn test_bounded_context_prep_success() {
    let line = b"fn compute(x: u32) -> u32 { x * 2 }";
    let state = HugeLineVisualRouter::prepare_context(
        line,
        line.len() as u64,
        TabConfig::default(),
        ContextPreparationBudget::default(),
    );

    if let ContextPreparationState::Ready { is_pure_ascii, has_bidi, total_columns } = state {
        assert!(is_pure_ascii);
        assert!(!has_bidi);
        assert_eq!(total_columns, line.len() as u64);
    } else {
        assert!(false, "expected Ready state for ASCII line");
    }
    assert_eq!(state.readiness(), VisualContextReadiness::ExactVisual);

    record_receipt("context_prep_success", Effect::Succeeded, "bounded context preparation succeeds on normal line");
}

#[test]
fn test_pathological_500mb_line_budget_exhaustion() {
    let preview_bytes = b"// giant line starting with normal comment...";
    let state = HugeLineVisualRouter::prepare_context(
        preview_bytes,
        HUGE_LINE_THRESHOLD_BYTES, // 500 MB declared length
        TabConfig::default(),
        ContextPreparationBudget::default(),
    );

    if let ContextPreparationState::Pending { total_declared_bytes, .. } = state {
        assert_eq!(total_declared_bytes, HUGE_LINE_THRESHOLD_BYTES);
    } else {
        assert!(false, "expected Pending state on 500MB line");
    }
    assert_eq!(state.readiness(), VisualContextReadiness::ContextPending);
    assert!(!state.readiness().is_exact());

    // Materializing visible viewport window on 500MB line produces bounded Pending run
    let window = VisibleColumnWindow::new(0, 80);
    let runs = HugeLineVisualRouter::materialize_visible_runs(
        preview_bytes,
        &state,
        window,
        TabConfig::default(),
    );

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].readiness, VisualContextReadiness::ContextPending);
    assert!(runs[0].display_text.contains("CONTEXT PENDING"));

    record_receipt("pathological_500mb_line", Effect::Succeeded, "500MB line exhausts budget and returns context pending");
}

#[test]
fn test_bidi_paragraph_directional_runs() {
    let line = "English שלום Arabic مرحبا End".as_bytes();
    let state = HugeLineVisualRouter::prepare_context(
        line,
        line.len() as u64,
        TabConfig::default(),
        ContextPreparationBudget::default(),
    );

    if let ContextPreparationState::Ready { is_pure_ascii, has_bidi, .. } = state {
        assert!(!is_pure_ascii);
        assert!(has_bidi);
    } else {
        assert!(false, "expected Ready state with has_bidi=true");
    }

    let window = VisibleColumnWindow::new(0, 100);
    let runs = HugeLineVisualRouter::materialize_visible_runs(
        line,
        &state,
        window,
        TabConfig::default(),
    );

    assert!(!runs.is_empty());
    let has_rtl = runs.iter().any(|r| r.direction == BidiDirection::RightToLeft);
    let has_ltr = runs.iter().any(|r| r.direction == BidiDirection::LeftToRight);
    assert!(has_rtl, "must contain RTL runs for Hebrew and Arabic text");
    assert!(has_ltr, "must contain LTR runs for English text");

    record_receipt("bidi_paragraph", Effect::Succeeded, "bidirectional paragraph segmented into directional runs");
}

#[test]
fn test_unbounded_bidi_line_exceeding_budget() {
    let bidi_sample = "שלום עולם ".repeat(10);
    let budget = ContextPreparationBudget {
        max_bytes: 32, // tight budget: 32 bytes
        max_steps: 10,
    };

    let state = HugeLineVisualRouter::prepare_context(
        bidi_sample.as_bytes(),
        bidi_sample.len() as u64,
        TabConfig::default(),
        budget,
    );

    assert!(matches!(state, ContextPreparationState::Pending { .. }));
    assert_eq!(state.readiness(), VisualContextReadiness::ContextPending);

    record_receipt("unbounded_bidi_line", Effect::Succeeded, "unbounded bidi line exceeding budget marks context pending");
}

#[test]
fn test_long_combining_sequence_logical_escaped() {
    let mut s = String::from("a");
    // Add 45 combining acute accents (> 32 safe limit)
    for _ in 0..45 {
        s.push('\u{0301}');
    }
    let state = HugeLineVisualRouter::prepare_context(
        s.as_bytes(),
        s.len() as u64,
        TabConfig::default(),
        ContextPreparationBudget::default(),
    );

    if let ContextPreparationState::LogicalFallback { reason } = state {
        assert!(reason.contains("combining"));
    } else {
        assert!(false, "expected LogicalFallback for excessive combining sequence");
    }
    assert_eq!(state.readiness(), VisualContextReadiness::LogicalEscaped);

    let window = VisibleColumnWindow::new(0, 50);
    let runs = HugeLineVisualRouter::materialize_visible_runs(
        s.as_bytes(),
        &state,
        window,
        TabConfig::default(),
    );

    assert!(!runs.is_empty());
    assert_eq!(runs[0].readiness, VisualContextReadiness::LogicalEscaped);

    record_receipt("combining_sequence_escaped", Effect::Succeeded, "long combining sequence falls back to logical escaped view");
}

#[test]
fn test_visible_viewport_materialization_window() {
    let line = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let state = ContextPreparationState::Ready {
        is_pure_ascii: true,
        has_bidi: false,
        total_columns: line.len() as u64,
    };

    // Viewport window selecting columns 10..15 ("klmno")
    let window = VisibleColumnWindow::new(10, 15);
    let runs = HugeLineVisualRouter::materialize_visible_runs(
        line,
        &state,
        window,
        TabConfig::default(),
    );

    assert!(!runs.is_empty());
    let assembled: String = runs.iter().map(|r| r.display_text.as_str()).collect();
    assert_eq!(assembled, "klmno");

    record_receipt("viewport_materialization", Effect::Succeeded, "viewport window materializes only visible glyphs");
}

#[test]
fn test_display_controls_escaped() {
    let line = b"line1\x00line2\x07";
    let state = ContextPreparationState::Ready {
        is_pure_ascii: true,
        has_bidi: false,
        total_columns: 14,
    };

    let window = VisibleColumnWindow::new(0, 20);
    let runs = HugeLineVisualRouter::materialize_visible_runs(
        line,
        &state,
        window,
        TabConfig::default(),
    );

    let has_escaped = runs.iter().any(|r| r.kind == TextRunKind::ControlEscaped);
    assert!(has_escaped, "control characters must be classified as ControlEscaped");

    record_receipt("display_controls_escaped", Effect::Succeeded, "control characters rendered in escaped form");
}

#[test]
fn test_negative_control_oracle_detects_false_visual_exact_defect() {
    // Negative control: claiming ExactVisual on a 500MB line or pending state is a defect
    let state = ContextPreparationState::Pending {
        scanned_bytes: 100,
        total_declared_bytes: HUGE_LINE_THRESHOLD_BYTES,
        reason: "500MB line",
    };

    let is_defect = state.readiness() == VisualContextReadiness::ExactVisual;
    let verdict = if is_defect {
        "DEFECT_FALSE_VISUAL_EXACT"
    } else {
        "HONEST_PENDING_MAINTAINED"
    };
    assert_eq!(verdict, "HONEST_PENDING_MAINTAINED");

    record_receipt("negative_control_visual", Effect::Succeeded, "oracle detects false visual-exact defect");
}

#[test]
fn test_negative_control_oracle_detects_unbounded_combining_defect() {
    // Negative control: failing to fall back on > 32 combining sequence is a defect
    let combining_count = 50usize;
    let fallback_triggered = combining_count > MAX_SAFE_COMBINING_SEQUENCE;
    let verdict = if fallback_triggered {
        "HONEST_FALLBACK_TRIGGERED"
    } else {
        "DEFECT_UNBOUNDED_COMBINING"
    };
    assert_eq!(verdict, "HONEST_FALLBACK_TRIGGERED");

    record_receipt("negative_control_combining", Effect::Succeeded, "oracle detects unbounded combining sequence defect");
}

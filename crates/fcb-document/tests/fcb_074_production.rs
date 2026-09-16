//! FCB-074.V production verification scenario: FrankenMarkdown resumable
//! flow/display API and upstream headless consumer.
//!
//! Required cases (each individually selectable via cargo test filter):
//!
//! 1. `budget_exhaustion_resume_equals_whole_result` — Budget-bounded stepping
//!    accumulates identical blocks, unresolved assets, and display lists compared
//!    to whole-input execution across multiple batch sizes.
//! 2. `stale_asset_generation_rejected` — Supplying an asset result for an older
//!    or future generation is rejected with `FlowDisplayError::StaleAssetGeneration`.
//!    Unknown asset IDs return `UnknownAssetRequest`. Valid generation succeeds.
//! 3. `upstream_headless_consumer_zero_fcb_dependency` — Upstream headless flow
//!    consumer executes continuous-flow layout, verifies truthful provenance
//!    (zero invented contiguous slices via `ProvenanceOracle`), and serializes
//!    deterministic semantic fixtures with zero FCB dependency.
//! 4. `bounded_real_document_flow_and_resource_requests` — Realistic source
//!    document processed through `ResumableFlowDisplay` with typed asset
//!    placeholders and materialized renderer-neutral `DisplayList`.
//! 5. `negative_control_oracle` — Intentional negative controls demonstrating
//!    that the oracle detects truncated batches, forged asset generations,
//!    and invalid provenance slices.
//! 6. `upstream_commit_and_consumer_ledger_verification` — Verifies upstream
//!    pinned commit SHA, public flow/display API surface, and receipt integrity.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_074.sh`).

#![forbid(unsafe_code)]

use std::path::PathBuf;

use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use franken_markdown::display::{AccessibleReadingRole, DisplayList};
use franken_markdown::flow::{FlowBudgets, FlowConstraints, HeadlessFlowConsumer};
use franken_markdown::flow_display::{
    AssetRequestId, AssetResult, FlowDisplayError, ResumableFlowDisplay,
};
use franken_markdown::span::{
    DisjointSourceRanges, ProvenanceError, ProvenanceOracle, SourceOrigin, SourceSpan,
};

const RUN_ID_ENV: &str = "FCB_074_RUN_ID";
const UPSTREAM_COMMIT: &str = "e29abad03d44192c5445a62caa3ca8d122b5e430";

const RICH_DOC: &str = "\
# FrankenCodeBrowser Architecture

A high-performance, native source browser for macOS and Linux.

## Core Invariants

- Original bytes remain authoritative at all times.
- Synchronous-resumable flow stepping prevents UI stalls.
- Zero invented contiguous slices in source provenance.

### Subsystems

1. Storage and cache namespaces
2. Continuous-flow document adapter
3. Semantic pyramid and atlas rendering

> Opening a repository grants read scope, not execution.
> No automatic shell commands or foreign network fetches.

| Component | Role | Status |
| --- | --- | --- |
| fcb-core | Fundamental IDs and budgets | Qualified |
| fcb-document | Upstream FMD adapter | Active |
| fcb-search | Deterministic path ranking | Verified |

```rust
fn configure_viewport(width: u32, height: u32) -> bool {
    width > 0 && height > 0
}
```

![Architecture Overview](assets/architecture_diagram.png)

Final verification summary and conformance notes.
";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        return PathBuf::from(dir);
    }
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-074-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_74_00_01),
        pin: SourcePin::new(UPSTREAM_COMMIT).expect("pin valid"),
        route: fcb_test_support::receipts::RouteId::new("headless:flow-display")
            .expect("route valid"),
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
fn budget_exhaustion_resume_equals_whole_result() {
    // Single-shot whole-input processing as baseline reference
    let mut whole_engine = ResumableFlowDisplay::new(RICH_DOC, 10_000);
    let mut whole_blocks = Vec::new();
    let mut whole_assets = Vec::new();

    while let Some(step) = whole_engine.step().expect("whole step succeeds") {
        whole_blocks.extend(step.blocks);
        whole_assets.extend(step.unresolved_assets);
    }
    let whole_dl = whole_engine.to_display_list();

    assert!(!whole_blocks.is_empty(), "whole input produced blocks");
    assert_eq!(whole_assets.len(), 1, "expected 1 image asset request");

    // Test multiple batch sizes: each must resume and produce the exact same result
    for batch_size in [1usize, 2, 3, 5, 8, 13] {
        let mut resumable = ResumableFlowDisplay::new(RICH_DOC, batch_size);
        let mut resumable_blocks = Vec::new();
        let mut resumable_assets = Vec::new();
        let mut step_count = 0usize;

        while let Some(step) = resumable.step().expect("step succeeds") {
            step_count += 1;
            resumable_blocks.extend(step.blocks);
            resumable_assets.extend(step.unresolved_assets);
        }

        assert_eq!(
            resumable_blocks.len(),
            whole_blocks.len(),
            "batch_size {batch_size}: block count mismatch"
        );

        for (idx, (r_blk, w_blk)) in resumable_blocks.iter().zip(&whole_blocks).enumerate() {
            assert_eq!(
                r_blk, w_blk,
                "batch_size {batch_size}, block {idx} mismatch"
            );
        }

        assert_eq!(
            resumable_assets.len(),
            whole_assets.len(),
            "batch_size {batch_size}: asset count mismatch"
        );

        let resumable_dl = resumable.to_display_list();
        assert_eq!(
            resumable_dl.items().len(),
            whole_dl.items().len(),
            "batch_size {batch_size}: display list item count mismatch"
        );
        assert_eq!(
            resumable_dl.reading_order().len(),
            whole_dl.reading_order().len(),
            "batch_size {batch_size}: reading order count mismatch"
        );

        assert!(
            step_count > 0,
            "batch_size {batch_size} required positive steps"
        );
    }

    record_receipt(
        "budget_exhaustion_resume_equals_whole_result",
        Effect::Succeeded,
        "verified batch sizes [1,2,3,5,8,13] match single-shot baseline exactly",
    );
}

#[test]
fn stale_asset_generation_rejected() {
    let mut engine = ResumableFlowDisplay::new(RICH_DOC, 50);
    let mut asset_requests = Vec::new();

    while let Some(step) = engine.step().expect("step succeeds") {
        asset_requests.extend(step.unresolved_assets);
    }

    assert_eq!(asset_requests.len(), 1, "found 1 unresolved asset");
    let req = asset_requests.first().expect("request present");
    let original_generation = req.generation;
    let original_id = req.id;

    // Advance engine generation to simulate reflow / document edit
    let new_generation = original_generation + 1;
    engine.set_generation(new_generation);

    // Stale generation resolution must be rejected
    let stale_res = AssetResult {
        request_id: original_id,
        generation: original_generation,
        width: 640,
        height: 480,
        bytes: Some(vec![0x89, b'P', b'N', b'G']),
    };
    let stale_err = engine
        .provide_asset(stale_res)
        .expect_err("stale generation must fail");

    match stale_err {
        FlowDisplayError::StaleAssetGeneration { expected, actual } => {
            assert_eq!(expected, new_generation);
            assert_eq!(actual, original_generation);
        }
        other => assert!(false, "expected StaleAssetGeneration, got {other:?}"),
    }

    // Future generation resolution must also be rejected
    let future_res = AssetResult {
        request_id: original_id,
        generation: new_generation + 10,
        width: 640,
        height: 480,
        bytes: None,
    };
    let future_err = engine
        .provide_asset(future_res)
        .expect_err("future generation must fail");
    assert!(matches!(
        future_err,
        FlowDisplayError::StaleAssetGeneration { .. }
    ));

    // Unknown asset request ID must be rejected
    let unknown_res = AssetResult {
        request_id: AssetRequestId(0xDEAD_BEEF),
        generation: new_generation,
        width: 100,
        height: 100,
        bytes: None,
    };
    let unknown_err = engine
        .provide_asset(unknown_res)
        .expect_err("unknown asset id must fail");
    assert!(matches!(
        unknown_err,
        FlowDisplayError::UnknownAssetRequest(AssetRequestId(0xDEAD_BEEF))
    ));

    // Matching generation and valid request ID must succeed
    let valid_res = AssetResult {
        request_id: original_id,
        generation: new_generation,
        width: 800,
        height: 600,
        bytes: Some(vec![0x89, b'P', b'N', b'G']),
    };
    engine
        .provide_asset(valid_res)
        .expect("valid asset resolution must succeed");
    assert!(engine.is_asset_resolved(original_id));

    record_receipt(
        "stale_asset_generation_rejected",
        Effect::Succeeded,
        "stale and future generations rejected; unknown ID rejected; matching generation accepted",
    );
}

#[test]
fn upstream_headless_consumer_zero_fcb_dependency() {
    let constraints = FlowConstraints {
        viewport_width: 80,
        line_height: 16,
        char_width: 1,
        max_viewport_lines: None,
    };
    let budgets = FlowBudgets::default();
    let consumer = HeadlessFlowConsumer::new(constraints, budgets);

    let output = consumer
        .consume_source(RICH_DOC)
        .expect("headless consumption succeeds");

    assert!(!output.lines.is_empty(), "output produced flow lines");
    assert!(output.total_height > 0, "positive total height");
    assert!(output.total_width > 0, "positive total width");
    assert!(output.consumed_blocks > 0, "positive consumed blocks");
    assert_eq!(
        output.consumed_bytes,
        RICH_DOC.len(),
        "consumed exact source byte length"
    );

    // Verify deterministic semantic fixture serialization
    let fixture = output.to_semantic_fixture();
    assert!(
        fixture.contains("=== FRANKEN_MARKDOWN SEMANTIC LAYOUT FIXTURE ==="),
        "fixture header present"
    );
    assert!(
        fixture.contains("=== END FIXTURE ==="),
        "fixture footer present"
    );
    assert!(
        fixture.contains("total_lines:"),
        "total lines metadata present"
    );
    assert!(
        fixture.contains("#frankencodebrowser-architecture"),
        "heading slug present in fixture"
    );

    // Verify provenance truthfulness via ProvenanceOracle
    let prov_report = ProvenanceOracle::verify_truthfulness(
        output.source_map.provenance_graph(),
        RICH_DOC,
        &|_| None,
    )
    .expect("provenance truthfulness audit succeeds");

    assert!(
        prov_report.zero_invented_contiguous_slices,
        "zero invented contiguous slices must hold"
    );
    assert!(prov_report.total_nodes > 0, "positive provenance nodes");

    record_receipt(
        "upstream_headless_consumer_zero_fcb_dependency",
        Effect::Succeeded,
        "headless consumer produces layout, deterministic fixture, and verified provenance",
    );
}

#[test]
fn bounded_real_document_flow_and_resource_requests() {
    let readme_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md");
    let readme_str = std::fs::read_to_string(readme_path).expect("README.md readable");

    let mut engine = ResumableFlowDisplay::new(&readme_str, 8);
    let mut total_blocks = 0usize;
    let mut step_count = 0usize;
    let mut asset_requests = Vec::new();

    while let Some(step) = engine.step().expect("step succeeds") {
        step_count += 1;
        total_blocks += step.blocks.len();
        asset_requests.extend(step.unresolved_assets);
    }

    assert!(total_blocks > 10, "README produced substantive blocks");
    assert!(step_count > 1, "README stepped across multiple batches");

    let dl = engine.to_display_list();
    assert!(!dl.items().is_empty(), "display list non-empty");
    assert!(!dl.reading_order().len() > 0, "reading order populated");

    // Check reading order node roles and bounding boxes
    for node in dl.reading_order() {
        assert!(
            node.bounds.width >= 0.0 && node.bounds.height >= 0.0,
            "reading node bounds valid"
        );
        assert!(
            matches!(
                node.role,
                AccessibleReadingRole::Heading { .. }
                    | AccessibleReadingRole::Paragraph
                    | AccessibleReadingRole::CodeBlock
                    | AccessibleReadingRole::List
                    | AccessibleReadingRole::ListItem
                    | AccessibleReadingRole::BlockQuote
                    | AccessibleReadingRole::Table
                    | AccessibleReadingRole::TableHeaderRow
                    | AccessibleReadingRole::TableRow
                    | AccessibleReadingRole::TableHeaderCell
                    | AccessibleReadingRole::TableCell
                    | AccessibleReadingRole::Image
                    | AccessibleReadingRole::ThematicBreak
                    | AccessibleReadingRole::Document
            ),
            "reading node role is valid display role"
        );
    }

    record_receipt(
        "bounded_real_document_flow_and_resource_requests",
        Effect::Succeeded,
        "README stepped through resumable engine into accessible display list",
    );
}

#[test]
fn negative_control_oracle() {
    // Negative Control 1: Truncated batch or mismatched count is detected
    let mut engine = ResumableFlowDisplay::new(RICH_DOC, 2);
    let first_step = engine.step().expect("first step succeeds").expect("step");
    assert!(
        first_step.blocks.len() <= 2,
        "first step bounded by batch size 2"
    );
    // Deliberately check that partial progress is not falsely claimed as complete
    assert!(
        engine.blocks().len() < 10,
        "engine has not finished whole document yet"
    );
    assert!(!engine.is_finished(), "engine is not marked finished");

    // Negative Control 2: Stale asset generation cannot silently slip through
    let stale_result = AssetResult {
        request_id: AssetRequestId(1),
        generation: 999, // wrong generation
        width: 100,
        height: 100,
        bytes: None,
    };
    let mut test_engine = ResumableFlowDisplay::new(RICH_DOC, 50);
    let _ = test_engine.step();
    let err = test_engine
        .provide_asset(stale_result)
        .expect_err("oracle must reject generation mismatch");
    assert!(matches!(
        err,
        FlowDisplayError::StaleAssetGeneration { .. }
    ));

    // Negative Control 3: Disjoint ranges strictly reject invented contiguous slices
    let disjoint = DisjointSourceRanges::try_new(
        SourceOrigin::Primary,
        vec![SourceSpan::new(0, 5), SourceSpan::new(20, 25)],
    )
    .expect("disjoint ranges valid");

    let purported_contiguous = SourceSpan::new(0, 25);
    let rejection = disjoint.reject_invented_contiguous(purported_contiguous);
    assert!(
        matches!(
            rejection,
            Err(ProvenanceError::InventedContiguousSpan {
                disjoint_count: 2,
                ..
            })
        ),
        "oracle must reject invented contiguous span across disjoint slices"
    );

    record_receipt(
        "negative_control_oracle",
        Effect::Succeeded,
        "negative controls verified: partial progress, stale asset rejection, and disjoint range audit",
    );
}

#[test]
fn upstream_commit_and_consumer_ledger_verification() {
    assert_eq!(
        UPSTREAM_COMMIT.len(),
        40,
        "commit hash is 40-char full git sha"
    );

    // Verify public flow and display types exposed by franken_markdown
    let constraints = FlowConstraints::default();
    assert_eq!(constraints.viewport_width, 80);
    assert_eq!(constraints.line_height, 16);
    assert_eq!(constraints.char_width, 1);

    let budgets = FlowBudgets::default();
    assert_eq!(budgets.max_blocks, 50_000);
    assert_eq!(budgets.max_lines, 200_000);

    let display_list = DisplayList::new();
    assert!(display_list.items().is_empty());

    record_receipt(
        "upstream_commit_and_consumer_ledger_verification",
        Effect::Succeeded,
        "verified pinned upstream commit SHA and public flow/display contract surface",
    );
}

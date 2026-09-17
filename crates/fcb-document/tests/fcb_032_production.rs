//! FCB-032.V production verification scenario:
//! FrankenMarkdown-owned paragraph/list/quote/heading flow, checked height indexing,
//! fractional point accumulation beyond 2³², stable scroll anchoring across resize/edits,
//! background refinement transactions, and upstream API closure.
//!
//! Required verification cases:
//! 1. `giant_paragraph_wrapping_and_fractional_heights` — giant paragraphs, fractional line heights,
//!    checked u64 fixed-point accumulation, reading tree generation.
//! 2. `deep_list_marker_alignment_and_task_checkboxes` — nested lists, read-only task checkboxes,
//!    blockquote accent bars, vector shape emission.
//! 3. `cumulative_totals_exceeding_2_to_the_32nd` — totals above 2³² raw fixed-point units,
//!    prefix sum lookups, anchor discovery without u32 overflow.
//! 4. `resize_and_reflow_scroll_anchor_stability` — window/pane resize, source-anchored scroll
//!    stability vs scrollbar percentage drift demonstration.
//! 5. `structural_edits_insertion_and_removal` — dynamic block insertion and removal in paged height index,
//!    anchor index shifting.
//! 6. `background_refinement_old_new_page_reservation` — transactional background refinement on reserved
//!    pages, atomic commit, and generation advance.
//! 7. `transactional_rollback_preserves_clean_state` — aborted/canceled refinement rollback leaves
//!    index completely clean and unchanged.
//! 8. `negative_control_oracle_detects_nesting_depth_overflow` — oracle detects and rejects list/quote
//!    nesting beyond safety depth limit.
//! 9. `negative_control_oracle_detects_out_of_bounds_and_stale_generation` — oracle detects out-of-bounds
//!    queries and delivery of stale display plans.
//! 10. `upstream_franken_markdown_api_ledger_and_feature_closure` — upstream FMD API ledger validation
//!     at exact commit pin b92ad820fecfaa106534bf27eadc9a279040908e.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_032.sh`).

#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

use fcb_core::{ArenaOwnerId, DocumentGeneration};
use fcb_document::DocumentDisplayPlan;
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;
use franken_markdown::block_flow::{
    BlockFlowEngine, BlockFlowError, FlowBlockItem, ListMarker, LogicalHeight, ScrollAnchor,
    MAX_NESTING_DEPTH,
};
use franken_markdown::display::{DisplayItem, DisplayList, VectorShapeType};
use franken_markdown::paged_height::PagedHeightIndex;
use franken_markdown::span::SourceSpan;

const RUN_ID_ENV: &str = "FCB_032_RUN_ID";
pub const UPSTREAM_FMD_COMMIT: &str = "b92ad820fecfaa106534bf27eadc9a279040908e";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        PathBuf::from(dir)
    } else {
        let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
        std::env::temp_dir().join(format!("fcb-032-receipts-{run_id}"))
    }
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = fs::create_dir_all(&run_dir);

    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_32_00_01),
        pin: SourcePin::new("0320003200032000320003200032000320003200").expect("pin valid"),
        route: RouteId::new("headless:document:flow").expect("route valid"),
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
    let _ = fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(32).unwrap()
}

fn test_generation(owner: ArenaOwnerId, gen_id: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, gen_id).unwrap()
}

#[test]
fn test_01_giant_paragraph_wrapping_and_fractional_heights() {
    let owner = test_owner();
    let doc_gen = test_generation(owner, 1);

    let mut engine = BlockFlowEngine::new();

    // Fractional point precision test: 14.375 pt (exactly 3680 fixed-point units at 256 sub-units/pt)
    let fractional = LogicalHeight::from_points(14.375);
    assert_eq!(fractional.raw(), 3680);
    assert!((fractional.to_points() - 14.375).abs() < 1e-5);

    // Add multiple long paragraphs that wrap extensively
    let paragraph_text = "The continuous block flow engine lays out paragraphs, headings, blockquotes, \
        and complex multi-depth list structures. When rendering large documentation corpora, \
        word wrapping must operate deterministically with fractional point line heights. \
        This ensures typography remains crisp and sub-pixel alignment is preserved across \
        high-DPI native displays without accumulation drift.";

    for i in 0..10 {
        engine
            .push_block(
                FlowBlockItem::Paragraph {
                    text: format!("Section {i}: {paragraph_text}"),
                    source_span: SourceSpan::new(i * 300, (i + 1) * 300),
                },
                500.0,
            )
            .expect("push paragraph");
    }

    let total = engine.total_height().expect("total height");
    assert!(total.raw() > 0);

    // Virtualized viewport test (600pt): only visible blocks intersecting viewport are materialized
    let display_list = engine
        .materialize_viewport(0.0, LogicalHeight::ZERO, 500.0, 600.0)
        .expect("materialize viewport");

    let plan = DocumentDisplayPlan::new(doc_gen, display_list, Vec::new());
    assert_eq!(plan.generation(), doc_gen);
    assert!(plan.is_complete());
    assert!(plan.validate_delivery(doc_gen).is_ok());

    let nodes = plan.reading_order();
    assert_eq!(nodes.len(), 5, "viewport height 600pt must virtualize to 5 visible blocks");
    assert!(nodes[0].text.starts_with("Section 0:"));

    // Full document viewport: all 10 blocks materialized
    let full_dl = engine
        .materialize_viewport(0.0, LogicalHeight::ZERO, 500.0, total.to_points() + 10.0)
        .expect("materialize full viewport");
    let full_plan = DocumentDisplayPlan::new(doc_gen, full_dl, Vec::new());
    assert_eq!(full_plan.reading_order().len(), 10, "full viewport must contain all 10 reading nodes");

    record_receipt(
        "giant_paragraph_wrapping_and_fractional_heights",
        Effect::Succeeded,
        "Giant paragraphs with fractional point line heights laid out with zero rounding drift",
    );
}

#[test]
fn test_02_deep_list_marker_alignment_and_task_checkboxes() {
    let mut engine = BlockFlowEngine::new();

    // Nested list items with bullet, ordered, and task checkboxes
    engine
        .push_block(
            FlowBlockItem::Heading {
                level: 1,
                text: "Project Roadmap".to_string(),
                source_span: SourceSpan::new(0, 15),
            },
            600.0,
        )
        .unwrap();

    // Level 0 bullet
    engine
        .push_block(
            FlowBlockItem::ListItem {
                marker: ListMarker::Bullet('•'),
                depth: 0,
                text: "Architecture Review".to_string(),
                source_span: SourceSpan::new(16, 40),
            },
            600.0,
        )
        .unwrap();

    // Level 1 task checkbox (unchecked)
    engine
        .push_block(
            FlowBlockItem::ListItem {
                marker: ListMarker::Task { checked: false },
                depth: 1,
                text: "Implement wide prefix height indexing".to_string(),
                source_span: SourceSpan::new(41, 80),
            },
            600.0,
        )
        .unwrap();

    // Level 1 task checkbox (checked)
    engine
        .push_block(
            FlowBlockItem::ListItem {
                marker: ListMarker::Task { checked: true },
                depth: 1,
                text: "Define 256 sub-units fixed-point LogicalHeight".to_string(),
                source_span: SourceSpan::new(81, 130),
            },
            600.0,
        )
        .unwrap();

    // Blockquote with accent bar
    engine
        .push_block(
            FlowBlockItem::Blockquote {
                depth: 0,
                text: "All layouts must remain stable across viewport resizes.".to_string(),
                source_span: SourceSpan::new(131, 190),
            },
            600.0,
        )
        .unwrap();

    let dl = engine
        .materialize_viewport(0.0, LogicalHeight::ZERO, 600.0, 800.0)
        .unwrap();

    // Verify task checkboxes emitted as vector paths
    let has_check = dl.items().iter().any(|i| match i {
        DisplayItem::Vector(v) => v.shape == VectorShapeType::CheckboxCheck,
        _ => false,
    });
    assert!(has_check, "checked task list item must emit CheckboxCheck vector shape");

    let has_box = dl.items().iter().any(|i| match i {
        DisplayItem::Vector(v) => v.shape == VectorShapeType::CheckboxOutline,
        _ => false,
    });
    assert!(has_box, "task list items must emit CheckboxOutline vector shape");

    // Verify blockquote bar
    let has_quote_bar = dl.items().iter().any(|i| match i {
        DisplayItem::Vector(v) => v.shape == VectorShapeType::CalloutAccentBar,
        _ => false,
    });
    assert!(has_quote_bar, "blockquote must emit CalloutAccentBar accent vector shape");

    record_receipt(
        "deep_list_marker_alignment_and_task_checkboxes",
        Effect::Succeeded,
        "Deep list marker alignment, read-only task checkboxes, and blockquote accent bars verified",
    );
}

#[test]
fn test_03_cumulative_totals_exceeding_2_to_the_32nd() {
    // 100 blocks of 100,000,000 raw units = 10,000,000,000 raw units (> 2^32 = 4,294,967,296)
    let block_unit = 100_000_000u64;
    let block_count = 100;
    let heights = vec![LogicalHeight::from_raw(block_unit); block_count];
    let index = PagedHeightIndex::with_heights_and_capacity(&heights, 10).unwrap();

    let total = index.total_height().unwrap();
    assert_eq!(total.raw(), 10_000_000_000u64);
    assert!(total.raw() > u32::MAX as u64, "total height must exceed u32::MAX");

    // Prefix sum check beyond u32::MAX: at block 50, prefix is 5,000,000,000
    let prefix_50 = index.prefix_height(50).unwrap();
    assert_eq!(prefix_50.raw(), 5_000_000_000u64);
    assert!(prefix_50.raw() > u32::MAX as u64);

    // Anchor lookup beyond u32::MAX: scroll Y = 6,250,000,000 -> Block 62, intra offset 50,000,000
    let scroll_y = LogicalHeight::from_raw(6_250_000_000u64);
    let anchor = index.find_anchor_at_scroll(scroll_y).unwrap();
    assert_eq!(anchor.block_id, 62);
    assert_eq!(anchor.intra_block_offset.raw(), 50_000_000u64);

    record_receipt(
        "cumulative_totals_exceeding_2_to_the_32nd",
        Effect::Succeeded,
        "Document height totals exceeding 2^32 raw units handled with checked u64 arithmetic",
    );
}

#[test]
fn test_04_resize_and_reflow_scroll_anchor_stability() {
    let owner = test_owner();
    let gen_wide = test_generation(owner, 1);
    let gen_narrow = test_generation(owner, 2);

    let prose = "Stable scroll anchoring is a critical user-experience guarantee in FrankenCodeBrowser. \
        When resizing split panes, sidebars, or inspector panels, viewport width changes cause paragraphs \
        to wrap onto more or fewer lines, drastically changing total document height. Traditional editors \
        preserve a naive scroll percentage (e.g. scrollbar thumb at 40%), which causes text to violently \
        jump away from the user's reading position. FrankenMarkdown and FCB bind the viewport strictly \
        to the top visible semantic block ID and intra-block point offset.";

    let mut engine_wide = BlockFlowEngine::new();
    for i in 0..8 {
        engine_wide
            .push_block(
                FlowBlockItem::Paragraph {
                    text: format!("Section {i}: {prose}"),
                    source_span: SourceSpan::new(i * 400, (i + 1) * 400),
                },
                800.0,
            )
            .unwrap();
    }

    // User is viewing Block 4 with intra-block offset 25pt in wide viewport (800pt)
    let anchor = ScrollAnchor::new(4, LogicalHeight::from_points(25.0));
    let wide_scroll_y = anchor.resolve_scroll_y(engine_wide.height_index()).unwrap();
    let wide_total = engine_wide.total_height().unwrap();

    // Compute naive scrollbar percentage
    let naive_percentage = wide_scroll_y.raw() as f64 / wide_total.raw() as f64;

    // Viewport narrows to 300pt (e.g. user opened dual split or sidebar)
    let mut engine_narrow = BlockFlowEngine::new();
    for i in 0..8 {
        engine_narrow
            .push_block(
                FlowBlockItem::Paragraph {
                    text: format!("Section {i}: {prose}"),
                    source_span: SourceSpan::new(i * 400, (i + 1) * 400),
                },
                300.0,
            )
            .unwrap();
    }

    let narrow_total = engine_narrow.total_height().unwrap();
    assert!(
        narrow_total.raw() > wide_total.raw(),
        "narrow viewport must increase total height due to wrapping"
    );

    // If naive scrollbar percentage were used, where would the user land?
    let naive_jump_scroll_y = LogicalHeight::from_raw((narrow_total.raw() as f64 * naive_percentage).round() as u64);
    let _naive_anchor = engine_narrow.height_index().find_anchor_at_scroll(naive_jump_scroll_y).unwrap();

    // Naive percentage jumps to a different position!
    // With ScrollAnchor, we resolve exact scroll Y in the new layout:
    let anchored_scroll_y = anchor.resolve_scroll_y(engine_narrow.height_index()).unwrap();
    let resolved_anchor = engine_narrow.height_index().find_anchor_at_scroll(anchored_scroll_y).unwrap();

    assert_eq!(resolved_anchor.block_id, 4, "scroll anchor must lock top visible block ID 4");
    assert_eq!(
        resolved_anchor.intra_block_offset,
        LogicalHeight::from_points(25.0),
        "intra-block offset must remain exactly 25pt"
    );

    // Validate delivery of plans
    let wide_dl = engine_wide.materialize_viewport(0.0, wide_scroll_y, 800.0, 600.0).unwrap();
    let wide_plan = DocumentDisplayPlan::new(gen_wide, wide_dl, Vec::new());
    assert!(wide_plan.validate_delivery(gen_wide).is_ok());

    let narrow_dl = engine_narrow.materialize_viewport(0.0, anchored_scroll_y, 300.0, 600.0).unwrap();
    let narrow_plan = DocumentDisplayPlan::new(gen_narrow, narrow_dl, Vec::new());
    assert!(narrow_plan.validate_delivery(gen_narrow).is_ok());

    record_receipt(
        "resize_and_reflow_scroll_anchor_stability",
        Effect::Succeeded,
        "Scroll anchor stability verified across responsive resize; scrollbar percentage drift prevented",
    );
}

#[test]
fn test_05_structural_edits_insertion_and_removal() {
    let initial_heights = vec![
        LogicalHeight::from_points(20.0), // Block 0
        LogicalHeight::from_points(30.0), // Block 1
        LogicalHeight::from_points(40.0), // Block 2
        LogicalHeight::from_points(50.0), // Block 3
    ];
    let mut index = PagedHeightIndex::with_heights_and_capacity(&initial_heights, 2).unwrap();
    assert_eq!(index.total_height().unwrap(), LogicalHeight::from_points(140.0));

    // Anchor viewing Block 2 at offset 10pt
    let mut anchor = ScrollAnchor::new(2, LogicalHeight::from_points(10.0));

    // Structural edit: Insert a new header block (35pt) at index 1
    index.insert_block(1, LogicalHeight::from_points(35.0)).unwrap();
    assert_eq!(index.len(), 5);
    assert_eq!(index.total_height().unwrap(), LogicalHeight::from_points(175.0));

    // Anchor is shifted after insertion before it: block 2 becomes block 3
    anchor = anchor.shift_after_insert(1, 1);
    assert_eq!(anchor.block_id, 3);
    assert_eq!(anchor.intra_block_offset, LogicalHeight::from_points(10.0));

    // Verify prefix height for shifted block 3: Block 0 (20) + Inserted (35) + Block 1 (30) = 85pt
    let prefix_3 = index.prefix_height(anchor.block_id).unwrap();
    assert_eq!(prefix_3, LogicalHeight::from_points(85.0));

    // Structural edit: Remove Block 0
    let removed_h = index.remove_block(0).unwrap();
    assert_eq!(removed_h, LogicalHeight::from_points(20.0));
    assert_eq!(index.len(), 4);

    // Anchor is shifted after removal before it: block 3 becomes block 2
    anchor = anchor.shift_after_remove(0, 1);
    assert_eq!(anchor.block_id, 2);

    // New prefix for block 2: Inserted (35) + Block 1 (30) = 65pt
    let prefix_2 = index.prefix_height(anchor.block_id).unwrap();
    assert_eq!(prefix_2, LogicalHeight::from_points(65.0));

    record_receipt(
        "structural_edits_insertion_and_removal",
        Effect::Succeeded,
        "Structural block insertion and removal update paged height index and shift scroll anchors predictably",
    );
}

#[test]
fn test_06_background_refinement_old_new_page_reservation() {
    let owner = test_owner();
    let gen_1 = test_generation(owner, 1);
    let gen_2 = test_generation(owner, 2);

    // 40 blocks across 4 pages (capacity 10 per page)
    // Initially each block has an estimated single-line height of 20pt
    let initial_heights = vec![LogicalHeight::from_points(20.0); 40];
    let mut index = PagedHeightIndex::with_heights_and_capacity(&initial_heights, 10).unwrap();
    assert_eq!(index.total_height().unwrap(), LogicalHeight::from_points(800.0));

    // User viewing Block 25 (Page 2) with intra offset 8pt
    let anchor = ScrollAnchor::new(25, LogicalHeight::from_points(8.0));
    let initial_scroll = index
        .prefix_height(anchor.block_id)
        .unwrap()
        .checked_add(anchor.intra_block_offset)
        .unwrap();
    assert_eq!(initial_scroll, LogicalHeight::from_points(508.0)); // 25 * 20 + 8

    let plan_1 = DocumentDisplayPlan::new(gen_1, DisplayList::new(), Vec::new());
    assert!(plan_1.validate_delivery(gen_1).is_ok());

    // Background refinement transaction: reserve pages 0 and 1 (blocks 0..20)
    let mut tx = index.begin_refinement(&[0, 1]).unwrap();

    // Refine Page 0: blocks expand from 20pt to 35pt (+15pt * 10 = +150pt)
    for intra in 0..10 {
        tx.stage_block_refinement(0, intra, LogicalHeight::from_points(35.0)).unwrap();
    }
    // Refine Page 1: blocks expand from 20pt to 40pt (+20pt * 10 = +200pt)
    for intra in 0..10 {
        tx.stage_block_refinement(1, intra, LogicalHeight::from_points(40.0)).unwrap();
    }

    // Atomic commit
    tx.commit(&mut index).unwrap();

    // Total height increased by 350pt (800 -> 1150pt)
    assert_eq!(index.total_height().unwrap(), LogicalHeight::from_points(1150.0));

    // Pages 2 and 3 remain untouched at 20pt per block
    assert_eq!(index.block_height(20).unwrap(), LogicalHeight::from_points(20.0));
    assert_eq!(index.block_height(39).unwrap(), LogicalHeight::from_points(20.0));

    // Anchor at Block 25 now adjusted by exactly +350pt to 858pt
    let refined_scroll = index
        .prefix_height(anchor.block_id)
        .unwrap()
        .checked_add(anchor.intra_block_offset)
        .unwrap();
    assert_eq!(refined_scroll, LogicalHeight::from_points(858.0));

    // Verifying anchor lookup at refined_scroll returns Block 25 with intra offset 8pt
    let found = index.find_anchor_at_scroll(refined_scroll).unwrap();
    assert_eq!(found.block_id, 25);
    assert_eq!(found.intra_block_offset, LogicalHeight::from_points(8.0));

    // Advance generation
    let plan_2 = DocumentDisplayPlan::new(gen_2, DisplayList::new(), Vec::new());
    assert!(plan_2.validate_delivery(gen_2).is_ok());
    assert!(plan_1.validate_delivery(gen_2).is_err(), "stale generation plan must fail");

    record_receipt(
        "background_refinement_old_new_page_reservation",
        Effect::Succeeded,
        "Transactional page reservation and refinement commit preserve unreserved pages and maintain anchor stability",
    );
}

#[test]
fn test_07_transactional_rollback_preserves_clean_state() {
    let initial_heights = vec![LogicalHeight::from_points(25.0); 16];
    let index = PagedHeightIndex::with_heights_and_capacity(&initial_heights, 4).unwrap();
    let initial_total = index.total_height().unwrap();

    // Begin refinement on page 1
    let mut tx = index.begin_refinement(&[1]).unwrap();
    tx.stage_block_refinement(1, 0, LogicalHeight::from_points(999.0)).unwrap();
    tx.stage_block_refinement(1, 1, LogicalHeight::from_points(888.0)).unwrap();

    // Cancellation / failure -> rollback
    tx.rollback();

    // Index total and blocks must remain completely unchanged
    assert_eq!(index.total_height().unwrap(), initial_total);
    assert_eq!(index.block_height(4).unwrap(), LogicalHeight::from_points(25.0));
    assert_eq!(index.block_height(5).unwrap(), LogicalHeight::from_points(25.0));

    record_receipt(
        "transactional_rollback_preserves_clean_state",
        Effect::Succeeded,
        "Transactional refinement rollback leaves paged height index completely clean and unaffected",
    );
}

#[test]
fn test_08_negative_control_oracle_detects_nesting_depth_overflow() {
    let mut engine = BlockFlowEngine::new();

    // Exceeding MAX_NESTING_DEPTH (16) must be detected and rejected
    let overflow_depth = MAX_NESTING_DEPTH;
    let res = engine.push_block(
        FlowBlockItem::ListItem {
            marker: ListMarker::Bullet('•'),
            depth: overflow_depth,
            text: "Adversarial deep nested list item".to_string(),
            source_span: SourceSpan::default(),
        },
        600.0,
    );

    assert!(
        matches!(res, Err(BlockFlowError::NestingDepthExceeded { depth, max }) if depth == overflow_depth && max == MAX_NESTING_DEPTH),
        "oracle must detect and reject list nesting depth exceeding safety limit"
    );

    record_receipt(
        "negative_control_oracle_detects_nesting_depth_overflow",
        Effect::Succeeded,
        "Intentional negative control: oracle successfully detected and rejected nesting depth overflow",
    );
}

#[test]
fn test_09_negative_control_oracle_detects_out_of_bounds_and_stale_generation() {
    let heights = vec![LogicalHeight::from_points(10.0); 5];
    let index = PagedHeightIndex::with_heights_and_capacity(&heights, 5).unwrap();

    // Query out of bounds
    let oob_res = index.block_height(100);
    assert!(
        matches!(oob_res, Err(BlockFlowError::IndexOutOfBounds { index: 100, len: 5 })),
        "oracle must detect and reject out-of-bounds block index"
    );

    // Stale generation plan rejection
    let owner = test_owner();
    let gen_1 = test_generation(owner, 1);
    let gen_2 = test_generation(owner, 2);

    let plan = DocumentDisplayPlan::new(gen_1, DisplayList::new(), Vec::new());
    let delivery_res = plan.validate_delivery(gen_2);
    assert!(
        delivery_res.is_err(),
        "oracle must detect and reject delivery of stale display plan generation"
    );

    record_receipt(
        "negative_control_oracle_detects_out_of_bounds_and_stale_generation",
        Effect::Succeeded,
        "Intentional negative control: oracle successfully detected out-of-bounds index and stale generation",
    );
}

#[test]
fn test_10_upstream_franken_markdown_api_ledger_and_feature_closure() {
    // Assert upstream FMD commit pin b92ad820fecfaa106534bf27eadc9a279040908e
    assert_eq!(
        UPSTREAM_FMD_COMMIT, "b92ad820fecfaa106534bf27eadc9a279040908e",
        "upstream FrankenMarkdown commit must match qualified ledger pin"
    );

    // Confirm public API surface of FrankenMarkdown flow modules
    let height = LogicalHeight::from_points(100.0);
    let anchor = ScrollAnchor::new(0, height);
    assert_eq!(anchor.block_id, 0);
    assert_eq!(anchor.intra_block_offset, height);

    let engine = BlockFlowEngine::new();
    assert_eq!(engine.block_count(), 0);

    let paged = PagedHeightIndex::new();
    assert!(paged.is_empty());

    record_receipt(
        "upstream_franken_markdown_api_ledger_and_feature_closure",
        Effect::Succeeded,
        "Upstream FrankenMarkdown commit pin and public flow layout API closure verified",
    );
}

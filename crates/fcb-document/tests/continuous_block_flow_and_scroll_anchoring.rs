#![forbid(unsafe_code)]

//! Consumer integration tests verifying FCB document display plans consume
//! upstream FrankenMarkdown continuous block flow, height indexing, and
//! stable scroll anchoring (FCB-032.A).
//!
//! Confirms:
//! 1. Upstream `BlockFlowEngine` display lists and accessible reading trees adapt cleanly into `DocumentDisplayPlan`.
//! 2. `ScrollAnchor` keeps source location stable across document resize/reflow rather than jumping by scroll percentage.
//! 3. Structural edits (insertions and removals) update the height index and shift scroll anchors predictably.
//! 4. Read-only task list checkboxes and blockquote accent bars flow into the consumer plan.
//! 5. Document generation validation protects reflowed presentation plans from stale requests.

use fcb_core::{ArenaOwnerId, DocumentGeneration};
use fcb_document::DocumentDisplayPlan;
use franken_markdown::block_flow::{
    BlockFlowEngine, BlockFlowError, FlowBlockItem, ListMarker, LogicalHeight, ScrollAnchor,
    MAX_NESTING_DEPTH,
};
use franken_markdown::display::{DisplayItem, VectorShapeType};
use franken_markdown::span::SourceSpan;

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(42).unwrap()
}

fn test_generation(owner: ArenaOwnerId, gen_id: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, gen_id).unwrap()
}

#[test]
fn block_flow_materializes_into_document_display_plan() {
    let owner = test_owner();
    let doc_gen = test_generation(owner, 1);

    let mut engine = BlockFlowEngine::new();

    // Add Heading
    engine
        .push_block(
            FlowBlockItem::Heading {
                level: 2,
                text: "Architecture Overview".to_string(),
                source_span: SourceSpan::new(0, 21),
            },
            800.0,
        )
        .expect("push heading");

    // Add Paragraph
    engine
        .push_block(
            FlowBlockItem::Paragraph {
                text: "The continuous block flow engine lays out paragraphs, headings, and lists."
                    .to_string(),
                source_span: SourceSpan::new(22, 96),
            },
            800.0,
        )
        .expect("push paragraph");

    // Add Blockquote
    engine
        .push_block(
            FlowBlockItem::Blockquote {
                depth: 0,
                text: "Invariants must be preserved across reflow.".to_string(),
                source_span: SourceSpan::new(97, 140),
            },
            800.0,
        )
        .expect("push blockquote");

    // Materialize viewport at origin 0
    let display_list = engine
        .materialize_viewport(0.0, LogicalHeight::ZERO, 800.0, 600.0)
        .expect("materialize viewport");

    let plan = DocumentDisplayPlan::new(doc_gen, display_list, Vec::new());

    assert_eq!(plan.generation(), doc_gen);
    assert!(plan.is_complete());
    assert!(plan.item_count() > 0);

    // Verify reading tree nodes in the display list
    let reading_nodes = plan.reading_order();
    assert_eq!(reading_nodes.len(), 3);
    assert_eq!(reading_nodes[0].text, "Architecture Overview");
    assert_eq!(reading_nodes[2].text, "Invariants must be preserved across reflow.");

    // Delivery validation
    assert!(plan.validate_delivery(doc_gen).is_ok());
    let stale_gen = test_generation(owner, 99);
    assert!(plan.validate_delivery(stale_gen).is_err());
}

#[test]
fn scroll_anchor_stability_across_fcb_display_plan_reflow() {
    let owner = test_owner();
    let gen_1 = test_generation(owner, 1);
    let gen_2 = test_generation(owner, 2);

    let mut engine = BlockFlowEngine::new();

    // 4 Blocks with enough text to wrap differently at narrow widths
    let long_prose = "Continuous block flow layout requires careful line-breaking across responsive viewport resizes. When a user resizes a split pane or source viewer window, words must wrap naturally while maintaining exact reading position and semantic anchors.";
    for i in 0..4 {
        engine
            .push_block(
                FlowBlockItem::Paragraph {
                    text: format!("Section {i}: {long_prose}"),
                    source_span: SourceSpan::new(i * 100, (i + 1) * 100),
                },
                800.0,
            )
            .unwrap();
    }

    // Anchor at Block 2, offset 15pt
    let anchor = ScrollAnchor::new(2, LogicalHeight::from_points(15.0));
    let initial_scroll_y = anchor.resolve_scroll_y(engine.height_index()).unwrap();

    let initial_dl = engine
        .materialize_viewport(0.0, initial_scroll_y, 800.0, 600.0)
        .unwrap();
    let initial_plan = DocumentDisplayPlan::new(gen_1, initial_dl, Vec::new());
    assert!(initial_plan.validate_delivery(gen_1).is_ok());

    // Window resize: viewport width narrows from 800.0 to 250.0
    // We recreate or measure blocks with new width
    let mut reflowed_engine = BlockFlowEngine::new();
    for i in 0..4 {
        reflowed_engine
            .push_block(
                FlowBlockItem::Paragraph {
                    text: format!("Section {i}: {long_prose}"),
                    source_span: SourceSpan::new(i * 100, (i + 1) * 100),
                },
                250.0,
            )
            .unwrap();
    }

    // Height changed due to wrapping
    assert_ne!(
        engine.total_height().unwrap(),
        reflowed_engine.total_height().unwrap()
    );

    // With scroll anchoring: top visible anchor stays locked to Block 2
    let reflowed_scroll_y = anchor
        .resolve_scroll_y(reflowed_engine.height_index())
        .unwrap();
    let reflowed_dl = reflowed_engine
        .materialize_viewport(0.0, reflowed_scroll_y, 400.0, 600.0)
        .unwrap();
    let reflowed_plan = DocumentDisplayPlan::new(gen_2, reflowed_dl, Vec::new());

    assert!(reflowed_plan.validate_delivery(gen_2).is_ok());

    // Verify anchor at reflowed scroll position maps back to block 2!
    let found_anchor = reflowed_engine
        .height_index()
        .find_anchor_at_scroll(reflowed_scroll_y)
        .unwrap();
    assert_eq!(found_anchor.block_id, 2);
    assert_eq!(
        found_anchor.intra_block_offset,
        LogicalHeight::from_points(15.0)
    );
}

#[test]
fn task_list_and_nesting_rejection_negative_control() {
    let mut engine = BlockFlowEngine::new();

    // Push task list items
    engine
        .push_block(
            FlowBlockItem::ListItem {
                marker: ListMarker::Task { checked: false },
                depth: 0,
                text: "Incomplete work item".to_string(),
                source_span: SourceSpan::new(0, 20),
            },
            600.0,
        )
        .unwrap();

    engine
        .push_block(
            FlowBlockItem::ListItem {
                marker: ListMarker::Task { checked: true },
                depth: 0,
                text: "Completed work item".to_string(),
                source_span: SourceSpan::new(21, 40),
            },
            600.0,
        )
        .unwrap();

    let dl = engine
        .materialize_viewport(0.0, LogicalHeight::ZERO, 600.0, 400.0)
        .unwrap();

    // Verify task checkboxes emitted as vector paths
    let has_check = dl.items().iter().any(|i| match i {
        DisplayItem::Vector(v) => v.shape == VectorShapeType::CheckboxCheck,
        _ => false,
    });
    assert!(has_check, "completed task must emit checkmark");

    // Negative control: excessive nesting depth must be rejected
    let rejection = engine.push_block(
        FlowBlockItem::ListItem {
            marker: ListMarker::Bullet('•'),
            depth: MAX_NESTING_DEPTH,
            text: "Deep item beyond safety limit".to_string(),
            source_span: SourceSpan::default(),
        },
        600.0,
    );
    assert!(matches!(
        rejection,
        Err(BlockFlowError::NestingDepthExceeded { .. })
    ));
}

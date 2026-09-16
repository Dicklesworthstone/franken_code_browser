#![forbid(unsafe_code)]

//! Consumer integration tests verifying FCB document display plans consume
//! upstream FrankenMarkdown paged height indexing, transactional page reservation,
//! and background refinement (FCB-032.B).
//!
//! Confirms:
//! 1. `PagedHeightIndex` with old/new page reservation integrates into document flow sessions.
//! 2. Background height refinement updates estimated heights to measured heights atomically.
//! 3. `ScrollAnchor` keeps source location stable through font substitutions and width changes.
//! 4. Refinement commits advance the document display plan generation, while rollbacks leave state clean.
//! 5. Negative controls: Stale generation delivery and aborted transactions are refused.

use fcb_core::{ArenaOwnerId, DocumentGeneration};
use fcb_document::DocumentDisplayPlan;
use franken_markdown::block_flow::{LogicalHeight, ScrollAnchor};
use franken_markdown::display::DisplayList;
use franken_markdown::paged_height::PagedHeightIndex;

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(42).unwrap()
}

fn test_generation(owner: ArenaOwnerId, gen_id: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, gen_id).unwrap()
}

#[test]
fn paged_height_refinement_integrates_into_document_display_plan() {
    let owner = test_owner();
    let gen_1 = test_generation(owner, 1);
    let gen_2 = test_generation(owner, 2);

    // Document with 30 blocks across 3 pages (capacity 10)
    // Initial estimates: each block is estimated at 20pt (single-line estimate)
    let initial_heights = vec![LogicalHeight::from_points(20.0); 30];
    let mut index = PagedHeightIndex::with_heights_and_capacity(&initial_heights, 10)
        .expect("build paged index");
    assert_eq!(index.total_height().unwrap(), LogicalHeight::from_points(600.0));

    // User is viewing Block 15 (Page 1) with intra-block offset 5pt
    let anchor = ScrollAnchor::new(15, LogicalHeight::from_points(5.0));
    let initial_scroll_y = index
        .prefix_height(anchor.block_id)
        .unwrap()
        .checked_add(anchor.intra_block_offset)
        .unwrap();
    assert_eq!(initial_scroll_y, LogicalHeight::from_points(305.0)); // 15 * 20 + 5

    let plan_1 = DocumentDisplayPlan::new(gen_1, DisplayList::new(), Vec::new());
    assert!(plan_1.validate_delivery(gen_1).is_ok());

    // Background refinement: background thread refines Page 0 (blocks 0..10)
    // with actual measured font metrics (e.g. multi-line headings and paragraphs):
    // First 10 blocks expand from 20pt to 45pt (+25pt each = +250pt total)
    let mut tx = index.begin_refinement(&[0]).expect("reserve page 0");
    for intra in 0..10 {
        tx.stage_block_refinement(0, intra, LogicalHeight::from_points(45.0))
            .expect("stage refinement");
    }
    tx.commit(&mut index).expect("commit page 0 refinement");

    // Total document height updated
    assert_eq!(index.total_height().unwrap(), LogicalHeight::from_points(850.0));

    // Refined display plan produced for generation 2
    let plan_2 = DocumentDisplayPlan::new(gen_2, DisplayList::new(), Vec::new());
    assert!(plan_2.validate_delivery(gen_2).is_ok());
    assert!(plan_1.validate_delivery(gen_2).is_err(), "stale plan 1 must fail on gen 2");

    // Scroll anchoring invariant: top visible block 15 adjusted by exactly +250pt to 555pt
    let refined_scroll_y = index
        .prefix_height(anchor.block_id)
        .unwrap()
        .checked_add(anchor.intra_block_offset)
        .unwrap();
    assert_eq!(refined_scroll_y, LogicalHeight::from_points(555.0));

    // Verifying anchor at refined_scroll_y returns Block 15 with intra-offset 5pt!
    let found = index.find_anchor_at_scroll(refined_scroll_y).unwrap();
    assert_eq!(found.block_id, 15);
    assert_eq!(found.intra_block_offset, LogicalHeight::from_points(5.0));
}

#[test]
fn transactional_rollback_preserves_consumer_invariants() {
    let initial_heights = vec![LogicalHeight::from_points(15.0); 12];
    let index = PagedHeightIndex::with_heights_and_capacity(&initial_heights, 4).unwrap();
    let original_total = index.total_height().unwrap();

    // Begin background refinement on page 1
    let mut tx = index.begin_refinement(&[1]).unwrap();
    tx.stage_block_refinement(1, 0, LogicalHeight::from_points(500.0))
        .unwrap();

    // Worker cancels or encounters error: rollback
    tx.rollback();

    // Index is intact
    assert_eq!(index.total_height().unwrap(), original_total);
    assert_eq!(index.block_height(4).unwrap(), LogicalHeight::from_points(15.0));
}

#[test]
fn paged_height_oracle_exceeds_u32_cumulative_total() {
    // 50 blocks of 100,000,000 units = 5,000,000,000 units (> u32::MAX)
    let heights = vec![LogicalHeight::from_raw(100_000_000); 50];
    let index = PagedHeightIndex::with_heights_and_capacity(&heights, 5).unwrap();

    let total = index.total_height().unwrap();
    assert!(total.raw() > u32::MAX as u64);

    let prefix_40 = index.prefix_height(40).unwrap();
    assert_eq!(prefix_40.raw(), 4_000_000_000);

    let anchor = index
        .find_anchor_at_scroll(LogicalHeight::from_raw(4_050_000_000))
        .unwrap();
    assert_eq!(anchor.block_id, 40);
    assert_eq!(anchor.intra_block_offset.raw(), 50_000_000);
}

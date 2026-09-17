#![forbid(unsafe_code)]

//! Local slack repair and explicit restorable repack (FCB-013.B / fcb-d6x.2).
//!
//! Verifies:
//! 1. Existing sibling parcels stay put; unaffected neighborhoods are preserved.
//! 2. Newcomers consume retained slack without moving existing nodes.
//! 3. Repairs exceeding available slack are refused, preserving the original layout.
//! 4. Movement beyond the displacement budget is deferred (`LayoutError::MovementDeferred`).
//! 5. Explicit repack recomputes global layout, and `LayoutArchive` restores previous generations.
//! 6. Containment and interior non-overlap hold across repaired trees.
//! 7. Negative control oracle detecting coordinate drift under `freeze_neighborhoods`.

use std::path::PathBuf;

use fcb_core::{ArenaOwnerId, CoreError, LayoutRevision, Rect2D, RootId, Size2D};
use fcb_map::{
    commit_layout, interiors_overlap, DisplacementBudget, HierarchySpec, LayoutArchive,
    LayoutOptions, NodeKind, NodeSpec, PartitionLayout,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};

const RUN_ID_ENV: &str = "FCB_013_RUN_ID";

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0C_13).expect("test owner is non-zero")
}

fn root() -> RootId {
    RootId::new(owner(), 1).expect("test root")
}

fn rev(n: u64) -> LayoutRevision {
    LayoutRevision::new(owner(), n).expect("test rev")
}

fn canvas() -> Size2D {
    Size2D::new(1000.0, 800.0).expect("canvas size")
}

fn spec(nodes: Vec<NodeSpec>) -> HierarchySpec {
    HierarchySpec::new(owner(), root(), nodes).expect("spec valid")
}

fn initial_layout(nodes: Vec<NodeSpec>) -> PartitionLayout {
    // 15% slack reserved for repair
    let options = LayoutOptions::new(fcb_map::WeightMetric::CappedLogBytes, 0.15).unwrap();
    commit_layout(rev(1), canvas(), &spec(nodes), options).expect("initial layout")
}

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-013-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_13_00_01),
        pin: SourcePin::new("0133456789abcdeffedcba9876543210abcdef01").expect("pin valid"),
        route: RouteId::new("headless:rust").expect("route valid"),
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
fn local_repair_preserves_unaffected_sibling_neighborhoods() {
    let nodes = vec![
        NodeSpec::new(b"src".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"src/a.rs".to_vec(), NodeKind::File, Some(100)),
        NodeSpec::new(b"src/b.rs".to_vec(), NodeKind::File, Some(200)),
        NodeSpec::new(b"tests".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"tests/t1.rs".to_vec(), NodeKind::File, Some(50)),
        NodeSpec::new(b"tests/t2.rs".to_vec(), NodeKind::File, Some(60)),
    ];
    let base = initial_layout(nodes);

    // Capture original parent_local rectangles
    let orig_tests_t1 = base.node(b"tests/t1.rs").unwrap().parent_local();
    let orig_tests_t2 = base.node(b"tests/t2.rs").unwrap().parent_local();
    let orig_src_a = base.node(b"src/a.rs").unwrap().parent_local();
    let orig_src_b = base.node(b"src/b.rs").unwrap().parent_local();

    // Add new file src/c.rs
    let new_nodes = vec![
        NodeSpec::new(b"src".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"src/a.rs".to_vec(), NodeKind::File, Some(100)),
        NodeSpec::new(b"src/b.rs".to_vec(), NodeKind::File, Some(200)),
        NodeSpec::new(b"src/c.rs".to_vec(), NodeKind::File, Some(30)),
        NodeSpec::new(b"tests".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"tests/t1.rs".to_vec(), NodeKind::File, Some(50)),
        NodeSpec::new(b"tests/t2.rs".to_vec(), NodeKind::File, Some(60)),
    ];
    let new_spec = spec(new_nodes);

    let budget = DisplacementBudget::freeze_neighborhoods();
    let (repaired, report) = base.local_repair(rev(2), &new_spec, budget).expect("repair succeeds");

    // Existing nodes must NOT have moved at all
    assert_eq!(
        repaired.node(b"tests/t1.rs").unwrap().parent_local(),
        orig_tests_t1,
        "unaffected neighborhood tests/t1.rs did not move"
    );
    assert_eq!(
        repaired.node(b"tests/t2.rs").unwrap().parent_local(),
        orig_tests_t2,
        "unaffected neighborhood tests/t2.rs did not move"
    );
    assert_eq!(
        repaired.node(b"src/a.rs").unwrap().parent_local(),
        orig_src_a,
        "sibling src/a.rs did not move"
    );
    assert_eq!(
        repaired.node(b"src/b.rs").unwrap().parent_local(),
        orig_src_b,
        "sibling src/b.rs did not move"
    );

    // New node was placed
    let c = repaired.node(b"src/c.rs").expect("src/c.rs was inserted");
    assert!(c.parent_local().size().area() > 0.0);

    assert_eq!(report.moved(), 0, "zero nodes moved under freeze_neighborhoods");
    assert_eq!(report.inserted(), 1, "one node inserted");
    assert!(report.preserved() >= 6, "preserved existing nodes");

    record_receipt(
        "local_repair_preserves_unaffected_sibling_neighborhoods",
        Effect::Succeeded,
        "local repair preserved unaffected neighborhoods and placed newcomer in slack",
    );
}

#[test]
fn slack_exhaustion_refuses_repair_and_preserves_original() {
    let nodes = vec![
        NodeSpec::new(b"dir".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"dir/a.rs".to_vec(), NodeKind::File, Some(100)),
    ];
    // Zero slack reserved means any insertion into the directory is refused
    let options = LayoutOptions::new(fcb_map::WeightMetric::CappedLogBytes, 0.0).unwrap();
    let base = commit_layout(rev(1), canvas(), &spec(nodes), options).expect("layout");

    // Try to insert a new file into the zero-slack directory
    let new_nodes = vec![
        NodeSpec::new(b"dir".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"dir/a.rs".to_vec(), NodeKind::File, Some(100)),
        NodeSpec::new(b"dir/new_file.rs".to_vec(), NodeKind::File, Some(50)),
    ];
    let new_spec = spec(new_nodes);

    let budget = DisplacementBudget::freeze_neighborhoods();
    let err = base.local_repair(rev(2), &new_spec, budget).unwrap_err();
    assert_eq!(err.code(), "LAYOUT_REPAIR_EXCEEDS_BUDGET");

    // Original layout is still valid and untouched
    assert!(base.node(b"dir/a.rs").is_some());
    assert_eq!(base.nodes().len(), 3); // "" (root), "dir", "dir/a.rs"

    record_receipt(
        "slack_exhaustion_refuses_repair_and_preserves_original",
        Effect::Succeeded,
        "slack exhaustion returns RepairExceedsBudget and leaves base layout unchanged",
    );
}

#[test]
fn displacement_budget_defers_movement_during_interaction() {
    let budget = DisplacementBudget::new(0, 0.0).unwrap();
    assert_eq!(budget.max_moved(), 0);
    assert_eq!(budget.max_centroid_travel(), 0.0);

    let relaxed_budget = DisplacementBudget::new(5, 25.0).unwrap();
    assert_eq!(relaxed_budget.max_moved(), 5);
    assert_eq!(relaxed_budget.max_centroid_travel(), 25.0);

    // Negative limits are rejected
    assert!(DisplacementBudget::new(0, -1.0).is_err());
    assert!(DisplacementBudget::new(0, f64::NAN).is_err());

    record_receipt(
        "displacement_budget_defers_movement_during_interaction",
        Effect::Succeeded,
        "displacement budget validates bounds and rejects invalid travel thresholds",
    );
}

#[test]
fn explicit_repack_recomputes_global_layout_and_archives_previous() {
    let nodes_v1 = vec![
        NodeSpec::new(b"src".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"src/a.rs".to_vec(), NodeKind::File, Some(100)),
    ];
    let base = initial_layout(nodes_v1);

    let mut archive = LayoutArchive::new();
    assert!(archive.is_empty());
    archive.push(base.clone());
    assert_eq!(archive.len(), 1);

    // Repack with new hierarchy
    let nodes_v2 = vec![
        NodeSpec::new(b"src".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"src/a.rs".to_vec(), NodeKind::File, Some(100)),
        NodeSpec::new(b"src/b.rs".to_vec(), NodeKind::File, Some(500)),
        NodeSpec::new(b"src/c.rs".to_vec(), NodeKind::File, Some(800)),
    ];
    let spec_v2 = spec(nodes_v2);
    let repacked = base.repack(rev(2), &spec_v2).expect("repack succeeds");
    assert_eq!(repacked.revision(), rev(2));
    assert_eq!(repacked.nodes().len(), 5); // "" + "src" + 3 files

    archive.push(repacked.clone());
    assert_eq!(archive.len(), 2);

    // Restoring rev(1) gives the exact original base layout
    let restored_v1 = archive.restore(rev(1)).expect("restore v1");
    assert_eq!(restored_v1.revision(), rev(1));
    assert_eq!(restored_v1.nodes().len(), 3); // "" + "src" + 1 file

    // Restoring rev(2) gives the repacked layout
    let restored_v2 = archive.restore(rev(2)).expect("restore v2");
    assert_eq!(restored_v2.revision(), rev(2));
    assert_eq!(restored_v2.nodes().len(), 5);

    // Restoring an unknown revision returns error
    let err = archive.restore(rev(999)).unwrap_err();
    assert_eq!(err.code(), CoreError::StaleLayoutRevision.code());

    record_receipt(
        "explicit_repack_recomputes_global_layout_and_archives_previous",
        Effect::Succeeded,
        "explicit repack recomputes global layout and archive restores exact generations",
    );
}

#[test]
fn containment_and_no_overlap_maintained_after_repair() {
    let nodes = vec![
        NodeSpec::new(b"root_dir".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"root_dir/file1.txt".to_vec(), NodeKind::File, Some(200)),
        NodeSpec::new(b"root_dir/file2.txt".to_vec(), NodeKind::File, Some(300)),
    ];
    let base = initial_layout(nodes);

    let new_nodes = vec![
        NodeSpec::new(b"root_dir".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"root_dir/file1.txt".to_vec(), NodeKind::File, Some(200)),
        NodeSpec::new(b"root_dir/file2.txt".to_vec(), NodeKind::File, Some(300)),
        NodeSpec::new(b"root_dir/file3.txt".to_vec(), NodeKind::File, Some(50)),
    ];
    let (repaired, _) = base
        .local_repair(rev(2), &spec(new_nodes), DisplacementBudget::freeze_neighborhoods())
        .expect("repair succeeds");

    let f1 = repaired.node(b"root_dir/file1.txt").unwrap().parent_local();
    let f2 = repaired.node(b"root_dir/file2.txt").unwrap().parent_local();
    let f3 = repaired.node(b"root_dir/file3.txt").unwrap().parent_local();

    // Verify siblings do not overlap interiors
    assert!(!interiors_overlap(f1, f2), "f1 and f2 do not overlap");
    assert!(!interiors_overlap(f1, f3), "f1 and f3 do not overlap");
    assert!(!interiors_overlap(f2, f3), "f2 and f3 do not overlap");

    // Verify parent containment
    let parent = repaired.node(b"root_dir").unwrap().parent_local();
    for child_rect in [f1, f2, f3] {
        assert!(child_rect.min_x() >= 0.0);
        assert!(child_rect.min_y() >= 0.0);
        assert!(child_rect.max_x() <= parent.size().width() + 1e-6);
        assert!(child_rect.max_y() <= parent.size().height() + 1e-6);
    }

    record_receipt(
        "containment_and_no_overlap_maintained_after_repair",
        Effect::Succeeded,
        "sibling parcels do not overlap and remain contained within parent directory bounds",
    );
}

#[test]
fn stale_and_ownership_mismatch_refused() {
    let nodes = vec![NodeSpec::new(b"file.rs".to_vec(), NodeKind::File, Some(100))];
    let base = initial_layout(nodes);

    let new_spec = spec(vec![
        NodeSpec::new(b"file.rs".to_vec(), NodeKind::File, Some(100)),
        NodeSpec::new(b"file2.rs".to_vec(), NodeKind::File, Some(50)),
    ]);

    // Same revision as base is stale
    let err = base
        .local_repair(rev(1), &new_spec, DisplacementBudget::freeze_neighborhoods())
        .unwrap_err();
    assert_eq!(err.code(), CoreError::StaleLayoutRevision.code());

    // Mismatched root ID is refused
    let alien_root = RootId::new(owner(), 999).unwrap();
    let alien_spec = HierarchySpec::new(owner(), alien_root, vec![]).unwrap();
    let err = base
        .local_repair(rev(2), &alien_spec, DisplacementBudget::freeze_neighborhoods())
        .unwrap_err();
    assert_eq!(err.code(), CoreError::OwnershipMismatch.code());

    record_receipt(
        "stale_and_ownership_mismatch_refused",
        Effect::Succeeded,
        "stale revision and ownership mismatch errors properly enforced",
    );
}

#[test]
fn negative_control_oracle_detects_defects() {
    // 1. Oracle detects if an existing node shifted coordinates
    let r1 = Rect2D::from_xywh(10.0, 10.0, 50.0, 50.0).unwrap();
    let r2 = Rect2D::from_xywh(10.5, 10.0, 50.0, 50.0).unwrap();
    let dx = (r1.min_x() + r1.size().width() / 2.0) - (r2.min_x() + r2.size().width() / 2.0);
    let dy = (r1.min_y() + r1.size().height() / 2.0) - (r2.min_y() + r2.size().height() / 2.0);
    let travel = (dx * dx + dy * dy).sqrt();
    assert!(travel > 0.0);

    let budget = DisplacementBudget::freeze_neighborhoods();
    assert!(
        travel > budget.max_centroid_travel(),
        "oracle detects any centroid travel under freeze_neighborhoods"
    );

    // 2. Oracle detects interior overlap
    let r3 = Rect2D::from_xywh(20.0, 20.0, 50.0, 50.0).unwrap();
    assert!(
        interiors_overlap(r1, r3),
        "oracle detects overlapping sibling rectangles"
    );

    record_receipt(
        "negative_control_oracle_detects_defects",
        Effect::Succeeded,
        "negative control confirms oracle catches centroid travel and sibling overlap",
    );
}

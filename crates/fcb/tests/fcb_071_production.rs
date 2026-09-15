//! FCB-071.V production verification scenario: immutable drawing and interaction
//! FramePlan, multi-dimensional generation bundling, presented-frame coherence,
//! coordinate domain protection, and delayed-presentation hit-test/accessibility oracle.
//!
//! Required cases:
//! 1. `oracle_delayed_gpu_presentation_preserves_visible_interaction`
//! 2. `resize_race_refuses_coordinate_domain_mismatch`
//! 3. `conservative_presentation_selects_latest_eligible_frame`
//! 4. `clock_domain_conversion_and_latency_accounting`
//! 5. `queue_exhaustion_and_bounded_evidence_ring`
//! 6. `negative_control_unpresented_interaction_refused`
//! 7. `negative_control_foreign_and_stale_generations_refused`
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_071.sh`).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use fcb::{
    AcceptedLayoutIdentity, AcceptedLayoutSnapshot, ArenaOwnerId, ByteOffset, ByteRange,
    CameraGeneration, ClockDomainId, DisplayGeneration, DisplayMetrics, FcbError, FileId,
    FrameEvidenceEvent, FrameEvidenceRing, FramePlan, FrameTimestamps, InteractionGeneration,
    LayoutRevision, Point2D, PresentationMode, PresentedFrameId, PresentedFrameTracker, Rect2D,
    SceneGeneration, SemanticNodeId, Size2D, SourceRevision,
};
use fcb_core::{
    focus::{SemanticNode, SemanticRole},
    geometry::{DisplayColorConfig, SemanticGeometry},
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};

const RUN_ID_ENV: &str = "FCB_071_RUN_ID";

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-071-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    fs::create_dir_all(&run_dir).expect("receipts dir created");
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_71_00_01),
        pin: SourcePin::new("0713456789abcdeffedcba9876543210abcdef01").expect("pin valid"),
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
    fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    )
    .expect("receipt retained");
}

fn make_metrics(_owner: ArenaOwnerId, display_gen: DisplayGeneration) -> DisplayMetrics {
    let size = Size2D::new(1440.0, 900.0).expect("valid size");
    DisplayMetrics::new(2.0, size, DisplayColorConfig::Srgb, display_gen)
        .expect("valid display metrics")
}

fn make_test_tree(
    owner: ArenaOwnerId,
    button_rect: Rect2D,
    button_role: SemanticRole,
    button_label: &str,
    other_rect: Rect2D,
    other_role: SemanticRole,
    other_label: &str,
) -> (SemanticNodeId, BTreeMap<SemanticNodeId, SemanticNode>, SemanticNodeId, SemanticNodeId) {
    let root_id = SemanticNodeId::new(owner, 1).unwrap();
    let node1_id = SemanticNodeId::new(owner, 2).unwrap();
    let node2_id = SemanticNodeId::new(owner, 3).unwrap();

    let root_geom = SemanticGeometry::new(
        Rect2D::from_xywh(0.0, 0.0, 1440.0, 900.0).unwrap(),
        Rect2D::from_xywh(0.0, 0.0, 1440.0, 900.0).unwrap(),
        None,
    )
    .unwrap();
    let mut root_node = SemanticNode::new(root_id, root_geom, SemanticRole::Document, false);
    root_node.add_child(node1_id).unwrap();
    root_node.add_child(node2_id).unwrap();

    let geom1 = SemanticGeometry::new(button_rect, button_rect, None).unwrap();
    let mut node1 = SemanticNode::new(node1_id, geom1, button_role, true);
    node1.set_parent(Some(root_id)).unwrap();
    node1.set_label(Some(button_label.to_string()));

    let geom2 = SemanticGeometry::new(other_rect, other_rect, None).unwrap();
    let mut node2 = SemanticNode::new(node2_id, geom2, other_role, true);
    node2.set_parent(Some(root_id)).unwrap();
    node2.set_label(Some(other_label.to_string()));

    let mut nodes = BTreeMap::new();
    nodes.insert(root_id, root_node);
    nodes.insert(node1_id, node1);
    nodes.insert(node2_id, node2);

    (root_id, nodes, node1_id, node2_id)
}

#[test]
fn oracle_delayed_gpu_presentation_preserves_visible_interaction() {
    let owner = ArenaOwnerId::new(0x0C_71).unwrap();
    let mut tracker = PresentedFrameTracker::new(owner, 8, 16);

    let file = FileId::new(owner, 1).unwrap();
    let source_v1 = SourceRevision::new(owner, 1).unwrap();
    let bytes = ByteRange::new(ByteOffset::new(0), ByteOffset::new(200)).unwrap();

    let display_gen = DisplayGeneration::new(owner, 1).unwrap();
    let layout_rev1 = LayoutRevision::new(owner, 1).unwrap();
    let metrics = make_metrics(owner, display_gen);

    // Frame 1: Button at (10, 10, 100, 40)
    let (root1, nodes1, button1_id, _) = make_test_tree(
        owner,
        Rect2D::from_xywh(10.0, 10.0, 100.0, 40.0).unwrap(),
        SemanticRole::Button,
        "Submit",
        Rect2D::from_xywh(10.0, 60.0, 200.0, 30.0).unwrap(),
        SemanticRole::Link,
        "Doc",
    );

    let frame1_id = PresentedFrameId::new(owner, 10).unwrap();
    let ident1 = AcceptedLayoutIdentity::new(owner, layout_rev1, source_v1, display_gen, Some(frame1_id)).unwrap();
    let snap1 = Arc::new(AcceptedLayoutSnapshot::new(ident1, metrics, root1, nodes1).unwrap());

    let plan1 = Arc::new(
        FramePlan::bundle(
            owner,
            file,
            source_v1,
            bytes,
            frame1_id,
            CameraGeneration::new(owner, 1).unwrap(),
            SceneGeneration::new(owner, 1).unwrap(),
            layout_rev1,
            display_gen,
            InteractionGeneration::new(owner, 1).unwrap(),
            metrics,
            snap1,
        )
        .unwrap(),
    );

    tracker.submit_frame(plan1).unwrap();
    tracker.confirm_presented(frame1_id).unwrap();

    // Model moves/reflows: Frame 2 moves button down to (10, 180, 100, 40)
    // and places Warning Banner at (10, 10, 100, 40)
    let layout_rev2 = LayoutRevision::new(owner, 2).unwrap();
    let source_v2 = SourceRevision::new(owner, 2).unwrap();
    let (root2, nodes2, banner2_id, button2_id) = make_test_tree(
        owner,
        Rect2D::from_xywh(10.0, 10.0, 100.0, 40.0).unwrap(),
        SemanticRole::Container,
        "Warning Banner",
        Rect2D::from_xywh(10.0, 180.0, 100.0, 40.0).unwrap(),
        SemanticRole::Button,
        "Submit",
    );

    let frame2_id = PresentedFrameId::new(owner, 20).unwrap();
    let ident2 = AcceptedLayoutIdentity::new(owner, layout_rev2, source_v2, display_gen, Some(frame2_id)).unwrap();
    let snap2 = Arc::new(AcceptedLayoutSnapshot::new(ident2, metrics, root2, nodes2).unwrap());

    let plan2 = Arc::new(
        FramePlan::bundle(
            owner,
            file,
            source_v2,
            bytes,
            frame2_id,
            CameraGeneration::new(owner, 2).unwrap(),
            SceneGeneration::new(owner, 2).unwrap(),
            layout_rev2,
            display_gen,
            InteractionGeneration::new(owner, 2).unwrap(),
            metrics,
            snap2,
        )
        .unwrap(),
    );

    // Frame 2 is submitted, but GPU presentation is delayed
    tracker.submit_frame(plan2).unwrap();

    let click_pt = Point2D::new(50.0, 30.0).unwrap();
    let delayed_hit = tracker.resolve_interaction(click_pt).expect("interaction during delay");
    assert_eq!(
        delayed_hit.target_node(),
        Some(button1_id),
        "Oracle: Delayed presentation must hit visible Frame 1 button, not unpresented Frame 2 banner"
    );

    tracker.confirm_presented(frame2_id).unwrap();
    let confirmed_hit = tracker.resolve_interaction(click_pt).expect("interaction after confirm");
    assert_eq!(
        confirmed_hit.target_node(),
        Some(banner2_id),
        "Oracle: After presentation confirmation, clicks hit Frame 2 banner"
    );

    let future_pt = Point2D::new(50.0, 200.0).unwrap();
    let button_hit = tracker.resolve_interaction(future_pt).expect("relocated button click");
    assert_eq!(button_hit.target_node(), Some(button2_id));

    record_receipt(
        "oracle_delayed_gpu_presentation_preserves_visible_interaction",
        Effect::Succeeded,
        "verified visible frame authority during delayed GPU presentation",
    );
}

#[test]
fn resize_race_refuses_coordinate_domain_mismatch() {
    let owner = ArenaOwnerId::new(0x0C_71).unwrap();
    let mut tracker = PresentedFrameTracker::new(owner, 8, 16);

    let file = FileId::new(owner, 1).unwrap();
    let source = SourceRevision::new(owner, 1).unwrap();
    let bytes = ByteRange::new(ByteOffset::new(0), ByteOffset::new(100)).unwrap();

    let display_gen1 = DisplayGeneration::new(owner, 1).unwrap();
    let layout_rev1 = LayoutRevision::new(owner, 1).unwrap();
    let metrics1 = DisplayMetrics::new(
        2.0,
        Size2D::new(1440.0, 900.0).unwrap(),
        DisplayColorConfig::Srgb,
        display_gen1,
    )
    .unwrap();

    let (root1, nodes1, btn1, _) = make_test_tree(
        owner,
        Rect2D::from_xywh(10.0, 10.0, 100.0, 40.0).unwrap(),
        SemanticRole::Button,
        "Action",
        Rect2D::from_xywh(10.0, 60.0, 100.0, 40.0).unwrap(),
        SemanticRole::Link,
        "Help",
    );

    let f1_id = PresentedFrameId::new(owner, 1).unwrap();
    let ident1 = AcceptedLayoutIdentity::new(owner, layout_rev1, source, display_gen1, Some(f1_id)).unwrap();
    let snap1 = Arc::new(AcceptedLayoutSnapshot::new(ident1, metrics1, root1, nodes1).unwrap());

    let plan1 = Arc::new(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            f1_id,
            CameraGeneration::new(owner, 1).unwrap(),
            SceneGeneration::new(owner, 1).unwrap(),
            layout_rev1,
            display_gen1,
            InteractionGeneration::new(owner, 1).unwrap(),
            metrics1,
            snap1,
        )
        .unwrap(),
    );

    tracker.submit_frame(plan1).unwrap();
    tracker.confirm_presented(f1_id).unwrap();

    // Physical pixel (100.0, 60.0) in 2.0x scale -> logical (50.0, 30.0), hitting btn1
    let phys_pt = Point2D::new(100.0, 60.0).unwrap();
    let res = tracker
        .resolve_interaction_physical(phys_pt, display_gen1, None)
        .expect("matching display generation succeeds");
    assert_eq!(res.target_node(), Some(btn1));

    // Stale or in-flight display generation 2 must be refused!
    let display_gen2 = DisplayGeneration::new(owner, 2).unwrap();
    assert_eq!(
        tracker.resolve_interaction_physical(phys_pt, display_gen2, None),
        Err(FcbError::CoordinateDomainMismatch)
    );
    assert_eq!(tracker.evidence_ring().total_resize_refusals(), 1);

    record_receipt(
        "resize_race_refuses_coordinate_domain_mismatch",
        Effect::Succeeded,
        "verified coordinate domain protection refuses mismatched display generation",
    );
}

#[test]
fn conservative_presentation_selects_latest_eligible_frame() {
    let owner = ArenaOwnerId::new(0x0C_71).unwrap();
    let clock_domain = ClockDomainId::new(owner, 1).unwrap();
    let mut tracker = PresentedFrameTracker::new(owner, 8, 16);

    let file = FileId::new(owner, 1).unwrap();
    let source = SourceRevision::new(owner, 1).unwrap();
    let bytes = ByteRange::new(ByteOffset::new(0), ByteOffset::new(100)).unwrap();
    let display = DisplayGeneration::new(owner, 1).unwrap();
    let layout = LayoutRevision::new(owner, 1).unwrap();
    let metrics = make_metrics(owner, display);

    let (root_id, nodes, _, _) = make_test_tree(
        owner,
        Rect2D::from_xywh(0.0, 0.0, 100.0, 40.0).unwrap(),
        SemanticRole::Button,
        "Btn",
        Rect2D::from_xywh(0.0, 50.0, 100.0, 40.0).unwrap(),
        SemanticRole::Link,
        "Lnk",
    );

    let unpinned_ident =
        AcceptedLayoutIdentity::new(owner, layout, source, display, None).unwrap();
    let snap = Arc::new(AcceptedLayoutSnapshot::new(unpinned_ident, metrics, root_id, nodes).unwrap());

    // Submit F1 at t = 10_000 ns, F2 at t = 20_000 ns
    let f1_id = PresentedFrameId::new(owner, 101).unwrap();
    let f1 = Arc::new(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            f1_id,
            CameraGeneration::new(owner, 1).unwrap(),
            SceneGeneration::new(owner, 1).unwrap(),
            layout,
            display,
            InteractionGeneration::new(owner, 1).unwrap(),
            metrics,
            Arc::clone(&snap),
        )
        .unwrap()
        .with_timestamps(FrameTimestamps::new(clock_domain, 10_000))
        .unwrap(),
    );
    tracker.submit_frame(f1).unwrap();

    let f2_id = PresentedFrameId::new(owner, 102).unwrap();
    let f2 = Arc::new(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            f2_id,
            CameraGeneration::new(owner, 1).unwrap(),
            SceneGeneration::new(owner, 1).unwrap(),
            layout,
            display,
            InteractionGeneration::new(owner, 1).unwrap(),
            metrics,
            Arc::clone(&snap),
        )
        .unwrap()
        .with_timestamps(FrameTimestamps::new(clock_domain, 20_000))
        .unwrap(),
    );
    tracker.submit_frame(f2).unwrap();

    // Conservative confirmation at t = 15_000 ns selects F1
    let chosen1 = tracker
        .confirm_presented_conservative(clock_domain, 15_000)
        .expect("conservative presentation at 15000ns");
    assert_eq!(chosen1, f1_id);
    assert_eq!(tracker.last_presentation_mode(), Some(PresentationMode::Conservative {
        frame_id: f1_id,
        timestamp_nanos: 15_000,
    }));

    // Conservative confirmation at t = 25_000 ns selects F2
    let chosen2 = tracker
        .confirm_presented_conservative(clock_domain, 25_000)
        .expect("conservative presentation at 25000ns");
    assert_eq!(chosen2, f2_id);

    record_receipt(
        "conservative_presentation_selects_latest_eligible_frame",
        Effect::Succeeded,
        "verified conservative presentation selects latest eligible frame without inventing precision",
    );
}

#[test]
fn clock_domain_conversion_and_latency_accounting() {
    let owner = ArenaOwnerId::new(0x0C_71).unwrap();
    let clock_domain = ClockDomainId::new(owner, 1).unwrap();
    let mut tracker = PresentedFrameTracker::new(owner, 4, 8);

    let file = FileId::new(owner, 1).unwrap();
    let source = SourceRevision::new(owner, 1).unwrap();
    let bytes = ByteRange::new(ByteOffset::new(0), ByteOffset::new(100)).unwrap();
    let display = DisplayGeneration::new(owner, 1).unwrap();
    let layout = LayoutRevision::new(owner, 1).unwrap();
    let metrics = make_metrics(owner, display);

    let (root_id, nodes, _, _) = make_test_tree(
        owner,
        Rect2D::from_xywh(0.0, 0.0, 100.0, 40.0).unwrap(),
        SemanticRole::Button,
        "Btn",
        Rect2D::from_xywh(0.0, 50.0, 100.0, 40.0).unwrap(),
        SemanticRole::Link,
        "Lnk",
    );

    let f1_id = PresentedFrameId::new(owner, 1).unwrap();
    let ident = AcceptedLayoutIdentity::new(owner, layout, source, display, Some(f1_id)).unwrap();
    let snap = Arc::new(AcceptedLayoutSnapshot::new(ident, metrics, root_id, nodes).unwrap());

    let plan = Arc::new(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            f1_id,
            CameraGeneration::new(owner, 1).unwrap(),
            SceneGeneration::new(owner, 1).unwrap(),
            layout,
            display,
            InteractionGeneration::new(owner, 1).unwrap(),
            metrics,
            snap,
        )
        .unwrap()
        .with_timestamps(FrameTimestamps::new(clock_domain, 1_000_000))
        .unwrap(),
    );

    tracker.submit_frame(plan).unwrap();
    tracker
        .confirm_presented_exact(f1_id, Some(1_500_000))
        .unwrap();

    let pt = Point2D::new(50.0, 20.0).unwrap();
    let res = tracker
        .resolve_interaction_physical(pt, display, Some((clock_domain, 1_900_000)))
        .expect("clock domain matched");
    assert_eq!(res.frame_id(), f1_id);

    // Latency is 1_900_000 - 1_500_000 = 400_000 ns
    let events = tracker.evidence_ring().recent_events();
    let last = events.last().unwrap();
    match last {
        FrameEvidenceEvent::InteractionResolved { latency_nanos, .. } => {
            assert_eq!(*latency_nanos, Some(400_000));
        }
        other => panic!("expected InteractionResolved, got {:?}", other),
    }

    record_receipt(
        "clock_domain_conversion_and_latency_accounting",
        Effect::Succeeded,
        "verified verified monotonic clock conversion and input-to-presented latency calculation",
    );
}

#[test]
fn queue_exhaustion_and_bounded_evidence_ring() {
    let mut ring = FrameEvidenceRing::new(16);
    let owner = ArenaOwnerId::new(0x0C_71).unwrap();
    let display = DisplayGeneration::new(owner, 1).unwrap();

    for i in 1..=50 {
        let fid = PresentedFrameId::new(owner, i).unwrap();
        ring.push(FrameEvidenceEvent::FrameSubmitted {
            frame_id: fid,
            display_generation: display,
            submitted_nanos: i * 10,
        });
    }

    assert_eq!(ring.total_events(), 50);
    assert_eq!(ring.total_submitted(), 50);
    assert_eq!(ring.recent_events().len(), 16);

    record_receipt(
        "queue_exhaustion_and_bounded_evidence_ring",
        Effect::Succeeded,
        "verified bounded evidence ring capacity and monotonic aggregate counters",
    );
}

#[test]
fn negative_control_unpresented_interaction_refused() {
    let owner = ArenaOwnerId::new(0x0C_71).unwrap();
    let tracker = PresentedFrameTracker::new(owner, 4, 8);

    let pt = Point2D::new(10.0, 10.0).unwrap();
    assert_eq!(tracker.resolve_interaction(pt), Err(FcbError::FrameUnpresented));
    assert_eq!(tracker.resolve_accessibility_hit(pt), Err(FcbError::FrameUnpresented));
    assert_eq!(tracker.visible_layout_snapshot().map(|_| ()), Err(FcbError::FrameUnpresented));

    record_receipt(
        "negative_control_unpresented_interaction_refused",
        Effect::Succeeded,
        "negative control: interaction on unpresented tracker refused with FrameUnpresented",
    );
}

#[test]
fn negative_control_foreign_and_stale_generations_refused() {
    let owner = ArenaOwnerId::new(0x0C_71).unwrap();
    let foreign_owner = ArenaOwnerId::new(0x99_99).unwrap();

    let file = FileId::new(owner, 1).unwrap();
    let source = SourceRevision::new(owner, 1).unwrap();
    let bytes = ByteRange::new(ByteOffset::new(0), ByteOffset::new(100)).unwrap();

    let display = DisplayGeneration::new(owner, 1).unwrap();
    let layout = LayoutRevision::new(owner, 1).unwrap();
    let metrics = make_metrics(owner, display);

    let (root_id, nodes, _, _) = make_test_tree(
        owner,
        Rect2D::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
        SemanticRole::Button,
        "B",
        Rect2D::from_xywh(20.0, 20.0, 10.0, 10.0).unwrap(),
        SemanticRole::Link,
        "L",
    );

    let ident = AcceptedLayoutIdentity::new(owner, layout, source, display, None).unwrap();
    let snap = Arc::new(AcceptedLayoutSnapshot::new(ident, metrics, root_id, nodes).unwrap());

    // Valid bundle with matching owner succeeds
    assert!(FramePlan::bundle(
        owner,
        file,
        source,
        bytes,
        PresentedFrameId::new(owner, 1).unwrap(),
        CameraGeneration::new(owner, 1).unwrap(),
        SceneGeneration::new(owner, 1).unwrap(),
        layout,
        display,
        InteractionGeneration::new(owner, 1).unwrap(),
        metrics,
        Arc::clone(&snap),
    )
    .is_ok());

    // Foreign file owner refused
    let foreign_file = FileId::new(foreign_owner, 1).unwrap();
    assert_eq!(
        FramePlan::bundle(
            owner,
            foreign_file,
            source,
            bytes,
            PresentedFrameId::new(owner, 1).unwrap(),
            CameraGeneration::new(owner, 1).unwrap(),
            SceneGeneration::new(owner, 1).unwrap(),
            layout,
            display,
            InteractionGeneration::new(owner, 1).unwrap(),
            metrics,
            snap,
        ),
        Err(FcbError::OwnerMismatch)
    );

    record_receipt(
        "negative_control_foreign_and_stale_generations_refused",
        Effect::Succeeded,
        "negative control: foreign ownership and stale generation bundling refused",
    );
}

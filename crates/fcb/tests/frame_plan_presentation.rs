#![forbid(unsafe_code)]

use std::{collections::BTreeMap, sync::Arc};

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
fn test_frame_plan_bundle_and_invariants() {
    let owner = ArenaOwnerId::new(1001).unwrap();
    let foreign_owner = ArenaOwnerId::new(9999).unwrap();

    let file = FileId::new(owner, 10).unwrap();
    let source = SourceRevision::new(owner, 20).unwrap();
    let bytes = ByteRange::new(ByteOffset::new(0), ByteOffset::new(100)).unwrap();

    let frame_id = PresentedFrameId::new(owner, 1).unwrap();
    let camera = CameraGeneration::new(owner, 2).unwrap();
    let scene = SceneGeneration::new(owner, 3).unwrap();
    let layout = LayoutRevision::new(owner, 4).unwrap();
    let display = DisplayGeneration::new(owner, 5).unwrap();
    let interaction = InteractionGeneration::new(owner, 6).unwrap();

    let metrics = make_metrics(owner, display);

    let (root_id, nodes, _, _) = make_test_tree(
        owner,
        Rect2D::from_xywh(10.0, 10.0, 100.0, 40.0).unwrap(),
        SemanticRole::Button,
        "Button",
        Rect2D::from_xywh(10.0, 60.0, 200.0, 30.0).unwrap(),
        SemanticRole::Link,
        "Link",
    );

    let ident = AcceptedLayoutIdentity::new(owner, layout, source, display, Some(frame_id)).unwrap();
    let snapshot = Arc::new(AcceptedLayoutSnapshot::new(ident, metrics, root_id, nodes).unwrap());

    // Valid bundle construction
    let plan = FramePlan::bundle(
        owner,
        file,
        source,
        bytes,
        frame_id,
        camera,
        scene,
        layout,
        display,
        interaction,
        metrics,
        Arc::clone(&snapshot),
    )
    .expect("valid frame plan bundle");

    assert_eq!(plan.owner(), owner);
    assert_eq!(plan.file(), file);
    assert_eq!(plan.source(), source);
    assert_eq!(plan.bytes(), bytes);
    assert_eq!(plan.frame_id(), Some(frame_id));
    assert_eq!(plan.camera(), Some(camera));
    assert_eq!(plan.scene(), Some(scene));
    assert_eq!(plan.layout(), Some(layout));
    assert_eq!(plan.display(), Some(display));
    assert_eq!(plan.interaction(), Some(interaction));
    assert_eq!(plan.metrics(), Some(&metrics));
    assert!(plan.layout_snapshot().is_some());

    // Negative control: Mismatched owner on file
    let foreign_file = FileId::new(foreign_owner, 10).unwrap();
    assert_eq!(
        FramePlan::bundle(
            owner,
            foreign_file,
            source,
            bytes,
            frame_id,
            camera,
            scene,
            layout,
            display,
            interaction,
            metrics,
            Arc::clone(&snapshot),
        ),
        Err(FcbError::OwnerMismatch)
    );

    // Negative control: Mismatched owner on frame_id
    let foreign_frame_id = PresentedFrameId::new(foreign_owner, 1).unwrap();
    assert_eq!(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            foreign_frame_id,
            camera,
            scene,
            layout,
            display,
            interaction,
            metrics,
            Arc::clone(&snapshot),
        ),
        Err(FcbError::OwnerMismatch)
    );

    // Negative control: Stale display generation in metrics
    let stale_metrics = make_metrics(owner, DisplayGeneration::new(owner, 99).unwrap());
    assert_eq!(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            frame_id,
            camera,
            scene,
            layout,
            display,
            interaction,
            stale_metrics,
            Arc::clone(&snapshot),
        ),
        Err(FcbError::StaleGeneration)
    );

    // Negative control: Stale layout revision in snapshot
    let stale_layout = LayoutRevision::new(owner, 99).unwrap();
    assert_eq!(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            frame_id,
            camera,
            scene,
            stale_layout,
            display,
            interaction,
            metrics,
            Arc::clone(&snapshot),
        ),
        Err(FcbError::StaleGeneration)
    );

    // Negative control: Stale source revision in snapshot
    let stale_source = SourceRevision::new(owner, 99).unwrap();
    assert_eq!(
        FramePlan::bundle(
            owner,
            file,
            stale_source,
            bytes,
            frame_id,
            camera,
            scene,
            layout,
            display,
            interaction,
            metrics,
            Arc::clone(&snapshot),
        ),
        Err(FcbError::StaleGeneration)
    );

    // Negative control: Snapshot with mismatched presented_frame ID
    let other_frame_id = PresentedFrameId::new(owner, 999).unwrap();
    let (r_id, n_map, _, _) = make_test_tree(
        owner,
        Rect2D::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
        SemanticRole::Button,
        "B",
        Rect2D::from_xywh(20.0, 20.0, 10.0, 10.0).unwrap(),
        SemanticRole::Link,
        "L",
    );
    let mismatched_ident =
        AcceptedLayoutIdentity::new(owner, layout, source, display, Some(other_frame_id)).unwrap();
    let mismatched_snapshot =
        Arc::new(AcceptedLayoutSnapshot::new(mismatched_ident, metrics, r_id, n_map).unwrap());
    assert_eq!(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            frame_id,
            camera,
            scene,
            layout,
            display,
            interaction,
            metrics,
            mismatched_snapshot,
        ),
        Err(FcbError::StaleGeneration)
    );
}

#[test]
fn test_tracker_queue_and_presentation_lifecycle() {
    let owner = ArenaOwnerId::new(2001).unwrap();
    let foreign_owner = ArenaOwnerId::new(8888).unwrap();
    let mut tracker = PresentedFrameTracker::new(owner, 4, 8);

    assert_eq!(tracker.owner(), owner);
    assert_eq!(tracker.pending_count(), 0);
    assert_eq!(tracker.history_count(), 0);
    assert!(tracker.last_presented().is_none());
    assert!(tracker.last_presented_frame_id().is_none());

    // Negative control: Querying unpresented tracker
    let pt = Point2D::new(50.0, 20.0).unwrap();
    assert_eq!(tracker.resolve_interaction(pt), Err(FcbError::FrameUnpresented));
    assert_eq!(tracker.resolve_accessibility_hit(pt), Err(FcbError::FrameUnpresented));
    assert_eq!(tracker.visible_layout_snapshot().map(|_| ()), Err(FcbError::FrameUnpresented));

    // Create a valid frame plan
    let file = FileId::new(owner, 1).unwrap();
    let source = SourceRevision::new(owner, 1).unwrap();
    let bytes = ByteRange::new(ByteOffset::new(0), ByteOffset::new(50)).unwrap();
    let display = DisplayGeneration::new(owner, 1).unwrap();
    let layout = LayoutRevision::new(owner, 1).unwrap();
    let metrics = make_metrics(owner, display);

    let (root_id, nodes, _, _) = make_test_tree(
        owner,
        Rect2D::from_xywh(10.0, 10.0, 100.0, 40.0).unwrap(),
        SemanticRole::Button,
        "Btn",
        Rect2D::from_xywh(10.0, 60.0, 200.0, 30.0).unwrap(),
        SemanticRole::Link,
        "Lnk",
    );

    let frame1_id = PresentedFrameId::new(owner, 101).unwrap();
    let ident1 = AcceptedLayoutIdentity::new(owner, layout, source, display, Some(frame1_id)).unwrap();
    let snap1 = Arc::new(AcceptedLayoutSnapshot::new(ident1, metrics, root_id, nodes.clone()).unwrap());

    let plan1 = Arc::new(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            frame1_id,
            CameraGeneration::new(owner, 1).unwrap(),
            SceneGeneration::new(owner, 1).unwrap(),
            layout,
            display,
            InteractionGeneration::new(owner, 1).unwrap(),
            metrics,
            snap1,
        )
        .unwrap(),
    );

    // Negative control: Foreign owner frame cannot be submitted
    let foreign_plan = Arc::new(FramePlan::new(
        foreign_owner,
        FileId::new(foreign_owner, 1).unwrap(),
        SourceRevision::new(foreign_owner, 1).unwrap(),
        bytes,
    ));
    assert_eq!(tracker.submit_frame(foreign_plan), Err(FcbError::OwnerMismatch));

    // Negative control: Baseline frame plan without frame_id cannot be submitted
    let unallocated_plan = Arc::new(FramePlan::new(owner, file, source, bytes));
    assert_eq!(tracker.submit_frame(unallocated_plan), Err(FcbError::FrameNotFound));

    // Submit frame 1
    tracker.submit_frame(Arc::clone(&plan1)).expect("submit frame 1");
    assert_eq!(tracker.pending_count(), 1);
    assert!(tracker.last_presented().is_none());

    // Negative control: Foreign owner confirmation
    let foreign_frame_id = PresentedFrameId::new(foreign_owner, 101).unwrap();
    assert_eq!(
        tracker.confirm_presented(foreign_frame_id),
        Err(FcbError::OwnerMismatch)
    );

    // Negative control: Confirm non-existent frame
    let unknown_frame_id = PresentedFrameId::new(owner, 9999).unwrap();
    assert_eq!(
        tracker.confirm_presented(unknown_frame_id),
        Err(FcbError::FrameNotFound)
    );

    // Confirm presentation of frame 1
    tracker.confirm_presented(frame1_id).expect("confirm frame 1");
    assert_eq!(tracker.pending_count(), 0);
    assert_eq!(tracker.last_presented_frame_id(), Some(frame1_id));
    assert_eq!(tracker.history_count(), 0);

    // Now interactions resolve successfully!
    let res = tracker.resolve_interaction(pt).expect("resolve interaction");
    assert_eq!(res.frame_id(), frame1_id);
    assert_eq!(res.point(), pt);
    assert_eq!(res.layout_revision(), layout);
    assert_eq!(res.display_generation(), display);

    // Test bounded pending queue capacity
    let unpinned_ident =
        AcceptedLayoutIdentity::new(owner, layout, source, display, None).unwrap();
    let unpinned_snap =
        Arc::new(AcceptedLayoutSnapshot::new(unpinned_ident, metrics, root_id, nodes).unwrap());

    for i in 201..=204 {
        let fid = PresentedFrameId::new(owner, i).unwrap();
        let plan = Arc::new(
            FramePlan::bundle(
                owner,
                file,
                source,
                bytes,
                fid,
                CameraGeneration::new(owner, 1).unwrap(),
                SceneGeneration::new(owner, 1).unwrap(),
                layout,
                display,
                InteractionGeneration::new(owner, 1).unwrap(),
                metrics,
                Arc::clone(&unpinned_snap),
            )
            .unwrap(),
        );
        tracker.submit_frame(plan).unwrap();
    }
    assert_eq!(tracker.pending_count(), 4);

    // Queue is full (max_pending = 4) -> next submit fails with FrameQueueExhausted
    let overflow_fid = PresentedFrameId::new(owner, 205).unwrap();
    let overflow_plan = Arc::new(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            overflow_fid,
            CameraGeneration::new(owner, 1).unwrap(),
            SceneGeneration::new(owner, 1).unwrap(),
            layout,
            display,
            InteractionGeneration::new(owner, 1).unwrap(),
            metrics,
            Arc::clone(&unpinned_snap),
        )
        .unwrap(),
    );
    assert_eq!(
        tracker.submit_frame(overflow_plan),
        Err(FcbError::FrameQueueExhausted)
    );

    // Confirm frame 203 (superseding 201 and 202, leaving 204 in queue)
    let fid_203 = PresentedFrameId::new(owner, 203).unwrap();
    tracker.confirm_presented(fid_203).expect("confirm 203");
    assert_eq!(tracker.last_presented_frame_id(), Some(fid_203));
    assert_eq!(tracker.pending_count(), 1); // 204 is still pending
    // History should contain: old last_presented (101) + superseded pending (201, 202) = 3 frames
    assert_eq!(tracker.history_count(), 3);
}

#[test]
fn test_package_oracle_delayed_gpu_presentation_coherence() {
    // Contract Oracle:
    // When the model moves/reflows, new frames are generated and queued.
    // While GPU presentation is delayed or in flight, clicks and a11y queries
    // MUST resolve strictly against the currently visible frame (last_presented),
    // and NEVER against unseen pending geometry.
    // Once presentation is confirmed, queries immediately resolve against the updated geometry.

    let owner = ArenaOwnerId::new(3001).unwrap();
    let mut tracker = PresentedFrameTracker::new(owner, 8, 16);

    let file = FileId::new(owner, 1).unwrap();
    let source_v1 = SourceRevision::new(owner, 1).unwrap();
    let bytes = ByteRange::new(ByteOffset::new(0), ByteOffset::new(200)).unwrap();

    let display_gen = DisplayGeneration::new(owner, 1).unwrap();
    let layout_rev1 = LayoutRevision::new(owner, 1).unwrap();
    let metrics = make_metrics(owner, display_gen);

    // Frame 1 Layout:
    // Button "Submit" is at (10, 10, 100, 40)
    // Link "Documentation" is at (10, 60, 200, 30)
    let (root1, nodes1, button1_id, link1_id) = make_test_tree(
        owner,
        Rect2D::from_xywh(10.0, 10.0, 100.0, 40.0).unwrap(),
        SemanticRole::Button,
        "Submit",
        Rect2D::from_xywh(10.0, 60.0, 200.0, 30.0).unwrap(),
        SemanticRole::Link,
        "Documentation",
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

    // Frame 1 is presented on screen
    tracker.submit_frame(plan1).unwrap();
    tracker.confirm_presented(frame1_id).unwrap();

    // Interaction point inside Frame 1 button: (50, 30)
    let click_p1 = Point2D::new(50.0, 30.0).unwrap();
    // Interaction point inside Frame 1 link: (50, 75)
    let click_p2 = Point2D::new(50.0, 75.0).unwrap();
    // Target location where the button WILL move in Frame 2: (50, 200)
    let future_button_p = Point2D::new(50.0, 200.0).unwrap();

    // Verify clicks on visible Frame 1
    let hit1 = tracker.resolve_interaction(click_p1).expect("click on button");
    assert_eq!(hit1.target_node(), Some(button1_id));
    assert_eq!(hit1.frame_id(), frame1_id);
    assert_eq!(hit1.layout_revision(), layout_rev1);

    let hit2 = tracker.resolve_interaction(click_p2).expect("click on link");
    assert_eq!(hit2.target_node(), Some(link1_id));
    assert_eq!(hit2.frame_id(), frame1_id);

    // Clicking at future location in Frame 1 hits the background (root node), NOT the button
    let hit_future = tracker.resolve_interaction(future_button_p).expect("click at future pt");
    assert_eq!(hit_future.target_node(), Some(root1));

    // Accessibility hit testing also matches visible Frame 1
    assert_eq!(tracker.resolve_accessibility_hit(click_p1).unwrap(), Some(button1_id));
    assert_eq!(tracker.resolve_accessibility_hit(click_p2).unwrap(), Some(link1_id));

    // -------------------------------------------------------------------
    // BACKGROUND REFLOW / MOVEMENT:
    // An edit or layout reflow causes Frame 2 to be constructed:
    // In Frame 2:
    // - A new Warning Banner is placed at (10, 10, 100, 40)
    // - The "Submit" Button moved down to (10, 180, 100, 40)
    // -------------------------------------------------------------------
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

    // Frame 2 is submitted to the presentation tracker, but GPU presentation is DELAYED!
    tracker.submit_frame(plan2).unwrap();
    assert_eq!(tracker.pending_count(), 1);

    // ===================================================================
    // ORACLE CRITICAL CHECK 1:
    // While Frame 2 is in flight / delayed, the user clicks at (50, 30).
    // The user saw the "Submit" Button (from Frame 1).
    // The click MUST resolve to button1_id from Frame 1, NOT the banner from Frame 2!
    // ===================================================================
    let delayed_hit = tracker.resolve_interaction(click_p1).expect("interaction during delay");
    assert_eq!(
        delayed_hit.target_node(),
        Some(button1_id),
        "Click must hit button from visible Frame 1, not banner from unpresented Frame 2"
    );
    assert_eq!(delayed_hit.frame_id(), frame1_id);
    assert_eq!(delayed_hit.layout_revision(), layout_rev1);

    // Clicking at the new button position (50, 200) while Frame 2 is delayed STILL hits root1 in Frame 1
    let delayed_future_hit = tracker.resolve_interaction(future_button_p).expect("interaction at new pos");
    assert_eq!(
        delayed_future_hit.target_node(),
        Some(root1),
        "Click at unpresented position must not hit the unpresented button"
    );

    // Accessibility queries also strictly observe visible Frame 1
    let a11y_snap = tracker.visible_layout_snapshot().expect("visible layout snapshot");
    assert_eq!(a11y_snap.identity().presented_frame(), Some(frame1_id));
    assert_eq!(a11y_snap.node(button1_id).unwrap().label(), Some("Submit"));
    assert_eq!(a11y_snap.node(button1_id).unwrap().role(), SemanticRole::Button);
    assert_eq!(tracker.resolve_accessibility_hit(click_p1).unwrap(), Some(button1_id));

    // ===================================================================
    // GPU PRESENTATION CONFIRMED:
    // The host presentation callback fires with frame2_id.
    // ===================================================================
    tracker.confirm_presented(frame2_id).expect("confirm frame 2");
    assert_eq!(tracker.last_presented_frame_id(), Some(frame2_id));
    assert_eq!(tracker.pending_count(), 0);
    assert_eq!(tracker.history_count(), 1); // Frame 1 is now in history

    // ===================================================================
    // ORACLE CRITICAL CHECK 2:
    // After confirmation, the same click at (50, 30) now hits the Warning Banner!
    // And clicking at (50, 200) now hits the relocated "Submit" button!
    // ===================================================================
    let confirmed_hit1 = tracker.resolve_interaction(click_p1).expect("interaction on frame 2");
    assert_eq!(
        confirmed_hit1.target_node(),
        Some(banner2_id),
        "Click at (50, 30) must now hit Warning Banner in presented Frame 2"
    );
    assert_eq!(confirmed_hit1.frame_id(), frame2_id);
    assert_eq!(confirmed_hit1.layout_revision(), layout_rev2);

    let confirmed_hit2 = tracker.resolve_interaction(future_button_p).expect("interaction at new button pos");
    assert_eq!(
        confirmed_hit2.target_node(),
        Some(button2_id),
        "Click at (50, 200) must now hit relocated Button in presented Frame 2"
    );
    assert_eq!(confirmed_hit2.frame_id(), frame2_id);

    // Accessibility queries now reflect Frame 2
    let updated_a11y = tracker.visible_layout_snapshot().expect("updated layout snapshot");
    assert_eq!(updated_a11y.identity().presented_frame(), Some(frame2_id));
    assert_eq!(updated_a11y.node(banner2_id).unwrap().label(), Some("Warning Banner"));
    assert_eq!(tracker.resolve_accessibility_hit(click_p1).unwrap(), Some(banner2_id));
    assert_eq!(tracker.resolve_accessibility_hit(future_button_p).unwrap(), Some(button2_id));
}

#[test]
fn test_conservative_presentation_acceptance() {
    let owner = ArenaOwnerId::new(4001).unwrap();
    let foreign_owner = ArenaOwnerId::new(7777).unwrap();
    let clock_domain = ClockDomainId::new(owner, 1).unwrap();
    let foreign_clock_domain = ClockDomainId::new(foreign_owner, 1).unwrap();

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

    // Submit Frame 1 at t = 1000 ns
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
        .with_timestamps(FrameTimestamps::new(clock_domain, 1_000))
        .unwrap(),
    );
    tracker.submit_frame(f1).unwrap();

    // Submit Frame 2 at t = 2000 ns
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
        .with_timestamps(FrameTimestamps::new(clock_domain, 2_000))
        .unwrap(),
    );
    tracker.submit_frame(f2).unwrap();

    // Submit Frame 3 at t = 3000 ns
    let f3_id = PresentedFrameId::new(owner, 103).unwrap();
    let f3 = Arc::new(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            f3_id,
            CameraGeneration::new(owner, 1).unwrap(),
            SceneGeneration::new(owner, 1).unwrap(),
            layout,
            display,
            InteractionGeneration::new(owner, 1).unwrap(),
            metrics,
            Arc::clone(&snap),
        )
        .unwrap()
        .with_timestamps(FrameTimestamps::new(clock_domain, 3_000))
        .unwrap(),
    );
    tracker.submit_frame(f3).unwrap();

    assert_eq!(tracker.pending_count(), 3);

    // Negative control: Foreign owner clock domain refused
    assert_eq!(
        tracker.confirm_presented_conservative(foreign_clock_domain, 2_500),
        Err(FcbError::OwnerMismatch)
    );

    // Negative control: Timestamp before any submitted frame (t = 500 ns) -> FrameNotFound
    assert_eq!(
        tracker.confirm_presented_conservative(clock_domain, 500),
        Err(FcbError::FrameNotFound)
    );

    // Conservative confirmation at t = 2500 ns:
    // Frames 1 (1000ns) and 2 (2000ns) are eligible (<= 2500ns).
    // The latest eligible frame is Frame 2 (f2_id).
    // Frame 1 is retired to history; Frame 3 (3000ns) remains in pending queue!
    let chosen_fid = tracker
        .confirm_presented_conservative(clock_domain, 2_500)
        .expect("conservative presentation at 2500ns");
    assert_eq!(chosen_fid, f2_id);
    assert_eq!(tracker.last_presented_frame_id(), Some(f2_id));

    // Mode is conservative
    let mode = tracker.last_presentation_mode().expect("presentation mode");
    assert!(mode.is_conservative());
    assert_eq!(mode.frame_id(), f2_id);
    assert!(matches!(mode, PresentationMode::Conservative { .. }));

    // Pending count is 1 (Frame 3 remains pending)
    assert_eq!(tracker.pending_count(), 1);
    // History contains Frame 1
    assert_eq!(tracker.history_count(), 1);

    // Conservative confirmation at t = 2800 ns:
    // Frame 3 was submitted at 3000ns > 2800ns, so Frame 3 is not yet eligible.
    // Frame 2 (already presented, submitted at 2000ns <= 2800ns) remains the conservative authority!
    let repeat_fid = tracker
        .confirm_presented_conservative(clock_domain, 2_800)
        .expect("conservative presentation at 2800ns");
    assert_eq!(repeat_fid, f2_id);
    assert_eq!(tracker.pending_count(), 1);

    // Conservative confirmation at t = 3500 ns:
    // Now Frame 3 (3000ns <= 3500ns) is eligible and becomes the presented frame!
    let f3_chosen = tracker
        .confirm_presented_conservative(clock_domain, 3_500)
        .expect("conservative presentation at 3500ns");
    assert_eq!(f3_chosen, f3_id);
    assert_eq!(tracker.pending_count(), 0);
    assert_eq!(tracker.history_count(), 2); // Frame 1 and Frame 2 in history
}

#[test]
fn test_coordinate_domains_and_resize_races() {
    // Contract:
    // "A resize or backing-scale transition cannot combine old pixel coordinates with new logical geometry.
    //  Multiple queued frames are not multiple authorities for one input event."

    let owner = ArenaOwnerId::new(5001).unwrap();
    let mut tracker = PresentedFrameTracker::new(owner, 8, 16);

    let file = FileId::new(owner, 1).unwrap();
    let source = SourceRevision::new(owner, 1).unwrap();
    let bytes = ByteRange::new(ByteOffset::new(0), ByteOffset::new(100)).unwrap();

    // Frame 1: Display generation 1, Scale 2.0, Size 1440x900
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

    // Physical pixel point: (100.0, 60.0) in 2.0x scale -> logical (50.0, 30.0).
    // In Frame 1, logical (50.0, 30.0) falls inside btn1 (10..110, 10..50)!
    let phys_pt = Point2D::new(100.0, 60.0).unwrap();

    // Resolving with matching display_gen1 succeeds!
    let res1 = tracker
        .resolve_interaction_physical(phys_pt, display_gen1, None)
        .expect("resolve physical interaction");
    assert_eq!(res1.target_node(), Some(btn1));
    assert_eq!(res1.point(), Point2D::new(50.0, 30.0).unwrap());
    assert_eq!(res1.display_generation(), display_gen1);

    // -------------------------------------------------------------------
    // RESIZE RACE:
    // The window is resized or moved to a different display:
    // A new DisplayGeneration 2 is assigned (Scale 1.0, Size 2560x1440).
    // Frame 2 is generated and queued, but GPU presentation is delayed!
    // -------------------------------------------------------------------
    let display_gen2 = DisplayGeneration::new(owner, 2).unwrap();
    let layout_rev2 = LayoutRevision::new(owner, 2).unwrap();
    let metrics2 = DisplayMetrics::new(
        1.0,
        Size2D::new(2560.0, 1440.0).unwrap(),
        DisplayColorConfig::DisplayP3,
        display_gen2,
    )
    .unwrap();

    let (root2, nodes2, btn2, _) = make_test_tree(
        owner,
        Rect2D::from_xywh(200.0, 200.0, 100.0, 40.0).unwrap(),
        SemanticRole::Button,
        "Action",
        Rect2D::from_xywh(200.0, 260.0, 100.0, 40.0).unwrap(),
        SemanticRole::Link,
        "Help",
    );

    let f2_id = PresentedFrameId::new(owner, 2).unwrap();
    let ident2 = AcceptedLayoutIdentity::new(owner, layout_rev2, source, display_gen2, Some(f2_id)).unwrap();
    let snap2 = Arc::new(AcceptedLayoutSnapshot::new(ident2, metrics2, root2, nodes2).unwrap());

    let plan2 = Arc::new(
        FramePlan::bundle(
            owner,
            file,
            source,
            bytes,
            f2_id,
            CameraGeneration::new(owner, 2).unwrap(),
            SceneGeneration::new(owner, 2).unwrap(),
            layout_rev2,
            display_gen2,
            InteractionGeneration::new(owner, 2).unwrap(),
            metrics2,
            snap2,
        )
        .unwrap(),
    );

    tracker.submit_frame(plan2).unwrap();
    assert_eq!(tracker.pending_count(), 1);

    // ===================================================================
    // ORACLE CHECK:
    // A platform event arrives sampled against the new display parameters (display_gen2).
    // But Frame 2 has NOT been presented by the GPU (Frame 1 is visible).
    // The tracker MUST REFUSE the interaction with CoordinateDomainMismatch!
    // ===================================================================
    assert_eq!(
        tracker.resolve_interaction_physical(phys_pt, display_gen2, None),
        Err(FcbError::CoordinateDomainMismatch)
    );
    assert_eq!(tracker.evidence_ring().total_resize_refusals(), 1);

    // An event arriving with the STILL-VISIBLE display_gen1 continues to resolve against Frame 1!
    let res_still_v1 = tracker
        .resolve_interaction_physical(phys_pt, display_gen1, None)
        .expect("interaction with visible display_gen1 succeeds");
    assert_eq!(res_still_v1.target_node(), Some(btn1));

    // ===================================================================
    // GPU CONFIRMS PRESENTATION OF FRAME 2:
    // ===================================================================
    tracker.confirm_presented(f2_id).expect("confirm frame 2");
    assert_eq!(tracker.last_presented_frame_id(), Some(f2_id));

    // Now events with display_gen2 succeed!
    // In Frame 2 (scale 1.0), physical (250.0, 220.0) converts to logical (250.0, 220.0),
    // which hits btn2 (200..300, 200..240)!
    let phys_pt2 = Point2D::new(250.0, 220.0).unwrap();
    let res2 = tracker
        .resolve_interaction_physical(phys_pt2, display_gen2, None)
        .expect("interaction with presented display_gen2");
    assert_eq!(res2.target_node(), Some(btn2));
    assert_eq!(res2.point(), Point2D::new(250.0, 220.0).unwrap());

    // And stale events claiming old display_gen1 are now refused!
    assert_eq!(
        tracker.resolve_interaction_physical(phys_pt, display_gen1, None),
        Err(FcbError::CoordinateDomainMismatch)
    );
    assert_eq!(tracker.evidence_ring().total_resize_refusals(), 2);
}

#[test]
fn test_clock_domain_conversion_and_latency_accounting() {
    let owner = ArenaOwnerId::new(6001).unwrap();
    let foreign_owner = ArenaOwnerId::new(9001).unwrap();
    let clock_domain = ClockDomainId::new(owner, 1).unwrap();
    let foreign_clock_domain = ClockDomainId::new(foreign_owner, 1).unwrap();
    let other_domain_same_owner = ClockDomainId::new(owner, 2).unwrap();

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
        .expect("exact confirm with presentation timestamp");

    let pt = Point2D::new(50.0, 20.0).unwrap();

    // Negative control: Foreign owner clock domain refused
    assert_eq!(
        tracker.resolve_interaction_physical(pt, display, Some((foreign_clock_domain, 1_800_000))),
        Err(FcbError::OwnerMismatch)
    );

    // Negative control: Mismatched clock domain in same owner domain refused
    assert_eq!(
        tracker.resolve_interaction_physical(pt, display, Some((other_domain_same_owner, 1_800_000))),
        Err(FcbError::ClockDomainMismatch)
    );

    // Matching clock domain:
    // Event at 1_800_000 ns, Frame presented at 1_500_000 ns -> latency = 300_000 ns!
    let res = tracker
        .resolve_interaction_physical(pt, display, Some((clock_domain, 1_800_000)))
        .expect("matching clock interaction succeeds");
    assert_eq!(res.frame_id(), f1_id);

    // Evidence ring recorded the interaction
    assert_eq!(tracker.evidence_ring().total_interactions(), 1);
    let events = tracker.evidence_ring().recent_events();
    let last_event = events.last().unwrap();
    match last_event {
        FrameEvidenceEvent::InteractionResolved {
            frame_id,
            latency_nanos,
            ..
        } => {
            assert_eq!(*frame_id, f1_id);
            assert_eq!(*latency_nanos, Some(300_000));
        }
        other => panic!("expected InteractionResolved event, got {:?}", other),
    }
}

#[test]
fn test_evidence_ring_bounding_and_counters() {
    let mut ring = FrameEvidenceRing::new(16);
    assert_eq!(ring.capacity(), 16);
    assert_eq!(ring.total_events(), 0);
    assert_eq!(ring.total_submitted(), 0);
    assert_eq!(ring.total_presented(), 0);

    let owner = ArenaOwnerId::new(7001).unwrap();
    let display = DisplayGeneration::new(owner, 1).unwrap();

    // Push 30 events into capacity-16 ring
    for i in 1..=30 {
        let fid = PresentedFrameId::new(owner, i).unwrap();
        ring.push(FrameEvidenceEvent::FrameSubmitted {
            frame_id: fid,
            display_generation: display,
            submitted_nanos: i * 100,
        });
    }

    assert_eq!(ring.total_events(), 30);
    assert_eq!(ring.total_submitted(), 30);
    assert_eq!(ring.recent_events().len(), 16); // Bounded!
}

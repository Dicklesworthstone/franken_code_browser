#![forbid(unsafe_code)]

use std::{collections::BTreeMap, sync::Arc};

use fcb::{
    AcceptedLayoutIdentity, AcceptedLayoutSnapshot, ArenaOwnerId, ByteOffset, ByteRange,
    CameraGeneration, DisplayGeneration, DisplayMetrics, FcbError, FileId, FramePlan,
    InteractionGeneration, LayoutRevision, Point2D, PresentedFrameId, PresentedFrameTracker,
    Rect2D, SceneGeneration, SemanticNodeId, Size2D, SourceRevision,
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

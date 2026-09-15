#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use fcb_core::{
    focus::{
        AcceptedLayoutIdentity, AcceptedLayoutSnapshot, SemanticFocusState, SemanticNode,
        SemanticRole,
    },
    geometry::{
        DisplayColorConfig, DisplayMetrics, Point2D, Rect2D, SemanticGeometry, Size2D,
    },
    pending_range::{PendingRangeStatus, PendingTextRangeResolver, RangeRequestToken},
    ArenaOwnerId, BidiBoundary, CaretAffinity, CoreError, DisplayGeneration, LayoutRevision,
    PresentedFrameId, RangeEvidenceEvent, RangeEvidenceKind, RangeEvidenceRing, SemanticNodeId,
    SourceRevision, Utf16CodeUnitOffset, Utf16CodeUnitRange, VisualPosition, NATIVE_NOT_FOUND,
};

#[test]
fn geometry_finite_and_bounds_contracts() {
    // Finite point creation
    let p1 = Point2D::new(10.0, 20.0).expect("valid point");
    assert_eq!(p1.x(), 10.0);
    assert_eq!(p1.y(), 20.0);

    // Negative canonicalization of -0.0 to +0.0
    let p_zero = Point2D::new(-0.0, -0.0).expect("zero point");
    assert_eq!(p_zero.x(), 0.0);
    assert_eq!(p_zero.y(), 0.0);
    assert!(!p_zero.x().is_sign_negative());
    assert!(!p_zero.y().is_sign_negative());

    // Negative controls: Non-finite points rejected
    assert_eq!(
        Point2D::new(f64::NAN, 10.0),
        Err(CoreError::NonFiniteGeometry)
    );
    assert_eq!(
        Point2D::new(10.0, f64::INFINITY),
        Err(CoreError::NonFiniteGeometry)
    );
    assert_eq!(
        Point2D::new(f64::NEG_INFINITY, 10.0),
        Err(CoreError::NonFiniteGeometry)
    );

    // Size2D finite and non-negative
    let s1 = Size2D::new(100.0, 50.0).expect("valid size");
    assert_eq!(s1.width(), 100.0);
    assert_eq!(s1.height(), 50.0);
    assert_eq!(s1.area(), 5000.0);
    assert!(!s1.is_empty());

    let s_empty = Size2D::new(0.0, 50.0).expect("empty width size");
    assert!(s_empty.is_empty());

    // Negative controls: Size with negative dimension or NaN rejected
    assert_eq!(
        Size2D::new(-1.0, 50.0),
        Err(CoreError::InvalidGeometry)
    );
    assert_eq!(
        Size2D::new(50.0, -0.001),
        Err(CoreError::InvalidGeometry)
    );
    assert_eq!(
        Size2D::new(f64::NAN, 50.0),
        Err(CoreError::NonFiniteGeometry)
    );

    // Rect2D construction and hit testing
    let r1 = Rect2D::from_xywh(10.0, 20.0, 100.0, 50.0).expect("valid rect");
    assert_eq!(r1.min_x(), 10.0);
    assert_eq!(r1.max_x(), 110.0);
    assert_eq!(r1.min_y(), 20.0);
    assert_eq!(r1.max_y(), 70.0);

    assert!(r1.contains_point(Point2D::new(10.0, 20.0).unwrap()));
    assert!(r1.contains_point(Point2D::new(50.0, 45.0).unwrap()));
    assert!(r1.contains_point(Point2D::new(110.0, 70.0).unwrap()));
    assert!(!r1.contains_point(Point2D::new(9.9, 20.0).unwrap()));
    assert!(!r1.contains_point(Point2D::new(110.1, 70.0).unwrap()));

    // Rect intersection
    let r2 = Rect2D::from_xywh(60.0, 40.0, 100.0, 100.0).expect("valid rect 2");
    let inter = r1.intersection(r2).expect("intersection exists");
    assert_eq!(inter.min_x(), 60.0);
    assert_eq!(inter.max_x(), 110.0);
    assert_eq!(inter.min_y(), 40.0);
    assert_eq!(inter.max_y(), 70.0);

    // Disjoint rect intersection returns None
    let r3 = Rect2D::from_xywh(200.0, 200.0, 10.0, 10.0).expect("disjoint rect");
    assert_eq!(r1.intersection(r3), None);

    // Union
    let un = r1.union(r2).expect("union exists");
    assert_eq!(un.min_x(), 10.0);
    assert_eq!(un.max_x(), 160.0);
    assert_eq!(un.min_y(), 20.0);
    assert_eq!(un.max_y(), 140.0);
}

#[test]
fn display_metrics_and_physical_conversions() {
    let owner = ArenaOwnerId::new(1).unwrap();
    let display_gen = DisplayGeneration::new(owner, 1).unwrap();
    let size = Size2D::new(1440.0, 900.0).unwrap();

    let metrics = DisplayMetrics::new(2.0, size, DisplayColorConfig::DisplayP3, display_gen)
        .expect("valid display metrics");

    assert_eq!(metrics.scale_factor(), 2.0);
    assert_eq!(metrics.logical_size(), size);
    assert_eq!(metrics.color_config(), DisplayColorConfig::DisplayP3);
    assert_eq!(metrics.generation(), display_gen);

    // Physical pixel conversion
    let (px_w, px_h) = metrics.physical_pixels().expect("pixels calculated");
    assert_eq!(px_w, 2880);
    assert_eq!(px_h, 1800);

    // Point coordinate conversions
    let logical_pt = Point2D::new(100.0, 50.0).unwrap();
    let (phys_x, phys_y) = metrics
        .logical_to_physical_point(logical_pt)
        .expect("logical to physical");
    assert_eq!(phys_x, 200.0);
    assert_eq!(phys_y, 100.0);

    let roundtrip_pt = metrics
        .physical_to_logical_point(phys_x, phys_y)
        .expect("physical to logical");
    assert_eq!(roundtrip_pt, logical_pt);

    // Negative controls: Invalid scale factors
    assert_eq!(
        DisplayMetrics::new(0.0, size, DisplayColorConfig::Srgb, display_gen),
        Err(CoreError::InvalidGeometry)
    );
    assert_eq!(
        DisplayMetrics::new(-1.0, size, DisplayColorConfig::Srgb, display_gen),
        Err(CoreError::InvalidGeometry)
    );
    assert_eq!(
        DisplayMetrics::new(f64::NAN, size, DisplayColorConfig::Srgb, display_gen),
        Err(CoreError::NonFiniteGeometry)
    );
}

#[test]
fn semantic_geometry_and_clipping_containment() {
    let bounds = Rect2D::from_xywh(0.0, 0.0, 200.0, 100.0).unwrap();
    let content = Rect2D::from_xywh(10.0, 10.0, 180.0, 80.0).unwrap();
    let clip = Rect2D::from_xywh(0.0, 0.0, 100.0, 100.0).unwrap(); // clips right half

    let geom = SemanticGeometry::new(bounds, content, Some(clip)).expect("valid geometry");

    // Visible rect is clipped to left half: [0, 0, 100, 100]
    let vis = geom.visible_rect().expect("visible rect");
    assert_eq!(vis.min_x(), 0.0);
    assert_eq!(vis.max_x(), 100.0);

    // Point in left half hits
    assert!(geom.contains_hit(Point2D::new(50.0, 50.0).unwrap()));
    // Point in right half is inside bounds but outside clip rect -> NO HIT
    assert!(!geom.contains_hit(Point2D::new(150.0, 50.0).unwrap()));
    // Point outside bounds -> NO HIT
    assert!(!geom.contains_hit(Point2D::new(250.0, 50.0).unwrap()));

    // Negative control: content rect exceeding bounds is refused
    let oversized_content = Rect2D::from_xywh(0.0, 0.0, 300.0, 100.0).unwrap();
    assert_eq!(
        SemanticGeometry::new(bounds, oversized_content, None),
        Err(CoreError::InvalidGeometry)
    );
}

#[test]
fn accepted_layout_identity_and_snapshot_hierarchy() {
    let owner = ArenaOwnerId::new(42).unwrap();
    let foreign_owner = ArenaOwnerId::new(99).unwrap();

    let layout_rev = LayoutRevision::new(owner, 10).unwrap();
    let source_rev = SourceRevision::new(owner, 5).unwrap();
    let display_gen = DisplayGeneration::new(owner, 2).unwrap();
    let frame_id = PresentedFrameId::new(owner, 100).unwrap();

    let identity = AcceptedLayoutIdentity::new(
        owner,
        layout_rev,
        source_rev,
        display_gen,
        Some(frame_id),
    )
    .expect("valid identity");

    assert_eq!(identity.owner(), owner);
    assert_eq!(identity.layout_revision(), layout_rev);
    assert_eq!(identity.source_revision(), source_rev);
    assert_eq!(identity.display_generation(), display_gen);
    assert_eq!(identity.presented_frame(), Some(frame_id));

    // Stale detection
    let stale_layout = AcceptedLayoutIdentity::new(
        owner,
        LayoutRevision::new(owner, 11).unwrap(),
        source_rev,
        display_gen,
        Some(frame_id),
    )
    .unwrap();
    assert_eq!(
        identity.validate_against(&stale_layout),
        Err(CoreError::StaleLayoutRevision)
    );

    let stale_source = AcceptedLayoutIdentity::new(
        owner,
        layout_rev,
        SourceRevision::new(owner, 6).unwrap(),
        display_gen,
        Some(frame_id),
    )
    .unwrap();
    assert_eq!(
        identity.validate_against(&stale_source),
        Err(CoreError::StaleSourceRevision)
    );

    let stale_display = AcceptedLayoutIdentity::new(
        owner,
        layout_rev,
        source_rev,
        DisplayGeneration::new(owner, 3).unwrap(),
        Some(frame_id),
    )
    .unwrap();
    assert_eq!(
        identity.validate_against(&stale_display),
        Err(CoreError::StaleDisplayGeneration)
    );

    // Negative control: foreign owner mismatch
    assert_eq!(
        AcceptedLayoutIdentity::new(
            owner,
            LayoutRevision::new(foreign_owner, 10).unwrap(),
            source_rev,
            display_gen,
            None
        ),
        Err(CoreError::OwnershipMismatch)
    );

    // Build hierarchical layout tree:
    // root (0, 0, 500, 500)
    //   ├── child1 (button: 10, 10, 100, 40) focusable
    //   └── child2 (container: 10, 60, 400, 200)
    //         └── grandchild (link: 20, 70, 80, 30) focusable
    let root_id = SemanticNodeId::new(owner, 1).unwrap();
    let child1_id = SemanticNodeId::new(owner, 2).unwrap();
    let child2_id = SemanticNodeId::new(owner, 3).unwrap();
    let grandchild_id = SemanticNodeId::new(owner, 4).unwrap();

    let root_geom = SemanticGeometry::new(
        Rect2D::from_xywh(0.0, 0.0, 500.0, 500.0).unwrap(),
        Rect2D::from_xywh(0.0, 0.0, 500.0, 500.0).unwrap(),
        None,
    )
    .unwrap();
    let mut root_node = SemanticNode::new(root_id, root_geom, SemanticRole::Document, false);
    root_node.add_child(child1_id).unwrap();
    root_node.add_child(child2_id).unwrap();

    let child1_geom = SemanticGeometry::new(
        Rect2D::from_xywh(10.0, 10.0, 100.0, 40.0).unwrap(),
        Rect2D::from_xywh(10.0, 10.0, 100.0, 40.0).unwrap(),
        None,
    )
    .unwrap();
    let mut child1_node = SemanticNode::new(child1_id, child1_geom, SemanticRole::Button, true);
    child1_node.set_parent(Some(root_id)).unwrap();
    child1_node.set_label(Some("Action Button".to_string()));

    let child2_geom = SemanticGeometry::new(
        Rect2D::from_xywh(10.0, 60.0, 400.0, 200.0).unwrap(),
        Rect2D::from_xywh(10.0, 60.0, 400.0, 200.0).unwrap(),
        None,
    )
    .unwrap();
    let mut child2_node = SemanticNode::new(child2_id, child2_geom, SemanticRole::Container, false);
    child2_node.set_parent(Some(root_id)).unwrap();
    child2_node.add_child(grandchild_id).unwrap();

    let grandchild_geom = SemanticGeometry::new(
        Rect2D::from_xywh(20.0, 70.0, 80.0, 30.0).unwrap(),
        Rect2D::from_xywh(20.0, 70.0, 80.0, 30.0).unwrap(),
        None,
    )
    .unwrap();
    let mut grandchild_node =
        SemanticNode::new(grandchild_id, grandchild_geom, SemanticRole::Link, true);
    grandchild_node.set_parent(Some(child2_id)).unwrap();
    grandchild_node.set_label(Some("Documentation Link".to_string()));

    let mut nodes = BTreeMap::new();
    nodes.insert(root_id, root_node);
    nodes.insert(child1_id, child1_node);
    nodes.insert(child2_id, child2_node);
    nodes.insert(grandchild_id, grandchild_node);

    let metrics = DisplayMetrics::new(
        2.0,
        Size2D::new(500.0, 500.0).unwrap(),
        DisplayColorConfig::DisplayP3,
        display_gen,
    )
    .unwrap();

    let layout = AcceptedLayoutSnapshot::new(identity, metrics, root_id, nodes)
        .expect("valid layout snapshot");

    assert_eq!(layout.node_count(), 4);

    // Hit testing:
    // Hit button at (20, 20) -> child1_id
    assert_eq!(
        layout.hit_test(Point2D::new(20.0, 20.0).unwrap()),
        Some(child1_id)
    );

    // Hit grandchild link at (30, 80) -> grandchild_id
    assert_eq!(
        layout.hit_test(Point2D::new(30.0, 80.0).unwrap()),
        Some(grandchild_id)
    );

    // Hit container background at (200, 100) -> child2_id
    assert_eq!(
        layout.hit_test(Point2D::new(200.0, 100.0).unwrap()),
        Some(child2_id)
    );

    // Hit root background at (450, 450) -> root_id
    assert_eq!(
        layout.hit_test(Point2D::new(450.0, 450.0).unwrap()),
        Some(root_id)
    );

    // Hit outside root -> None
    assert_eq!(layout.hit_test(Point2D::new(600.0, 600.0).unwrap()), None);

    // Focusable nodes list (in tree order: child1, grandchild)
    let focusable = layout.focusable_nodes();
    assert_eq!(focusable, vec![child1_id, grandchild_id]);
}

#[test]
fn semantic_focus_state_and_focus_return_contract() {
    let owner = ArenaOwnerId::new(77).unwrap();
    let foreign_owner = ArenaOwnerId::new(88).unwrap();
    let layout_rev = LayoutRevision::new(owner, 1).unwrap();
    let source_rev = SourceRevision::new(owner, 1).unwrap();
    let display_gen = DisplayGeneration::new(owner, 1).unwrap();
    let identity =
        AcceptedLayoutIdentity::new(owner, layout_rev, source_rev, display_gen, None).unwrap();
    let metrics = DisplayMetrics::new(
        1.0,
        Size2D::new(800.0, 600.0).unwrap(),
        DisplayColorConfig::Srgb,
        display_gen,
    )
    .unwrap();

    let root_id = SemanticNodeId::new(owner, 1).unwrap();
    let btn_id = SemanticNodeId::new(owner, 2).unwrap();
    let link_id = SemanticNodeId::new(owner, 3).unwrap();
    let non_focusable_id = SemanticNodeId::new(owner, 4).unwrap();

    let r_geom = SemanticGeometry::new(
        Rect2D::from_xywh(0.0, 0.0, 800.0, 600.0).unwrap(),
        Rect2D::from_xywh(0.0, 0.0, 800.0, 600.0).unwrap(),
        None,
    )
    .unwrap();
    let mut root_node = SemanticNode::new(root_id, r_geom, SemanticRole::Document, false);
    root_node.add_child(btn_id).unwrap();
    root_node.add_child(link_id).unwrap();
    root_node.add_child(non_focusable_id).unwrap();

    let b_geom = SemanticGeometry::new(
        Rect2D::from_xywh(10.0, 10.0, 100.0, 30.0).unwrap(),
        Rect2D::from_xywh(10.0, 10.0, 100.0, 30.0).unwrap(),
        None,
    )
    .unwrap();
    let btn_node = SemanticNode::new(btn_id, b_geom, SemanticRole::Button, true);

    let l_geom = SemanticGeometry::new(
        Rect2D::from_xywh(10.0, 50.0, 100.0, 30.0).unwrap(),
        Rect2D::from_xywh(10.0, 50.0, 100.0, 30.0).unwrap(),
        None,
    )
    .unwrap();
    let link_node = SemanticNode::new(link_id, l_geom, SemanticRole::Link, true);

    let nf_geom = SemanticGeometry::new(
        Rect2D::from_xywh(10.0, 90.0, 100.0, 30.0).unwrap(),
        Rect2D::from_xywh(10.0, 90.0, 100.0, 30.0).unwrap(),
        None,
    )
    .unwrap();
    let nf_node = SemanticNode::new(non_focusable_id, nf_geom, SemanticRole::Paragraph, false);

    let mut nodes = BTreeMap::new();
    nodes.insert(root_id, root_node);
    nodes.insert(btn_id, btn_node);
    nodes.insert(link_id, link_node);
    nodes.insert(non_focusable_id, nf_node);

    let layout = AcceptedLayoutSnapshot::new(identity, metrics, root_id, nodes).unwrap();

    let mut focus_state = SemanticFocusState::new(owner, 4);
    assert_eq!(focus_state.current_focus(), None);
    assert_eq!(focus_state.focus_stack_depth(), 0);

    // Focus button
    focus_state.focus_node(btn_id, &layout).expect("focus button");
    assert_eq!(focus_state.current_focus(), Some(btn_id));
    assert_eq!(focus_state.focus_stack_depth(), 0);

    // Verify focus bounds
    let bounds = focus_state
        .focus_bounds(&layout)
        .expect("focus bounds exist");
    assert_eq!(bounds.min_x(), 10.0);
    assert_eq!(bounds.min_y(), 10.0);

    // Focus link -> button goes to focus stack for focus return
    focus_state.focus_node(link_id, &layout).expect("focus link");
    assert_eq!(focus_state.current_focus(), Some(link_id));
    assert_eq!(focus_state.focus_stack_depth(), 1);

    // Re-focusing same node is an idempotent no-op
    focus_state.focus_node(link_id, &layout).expect("re-focus link");
    assert_eq!(focus_state.focus_stack_depth(), 1);

    // Negative control: focusing non-focusable node is refused
    assert_eq!(
        focus_state.focus_node(non_focusable_id, &layout),
        Err(CoreError::FocusTargetNotFound)
    );

    // Negative control: foreign owner node is refused
    let foreign_node_id = SemanticNodeId::new(foreign_owner, 9).unwrap();
    assert_eq!(
        focus_state.focus_node(foreign_node_id, &layout),
        Err(CoreError::OwnershipMismatch)
    );

    // Focus return restores previous button focus
    let returned = focus_state.return_focus(&layout).expect("focus returned");
    assert_eq!(returned, Some(btn_id));
    assert_eq!(focus_state.current_focus(), Some(btn_id));
    assert_eq!(focus_state.focus_stack_depth(), 0);

    // Focus return when stack is empty returns None
    let empty_return = focus_state.return_focus(&layout).expect("empty return");
    assert_eq!(empty_return, None);
    assert_eq!(focus_state.current_focus(), None);
}

#[test]
fn pending_range_explicit_state_and_surrogate_boundary_oracle() {
    let owner = ArenaOwnerId::new(999).unwrap();
    let layout_rev = LayoutRevision::new(owner, 1).unwrap();
    let source_rev = SourceRevision::new(owner, 1).unwrap();
    let display_gen = DisplayGeneration::new(owner, 1).unwrap();
    let identity =
        AcceptedLayoutIdentity::new(owner, layout_rev, source_rev, display_gen, None).unwrap();
    let metrics = DisplayMetrics::new(
        1.0,
        Size2D::new(800.0, 600.0).unwrap(),
        DisplayColorConfig::Srgb,
        display_gen,
    )
    .unwrap();

    let text_node_id = SemanticNodeId::new(owner, 1).unwrap();
    let geom = SemanticGeometry::new(
        Rect2D::from_xywh(0.0, 0.0, 400.0, 200.0).unwrap(),
        Rect2D::from_xywh(0.0, 0.0, 400.0, 200.0).unwrap(),
        None,
    )
    .unwrap();
    let text_node = SemanticNode::new(text_node_id, geom, SemanticRole::SourceText, false);

    let mut nodes = BTreeMap::new();
    nodes.insert(text_node_id, text_node);
    let layout = AcceptedLayoutSnapshot::new(identity, metrics, text_node_id, nodes).unwrap();

    let token = RangeRequestToken::new(owner, 1, text_node_id, layout_rev).unwrap();

    // The text contains a surrogate pair emoji (U+1F600, utf16 code units 0xD83D, 0xDE00)
    // String: "A😀B"
    // UTF-16 indices:
    //   0: 'A' (len 1)
    //   1: high surrogate 0xD83D
    //   2: low surrogate 0xDE00
    //   3: 'B' (len 1)
    // Total utf16 length = 4
    let sample_text = "A😀B";

    // 1. Explicit Pending state when text context is not yet loaded:
    let req_range = Utf16CodeUnitRange::new(
        Utf16CodeUnitOffset::new(0),
        Utf16CodeUnitOffset::new(3),
    )
    .unwrap();

    let pending_status = PendingTextRangeResolver::resolve_range(
        token,
        &layout,
        None, // Text not loaded yet -> returns Pending!
        req_range,
        &[],
    );
    assert_eq!(
        pending_status,
        PendingRangeStatus::Pending {
            token,
            requested_range: req_range,
        }
    );

    // 2. Ready state when text is provided and boundaries are valid:
    // Request range [0, 3] covering "A😀" (code units 0..3)
    let ready_status = PendingTextRangeResolver::resolve_range(
        token,
        &layout,
        Some(sample_text),
        req_range,
        &[],
    );
    match ready_status {
        PendingRangeStatus::Ready(resolved) => {
            assert_eq!(resolved.text(), "A😀");
            assert_eq!(resolved.range(), req_range);
            assert_eq!(resolved.byte_range().len().get(), 5); // 1 + 4 bytes
            assert_eq!(resolved.scalar_range().len(), 2); // 2 scalar chars
        }
        other => panic!("expected Ready, got {other:?}"),
    }

    // 3. Oracle / Negative Control: Slicing within a surrogate pair is refused!
    // Offset 2 lands in the middle of "😀" between high and low surrogate.
    let split_surrogate_range = Utf16CodeUnitRange::new(
        Utf16CodeUnitOffset::new(0),
        Utf16CodeUnitOffset::new(2),
    )
    .unwrap();

    let refused_surrogate = PendingTextRangeResolver::resolve_range(
        token,
        &layout,
        Some(sample_text),
        split_surrogate_range,
        &[],
    );
    assert_eq!(
        refused_surrogate,
        PendingRangeStatus::Refused(CoreError::InvalidUtf16)
    );

    // 4. Oracle / Negative Control: NATIVE_NOT_FOUND sentinel is refused!
    let sentinel_range = Utf16CodeUnitRange::new(
        Utf16CodeUnitOffset::new(0),
        Utf16CodeUnitOffset::new(NATIVE_NOT_FOUND),
    )
    .unwrap();

    let refused_sentinel = PendingTextRangeResolver::resolve_range(
        token,
        &layout,
        Some(sample_text),
        sentinel_range,
        &[],
    );
    assert_eq!(
        refused_sentinel,
        PendingRangeStatus::Refused(CoreError::NativeSentinel)
    );

    // 5. Oracle / Negative Control: Stale layout revision is detected!
    let stale_token = RangeRequestToken::new(
        owner,
        2,
        text_node_id,
        LayoutRevision::new(owner, 999).unwrap(),
    )
    .unwrap();
    let stale_result = PendingTextRangeResolver::resolve_range(
        stale_token,
        &layout,
        Some(sample_text),
        req_range,
        &[],
    );
    assert_eq!(
        stale_result,
        PendingRangeStatus::Stale(CoreError::StaleLayoutRevision)
    );
}

#[test]
fn bidi_non_one_to_one_mapping_oracle_detection() {
    // In bidi or ligature text, multiple logical boundaries map to the same visual position.
    // E.g., at visual position 5, upstream affinity chooses logical offset 3,
    // downstream affinity chooses logical offset 4.
    let boundary_upstream = BidiBoundary::new(
        Utf16CodeUnitOffset::new(3),
        VisualPosition::new(5),
        CaretAffinity::Upstream,
    );
    let boundary_downstream = BidiBoundary::new(
        Utf16CodeUnitOffset::new(4),
        VisualPosition::new(5),
        CaretAffinity::Downstream,
    );

    let boundaries = [boundary_upstream, boundary_downstream];

    // Oracle demonstrates that a 1:1 mapping claim is false
    assert!(PendingTextRangeResolver::has_non_one_to_one_bidi_mapping(&boundaries));

    // Linear non-bidi single-element mapping is 1:1
    let simple_boundaries = [BidiBoundary::new(
        Utf16CodeUnitOffset::new(0),
        VisualPosition::new(0),
        CaretAffinity::Downstream,
    )];
    assert!(!PendingTextRangeResolver::has_non_one_to_one_bidi_mapping(&simple_boundaries));
}

#[test]
fn bounded_evidence_ring_tracks_focus_and_geometry_events() {
    let mut ring = RangeEvidenceRing::<4>::new();

    ring.record(RangeEvidenceEvent::new(
        RangeEvidenceKind::SemanticNode,
        Ok(()),
    ));
    ring.record(RangeEvidenceEvent::new(
        RangeEvidenceKind::NativeUtf16,
        Err(CoreError::NativeSentinel),
    ));
    ring.record(RangeEvidenceEvent::new(
        RangeEvidenceKind::Utf16Boundary,
        Err(CoreError::InvalidUtf16),
    ));
    ring.record(RangeEvidenceEvent::new(
        RangeEvidenceKind::BidiBoundary,
        Ok(()),
    ));

    assert_eq!(ring.capacity(), 4);
    assert_eq!(ring.accepted(), 2);
    assert_eq!(ring.rejected(), 2);
    assert_eq!(ring.events().len(), 4);
}

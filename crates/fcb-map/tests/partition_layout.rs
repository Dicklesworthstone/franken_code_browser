#![forbid(unsafe_code)]

//! Deterministic retained partition layout (FCB-013.A / fcb-d6x.1).
//!
//! Verifies:
//! 1. Permutation oracle: shuffling input nodes does not change rectangles.
//! 2. Containment and no interior overlap among siblings.
//! 3. Empty/unknown files get a visible parcel; huge files are capped.
//! 4. Frozen layout revision restores identically; a different revision is stale.
//! 5. Planted negative: packing by weight would place a huge `z` before a tiny
//!    `a`; path-order packing does not.

use fcb_core::{ArenaOwnerId, CoreError, LayoutRevision, Rect2D, RootId, Size2D};
use fcb_map::{
    bounded_weight, commit_layout, interiors_overlap, HierarchySpec, LayoutOptions, NodeKind,
    NodeSpec, PartitionLayout, WeightMetric,
};

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(13).unwrap()
}

fn root() -> RootId {
    RootId::new(owner(), 1).unwrap()
}

fn rev(n: u64) -> LayoutRevision {
    LayoutRevision::new(owner(), n).unwrap()
}

fn canvas() -> Size2D {
    Size2D::new(800.0, 600.0).unwrap()
}

fn spec(nodes: Vec<NodeSpec>) -> HierarchySpec {
    HierarchySpec::new(owner(), root(), nodes).unwrap()
}

fn layout(nodes: Vec<NodeSpec>) -> PartitionLayout {
    commit_layout(rev(1), canvas(), &spec(nodes), LayoutOptions::modest()).unwrap()
}

fn parent_of(path: &[u8]) -> Option<&[u8]> {
    path.iter()
        .rposition(|byte| *byte == b'/')
        .map(|idx| &path[..idx])
}

#[test]
fn permutation_oracle_shuffled_input_is_identical() {
    let mut nodes = vec![
        NodeSpec::new(b"src".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"src/a.rs".to_vec(), NodeKind::File, Some(10)),
        NodeSpec::new(b"src/z.rs".to_vec(), NodeKind::File, Some(9_000_000)),
        NodeSpec::new(b"README.md".to_vec(), NodeKind::File, Some(100)),
        NodeSpec::new(b"vendor".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"vendor/lib.c".to_vec(), NodeKind::File, Some(50)),
    ];
    let first = layout(nodes.clone());
    nodes.reverse();
    let second = layout(nodes);
    assert_eq!(first.nodes().len(), second.nodes().len());
    for node in first.nodes() {
        let other = second.node(node.path()).expect("same path");
        assert_eq!(node.parent_local(), other.parent_local(), "path {:?}", node.path());
        assert_eq!(node.weight(), other.weight());
    }
}

#[test]
fn siblings_are_contained_and_do_not_overlap_interiors() {
    let laid = layout(vec![
        NodeSpec::new(b"a".to_vec(), NodeKind::File, Some(10)),
        NodeSpec::new(b"b".to_vec(), NodeKind::File, Some(20)),
        NodeSpec::new(b"c".to_vec(), NodeKind::File, Some(30)),
        NodeSpec::new(b"dir".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"dir/x".to_vec(), NodeKind::File, Some(5)),
        NodeSpec::new(b"dir/y".to_vec(), NodeKind::File, Some(5)),
    ]);
    let root_node = laid.node(b"").expect("root");
    let root_bounds = Rect2D::from_xywh(
        0.0,
        0.0,
        root_node.parent_local().size().width(),
        root_node.parent_local().size().height(),
    )
    .unwrap();
    let top: Vec<_> = laid
        .nodes()
        .iter()
        .filter(|n| parent_of(n.path()).is_none() && !n.path().is_empty())
        .collect();
    for child in &top {
        assert!(
            root_bounds.contains_rect(child.parent_local()),
            "child {:?} escapes root-local bounds",
            child.path()
        );
    }
    for (i, left) in top.iter().enumerate() {
        for right in top.iter().skip(i + 1) {
            assert!(
                !interiors_overlap(left.parent_local(), right.parent_local()),
                "siblings {:?} and {:?} overlap",
                left.path(),
                right.path()
            );
        }
    }
    let dir = laid.node(b"dir").unwrap();
    let dir_kids: Vec<_> = laid
        .nodes()
        .iter()
        .filter(|n| parent_of(n.path()) == Some(b"dir".as_ref()))
        .collect();
    assert_eq!(dir_kids.len(), 2);
    let dir_bounds = Rect2D::from_xywh(
        0.0,
        0.0,
        dir.parent_local().size().width(),
        dir.parent_local().size().height(),
    )
    .unwrap();
    for child in &dir_kids {
        assert!(
            dir_bounds.contains_rect(child.parent_local()),
            "nested child {:?} escapes directory-local bounds",
            child.path()
        );
    }
    assert!(!interiors_overlap(
        dir_kids[0].parent_local(),
        dir_kids[1].parent_local()
    ));
}

#[test]
fn unknown_placeholder_is_not_zero_and_huge_file_is_capped() {
    let unknown = bounded_weight(WeightMetric::CappedLogBytes, NodeKind::Placeholder, None);
    let empty = bounded_weight(WeightMetric::CappedLogBytes, NodeKind::File, Some(0));
    let huge = bounded_weight(
        WeightMetric::CappedLogBytes,
        NodeKind::File,
        Some(u64::MAX / 2),
    );
    let modest = bounded_weight(WeightMetric::CappedLogBytes, NodeKind::File, Some(1_048_576));
    assert!(unknown > 0.0);
    assert!(empty > 0.0);
    assert_eq!(huge, modest, "cap must bind so a giant file cannot dominate");

    let laid = layout(vec![
        NodeSpec::new(b"tiny.rs".to_vec(), NodeKind::File, Some(0)),
        NodeSpec::new(b"missing.bin".to_vec(), NodeKind::Placeholder, None),
        NodeSpec::new(b"generated.min.js".to_vec(), NodeKind::File, Some(10_000_000_000)),
    ]);
    let tiny = laid.node(b"tiny.rs").unwrap();
    let missing = laid.node(b"missing.bin").unwrap();
    let generated = laid.node(b"generated.min.js").unwrap();
    assert!(tiny.parent_local().size().area() > 0.0);
    assert!(missing.parent_local().size().area() > 0.0);
    assert!(generated.parent_local().size().area() > 0.0);
    assert!(
        generated.parent_local().size().area() < laid.world().size().area() * 0.9,
        "capped giant must not consume the atlas"
    );
}

#[test]
fn committed_revision_restores_and_stale_revision_is_refused() {
    let laid = layout(vec![NodeSpec::new(b"a.rs".to_vec(), NodeKind::File, Some(8))]);
    assert!(laid.restore(rev(1)).is_ok());
    let err = laid.restore(rev(2)).unwrap_err();
    assert_eq!(err.code(), CoreError::StaleLayoutRevision.code());
}

#[test]
fn path_order_not_weight_order_is_the_planted_negative() {
    let laid = layout(vec![
        NodeSpec::new(b"z.rs".to_vec(), NodeKind::File, Some(1_000_000)),
        NodeSpec::new(b"a.rs".to_vec(), NodeKind::File, Some(1)),
    ]);
    let a = laid.node(b"a.rs").unwrap();
    let z = laid.node(b"z.rs").unwrap();
    let a_origin = a.parent_local().origin();
    let z_origin = z.parent_local().origin();
    let a_first = a_origin.x() + a_origin.y();
    let z_first = z_origin.x() + z_origin.y();
    assert!(
        a_first <= z_first,
        "path order must place a.rs at or before z.rs; a weight-sorted pack would put z first"
    );
}

#[test]
fn duplicate_path_and_zero_canvas_are_refused() {
    let dup = spec(vec![
        NodeSpec::new(b"a.rs".to_vec(), NodeKind::File, Some(1)),
        NodeSpec::new(b"a.rs".to_vec(), NodeKind::File, Some(2)),
    ]);
    let err = commit_layout(rev(1), canvas(), &dup, LayoutOptions::modest()).unwrap_err();
    assert_eq!(err.code(), "LAYOUT_DUPLICATE_PATH");

    let ok_spec = spec(vec![NodeSpec::new(b"a.rs".to_vec(), NodeKind::File, Some(1))]);
    let err = commit_layout(rev(1), Size2D::ZERO, &ok_spec, LayoutOptions::modest()).unwrap_err();
    assert_eq!(err.code(), CoreError::InvalidGeometry.code());
}

#[test]
fn invalid_paths_and_slack_are_refused() {
    let bad = spec(vec![NodeSpec::new(b"/abs".to_vec(), NodeKind::File, Some(1))]);
    let err = commit_layout(rev(1), canvas(), &bad, LayoutOptions::modest()).unwrap_err();
    assert_eq!(err.code(), "LAYOUT_INVALID_PATH");

    let err = LayoutOptions::new(WeightMetric::CappedLogBytes, 1.5).unwrap_err();
    assert_eq!(err.code(), "LAYOUT_SLACK_OUT_OF_RANGE");
}

#[test]
fn empty_directory_still_receives_a_parcel() {
    let laid = layout(vec![
        NodeSpec::new(b"empty".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"file.rs".to_vec(), NodeKind::File, Some(80)),
    ]);
    let empty = laid.node(b"empty").unwrap();
    assert!(empty.parent_local().size().area() > 0.0);
    assert!(
        empty.slack().is_none(),
        "a leaf directory has no child packing, so no slack strip"
    );
}

#[test]
fn directory_slack_does_not_overlap_child_interiors() {
    let laid = layout(vec![
        NodeSpec::new(b"src".to_vec(), NodeKind::Directory, None),
        NodeSpec::new(b"src/a.rs".to_vec(), NodeKind::File, Some(10)),
        NodeSpec::new(b"src/b.rs".to_vec(), NodeKind::File, Some(10)),
    ]);
    let src = laid.node(b"src").unwrap();
    let slack = src.slack().expect("directory slack");
    for child in laid.nodes().iter().filter(|n| parent_of(n.path()) == Some(b"src".as_ref())) {
        assert!(
            !interiors_overlap(slack, child.parent_local()),
            "child {:?} intersects slack",
            child.path()
        );
    }
}

#[test]
fn owner_mismatch_on_revision_is_refused() {
    let other = ArenaOwnerId::new(99).unwrap();
    let foreign = LayoutRevision::new(other, 1).unwrap();
    let s = spec(vec![NodeSpec::new(b"a.rs".to_vec(), NodeKind::File, Some(1))]);
    let err = commit_layout(foreign, canvas(), &s, LayoutOptions::modest()).unwrap_err();
    assert_eq!(err.code(), CoreError::OwnershipMismatch.code());
}

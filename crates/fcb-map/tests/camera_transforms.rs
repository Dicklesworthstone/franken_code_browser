#![forbid(unsafe_code)]

//! Integration test suite for FCB-015.A (fcb-gzx.1):
//! Pointer-anchored camera transforms, deep-zoom precision, and inverse-projection oracle.

use std::path::PathBuf;

use fcb_core::{
    ArenaOwnerId, CameraGeneration, DisplayColorConfig, DisplayGeneration, DisplayMetrics, Point2D,
    Rect2D, Size2D,
};
use fcb_map::{Camera2D, CameraError};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;

const RUN_ID_ENV: &str = "FCB_015_RUN_ID";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        PathBuf::from(dir)
    } else {
        let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
        std::env::temp_dir().join(format!("fcb-015-receipts-{run_id}"))
    }
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_15_00_01),
        pin: SourcePin::new("0150000000000000000000000000000000000001").expect("pin valid"),
        route: RouteId::new("headless:rust").expect("route valid"),
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
    let _ = std::fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0C15_000A).unwrap()
}

fn test_display(owner_id: ArenaOwnerId, id: u64) -> DisplayMetrics {
    DisplayMetrics::new(
        2.0,
        Size2D::new(1920.0, 1080.0).unwrap(),
        DisplayColorConfig::Srgb,
        DisplayGeneration::new(owner_id, id).unwrap(),
    )
    .unwrap()
}

fn camera(owner_id: ArenaOwnerId, generation: u64, x: f64, y: f64, scale: f64) -> Camera2D {
    Camera2D::new(
        CameraGeneration::new(owner_id, generation).unwrap(),
        test_display(owner_id, generation),
        Point2D::new(x, y).unwrap(),
        scale,
    )
    .unwrap()
}

#[test]
fn test_pointer_anchored_zoom_inverse_projection_oracle() {
    let own = owner();
    let cam = camera(own, 1, 120.0, 240.0, 1.0);

    let test_anchors = [
        Point2D::new(0.0, 0.0).unwrap(),
        Point2D::new(960.0, 540.0).unwrap(),
        Point2D::new(1920.0, 1080.0).unwrap(),
        Point2D::new(450.5, 320.25).unwrap(),
        Point2D::new(123.456, 789.012).unwrap(),
    ];

    let zoom_factors = [0.1, 0.25, 0.5, 0.8, 1.25, 1.41421356, 2.0, 5.0, 10.0];

    for &anchor in &test_anchors {
        // World point before zoom
        let world_point = cam.logical_to_local(anchor).unwrap();

        for &factor in &zoom_factors {
            let zoomed = cam.zoom_at(anchor, factor).unwrap();

            // World point projected under new transform must project back to the same anchor point
            let screen_after = zoomed.local_to_logical(world_point).unwrap();

            assert!(
                (screen_after.x() - anchor.x()).abs() < 1e-10,
                "anchor x mismatch: expected {}, got {}, diff {}",
                anchor.x(),
                screen_after.x(),
                (screen_after.x() - anchor.x()).abs()
            );
            assert!(
                (screen_after.y() - anchor.y()).abs() < 1e-10,
                "anchor y mismatch: expected {}, got {}, diff {}",
                anchor.y(),
                screen_after.y(),
                (screen_after.y() - anchor.y()).abs()
            );
        }
    }

    record_receipt(
        "fcb_015_camera_inverse_projection_zoom",
        Effect::Succeeded,
        "world point under gesture anchor invariant preserved exactly across all zoom factors",
    );
}

#[test]
fn test_deep_zoom_precision_and_repeated_pinch_drift() {
    let own = owner();
    let mut cam = camera(own, 1, 300.0, 500.0, 1.0);
    let anchor = Point2D::new(960.0, 540.0).unwrap();

    // 50 cycles of zoom in 2x, zoom out 0.5x
    for _ in 0..50 {
        cam = cam.zoom_at(anchor, 2.0).unwrap();
        cam = cam.zoom_at(anchor, 0.5).unwrap();
    }

    assert!(
        (cam.points_per_unit() - 1.0).abs() < 1e-12,
        "scale drift after 50 pinch cycles"
    );
    assert!(
        (cam.origin().x() - 300.0).abs() < 1e-6,
        "origin x drift: {}",
        (cam.origin().x() - 300.0).abs()
    );
    assert!(
        (cam.origin().y() - 500.0).abs() < 1e-6,
        "origin y drift: {}",
        (cam.origin().y() - 500.0).abs()
    );

    // Deep zoom: zoom in 30x by 2.0 -> scale > 10^9
    for _ in 0..30 {
        cam = cam.zoom_at(anchor, 2.0).unwrap();
    }
    assert!(cam.points_per_unit() > 1_000_000_000.0);
    let vp = cam.local_viewport().unwrap();
    assert!(vp.size().width() > 0.0 && vp.size().width().is_finite());
    assert!(vp.size().height() > 0.0 && vp.size().height().is_finite());

    // Deep zoom out: 30x by 0.5 -> scale back to 1.0
    for _ in 0..30 {
        cam = cam.zoom_at(anchor, 0.5).unwrap();
    }
    assert!(
        (cam.origin().x() - 300.0).abs() < 1e-6,
        "origin x drift after deep zoom: {}",
        (cam.origin().x() - 300.0).abs()
    );
    assert!(
        (cam.origin().y() - 500.0).abs() < 1e-6,
        "origin y drift after deep zoom: {}",
        (cam.origin().y() - 500.0).abs()
    );

    record_receipt(
        "fcb_015_camera_deep_zoom_pinch_drift",
        Effect::Succeeded,
        "deep zoom to 10^9 and 50 pinch cycles drift strictly bounded under 1e-6",
    );
}

#[test]
fn test_pan_tracks_gesture_in_logical_points() {
    let own = owner();
    let cam = camera(own, 1, 0.0, 0.0, 2.0);

    let test_point = Point2D::new(50.0, 50.0).unwrap();
    let initial_screen = cam.local_to_logical(test_point).unwrap();

    let pan_delta = Point2D::new(100.0, -50.0).unwrap();
    let panned = cam.pan(pan_delta).unwrap();

    let after_screen = panned.local_to_logical(test_point).unwrap();

    assert!(
        (after_screen.x() - (initial_screen.x() + 100.0)).abs() < 1e-10,
        "screen x did not shift by pan delta"
    );
    assert!(
        (after_screen.y() - (initial_screen.y() - 50.0)).abs() < 1e-10,
        "screen y did not shift by pan delta"
    );

    record_receipt(
        "fcb_015_camera_pan_gesture_points",
        Effect::Succeeded,
        "pan tracks gesture in logical points exactly",
    );
}

#[test]
fn test_rebased_f32_precision_and_refusal() {
    let own = owner();
    let cam = camera(own, 1, 0.0, 0.0, 1.0);

    // Inside viewport
    let valid_point = Point2D::new(100.0, 100.0).unwrap();
    let screen = cam.checked_screen_f32(valid_point).unwrap();
    assert_eq!(f64::from(screen.x), 100.0);
    assert_eq!(f64::from(screen.y), 100.0);

    // Rebasing subtraction that needs > 24 mantissa bits refuses precision loss
    let tricky_cam = Camera2D::new(
        CameraGeneration::new(own, 2).unwrap(),
        test_display(own, 2),
        Point2D::new(0.5, 0.5).unwrap(),
        1.0,
    )
    .unwrap();
    let local = Point2D::new(16777216.0, 0.0).unwrap();
    assert_eq!(
        tricky_cam.checked_screen_f32(local).unwrap_err(),
        CameraError::PrecisionLost
    );

    record_receipt(
        "fcb_015_camera_rebased_f32_precision",
        Effect::Succeeded,
        "rebased f32 conversions exact within viewport and refuse precision loss",
    );
}

#[test]
fn test_negative_control_oracle_detects_projection_drift() {
    let own = owner();
    let cam = camera(own, 1, 100.0, 100.0, 1.0);
    let anchor = Point2D::new(500.0, 500.0).unwrap();
    let world_point = cam.logical_to_local(anchor).unwrap();

    // Planted defect: perturb zoomed camera origin by +0.05
    let zoomed = cam.zoom_at(anchor, 2.0).unwrap();
    let defective_origin = Point2D::new(zoomed.origin().x() + 0.05, zoomed.origin().y()).unwrap();
    let defective_cam = Camera2D::new(
        zoomed.generation(),
        zoomed.display(),
        defective_origin,
        zoomed.points_per_unit(),
    )
    .unwrap();

    let screen_after = defective_cam.local_to_logical(world_point).unwrap();
    let drift = (screen_after.x() - anchor.x()).abs();

    // Oracle detects drift > 1e-10
    assert!(drift > 0.01, "oracle must detect planted 0.05 drift");

    record_receipt(
        "fcb_015_negative_control_drift_detection",
        Effect::Succeeded,
        "inverse projection oracle reliably detects planted 0.05 pt camera drift",
    );
}

#[test]
fn test_camera_invalid_inputs_rejected() {
    let own = owner();
    let cam = camera(own, 1, 0.0, 0.0, 1.0);
    let anchor = Point2D::new(100.0, 100.0).unwrap();

    // Negative zoom factor
    assert_eq!(cam.zoom_at(anchor, -1.0), Err(CameraError::InvalidGeometry));
    // Zero zoom factor
    assert_eq!(cam.zoom_at(anchor, 0.0), Err(CameraError::InvalidGeometry));
    // NaN zoom factor
    assert_eq!(
        cam.zoom_at(anchor, f64::NAN),
        Err(CameraError::InvalidGeometry)
    );
    // Infinite zoom factor
    assert_eq!(
        cam.zoom_at(anchor, f64::INFINITY),
        Err(CameraError::InvalidGeometry)
    );

    // Empty rect in project_clipped returns Err(InvalidGeometry)
    let empty_rect = Rect2D::from_xywh(0.0, 0.0, 0.0, 0.0).unwrap();
    assert_eq!(cam.project_clipped(empty_rect), Err(CameraError::InvalidGeometry));

    // Disjoint rect outside viewport returns Ok(None)
    let disjoint_rect = Rect2D::from_xywh(10000.0, 10000.0, 50.0, 50.0).unwrap();
    assert_eq!(cam.project_clipped(disjoint_rect), Ok(None));

    record_receipt(
        "fcb_015_camera_invalid_inputs_rejected",
        Effect::Succeeded,
        "invalid zoom factors and degenerate geometries are safely refused",
    );
}

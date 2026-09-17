#![forbid(unsafe_code)]

//! Integration test suite for FCB-015.B (fcb-gzx.2):
//! Interruptible navigation history, semantic endpoints, and bounded camera flight.

use fcb_core::{
    ArenaOwnerId, CameraGeneration, DisplayColorConfig, DisplayGeneration, DisplayMetrics, Point2D,
    Size2D,
};
use fcb_map::{
    Camera2D, MotionPreference, NavigationError, NavigationFlight, NavigationFlightConfig,
    NavigationHistory, NavigationReason, DEFAULT_FLIGHT_DURATION_NANOS,
    DEFAULT_MAX_STEP_DELTA_NANOS,
};

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0C15_000B).unwrap()
}

fn foreign_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0xDEAD_BEEF).unwrap()
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
fn test_semantic_endpoints_and_history_branching() {
    let own = owner();
    let c_root = camera(own, 1, 0.0, 0.0, 1.0);
    let c_dir1 = camera(own, 2, 250.0, 120.0, 2.0);
    let c_dir2 = camera(own, 3, 500.0, 300.0, 4.0);
    let c_dir3 = camera(own, 4, 100.0, -50.0, 1.5);

    let config = NavigationFlightConfig::new(
        DEFAULT_FLIGHT_DURATION_NANOS,
        DEFAULT_MAX_STEP_DELTA_NANOS,
        MotionPreference::ReducedMotion,
    )
    .unwrap();

    let mut history = NavigationHistory::with_initial_camera(own, c_root, config).unwrap();
    assert_eq!(history.current_camera().unwrap(), c_root);
    assert!(!history.can_go_back());
    assert!(!history.can_go_forward());

    // Push semantic endpoints
    history
        .push_semantic_endpoint(c_dir1, NavigationReason::FitSelection, 100)
        .unwrap();
    history
        .push_semantic_endpoint(c_dir2, NavigationReason::FocusNode, 200)
        .unwrap();

    assert_eq!(history.past_count(), 2);
    assert_eq!(history.future_count(), 0);
    assert!(history.can_go_back());

    // Back to c_dir1
    let back1 = history.navigate_back(300).unwrap();
    assert_eq!(back1, c_dir1);
    assert_eq!(history.past_count(), 1);
    assert_eq!(history.future_count(), 1);

    // Back to c_root
    let back2 = history.navigate_back(400).unwrap();
    assert_eq!(back2, c_root);
    assert_eq!(history.past_count(), 0);
    assert_eq!(history.future_count(), 2);
    assert!(!history.can_go_back());

    // Forward to c_dir1
    let fwd1 = history.navigate_forward(500).unwrap();
    assert_eq!(fwd1, c_dir1);
    assert_eq!(history.past_count(), 1);
    assert_eq!(history.future_count(), 1);

    // Branching: pushing c_dir3 while at c_dir1 must truncate future (c_dir2 is dropped)
    history
        .push_semantic_endpoint(c_dir3, NavigationReason::FitProject, 600)
        .unwrap();
    assert_eq!(history.current_camera().unwrap(), c_dir3);
    assert_eq!(history.past_count(), 2); // c_root, c_dir1
    assert_eq!(history.future_count(), 0, "future branch truncated");
    assert!(!history.can_go_forward());

    // Back returns to c_dir1, then c_root
    assert_eq!(history.navigate_back(700).unwrap(), c_dir1);
    assert_eq!(history.navigate_back(800).unwrap(), c_root);
}

#[test]
fn test_reduced_motion_transitions_directly() {
    let own = owner();
    let c1 = camera(own, 1, 0.0, 0.0, 1.0);
    let c2 = camera(own, 2, 800.0, 600.0, 3.5);

    let config = NavigationFlightConfig::new(
        DEFAULT_FLIGHT_DURATION_NANOS,
        DEFAULT_MAX_STEP_DELTA_NANOS,
        MotionPreference::ReducedMotion,
    )
    .unwrap();

    let flight = NavigationFlight::new(c1, c2, NavigationReason::FitSelection, config).unwrap();
    assert!(flight.is_complete());
    assert_eq!(flight.current_camera().unwrap(), c2);
}

#[test]
fn test_sleep_delta_clamping_prevents_simulation_blowup() {
    let own = owner();
    let c1 = camera(own, 1, 0.0, 0.0, 1.0);
    let c2 = camera(own, 2, 1200.0, 800.0, 8.0);

    let max_step = 50_000_000; // 50ms clamp
    let config = NavigationFlightConfig::new(
        250_000_000, // 250ms total flight
        max_step,
        MotionPreference::Normal,
    )
    .unwrap();

    let mut flight = NavigationFlight::new(c1, c2, NavigationReason::FitProject, config).unwrap();

    // Huge sleep delta: 30 minutes of elapsed time
    let huge_delta = 30 * 60 * 1_000_000_000_u64;
    let stepped = flight.step(huge_delta).unwrap();

    // Must be clamped to 50ms (0.05s / 0.25s = 20% progress), NOT complete!
    assert_eq!(flight.elapsed_nanos(), max_step);
    assert!(!flight.is_complete());
    assert!(stepped.origin().x() < 500.0);
}

#[test]
fn test_flight_interruption_hands_control_back_immediately() {
    let own = owner();
    let c_start = camera(own, 1, 0.0, 0.0, 1.0);
    let c_dest = camera(own, 2, 1000.0, 1000.0, 10.0);

    let mut history = NavigationHistory::with_initial_camera(
        own,
        c_start,
        NavigationFlightConfig::default(),
    )
    .unwrap();

    history
        .push_semantic_endpoint(c_dest, NavigationReason::FitSelection, 10)
        .unwrap();
    assert!(history.active_flight().is_some());

    // Advance flight by 100ms
    let midway = history.step_flight(100_000_000).unwrap().unwrap();
    assert!(midway.origin().x() > 0.0 && midway.origin().x() < 1000.0);

    // User gesture arrives -> interrupt flight
    let interrupted = history.interrupt_flight().unwrap().unwrap();
    assert_eq!(interrupted, midway);
    assert!(history.active_flight().is_none());

    // History now points to the interrupted position so next gesture continues seamlessly
    assert_eq!(history.current_camera().unwrap(), interrupted);

    // User pans by 25 points
    let panned = interrupted.pan(Point2D::new(25.0, 0.0).unwrap()).unwrap();
    assert!((panned.origin().x() - (interrupted.origin().x() - 25.0 / interrupted.points_per_unit())).abs() < 1e-10);
}

#[test]
fn test_inverse_projection_oracle_during_all_flight_stages() {
    let own = owner();
    let c1 = camera(own, 1, -500.0, 300.0, 0.25);
    let c2 = camera(own, 2, 750.0, -400.0, 32.0);

    let config = NavigationFlightConfig::default();
    let flight = NavigationFlight::new(c1, c2, NavigationReason::FitProject, config).unwrap();

    let check_points = [
        Point2D::new(0.0, 0.0).unwrap(),
        Point2D::new(960.0, 540.0).unwrap(),
        Point2D::new(1919.0, 1079.0).unwrap(),
        Point2D::new(314.15, 271.82).unwrap(),
    ];

    for step_num in 0..=20 {
        let dt = (config.duration_nanos / 20) * step_num;
        let mut clone = flight.clone();
        let cam = clone.step(dt).unwrap();

        for &pt in &check_points {
            let local = cam.logical_to_local(pt).unwrap();
            let round_trip = cam.local_to_logical(local).unwrap();
            assert!(
                (round_trip.x() - pt.x()).abs() < 1e-8,
                "x drift at step {step_num}"
            );
            assert!(
                (round_trip.y() - pt.y()).abs() < 1e-8,
                "y drift at step {step_num}"
            );
        }
    }
}

#[test]
fn test_deep_zoom_drift_bounded_over_40_pinch_cycles() {
    let own = owner();
    let mut cam = camera(own, 1, 42.0, 84.0, 1.0);
    let centroid = Point2D::new(960.0, 540.0).unwrap();

    // Zoom in 30x by 2.0 (scale ~ 10^9)
    for _ in 0..30 {
        cam = cam.zoom_at(centroid, 2.0).unwrap();
    }
    assert!(cam.points_per_unit() > 1_000_000_000.0);

    // Zoom out 30x by 0.5
    for _ in 0..30 {
        cam = cam.zoom_at(centroid, 0.5).unwrap();
    }

    // Drift must remain sub-micro-point
    assert!((cam.origin().x() - 42.0).abs() < 1e-6);
    assert!((cam.origin().y() - 84.0).abs() < 1e-6);
}

#[test]
fn test_deterministic_keyboard_replay_equality() {
    let own = owner();
    let c0 = camera(own, 1, 0.0, 0.0, 1.0);
    let c1 = camera(own, 2, 100.0, 50.0, 2.0);
    let c2 = camera(own, 3, 200.0, 150.0, 4.0);
    let c3 = camera(own, 4, -80.0, 90.0, 1.25);

    let replay = || -> Camera2D {
        let config = NavigationFlightConfig::new(
            100_000_000,
            25_000_000,
            MotionPreference::ReducedMotion,
        )
        .unwrap();

        let mut h = NavigationHistory::with_initial_camera(own, c0, config).unwrap();
        h.push_semantic_endpoint(c1, NavigationReason::KeyboardReplay, 10).unwrap();
        h.push_semantic_endpoint(c2, NavigationReason::KeyboardReplay, 20).unwrap();
        let _ = h.navigate_back(30).unwrap();
        h.push_semantic_endpoint(c3, NavigationReason::KeyboardReplay, 40).unwrap();
        let _ = h.navigate_back(50).unwrap();
        let end = h.navigate_forward(60).unwrap();
        end
    };

    let run_a = replay();
    let run_b = replay();
    assert_eq!(run_a, run_b);
    assert_eq!(run_a, c3);
}

#[test]
fn test_negative_controls_owner_mismatch_and_empty_history() {
    let own = owner();
    let foreign = foreign_owner();

    let local_cam = camera(own, 1, 0.0, 0.0, 1.0);
    let foreign_cam = camera(foreign, 1, 0.0, 0.0, 1.0);

    // Cross-owner flight rejected
    assert_eq!(
        NavigationFlight::new(
            local_cam,
            foreign_cam,
            NavigationReason::FitSelection,
            NavigationFlightConfig::default(),
        ),
        Err(NavigationError::OwnerMismatch)
    );

    let mut h = NavigationHistory::with_initial_camera(
        own,
        local_cam,
        NavigationFlightConfig::default(),
    )
    .unwrap();

    // Push foreign owner rejected
    assert_eq!(
        h.push_semantic_endpoint(foreign_cam, NavigationReason::FocusNode, 1),
        Err(NavigationError::OwnerMismatch)
    );

    // Initial state cannot navigate back or forward
    assert_eq!(h.navigate_back(2), Err(NavigationError::EmptyHistory));
    assert_eq!(h.navigate_forward(3), Err(NavigationError::EmptyHistory));

    // Zero durations rejected
    assert_eq!(
        NavigationFlightConfig::new(0, 50, MotionPreference::Normal),
        Err(NavigationError::InvalidDuration)
    );
    assert_eq!(
        NavigationFlightConfig::new(50, 0, MotionPreference::Normal),
        Err(NavigationError::InvalidDuration)
    );
}

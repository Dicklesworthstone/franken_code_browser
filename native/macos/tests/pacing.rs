//! Focused tests for display-link pacing and single drawable ownership (FCB-006.A).

#![forbid(unsafe_code)]

use franken_macos::pacing::{
    DEFAULT_MAX_IN_FLIGHT, DisplayLinkAvailability, DisplayLinkRoute, DisplayPacingEngine,
    PACING_EVENT_RING_CAPACITY, PacingConfig, PacingCounters, PacingError, PacingEventType,
    PacingOutcome,
};
use franken_macos::view_binding::{DrawablePath, NativeViewBinding};
use franken_macos::windowing::{NativeWindow, Rect, WindowStyle};
use franken_macos::{
    ColorSpace, DisplayMetrics, HostTargetDescriptor, MainThreadToken, MetalDevice, MetalLayer,
    PixelFormat,
};

fn main_token() -> Option<MainThreadToken> {
    MainThreadToken::capture_current().ok()
}

fn valid_host(scale: f64, width: f64, height: f64) -> HostTargetDescriptor {
    HostTargetDescriptor {
        pixel_format: PixelFormat::Bgra8Unorm,
        color_space: ColorSpace::Srgb,
        sample_count: 1,
        metrics: DisplayMetrics {
            backing_scale: scale,
            drawable_width: width,
            drawable_height: height,
        },
    }
}

#[test]
fn pacing_config_validation() {
    let def = PacingConfig::default();
    assert_eq!(def.max_in_flight, DEFAULT_MAX_IN_FLIGHT);
    assert_eq!(def.target_fps, 60);
    assert_eq!(def.preferred_latency_frames, 2);
    assert!(def.validate().is_ok());

    let bad_max_zero = PacingConfig {
        max_in_flight: 0,
        ..def
    };
    assert_eq!(bad_max_zero.validate(), Err(PacingError::InvalidConfig));

    let bad_max_excess = PacingConfig {
        max_in_flight: 4,
        ..def
    };
    assert_eq!(bad_max_excess.validate(), Err(PacingError::InvalidConfig));

    let bad_fps_zero = PacingConfig {
        target_fps: 0,
        ..def
    };
    assert_eq!(bad_fps_zero.validate(), Err(PacingError::InvalidConfig));

    let bad_fps_excess = PacingConfig {
        target_fps: 241,
        ..def
    };
    assert_eq!(bad_fps_excess.validate(), Err(PacingError::InvalidConfig));

    let bad_lat_zero = PacingConfig {
        preferred_latency_frames: 0,
        ..def
    };
    assert_eq!(bad_lat_zero.validate(), Err(PacingError::InvalidConfig));

    let bad_lat_excess = PacingConfig {
        preferred_latency_frames: 4,
        ..def
    };
    assert_eq!(bad_lat_excess.validate(), Err(PacingError::InvalidConfig));
}

#[test]
fn pacing_constants_and_default_counters() {
    assert_eq!(DEFAULT_MAX_IN_FLIGHT, 2);
    assert_eq!(PACING_EVENT_RING_CAPACITY, 64);
    let c = PacingCounters::default();
    assert_eq!(c.ticks, 0);
    assert_eq!(c.frames_acquired, 0);
    assert_eq!(c.frames_presented, 0);
    assert_eq!(c.frames_dropped, 0);
    assert_eq!(c.deferred_full_queue, 0);
    assert_eq!(c.pauses, 0);
    assert_eq!(c.resumes, 0);
    assert_eq!(c.invalidations, 0);
    assert_eq!(c.rejected_competing_owner, 0);
    assert_eq!(c.rejected_stale_owner, 0);
}

#[test]
fn sdk_availability_probe_contract() {
    let report = DisplayLinkAvailability::probe();
    if cfg!(target_os = "macos") {
        assert!(
            report.is_available,
            "CAMetalDisplayLink should be available on macOS 14+"
        );
        assert_eq!(report.route, DisplayLinkRoute::CAMetalDisplayLink);
    } else {
        assert!(!report.is_available);
        assert_eq!(report.route, DisplayLinkRoute::ManualPacing);
    }
}

#[test]
fn negative_control_oracle_detects_competing_presentation_owner() {
    let Some(token) = main_token() else {
        return;
    };
    let win1 = NativeWindow::create(
        token,
        "fcb-pxg-comp-1",
        Rect::at_origin(64.0, 64.0),
        WindowStyle::standard(),
    )
    .expect("win1");
    let win2 = NativeWindow::create(
        token,
        "fcb-pxg-comp-2",
        Rect::at_origin(64.0, 64.0),
        WindowStyle::standard(),
    )
    .expect("win2");
    let device = MetalDevice::system_default(token).expect("device");
    let host = valid_host(2.0, 64.0, 64.0);

    let (b1, o1) = NativeViewBinding::attach(
        token,
        &win1,
        MetalLayer::new(token).unwrap(),
        &device,
        host,
        DrawablePath::HostOwned,
    )
    .expect("b1");
    let (b2, o2) = NativeViewBinding::attach(
        token,
        &win2,
        MetalLayer::new(token).unwrap(),
        &device,
        host,
        DrawablePath::HostOwned,
    )
    .expect("b2");

    let e1 = DisplayPacingEngine::bind(token, &b1, o1, PacingConfig::default()).expect("e1");
    let e2 = DisplayPacingEngine::bind(token, &b2, o2, PacingConfig::default()).expect("e2");

    e1.request_frame();
    let outcome = e1.tick(token, &b1, 0.0, 1.0 / 60.0).expect("tick");
    let PacingOutcome::FrameStarted(frame1) = outcome else {
        panic!("expected FrameStarted");
    };

    // Negative control oracle: presenting frame1 with competing engine e2 must fail
    let err = frame1.present(&e2);
    assert_eq!(err, Err(PacingError::CompetingPresentationOwner));
    assert_eq!(e2.counters().rejected_competing_owner, 1);
    assert!(
        e2.event_records()
            .iter()
            .any(|r| r.event_type == PacingEventType::RejectedCompetingOwner)
    );
}

#[test]
fn negative_control_oracle_detects_stale_presentation_owner() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-pxg-stale",
        Rect::at_origin(64.0, 64.0),
        WindowStyle::standard(),
    )
    .expect("win");
    let device = MetalDevice::system_default(token).expect("device");
    let host = valid_host(2.0, 64.0, 64.0);

    let (binding, owner1) = NativeViewBinding::attach(
        token,
        &win,
        MetalLayer::new(token).unwrap(),
        &device,
        host,
        DrawablePath::HostOwned,
    )
    .expect("binding");
    let engine = DisplayPacingEngine::bind(token, &binding, owner1, PacingConfig::default())
        .expect("engine");

    // Detach and re-attach: advances generation from 1 to 3
    binding.detach(token).expect("detach");
    let _new_owner = binding
        .reattach(token, &device, host, DrawablePath::HostOwned)
        .expect("reattach");
    assert_eq!(binding.generation(), 3);

    // Negative control oracle: tick with stale generation 1 owner must fail
    engine.request_frame();
    let err = engine.tick(token, &binding, 0.0, 1.0 / 60.0);
    assert_eq!(err.unwrap_err(), PacingError::StalePresentationOwner);
    assert_eq!(engine.counters().rejected_stale_owner, 1);
}

#[test]
fn two_frame_starting_policy_defers_when_saturated() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-pxg-2frame",
        Rect::at_origin(64.0, 64.0),
        WindowStyle::standard(),
    )
    .expect("win");
    let device = MetalDevice::system_default(token).expect("device");
    let host = valid_host(2.0, 64.0, 64.0);

    let (binding, owner) = NativeViewBinding::attach(
        token,
        &win,
        MetalLayer::new(token).unwrap(),
        &device,
        host,
        DrawablePath::HostOwned,
    )
    .expect("binding");
    let config = PacingConfig {
        max_in_flight: 2,
        target_fps: 60,
        preferred_latency_frames: 2,
    };
    let engine = DisplayPacingEngine::bind(token, &binding, owner, config).expect("engine");

    // Frame 1
    engine.request_frame();
    let outcome1 = engine
        .tick(token, &binding, 0.0, 1.0 / 60.0)
        .expect("tick 1");
    let PacingOutcome::FrameStarted(f1) = outcome1 else {
        panic!("expected FrameStarted")
    };
    assert_eq!(engine.in_flight_count(), 1);

    // Frame 2
    engine.request_frame();
    let outcome2 = engine
        .tick(token, &binding, 1.0 / 60.0, 2.0 / 60.0)
        .expect("tick 2");
    let PacingOutcome::FrameStarted(f2) = outcome2 else {
        panic!("expected FrameStarted")
    };
    assert_eq!(engine.in_flight_count(), 2);

    // Frame 3: Saturated! Must defer.
    engine.request_frame();
    let outcome3 = engine
        .tick(token, &binding, 2.0 / 60.0, 3.0 / 60.0)
        .expect("tick 3");
    match outcome3 {
        PacingOutcome::DeferredFullQueue => {}
        other => panic!("expected DeferredFullQueue, got {other:?}"),
    }
    assert_eq!(engine.in_flight_count(), 2);
    assert_eq!(engine.counters().deferred_full_queue, 1);

    // Present frame 1: in_flight decrements to 1
    f1.present(&engine).expect("present f1");
    assert_eq!(engine.in_flight_count(), 1);
    assert_eq!(engine.counters().frames_presented, 1);

    // Now Frame 3 can start:
    engine.request_frame();
    let outcome4 = engine
        .tick(token, &binding, 3.0 / 60.0, 4.0 / 60.0)
        .expect("tick 4");
    let PacingOutcome::FrameStarted(f3) = outcome4 else {
        panic!("expected FrameStarted")
    };
    assert_eq!(engine.in_flight_count(), 2);

    // Present remaining frames
    f2.present(&engine).expect("present f2");
    f3.present(&engine).expect("present f3");
    assert_eq!(engine.in_flight_count(), 0);
    assert_eq!(engine.counters().frames_presented, 3);
}

#[test]
fn stationary_pause_avoids_continuous_idle_redraw() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-pxg-pause",
        Rect::at_origin(64.0, 64.0),
        WindowStyle::standard(),
    )
    .expect("win");
    let device = MetalDevice::system_default(token).expect("device");
    let host = valid_host(2.0, 64.0, 64.0);

    let (binding, owner) = NativeViewBinding::attach(
        token,
        &win,
        MetalLayer::new(token).unwrap(),
        &device,
        host,
        DrawablePath::HostOwned,
    )
    .expect("binding");
    let engine =
        DisplayPacingEngine::bind(token, &binding, owner, PacingConfig::default()).expect("engine");

    // Initially needs_render is true (initial frame)
    let outcome = engine
        .tick(token, &binding, 0.0, 1.0 / 60.0)
        .expect("initial tick");
    let PacingOutcome::FrameStarted(f0) = outcome else {
        panic!("expected FrameStarted")
    };
    f0.present(&engine).expect("present f0");

    // Stationary state: ticks without request_frame() are Idle.
    for i in 1..=5 {
        let t = i as f64 * (1.0 / 60.0);
        let outcome = engine
            .tick(token, &binding, t, t + 1.0 / 60.0)
            .expect("idle tick");
        match outcome {
            PacingOutcome::Idle => {}
            other => panic!("expected Idle in stationary state, got {other:?}"),
        }
    }
    assert_eq!(engine.counters().frames_acquired, 1);
    assert_eq!(engine.counters().ticks, 6);

    // Pause the engine (e.g. window occluded)
    engine.pause();
    assert!(engine.is_paused());
    assert_eq!(engine.counters().pauses, 1);

    // Ticks while paused return Paused
    let outcome = engine
        .tick(token, &binding, 0.1, 0.116)
        .expect("paused tick");
    match outcome {
        PacingOutcome::Paused => {}
        other => panic!("expected Paused, got {other:?}"),
    }

    // Resume the engine
    engine.resume();
    assert!(!engine.is_paused());
    assert_eq!(engine.counters().resumes, 1);

    // Resumed engine automatically requests a frame
    let outcome = engine
        .tick(token, &binding, 0.12, 0.136)
        .expect("resumed tick");
    let PacingOutcome::FrameStarted(f1) = outcome else {
        panic!("expected FrameStarted after resume")
    };
    f1.present(&engine).expect("present f1");
    assert_eq!(engine.counters().frames_acquired, 2);
    assert_eq!(engine.counters().frames_presented, 2);
}

#[test]
fn simulated_60hz_and_120hz_traces() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-pxg-traces",
        Rect::at_origin(64.0, 64.0),
        WindowStyle::standard(),
    )
    .expect("win");
    let device = MetalDevice::system_default(token).expect("device");
    let host = valid_host(2.0, 64.0, 64.0);

    let (binding, owner) = NativeViewBinding::attach(
        token,
        &win,
        MetalLayer::new(token).unwrap(),
        &device,
        host,
        DrawablePath::HostOwned,
    )
    .expect("binding");
    let config = PacingConfig {
        max_in_flight: 2,
        target_fps: 120,
        preferred_latency_frames: 2,
    };
    let engine = DisplayPacingEngine::bind(token, &binding, owner, config).expect("engine");

    // 120 Hz trace (8.333 ms per tick) over 24 frames
    let delta = 1.0 / 120.0;
    for i in 0..24 {
        let t = i as f64 * delta;
        engine.request_frame();
        let outcome = engine
            .tick(token, &binding, t, t + delta * 2.0)
            .expect("120Hz tick");
        let PacingOutcome::FrameStarted(frame) = outcome else {
            panic!("expected FrameStarted at tick {i}");
        };
        assert_eq!(frame.target_timestamp(), t);
        frame.present(&engine).expect("present 120Hz frame");
    }

    assert_eq!(engine.counters().ticks, 24);
    assert_eq!(engine.counters().frames_acquired, 24);
    assert_eq!(engine.counters().frames_presented, 24);
    assert_eq!(engine.in_flight_count(), 0);
}

#[test]
fn dropped_frame_cleans_up_in_flight_count() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-pxg-drop",
        Rect::at_origin(64.0, 64.0),
        WindowStyle::standard(),
    )
    .expect("win");
    let device = MetalDevice::system_default(token).expect("device");
    let host = valid_host(2.0, 64.0, 64.0);

    let (binding, owner) = NativeViewBinding::attach(
        token,
        &win,
        MetalLayer::new(token).unwrap(),
        &device,
        host,
        DrawablePath::HostOwned,
    )
    .expect("binding");
    let engine =
        DisplayPacingEngine::bind(token, &binding, owner, PacingConfig::default()).expect("engine");

    engine.request_frame();
    let outcome = engine.tick(token, &binding, 0.0, 1.0 / 60.0).expect("tick");
    let PacingOutcome::FrameStarted(frame) = outcome else {
        panic!("expected FrameStarted")
    };
    let frame_id = frame.frame_id();
    assert_eq!(engine.in_flight_count(), 1);

    // Drop frame without calling present()
    drop(frame);

    // in_flight is decremented cleanly
    assert_eq!(engine.in_flight_count(), 0);
    engine.record_frame_dropped(frame_id);
    assert_eq!(engine.counters().frames_dropped, 1);
    assert!(
        engine
            .event_records()
            .iter()
            .any(|r| r.event_type == PacingEventType::FrameDropped)
    );
}

#[test]
fn non_blocking_teardown_and_invalidation() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-pxg-inval",
        Rect::at_origin(64.0, 64.0),
        WindowStyle::standard(),
    )
    .expect("win");
    let device = MetalDevice::system_default(token).expect("device");
    let host = valid_host(2.0, 64.0, 64.0);

    let (binding, owner) = NativeViewBinding::attach(
        token,
        &win,
        MetalLayer::new(token).unwrap(),
        &device,
        host,
        DrawablePath::HostOwned,
    )
    .expect("binding");
    let engine =
        DisplayPacingEngine::bind(token, &binding, owner, PacingConfig::default()).expect("engine");

    assert!(!engine.is_invalidated());
    engine.invalidate();
    assert!(engine.is_invalidated());
    assert_eq!(engine.counters().invalidations, 1);

    // Ticks after invalidation fail immediately
    let err = engine.tick(token, &binding, 0.0, 1.0 / 60.0);
    assert_eq!(err.unwrap_err(), PacingError::Invalidated);
}

#[test]
fn event_ring_capacity_and_rotation() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-pxg-ring",
        Rect::at_origin(64.0, 64.0),
        WindowStyle::standard(),
    )
    .expect("win");
    let device = MetalDevice::system_default(token).expect("device");
    let host = valid_host(2.0, 64.0, 64.0);

    let (binding, owner) = NativeViewBinding::attach(
        token,
        &win,
        MetalLayer::new(token).unwrap(),
        &device,
        host,
        DrawablePath::HostOwned,
    )
    .expect("binding");
    let engine =
        DisplayPacingEngine::bind(token, &binding, owner, PacingConfig::default()).expect("engine");

    // Generate 100 idle ticks (exceeding ring capacity of 64)
    for i in 0..100 {
        let _ = engine.tick(token, &binding, i as f64 * 0.016, (i + 1) as f64 * 0.016);
    }

    assert_eq!(engine.counters().ticks, 100);
    let records = engine.event_records();
    assert_eq!(records.len(), PACING_EVENT_RING_CAPACITY);
}

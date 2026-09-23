//! Focused tests for demand-driven frame scheduling and camera coalescing (FCB-006.B).

#![forbid(unsafe_code)]

use franken_macos::pacing::{DisplayPacingEngine, PacingConfig};
use franken_macos::scheduler::{
    CameraSnapshot, FrameRequestReason, FrameScheduler, SCHEDULER_EVENT_RING_CAPACITY,
    SchedulerEventType, SchedulerOutcome, WindowVisibility,
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
fn window_visibility_predicates() {
    assert!(WindowVisibility::VisibleActive.should_render());
    assert!(WindowVisibility::VisibleInactive.should_render());
    assert!(!WindowVisibility::Occluded.should_render());
    assert!(!WindowVisibility::Hidden.should_render());
}

#[test]
fn frame_request_reason_coalescing() {
    assert!(FrameRequestReason::CameraMotion.is_coalescable());
    assert!(FrameRequestReason::CursorBlink.is_coalescable());
    assert!(FrameRequestReason::Animation.is_coalescable());

    // Discrete user input and geometry changes must never be discarded:
    assert!(!FrameRequestReason::UserInput.is_coalescable());
    assert!(!FrameRequestReason::ContentUpdate.is_coalescable());
    assert!(!FrameRequestReason::WindowResize.is_coalescable());
    assert!(!FrameRequestReason::DisplayMigration.is_coalescable());
}

#[test]
fn scheduler_default_state_and_counters() {
    let scheduler = FrameScheduler::new();
    assert_eq!(scheduler.visibility(), WindowVisibility::VisibleActive);
    assert_eq!(scheduler.latest_camera().generation, 1);
    let c = scheduler.counters();
    assert_eq!(c.frames_requested, 0);
    assert_eq!(c.frames_started, 0);
    assert_eq!(c.frames_submitted, 0);
    assert_eq!(c.obsolete_frames_dropped, 0);
    assert_eq!(c.deferred_occluded, 0);
    assert_eq!(c.deferred_full_queue, 0);
    assert_eq!(c.drawables_unavailable, 0);
    assert_eq!(c.display_migrations, 0);
    assert_eq!(c.completions_drained, 0);
}

#[test]
fn negative_control_oracle_detects_obsolete_camera_frame() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-sched-obsolete",
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
    let pacing =
        DisplayPacingEngine::bind(token, &binding, owner, PacingConfig::default()).expect("pacing");
    let scheduler = FrameScheduler::new();

    // Camera at Gen 1: request frame
    scheduler.update_camera(
        CameraSnapshot {
            generation: 1,
            offset_x: 0.0,
            offset_y: 0.0,
            zoom: 1.0,
        },
        &pacing,
    );
    let outcome1 = scheduler
        .tick(token, &pacing, &binding, 0.0, 1.0 / 60.0)
        .expect("tick 1");
    let SchedulerOutcome::FrameReady(frame1) = outcome1 else {
        panic!("expected FrameReady")
    };
    assert_eq!(frame1.camera_generation(), 1);

    // Rapid navigation updates camera to Gen 2 before frame 1 is submitted:
    scheduler.update_camera(
        CameraSnapshot {
            generation: 2,
            offset_x: 50.0,
            offset_y: 100.0,
            zoom: 1.5,
        },
        &pacing,
    );

    // Oracle negative control: frame1 is detected as obsolete
    assert!(frame1.is_obsolete(scheduler.latest_camera().generation));

    // Drop obsolete frame without submitting to GPU:
    frame1.drop_obsolete(&scheduler);
    assert_eq!(scheduler.counters().obsolete_frames_dropped, 1);
    assert!(
        scheduler
            .event_records()
            .iter()
            .any(|r| r.event_type == SchedulerEventType::ObsoleteFrameDropped)
    );

    // Next tick renders the latest camera generation 2:
    let outcome2 = scheduler
        .tick(token, &pacing, &binding, 1.0 / 60.0, 2.0 / 60.0)
        .expect("tick 2");
    let SchedulerOutcome::FrameReady(frame2) = outcome2 else {
        panic!("expected FrameReady")
    };
    assert_eq!(frame2.camera_generation(), 2);
    assert!(!frame2.is_obsolete(scheduler.latest_camera().generation));
    frame2.present(&pacing).expect("present frame2");
}

#[test]
fn stationary_window_avoids_continuous_idle_redraw() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-sched-idle",
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
    let pacing =
        DisplayPacingEngine::bind(token, &binding, owner, PacingConfig::default()).expect("pacing");
    let scheduler = FrameScheduler::new();

    // With no pending requests, tick returns StationaryIdle without acquiring drawables
    for i in 0..5 {
        let t = i as f64 * (1.0 / 60.0);
        let outcome = scheduler
            .tick(token, &pacing, &binding, t, t + 1.0 / 60.0)
            .expect("idle tick");
        match outcome {
            SchedulerOutcome::StationaryIdle => {}
            other => panic!("expected StationaryIdle, got {other:?}"),
        }
    }
    assert_eq!(scheduler.counters().frames_started, 0);

    // Request frame (e.g. cursor blink):
    scheduler.request_frame(FrameRequestReason::CursorBlink, &pacing);
    let outcome = scheduler
        .tick(token, &pacing, &binding, 0.1, 0.116)
        .expect("cursor tick");
    let SchedulerOutcome::FrameReady(f) = outcome else {
        panic!("expected FrameReady")
    };
    f.present(&pacing).expect("present cursor frame");
    assert_eq!(scheduler.counters().frames_started, 1);

    // Immediately back to StationaryIdle:
    let outcome = scheduler
        .tick(token, &pacing, &binding, 0.116, 0.133)
        .expect("post-cursor tick");
    match outcome {
        SchedulerOutcome::StationaryIdle => {}
        other => panic!("expected StationaryIdle, got {other:?}"),
    }
}

#[test]
fn occluded_window_pauses_renders_while_completions_drain() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-sched-occl",
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
    let pacing =
        DisplayPacingEngine::bind(token, &binding, owner, PacingConfig::default()).expect("pacing");
    let mut scheduler = FrameScheduler::new();

    // Mark window occluded:
    scheduler.set_visibility(WindowVisibility::Occluded, &pacing);
    assert_eq!(scheduler.visibility(), WindowVisibility::Occluded);
    assert!(pacing.is_paused());

    // Request frame while occluded:
    scheduler.request_frame(FrameRequestReason::ContentUpdate, &pacing);

    // Tick returns PausedOccluded: no drawable acquired
    let outcome = scheduler
        .tick(token, &pacing, &binding, 0.0, 1.0 / 60.0)
        .expect("occluded tick");
    match outcome {
        SchedulerOutcome::PausedOccluded => {}
        other => panic!("expected PausedOccluded, got {other:?}"),
    }
    assert_eq!(scheduler.counters().deferred_occluded, 2);

    // Completions continue to drain live (§15.4):
    scheduler.record_completion_drained();
    assert_eq!(scheduler.counters().completions_drained, 1);

    // Return to visible:
    scheduler.set_visibility(WindowVisibility::VisibleActive, &pacing);
    assert!(!pacing.is_paused());

    // Now render proceeds:
    let outcome = scheduler
        .tick(token, &pacing, &binding, 0.016, 0.033)
        .expect("visible tick");
    let SchedulerOutcome::FrameReady(f) = outcome else {
        panic!("expected FrameReady after un-occlusion")
    };
    f.present(&pacing).expect("present");
}

#[test]
fn display_migration_and_resizing() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-sched-migr",
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
    let pacing =
        DisplayPacingEngine::bind(token, &binding, owner, PacingConfig::default()).expect("pacing");
    let scheduler = FrameScheduler::new();

    scheduler.handle_display_migration(&pacing);
    assert_eq!(scheduler.counters().display_migrations, 1);

    let outcome = scheduler
        .tick(token, &pacing, &binding, 0.0, 1.0 / 60.0)
        .expect("migration tick");
    let SchedulerOutcome::FrameReady(f) = outcome else {
        panic!("expected FrameReady for migration")
    };
    assert_eq!(f.reason(), FrameRequestReason::DisplayMigration);
    f.present(&pacing).expect("present");
}

#[test]
fn two_frame_policy_saturation_via_scheduler() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-sched-sat",
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
    let pacing =
        DisplayPacingEngine::bind(token, &binding, owner, PacingConfig::default()).expect("pacing");
    let scheduler = FrameScheduler::new();

    // Frame 1
    scheduler.request_frame(FrameRequestReason::CameraMotion, &pacing);
    let outcome1 = scheduler
        .tick(token, &pacing, &binding, 0.0, 1.0 / 60.0)
        .expect("tick 1");
    let SchedulerOutcome::FrameReady(f1) = outcome1 else {
        panic!("expected FrameReady")
    };

    // Frame 2
    scheduler.request_frame(FrameRequestReason::CameraMotion, &pacing);
    let outcome2 = scheduler
        .tick(token, &pacing, &binding, 1.0 / 60.0, 2.0 / 60.0)
        .expect("tick 2");
    let SchedulerOutcome::FrameReady(f2) = outcome2 else {
        panic!("expected FrameReady")
    };

    // Frame 3: Saturation!
    scheduler.request_frame(FrameRequestReason::CameraMotion, &pacing);
    let outcome3 = scheduler
        .tick(token, &pacing, &binding, 2.0 / 60.0, 3.0 / 60.0)
        .expect("tick 3");
    match outcome3 {
        SchedulerOutcome::DeferredQueueFull => {}
        other => panic!("expected DeferredQueueFull, got {other:?}"),
    }
    assert_eq!(scheduler.counters().deferred_full_queue, 1);

    // Present f1 to open up queue:
    f1.present(&pacing).expect("present f1");
    f2.present(&pacing).expect("present f2");
}

#[test]
fn event_ring_bounded_at_capacity() {
    let Some(token) = main_token() else {
        return;
    };
    let win = NativeWindow::create(
        token,
        "fcb-sched-ring",
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
    let pacing =
        DisplayPacingEngine::bind(token, &binding, owner, PacingConfig::default()).expect("pacing");
    let scheduler = FrameScheduler::new();

    for _ in 0..100 {
        scheduler.request_frame(FrameRequestReason::CameraMotion, &pacing);
    }

    assert_eq!(scheduler.counters().frames_requested, 100);
    assert_eq!(
        scheduler.event_records().len(),
        SCHEDULER_EVENT_RING_CAPACITY
    );
}

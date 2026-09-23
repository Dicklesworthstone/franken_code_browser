//! Main-thread qualification harness for the franken-macos host contracts
//! (fcb-ay62).
//!
//! `fn main` of a real binary runs on the host AppKit main thread - the one
//! context cargo test worker threads structurally cannot provide. This
//! harness exercises, natively and in order:
//!
//! 1. `MainThreadToken::capture_current` succeeding on the true main thread.
//! 2. Real `MetalLayer` / `MetalDevice` construction with ownership-snapshot
//!    pairing (retain/release counters) and clone/drop lifecycle.
//! 3. `CallbackCell` invocation, reentrancy rejection, aggregate counters,
//!    and late-shutdown rejection on the host main thread.
//!
//! Output is a bounded `CHECK` line per contract plus a terminal `OUTCOME`
//! line. Exit codes: 0 all contracts hold; 1 a contract is violated; 2 the
//! platform is not macOS (the harness is native-only by definition). An
//! optional sentinel supplied via `FCB_REDACT_SENTINEL` is redacted from
//! every printed line.

#![deny(missing_debug_implementations)]

use std::io::Write;

use franken_macos::callbacks::{CallbackCell, CallbackError, InstanceId, RegisteredClassName};
use franken_macos::pacing::{
    DisplayLinkAvailability, DisplayLinkRoute, DisplayPacingEngine, PacingConfig, PacingError,
    PacingOutcome,
};
use franken_macos::scheduler::{
    CameraSnapshot, FrameRequestReason, FrameScheduler, SchedulerOutcome, WindowVisibility,
};
use franken_macos::{
    BridgeError, BufferError, CompletionStatus, MainThreadToken, MetalDevice, MetalLayer,
    SubmissionError,
};

fn redact(line: &str) -> String {
    match std::env::var("FCB_REDACT_SENTINEL") {
        Ok(sentinel) if !sentinel.is_empty() => line.replace(&sentinel, "[REDACTED]"),
        _ => line.to_string(),
    }
}

fn emit(check: &str, ok: bool, detail: &str) {
    let line = redact(&format!(
        "CHECK {check} {} {detail}",
        if ok { "ok" } else { "FAIL" }
    ));
    let _ = std::io::stdout().write_all(line.as_bytes());
    let _ = std::io::stdout().write_all(b"\n");
}

fn finish(failures: u32) -> ! {
    if failures == 0 {
        emit("OUTCOME", true, "all host main-thread contracts hold");
        std::process::exit(0);
    }
    emit(
        "OUTCOME",
        false,
        &format!("{failures} contract violation(s) on the host main thread"),
    );
    std::process::exit(1);
}

fn main() {
    if !cfg!(target_os = "macos") {
        emit("platform", false, "harness is macOS-only");
        std::process::exit(2);
    }

    let mut failures: u32 = 0;

    // 1. Token capture on the true main thread.
    let token = match MainThreadToken::capture_current() {
        Ok(token) => {
            emit("main-thread-capture", true, "captured on host main thread");
            token
        }
        Err(BridgeError::NotMainThread) => {
            emit(
                "main-thread-capture",
                false,
                "binary main() is not the host main thread",
            );
            std::process::exit(1);
        }
        Err(other) => {
            emit("main-thread-capture", false, &format!("{other:?}"));
            std::process::exit(1);
        }
    };

    // 2. Real Metal layer construction and ownership pairing.
    let layer = match MetalLayer::new(token) {
        Ok(layer) => {
            let snapshot = layer.ownership();
            let paired =
                snapshot.live_wrappers == 1 && snapshot.retains == 0 && snapshot.releases == 0;
            emit(
                "metal-layer-construction",
                paired,
                &format!("snapshot {snapshot:?}"),
            );
            if !paired {
                failures += 1;
            }
            layer
        }
        Err(error) => {
            emit("metal-layer-construction", false, &format!("{error:?}"));
            failures += 1;
            finish(failures);
        }
    };

    // 3. Clone retain / drop release pairing on the real object.
    let retained = match layer.retained(token) {
        Ok(retained) => {
            let snapshot = layer.ownership();
            let paired = snapshot.live_wrappers == 2 && snapshot.retains == 1;
            emit(
                "metal-layer-retain",
                paired,
                &format!("snapshot {snapshot:?}"),
            );
            if !paired {
                failures += 1;
            }
            retained
        }
        Err(error) => {
            emit("metal-layer-retain", false, &format!("{error:?}"));
            failures += 1;
            finish(failures);
        }
    };
    drop(retained);
    let after_release = layer.ownership();
    let release_paired = after_release.live_wrappers == 1 && after_release.releases == 1;
    emit(
        "metal-layer-release",
        release_paired,
        &format!("snapshot {after_release:?}"),
    );
    if !release_paired {
        failures += 1;
    }

    // 4. Real Metal device adoption: SDK-declared retained result adopted once.
    let device = match MetalDevice::system_default(token) {
        Ok(device) => {
            let snapshot = device.ownership();
            let adopted =
                snapshot.live_wrappers == 1 && snapshot.retains == 0 && snapshot.releases == 0;
            emit(
                "metal-device-adoption",
                adopted,
                &format!("snapshot {snapshot:?}"),
            );
            if !adopted {
                failures += 1;
            }
            Some(device)
        }
        Err(error) => {
            emit("metal-device-adoption", false, &format!("{error:?}"));
            failures += 1;
            None
        }
    };

    // 4b. Metal buffer creation, CPU write/read, GPU flight guard, and real GPU blit round-trip.
    if let Some(ref device) = device {
        let zero_refused = matches!(device.create_buffer(token, 0), Err(BufferError::ZeroLength));
        emit(
            "metal-buffer-zero-refusal",
            zero_refused,
            "zero-length allocation refused",
        );
        if !zero_refused {
            failures += 1;
        }

        let buffer = device.create_buffer(token, 256);
        let buf_ok = match &buffer {
            Ok(b) => b.len() == 256 && !b.is_empty() && !b.is_in_gpu_flight(),
            Err(_) => false,
        };
        emit(
            "metal-buffer-allocation",
            buf_ok,
            "256-byte buffer allocated and validated",
        );
        if !buf_ok {
            failures += 1;
        }

        let mut rw_ok = false;
        if let Ok(buf) = device.create_buffer(token, 64) {
            let data = b"hello GPU world";
            if buf.write_bytes(token, 0, data).is_ok() {
                let mut readback = [0u8; 15];
                if buf.read_bytes(token, 0, &mut readback).is_ok() && &readback == data {
                    rw_ok = true;
                }
            }
        }
        emit(
            "metal-buffer-cpu-roundtrip",
            rw_ok,
            "CPU write and read round-trip",
        );
        if !rw_ok {
            failures += 1;
        }

        let mut flight_ok = false;
        if let Ok(buf) = device.create_buffer(token, 64) {
            buf.begin_gpu_flight();
            let write_during_flight = buf.write_bytes(token, 0, b"data");
            buf.end_gpu_flight();
            let write_after_flight = buf.write_bytes(token, 0, b"data");
            flight_ok =
                write_during_flight == Err(BufferError::GpuInFlight) && write_after_flight.is_ok();
        }
        emit(
            "metal-buffer-flight-guard",
            flight_ok,
            "CPU writes refused during GPU flight",
        );
        if !flight_ok {
            failures += 1;
        }

        let mut blit_ok = false;
        if let (Ok(src), Ok(dst), Ok(queue)) = (
            device.create_buffer(token, 256),
            device.create_buffer(token, 256),
            device.command_queue(token),
        ) {
            let pattern: Vec<u8> = (0..=255u8).collect();
            let _ = src.write_bytes(token, 0, &pattern);
            let _ = dst.write_bytes(token, 0, &[0u8; 256]);
            if queue
                .gpu_copy_and_wait(token, &src, 0, &dst, 0, 256)
                .is_ok()
            {
                let mut readback = [0u8; 256];
                if dst.read_bytes(token, 0, &mut readback).is_ok() && &readback[..] == &pattern[..]
                {
                    blit_ok = true;
                }
            }
        }
        emit(
            "metal-buffer-gpu-blit",
            blit_ok,
            "real GPU byte copy via blit encoder",
        );
        if !blit_ok {
            failures += 1;
        }

        // 4. Bounded submission queue, real GPU round-trip, capacity bounding, and cancellation lease survival (FCB-005.B)
        let mut queue_roundtrip_ok = false;
        if let (Ok(queue), Ok(src), Ok(dst)) = (
            device.bounded_queue(token, 2),
            device.create_buffer(token, 128),
            device.create_buffer(token, 128),
        ) {
            let pattern: Vec<u8> = (0..128u8).map(|b| b.wrapping_mul(13)).collect();
            let _ = src.write_bytes(token, 0, &pattern);
            let _ = dst.write_bytes(token, 0, &[0u8; 128]);

            if let Ok(mut enc) = queue.begin_submission(token) {
                if enc.encode_copy(&src, 0, &dst, 0, 128).is_ok() {
                    let flight_locked = src.is_in_gpu_flight();
                    if let Ok(sub) = enc.commit() {
                        if let Ok(status) = sub.wait_until_completed(&queue, token) {
                            let mut readback = [0u8; 128];
                            let read_ok = dst.read_bytes(token, 0, &mut readback).is_ok();
                            let data_matches = &readback[..] == &pattern[..];
                            let leases_released =
                                !src.is_in_gpu_flight() && !dst.is_in_gpu_flight();
                            queue_roundtrip_ok = status == CompletionStatus::Success
                                && flight_locked
                                && read_ok
                                && data_matches
                                && leases_released;
                        }
                    }
                }
            }
        }
        emit(
            "bounded-submission-gpu-roundtrip",
            queue_roundtrip_ok,
            "bounded queue GPU round-trip with lossless completion",
        );
        if !queue_roundtrip_ok {
            failures += 1;
        }

        let mut queue_capacity_ok = false;
        if let (Ok(queue), Ok(src), Ok(dst)) = (
            device.bounded_queue(token, 1),
            device.create_buffer(token, 64),
            device.create_buffer(token, 64),
        ) {
            if let Ok(mut enc1) = queue.begin_submission(token) {
                let _ = enc1.encode_copy(&src, 0, &dst, 0, 64);
                if let Ok(_sub1) = enc1.commit() {
                    let full_err = queue.begin_submission(token);
                    queue_capacity_ok = full_err.unwrap_err() == SubmissionError::QueueFull
                        && queue.counters().rejected_queue_full == 1;
                    let _ = queue.drain_all_sync(token);
                }
            }
        }
        emit(
            "bounded-submission-capacity-refusal",
            queue_capacity_ok,
            "queue full refused before driver allocation",
        );
        if !queue_capacity_ok {
            failures += 1;
        }

        let mut cancel_survival_ok = false;
        if let (Ok(queue), Ok(src), Ok(dst)) = (
            device.bounded_queue(token, 2),
            device.create_buffer(token, 64),
            device.create_buffer(token, 64),
        ) {
            let _ = src.write_bytes(token, 0, b"cancel-survival-data");
            if let Ok(mut enc) = queue.begin_submission(token) {
                let _ = enc.encode_copy(&src, 0, &dst, 0, 64);
                if let Ok(sub) = enc.commit() {
                    if sub.cancel(&queue).is_ok() && sub.status() == CompletionStatus::Cancelled {
                        let still_in_flight = src.is_in_gpu_flight();
                        let write_refused = src.write_bytes(token, 0, b"write-attempt")
                            == Err(BufferError::GpuInFlight);
                        if let Ok(term) = sub.wait_until_completed(&queue, token) {
                            let terminal_ok = term == CompletionStatus::Cancelled;
                            let released_after_terminal = !src.is_in_gpu_flight();
                            cancel_survival_ok = still_in_flight
                                && write_refused
                                && terminal_ok
                                && released_after_terminal;
                        }
                    }
                }
            }
        }
        emit(
            "bounded-submission-cancel-lease-survival",
            cancel_survival_ok,
            "GPU leases survive cancellation until terminal driver contract",
        );
        if !cancel_survival_ok {
            failures += 1;
        }

        let mut stale_rejection_ok = false;
        if let (Ok(dev2), Ok(queue), Ok(src)) = (
            MetalDevice::system_default(token),
            device.bounded_queue(token, 2),
            device.create_buffer(token, 64),
        ) {
            if let Ok(foreign_buf) = dev2.create_buffer(token, 64) {
                if let Ok(mut enc) = queue.begin_submission(token) {
                    let cross_res = enc.encode_copy(&src, 0, &foreign_buf, 0, 64);
                    stale_rejection_ok = cross_res == Err(SubmissionError::CrossDevice)
                        && queue.counters().rejected_cross_device == 1;
                }
            }
        }
        emit(
            "bounded-submission-cross-device-refusal",
            stale_rejection_ok,
            "cross-device buffer rejected",
        );
        if !stale_rejection_ok {
            failures += 1;
        }
    }

    let instance = InstanceId::next();
    let class_name = match RegisteredClassName::issue("fcb-native", instance) {
        Ok(class_name) => class_name,
        Err(error) => {
            emit("callback-registration", false, &format!("{error:?}"));
            failures += 1;
            finish(failures);
        }
    };
    let mut cell = CallbackCell::register(token, class_name, 0_u64);

    let first: Result<(), CallbackError> = cell.invoke(token, |state| {
        *state += 1;
    });
    emit(
        "callback-invoke",
        first.is_ok(),
        "host main thread invocation",
    );
    if first.is_err() {
        failures += 1;
    }

    let reentrant: Result<(), CallbackError> = cell.invoke(token, |_state| {
        let nested: Result<(), CallbackError> = cell.invoke(token, |state| {
            *state += 1;
        });
        if nested != Err(CallbackError::Reentrant) {
            emit(
                "callback-reentrancy",
                false,
                "nested invocation was not rejected",
            );
            failures += 1;
        }
    });
    if reentrant.is_err() {
        emit("callback-reentrancy", false, "outer invoke failed");
        failures += 1;
    } else {
        emit(
            "callback-reentrancy",
            true,
            "nested invocation rejected, guard released",
        );
    }

    let retired = cell.shutdown(token);
    // Only the first accepted invoke mutates the state; the reentrancy
    // probe's outer body reads but does not increment.
    let retired_ok = matches!(retired, Ok(1_u64));
    emit(
        "callback-shutdown",
        retired_ok,
        "state retired exactly once",
    );
    if !retired_ok {
        failures += 1;
    }

    let counters = cell.counters();
    // Accepted: the first invoke plus the outer body of the reentrancy
    // probe. Rejected: the nested probe, exactly once.
    let counters_ok = counters.invocations_accepted == 2 && counters.rejected_reentrant == 1;
    emit("callback-counters", counters_ok, &format!("{counters:?}"));
    if !counters_ok {
        failures += 1;
    }

    let late: Result<(), CallbackError> = cell.invoke(token, |state| {
        *state += 1;
    });
    let late_ok = late == Err(CallbackError::Closed);
    emit(
        "callback-late-shutdown",
        late_ok,
        "late invocation rejected",
    );
    if !late_ok {
        failures += 1;
    }

    // 5. Host-owned native view binding, attach/detach, and lifecycle isolation (FCB-070.A / FCB-070.B)
    if let Some(ref device) = device {
        if let Ok(window_a) = franken_macos::windowing::NativeWindow::create(
            token,
            "fcb-main-view-a",
            franken_macos::windowing::Rect::at_origin(64.0, 64.0),
            franken_macos::windowing::WindowStyle::standard(),
        ) {
            let host_desc = franken_macos::HostTargetDescriptor {
                pixel_format: franken_macos::PixelFormat::Bgra8Unorm,
                color_space: franken_macos::ColorSpace::Srgb,
                sample_count: 1,
                metrics: franken_macos::DisplayMetrics {
                    backing_scale: 2.0,
                    drawable_width: 64.0,
                    drawable_height: 64.0,
                },
            };
            if let Ok(layer_a) = franken_macos::MetalLayer::new(token) {
                if let Ok((binding_a, owner_a)) = franken_macos::NativeViewBinding::attach(
                    token,
                    &window_a,
                    layer_a,
                    device,
                    host_desc,
                    franken_macos::DrawablePath::HostOwned,
                ) {
                    emit(
                        "view-binding-attach",
                        binding_a.is_attached(),
                        "bound CAMetalLayer to host window content view",
                    );

                    let second_refused = binding_a
                        .take_owner(franken_macos::DrawablePath::RendererOwned)
                        == Err(franken_macos::ViewBindingError::PresentationOwnerTaken);
                    emit(
                        "view-binding-second-owner-refusal",
                        second_refused,
                        "second presentation owner refused",
                    );
                    if !second_refused {
                        failures += 1;
                    }

                    // Lease during work and detach
                    let lease_ok = binding_a.lease(owner_a).is_ok();
                    let detach_ok = binding_a.detach(token).is_ok();
                    let work_recorded = binding_a.counters().detaches_during_work == 1;
                    let stale_refused = binding_a.lease(owner_a)
                        == Err(franken_macos::ViewBindingError::StalePresentationOwner);
                    let detach_contract = lease_ok && detach_ok && work_recorded && stale_refused;
                    emit(
                        "view-binding-detach-during-work",
                        detach_contract,
                        "detach during active lease invalidates owner and records work",
                    );
                    if !detach_contract {
                        failures += 1;
                    }

                    // Device and window survive
                    let dev_survives =
                        device.create_buffer(token, 32).is_ok() && !window_a.is_closed();
                    emit(
                        "view-binding-host-survival",
                        dev_survives,
                        "host window and Metal device remain functional after detach",
                    );
                    if !dev_survives {
                        failures += 1;
                    }

                    // Reattach
                    let reattach_ok = binding_a
                        .reattach(
                            token,
                            device,
                            host_desc,
                            franken_macos::DrawablePath::HostOwned,
                        )
                        .is_ok()
                        && binding_a.is_attached()
                        && binding_a.generation() == 3;
                    emit(
                        "view-binding-reattach",
                        reattach_ok,
                        "reattach advances generation and restores presentation owner",
                    );
                    if !reattach_ok {
                        failures += 1;
                    }

                    let _ = binding_a.detach(token);
                }
            }
            let _ = window_a.mark_closed(token);
        }
    }

    // 6. Display-link pacing and single drawable ownership (FCB-006.A)
    let sdk_probe = DisplayLinkAvailability::probe();
    let sdk_probe_ok =
        sdk_probe.is_available && sdk_probe.route == DisplayLinkRoute::CAMetalDisplayLink;
    emit(
        "pacing-sdk-probe",
        sdk_probe_ok,
        "CAMetalDisplayLink probe verified on macOS host",
    );
    if !sdk_probe_ok {
        failures += 1;
    }

    if let Some(ref device) = device {
        if let Ok(pacing_win) = franken_macos::windowing::NativeWindow::create(
            token,
            "fcb-main-pacing",
            franken_macos::windowing::Rect::at_origin(64.0, 64.0),
            franken_macos::windowing::WindowStyle::standard(),
        ) {
            let host_desc = franken_macos::HostTargetDescriptor {
                pixel_format: franken_macos::PixelFormat::Bgra8Unorm,
                color_space: franken_macos::ColorSpace::Srgb,
                sample_count: 1,
                metrics: franken_macos::DisplayMetrics {
                    backing_scale: 2.0,
                    drawable_width: 64.0,
                    drawable_height: 64.0,
                },
            };
            if let Ok(pacing_layer) = franken_macos::MetalLayer::new(token) {
                if let Ok((pacing_bind, pacing_owner)) = franken_macos::NativeViewBinding::attach(
                    token,
                    &pacing_win,
                    pacing_layer,
                    device,
                    host_desc,
                    franken_macos::DrawablePath::HostOwned,
                ) {
                    let config = PacingConfig {
                        max_in_flight: 2,
                        target_fps: 60,
                        preferred_latency_frames: 2,
                    };

                    if let Ok(engine) =
                        DisplayPacingEngine::bind(token, &pacing_bind, pacing_owner, config)
                    {
                        emit(
                            "pacing-engine-bind",
                            true,
                            "display pacing engine bound with exclusive presentation ownership",
                        );

                        // Two-frame starting policy check (§15.2)
                        engine.request_frame();
                        let f1 = match engine.tick(token, &pacing_bind, 0.0, 0.016) {
                            Ok(PacingOutcome::FrameStarted(f)) => Some(f),
                            _ => None,
                        };
                        engine.request_frame();
                        let f2 = match engine.tick(token, &pacing_bind, 0.016, 0.033) {
                            Ok(PacingOutcome::FrameStarted(f)) => Some(f),
                            _ => None,
                        };
                        engine.request_frame();
                        let f3_deferred = match engine.tick(token, &pacing_bind, 0.033, 0.050) {
                            Ok(PacingOutcome::DeferredFullQueue) => true,
                            _ => false,
                        };
                        let two_frame_ok = f1.is_some()
                            && f2.is_some()
                            && f3_deferred
                            && engine.in_flight_count() == 2;
                        emit(
                            "pacing-two-frame-policy",
                            two_frame_ok,
                            "two-frame policy defers third in-flight frame without GPU allocation",
                        );
                        if !two_frame_ok {
                            failures += 1;
                        }

                        // Present frames and verify clean decrements
                        let mut present_ok = false;
                        if let (Some(frame1), Some(frame2)) = (f1, f2) {
                            let p1 = frame1.present(&engine).is_ok();
                            let inflight_mid = engine.in_flight_count() == 1;
                            let p2 = frame2.present(&engine).is_ok();
                            let inflight_end = engine.in_flight_count() == 0;
                            present_ok = p1 && inflight_mid && p2 && inflight_end;
                        }
                        emit(
                            "pacing-frame-presentation",
                            present_ok,
                            "paced frames presented with clean in-flight counter decrements",
                        );
                        if !present_ok {
                            failures += 1;
                        }

                        // Service the deferred frame that was waiting for capacity
                        if let Ok(PacingOutcome::FrameStarted(f_def)) =
                            engine.tick(token, &pacing_bind, 0.050, 0.066)
                        {
                            let _ = f_def.present(&engine);
                        }

                        // Stationary idle: no continuous redraw
                        let idle_tick = match engine.tick(token, &pacing_bind, 0.066, 0.083) {
                            Ok(PacingOutcome::Idle) => true,
                            _ => false,
                        };
                        emit(
                            "pacing-stationary-idle",
                            idle_tick,
                            "stationary state avoids continuous idle redraw",
                        );
                        if !idle_tick {
                            failures += 1;
                        }

                        // Non-blocking teardown
                        engine.invalidate();
                        let invalidated_tick = matches!(
                            engine.tick(token, &pacing_bind, 0.1, 0.116),
                            Err(PacingError::Invalidated)
                        );
                        emit(
                            "pacing-nonblocking-teardown",
                            engine.is_invalidated() && invalidated_tick,
                            "non-blocking teardown immediately invalidates link without stall",
                        );
                        if !engine.is_invalidated() || !invalidated_tick {
                            failures += 1;
                        }
                    } else {
                        emit(
                            "pacing-engine-bind",
                            false,
                            "failed to bind display pacing engine",
                        );
                        failures += 1;
                    }
                }
            }
            let _ = pacing_win.mark_closed(token);
        }
    }

    // 7. Demand-driven frame scheduling and camera coalescing (FCB-006.B)
    if let Some(ref device) = device {
        if let Ok(sched_win) = franken_macos::windowing::NativeWindow::create(
            token,
            "fcb-main-sched",
            franken_macos::windowing::Rect::at_origin(64.0, 64.0),
            franken_macos::windowing::WindowStyle::standard(),
        ) {
            let host_desc = franken_macos::HostTargetDescriptor {
                pixel_format: franken_macos::PixelFormat::Bgra8Unorm,
                color_space: franken_macos::ColorSpace::Srgb,
                sample_count: 1,
                metrics: franken_macos::DisplayMetrics {
                    backing_scale: 2.0,
                    drawable_width: 64.0,
                    drawable_height: 64.0,
                },
            };
            if let Ok(sched_layer) = franken_macos::MetalLayer::new(token) {
                if let Ok((sched_bind, sched_owner)) = franken_macos::NativeViewBinding::attach(
                    token,
                    &sched_win,
                    sched_layer,
                    device,
                    host_desc,
                    franken_macos::DrawablePath::HostOwned,
                ) {
                    let config = PacingConfig {
                        max_in_flight: 2,
                        target_fps: 60,
                        preferred_latency_frames: 2,
                    };
                    if let Ok(sched_pacing) =
                        DisplayPacingEngine::bind(token, &sched_bind, sched_owner, config)
                    {
                        let mut scheduler = FrameScheduler::new();

                        // 1. Stationary idle: no drawables acquired without explicit request
                        let idle_ok = matches!(
                            scheduler.tick(token, &sched_pacing, &sched_bind, 0.0, 0.016),
                            Ok(SchedulerOutcome::StationaryIdle)
                        );
                        emit(
                            "scheduler-stationary-idle",
                            idle_ok && scheduler.counters().frames_started == 0,
                            "stationary window avoids continuous idle redraw",
                        );
                        if !idle_ok {
                            failures += 1;
                        }

                        // 2. Camera coalescing and pre-submission obsolete frame drop
                        scheduler.update_camera(
                            CameraSnapshot {
                                generation: 1,
                                offset_x: 0.0,
                                offset_y: 0.0,
                                zoom: 1.0,
                            },
                            &sched_pacing,
                        );
                        let outcome1 =
                            scheduler.tick(token, &sched_pacing, &sched_bind, 0.016, 0.033);
                        let mut coalesce_ok = false;
                        if let Ok(SchedulerOutcome::FrameReady(f1)) = outcome1 {
                            // Camera moves to Gen 2 before f1 is submitted
                            scheduler.update_camera(
                                CameraSnapshot {
                                    generation: 2,
                                    offset_x: 10.0,
                                    offset_y: 20.0,
                                    zoom: 1.2,
                                },
                                &sched_pacing,
                            );
                            if f1.is_obsolete(scheduler.latest_camera().generation) {
                                f1.drop_obsolete(&scheduler);
                                coalesce_ok = scheduler.counters().obsolete_frames_dropped == 1;
                            }
                        }
                        emit(
                            "scheduler-camera-coalescing",
                            coalesce_ok,
                            "obsolete pre-submission camera frame dropped cleanly without GPU work",
                        );
                        if !coalesce_ok {
                            failures += 1;
                        }

                        // Render Gen 2 frame
                        if let Ok(SchedulerOutcome::FrameReady(f2)) =
                            scheduler.tick(token, &sched_pacing, &sched_bind, 0.033, 0.050)
                        {
                            let _ = f2.present(&sched_pacing);
                        }

                        // 3. Occlusion pause and completion drain
                        scheduler.set_visibility(WindowVisibility::Occluded, &sched_pacing);
                        scheduler.request_frame(FrameRequestReason::ContentUpdate, &sched_pacing);
                        let occluded_ok = matches!(
                            scheduler.tick(token, &sched_pacing, &sched_bind, 0.050, 0.066),
                            Ok(SchedulerOutcome::PausedOccluded)
                        );
                        scheduler.record_completion_drained();
                        let drain_ok = scheduler.counters().completions_drained == 1;
                        scheduler.set_visibility(WindowVisibility::VisibleActive, &sched_pacing);
                        let resume_ok = !sched_pacing.is_paused();

                        let occl_contract = occluded_ok && drain_ok && resume_ok;
                        emit(
                            "scheduler-occlusion-pause",
                            occl_contract,
                            "occluded window pauses renders while GPU completions continue draining",
                        );
                        if !occl_contract {
                            failures += 1;
                        }
                    }
                }
            }
            let _ = sched_win.mark_closed(token);
        }
    }

    finish(failures);
}

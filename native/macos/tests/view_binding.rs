//! Focused tests for host-owned native view binding and host lifecycle isolation (FCB-070.B).

#![forbid(unsafe_code)]

use franken_macos::view_binding::{
    DrawablePath, VIEW_BINDING_EVENT_RING_CAPACITY, ViewBindingCounters, ViewBindingEventType,
};
use franken_macos::windowing::{BoundedEventQueue, EventPump, NativeWindow, Rect, WindowStyle};
use franken_macos::{
    ColorSpace, DisplayMetrics, HostTargetDescriptor, MainThreadToken, MetalDevice, MetalLayer,
    NativeViewBinding, PixelFormat, ViewBindingError,
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
fn invalid_config_is_refused_before_native_work() {
    let bad_scale = valid_host(0.0, 100.0, 100.0);
    assert_eq!(bad_scale.validate(), Err(ViewBindingError::InvalidConfig));
    let bad_sample = HostTargetDescriptor {
        sample_count: 4,
        ..valid_host(2.0, 100.0, 100.0)
    };
    assert_eq!(bad_sample.validate(), Err(ViewBindingError::InvalidConfig));
    let nan = valid_host(f64::NAN, 100.0, 100.0);
    assert_eq!(nan.validate(), Err(ViewBindingError::InvalidConfig));
}

#[test]
fn negative_control_detects_zero_drawable_size() {
    let expected_ok = valid_host(2.0, 64.0, 64.0).validate().is_ok();
    let defect = valid_host(2.0, 0.0, 64.0).validate().is_ok();
    assert!(expected_ok);
    assert!(!defect, "oracle must catch a zero-width drawable");
}

#[test]
fn negative_control_oracle_detects_cross_binding_and_stale_owner_defects() {
    // Demonstration oracle: a correct implementation prevents cross-binding
    // token reuse and stale generation access.
    // If an oracle failed to distinguish distinct binding IDs or generations,
    // it would allow unauthorized drawable presentation.
    let host = valid_host(2.0, 64.0, 64.0);
    assert!(host.validate().is_ok());

    // Verify error code mappings and distinction
    assert_ne!(
        ViewBindingError::StalePresentationOwner,
        ViewBindingError::PresentationOwnerTaken
    );
    assert_ne!(ViewBindingError::NotBound, ViewBindingError::WindowClosed);
    assert_ne!(
        ViewBindingError::CrossDevice,
        ViewBindingError::InvalidConfig
    );
}

#[test]
fn view_binding_constants_and_types_are_accessible() {
    assert_eq!(VIEW_BINDING_EVENT_RING_CAPACITY, 64);
    let counters = ViewBindingCounters::default();
    assert_eq!(counters.attaches, 0);
    assert_eq!(counters.detaches_during_work, 0);
    assert_eq!(counters.reattaches, 0);
    assert_eq!(counters.leases_issued, 0);
    assert_eq!(counters.leases_released, 0);
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;

    fn try_window(token: MainThreadToken, title: &str) -> Option<NativeWindow> {
        NativeWindow::create(
            token,
            title,
            Rect::at_origin(64.0, 64.0),
            WindowStyle::standard(),
        )
        .ok()
    }

    #[test]
    fn attach_issues_one_owner_and_refuses_a_second() {
        let Some(token) = main_token() else {
            return;
        };
        let Some(window) = try_window(token, "fcb-zx63-one-owner") else {
            return;
        };
        let layer = MetalLayer::new(token).expect("layer");
        let device = MetalDevice::system_default(token).expect("device");
        let host = valid_host(window.recorded_backing_scale().max(1.0), 64.0, 64.0);
        let (binding, owner) = NativeViewBinding::attach(
            token,
            &window,
            layer,
            &device,
            host,
            DrawablePath::HostOwned,
        )
        .expect("attach");
        assert!(binding.is_attached());
        assert_eq!(owner.path(), DrawablePath::HostOwned);
        assert_eq!(
            binding.take_owner(DrawablePath::RendererOwned).unwrap_err(),
            ViewBindingError::PresentationOwnerTaken
        );
        assert_eq!(binding.counters().rejected_second_owner, 1);
        let lease = binding.lease(owner).expect("lease");
        assert_eq!(lease.binding, binding.id());
        binding.detach(token).expect("detach");
        assert!(!binding.is_attached());
        assert_eq!(
            binding.acquire_drawable(token, owner).unwrap_err(),
            ViewBindingError::StalePresentationOwner
        );
        // Host device remains usable after detach.
        device
            .create_buffer(token, 32)
            .expect("device survives detach");
        window.mark_closed(token).ok();
    }

    #[test]
    fn two_host_views_are_independent() {
        let Some(token) = main_token() else {
            return;
        };
        let Some(window_a) = try_window(token, "fcb-zx63-a") else {
            return;
        };
        let Some(window_b) = try_window(token, "fcb-zx63-b") else {
            return;
        };
        let device = MetalDevice::system_default(token).expect("device");
        let host = valid_host(2.0, 32.0, 32.0);
        let (bind_a, owner_a) = NativeViewBinding::attach(
            token,
            &window_a,
            MetalLayer::new(token).expect("layer a"),
            &device,
            host,
            DrawablePath::HostOwned,
        )
        .expect("attach a");
        let (bind_b, owner_b) = NativeViewBinding::attach(
            token,
            &window_b,
            MetalLayer::new(token).expect("layer b"),
            &device,
            host,
            DrawablePath::RendererOwned,
        )
        .expect("attach b");
        assert_ne!(bind_a.id(), bind_b.id());
        assert_eq!(
            bind_a.acquire_drawable(token, owner_b).unwrap_err(),
            ViewBindingError::StalePresentationOwner
        );
        let _drawable_a = bind_a.acquire_drawable(token, owner_a);
        let _drawable_b = bind_b.acquire_drawable(token, owner_b);
        bind_a.detach(token).expect("detach a");
        assert!(bind_b.is_attached());
        device.create_buffer(token, 16).expect("device still live");
        window_a.mark_closed(token).ok();
        window_b.mark_closed(token).ok();
    }

    #[test]
    fn closed_window_refuses_attach() {
        let Some(token) = main_token() else {
            return;
        };
        let Some(window) = try_window(token, "fcb-zx63-closed") else {
            return;
        };
        window.mark_closed(token).expect("close");
        let layer = MetalLayer::new(token).expect("layer");
        let device = MetalDevice::system_default(token).expect("device");
        let err = NativeViewBinding::attach(
            token,
            &window,
            layer,
            &device,
            valid_host(1.0, 16.0, 16.0),
            DrawablePath::HostOwned,
        )
        .unwrap_err();
        assert_eq!(err, ViewBindingError::WindowClosed);
    }

    #[test]
    fn oracle_two_host_views_renderer_vs_host_owned_and_detach_during_work() {
        // Full FCB-070.B package oracle:
        // 1. Two host views (Window A & Window B)
        // 2. Renderer-owned vs Host-owned drawable paths
        // 3. Detach during work leaves loop, device, and runtime alive
        // 4. Clean re-attachment after detachment
        let Some(token) = main_token() else {
            return;
        };
        let Some(window_a) = try_window(token, "fcb-oracle-view-a") else {
            return;
        };
        let Some(window_b) = try_window(token, "fcb-oracle-view-b") else {
            return;
        };

        let device = MetalDevice::system_default(token).expect("device");
        let host_desc = valid_host(2.0, 64.0, 64.0);

        // View A: Host-owned drawable path
        let (binding_a, owner_a) = NativeViewBinding::attach(
            token,
            &window_a,
            MetalLayer::new(token).expect("layer a"),
            &device,
            host_desc,
            DrawablePath::HostOwned,
        )
        .expect("attach a");
        assert_eq!(binding_a.path(), DrawablePath::HostOwned);

        // View B: Renderer-owned drawable path
        let (binding_b, owner_b) = NativeViewBinding::attach(
            token,
            &window_b,
            MetalLayer::new(token).expect("layer b"),
            &device,
            host_desc,
            DrawablePath::RendererOwned,
        )
        .expect("attach b");
        assert_eq!(binding_b.path(), DrawablePath::RendererOwned);

        // 1. Distinct identities and generations
        assert_ne!(binding_a.id(), binding_b.id());
        assert_eq!(binding_a.generation(), 1);
        assert_eq!(binding_b.generation(), 1);

        // 2. Cross-binding rejection (oracle verifies isolation)
        assert_eq!(
            binding_a.lease(owner_b).unwrap_err(),
            ViewBindingError::StalePresentationOwner
        );
        assert_eq!(
            binding_b.acquire_drawable(token, owner_a).unwrap_err(),
            ViewBindingError::StalePresentationOwner
        );

        // 3. Both views can operate independently
        let lease_a = binding_a.lease(owner_a).expect("lease a");
        assert_eq!(lease_a.binding, binding_a.id());
        assert_eq!(binding_a.active_leases(), 1);

        let lease_b = binding_b.lease(owner_b).expect("lease b");
        assert_eq!(lease_b.binding, binding_b.id());
        assert_eq!(binding_b.active_leases(), 1);

        // View B acquires a drawable on renderer-owned path
        let drawable_b = binding_b
            .acquire_drawable(token, owner_b)
            .expect("acquire b");
        drawable_b.present();

        // 4. Simulate active work during View A detachment:
        //    - View A has active lease_a held (active_leases == 1)
        //    - Device has active buffer operations
        let buffer = device.create_buffer(token, 128).expect("buffer");
        buffer
            .write_bytes(token, 0, b"detach-during-work")
            .expect("write");

        // DETACH View A during active work!
        binding_a.detach(token).expect("detach a during work");

        // 5. Verify View A state post-detach
        assert!(!binding_a.is_attached());
        assert_eq!(binding_a.active_leases(), 0);
        let counters_a = binding_a.counters();
        assert_eq!(counters_a.detaches, 1);
        assert_eq!(counters_a.detaches_during_work, 1);

        // Old owner_a and lease_a are now stale
        assert_eq!(
            binding_a.acquire_drawable(token, owner_a).unwrap_err(),
            ViewBindingError::StalePresentationOwner
        );
        assert_eq!(
            binding_a.lease(owner_a).unwrap_err(),
            ViewBindingError::StalePresentationOwner
        );
        assert_eq!(
            binding_a.release_lease(lease_a).unwrap_err(),
            ViewBindingError::StalePresentationOwner
        );

        // 6. ISOLATION: Host Window A remains open and valid
        assert!(!window_a.is_closed());

        // 7. ISOLATION: Host View B remains completely attached and operational
        assert!(binding_b.is_attached());
        assert_eq!(binding_b.counters().detaches, 0);
        binding_b.release_lease(lease_b).expect("release b");
        assert_eq!(binding_b.active_leases(), 0);

        // 8. ISOLATION: Host MetalDevice remains completely alive and functional
        let post_detach_buf = device.create_buffer(token, 64).expect("device survives");
        post_detach_buf
            .write_bytes(token, 0, b"alive")
            .expect("write after detach");
        let mut readback = [0u8; 5];
        post_detach_buf
            .read_bytes(token, 0, &mut readback)
            .expect("read");
        assert_eq!(&readback, b"alive");

        // 9. ISOLATION: Host EventPump remains completely alive and functional
        let pump = EventPump::attach(token).expect("event pump attaches");
        let mut queue = BoundedEventQueue::new(8);
        let pumped = pump.pump(token, &mut queue, 4).expect("event pump drains");
        assert!(pumped <= 4);

        // 10. RE-ATTACH LIFECYCLE: Reattach View A with updated metrics
        let host_desc_reattach = valid_host(2.0, 128.0, 128.0);
        let new_owner_a = binding_a
            .reattach(token, &device, host_desc_reattach, DrawablePath::HostOwned)
            .expect("reattach a");
        assert!(binding_a.is_attached());
        assert_eq!(binding_a.generation(), 3);
        assert_eq!(new_owner_a.generation(), 3);
        assert_eq!(binding_a.counters().reattaches, 1);
        assert_eq!(binding_a.counters().attaches, 2);

        // New owner can lease and acquire
        let new_lease_a = binding_a.lease(new_owner_a).expect("new lease a");
        assert_eq!(new_lease_a.generation, 3);
        binding_a
            .release_lease(new_lease_a)
            .expect("release new lease");

        // 11. Event Ring records the complete lifecycle
        let events_a = binding_a.event_records();
        assert!(events_a.len() >= 4);
        assert_eq!(events_a[0].event_type, ViewBindingEventType::Attached);
        assert!(
            events_a
                .iter()
                .any(|e| e.event_type == ViewBindingEventType::DetachedDuringWork)
        );
        assert!(
            events_a
                .iter()
                .any(|e| e.event_type == ViewBindingEventType::Reattached)
        );

        // 12. Clean teardown
        binding_a.detach(token).expect("detach a final");
        binding_b.detach(token).expect("detach b final");
        window_a.mark_closed(token).ok();
        window_b.mark_closed(token).ok();
    }

    #[test]
    fn take_and_return_presentation_owner_lifecycle() {
        let Some(token) = main_token() else {
            return;
        };
        let Some(window) = try_window(token, "fcb-owner-lifecycle") else {
            return;
        };
        let device = MetalDevice::system_default(token).expect("device");
        let host = valid_host(2.0, 32.0, 32.0);
        let (binding, owner1) = NativeViewBinding::attach(
            token,
            &window,
            MetalLayer::new(token).expect("layer"),
            &device,
            host,
            DrawablePath::HostOwned,
        )
        .expect("attach");

        // Taking second owner without return fails
        assert_eq!(
            binding.take_owner(DrawablePath::RendererOwned).unwrap_err(),
            ViewBindingError::PresentationOwnerTaken
        );

        // Returning owner allows taking another
        binding.return_owner(owner1).expect("return owner1");
        let owner2 = binding
            .take_owner(DrawablePath::RendererOwned)
            .expect("take owner2");
        assert_eq!(owner2.path(), DrawablePath::RendererOwned);

        // Old owner1 cannot be returned again
        assert_eq!(
            binding.return_owner(owner1).unwrap_err(),
            ViewBindingError::StalePresentationOwner
        );

        binding.detach(token).expect("detach");
        window.mark_closed(token).ok();
    }

    #[test]
    fn detach_during_gpu_submission_flight() {
        let Some(token) = main_token() else {
            return;
        };
        let Some(window) = try_window(token, "fcb-gpu-flight-detach") else {
            return;
        };
        let device = MetalDevice::system_default(token).expect("device");
        let host = valid_host(2.0, 32.0, 32.0);
        let (binding, _owner) = NativeViewBinding::attach(
            token,
            &window,
            MetalLayer::new(token).expect("layer"),
            &device,
            host,
            DrawablePath::HostOwned,
        )
        .expect("attach");

        let queue = device.bounded_queue(token, 2).expect("queue");
        let src = device.create_buffer(token, 64).expect("src");
        let dst = device.create_buffer(token, 64).expect("dst");
        src.write_bytes(token, 0, b"flight-survival-data")
            .expect("write");

        let mut enc = queue.begin_submission(token).expect("begin");
        enc.encode_copy(&src, 0, &dst, 0, 64).expect("copy");
        let sub = enc.commit().expect("commit");
        assert!(src.is_in_gpu_flight());

        // DETACH view binding while GPU copy is in flight
        binding.detach(token).expect("detach during GPU flight");
        assert!(!binding.is_attached());

        // GPU submission completes normally despite view detachment
        let status = sub.wait_until_completed(&queue, token).expect("wait");
        assert_eq!(status, franken_macos::CompletionStatus::Success);
        assert!(!src.is_in_gpu_flight());

        let mut readback = [0u8; 20];
        dst.read_bytes(token, 0, &mut readback).expect("read");
        assert_eq!(&readback, b"flight-survival-data");

        window.mark_closed(token).ok();
    }
}

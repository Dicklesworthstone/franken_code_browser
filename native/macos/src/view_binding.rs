//! Host-owned native view binding: one CAMetalLayer, one presentation owner
//! (FCB-070.A).
//!
//! The host keeps the event loop, window, and device. This seam only binds a
//! validated render target to a window content view, issues exactly one
//! presentation owner, and detaches without tearing down host resources.

#![forbid(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::windowing::NativeWindow;
use crate::{BridgeError, DeviceId, MainThreadToken, MetalDevice, MetalLayer, NativeObject, ffi};

static BINDING_NONCE: AtomicU64 = AtomicU64::new(1);

/// Identity of one view-binding instance.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub struct ViewBindingId(u64);

impl ViewBindingId {
    fn next() -> Self {
        Self(BINDING_NONCE.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Qualified Metal pixel format admitted by this seam.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    /// `MTLPixelFormatBGRA8Unorm` (80). The only format this slice qualifies.
    Bgra8Unorm,
}

impl PixelFormat {
    const fn to_mtl(self) -> u64 {
        match self {
            Self::Bgra8Unorm => 80,
        }
    }
}

/// Color space named by the target descriptor. Binding does not install a
/// `CGColorSpace` object; it records the host's declared intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorSpace {
    Srgb,
}

/// Display metrics used to validate scale and drawable size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayMetrics {
    pub backing_scale: f64,
    pub drawable_width: f64,
    pub drawable_height: f64,
}

/// Host-supplied target configuration for a bound view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostTargetDescriptor {
    pub pixel_format: PixelFormat,
    pub color_space: ColorSpace,
    pub sample_count: u32,
    pub metrics: DisplayMetrics,
}

impl HostTargetDescriptor {
    pub fn validate(self) -> Result<(), ViewBindingError> {
        if self.sample_count != 1 {
            return Err(ViewBindingError::InvalidConfig);
        }
        let m = self.metrics;
        if !m.backing_scale.is_finite()
            || m.backing_scale <= 0.0
            || !m.drawable_width.is_finite()
            || !m.drawable_height.is_finite()
            || m.drawable_width <= 0.0
            || m.drawable_height <= 0.0
        {
            return Err(ViewBindingError::InvalidConfig);
        }
        Ok(())
    }
}

/// Renderer-side copy of the accepted target (format/size/scale/sample).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderTargetDescriptor {
    pub pixel_format: PixelFormat,
    pub color_space: ColorSpace,
    pub sample_count: u32,
    pub drawable_width: f64,
    pub drawable_height: f64,
    pub backing_scale: f64,
}

/// Exclusive lease on the bound render target. Dropping it does not detach.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderTargetLease {
    pub binding: ViewBindingId,
    pub generation: u64,
}

/// Who is allowed to acquire the next drawable from this binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrawablePath {
    HostOwned,
    RendererOwned,
}

/// Token proving exclusive presentation ownership for one binding generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationOwner {
    binding: ViewBindingId,
    generation: u64,
    path: DrawablePath,
}

impl PresentationOwner {
    pub const fn binding_id(self) -> ViewBindingId {
        self.binding
    }

    pub const fn path(self) -> DrawablePath {
        self.path
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Errors from the view-binding seam.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewBindingError {
    WrongThread,
    NotMainThread,
    WindowClosed,
    InvalidConfig,
    NativeUnavailable,
    AlreadyBound,
    NotBound,
    PresentationOwnerTaken,
    StalePresentationOwner,
    CrossDevice,
    DrawableUnavailable,
}

impl From<BridgeError> for ViewBindingError {
    fn from(error: BridgeError) -> Self {
        match error {
            BridgeError::NotMainThread => Self::NotMainThread,
            BridgeError::WrongThread => Self::WrongThread,
            BridgeError::UnsupportedPlatform | BridgeError::NullNativeObject => {
                Self::NativeUnavailable
            }
            _ => Self::NativeUnavailable,
        }
    }
}

/// Capacity of the structured event ring.
pub const VIEW_BINDING_EVENT_RING_CAPACITY: usize = 64;

/// Aggregate counters for binding operations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ViewBindingCounters {
    pub attaches: u64,
    pub detaches: u64,
    pub detaches_during_work: u64,
    pub reattaches: u64,
    pub leases_issued: u64,
    pub leases_released: u64,
    pub drawable_acquires: u64,
    pub rejected_stale_owner: u64,
    pub rejected_invalid_config: u64,
    pub rejected_second_owner: u64,
    pub rejected_not_bound: u64,
    pub rejected_wrong_thread: u64,
    pub rejected_window_closed: u64,
    pub rejected_already_bound: u64,
}

/// Structured binding events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewBindingEventType {
    Attached,
    Detached,
    DetachedDuringWork,
    Reattached,
    LeaseIssued,
    LeaseReleased,
    DrawableAcquired,
    RejectedStaleOwner,
    RejectedSecondOwner,
    RejectedInvalidConfig,
    RejectedNotBound,
    RejectedWrongThread,
    RejectedWindowClosed,
    RejectedAlreadyBound,
}

/// One structured event record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewBindingEventRecord {
    pub binding: ViewBindingId,
    pub generation: u64,
    pub event_type: ViewBindingEventType,
}

#[derive(Debug)]
struct EventRing {
    entries: Vec<ViewBindingEventRecord>,
    capacity: usize,
    write_pos: usize,
    total_events: u64,
}

impl EventRing {
    fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
            write_pos: 0,
            total_events: 0,
        }
    }

    fn record(&mut self, record: ViewBindingEventRecord) {
        self.total_events = self.total_events.saturating_add(1);
        if self.entries.len() < self.capacity {
            self.entries.push(record);
        } else {
            self.entries[self.write_pos] = record;
            self.write_pos = (self.write_pos + 1) % self.capacity;
        }
    }

    fn records(&self) -> Vec<ViewBindingEventRecord> {
        if self.entries.len() < self.capacity {
            self.entries.clone()
        } else {
            let mut out = Vec::with_capacity(self.capacity);
            for i in 0..self.capacity {
                let idx = (self.write_pos + i) % self.capacity;
                out.push(self.entries[idx]);
            }
            out
        }
    }
}

/// Bound native view: window content layer + exclusive presentation owner.
///
/// Attach does not take over `NSApplication` or the host event loop. Detach
/// unsets the layer and invalidates the owner; the device and window remain.
#[derive(Debug)]
pub struct NativeViewBinding {
    id: ViewBindingId,
    token: MainThreadToken,
    device_id: DeviceId,
    window: NativeObject,
    layer: MetalLayer,
    descriptor: Cell<RenderTargetDescriptor>,
    path: Cell<DrawablePath>,
    generation: Cell<u64>,
    owner_live: Cell<bool>,
    attached: Cell<bool>,
    active_leases: Cell<u32>,
    counters: Cell<ViewBindingCounters>,
    event_ring: RefCell<EventRing>,
}

impl NativeViewBinding {
    /// Bind `layer` to `window`'s content view as the sole presentation target.
    pub fn attach(
        token: MainThreadToken,
        window: &NativeWindow,
        layer: MetalLayer,
        device: &MetalDevice,
        host: HostTargetDescriptor,
        path: DrawablePath,
    ) -> Result<(Self, PresentationOwner), ViewBindingError> {
        token.assert_current()?;
        if window.is_closed() {
            return Err(ViewBindingError::WindowClosed);
        }
        if let Err(err) = host.validate() {
            return Err(err);
        }
        let retained_window =
            ffi::retain_object(window.raw_ref()).ok_or(ViewBindingError::NativeUnavailable)?;
        ffi::configure_metal_layer(
            layer.raw_ref(),
            device.raw_ref(),
            host.pixel_format.to_mtl(),
            host.metrics.drawable_width,
            host.metrics.drawable_height,
            host.metrics.backing_scale,
        );
        if !ffi::attach_layer_to_window(window.raw_ref(), layer.raw_ref()) {
            return Err(ViewBindingError::NativeUnavailable);
        }
        let id = ViewBindingId::next();
        let generation = 1;
        let mut event_ring = EventRing::new(VIEW_BINDING_EVENT_RING_CAPACITY);
        event_ring.record(ViewBindingEventRecord {
            binding: id,
            generation,
            event_type: ViewBindingEventType::Attached,
        });
        let binding = Self {
            id,
            token,
            device_id: device.device_id(),
            window: NativeObject::adopt(retained_window, token),
            layer,
            descriptor: Cell::new(RenderTargetDescriptor {
                pixel_format: host.pixel_format,
                color_space: host.color_space,
                sample_count: host.sample_count,
                drawable_width: host.metrics.drawable_width,
                drawable_height: host.metrics.drawable_height,
                backing_scale: host.metrics.backing_scale,
            }),
            path: Cell::new(path),
            generation: Cell::new(generation),
            owner_live: Cell::new(true),
            attached: Cell::new(true),
            active_leases: Cell::new(0),
            counters: Cell::new(ViewBindingCounters {
                attaches: 1,
                ..ViewBindingCounters::default()
            }),
            event_ring: RefCell::new(event_ring),
        };
        let owner = PresentationOwner {
            binding: id,
            generation,
            path,
        };
        Ok((binding, owner))
    }

    pub const fn id(&self) -> ViewBindingId {
        self.id
    }

    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    pub fn path(&self) -> DrawablePath {
        self.path.get()
    }

    pub fn is_attached(&self) -> bool {
        self.attached.get()
    }

    pub fn descriptor(&self) -> RenderTargetDescriptor {
        self.descriptor.get()
    }

    pub fn counters(&self) -> ViewBindingCounters {
        self.counters.get()
    }

    pub fn event_count(&self) -> u64 {
        self.event_ring.borrow().total_events
    }

    pub fn event_records(&self) -> Vec<ViewBindingEventRecord> {
        self.event_ring.borrow().records()
    }

    pub fn active_leases(&self) -> u32 {
        self.active_leases.get()
    }

    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// Acquire a render target lease using a live presentation owner.
    pub fn lease(&self, owner: PresentationOwner) -> Result<RenderTargetLease, ViewBindingError> {
        self.require_owner(owner)?;
        if !self.attached.get() {
            self.bump_counter(|c| c.rejected_not_bound += 1);
            self.record_event(ViewBindingEventType::RejectedNotBound);
            return Err(ViewBindingError::NotBound);
        }
        self.active_leases
            .set(self.active_leases.get().saturating_add(1));
        self.bump_counter(|c| c.leases_issued += 1);
        self.record_event(ViewBindingEventType::LeaseIssued);
        Ok(RenderTargetLease {
            binding: self.id,
            generation: self.generation.get(),
        })
    }

    /// Release an active render target lease.
    pub fn release_lease(&self, lease: RenderTargetLease) -> Result<(), ViewBindingError> {
        if lease.binding != self.id || lease.generation != self.generation.get() {
            self.bump_counter(|c| c.rejected_stale_owner += 1);
            self.record_event(ViewBindingEventType::RejectedStaleOwner);
            return Err(ViewBindingError::StalePresentationOwner);
        }
        self.active_leases
            .set(self.active_leases.get().saturating_sub(1));
        self.bump_counter(|c| c.leases_released += 1);
        self.record_event(ViewBindingEventType::LeaseReleased);
        Ok(())
    }

    /// A second live owner for the same generation is refused.
    pub fn take_owner(&self, path: DrawablePath) -> Result<PresentationOwner, ViewBindingError> {
        if !self.attached.get() {
            self.bump_counter(|c| c.rejected_not_bound += 1);
            self.record_event(ViewBindingEventType::RejectedNotBound);
            return Err(ViewBindingError::NotBound);
        }
        if self.owner_live.get() {
            self.bump_counter(|c| c.rejected_second_owner += 1);
            self.record_event(ViewBindingEventType::RejectedSecondOwner);
            return Err(ViewBindingError::PresentationOwnerTaken);
        }
        self.owner_live.set(true);
        self.path.set(path);
        Ok(PresentationOwner {
            binding: self.id,
            generation: self.generation.get(),
            path,
        })
    }

    /// Return a live presentation owner so a new owner may be taken.
    pub fn return_owner(&self, owner: PresentationOwner) -> Result<(), ViewBindingError> {
        if owner.binding != self.id || owner.generation != self.generation.get() {
            self.bump_counter(|c| c.rejected_stale_owner += 1);
            self.record_event(ViewBindingEventType::RejectedStaleOwner);
            return Err(ViewBindingError::StalePresentationOwner);
        }
        if !self.owner_live.get() {
            self.bump_counter(|c| c.rejected_stale_owner += 1);
            self.record_event(ViewBindingEventType::RejectedStaleOwner);
            return Err(ViewBindingError::StalePresentationOwner);
        }
        self.owner_live.set(false);
        Ok(())
    }

    /// Acquire the next drawable. Only the live presentation owner may do this.
    pub fn acquire_drawable(
        &self,
        token: MainThreadToken,
        owner: PresentationOwner,
    ) -> Result<NativeDrawable, ViewBindingError> {
        token.assert_current()?;
        if token != self.token {
            self.bump_counter(|c| c.rejected_wrong_thread += 1);
            self.record_event(ViewBindingEventType::RejectedWrongThread);
            return Err(ViewBindingError::WrongThread);
        }
        self.require_owner(owner)?;
        if !self.attached.get() {
            self.bump_counter(|c| c.rejected_not_bound += 1);
            self.record_event(ViewBindingEventType::RejectedNotBound);
            return Err(ViewBindingError::NotBound);
        }
        let raw = ffi::next_drawable(self.layer.raw_ref())
            .ok_or(ViewBindingError::DrawableUnavailable)?;
        self.bump_counter(|c| c.drawable_acquires += 1);
        self.record_event(ViewBindingEventType::DrawableAcquired);
        Ok(NativeDrawable {
            object: NativeObject::adopt(raw, token),
        })
    }

    /// Encodes and presents one clear-color frame through the bound layer.
    ///
    /// Minimal render-pass glue: the load action clears the drawable to
    /// `color`, the store keeps it, no draw calls yet (the atlas pipeline
    /// lands next). The drawable is presented on the command buffer before
    /// commit, per CAMetalLayer scheduling, and the CPU waits for
    /// completion so a demo/test caller observes the finished frame.
    pub fn present_clear(
        &self,
        token: MainThreadToken,
        owner: PresentationOwner,
        device: &MetalDevice,
        color: [f64; 4],
    ) -> Result<(), ViewBindingError> {
        token.assert_current()?;
        for channel in color {
            if !channel.is_finite() || !(0.0..=1.0).contains(&channel) {
                return Err(ViewBindingError::InvalidConfig);
            }
        }
        let drawable = self.acquire_drawable(token, owner)?;
        let queue =
            ffi::new_command_queue(device.raw_ref()).ok_or(ViewBindingError::NativeUnavailable)?;
        let cmd_buf = ffi::command_buffer(queue).ok_or(ViewBindingError::NativeUnavailable)?;
        let rpd = ffi::render_pass_descriptor().ok_or(ViewBindingError::NativeUnavailable)?;
        let attachments = ffi::color_attachments(rpd).ok_or(ViewBindingError::NativeUnavailable)?;
        let attachment =
            ffi::color_attachment_at(attachments, 0).ok_or(ViewBindingError::NativeUnavailable)?;
        let texture = ffi::drawable_texture(drawable.object.raw)
            .ok_or(ViewBindingError::NativeUnavailable)?;
        ffi::set_attachment_texture(attachment, texture);
        ffi::set_attachment_load_clear(attachment);
        ffi::set_attachment_clear_color(attachment, color);
        ffi::set_attachment_store(attachment);
        let encoder =
            ffi::render_command_encoder(cmd_buf, rpd).ok_or(ViewBindingError::NativeUnavailable)?;
        ffi::end_encoding(encoder);
        ffi::present_drawable_on_buffer(cmd_buf, drawable.object.raw);
        ffi::commit(cmd_buf);
        Ok(())
    }

    /// Detach the layer from the host window.
    ///
    /// The host window, device, and event loop stay alive. In-flight leases
    /// or owners for this generation become stale immediately. If work was
    /// in-flight (e.g. active render-target leases), it is recorded as
    /// `DetachedDuringWork`.
    pub fn detach(&self, token: MainThreadToken) -> Result<(), ViewBindingError> {
        token.assert_current()?;
        if token != self.token {
            self.bump_counter(|c| c.rejected_wrong_thread += 1);
            self.record_event(ViewBindingEventType::RejectedWrongThread);
            return Err(ViewBindingError::WrongThread);
        }
        if !self.attached.get() {
            self.bump_counter(|c| c.rejected_not_bound += 1);
            self.record_event(ViewBindingEventType::RejectedNotBound);
            return Err(ViewBindingError::NotBound);
        }
        let _ = ffi::detach_layer_from_window(self.window_raw());
        self.attached.set(false);
        self.owner_live.set(false);
        let had_work = self.active_leases.get() > 0;
        self.active_leases.set(0);
        let current_gen = self.generation.get();
        self.generation.set(current_gen.wrapping_add(1).max(1));
        self.bump_counter(|c| {
            c.detaches += 1;
            if had_work {
                c.detaches_during_work += 1;
            }
        });
        if had_work {
            self.record_event(ViewBindingEventType::DetachedDuringWork);
        } else {
            self.record_event(ViewBindingEventType::Detached);
        }
        Ok(())
    }

    /// Re-attach a previously detached binding to the same host window.
    ///
    /// Configures the Metal layer with updated metrics, attaches it to the
    /// host window, advances the generation, and returns a fresh
    /// presentation owner.
    pub fn reattach(
        &self,
        token: MainThreadToken,
        device: &MetalDevice,
        host: HostTargetDescriptor,
        path: DrawablePath,
    ) -> Result<PresentationOwner, ViewBindingError> {
        token.assert_current()?;
        if token != self.token {
            self.bump_counter(|c| c.rejected_wrong_thread += 1);
            self.record_event(ViewBindingEventType::RejectedWrongThread);
            return Err(ViewBindingError::WrongThread);
        }
        if self.attached.get() {
            self.bump_counter(|c| c.rejected_already_bound += 1);
            self.record_event(ViewBindingEventType::RejectedAlreadyBound);
            return Err(ViewBindingError::AlreadyBound);
        }
        if device.device_id() != self.device_id {
            return Err(ViewBindingError::CrossDevice);
        }
        if let Err(err) = host.validate() {
            self.bump_counter(|c| c.rejected_invalid_config += 1);
            self.record_event(ViewBindingEventType::RejectedInvalidConfig);
            return Err(err);
        }
        ffi::configure_metal_layer(
            self.layer.raw_ref(),
            device.raw_ref(),
            host.pixel_format.to_mtl(),
            host.metrics.drawable_width,
            host.metrics.drawable_height,
            host.metrics.backing_scale,
        );
        if !ffi::attach_layer_to_window(self.window_raw(), self.layer.raw_ref()) {
            return Err(ViewBindingError::NativeUnavailable);
        }
        let generation = self.generation.get().wrapping_add(1).max(1);
        self.generation.set(generation);
        self.descriptor.set(RenderTargetDescriptor {
            pixel_format: host.pixel_format,
            color_space: host.color_space,
            sample_count: host.sample_count,
            drawable_width: host.metrics.drawable_width,
            drawable_height: host.metrics.drawable_height,
            backing_scale: host.metrics.backing_scale,
        });
        self.path.set(path);
        self.attached.set(true);
        self.owner_live.set(true);
        self.active_leases.set(0);
        self.bump_counter(|c| {
            c.attaches += 1;
            c.reattaches += 1;
        });
        self.record_event(ViewBindingEventType::Reattached);
        Ok(PresentationOwner {
            binding: self.id,
            generation,
            path,
        })
    }

    fn window_raw(&self) -> ffi::ObjectRef {
        // NativeObject.raw is crate-visible via the parent module.
        self.window.raw
    }

    pub(crate) fn layer_raw(&self) -> ffi::ObjectRef {
        self.layer.raw_ref()
    }

    fn require_owner(&self, owner: PresentationOwner) -> Result<(), ViewBindingError> {
        if owner.binding != self.id || owner.generation != self.generation.get() {
            self.bump_counter(|c| c.rejected_stale_owner += 1);
            self.record_event(ViewBindingEventType::RejectedStaleOwner);
            return Err(ViewBindingError::StalePresentationOwner);
        }
        if !self.owner_live.get() {
            self.bump_counter(|c| c.rejected_stale_owner += 1);
            self.record_event(ViewBindingEventType::RejectedStaleOwner);
            return Err(ViewBindingError::StalePresentationOwner);
        }
        Ok(())
    }

    fn bump_counter(&self, f: impl FnOnce(&mut ViewBindingCounters)) {
        let mut counters = self.counters.get();
        f(&mut counters);
        self.counters.set(counters);
    }

    fn record_event(&self, event_type: ViewBindingEventType) {
        self.event_ring.borrow_mut().record(ViewBindingEventRecord {
            binding: self.id,
            generation: self.generation.get(),
            event_type,
        });
    }
}

impl Drop for NativeViewBinding {
    fn drop(&mut self) {
        if self.attached.get() {
            let _ = ffi::detach_layer_from_window(self.window_raw());
            self.attached.set(false);
            self.owner_live.set(false);
            self.active_leases.set(0);
        }
    }
}

/// An acquired CAMetalDrawable. Dropping it releases the native object; it
/// does not present. Presentation is owned by a later pacing seam.
#[derive(Debug)]
pub struct NativeDrawable {
    object: NativeObject,
}

impl NativeDrawable {
    pub fn present(&self) {
        ffi::present_drawable(self.object.raw);
    }
}

#![deny(missing_debug_implementations)]

//! Safe, thread-affine ownership for the narrow AppKit/Metal boundary.
//!
//! This crate owns only native ABI/object/device lifetimes.  It does not own
//! an application event loop, source policy, parsing, search, rendering
//! policy, or host runtime.  The current exception policy is deliberately
//! narrow: fixed ABI calls return `Result` for null/affinity failures, but no
//! Objective-C exception catcher is installed and exception-to-`Result`
//! conversion is not claimed by this crate.

pub mod callbacks;
mod ffi;
pub mod pacing;
pub mod render_batch;
pub mod scheduler;
pub mod submission;
pub mod view_binding;
pub mod windowing;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, ThreadId};

static DEVICE_ID_NONCE: AtomicU64 = AtomicU64::new(1);
static BUFFER_ID_NONCE: AtomicU64 = AtomicU64::new(1);

/// Monotonic, process-unique Metal device identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DeviceId(pub(crate) u64);

impl DeviceId {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

pub use pacing::{
    DEFAULT_MAX_IN_FLIGHT, DisplayLinkAvailability, DisplayLinkRoute, DisplayPacingEngine,
    PACING_EVENT_RING_CAPACITY, PacedFrame, PacingConfig, PacingCounters, PacingError,
    PacingEventRecord, PacingEventType, PacingOutcome,
};
pub use render_batch::{
    AtlasFrameComposer, ClipLayer, ComposedFrame, DrawableTarget, FrameBudgetLimits,
    FrameCompositionError, FrameInvalidation, FrameRevisions, InvalidationCounters, Primitive,
    PrimitiveKind, RenderBatch, ShaderRecord, TextFrameComposer, compare_frames,
};
pub use scheduler::{
    CameraSnapshot, FrameRequestReason, FrameScheduler, SCHEDULER_EVENT_RING_CAPACITY,
    ScheduledFrame, SchedulerCounters, SchedulerEventRecord, SchedulerEventType, SchedulerOutcome,
    WindowVisibility,
};
pub use submission::{
    BoundedSubmissionQueue, CompletionCell, CompletionStatus, Submission, SubmissionCounters,
    SubmissionEncoder, SubmissionError, SubmissionEventRecord, SubmissionEventType, SubmissionId,
};
pub use view_binding::{
    ColorSpace, DisplayMetrics, DrawablePath, HostTargetDescriptor, NativeDrawable,
    NativeViewBinding, PixelFormat, PresentationOwner, RenderTargetDescriptor, RenderTargetLease,
    VIEW_BINDING_EVENT_RING_CAPACITY, ViewBindingCounters, ViewBindingError,
    ViewBindingEventRecord, ViewBindingEventType, ViewBindingId,
};

pub const ABI_POLICY: &str =
    "fixed private Apple signatures; typed owned objects; no generic pointer or selector escapes";
pub const PANIC_EXCEPTION_POLICY: &str = "Rust panics do not cross the bridge; Objective-C exception containment is unavailable and unqualified";

/// Services pending window updates and commits Core Animation state for
/// hosts running a manual event pump. Call once per pump iteration after
/// draining events: `updateWindows` is what paints `needsDisplay` views
/// when the runloop never runs its automatic update pass.
pub fn flush_display() {
    ffi::update_windows();
    ffi::flush_core_animation();
}

/// Hands the main thread to the platform event/draw runloop. Returns when
/// the application terminates. Hosts that need their own loop use
/// [`EventPump`] + [`flush_display`] instead.
pub fn run_app() {
    ffi::app_run();
}

/// Requests regular foreground-application behavior: Dock icon, Cmd-Tab
/// entry, full app menu presence. Call once before creating windows.
pub fn set_regular_activation_policy() -> bool {
    ffi::set_activation_policy_regular()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MainThreadToken {
    thread: ThreadId,
}

impl MainThreadToken {
    pub fn capture_current() -> Result<Self, BridgeError> {
        if !ffi::is_main_thread() {
            return Err(BridgeError::NotMainThread);
        }
        Ok(Self {
            thread: thread::current().id(),
        })
    }

    fn assert_current(self) -> Result<(), BridgeError> {
        let current = thread::current().id();
        if current == self.thread {
            Ok(())
        } else {
            Err(BridgeError::WrongThread)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeError {
    NotMainThread,
    WrongThread,
    UnsupportedPlatform,
    NullNativeObject,
    RetainFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnershipSnapshot {
    pub live_wrappers: u32,
    pub retains: u32,
    pub releases: u32,
}

#[derive(Debug)]
struct OwnershipState {
    live_wrappers: Cell<u32>,
    retains: Cell<u32>,
    releases: Cell<u32>,
}

impl OwnershipState {
    fn new() -> Self {
        Self {
            live_wrappers: Cell::new(1),
            retains: Cell::new(0),
            releases: Cell::new(0),
        }
    }

    fn snapshot(&self) -> OwnershipSnapshot {
        OwnershipSnapshot {
            live_wrappers: self.live_wrappers.get(),
            retains: self.retains.get(),
            releases: self.releases.get(),
        }
    }

    fn next_retain_counts(&self) -> Result<(u32, u32), BridgeError> {
        let live = self
            .live_wrappers
            .get()
            .checked_add(1)
            .ok_or(BridgeError::RetainFailed)?;
        let retains = self
            .retains
            .get()
            .checked_add(1)
            .ok_or(BridgeError::RetainFailed)?;
        Ok((live, retains))
    }

    fn commit_retain(&self, live: u32, retains: u32) {
        self.live_wrappers.set(live);
        self.retains.set(retains);
    }
}

#[derive(Debug)]
struct NativeObject {
    raw: ffi::ObjectRef,
    owner: ThreadId,
    state: Rc<RefCell<OwnershipState>>,
    #[cfg(test)]
    synthetic: bool,
}

impl NativeObject {
    fn adopt(raw: ffi::ObjectRef, owner: MainThreadToken) -> Self {
        Self {
            raw,
            owner: owner.thread,
            state: Rc::new(RefCell::new(OwnershipState::new())),
            #[cfg(test)]
            synthetic: false,
        }
    }

    #[cfg(test)]
    fn adopt_test(owner: MainThreadToken) -> Self {
        Self {
            raw: ffi::test_object_ref(),
            owner: owner.thread,
            state: Rc::new(RefCell::new(OwnershipState::new())),
            synthetic: true,
        }
    }

    fn try_clone(&self, token: MainThreadToken) -> Result<Self, BridgeError> {
        token.assert_current()?;
        if token.thread != self.owner {
            return Err(BridgeError::WrongThread);
        }
        let (next_live, next_retains) = self.state.borrow().next_retain_counts()?;
        #[cfg(test)]
        let raw = if self.synthetic {
            self.raw
        } else {
            ffi::retain_object(self.raw).ok_or(BridgeError::RetainFailed)?
        };
        #[cfg(not(test))]
        let raw = ffi::retain_object(self.raw).ok_or(BridgeError::RetainFailed)?;
        self.state.borrow().commit_retain(next_live, next_retains);
        Ok(Self {
            raw,
            owner: self.owner,
            state: Rc::clone(&self.state),
            #[cfg(test)]
            synthetic: self.synthetic,
        })
    }

    fn ownership(&self) -> OwnershipSnapshot {
        self.state.borrow().snapshot()
    }

    #[cfg(test)]
    fn set_counts_for_test(&self, live: u32, retains: u32) {
        let state = self.state.borrow();
        state.live_wrappers.set(live);
        state.retains.set(retains);
    }
}

impl Drop for NativeObject {
    fn drop(&mut self) {
        #[cfg(not(test))]
        ffi::release_object(self.raw);
        #[cfg(test)]
        if !self.synthetic {
            ffi::release_object(self.raw);
        }
        let state = self.state.borrow();
        state
            .live_wrappers
            .set(state.live_wrappers.get().saturating_sub(1));
        state.releases.set(state.releases.get().saturating_add(1));
    }
}

#[derive(Debug)]
pub struct Application {
    object: NativeObject,
}

impl Application {
    pub fn shared(token: MainThreadToken) -> Result<Self, BridgeError> {
        token.assert_current()?;
        let raw = ffi::shared_application().ok_or(if cfg!(target_os = "macos") {
            BridgeError::NullNativeObject
        } else {
            BridgeError::UnsupportedPlatform
        })?;
        Ok(Self {
            object: NativeObject::adopt(raw, token),
        })
    }

    pub fn retained(&self, token: MainThreadToken) -> Result<Self, BridgeError> {
        Ok(Self {
            object: self.object.try_clone(token)?,
        })
    }

    pub fn ownership(&self) -> OwnershipSnapshot {
        self.object.ownership()
    }
}

#[derive(Debug)]
pub struct MetalLayer {
    object: NativeObject,
}

impl MetalLayer {
    pub fn new(token: MainThreadToken) -> Result<Self, BridgeError> {
        token.assert_current()?;
        let raw = ffi::new_metal_layer().ok_or(if cfg!(target_os = "macos") {
            BridgeError::NullNativeObject
        } else {
            BridgeError::UnsupportedPlatform
        })?;
        Ok(Self {
            object: NativeObject::adopt(raw, token),
        })
    }

    pub fn retained(&self, token: MainThreadToken) -> Result<Self, BridgeError> {
        Ok(Self {
            object: self.object.try_clone(token)?,
        })
    }

    pub(crate) fn raw_ref(&self) -> ffi::ObjectRef {
        self.object.raw
    }

    #[cfg(test)]
    pub fn test_new(token: MainThreadToken) -> Self {
        Self {
            object: NativeObject::adopt_test(token),
        }
    }

    pub fn ownership(&self) -> OwnershipSnapshot {
        self.object.ownership()
    }
}

#[derive(Debug)]
pub struct MetalDevice {
    object: NativeObject,
    device_id: DeviceId,
    resource_epoch: Rc<Cell<u64>>,
}

impl MetalDevice {
    pub fn system_default(token: MainThreadToken) -> Result<Self, BridgeError> {
        token.assert_current()?;
        let raw = ffi::default_metal_device().ok_or(if cfg!(target_os = "macos") {
            BridgeError::NullNativeObject
        } else {
            BridgeError::UnsupportedPlatform
        })?;
        let device_id = DeviceId(DEVICE_ID_NONCE.fetch_add(1, Ordering::Relaxed));
        Ok(Self {
            object: NativeObject::adopt(raw, token),
            device_id,
            resource_epoch: Rc::new(Cell::new(1)),
        })
    }

    pub fn retained(&self, token: MainThreadToken) -> Result<Self, BridgeError> {
        Ok(Self {
            object: self.object.try_clone(token)?,
            device_id: self.device_id,
            resource_epoch: Rc::clone(&self.resource_epoch),
        })
    }

    pub(crate) fn resource_epoch(&self) -> u64 {
        self.resource_epoch.get()
    }

    pub(crate) fn resource_epoch_cell(&self) -> Rc<Cell<u64>> {
        Rc::clone(&self.resource_epoch)
    }

    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    #[allow(dead_code)]
    pub(crate) fn raw_ref(&self) -> ffi::ObjectRef {
        self.object.raw
    }

    #[cfg(test)]
    pub fn test_new(token: MainThreadToken) -> Self {
        Self {
            object: NativeObject::adopt_test(token),
            device_id: DeviceId(DEVICE_ID_NONCE.fetch_add(1, Ordering::Relaxed)),
            resource_epoch: Rc::new(Cell::new(1)),
        }
    }

    pub fn ownership(&self) -> OwnershipSnapshot {
        self.object.ownership()
    }
}

impl MetalDevice {
    /// Allocate a Metal buffer with the given length in bytes.
    ///
    /// Uses shared storage (CPU-GPU unified on Apple Silicon) so the CPU can
    /// write via the `contents` pointer without a copy.
    pub(crate) fn new_buffer_raw(&self, length: u64) -> Option<ffi::ObjectRef> {
        ffi::new_buffer_with_length(self.object.raw, length)
    }

    pub(crate) fn buffer_contents_raw(&self, buffer: ffi::ObjectRef) -> *mut u8 {
        ffi::buffer_contents(buffer)
    }

    pub(crate) fn new_command_queue_raw(&self) -> Option<ffi::ObjectRef> {
        ffi::new_command_queue(self.object.raw)
    }
}

impl MetalDevice {
    /// Allocate a validated Metal buffer (FCB-005.A).
    pub fn create_buffer(
        &self,
        token: MainThreadToken,
        length: usize,
    ) -> Result<MetalBuffer, BufferError> {
        token.assert_current()?;
        if length == 0 {
            return Err(BufferError::ZeroLength);
        }
        let object = self
            .new_buffer_raw(length as u64)
            .ok_or(BufferError::AllocationFailed)?;
        let contents = self.buffer_contents_raw(object);
        let native = NativeObject::adopt(object, token);
        let buffer_id = BUFFER_ID_NONCE.fetch_add(1, Ordering::Relaxed);
        Ok(MetalBuffer {
            object: native,
            device_id: self.device_id,
            buffer_id,
            generation: Cell::new(self.resource_epoch()),
            length,
            contents,
            gpu_in_flight: Rc::new(Cell::new(false)),
        })
    }

    /// Create a bounded submission queue with pre-reserved completion storage.
    pub fn bounded_queue(
        &self,
        token: MainThreadToken,
        capacity: usize,
    ) -> Result<crate::submission::BoundedSubmissionQueue, crate::submission::SubmissionError> {
        crate::submission::BoundedSubmissionQueue::new(self, token, capacity)
    }

    /// Create a command queue for GPU round-trip verification.
    pub fn command_queue(&self, token: MainThreadToken) -> Result<MetalCommandQueue, BufferError> {
        token.assert_current()?;
        let object = self
            .new_command_queue_raw()
            .ok_or(BufferError::AllocationFailed)?;
        Ok(MetalCommandQueue {
            object: NativeObject::adopt(object, token),
        })
    }
}

/// Typed error from the Metal buffer seam (FCB-005.A).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferError {
    /// Zero-length buffers are not valid Metal allocations.
    ZeroLength,
    /// The requested length exceeds the maximum allocation size.
    TooLarge,
    /// The device refused the allocation.
    AllocationFailed,
    /// The device handle does not match the buffer's creating device.
    CrossDevice,
    /// The buffer was already destroyed, invalidated, or evicted.
    Stale,
    /// The buffer is in GPU flight; CPU writes are refused.
    GpuInFlight,
    /// The main thread is required but not available.
    NotMainThread,
    /// Wrong thread was used.
    WrongThread,
}

impl From<BridgeError> for BufferError {
    fn from(error: BridgeError) -> Self {
        match error {
            BridgeError::NotMainThread => Self::NotMainThread,
            BridgeError::WrongThread => Self::WrongThread,
            _ => Self::AllocationFailed,
        }
    }
}

/// An owned Metal buffer with validated length and CPU-mapped access.
#[derive(Debug)]
pub struct MetalBuffer {
    object: NativeObject,
    device_id: DeviceId,
    buffer_id: u64,
    generation: Cell<u64>,
    length: usize,
    contents: *mut u8,
    gpu_in_flight: Rc<Cell<bool>>,
}

impl MetalBuffer {
    /// The allocated length in bytes.
    pub fn len(&self) -> usize {
        self.length
    }

    /// Whether the buffer is empty (always false; zero-length is refused).
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    pub fn buffer_id(&self) -> u64 {
        self.buffer_id
    }

    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    pub fn invalidate(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    pub fn flight_cell(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.gpu_in_flight)
    }

    pub(crate) fn raw_ref(&self) -> ffi::ObjectRef {
        self.object.raw
    }

    /// Whether the GPU is currently using this buffer.
    pub fn is_in_gpu_flight(&self) -> bool {
        self.gpu_in_flight.get()
    }

    /// Begin a GPU flight; CPU writes are refused until `end_gpu_flight`.
    pub fn begin_gpu_flight(&self) {
        self.gpu_in_flight.set(true);
    }

    /// End the GPU flight; CPU access is restored.
    pub fn end_gpu_flight(&self) {
        self.gpu_in_flight.set(false);
    }

    /// Write bytes into the buffer at the given offset.
    ///
    /// Refused while a GPU flight is in progress.
    pub fn write_bytes(
        &self,
        token: MainThreadToken,
        offset: usize,
        data: &[u8],
    ) -> Result<(), BufferError> {
        token.assert_current()?;
        if self.gpu_in_flight.get() {
            return Err(BufferError::GpuInFlight);
        }
        if offset + data.len() > self.length {
            return Err(BufferError::TooLarge);
        }
        // SAFETY: the contents pointer is valid for the buffer's lifetime,
        // which is guaranteed by the MetalBuffer holding a +1 retain.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), self.contents.add(offset), data.len());
        }
        Ok(())
    }

    /// Read bytes from the buffer at the given offset.
    pub fn read_bytes(
        &self,
        token: MainThreadToken,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<(), BufferError> {
        token.assert_current()?;
        if offset + buf.len() > self.length {
            return Err(BufferError::TooLarge);
        }
        // SAFETY: same lifetime guarantee as write_bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(self.contents.add(offset), buf.as_mut_ptr(), buf.len());
        }
        Ok(())
    }
}

/// A command queue for GPU round-trip verification.
#[derive(Debug)]
pub struct MetalCommandQueue {
    object: NativeObject,
}

impl MetalCommandQueue {
    /// Execute a GPU byte round-trip via a blit encoder: copy `size` bytes
    /// from `src` at `src_off` to `dst` at `dst_off`, commit, and wait.
    pub fn gpu_copy_and_wait(
        &self,
        token: MainThreadToken,
        src: &MetalBuffer,
        src_off: usize,
        dst: &MetalBuffer,
        dst_off: usize,
        size: usize,
    ) -> Result<(), BufferError> {
        token.assert_current()?;
        let cmd_buf = ffi::command_buffer(self.object.raw).ok_or(BufferError::AllocationFailed)?;
        let encoder = ffi::blit_encoder(cmd_buf).ok_or(BufferError::AllocationFailed)?;
        ffi::copy_bytes(
            encoder,
            src.object.raw,
            src_off as u64,
            dst.object.raw,
            dst_off as u64,
            size as u64,
        );
        ffi::end_encoding(encoder);
        ffi::commit(cmd_buf);
        ffi::wait_until_completed(cmd_buf);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingError {
    Bridge(BridgeError),
    AlreadyAttached,
    NotAttached,
    ObjectAffinityMismatch,
}

impl From<BridgeError> for BindingError {
    fn from(error: BridgeError) -> Self {
        Self::Bridge(error)
    }
}

#[derive(Debug)]
pub struct DetachedLayer {
    pub layer: MetalLayer,
    pub device: MetalDevice,
}

#[derive(Debug)]
pub struct MetalLayerBinding {
    owner: MainThreadToken,
    attached: Option<DetachedLayer>,
}

impl MetalLayerBinding {
    pub fn new(owner: MainThreadToken) -> Self {
        Self {
            owner,
            attached: None,
        }
    }

    pub fn attach(
        &mut self,
        token: MainThreadToken,
        layer: MetalLayer,
        device: MetalDevice,
    ) -> Result<(), BindingError> {
        token.assert_current()?;
        if token != self.owner {
            return Err(BindingError::ObjectAffinityMismatch);
        }
        if self.attached.is_some() {
            return Err(BindingError::AlreadyAttached);
        }
        if layer.object.owner != token.thread || device.object.owner != token.thread {
            return Err(BindingError::ObjectAffinityMismatch);
        }
        self.attached = Some(DetachedLayer { layer, device });
        Ok(())
    }

    pub fn detach(&mut self, token: MainThreadToken) -> Result<DetachedLayer, BindingError> {
        token.assert_current()?;
        if token != self.owner {
            return Err(BindingError::ObjectAffinityMismatch);
        }
        self.attached.take().ok_or(BindingError::NotAttached)
    }

    pub fn is_attached(&self) -> bool {
        self.attached.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_object(token: MainThreadToken) -> NativeObject {
        NativeObject::adopt_test(token)
    }

    fn test_layer(token: MainThreadToken) -> MetalLayer {
        MetalLayer {
            object: test_object(token),
        }
    }

    fn test_device(token: MainThreadToken) -> MetalDevice {
        MetalDevice {
            object: test_object(token),
            device_id: DeviceId(DEVICE_ID_NONCE.fetch_add(1, Ordering::Relaxed)),
            resource_epoch: Rc::new(Cell::new(1)),
        }
    }

    #[test]
    fn retain_and_release_counters_track_owned_wrappers() {
        let token = MainThreadToken {
            thread: thread::current().id(),
        };
        let object = test_layer(token);
        let clone = object.retained(token).expect("same-thread retain succeeds");
        assert_eq!(object.ownership().live_wrappers, 2);
        assert_eq!(object.ownership().retains, 1);
        drop(clone);
        assert_eq!(object.ownership().live_wrappers, 1);
        assert_eq!(object.ownership().releases, 1);
    }

    #[test]
    fn retain_overflow_is_rejected_before_native_retain() {
        let token = MainThreadToken {
            thread: thread::current().id(),
        };
        let object = test_layer(token);
        object.object.set_counts_for_test(u32::MAX, 0);
        assert!(matches!(
            object.retained(token),
            Err(BridgeError::RetainFailed)
        ));
        assert_eq!(object.ownership().live_wrappers, u32::MAX);
        assert_eq!(object.ownership().retains, 0);

        object.object.set_counts_for_test(1, u32::MAX);
        assert!(matches!(
            object.retained(token),
            Err(BridgeError::RetainFailed)
        ));
        assert_eq!(object.ownership().live_wrappers, 1);
        assert_eq!(object.ownership().retains, u32::MAX);
    }

    #[test]
    fn exception_policy_does_not_claim_unimplemented_containment() {
        assert!(PANIC_EXCEPTION_POLICY.contains("unavailable"));
        assert!(PANIC_EXCEPTION_POLICY.contains("unqualified"));
    }

    #[test]
    fn attach_detach_is_idempotence_safe_and_preserves_ownership() {
        let token = MainThreadToken {
            thread: thread::current().id(),
        };
        let mut binding = MetalLayerBinding::new(token);
        assert!(matches!(
            binding.detach(token),
            Err(BindingError::NotAttached)
        ));
        binding
            .attach(token, test_layer(token), test_device(token))
            .expect("first attach succeeds");
        assert!(matches!(
            binding.attach(token, test_layer(token), test_device(token)),
            Err(BindingError::AlreadyAttached)
        ));
        let detached = binding.detach(token).expect("first detach succeeds");
        assert!(!binding.is_attached());
        assert_eq!(detached.layer.ownership().live_wrappers, 1);
        assert!(matches!(
            binding.detach(token),
            Err(BindingError::NotAttached)
        ));
    }

    #[test]
    fn wrong_affinity_is_rejected_before_attach() {
        let owner = MainThreadToken {
            thread: thread::current().id(),
        };
        let other = thread::spawn(|| MainThreadToken {
            thread: thread::current().id(),
        })
        .join()
        .expect("thread token capture succeeds");
        let mut binding = MetalLayerBinding::new(owner);
        let result = binding.attach(other, test_layer(owner), test_device(owner));
        assert_eq!(result, Err(BindingError::Bridge(BridgeError::WrongThread)));
        assert!(!binding.is_attached());
    }
}

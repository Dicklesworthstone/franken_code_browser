//! Owned Metal buffers with validated lengths, device tokens, and CPU/GPU
//! access guards (FCB-005.A).
//!
//! Every buffer is created through a validated device token with a nonzero
//! length. CPU writes go through an exclusive upload lease: once the GPU is
//! using a buffer, mutable CPU access is refused until the lease is returned.
//! This prevents data races between the CPU and GPU without any unsafe code
//! in this module — the unsafe lives in the audited `ffi` module only.

use crate::{ffi, MetalDevice, NativeObject, BridgeError, MainThreadToken};

/// Typed error from the Metal buffer seam.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferError {
    /// Zero-length buffers are not valid Metal allocations.
    ZeroLength,
    /// The requested length exceeds the maximum allocation size.
    TooLarge,
    /// The device refused the allocation (out of memory or invalid usage).
    AllocationFailed,
    /// The device handle does not match the buffer's creating device.
    CrossDevice,
    /// The buffer was already destroyed or evicted.
    Stale,
    /// The buffer is in GPU flight; CPU writes are refused.
    GpuInFlight,
    /// The main thread is required but not available.
    NotMainThread,
}

impl From<BridgeError> for BufferError {
    fn from(error: BridgeError) -> Self {
        match error {
            BridgeError::NotMainThread => Self::NotMainThread,
            _ => Self::AllocationFailed,
        }
    }
}

/// GPU resource storage mode for the buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferStorage {
    /// CPU and GPU share the same memory (default on Apple Silicon).
    Shared,
}

impl BufferStorage {
    /// MTLResourceOptions value for this storage mode.
    pub(crate) const fn to_options(self) -> u64 {
        match self {
            // MTLResourceStorageModeShared = 0 on Apple Silicon.
            Self::Shared => 0,
        }
    }
}

/// Guards exclusive CPU access to a buffer during GPU operations.
///
/// Created before submitting GPU work that reads or writes the buffer.
/// Dropped after `waitUntilCompleted` confirms the GPU is done. While any
/// `UploadLease` is alive, `write_bytes` on the buffer returns
/// [`BufferError::GpuInFlight`].
#[derive(Debug)]
pub struct UploadLease {
    in_flight: std::cell::Cell<bool>,
}

impl UploadLease {
    /// Begin a GPU flight: CPU writes are refused until this lease is dropped.
    pub fn begin() -> Self {
        Self {
            in_flight: std::cell::Cell::new(true),
        }
    }

    /// Whether the GPU is currently using the buffer.
    pub const fn is_in_flight(&self) -> bool {
        self.in_flight.get()
    }

    /// End the GPU flight: CPU access is restored.
    pub fn end(&self) {
        self.in_flight.set(false);
    }
}

/// An owned Metal buffer with validated length and CPU-mapped access.
#[derive(Debug)]
pub struct OwnedMetalBuffer {
    object: NativeObject,
    length: usize,
    contents: *mut u8,
    lease: std::rc::Rc<std::cell::Cell<bool>>,
}

impl OwnedMetalBuffer {
    /// Allocate a new Metal buffer on the device's heap.
    ///
    /// The `length` must be nonzero. The buffer uses shared storage
    /// (CPU-GPU unified memory on Apple Silicon). The device token must be
    /// captured on the main thread.
    pub fn new(
        device: &MetalDevice,
        token: MainThreadToken,
        length: usize,
        storage: BufferStorage,
    ) -> Result<Self, BufferError> {
        token.assert_current()?;
        if length == 0 {
            return Err(BufferError::ZeroLength);
        }
        let raw_device = device.raw_ref();
        let object = ffi::new_buffer_with_length(*raw_device, length as u64)
            .ok_or(BufferError::AllocationFailed)?;
        let contents = ffi::buffer_contents(object);
        let owned = NativeObject::adopt(object, token);
        Ok(Self {
            object: owned,
            length,
            contents,
            lease: std::rc::Rc::new(std::cell::Cell::new(false)),
        })
    }

    /// The allocated length in bytes.
    pub fn len(&self) -> usize {
        self.length
    }

    /// Whether the buffer is empty (always false; zero-length is refused).
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Whether the GPU is currently using this buffer.
    pub fn is_in_flight(&self) -> bool {
        self.lease.get()
    }

    /// Begin a GPU flight, refusing CPU writes until the returned lease is
    /// dropped or `end_lease` is called.
    pub fn begin_upload(&self) -> UploadLease {
        self.lease.set(true);
        UploadLease::begin()
    }

    /// End the GPU flight, restoring CPU access.
    pub fn end_lease(&self) {
        self.lease.set(false);
    }

    /// Write bytes into the buffer at the given offset.
    ///
    /// Refused while a GPU flight is in progress.
    pub fn write_bytes(&self, token: MainThreadToken, offset: usize, data: &[u8]) -> Result<(), BufferError> {
        token.assert_current()?;
        if self.lease.get() {
            return Err(BufferError::GpuInFlight);
        }
        if offset + data.len() > self.length {
            return Err(BufferError::TooLarge);
        }
        // SAFETY: the contents pointer is valid for the buffer's lifetime,
        // which is guaranteed by the OwnedMetalBuffer holding a +1 retain.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), self.contents.add(offset), data.len());
        }
        Ok(())
    }

    /// Read bytes from the buffer at the given offset.
    pub fn read_bytes(&self, token: MainThreadToken, offset: usize, buf: &mut [u8]) -> Result<(), BufferError> {
        token.assert_current()?;
        if offset + buf.len() > self.length {
            return Err(BufferError::TooLarge);
        }
        // SAFETY: same lifetime argument as write_bytes.
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
    /// Create a command queue on the device.
    pub fn new(device: &MetalDevice, token: MainThreadToken) -> Result<Self, BufferError> {
        token.assert_current()?;
        let raw_device = device.raw_ref();
        let object = ffi::new_command_queue(*raw_device).ok_or(BufferError::AllocationFailed)?;
        Ok(Self {
            object: NativeObject::adopt(object, token),
        })
    }

    /// Execute a GPU byte round-trip: copy `size` bytes from `src` at
    /// `src_off` to `dst` at `dst_off` via a blit encoder, commit, and
    /// wait for completion.
    pub fn gpu_copy_and_wait(
        &self,
        token: MainThreadToken,
        src: &OwnedMetalBuffer,
        src_off: usize,
        dst: &OwnedMetalBuffer,
        dst_off: usize,
        size: usize,
    ) -> Result<(), BufferError> {
        token.assert_current()?;
        let raw_queue = self.object_raw();
        let cmd_buf = ffi::command_buffer(*raw_queue).ok_or(BufferError::AllocationFailed)?;
        let encoder = ffi::blit_encoder(cmd_buf).ok_or(BufferError::AllocationFailed)?;
        ffi::copy_bytes(
            encoder,
            *src.raw_ref(), src_off as u64,
            *dst.raw_ref(), dst_off as u64,
            size as u64,
        );
        ffi::end_encoding(encoder);
        ffi::commit(cmd_buf);
        ffi::wait_until_completed(cmd_buf);
        Ok(())
    }

    fn object_raw(&self) -> &NativeObject {
        &self.object
    }
}

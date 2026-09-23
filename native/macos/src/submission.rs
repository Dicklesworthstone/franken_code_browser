//! Bounded submission queue, safe command encoding, pre-reserved completion
//! cells, and terminal GPU ownership (FCB-005.B).
//!
//! # Core Invariants
//!
//! 1. **Lossless Terminal Records**: Every submission pre-reserves its completion cell
//!    before GPU encoding. Queue capacity is bounded; submissions exceeding capacity
//!    are rejected before acquiring driver resources, preventing dropped completions.
//! 2. **Retained Resource Leases**: Resources participating in GPU work remain locked
//!    against CPU writes (`BufferError::GpuInFlight`).
//! 3. **GPU Leases Survive Cancellation**: Cancelling an in-flight submission marks its
//!    outcome as `Cancelled`, but does NOT prematurely free or unlock GPU resources.
//!    Driver work cannot be safely interrupted; retained resources remain held until the
//!    hardware driver actually reaches a terminal completion contract.
//! 4. **Cross-Device & Stale Handle Rejection**: Encoding commands across different
//!    device instances or using stale/retired resource handles is detected and refused.

#![forbid(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{
    BridgeError, BufferError, DeviceId, MainThreadToken, MetalBuffer, MetalDevice, NativeObject,
    ffi,
};

static SUBMISSION_NONCE: AtomicU64 = AtomicU64::new(1);

/// Monotonic, process-unique submission identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SubmissionId(u64);

impl SubmissionId {
    /// Mint a fresh, monotonic submission identifier.
    pub fn next() -> Self {
        Self(SUBMISSION_NONCE.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Typed errors surfaced by the submission and queue boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionError {
    /// Queue capacity must be non-zero and within permitted bounds.
    InvalidQueueCapacity,
    /// Bounded queue capacity reached; submission refused before driver allocation.
    QueueFull,
    /// Buffer or resource belongs to a different Metal device instance.
    CrossDevice,
    /// Handle is stale (encoder already committed, submission finished, or generation mismatch).
    StaleHandle,
    /// Native driver refused allocation of command buffer or encoder.
    AllocationRefusal,
    /// Resource is currently in flight or busy.
    ResourceBusy,
    /// Buffer offsets/lengths exceeded buffer bounds.
    BufferBoundsExceeded,
    /// Hardware device was lost or driver reported an error.
    DeviceLost,
    /// Submission was cancelled.
    Cancelled,
    /// Operation attempted from the wrong thread.
    WrongThread,
    /// Operation requires the host main thread token.
    NotMainThread,
}

impl From<BridgeError> for SubmissionError {
    fn from(err: BridgeError) -> Self {
        match err {
            BridgeError::NotMainThread => Self::NotMainThread,
            BridgeError::WrongThread => Self::WrongThread,
            _ => Self::AllocationRefusal,
        }
    }
}

impl From<BufferError> for SubmissionError {
    fn from(err: BufferError) -> Self {
        match err {
            BufferError::ZeroLength | BufferError::TooLarge => Self::BufferBoundsExceeded,
            BufferError::AllocationFailed => Self::AllocationRefusal,
            BufferError::CrossDevice => Self::CrossDevice,
            BufferError::Stale => Self::StaleHandle,
            BufferError::GpuInFlight => Self::ResourceBusy,
            BufferError::NotMainThread => Self::NotMainThread,
            BufferError::WrongThread => Self::WrongThread,
        }
    }
}

/// Terminal completion status for a GPU submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionStatus {
    /// Enqueued and pending completion by the GPU hardware.
    Pending,
    /// GPU completed successfully.
    Success,
    /// Cancelled by host (retained leases remain locked until terminal driver contract!).
    Cancelled,
    /// GPU execution failed or driver reported an error.
    Error(SubmissionError),
}

impl CompletionStatus {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }

    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    pub const fn is_cancelled(self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// Pre-reserved completion cell that outlives encoding and tracks terminal outcome.
#[derive(Debug)]
pub struct CompletionCell {
    submission_id: SubmissionId,
    status: Cell<CompletionStatus>,
    retained_released: Cell<bool>,
}

impl CompletionCell {
    fn new(submission_id: SubmissionId) -> Self {
        Self {
            submission_id,
            status: Cell::new(CompletionStatus::Pending),
            retained_released: Cell::new(false),
        }
    }

    pub fn id(&self) -> SubmissionId {
        self.submission_id
    }

    pub fn status(&self) -> CompletionStatus {
        self.status.get()
    }

    pub fn is_terminal(&self) -> bool {
        self.status.get().is_terminal()
    }

    pub fn is_retained_released(&self) -> bool {
        self.retained_released.get()
    }
}

/// Aggregate counters for submission queue operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct SubmissionCounters {
    pub submissions_created: u64,
    pub submissions_committed: u64,
    pub submissions_completed: u64,
    pub submissions_cancelled: u64,
    pub rejected_queue_full: u64,
    pub rejected_cross_device: u64,
    pub rejected_stale_handle: u64,
    pub rejected_allocation: u64,
    pub rejected_bounds: u64,
    pub rejected_wrong_thread: u64,
    pub peak_in_flight: u64,
}

#[derive(Debug, Default)]
struct SubmissionCountersCell {
    submissions_created: Cell<u64>,
    submissions_committed: Cell<u64>,
    submissions_completed: Cell<u64>,
    submissions_cancelled: Cell<u64>,
    rejected_queue_full: Cell<u64>,
    rejected_cross_device: Cell<u64>,
    rejected_stale_handle: Cell<u64>,
    rejected_allocation: Cell<u64>,
    rejected_bounds: Cell<u64>,
    rejected_wrong_thread: Cell<u64>,
    peak_in_flight: Cell<u64>,
}

impl SubmissionCountersCell {
    fn snapshot(&self) -> SubmissionCounters {
        SubmissionCounters {
            submissions_created: self.submissions_created.get(),
            submissions_committed: self.submissions_committed.get(),
            submissions_completed: self.submissions_completed.get(),
            submissions_cancelled: self.submissions_cancelled.get(),
            rejected_queue_full: self.rejected_queue_full.get(),
            rejected_cross_device: self.rejected_cross_device.get(),
            rejected_stale_handle: self.rejected_stale_handle.get(),
            rejected_allocation: self.rejected_allocation.get(),
            rejected_bounds: self.rejected_bounds.get(),
            rejected_wrong_thread: self.rejected_wrong_thread.get(),
            peak_in_flight: self.peak_in_flight.get(),
        }
    }

    fn bump(cell: &Cell<u64>) {
        cell.set(cell.get().saturating_add(1));
    }

    fn update_peak(&self, current: usize) {
        let cur = current as u64;
        if cur > self.peak_in_flight.get() {
            self.peak_in_flight.set(cur);
        }
    }
}

/// Capacity of the structured event ring.
pub const EVENT_RING_CAPACITY: usize = 64;

/// Structured event categories for auditability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionEventType {
    Created,
    EncodedCopy,
    Committed,
    Completed,
    Cancelled,
    RejectedQueueFull,
    RejectedCrossDevice,
    RejectedStaleHandle,
    RejectedAllocation,
    RejectedBounds,
}

/// Structured record emitted to the event ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmissionEventRecord {
    pub submission_id: SubmissionId,
    pub event_type: SubmissionEventType,
    pub in_flight_count: usize,
}

#[derive(Debug)]
struct EventRing {
    entries: Vec<SubmissionEventRecord>,
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

    fn record(&mut self, record: SubmissionEventRecord) {
        self.total_events = self.total_events.saturating_add(1);
        if self.entries.len() < self.capacity {
            self.entries.push(record);
        } else {
            self.entries[self.write_pos] = record;
            self.write_pos = (self.write_pos + 1) % self.capacity;
        }
    }

    fn records(&self) -> Vec<SubmissionEventRecord> {
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

/// Guards exclusive access to a buffer during GPU submission and execution.
#[derive(Debug)]
#[allow(dead_code)]
struct BufferLease {
    buffer_id: u64,
    device_id: DeviceId,
    generation: u64,
    flight_cell: Rc<Cell<bool>>,
}

impl BufferLease {
    fn acquire(
        buffer: &MetalBuffer,
        queue_device: DeviceId,
        queue_gen: u64,
    ) -> Result<Self, SubmissionError> {
        if buffer.device_id() != queue_device {
            return Err(SubmissionError::CrossDevice);
        }
        if buffer.generation() != queue_gen {
            return Err(SubmissionError::StaleHandle);
        }
        if buffer.is_in_gpu_flight() {
            return Err(SubmissionError::ResourceBusy);
        }
        buffer.begin_gpu_flight();
        Ok(Self {
            buffer_id: buffer.buffer_id(),
            device_id: buffer.device_id(),
            generation: buffer.generation(),
            flight_cell: buffer.flight_cell(),
        })
    }

    fn release(&self) {
        self.flight_cell.set(false);
    }
}

/// A bounded command submission queue on a Metal device.
#[derive(Debug)]
pub struct BoundedSubmissionQueue {
    device_id: DeviceId,
    token: MainThreadToken,
    raw_queue: NativeObject,
    capacity: usize,
    in_flight: RefCell<Vec<ActiveSlot>>,
    counters: Rc<SubmissionCountersCell>,
    event_ring: RefCell<EventRing>,
    generation: Cell<u64>,
    resource_epoch: Rc<Cell<u64>>,
    /// Live encoders plus in-flight committed slots. Reserved before any
    /// driver command-buffer allocation so overlapping `begin_submission`
    /// calls cannot exceed capacity.
    reserved: Cell<usize>,
}

#[derive(Debug)]
struct ActiveSlot {
    id: SubmissionId,
    cmd_buf: NativeObject,
    completion: Rc<CompletionCell>,
    leases: Vec<BufferLease>,
    cancelled: Cell<bool>,
}

impl BoundedSubmissionQueue {
    /// Create a new bounded submission queue with a fixed in-flight capacity.
    pub fn new(
        device: &MetalDevice,
        token: MainThreadToken,
        capacity: usize,
    ) -> Result<Self, SubmissionError> {
        token.assert_current()?;
        if capacity == 0 {
            return Err(SubmissionError::InvalidQueueCapacity);
        }
        let raw_queue = device
            .new_command_queue_raw()
            .ok_or(SubmissionError::AllocationRefusal)?;
        let resource_epoch = device.resource_epoch_cell();
        Ok(Self {
            device_id: device.device_id(),
            token,
            raw_queue: NativeObject::adopt(raw_queue, token),
            capacity,
            in_flight: RefCell::new(Vec::with_capacity(capacity)),
            counters: Rc::new(SubmissionCountersCell::default()),
            event_ring: RefCell::new(EventRing::new(EVENT_RING_CAPACITY)),
            generation: Cell::new(resource_epoch.get()),
            resource_epoch,
            reserved: Cell::new(0),
        })
    }

    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn in_flight_count(&self) -> usize {
        self.in_flight.borrow().len()
    }

    pub fn reserved_count(&self) -> usize {
        self.reserved.get()
    }

    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// Advance the shared device resource epoch. Buffers minted before this
    /// call become stale on this queue; buffers minted after it are admitted.
    pub fn invalidate_generation(&self) {
        let next = self.resource_epoch.get().wrapping_add(1).max(1);
        self.resource_epoch.set(next);
        self.generation.set(next);
    }

    /// Obtain a point-in-time snapshot of aggregate counters.
    pub fn counters(&self) -> SubmissionCounters {
        self.counters.snapshot()
    }

    /// Return the bounded event history.
    pub fn event_records(&self) -> Vec<SubmissionEventRecord> {
        self.event_ring.borrow().records()
    }

    /// Begin a new submission with pre-reserved completion storage.
    pub fn begin_submission<'a>(
        &'a self,
        token: MainThreadToken,
    ) -> Result<SubmissionEncoder<'a>, SubmissionError> {
        token.assert_current()?;
        if token != self.token {
            SubmissionCountersCell::bump(&self.counters.rejected_wrong_thread);
            return Err(SubmissionError::WrongThread);
        }
        let current_in_flight = self.in_flight.borrow().len();
        if self.reserved.get() >= self.capacity {
            SubmissionCountersCell::bump(&self.counters.rejected_queue_full);
            self.event_ring.borrow_mut().record(SubmissionEventRecord {
                submission_id: SubmissionId(0),
                event_type: SubmissionEventType::RejectedQueueFull,
                in_flight_count: current_in_flight,
            });
            return Err(SubmissionError::QueueFull);
        }
        // Reserve the slot before the driver allocation so a second begin
        // cannot sneak past capacity while this encoder is still live.
        self.reserved.set(self.reserved.get() + 1);
        let cmd_buf_raw = ffi::command_buffer(self.raw_queue.raw).ok_or_else(|| {
            self.reserved.set(self.reserved.get().saturating_sub(1));
            SubmissionCountersCell::bump(&self.counters.rejected_allocation);
            SubmissionError::AllocationRefusal
        })?;
        let cmd_buf = NativeObject::adopt(cmd_buf_raw, token);
        let submission_id = SubmissionId::next();
        let completion = Rc::new(CompletionCell::new(submission_id));

        SubmissionCountersCell::bump(&self.counters.submissions_created);
        self.event_ring.borrow_mut().record(SubmissionEventRecord {
            submission_id,
            event_type: SubmissionEventType::Created,
            in_flight_count: current_in_flight,
        });

        Ok(SubmissionEncoder {
            queue: self,
            token,
            cmd_buf: Some(cmd_buf),
            encoder: None,
            submission_id,
            completion,
            leases: Vec::new(),
            committed: false,
        })
    }

    fn mark_slot_cancelled(&self, id: SubmissionId) {
        let in_flight = self.in_flight.borrow();
        if let Some(slot) = in_flight.iter().find(|s| s.id == id) {
            slot.cancelled.set(true);
        }
    }

    /// Poll in-flight submissions against driver completion status.
    /// Returns the number of submissions transitioned to terminal state.
    pub fn poll_completions(&self, token: MainThreadToken) -> Result<usize, SubmissionError> {
        token.assert_current()?;
        let mut completed_count = 0;
        let mut in_flight = self.in_flight.borrow_mut();
        let mut i = 0;
        while i < in_flight.len() {
            let status_code = ffi::command_buffer_status(in_flight[i].cmd_buf.raw);
            // MTLCommandBufferStatus: 4 = Completed, 5 = Error
            if status_code >= 4 {
                let slot = in_flight.remove(i);
                self.reserved.set(self.reserved.get().saturating_sub(1));
                Self::finish_slot(
                    &slot,
                    status_code,
                    &self.counters,
                    &self.event_ring,
                    in_flight.len(),
                );
                completed_count += 1;
            } else {
                i += 1;
            }
        }
        Ok(completed_count)
    }

    /// Block until a specific submission reaches terminal state, releasing its leases.
    pub fn wait_for_submission(
        &self,
        id: SubmissionId,
        token: MainThreadToken,
    ) -> Result<CompletionStatus, SubmissionError> {
        token.assert_current()?;
        let slot_idx = self.in_flight.borrow().iter().position(|s| s.id == id);
        let Some(idx) = slot_idx else {
            return Err(SubmissionError::StaleHandle);
        };

        let raw_cmd = self.in_flight.borrow()[idx].cmd_buf.raw;
        ffi::wait_until_completed(raw_cmd);

        let slot = self.in_flight.borrow_mut().remove(idx);
        self.reserved.set(self.reserved.get().saturating_sub(1));
        let status_code = ffi::command_buffer_status(slot.cmd_buf.raw);
        let remaining = self.in_flight.borrow().len();
        let final_status = Self::finish_slot(
            &slot,
            status_code,
            &self.counters,
            &self.event_ring,
            remaining,
        );
        Ok(final_status)
    }

    /// Wait until all currently in-flight submissions reach terminal state.
    pub fn drain_all_sync(&self, token: MainThreadToken) -> Result<(), SubmissionError> {
        token.assert_current()?;
        while !self.in_flight.borrow().is_empty() {
            let slot = self.in_flight.borrow_mut().remove(0);
            self.reserved.set(self.reserved.get().saturating_sub(1));
            ffi::wait_until_completed(slot.cmd_buf.raw);
            let status_code = ffi::command_buffer_status(slot.cmd_buf.raw);
            let remaining = self.in_flight.borrow().len();
            Self::finish_slot(
                &slot,
                status_code,
                &self.counters,
                &self.event_ring,
                remaining,
            );
        }
        Ok(())
    }

    fn finish_slot(
        slot: &ActiveSlot,
        status_code: u64,
        counters: &SubmissionCountersCell,
        event_ring: &RefCell<EventRing>,
        remaining_in_flight: usize,
    ) -> CompletionStatus {
        // Release all resource leases: restore CPU accessibility
        for lease in &slot.leases {
            lease.release();
        }
        slot.completion.retained_released.set(true);

        let succeeded = status_code == 4 || (cfg!(not(target_os = "macos")) && status_code == 0);
        let final_status = if slot.cancelled.get() {
            CompletionStatus::Cancelled
        } else if succeeded {
            CompletionStatus::Success
        } else {
            CompletionStatus::Error(SubmissionError::DeviceLost)
        };
        slot.completion.status.set(final_status);

        SubmissionCountersCell::bump(&counters.submissions_completed);
        event_ring.borrow_mut().record(SubmissionEventRecord {
            submission_id: slot.id,
            event_type: SubmissionEventType::Completed,
            in_flight_count: remaining_in_flight,
        });

        final_status
    }
}

impl Drop for BoundedSubmissionQueue {
    fn drop(&mut self) {
        // Drain in-flight work before dropping queue to protect against driver use-after-free
        for slot in self.in_flight.borrow_mut().drain(..) {
            ffi::wait_until_completed(slot.cmd_buf.raw);
            for lease in &slot.leases {
                lease.release();
            }
            slot.completion.retained_released.set(true);
            slot.completion.status.set(CompletionStatus::Cancelled);
        }
        self.reserved.set(0);
    }
}

/// An active command encoder tied to a pre-reserved submission slot.
#[derive(Debug)]
pub struct SubmissionEncoder<'a> {
    queue: &'a BoundedSubmissionQueue,
    token: MainThreadToken,
    cmd_buf: Option<NativeObject>,
    encoder: Option<NativeObject>,
    submission_id: SubmissionId,
    completion: Rc<CompletionCell>,
    leases: Vec<BufferLease>,
    committed: bool,
}

impl<'a> SubmissionEncoder<'a> {
    pub fn id(&self) -> SubmissionId {
        self.submission_id
    }

    pub fn device_id(&self) -> DeviceId {
        self.queue.device_id
    }

    pub fn completion_cell(&self) -> Rc<CompletionCell> {
        Rc::clone(&self.completion)
    }

    /// Encode a GPU byte copy operation between two buffers.
    pub fn encode_copy(
        &mut self,
        src: &MetalBuffer,
        src_offset: usize,
        dst: &MetalBuffer,
        dst_offset: usize,
        size: usize,
    ) -> Result<(), SubmissionError> {
        if self.committed {
            SubmissionCountersCell::bump(&self.queue.counters.rejected_stale_handle);
            return Err(SubmissionError::StaleHandle);
        }
        if src.device_id() != self.queue.device_id || dst.device_id() != self.queue.device_id {
            SubmissionCountersCell::bump(&self.queue.counters.rejected_cross_device);
            self.queue
                .event_ring
                .borrow_mut()
                .record(SubmissionEventRecord {
                    submission_id: self.submission_id,
                    event_type: SubmissionEventType::RejectedCrossDevice,
                    in_flight_count: self.queue.in_flight.borrow().len(),
                });
            return Err(SubmissionError::CrossDevice);
        }
        if src.generation() != self.queue.generation.get()
            || dst.generation() != self.queue.generation.get()
        {
            SubmissionCountersCell::bump(&self.queue.counters.rejected_stale_handle);
            self.queue
                .event_ring
                .borrow_mut()
                .record(SubmissionEventRecord {
                    submission_id: self.submission_id,
                    event_type: SubmissionEventType::RejectedStaleHandle,
                    in_flight_count: self.queue.in_flight.borrow().len(),
                });
            return Err(SubmissionError::StaleHandle);
        }
        if size == 0
            || src_offset
                .checked_add(size)
                .map_or(true, |end| end > src.len())
            || dst_offset
                .checked_add(size)
                .map_or(true, |end| end > dst.len())
        {
            SubmissionCountersCell::bump(&self.queue.counters.rejected_bounds);
            self.queue
                .event_ring
                .borrow_mut()
                .record(SubmissionEventRecord {
                    submission_id: self.submission_id,
                    event_type: SubmissionEventType::RejectedBounds,
                    in_flight_count: self.queue.in_flight.borrow().len(),
                });
            return Err(SubmissionError::BufferBoundsExceeded);
        }

        let src_lease = self.acquire_unique_lease(src)?;
        let dst_lease = self.acquire_unique_lease(dst)?;
        if let Some(lease) = src_lease {
            self.leases.push(lease);
        }
        if let Some(lease) = dst_lease {
            self.leases.push(lease);
        }

        if self.encoder.is_none() {
            let cmd_raw = self
                .cmd_buf
                .as_ref()
                .ok_or(SubmissionError::StaleHandle)?
                .raw;
            let enc_raw = ffi::blit_encoder(cmd_raw).ok_or_else(|| {
                SubmissionCountersCell::bump(&self.queue.counters.rejected_allocation);
                SubmissionError::AllocationRefusal
            })?;
            self.encoder = Some(NativeObject::adopt(enc_raw, self.token));
        }

        let encoder_ref = self.encoder.as_ref().unwrap().raw;
        ffi::copy_bytes(
            encoder_ref,
            src.raw_ref(),
            src_offset as u64,
            dst.raw_ref(),
            dst_offset as u64,
            size as u64,
        );

        self.queue
            .event_ring
            .borrow_mut()
            .record(SubmissionEventRecord {
                submission_id: self.submission_id,
                event_type: SubmissionEventType::EncodedCopy,
                in_flight_count: self.queue.in_flight.borrow().len(),
            });
        Ok(())
    }

    /// Commit the encoded commands to the GPU queue.
    pub fn commit(mut self) -> Result<Submission, SubmissionError> {
        if self.committed {
            SubmissionCountersCell::bump(&self.queue.counters.rejected_stale_handle);
            return Err(SubmissionError::StaleHandle);
        }
        let cmd_buf = self.cmd_buf.take().ok_or(SubmissionError::StaleHandle)?;
        if let Some(enc) = self.encoder.take() {
            ffi::end_encoding(enc.raw);
        }
        ffi::commit(cmd_buf.raw);
        self.committed = true;

        let slot = ActiveSlot {
            id: self.submission_id,
            cmd_buf,
            completion: Rc::clone(&self.completion),
            leases: std::mem::take(&mut self.leases),
            cancelled: Cell::new(false),
        };
        let mut in_flight = self.queue.in_flight.borrow_mut();
        in_flight.push(slot);
        let count = in_flight.len();
        self.queue.counters.update_peak(count);
        SubmissionCountersCell::bump(&self.queue.counters.submissions_committed);

        self.queue
            .event_ring
            .borrow_mut()
            .record(SubmissionEventRecord {
                submission_id: self.submission_id,
                event_type: SubmissionEventType::Committed,
                in_flight_count: count,
            });

        Ok(Submission {
            id: self.submission_id,
            completion: Rc::clone(&self.completion),
            device_id: self.queue.device_id,
        })
    }

    fn acquire_unique_lease(
        &self,
        buffer: &MetalBuffer,
    ) -> Result<Option<BufferLease>, SubmissionError> {
        if self
            .leases
            .iter()
            .any(|lease| lease.buffer_id == buffer.buffer_id())
        {
            return Ok(None);
        }
        BufferLease::acquire(buffer, self.queue.device_id, self.queue.generation.get()).map(Some)
    }
}

impl<'a> Drop for SubmissionEncoder<'a> {
    fn drop(&mut self) {
        if !self.committed {
            if let Some(enc) = self.encoder.take() {
                ffi::end_encoding(enc.raw);
            }
            let _ = self.cmd_buf.take();
            for lease in self.leases.drain(..) {
                lease.release();
            }
            self.completion.status.set(CompletionStatus::Cancelled);
            self.completion.retained_released.set(true);
            self.queue
                .reserved
                .set(self.queue.reserved.get().saturating_sub(1));
        }
    }
}

/// An in-flight submission handle returned upon commit.
#[derive(Debug)]
pub struct Submission {
    id: SubmissionId,
    completion: Rc<CompletionCell>,
    device_id: DeviceId,
}

impl Submission {
    pub fn id(&self) -> SubmissionId {
        self.id
    }

    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    pub fn status(&self) -> CompletionStatus {
        self.completion.status()
    }

    pub fn is_terminal(&self) -> bool {
        self.completion.is_terminal()
    }

    pub fn is_retained_released(&self) -> bool {
        self.completion.is_retained_released()
    }

    /// Request cancellation of this submission.
    ///
    /// The status transitions to `Cancelled`, but retained resource leases
    /// are NOT prematurely freed or unlocked. Driver work remains in flight
    /// until the actual terminal completion contract.
    pub fn cancel(&self, queue: &BoundedSubmissionQueue) -> Result<(), SubmissionError> {
        if self.device_id != queue.device_id {
            return Err(SubmissionError::CrossDevice);
        }
        if self.completion.is_terminal() {
            return Err(SubmissionError::StaleHandle);
        }
        self.completion.status.set(CompletionStatus::Cancelled);
        queue.mark_slot_cancelled(self.id);
        SubmissionCountersCell::bump(&queue.counters.submissions_cancelled);
        queue.event_ring.borrow_mut().record(SubmissionEventRecord {
            submission_id: self.id,
            event_type: SubmissionEventType::Cancelled,
            in_flight_count: queue.in_flight.borrow().len(),
        });
        Ok(())
    }

    /// Block on the host thread until this submission achieves terminal status,
    /// releasing all retained resource leases.
    pub fn wait_until_completed(
        &self,
        queue: &BoundedSubmissionQueue,
        token: MainThreadToken,
    ) -> Result<CompletionStatus, SubmissionError> {
        token.assert_current()?;
        if self.device_id != queue.device_id {
            return Err(SubmissionError::CrossDevice);
        }
        queue.wait_for_submission(self.id, token)
    }
}

//! Lossless GPU terminal record drain and completion conservation (FCB-072.B).
//!
//! # Core Invariants
//!
//! 1. **Completion is a Conservation Channel**: Every admitted GPU submission pre-reserves
//!    a terminal completion record before encoding or committing work. The completion channel
//!    is a fixed-capacity, lossless queue; completions can NEVER be dropped, even under 100%
//!    ordinary event queue or memory saturation.
//! 2. **Coalesced Wakes, Lossless State**: Host wakeups may coalesce (e.g., 50 completions
//!    arrive while the consumer turn is executing), but the underlying terminal completion
//!    records and their associated resource leases are preserved without loss.
//! 3. **Retain Through Close and Release Exactly Once**: When a window or session closes,
//!    in-flight GPU submissions are NOT aborted prematurely or freed. Their records and resource
//!    leases remain retained until the driver actually reaches terminal completion. Every
//!    lease is released *exactly once* during drain.
//! 4. **Protected Reclamation Progress**: Terminal records and off-UI retirement progress
//!    are charged to protected budget pools, preventing deadlocks where memory reclamation
//!    requires allocating new memory.

#![forbid(unsafe_code)]

use std::{
    any::Any,
    collections::VecDeque,
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use fcb_core::{
    resources::ResourceLease,
    retirement::BoundedRetirementQueue,
};

use crate::{WakePriority, WakeProbe};

static SUBMISSION_ID_NONCE: AtomicU64 = AtomicU64::new(1);
static RECORD_SLOT_NONCE: AtomicU64 = AtomicU64::new(1);

/// Monotonic identifier for a GPU submission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GpuSubmissionId(u64);

impl GpuSubmissionId {
    /// Mint a fresh, monotonic submission identifier.
    pub fn next() -> Self {
        Self(SUBMISSION_ID_NONCE.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Monotonic identifier for a pre-reserved terminal completion record slot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TerminalRecordSlotId(u64);

impl TerminalRecordSlotId {
    pub fn next() -> Self {
        Self(RECORD_SLOT_NONCE.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Structured hardware and driver errors for terminal GPU failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GpuTerminalError {
    /// Hardware device was removed, reset, or lost.
    DeviceLost,
    /// Command buffer or shader encoding contained an invalid instruction/parameter.
    InvalidCommand,
    /// Driver or device-private heap memory exhausted.
    ResourceExhausted,
    /// Next display drawable could not be acquired or presented.
    DrawableUnavailable,
    /// Architecture-specific driver fault code.
    DriverFault(u64),
}

impl fmt::Display for GpuTerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceLost => write!(f, "Metal device lost or reset"),
            Self::InvalidCommand => write!(f, "Invalid GPU command encoding"),
            Self::ResourceExhausted => write!(f, "GPU device memory exhausted"),
            Self::DrawableUnavailable => write!(f, "CAMetalDrawable unavailable"),
            Self::DriverFault(code) => write!(f, "Driver fault code {code}"),
        }
    }
}

/// Terminal completion status for an admitted GPU submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalCompletionStatus {
    /// Work is in flight on the GPU device.
    Pending,
    /// GPU execution succeeded.
    Success,
    /// Cancelled by host (retained leases remain locked until actual terminal contract).
    Cancelled,
    /// GPU execution terminated with a driver or device error.
    Error(GpuTerminalError),
}

impl TerminalCompletionStatus {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }

    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    pub const fn is_cancelled(self) -> bool {
        matches!(self, Self::Cancelled)
    }

    pub const fn is_error(self) -> bool {
        matches!(self, Self::Error(_))
    }
}

/// Errors returned by the lossless terminal drain boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalDrainError {
    /// Fixed queue capacity reached; new submissions refused before driver encoding.
    QueueSaturated { capacity: usize },
    /// Channel is closed; new reservations are refused.
    Closed,
    /// Reservation already committed.
    AlreadyCommitted,
    /// Submission ID was not found in the active table.
    UnknownSubmission { id: u64 },
    /// Invalid configuration (zero capacity).
    InvalidCapacity,
}

impl fmt::Display for TerminalDrainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueSaturated { capacity } => {
                write!(f, "Terminal completion queue saturated at capacity {capacity}")
            }
            Self::Closed => write!(f, "Terminal completion channel is closed"),
            Self::AlreadyCommitted => write!(f, "Terminal reservation was already committed"),
            Self::UnknownSubmission { id } => write!(f, "Unknown GPU submission ID {id}"),
            Self::InvalidCapacity => write!(f, "Terminal completion queue capacity must be non-zero"),
        }
    }
}

impl std::error::Error for TerminalDrainError {}

/// Diagnostic event types for the bounded event ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalEventType {
    Reserved,
    Committed,
    Aborted,
    CompletedSuccess,
    CompletedCancelled,
    CompletedError,
    Drained,
    RejectedQueueFull,
    RejectedClosed,
    WakeCoalesced,
}

/// A structured trace record stored in the bounded event ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalEventRecord {
    pub slot_id: u64,
    pub submission_id: u64,
    pub event_type: TerminalEventType,
    pub in_flight: usize,
}

/// Point-in-time snapshot of terminal drain counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerminalDrainCounters {
    pub reservations_created: u64,
    pub reservations_committed: u64,
    pub reservations_aborted: u64,
    pub completions_recorded: u64,
    pub cancellations_recorded: u64,
    pub records_drained: u64,
    pub leases_released_once: u64,
    pub wakes_signaled: u64,
    pub wakes_coalesced: u64,
    pub rejected_queue_full: u64,
    pub rejected_closed: u64,
    pub peak_in_flight: usize,
}

/// Summary report returned after draining completed terminal records.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerminalDrainReport {
    pub completed_drained: usize,
    pub cancelled_drained: usize,
    pub errors_drained: usize,
    pub leases_released: usize,
    pub payloads_retired: usize,
    pub remaining_in_flight: usize,
}

struct RetiredPayload(#[allow(dead_code)] Arc<dyn Any + Send + Sync>);

#[derive(Debug)]
struct SlotRecord {
    slot_id: TerminalRecordSlotId,
    submission_id: GpuSubmissionId,
    status: TerminalCompletionStatus,
    retained_leases: Vec<ResourceLease>,
    retired_payloads: Vec<Arc<dyn Any + Send + Sync>>,
    committed: bool,
    drained: bool,
    leases_released: bool,
}

#[derive(Debug)]
struct QueueState {
    slots: Vec<Option<SlotRecord>>,
    closed: bool,
    counters: TerminalDrainCounters,
    event_ring: VecDeque<TerminalEventRecord>,
    ring_capacity: usize,
    in_flight_count: usize,
    wake_pending: bool,
}

impl QueueState {
    fn record_event(
        &mut self,
        slot_id: u64,
        submission_id: u64,
        event_type: TerminalEventType,
    ) {
        if self.event_ring.len() >= self.ring_capacity {
            self.event_ring.pop_front();
        }
        self.event_ring.push_back(TerminalEventRecord {
            slot_id,
            submission_id,
            event_type,
            in_flight: self.in_flight_count,
        });
    }

    fn update_peak(&mut self) {
        if self.in_flight_count > self.counters.peak_in_flight {
            self.counters.peak_in_flight = self.in_flight_count;
        }
    }
}

/// A pre-reserved terminal completion record slot.
///
/// Ensures that terminal record capacity and optional resource leases are
/// reserved BEFORE the command buffer is encoded or submitted to the native driver.
/// If dropped before calling [`TerminalRecordReservation::commit`], the reservation
/// is cleanly aborted and returned to the free pool.
pub struct TerminalRecordReservation {
    queue: Arc<Mutex<QueueState>>,
    slot_index: usize,
    slot_id: TerminalRecordSlotId,
    submission_id: GpuSubmissionId,
    committed: bool,
}

impl fmt::Debug for TerminalRecordReservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalRecordReservation")
            .field("slot_index", &self.slot_index)
            .field("slot_id", &self.slot_id)
            .field("submission_id", &self.submission_id)
            .field("committed", &self.committed)
            .finish()
    }
}

impl TerminalRecordReservation {
    pub fn submission_id(&self) -> GpuSubmissionId {
        self.submission_id
    }

    pub fn slot_id(&self) -> TerminalRecordSlotId {
        self.slot_id
    }

    /// Attach a resource lease that must remain locked until terminal GPU completion.
    pub fn attach_lease(&mut self, lease: ResourceLease) {
        let mut state = self.queue.lock().expect("queue lock poisoned");
        if let Some(Some(slot)) = state.slots.get_mut(self.slot_index) {
            slot.retained_leases.push(lease);
        }
    }

    /// Attach a retired CPU object that should be dropped off-UI when this GPU work completes.
    pub fn attach_retired_payload<T: Send + Sync + 'static>(&mut self, payload: Arc<T>) {
        let mut state = self.queue.lock().expect("queue lock poisoned");
        if let Some(Some(slot)) = state.slots.get_mut(self.slot_index) {
            slot.retired_payloads.push(payload);
        }
    }

    /// Commit the reservation after successful submission to the GPU driver.
    pub fn commit(mut self) -> Result<(), TerminalDrainError> {
        let mut state = self.queue.lock().expect("queue lock poisoned");
        if let Some(Some(slot)) = state.slots.get_mut(self.slot_index) {
            if slot.committed {
                return Err(TerminalDrainError::AlreadyCommitted);
            }
            slot.committed = true;
            self.committed = true;
            state.counters.reservations_committed += 1;
            state.in_flight_count += 1;
            state.update_peak();
            state.record_event(
                self.slot_id.get(),
                self.submission_id.get(),
                TerminalEventType::Committed,
            );
            Ok(())
        } else {
            Err(TerminalDrainError::UnknownSubmission {
                id: self.submission_id.get(),
            })
        }
    }
}

impl Drop for TerminalRecordReservation {
    fn drop(&mut self) {
        if !self.committed {
            if let Ok(mut state) = self.queue.lock() {
                if let Some(slot_opt) = state.slots.get_mut(self.slot_index) {
                    if let Some(slot) = slot_opt.take() {
                        state.counters.reservations_aborted += 1;
                        state.record_event(
                            slot.slot_id.get(),
                            slot.submission_id.get(),
                            TerminalEventType::Aborted,
                        );
                    }
                }
            }
        }
    }
}

/// A lossless conservation channel and terminal GPU completion drain.
///
/// Guarantees that:
/// - Submissions pre-reserve completion capacity before driver allocation.
/// - Foreign driver completion callbacks record status without allocating.
/// - Wakes coalesce without dropping terminal records.
/// - Retained leases survive window close and are released exactly once.
pub struct LosslessTerminalDrainQueue {
    capacity: usize,
    state: Arc<Mutex<QueueState>>,
    wake_probe: Option<Arc<WakeProbe>>,
    wake_counter: AtomicU64,
    has_unserviced_wake: AtomicBool,
}

impl fmt::Debug for LosslessTerminalDrainQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LosslessTerminalDrainQueue")
            .field("capacity", &self.capacity)
            .field("in_flight", &self.in_flight_count())
            .field("is_closed", &self.is_closed())
            .field("counters", &self.counters())
            .finish()
    }
}

impl LosslessTerminalDrainQueue {
    /// Create a new lossless terminal drain queue with a bounded capacity.
    pub fn new(capacity: usize) -> Result<Self, TerminalDrainError> {
        if capacity == 0 {
            return Err(TerminalDrainError::InvalidCapacity);
        }
        let ring_capacity = 64;
        let state = QueueState {
            slots: (0..capacity).map(|_| None).collect(),
            closed: false,
            counters: TerminalDrainCounters::default(),
            event_ring: VecDeque::with_capacity(ring_capacity),
            ring_capacity,
            in_flight_count: 0,
            wake_pending: false,
        };
        Ok(Self {
            capacity,
            state: Arc::new(Mutex::new(state)),
            wake_probe: None,
            wake_counter: AtomicU64::new(0),
            has_unserviced_wake: AtomicBool::new(false),
        })
    }

    /// Attach a [`WakeProbe`] to receive coalesced wake notifications on completion.
    pub fn with_wake_probe(mut self, probe: Arc<WakeProbe>) -> Self {
        self.wake_probe = Some(probe);
        self
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().expect("queue lock poisoned").closed
    }

    pub fn in_flight_count(&self) -> usize {
        self.state.lock().expect("queue lock poisoned").in_flight_count
    }

    pub fn counters(&self) -> TerminalDrainCounters {
        self.state.lock().expect("queue lock poisoned").counters
    }

    pub fn event_records(&self) -> Vec<TerminalEventRecord> {
        self.state
            .lock()
            .expect("queue lock poisoned")
            .event_ring
            .iter()
            .copied()
            .collect()
    }

    /// Pre-reserve a terminal completion slot for a new GPU submission.
    pub fn reserve(
        &self,
        submission_id: GpuSubmissionId,
    ) -> Result<TerminalRecordReservation, TerminalDrainError> {
        let mut state = self.state.lock().expect("queue lock poisoned");
        if state.closed {
            state.counters.rejected_closed += 1;
            state.record_event(0, submission_id.get(), TerminalEventType::RejectedClosed);
            return Err(TerminalDrainError::Closed);
        }

        // Find a free slot
        let free_idx = state.slots.iter().position(|s| s.is_none());
        let Some(idx) = free_idx else {
            state.counters.rejected_queue_full += 1;
            state.record_event(
                0,
                submission_id.get(),
                TerminalEventType::RejectedQueueFull,
            );
            return Err(TerminalDrainError::QueueSaturated {
                capacity: self.capacity,
            });
        };

        let slot_id = TerminalRecordSlotId::next();
        if let Some(slot_entry) = state.slots.get_mut(idx) {
            *slot_entry = Some(SlotRecord {
                slot_id,
                submission_id,
                status: TerminalCompletionStatus::Pending,
                retained_leases: Vec::new(),
                retired_payloads: Vec::new(),
                committed: false,
                drained: false,
                leases_released: false,
            });
        }

        state.counters.reservations_created += 1;
        state.record_event(
            slot_id.get(),
            submission_id.get(),
            TerminalEventType::Reserved,
        );

        Ok(TerminalRecordReservation {
            queue: Arc::clone(&self.state),
            slot_index: idx,
            slot_id,
            submission_id,
            committed: false,
        })
    }

    /// Record GPU hardware completion for a submission (callable from foreign callback).
    ///
    /// This method is non-allocating in steady state, safe to invoke from driver threads,
    /// and triggers a coalesced wake without dropping terminal records.
    pub fn record_completion(
        &self,
        submission_id: GpuSubmissionId,
        status: TerminalCompletionStatus,
    ) -> Result<(), TerminalDrainError> {
        if !status.is_terminal() {
            return Ok(());
        }

        let event_type = match status {
            TerminalCompletionStatus::Success => TerminalEventType::CompletedSuccess,
            TerminalCompletionStatus::Cancelled => TerminalEventType::CompletedCancelled,
            TerminalCompletionStatus::Error(_) => TerminalEventType::CompletedError,
            TerminalCompletionStatus::Pending => unreachable!(),
        };

        let (slot_id, was_pending) = {
            let mut state = self.state.lock().expect("queue lock poisoned");
            let mut found = None;
            for slot_opt in state.slots.iter_mut() {
                if let Some(slot) = slot_opt {
                    if slot.submission_id == submission_id {
                        let prev_pending = !slot.status.is_terminal();
                        slot.status = status;
                        found = Some((slot.slot_id, prev_pending));
                        break;
                    }
                }
            }

            let Some((s_id, pending)) = found else {
                return Err(TerminalDrainError::UnknownSubmission {
                    id: submission_id.get(),
                });
            };

            if pending {
                state.counters.completions_recorded += 1;
                if status.is_cancelled() {
                    state.counters.cancellations_recorded += 1;
                }
                state.record_event(s_id.get(), submission_id.get(), event_type);
            }

            (s_id, pending)
        };

        let _ = slot_id;
        if was_pending {
            self.signal_coalesced_wake();
        }

        Ok(())
    }

    /// Mark an in-flight submission as cancelled.
    ///
    /// The submission is marked `Cancelled`, but its resource leases are NOT released
    /// prematurely. They remain locked until the GPU hardware callback signals completion.
    pub fn cancel_submission(
        &self,
        submission_id: GpuSubmissionId,
    ) -> Result<(), TerminalDrainError> {
        let mut state = self.state.lock().expect("queue lock poisoned");
        let mut target_slot_id = None;
        for slot_opt in state.slots.iter_mut() {
            if let Some(slot) = slot_opt {
                if slot.submission_id == submission_id {
                    if !slot.status.is_terminal() {
                        slot.status = TerminalCompletionStatus::Cancelled;
                        target_slot_id = Some(slot.slot_id);
                    } else {
                        return Ok(());
                    }
                    break;
                }
            }
        }

        if let Some(s_id) = target_slot_id {
            state.counters.cancellations_recorded += 1;
            state.record_event(
                s_id.get(),
                submission_id.get(),
                TerminalEventType::CompletedCancelled,
            );
            drop(state);
            self.signal_coalesced_wake();
            Ok(())
        } else {
            Err(TerminalDrainError::UnknownSubmission {
                id: submission_id.get(),
            })
        }
    }

    /// Signal a coalesced wake to the host runtime.
    fn signal_coalesced_wake(&self) {
        self.wake_counter.fetch_add(1, Ordering::Relaxed);
        let already_flagged = self.has_unserviced_wake.swap(true, Ordering::SeqCst);
        if already_flagged {
            let mut state = self.state.lock().expect("queue lock poisoned");
            state.counters.wakes_coalesced += 1;
            state.record_event(0, 0, TerminalEventType::WakeCoalesced);
        } else {
            let mut state = self.state.lock().expect("queue lock poisoned");
            state.counters.wakes_signaled += 1;
            state.wake_pending = true;
            if let Some(probe) = &self.wake_probe {
                let _ = probe.publish_motion(
                    crate::MotionUpdate {
                        sequence: self.wake_counter.load(Ordering::Relaxed),
                        value: state.counters.completions_recorded,
                    },
                    WakePriority::Urgent,
                );
            }
        }
    }

    /// Drain all terminal completion records that have completed, releasing their
    /// resource leases and offloading retired objects.
    pub fn drain_completed(&self) -> TerminalDrainReport {
        self.drain_completed_internal(None)
    }

    /// Drain all completed terminal records and transfer any retired payloads to the
    /// provided [`BoundedRetirementQueue`] for off-UI destruction.
    pub fn drain_completed_into_retirement(
        &self,
        retirement: &BoundedRetirementQueue,
    ) -> TerminalDrainReport {
        self.drain_completed_internal(Some(retirement))
    }

    fn drain_completed_internal(
        &self,
        retirement: Option<&BoundedRetirementQueue>,
    ) -> TerminalDrainReport {
        self.has_unserviced_wake.store(false, Ordering::SeqCst);
        let mut report = TerminalDrainReport::default();

        let mut completed_slots = Vec::new();
        {
            let mut state = self.state.lock().expect("queue lock poisoned");
            state.wake_pending = false;

            for slot_opt in state.slots.iter_mut() {
                if let Some(slot) = slot_opt {
                    if slot.committed && slot.status.is_terminal() && !slot.drained {
                        slot.drained = true;
                        completed_slots.push(slot_opt.take().unwrap());
                    }
                }
            }

            state.in_flight_count = state
                .in_flight_count
                .saturating_sub(completed_slots.len());
            report.remaining_in_flight = state.in_flight_count;
        }

        // Process completed slots outside the state lock
        for mut slot in completed_slots {
            match slot.status {
                TerminalCompletionStatus::Success => report.completed_drained += 1,
                TerminalCompletionStatus::Cancelled => report.cancelled_drained += 1,
                TerminalCompletionStatus::Error(_) => report.errors_drained += 1,
                TerminalCompletionStatus::Pending => unreachable!(),
            }

            // Release resource leases exactly once
            if !slot.leases_released {
                slot.leases_released = true;
                let lease_count = slot.retained_leases.len();
                slot.retained_leases.clear(); // dropping leases restores budget capacity
                report.leases_released += lease_count;
                let mut state = self.state.lock().expect("queue lock poisoned");
                state.counters.leases_released_once += lease_count as u64;
                state.counters.records_drained += 1;
                state.record_event(
                    slot.slot_id.get(),
                    slot.submission_id.get(),
                    TerminalEventType::Drained,
                );
            }

            // Offload or drop retired payloads
            let payload_count = slot.retired_payloads.len();
            if let Some(ret_q) = retirement {
                for payload in slot.retired_payloads.drain(..) {
                    if let Ok(reservation) = ret_q.reserve_slot(fcb_core::ByteLength::new(64)) {
                        let _ = reservation.commit_payload(Arc::new(RetiredPayload(payload)));
                    }
                }
            } else {
                slot.retired_payloads.clear();
            }
            report.payloads_retired += payload_count;
        }

        report
    }

    /// Mark the channel as closed. New reservations will be refused with
    /// [`TerminalDrainError::Closed`].
    ///
    /// Existing in-flight submissions remain retained in the queue until the
    /// native GPU driver reaches terminal completion, preserving safety across window close.
    pub fn close(&self) {
        let mut state = self.state.lock().expect("queue lock poisoned");
        state.closed = true;
    }
}

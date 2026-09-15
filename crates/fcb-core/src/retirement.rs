//! Bounded off-UI CPU retirement and publication swap guards (FCB-072.A).
//!
//! # Core Invariants
//!
//! 1. **Destruction is Work**: Replacing the last `Arc` on an interaction or event thread
//!    can recursively deallocate large document trees, source partitions, or indices.
//!    Publication therefore reserves a bounded retirement slot before swapping.
//! 2. **Protected Progress**: Ordinary memory or queue saturation cannot starve or
//!    block retirement capacity. Retirement uses protected budget allocation.
//! 3. **Bounded Capacity**: The retirement queue has a fixed capacity. If saturated,
//!    optional publications are deferred (`PublicationOutcome::Deferred`) rather than
//!    growing an unbounded queue or stalling the interaction thread.
//! 4. **Retained Bytes Charged Until Drop**: The byte size of the retired artifact remains
//!    charged to the budget while enqueued, and is credited only upon actual off-thread destruction.

#![forbid(unsafe_code)]

use std::{
    any::Any,
    collections::VecDeque,
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use crate::{
    resources::{
        ResourceAcquisitionOrder, ResourceAcquisitionSequence, ResourceAdmissionError,
        ResourceAllocationId, ResourceBudget, ResourceKind, ResourceLease,
        ResourceReservationClass,
    },
    ArenaOwnerId, ByteLength, CoreError,
};

static RETIREMENT_SLOT_NONCE: AtomicU64 = AtomicU64::new(1);

/// Monotonic identifier for a pre-reserved retirement slot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RetirementSlotId(u64);

impl RetirementSlotId {
    /// Mint a fresh, monotonic slot identifier.
    pub fn next() -> Self {
        Self(RETIREMENT_SLOT_NONCE.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Refusal reasons from the bounded retirement subsystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetirementError {
    /// The retirement queue has no free slots available.
    QueueSaturated { capacity: usize },
    /// Zero-byte retirement reservations are invalid.
    InvalidBytes,
    /// The reservation slot was already consumed by a prior swap.
    SlotAlreadyConsumed,
    /// Resource budget denied the retirement reservation.
    BudgetDenied(ResourceAdmissionError),
    /// Mutex was poisoned by a previous panic.
    Poisoned,
}

impl fmt::Display for RetirementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueSaturated { capacity } => {
                write!(formatter, "retirement queue saturated at capacity {capacity}")
            }
            Self::InvalidBytes => formatter.write_str("invalid zero-byte retirement reservation"),
            Self::SlotAlreadyConsumed => formatter.write_str("retirement slot already consumed"),
            Self::BudgetDenied(err) => write!(formatter, "budget denied retirement: {err}"),
            Self::Poisoned => formatter.write_str("retirement queue mutex poisoned"),
        }
    }
}

impl std::error::Error for RetirementError {}

impl From<ResourceAdmissionError> for RetirementError {
    fn from(err: ResourceAdmissionError) -> Self {
        Self::BudgetDenied(err)
    }
}

/// Result of attempting an off-UI publication swap.
#[derive(Debug)]
pub enum PublicationOutcome<T> {
    /// Publication succeeded; previous artifact was successfully enqueued for off-UI retirement.
    Published {
        slot_id: RetirementSlotId,
        retired_bytes: ByteLength,
    },
    /// Retirement queue was saturated; publication was deferred, retaining current active state.
    Deferred {
        attempted: Arc<T>,
        reason: RetirementError,
    },
}

impl<T> PublicationOutcome<T> {
    pub fn is_published(&self) -> bool {
        matches!(self, Self::Published { .. })
    }

    pub fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred { .. })
    }
}

/// A pre-reserved retirement slot acquired before performing an active publication.
#[derive(Debug)]
pub struct RetirementReservation {
    slot_id: RetirementSlotId,
    bytes: ByteLength,
    lease: Option<ResourceLease>,
    queue: Arc<RetirementQueueInner>,
    consumed: bool,
}

impl RetirementReservation {
    pub const fn slot_id(&self) -> RetirementSlotId {
        self.slot_id
    }

    pub const fn reserved_bytes(&self) -> ByteLength {
        self.bytes
    }

    /// Commit the publication swap on the interaction thread in O(1) time.
    ///
    /// Swaps `*active` with `new_value`, moving the old `Arc<T>` into the pre-reserved
    /// retirement queue without dropping its last reference on the calling thread.
    pub fn commit_swap<T: 'static + Send + Sync>(
        self,
        active: &mut Arc<T>,
        new_value: Arc<T>,
    ) -> Result<(), RetirementError> {
        if self.consumed {
            return Err(RetirementError::SlotAlreadyConsumed);
        }
        let old = std::mem::replace(active, new_value);
        self.commit_payload(old)
    }

    /// Hand an arbitrary payload to the retirement queue for off-thread destruction.
    pub fn commit_payload<T: 'static + Send + Sync>(
        mut self,
        payload: Arc<T>,
    ) -> Result<(), RetirementError> {
        if self.consumed {
            return Err(RetirementError::SlotAlreadyConsumed);
        }
        self.consumed = true;
        let item = RetiredItem {
            slot_id: self.slot_id,
            bytes: self.bytes,
            lease: self.lease.take(),
            _payload: payload,
        };
        self.queue.push_item(item)
    }
}

impl Drop for RetirementReservation {
    fn drop(&mut self) {
        if !self.consumed {
            // Reservation was abandoned before commit; cancel slot reservation
            self.queue.cancel_reservation(self.bytes, self.lease.take());
        }
    }
}

struct RetiredItem {
    slot_id: RetirementSlotId,
    bytes: ByteLength,
    #[allow(dead_code)]
    lease: Option<ResourceLease>,
    _payload: Arc<dyn Any + Send + Sync>,
}

impl fmt::Debug for RetiredItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetiredItem")
            .field("slot_id", &self.slot_id)
            .field("bytes", &self.bytes)
            .finish()
    }
}

#[derive(Debug, Default)]
struct RetirementCounters {
    accepted_reservations: AtomicU64,
    rejected_saturation: AtomicU64,
    items_destroyed: AtomicU64,
    bytes_reclaimed: AtomicU64,
}

#[derive(Debug)]
struct RetirementState {
    reserved_slots: usize,
    items: VecDeque<RetiredItem>,
    retained_bytes: u64,
}

#[derive(Debug)]
struct RetirementQueueInner {
    capacity: usize,
    owner: ArenaOwnerId,
    budget: Option<ResourceBudget>,
    counters: RetirementCounters,
    state: Mutex<RetirementState>,
}

impl RetirementQueueInner {
    fn push_item(&self, item: RetiredItem) -> Result<(), RetirementError> {
        let mut state = self.state.lock().map_err(|_| RetirementError::Poisoned)?;
        state.retained_bytes = state
            .retained_bytes
            .checked_add(item.bytes.get())
            .expect("retained bytes arithmetic overflow");
        state.items.push_back(item);
        Ok(())
    }

    fn cancel_reservation(&self, _bytes: ByteLength, lease: Option<ResourceLease>) {
        if let Ok(mut state) = self.state.lock() {
            state.reserved_slots = state.reserved_slots.saturating_sub(1);
        }
        // Dropping lease releases budget
        drop(lease);
    }
}

/// Point-in-time snapshot of the bounded retirement queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetirementStatus {
    pub capacity: usize,
    pub in_flight_slots: usize,
    pub pending_items: usize,
    pub retained_bytes: ByteLength,
    pub accepted_reservations: u64,
    pub rejected_saturation: u64,
    pub items_destroyed: u64,
    pub bytes_reclaimed: u64,
}

/// Report produced by an off-UI drain cycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetirementDrainReport {
    pub items_destroyed: usize,
    pub bytes_reclaimed: ByteLength,
    pub remaining_pending: usize,
}

/// A host-owned, bounded CPU retirement queue for off-UI destruction.
#[derive(Debug, Clone)]
pub struct BoundedRetirementQueue {
    inner: Arc<RetirementQueueInner>,
}

impl BoundedRetirementQueue {
    /// Create a new bounded retirement queue with a fixed slot capacity.
    pub fn new(
        owner: ArenaOwnerId,
        capacity: usize,
        budget: Option<ResourceBudget>,
    ) -> Result<Self, CoreError> {
        if capacity == 0 {
            return Err(CoreError::LimitExceeded);
        }
        Ok(Self {
            inner: Arc::new(RetirementQueueInner {
                capacity,
                owner,
                budget,
                counters: RetirementCounters::default(),
                state: Mutex::new(RetirementState {
                    reserved_slots: 0,
                    items: VecDeque::with_capacity(capacity),
                    retained_bytes: 0,
                }),
            }),
        })
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    pub fn owner(&self) -> ArenaOwnerId {
        self.inner.owner
    }

    /// Pre-reserve a retirement slot before attempting a publication.
    pub fn reserve_slot(
        &self,
        bytes: ByteLength,
    ) -> Result<RetirementReservation, RetirementError> {
        if bytes.get() == 0 {
            return Err(RetirementError::InvalidBytes);
        }

        let mut state = self.inner.state.lock().map_err(|_| RetirementError::Poisoned)?;
        if state.reserved_slots >= self.inner.capacity {
            self.inner.counters.rejected_saturation.fetch_add(1, Ordering::Relaxed);
            return Err(RetirementError::QueueSaturated {
                capacity: self.inner.capacity,
            });
        }

        // Acquire protected retirement capacity from budget if attached
        let lease = if let Some(ref budget) = self.inner.budget {
            let alloc_id = ResourceAllocationId::new(RETIREMENT_SLOT_NONCE.fetch_add(1, Ordering::Relaxed))
                .map_err(|_| RetirementError::InvalidBytes)?;
            let mut seq = ResourceAcquisitionSequence::new(self.inner.owner);
            let lease = seq.try_reserve(
                budget,
                alloc_id,
                ResourceKind::Managed,
                ResourceReservationClass::Retirement,
                ResourceAcquisitionOrder::Publication,
                bytes,
            )?;
            Some(lease)
        } else {
            None
        };

        state.reserved_slots += 1;
        self.inner.counters.accepted_reservations.fetch_add(1, Ordering::Relaxed);
        let slot_id = RetirementSlotId::next();

        Ok(RetirementReservation {
            slot_id,
            bytes,
            lease,
            queue: Arc::clone(&self.inner),
            consumed: false,
        })
    }

    /// Atomically swap an active snapshot on the interaction thread if retirement
    /// capacity exists, or defer if the queue is saturated.
    pub fn publish_or_defer<T: 'static + Send + Sync>(
        &self,
        active: &mut Arc<T>,
        new_value: Arc<T>,
        old_bytes: ByteLength,
    ) -> PublicationOutcome<T> {
        let reservation = match self.reserve_slot(old_bytes) {
            Ok(res) => res,
            Err(err) => {
                return PublicationOutcome::Deferred {
                    attempted: new_value,
                    reason: err,
                };
            }
        };

        let slot_id = reservation.slot_id();
        let retired_bytes = reservation.reserved_bytes();
        match reservation.commit_swap(active, new_value) {
            Ok(()) => PublicationOutcome::Published {
                slot_id,
                retired_bytes,
            },
            Err(err) => PublicationOutcome::Deferred {
                attempted: Arc::clone(active),
                reason: err,
            },
        }
    }

    /// Drain up to `max_items` off the calling thread, performing the actual
    /// deallocation of old snapshots outside the interaction path.
    pub fn drain_batch(&self, max_items: usize) -> Result<RetirementDrainReport, RetirementError> {
        if max_items == 0 {
            return Ok(RetirementDrainReport {
                items_destroyed: 0,
                bytes_reclaimed: ByteLength::new(0),
                remaining_pending: self.status()?.pending_items,
            });
        }

        let items_to_drop = {
            let mut state = self.inner.state.lock().map_err(|_| RetirementError::Poisoned)?;
            let count = max_items.min(state.items.len());
            let mut drained = Vec::with_capacity(count);
            for _ in 0..count {
                if let Some(item) = state.items.pop_front() {
                    state.reserved_slots = state.reserved_slots.saturating_sub(1);
                    state.retained_bytes = state.retained_bytes.saturating_sub(item.bytes.get());
                    drained.push(item);
                }
            }
            drained
        };

        let count = items_to_drop.len();
        let bytes_sum: u64 = items_to_drop.iter().map(|it| it.bytes.get()).sum();

        // Drop payloads and leases outside the lock!
        drop(items_to_drop);

        self.inner.counters.items_destroyed.fetch_add(count as u64, Ordering::Relaxed);
        self.inner.counters.bytes_reclaimed.fetch_add(bytes_sum, Ordering::Relaxed);

        let remaining = self.inner.state.lock().map_or(0, |s| s.items.len());

        Ok(RetirementDrainReport {
            items_destroyed: count,
            bytes_reclaimed: ByteLength::new(bytes_sum),
            remaining_pending: remaining,
        })
    }

    /// Drain all pending retirement items off the interaction thread.
    pub fn drain_all(&self) -> Result<RetirementDrainReport, RetirementError> {
        self.drain_batch(usize::MAX)
    }

    /// Read the current state and metrics snapshot.
    pub fn status(&self) -> Result<RetirementStatus, RetirementError> {
        let state = self.inner.state.lock().map_err(|_| RetirementError::Poisoned)?;
        Ok(RetirementStatus {
            capacity: self.inner.capacity,
            in_flight_slots: state.reserved_slots,
            pending_items: state.items.len(),
            retained_bytes: ByteLength::new(state.retained_bytes),
            accepted_reservations: self.inner.counters.accepted_reservations.load(Ordering::Relaxed),
            rejected_saturation: self.inner.counters.rejected_saturation.load(Ordering::Relaxed),
            items_destroyed: self.inner.counters.items_destroyed.load(Ordering::Relaxed),
            bytes_reclaimed: self.inner.counters.bytes_reclaimed.load(Ordering::Relaxed),
        })
    }
}

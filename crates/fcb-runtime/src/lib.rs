//! Host-owned runtime probes for bounded wake delivery.
//!
//! [`WakeProbe`] is deliberately a small lab seam rather than a scheduler. It
//! makes the two wake classes explicit: ordered commands are retained in FIFO
//! order up to a fixed capacity, while motion is represented by its latest
//! update. A generation carried by each consumer batch makes the final wake
//! reset conditional on the state the consumer actually observed. A producer
//! arriving between drain and acknowledgement therefore rearms the wake
//! instead of being lost.

#![forbid(unsafe_code)]

use fcb_core::tracing::{AbsoluteDeadline, DeadlineState, MonotonicTimestamp, TracingError};
use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::Duration;

/// Urgency carried by a host wake request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum WakePriority {
    /// Background maintenance that can yield to all other work.
    Background = 0,
    /// Ordinary interactive work.
    Normal = 1,
    /// Work that should be serviced before ordinary work.
    Urgent = 2,
}

impl WakePriority {
    /// Meet two requests by retaining the greatest urgency.
    pub const fn meet(self, other: Self) -> Self {
        if self as u8 >= other as u8 {
            self
        } else {
            other
        }
    }
}

/// An ordered, non-coalescible command.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WakeCommand {
    /// Caller-owned identity used to correlate the command at delivery.
    pub id: u64,
    /// Opaque command payload; the probe never interprets it.
    pub payload: u64,
}

/// The latest coalescible motion state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MotionUpdate {
    /// Caller-owned update sequence.
    pub sequence: u64,
    /// Opaque latest motion value.
    pub value: u64,
}

/// Result of publishing an update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishOutcome {
    /// Whether a previous motion update was replaced.
    pub coalesced_motion: bool,
    /// Generation that a consumer must observe before acknowledging.
    pub wake_generation: u64,
}

/// A bounded snapshot delivered to one consumer turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WakeBatch {
    generation: u64,
    priority: WakePriority,
    ordered: Vec<WakeCommand>,
    motion: Option<MotionUpdate>,
}

impl WakeBatch {
    /// Generation sampled when this batch was taken.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Maximum urgency observed by this batch.
    pub const fn priority(&self) -> WakePriority {
        self.priority
    }

    /// Ordered commands in their original FIFO order.
    pub fn ordered(&self) -> &[WakeCommand] {
        &self.ordered
    }

    /// Latest motion state, if one was pending at the snapshot boundary.
    pub const fn motion(&self) -> Option<MotionUpdate> {
        self.motion
    }
}

/// Outcome of attempting to retire a consumer batch's wake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WakeReset {
    /// The batch was the complete current state and the wake was retired.
    Reset,
    /// A newer event or an undrained command keeps the wake pending.
    StillPending { generation: u64 },
}

/// A read-only bounded state snapshot for tests and host diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WakeStatus {
    /// Whether a consumer wake remains pending.
    pub wake_pending: bool,
    /// Highest urgency among pending requests.
    pub priority: WakePriority,
    /// Number of ordered commands waiting in FIFO storage.
    pub ordered_pending: usize,
    /// Whether a motion update is waiting.
    pub motion_pending: bool,
    /// Successfully accepted ordered commands over the probe lifetime.
    pub accepted_ordered: u64,
    /// Ordered commands refused at the fixed capacity.
    pub rejected_ordered: u64,
    /// Motion updates that replaced an earlier pending motion value.
    pub coalesced_motion: u64,
    /// Ordered commands delivered to a consumer, a monotonic fairness signal.
    pub maintenance_progress: u64,
}

/// Refusals from the bounded wake probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WakeError {
    /// The ordered command queue has no free slot.
    QueueSaturated { capacity: usize },
    /// A consumer must request at least one ordered command.
    InvalidDrainLimit,
    /// A prior panic poisoned the host-owned mutex.
    Poisoned,
    /// The wake generation or command sequence space cannot advance.
    SequenceExhausted,
    /// The clock-domain adapter rejected a deadline operation.
    Deadline(TracingError),
}

impl fmt::Display for WakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueSaturated { capacity } => {
                write!(formatter, "ordered wake queue saturated at {capacity}")
            }
            Self::InvalidDrainLimit => formatter.write_str("wake drain limit must be non-zero"),
            Self::Poisoned => formatter.write_str("wake probe state is poisoned"),
            Self::SequenceExhausted => formatter.write_str("wake generation space exhausted"),
            Self::Deadline(error) => write!(formatter, "deadline rejected: {error:?}"),
        }
    }
}

impl std::error::Error for WakeError {}

impl From<TracingError> for WakeError {
    fn from(error: TracingError) -> Self {
        Self::Deadline(error)
    }
}

/// Construct a checked absolute deadline from a host monotonic reading.
pub fn deadline_after(
    start: MonotonicTimestamp,
    lead: Duration,
) -> Result<AbsoluteDeadline, WakeError> {
    AbsoluteDeadline::after(start, lead).map_err(WakeError::from)
}

/// Compare a checked deadline with a host monotonic reading.
pub fn deadline_state(
    deadline: &AbsoluteDeadline,
    now: MonotonicTimestamp,
) -> Result<DeadlineState, WakeError> {
    deadline.state_from(now).map_err(WakeError::from)
}

struct WakeState {
    ordered: VecDeque<WakeCommand>,
    motion: Option<MotionUpdate>,
    generation: u64,
    priority: WakePriority,
    wake_pending: bool,
    accepted_ordered: u64,
    rejected_ordered: u64,
    coalesced_motion: u64,
    maintenance_progress: u64,
}

/// A fixed-capacity, host-owned wake coalescing and fairness probe.
pub struct WakeProbe {
    capacity: NonZeroUsize,
    state: Mutex<WakeState>,
}

impl WakeProbe {
    /// Create an inert probe with a fixed ordered-command capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            state: Mutex::new(WakeState {
                ordered: VecDeque::with_capacity(capacity.get()),
                motion: None,
                generation: 0,
                priority: WakePriority::Background,
                wake_pending: false,
                accepted_ordered: 0,
                rejected_ordered: 0,
                coalesced_motion: 0,
                maintenance_progress: 0,
            }),
        }
    }

    /// Fixed queue capacity, useful for reporting saturation decisions.
    pub const fn capacity(&self) -> NonZeroUsize {
        self.capacity
    }

    /// Request a wake without adding payload; urgency is combined by max.
    pub fn request_wake(&self, priority: WakePriority) -> Result<u64, WakeError> {
        let mut state = self.lock()?;
        Self::arm(&mut state, priority)?;
        Ok(state.generation)
    }

    /// Publish a non-coalescible command in FIFO order.
    pub fn publish_ordered(
        &self,
        command: WakeCommand,
        priority: WakePriority,
    ) -> Result<PublishOutcome, WakeError> {
        let mut state = self.lock()?;
        if state.ordered.len() == self.capacity.get() {
            state.rejected_ordered = state.rejected_ordered.saturating_add(1);
            return Err(WakeError::QueueSaturated {
                capacity: self.capacity.get(),
            });
        }
        if state.accepted_ordered == u64::MAX {
            return Err(WakeError::SequenceExhausted);
        }
        state.ordered.push_back(command);
        state.accepted_ordered += 1;
        Self::arm(&mut state, priority)?;
        Ok(PublishOutcome {
            coalesced_motion: false,
            wake_generation: state.generation,
        })
    }

    /// Publish motion, replacing only the previous pending motion value.
    pub fn publish_motion(
        &self,
        motion: MotionUpdate,
        priority: WakePriority,
    ) -> Result<PublishOutcome, WakeError> {
        let mut state = self.lock()?;
        let coalesced = state.motion.is_some();
        if coalesced && state.coalesced_motion == u64::MAX {
            return Err(WakeError::SequenceExhausted);
        }
        state.motion = Some(motion);
        if coalesced {
            state.coalesced_motion += 1;
        }
        Self::arm(&mut state, priority)?;
        Ok(PublishOutcome {
            coalesced_motion: coalesced,
            wake_generation: state.generation,
        })
    }

    /// Take a bounded batch. Ordered work is drained first, so motion floods
    /// cannot starve maintenance or completion commands.
    pub fn take(&self, max_ordered: usize) -> Result<WakeBatch, WakeError> {
        if max_ordered == 0 {
            return Err(WakeError::InvalidDrainLimit);
        }
        let mut state = self.lock()?;
        let count = max_ordered.min(state.ordered.len());
        let progress = u64::try_from(count).map_err(|_| WakeError::SequenceExhausted)?;
        let new_progress = state
            .maintenance_progress
            .checked_add(progress)
            .ok_or(WakeError::SequenceExhausted)?;
        let mut ordered = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(command) = state.ordered.pop_front() {
                ordered.push(command);
            }
        }
        state.maintenance_progress = new_progress;
        Ok(WakeBatch {
            generation: state.generation,
            priority: state.priority,
            ordered,
            motion: state.motion.take(),
        })
    }

    /// Retire a batch only if no newer producer mutation occurred.
    pub fn acknowledge(&self, batch: WakeBatch) -> Result<WakeReset, WakeError> {
        let mut state = self.lock()?;
        if state.generation == batch.generation
            && state.ordered.is_empty()
            && state.motion.is_none()
        {
            state.wake_pending = false;
            state.priority = WakePriority::Background;
            Ok(WakeReset::Reset)
        } else {
            Ok(WakeReset::StillPending {
                generation: state.generation,
            })
        }
    }

    /// Read counters and pending-state flags without changing the probe.
    pub fn status(&self) -> Result<WakeStatus, WakeError> {
        let state = self.lock()?;
        Ok(WakeStatus {
            wake_pending: state.wake_pending,
            priority: state.priority,
            ordered_pending: state.ordered.len(),
            motion_pending: state.motion.is_some(),
            accepted_ordered: state.accepted_ordered,
            rejected_ordered: state.rejected_ordered,
            coalesced_motion: state.coalesced_motion,
            maintenance_progress: state.maintenance_progress,
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, WakeState>, WakeError> {
        self.state.lock().map_err(|_| WakeError::Poisoned)
    }

    fn arm(state: &mut WakeState, priority: WakePriority) -> Result<(), WakeError> {
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(WakeError::SequenceExhausted)?;
        state.wake_pending = true;
        state.priority = state.priority.meet(priority);
        Ok(())
    }
}

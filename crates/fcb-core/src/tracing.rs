//! Early bounded tracing and clock-domain adapters.
//!
//! This module provides the fixed-capacity event rings, verified absolute
//! deadlines, and checked monotonic conversions used by early runtime
//! diagnostics. Construction performs no global, thread, environment,
//! filesystem, or runtime side effects: a ring allocates exactly the buffer
//! its capacity requests, and every clock value is supplied by the host.
//!
//! Overflow is never silent. A full ring either rejects the event (counting
//! it as dropped) or explicitly evicts the oldest record (counting it as
//! evicted); both counters are readable at any time through
//! [`FixedEventRing::overflow_summary`].

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::time::Duration;

use crate::ClockDomainId;

/// Typed refusal from the tracing and clock-domain seam.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TracingError {
    /// Adding nanoseconds to a timestamp exceeded `u64::MAX`.
    TimestampOverflow,
    /// Subtracting from a timestamp would precede the domain epoch (`0`).
    TimestampUnderflow,
    /// A `Duration` could not be represented as `u64` nanoseconds.
    InvalidDuration,
    /// An adapter does not link the requested source and target domains.
    DomainMismatch {
        /// Domain the adapter maps from.
        expected: ClockDomainId,
        /// Domain the caller asked to map onto.
        actual: ClockDomainId,
    },
    /// The ring is at capacity and the event was rejected, not recorded.
    RingFull {
        /// Fixed capacity of the ring.
        capacity: usize,
        /// Total events dropped so far, including this rejection.
        dropped_total: u64,
    },
    /// The ring's `u64` sequence space is exhausted; no further event can
    /// be accepted on this ring.
    SequenceExhausted,
}

impl TracingError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TimestampOverflow => "TIMESTAMP_OVERFLOW",
            Self::TimestampUnderflow => "TIMESTAMP_UNDERFLOW",
            Self::InvalidDuration => "INVALID_DURATION",
            Self::DomainMismatch { .. } => "DOMAIN_MISMATCH",
            Self::RingFull { .. } => "RING_FULL",
            Self::SequenceExhausted => "SEQUENCE_EXHAUSTED",
        }
    }
}

impl fmt::Display for TracingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimestampOverflow => formatter.write_str("timestamp overflow"),
            Self::TimestampUnderflow => formatter.write_str("timestamp underflow"),
            Self::InvalidDuration => formatter.write_str("invalid duration"),
            Self::DomainMismatch { expected, actual } => {
                write!(
                    formatter,
                    "clock domain mismatch: expected domain {}, got domain {}",
                    expected.get(),
                    actual.get()
                )
            }
            Self::RingFull { capacity, dropped_total } => {
                write!(
                    formatter,
                    "trace event ring full (capacity {capacity}, total dropped {dropped_total})"
                )
            }
            Self::SequenceExhausted => formatter.write_str("trace sequence space exhausted"),
        }
    }
}

impl std::error::Error for TracingError {}

/// A reading on one clock domain's monotonic timeline, in nanoseconds.
///
/// `0` is a valid reading (the domain epoch). Arithmetic is checked; no
/// conversion may wrap or silently saturate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MonotonicTimestamp {
    domain: ClockDomainId,
    nanos: u64,
}

impl MonotonicTimestamp {
    pub const fn new(domain: ClockDomainId, nanos: u64) -> Self {
        Self { domain, nanos }
    }

    pub const fn domain(self) -> ClockDomainId {
        self.domain
    }

    pub const fn as_nanos(self) -> u64 {
        self.nanos
    }

    /// Checked forward movement on the same domain's timeline.
    pub fn checked_add_nanos(self, nanos: u64) -> Result<Self, TracingError> {
        let total = self
            .nanos
            .checked_add(nanos)
            .ok_or(TracingError::TimestampOverflow)?;
        Ok(Self {
            domain: self.domain,
            nanos: total,
        })
    }

    /// Checked backward movement on the same domain's timeline.
    pub fn checked_sub_nanos(self, nanos: u64) -> Result<Self, TracingError> {
        let difference = self
            .nanos
            .checked_sub(nanos)
            .ok_or(TracingError::TimestampUnderflow)?;
        Ok(Self {
            domain: self.domain,
            nanos: difference,
        })
    }

    /// Native conversion from a `Duration` offset on this domain's timeline.
    pub fn from_duration(domain: ClockDomainId, offset: Duration) -> Result<Self, TracingError> {
        let nanos = u64::try_from(offset.as_nanos()).map_err(|_| TracingError::InvalidDuration)?;
        Ok(Self { domain, nanos })
    }

    /// Native conversion to a `Duration` offset; lossless for `u64` nanos.
    pub const fn as_duration(self) -> Duration {
        Duration::from_nanos(self.nanos)
    }

    /// Map this reading onto another clock domain through a verified adapter.
    pub fn convert_to(
        self,
        adapter: &DomainOffset,
        target: ClockDomainId,
    ) -> Result<Self, TracingError> {
        if adapter.source != self.domain {
            return Err(TracingError::DomainMismatch {
                expected: adapter.source,
                actual: self.domain,
            });
        }
        if adapter.target != target {
            return Err(TracingError::DomainMismatch {
                expected: adapter.target,
                actual: target,
            });
        }
        let shifted = self.nanos as i128 + adapter.nanos as i128;
        if shifted < 0 {
            return Err(TracingError::TimestampUnderflow);
        }
        let nanos = u64::try_from(shifted).map_err(|_| TracingError::TimestampOverflow)?;
        Ok(Self {
            domain: target,
            nanos,
        })
    }
}

/// A verified mapping between two clock domains of one owner.
///
/// The offset is a signed correction such that
/// `target_reading = source_reading + offset`. It is only accepted for the
/// exact domain pair it was established for; every conversion revalidates the
/// link so a stale adapter cannot move readings across the wrong domains.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DomainOffset {
    source: ClockDomainId,
    target: ClockDomainId,
    nanos: i64,
}

impl DomainOffset {
    /// Establish an adapter for an exact domain pair from a signed offset.
    pub fn new(
        source: ClockDomainId,
        target: ClockDomainId,
        nanos: i64,
    ) -> Result<Self, TracingError> {
        if source == target {
            return Err(TracingError::DomainMismatch {
                expected: source,
                actual: target,
            });
        }
        Ok(Self {
            source,
            target,
            nanos,
        })
    }

    /// Verify an adapter from one paired reading across two domains.
    ///
    /// The offset is computed in `i128` and rejected if it cannot be stored
    /// exactly, so adapters are never lossy approximations.
    pub fn between_readings(
        source: MonotonicTimestamp,
        target: MonotonicTimestamp,
    ) -> Result<Self, TracingError> {
        let difference = target.nanos as i128 - source.nanos as i128;
        let nanos = i64::try_from(difference).map_err(|_| TracingError::InvalidDuration)?;
        Self::new(source.domain, target.domain, nanos)
    }

    pub const fn source(self) -> ClockDomainId {
        self.source
    }

    pub const fn target(self) -> ClockDomainId {
        self.target
    }

    pub const fn offset_nanos(self) -> i64 {
        self.nanos
    }
}

/// Whether a deadline is still outstanding or has already passed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeadlineState {
    /// The deadline is in the future by the stated remainder.
    Active {
        /// Time left until the deadline (zero at the deadline itself).
        remaining: Duration,
    },
    /// The deadline passed by the stated amount.
    Expired {
        /// Strictly positive time since the deadline.
        ago: Duration,
    },
}

/// A verified absolute deadline on one clock domain's monotonic timeline.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AbsoluteDeadline {
    at: MonotonicTimestamp,
}

impl AbsoluteDeadline {
    /// Deadlines for `start + lead`, with checked native arithmetic.
    pub fn after(start: MonotonicTimestamp, lead: Duration) -> Result<Self, TracingError> {
        let nanos = u64::try_from(lead.as_nanos()).map_err(|_| TracingError::InvalidDuration)?;
        Ok(Self {
            at: start.checked_add_nanos(nanos)?,
        })
    }

    pub const fn at(self) -> MonotonicTimestamp {
        self.at
    }

    /// Compare the deadline against a host reading on the same domain.
    pub fn state_from(&self, now: MonotonicTimestamp) -> Result<DeadlineState, TracingError> {
        if now.domain != self.at.domain {
            return Err(TracingError::DomainMismatch {
                expected: self.at.domain,
                actual: now.domain,
            });
        }
        if now.nanos <= self.at.nanos {
            let remaining = self
                .at
                .nanos
                .checked_sub(now.nanos)
                .ok_or(TracingError::TimestampUnderflow)?;
            Ok(DeadlineState::Active {
                remaining: Duration::from_nanos(remaining),
            })
        } else {
            let ago = now
                .nanos
                .checked_sub(self.at.nanos)
                .ok_or(TracingError::TimestampOverflow)?;
            Ok(DeadlineState::Expired {
                ago: Duration::from_nanos(ago),
            })
        }
    }
}

/// The fixed vocabulary of early runtime trace events.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TraceEventKind {
    /// Capacity or work was admitted.
    Admitted,
    /// An admitted unit reached a committed outcome.
    Completed,
    /// A generation was retired and its capacity released.
    Retired,
    /// An event was dropped for lack of ring or capacity budget.
    Dropped,
}

/// One recorded trace event with a ring-unique sequence number.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TraceEvent {
    sequence: u64,
    at: MonotonicTimestamp,
    kind: TraceEventKind,
    detail: u64,
}

impl TraceEvent {
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    pub const fn at(self) -> MonotonicTimestamp {
        self.at
    }

    pub const fn kind(self) -> TraceEventKind {
        self.kind
    }

    pub const fn detail(self) -> u64 {
        self.detail
    }
}

/// Explicit, always-readable overflow accounting for a ring.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OverflowSummary {
    /// Fixed capacity; never changes after construction.
    pub capacity: usize,
    /// Records currently retained.
    pub occupied: usize,
    /// Events rejected because the ring was full.
    pub dropped: u64,
    /// Oldest records evicted by overwrite pushes.
    pub evicted: u64,
    /// Events accepted into the ring over its lifetime.
    pub accepted: u64,
}

/// A fixed-capacity event ring with monotonic sequence numbers.
///
/// The ring allocates its buffer once at construction and never reallocates.
/// Sequence numbers start at 1 and increase by one for every accepted event,
/// including evicted ones, so downstream consumers can detect gaps exactly.
#[derive(Debug)]
pub struct FixedEventRing {
    capacity: NonZeroUsize,
    events: VecDeque<TraceEvent>,
    next_sequence: u64,
    dropped: u64,
    evicted: u64,
    accepted: u64,
}

impl FixedEventRing {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            events: VecDeque::with_capacity(capacity.get()),
            next_sequence: 1,
            dropped: 0,
            evicted: 0,
            accepted: 0,
        }
    }

    pub const fn capacity(&self) -> usize {
        self.capacity.get()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &TraceEvent> {
        self.events.iter()
    }

    /// Record an event, rejecting it if the ring is at capacity.
    ///
    /// A rejected event never enters the ring and consumes no sequence
    /// number; the drop is counted and reported in the overflow summary.
    /// Sequence numbers name accepted records only, so gaps are exactly
    /// observable by downstream consumers.
    pub fn push(
        &mut self,
        at: MonotonicTimestamp,
        kind: TraceEventKind,
        detail: u64,
    ) -> Result<u64, TracingError> {
        if self.events.len() >= self.capacity.get() {
            self.dropped = self
                .dropped
                .checked_add(1)
                .ok_or(TracingError::SequenceExhausted)?;
            return Err(TracingError::RingFull {
                capacity: self.capacity.get(),
                dropped_total: self.dropped,
            });
        }
        self.push_accepted(at, kind, detail)
    }

    /// Record an event, explicitly evicting the oldest record if full.
    pub fn push_overwrite(
        &mut self,
        at: MonotonicTimestamp,
        kind: TraceEventKind,
        detail: u64,
    ) -> Result<u64, TracingError> {
        if self.events.len() >= self.capacity.get() {
            self.events.pop_front();
            self.evicted = self
                .evicted
                .checked_add(1)
                .ok_or(TracingError::SequenceExhausted)?;
        }
        self.push_accepted(at, kind, detail)
    }

    /// Read the current explicit overflow accounting.
    pub fn overflow_summary(&self) -> OverflowSummary {
        OverflowSummary {
            capacity: self.capacity.get(),
            occupied: self.events.len(),
            dropped: self.dropped,
            evicted: self.evicted,
            accepted: self.accepted,
        }
    }

    fn push_accepted(
        &mut self,
        at: MonotonicTimestamp,
        kind: TraceEventKind,
        detail: u64,
    ) -> Result<u64, TracingError> {
        let sequence = self.next_sequence;
        self.next_sequence = sequence
            .checked_add(1)
            .ok_or(TracingError::SequenceExhausted)?;
        self.events.push_back(TraceEvent {
            sequence,
            at,
            kind,
            detail,
        });
        self.accepted = self
            .accepted
            .checked_add(1)
            .ok_or(TracingError::SequenceExhausted)?;
        Ok(sequence)
    }
}

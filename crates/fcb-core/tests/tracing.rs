//! Focused tests for the early bounded tracing and clock-domain seam.

#![forbid(unsafe_code)]

use fcb_core::tracing::{
    AbsoluteDeadline, DeadlineState, DomainOffset, FixedEventRing, MonotonicTimestamp,
    OverflowSummary, TraceEventKind, TracingError,
};
use fcb_core::{ArenaOwnerId, ClockDomainId};
use std::time::Duration;

fn owner(value: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(value).unwrap()
}

fn domain(owner_value: u64, domain_value: u64) -> ClockDomainId {
    ClockDomainId::new(owner(owner_value), domain_value).unwrap()
}

fn stamp(domain_value: u64, nanos: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::new(domain(7, domain_value), nanos)
}

#[test]
fn full_ring_rejects_and_reports_explicit_overflow_summary() {
    let mut ring = FixedEventRing::new(std::num::NonZeroUsize::new(2).unwrap());
    assert!(ring.is_empty());
    assert_eq!(ring.capacity(), 2);

    let a = stamp(1, 10);
    let b = stamp(1, 20);
    let c = stamp(1, 30);

    assert_eq!(ring.push(a, TraceEventKind::Admitted, 1).unwrap(), 1);
    assert_eq!(ring.push(b, TraceEventKind::Completed, 2).unwrap(), 2);

    let rejected = ring.push(c, TraceEventKind::Dropped, 3);
    assert_eq!(
        rejected,
        Err(TracingError::RingFull {
            capacity: 2,
            dropped_total: 1
        })
    );
    assert_eq!(rejected.unwrap_err().code(), "RING_FULL");

    // The rejected event consumed no slot and no sequence number.
    assert_eq!(ring.len(), 2);

    // Overwrite explicitly evicts the oldest record and keeps counting.
    assert_eq!(ring.push_overwrite(c, TraceEventKind::Retired, 4).unwrap(), 3);
    let evicted_first = ring.iter().next().unwrap();
    assert_eq!(evicted_first.sequence(), 2);
    assert_eq!(evicted_first.kind(), TraceEventKind::Completed);

    let summary = ring.overflow_summary();
    assert_eq!(
        summary,
        OverflowSummary {
            capacity: 2,
            occupied: 2,
            dropped: 1,
            evicted: 1,
            accepted: 3,
        }
    );
}

#[test]
fn deadlines_and_native_duration_conversions_are_checked() {
    let now = stamp(1, 1_000);

    // Duration -> nanos -> Duration round-trips exactly.
    let offset = MonotonicTimestamp::from_duration(now.domain(), Duration::from_nanos(987_654_321))
        .unwrap();
    assert_eq!(offset.as_nanos(), 987_654_321);
    assert_eq!(offset.as_duration(), Duration::from_nanos(987_654_321));

    // A Duration above u64::MAX nanoseconds is rejected, not truncated.
    let too_big = Duration::from_secs(u64::MAX);
    assert_eq!(
        MonotonicTimestamp::from_duration(now.domain(), too_big),
        Err(TracingError::InvalidDuration)
    );

    // Deadline arithmetic is checked at the u64 boundary.
    let edge = MonotonicTimestamp::new(now.domain(), u64::MAX);
    assert_eq!(
        AbsoluteDeadline::after(edge, Duration::from_nanos(1))
            .unwrap_err()
            .code(),
        "TIMESTAMP_OVERFLOW"
    );
    assert_eq!(
        edge.checked_add_nanos(1),
        Err(TracingError::TimestampOverflow)
    );

    let deadline = AbsoluteDeadline::after(now, Duration::from_nanos(100)).unwrap();
    assert_eq!(
        deadline.state_from(now.checked_add_nanos(40).unwrap()).unwrap(),
        DeadlineState::Active {
            remaining: Duration::from_nanos(60)
        }
    );
    assert_eq!(
        deadline.state_from(now.checked_add_nanos(100).unwrap()).unwrap(),
        DeadlineState::Active {
            remaining: Duration::from_nanos(0)
        }
    );
    let expired = deadline.state_from(now.checked_add_nanos(101).unwrap()).unwrap();
    assert_eq!(
        expired,
        DeadlineState::Expired {
            ago: Duration::from_nanos(1)
        }
    );

    // Deadlines refuse readings from a different clock domain.
    let foreign = MonotonicTimestamp::new(domain(7, 2), now.as_nanos());
    assert_eq!(
        deadline.state_from(foreign),
        Err(TracingError::DomainMismatch {
            expected: now.domain(),
            actual: foreign.domain()
        })
    );
}

#[test]
fn domain_adapters_are_pair_verified_and_checked() {
    let source_domain = domain(7, 1);
    let target_domain = domain(7, 2);
    let unrelated_domain = domain(7, 3);

    let source = MonotonicTimestamp::new(source_domain, 1_000);
    let target = MonotonicTimestamp::new(target_domain, 1_005);

    // A verified adapter from one paired reading.
    let adapter = DomainOffset::between_readings(source, target).unwrap();
    assert_eq!(adapter.offset_nanos(), 5);
    assert_eq!(adapter.source(), source_domain);
    assert_eq!(adapter.target(), target_domain);

    let converted = source.convert_to(&adapter, target_domain).unwrap();
    assert_eq!(converted.domain(), target_domain);
    assert_eq!(converted.as_nanos(), 1_005);

    // The adapter refuses to move readings across the wrong pair.
    assert_eq!(
        source.convert_to(&adapter, unrelated_domain),
        Err(TracingError::DomainMismatch {
            expected: target_domain,
            actual: unrelated_domain
        })
    );

    // Same-domain adapters are rejected at construction.
    assert_eq!(
        DomainOffset::new(source_domain, source_domain, 0),
        Err(TracingError::DomainMismatch {
            expected: source_domain,
            actual: source_domain
        })
    );

    // Negative offsets are exact and refuse to precede the target epoch.
    let negative = DomainOffset::new(target_domain, source_domain, -2).unwrap();
    assert_eq!(
        MonotonicTimestamp::new(target_domain, 1)
            .convert_to(&negative, source_domain)
            .unwrap_err()
            .code(),
        "TIMESTAMP_UNDERFLOW"
    );
    let mapped = MonotonicTimestamp::new(target_domain, 3)
        .convert_to(&negative, source_domain)
        .unwrap();
    assert_eq!(mapped.as_nanos(), 1);

    // Offsets that cannot be stored exactly are rejected, not approximated.
    let huge = MonotonicTimestamp::new(source_domain, u64::MAX);
    let tiny = MonotonicTimestamp::new(target_domain, 0);
    assert_eq!(
        DomainOffset::between_readings(tiny, huge)
            .unwrap_err()
            .code(),
        "INVALID_DURATION"
    );
}

#[test]
fn backward_movement_stays_on_domain_timeline() {
    let now = stamp(1, 100);
    assert_eq!(
        now.checked_sub_nanos(101),
        Err(TracingError::TimestampUnderflow)
    );
    assert_eq!(
        now.checked_sub_nanos(100).unwrap().as_nanos(),
        0,
        "the domain epoch is a valid reading"
    );
}

#[test]
fn tracing_error_implements_display_and_error() {
    use std::error::Error;
    let err = TracingError::TimestampOverflow;
    assert_eq!(format!("{err}"), "timestamp overflow");
    let trait_obj: &dyn Error = &err;
    assert_eq!(trait_obj.to_string(), "timestamp overflow");

    let d_err = TracingError::DomainMismatch {
        expected: domain(7, 1),
        actual: domain(7, 2),
    };
    assert_eq!(
        format!("{d_err}"),
        "clock domain mismatch: expected domain 1, got domain 2"
    );
}

#[test]
fn convert_to_identifies_source_domain_mismatch_specifically() {
    let d1 = domain(7, 1);
    let d2 = domain(7, 2);
    let d3 = domain(7, 3);
    let adapter = DomainOffset::new(d1, d2, 10).unwrap();

    // Timestamp from domain 3 passed to adapter mapping from domain 1
    let reading_d3 = MonotonicTimestamp::new(d3, 100);
    assert_eq!(
        reading_d3.convert_to(&adapter, d2),
        Err(TracingError::DomainMismatch {
            expected: d1,
            actual: d3,
        })
    );
}

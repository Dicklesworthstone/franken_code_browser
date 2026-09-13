use fcb_core::tracing::{DeadlineState, MonotonicTimestamp};
use fcb_core::{ArenaOwnerId, ClockDomainId};
use fcb_runtime::{
    deadline_after, deadline_state, MotionUpdate, WakeCommand, WakeError, WakePriority, WakeProbe,
    WakeReset,
};
use std::num::NonZeroUsize;
use std::time::Duration;

fn capacity(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test capacities are non-zero")
}

fn domain(value: u64) -> ClockDomainId {
    ClockDomainId::new(ArenaOwnerId::new(1).expect("test owner is non-zero"), value)
        .expect("test domain is non-zero")
}

#[test]
fn motion_coalesces_but_ordered_commands_remain_fifo() {
    let probe = WakeProbe::new(capacity(4));
    probe
        .publish_ordered(
            WakeCommand { id: 1, payload: 11 },
            WakePriority::Normal,
        )
        .expect("first command fits");
    probe
        .publish_ordered(
            WakeCommand { id: 2, payload: 22 },
            WakePriority::Normal,
        )
        .expect("second command fits");
    let first = probe
        .publish_motion(
            MotionUpdate {
                sequence: 7,
                value: 70,
            },
            WakePriority::Background,
        )
        .expect("first motion fits");
    let second = probe
        .publish_motion(
            MotionUpdate {
                sequence: 8,
                value: 80,
            },
            WakePriority::Urgent,
        )
        .expect("latest motion replaces the prior value");

    assert!(!first.coalesced_motion);
    assert!(second.coalesced_motion);
    let batch = probe.take(4).expect("batch is bounded");
    assert_eq!(
        batch.ordered(),
        &[
            WakeCommand { id: 1, payload: 11 },
            WakeCommand { id: 2, payload: 22 }
        ]
    );
    assert_eq!(
        batch.motion(),
        Some(MotionUpdate {
            sequence: 8,
            value: 80
        })
    );
    assert_eq!(batch.priority(), WakePriority::Urgent);
    assert_eq!(probe.acknowledge(batch).expect("acknowledge batch"), WakeReset::Reset);
}

#[test]
fn producer_interleaving_rearms_after_consumer_drain() {
    let probe = WakeProbe::new(capacity(2));
    probe
        .publish_ordered(
            WakeCommand { id: 1, payload: 0 },
            WakePriority::Normal,
        )
        .expect("initial command fits");
    let batch = probe.take(2).expect("consumer snapshots state");
    probe
        .publish_motion(
            MotionUpdate {
                sequence: 2,
                value: 200,
            },
            WakePriority::Urgent,
        )
        .expect("producer arrives before acknowledgement");

    assert_eq!(
        probe.acknowledge(batch).expect("stale acknowledgement is safe"),
        WakeReset::StillPending { generation: 2 }
    );
    let next = probe.take(1).expect("rearmed wake is consumable");
    assert_eq!(next.ordered(), &[]);
    assert_eq!(next.motion().expect("interleaved motion" ).value, 200);
    assert_eq!(probe.acknowledge(next).expect("final acknowledgement"), WakeReset::Reset);
}

#[test]
fn saturation_refuses_ordered_work_without_losing_motion_or_wake() {
    let probe = WakeProbe::new(capacity(1));
    probe
        .publish_ordered(
            WakeCommand { id: 1, payload: 1 },
            WakePriority::Normal,
        )
        .expect("one command fits");
    assert_eq!(
        probe.publish_ordered(
            WakeCommand { id: 2, payload: 2 },
            WakePriority::Urgent,
        ),
        Err(WakeError::QueueSaturated { capacity: 1 })
    );
    probe
        .publish_motion(
            MotionUpdate {
                sequence: 3,
                value: 300,
            },
            WakePriority::Urgent,
        )
        .expect("motion remains independently coalescible");
    let status = probe.status().expect("status is available");
    assert!(status.wake_pending);
    assert_eq!(status.ordered_pending, 1);
    assert!(status.motion_pending);
    assert_eq!(status.rejected_ordered, 1);
}

#[test]
fn priority_meet_and_fairness_survive_motion_flood() {
    let probe = WakeProbe::new(capacity(8));
    for id in 1..=3 {
        probe
            .publish_ordered(
                WakeCommand { id, payload: id * 10 },
                WakePriority::Background,
            )
            .expect("maintenance command fits");
    }
    for sequence in 1..=32 {
        probe
            .publish_motion(
                MotionUpdate {
                    sequence,
                    value: sequence * 100,
                },
                if sequence == 32 {
                    WakePriority::Urgent
                } else {
                    WakePriority::Background
                },
            )
            .expect("motion update coalesces");
    }
    let batch = probe.take(3).expect("fair batch");
    assert_eq!(batch.ordered().iter().map(|command| command.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(batch.motion().expect("latest motion").sequence, 32);
    assert_eq!(batch.priority(), WakePriority::Urgent);
    assert_eq!(
        probe.status().expect("progress status").maintenance_progress,
        3
    );
    let status = probe.status().expect("bounded aggregate counters");
    assert_eq!(status.accepted_ordered, 3);
    assert_eq!(status.coalesced_motion, 31);
}

#[test]
fn duration_is_not_an_absolute_deadline_and_domain_errors_are_preserved() {
    let domain = domain(41);
    let start = MonotonicTimestamp::new(domain, 100);
    let deadline = deadline_after(start, Duration::from_nanos(25)).expect("duration is checked");
    assert_eq!(
        deadline_state(&deadline, MonotonicTimestamp::new(domain, 110)).expect("same domain"),
        DeadlineState::Active {
            remaining: Duration::from_nanos(15)
        }
    );
    assert_eq!(
        deadline_state(&deadline, MonotonicTimestamp::new(domain, 130)).expect("expired state"),
        DeadlineState::Expired {
            ago: Duration::from_nanos(5)
        }
    );
    let wrong_domain = deadline_state(
        &deadline,
        MonotonicTimestamp::new(domain(42), 110),
    );
    assert!(matches!(wrong_domain, Err(WakeError::Deadline(_))));
}

#[test]
fn zero_drain_limit_is_a_bounded_failure() {
    let probe = WakeProbe::new(capacity(1));
    assert_eq!(
        probe.take(0),
        Err(WakeError::InvalidDrainLimit)
    );
}

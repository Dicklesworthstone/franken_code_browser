//! Production-path tests for lossless GPU terminal record drain and completion conservation (FCB-072.B).

use std::{
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use fcb_core::{
    resources::{
        ResourceAllocationId, ResourceBudget, ResourceKind,
    },
    retirement::BoundedRetirementQueue,
    ArenaOwnerId, ByteLength,
};
use fcb_runtime::{
    terminal::{
        GpuSubmissionId, GpuTerminalError, LosslessTerminalDrainQueue, TerminalCompletionStatus,
        TerminalDrainError, TerminalEventType,
    },
    WakeCommand, WakePriority, WakeProbe,
};

fn arena_owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).expect("valid owner")
}

fn alloc_id(id: u64) -> ResourceAllocationId {
    ResourceAllocationId::new(id).expect("valid alloc")
}

#[test]
fn one_preallocated_record_per_submit_with_capacity_bounding() {
    let queue = LosslessTerminalDrainQueue::new(2).expect("valid queue creation");
    assert_eq!(queue.capacity(), 2);
    assert_eq!(queue.in_flight_count(), 0);

    let sub1 = GpuSubmissionId::next();
    let res1 = queue.reserve(sub1).expect("first reservation succeeds");
    assert_eq!(res1.submission_id(), sub1);

    let sub2 = GpuSubmissionId::next();
    let res2 = queue.reserve(sub2).expect("second reservation succeeds");
    assert_eq!(res2.submission_id(), sub2);

    // Third reservation exceeds bounded capacity and is rejected before driver encoding
    let sub3 = GpuSubmissionId::next();
    let err = queue.reserve(sub3).expect_err("queue full must be refused");
    assert_eq!(err, TerminalDrainError::QueueSaturated { capacity: 2 });

    // Commit the first two
    res1.commit().expect("commit 1");
    res2.commit().expect("commit 2");
    assert_eq!(queue.in_flight_count(), 2);

    let counters = queue.counters();
    assert_eq!(counters.reservations_created, 2);
    assert_eq!(counters.reservations_committed, 2);
    assert_eq!(counters.rejected_queue_full, 1);
    assert_eq!(counters.peak_in_flight, 2);
}

#[test]
fn coalesced_wakes_never_lose_completion_state() {
    let probe = Arc::new(WakeProbe::new(
        NonZeroUsize::new(10).expect("non-zero capacity"),
    ));
    let queue = LosslessTerminalDrainQueue::new(16)
        .expect("valid queue")
        .with_wake_probe(Arc::clone(&probe));

    let mut subs = Vec::new();
    for _ in 0..8 {
        let sid = GpuSubmissionId::next();
        let res = queue.reserve(sid).expect("reserve slot");
        res.commit().expect("commit");
        subs.push(sid);
    }
    assert_eq!(queue.in_flight_count(), 8);

    // Simulate 8 GPU hardware completions arriving from driver callbacks
    for (i, sid) in subs.iter().enumerate() {
        let status = if i % 2 == 0 {
            TerminalCompletionStatus::Success
        } else {
            TerminalCompletionStatus::Cancelled
        };
        queue.record_completion(*sid, status).expect("record completion");
    }

    let counters = queue.counters();
    assert_eq!(counters.completions_recorded, 8);
    // Wakes coalesced: 1 wake signaled, 7 coalesced
    assert_eq!(counters.wakes_signaled, 1);
    assert_eq!(counters.wakes_coalesced, 7);

    // Drain completes all 8 records without loss
    let report = queue.drain_completed();
    assert_eq!(report.completed_drained, 4);
    assert_eq!(report.cancelled_drained, 4);
    assert_eq!(report.remaining_in_flight, 0);

    let final_counters = queue.counters();
    assert_eq!(final_counters.records_drained, 8);
}

#[test]
fn retain_through_close_and_release_exactly_once() {
    let owner = arena_owner(42);
    let budget = ResourceBudget::new_with_protected_capacity(
        owner,
        ByteLength::new(1024),
        ByteLength::new(512),
        ByteLength::new(512),
    )
    .expect("budget");

    let lease1 = budget
        .try_reserve_terminal(owner, alloc_id(101), ResourceKind::Managed, ByteLength::new(128))
        .expect("reserve terminal lease 1");
    let lease2 = budget
        .try_reserve_terminal(owner, alloc_id(102), ResourceKind::Managed, ByteLength::new(256))
        .expect("reserve terminal lease 2");

    assert_eq!(budget.accounting().terminal_reserved().get(), 384);

    let queue = LosslessTerminalDrainQueue::new(4).expect("valid queue");
    let sub1 = GpuSubmissionId::next();
    let sub2 = GpuSubmissionId::next();

    let mut res1 = queue.reserve(sub1).expect("res 1");
    res1.attach_lease(lease1);
    res1.commit().expect("commit 1");

    let mut res2 = queue.reserve(sub2).expect("res 2");
    res2.attach_lease(lease2);
    res2.commit().expect("commit 2");

    // Window / session closes while submissions are still in flight on GPU
    queue.close();
    assert!(queue.is_closed());

    // Subsequent reservations are refused
    let sub3 = GpuSubmissionId::next();
    assert_eq!(queue.reserve(sub3).unwrap_err(), TerminalDrainError::Closed);

    // In-flight leases remain locked during close
    assert_eq!(budget.accounting().terminal_reserved().get(), 384);
    assert_eq!(queue.in_flight_count(), 2);

    // First GPU submission completes with error
    queue
        .record_completion(
            sub1,
            TerminalCompletionStatus::Error(GpuTerminalError::DeviceLost),
        )
        .expect("record sub1 completion");

    // Partial drain drains sub1 and releases its lease
    let rep1 = queue.drain_completed();
    assert_eq!(rep1.errors_drained, 1);
    assert_eq!(rep1.leases_released, 1);
    assert_eq!(rep1.remaining_in_flight, 1);
    assert_eq!(budget.accounting().terminal_reserved().get(), 256);

    // Second GPU submission completes successfully
    queue
        .record_completion(sub2, TerminalCompletionStatus::Success)
        .expect("record sub2 completion");

    // Final drain releases sub2 lease exactly once
    let rep2 = queue.drain_completed();
    assert_eq!(rep2.completed_drained, 1);
    assert_eq!(rep2.leases_released, 1);
    assert_eq!(rep2.remaining_in_flight, 0);
    assert_eq!(budget.accounting().terminal_reserved().get(), 0);

    let counters = queue.counters();
    assert_eq!(counters.leases_released_once, 2);
    assert_eq!(counters.records_drained, 2);
}

#[test]
fn ordinary_memory_and_event_saturation_cannot_starve_terminal_completion() {
    let owner = arena_owner(1);
    // 1024 bytes ordinary capacity, 512 bytes terminal, 512 bytes retirement (2048 total)
    let budget = ResourceBudget::new_with_protected_capacity(
        owner,
        ByteLength::new(2048),
        ByteLength::new(512),
        ByteLength::new(512),
    )
    .expect("budget");

    // 1. Saturate ordinary memory to 100% (1024 / 1024)
    let _ordinary_lease = budget
        .try_reserve(owner, alloc_id(9001), ResourceKind::Managed, ByteLength::new(1024))
        .expect("ordinary reserve");
    assert_eq!(budget.accounting().ordinary_available().get(), 0);

    // Ordinary allocation is now refused
    assert!(budget
        .try_reserve(owner, alloc_id(9002), ResourceKind::Managed, ByteLength::new(1))
        .is_err());

    // 2. Saturate ordinary event queue in WakeProbe
    let probe = Arc::new(WakeProbe::new(NonZeroUsize::new(2).expect("non-zero")));
    probe
        .publish_ordered(WakeCommand { id: 1, payload: 10 }, WakePriority::Normal)
        .expect("cmd 1");
    probe
        .publish_ordered(WakeCommand { id: 2, payload: 20 }, WakePriority::Normal)
        .expect("cmd 2");
    assert!(probe
        .publish_ordered(WakeCommand { id: 3, payload: 30 }, WakePriority::Normal)
        .is_err());

    // 3. Protected terminal completion progress survives!
    let queue = LosslessTerminalDrainQueue::new(4)
        .expect("queue")
        .with_wake_probe(Arc::clone(&probe));

    let term_lease = budget
        .try_reserve_terminal(owner, alloc_id(9010), ResourceKind::Managed, ByteLength::new(256))
        .expect("protected terminal reservation succeeds despite ordinary memory saturation");

    let sid = GpuSubmissionId::next();
    let mut res = queue.reserve(sid).expect("reserve terminal slot");
    res.attach_lease(term_lease);

    // Attach retired object to verify off-UI destruction
    static DROPPED: AtomicBool = AtomicBool::new(false);
    struct Dropper;
    impl Drop for Dropper {
        fn drop(&mut self) {
            DROPPED.store(true, Ordering::SeqCst);
        }
    }
    res.attach_retired_payload(Arc::new(Dropper));
    res.commit().expect("commit");

    // Complete the GPU submission
    queue
        .record_completion(sid, TerminalCompletionStatus::Success)
        .expect("completion recorded");

    // Off-UI retirement queue with protected retirement capacity
    let ret_q = BoundedRetirementQueue::new(owner, 4, Some(budget.clone())).expect("ret_q");
    let rep = queue.drain_completed_into_retirement(&ret_q);
    assert_eq!(rep.completed_drained, 1);
    assert_eq!(rep.leases_released, 1);
    assert_eq!(rep.payloads_retired, 1);

    // Destructor has not run on interaction thread yet (it was offloaded to retirement queue)
    assert!(!DROPPED.load(Ordering::SeqCst));

    // Drain retirement queue on maintenance thread
    let ret_rep = ret_q.drain_all().expect("drain_all");
    assert_eq!(ret_rep.items_destroyed, 1);
    assert!(DROPPED.load(Ordering::SeqCst));
}

#[test]
fn negative_control_demonstrates_defect_detection() {
    let queue = LosslessTerminalDrainQueue::new(2).expect("valid queue");
    let sid = GpuSubmissionId::next();

    // 1. Unknown submission completion fails
    let err = queue.record_completion(sid, TerminalCompletionStatus::Success);
    assert!(matches!(err, Err(TerminalDrainError::UnknownSubmission { .. })));

    // 2. Aborted reservation does not leave in-flight work or leak slots
    {
        let res = queue.reserve(sid).expect("reserve");
        // Drop without commit -> abort
        drop(res);
    }
    assert_eq!(queue.in_flight_count(), 0);
    assert_eq!(queue.counters().reservations_aborted, 1);

    // 3. Slot is reusable after abort
    let res2 = queue.reserve(sid).expect("reserve again");
    res2.commit().expect("commit succeeds");
    assert_eq!(queue.in_flight_count(), 1);

    // 4. Invariant: events ring records all transitions
    let events = queue.event_records();
    assert!(events.iter().any(|e| e.event_type == TerminalEventType::Aborted));
    assert!(events.iter().any(|e| e.event_type == TerminalEventType::Committed));
}

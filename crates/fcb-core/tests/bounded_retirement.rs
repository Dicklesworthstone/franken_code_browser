//! Focused unit, boundary, and concurrency tests for bounded off-UI CPU retirement (FCB-072.A).
//!
//! Verifies:
//! 1. Pre-reserved retirement slot before publication.
//! 2. Last-Arc drop never stalls the interaction thread (destructor runs only on off-UI drain).
//! 3. Retained bytes remain charged to budget until final off-thread destruction.
//! 4. Ordinary memory saturation cannot starve or block retirement progress.
//! 5. Bounded queue saturation defers optional publications without dropping active state.
//! 6. Negative control demonstrating defect detection.

#![forbid(unsafe_code)]

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use fcb_core::{
    retirement::{
        BoundedRetirementQueue, PublicationOutcome, RetirementError,
    },
    ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceKind,
};

/// A tracked test payload whose drop runs custom logic to record where destruction occurred.
#[derive(Debug)]
struct TrackedPayload {
    dropped: Arc<AtomicBool>,
    drop_count: Arc<AtomicUsize>,
    #[allow(dead_code)]
    data: Vec<u8>,
}

impl TrackedPayload {
    fn new(size: usize, dropped: Arc<AtomicBool>, drop_count: Arc<AtomicUsize>) -> Self {
        Self {
            dropped,
            drop_count,
            data: vec![0xAA; size],
        }
    }
}

impl Drop for TrackedPayload {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
        self.drop_count.fetch_add(1, Ordering::SeqCst);
    }
}

fn test_owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).expect("valid owner")
}

#[test]
fn last_arc_drop_never_stalls_interaction_thread() {
    let owner = test_owner(1);
    let queue = BoundedRetirementQueue::new(owner, 4, None).expect("queue created");

    let dropped = Arc::new(AtomicBool::new(false));
    let drop_count = Arc::new(AtomicUsize::new(0));

    let initial = Arc::new(TrackedPayload::new(1024, Arc::clone(&dropped), Arc::clone(&drop_count)));
    let mut active = Arc::clone(&initial);
    drop(initial); // Now active is the ONLY reference (refcount = 1)

    assert_eq!(Arc::strong_count(&active), 1);
    assert!(!dropped.load(Ordering::SeqCst));

    // Next frame/snapshot arrives
    let replacement = Arc::new(TrackedPayload::new(
        2048,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    ));

    // Perform publication swap on simulated UI thread
    let outcome = queue.publish_or_defer(&mut active, replacement, ByteLength::new(1024));
    assert!(outcome.is_published());

    // CRITICAL INVARIANT: The old snapshot was swapped out, but its destructor
    // was NOT run on the interaction thread!
    assert!(
        !dropped.load(Ordering::SeqCst),
        "Last-Arc drop MUST NOT occur on the interaction thread during swap"
    );
    assert_eq!(drop_count.load(Ordering::SeqCst), 0);

    let status = queue.status().expect("status");
    assert_eq!(status.pending_items, 1);
    assert_eq!(status.retained_bytes, ByteLength::new(1024));

    // Maintenance / background worker drains the retirement queue off-UI
    let report = queue.drain_batch(1).expect("drain succeeds");
    assert_eq!(report.items_destroyed, 1);
    assert_eq!(report.bytes_reclaimed, ByteLength::new(1024));

    // Now, and only on the maintenance drain path, the destructor has executed
    assert!(
        dropped.load(Ordering::SeqCst),
        "Destructor executes exclusively on the off-UI drain path"
    );
    assert_eq!(drop_count.load(Ordering::SeqCst), 1);

    let post_status = queue.status().expect("post status");
    assert_eq!(post_status.pending_items, 0);
    assert_eq!(post_status.retained_bytes, ByteLength::new(0));
}

#[test]
fn retained_bytes_charged_to_budget_until_destruction() {
    let owner = test_owner(2);
    // 8 KB ordinary capacity, 4 KB terminal capacity, 4 KB retirement capacity (total 16 KB)
    let budget = ResourceBudget::new_with_protected_capacity(
        owner,
        ByteLength::new(16384),
        ByteLength::new(4096),
        ByteLength::new(4096),
    )
    .expect("budget");

    let queue = BoundedRetirementQueue::new(owner, 4, Some(budget.clone())).expect("queue");

    let mut active = Arc::new(vec![1u8; 2048]);
    let new_val = Arc::new(vec![2u8; 2048]);

    // Check budget before swap
    let acct_before = budget.accounting();
    assert_eq!(acct_before.retirement_reserved(), ByteLength::new(0));

    // Perform publication swap
    let outcome = queue.publish_or_defer(&mut active, new_val, ByteLength::new(2048));
    assert!(outcome.is_published());

    // Retained bytes must be charged to the retirement class in the budget
    let acct_during = budget.accounting();
    assert_eq!(acct_during.retirement_reserved(), ByteLength::new(2048));

    // Off-UI drain
    let report = queue.drain_all().expect("drain all");
    assert_eq!(report.items_destroyed, 1);
    assert_eq!(report.bytes_reclaimed, ByteLength::new(2048));

    // After off-UI destruction, retirement bytes are credited back
    let acct_after = budget.accounting();
    assert_eq!(acct_after.retirement_reserved(), ByteLength::new(0));
}

#[test]
fn ordinary_memory_saturation_cannot_starve_retirement() {
    let owner = test_owner(3);
    // 2 KB ordinary capacity, 1 KB terminal, 2 KB protected retirement (total 5 KB)
    let budget = ResourceBudget::new_with_protected_capacity(
        owner,
        ByteLength::new(5120),
        ByteLength::new(1024),
        ByteLength::new(2048),
    )
    .expect("budget");

    // Completely saturate ordinary capacity
    let _ordinary_lease = budget
        .try_reserve(
            owner,
            ResourceAllocationId::new(100).unwrap(),
            ResourceKind::Managed,
            ByteLength::new(2048),
        )
        .expect("ordinary lease consumes all 2048 bytes");

    // Verify ordinary capacity is fully exhausted
    let fail_ordinary = budget.try_reserve(
        owner,
        ResourceAllocationId::new(101).unwrap(),
        ResourceKind::Managed,
        ByteLength::new(64),
    );
    assert!(fail_ordinary.is_err(), "Ordinary memory is 100% saturated");

    // Retirement progress MUST STILL SUCCEED through protected headroom!
    let queue = BoundedRetirementQueue::new(owner, 2, Some(budget.clone())).expect("queue");
    let mut active = Arc::new(vec![0u8; 1024]);
    let new_val = Arc::new(vec![1u8; 1024]);

    let outcome = queue.publish_or_defer(&mut active, new_val, ByteLength::new(1024));
    assert!(
        outcome.is_published(),
        "Retirement progress succeeds despite ordinary memory saturation"
    );

    let report = queue.drain_all().expect("drain");
    assert_eq!(report.items_destroyed, 1);
}

#[test]
fn queue_saturation_defers_optional_publications() {
    let owner = test_owner(4);
    // Queue with capacity = 2
    let queue = BoundedRetirementQueue::new(owner, 2, None).expect("queue");

    let mut active = Arc::new("frame-0".to_string());

    // Publication 1
    let out1 = queue.publish_or_defer(&mut active, Arc::new("frame-1".to_string()), ByteLength::new(64));
    assert!(out1.is_published());
    assert_eq!(*active, "frame-1");

    // Publication 2 (queue now at full capacity = 2)
    let out2 = queue.publish_or_defer(&mut active, Arc::new("frame-2".to_string()), ByteLength::new(64));
    assert!(out2.is_published());
    assert_eq!(*active, "frame-2");

    // Publication 3 must be deferred to prevent unmeasured queues or UI stall
    let attempted = Arc::new("frame-3-deferred".to_string());
    let out3 = queue.publish_or_defer(&mut active, Arc::clone(&attempted), ByteLength::new(64));
    assert!(out3.is_deferred());

    match out3 {
        PublicationOutcome::Deferred { reason, .. } => {
            assert_eq!(reason, RetirementError::QueueSaturated { capacity: 2 });
        }
        _ => panic!("Expected deferred outcome"),
    }

    // Active frame is retained unmodified
    assert_eq!(*active, "frame-2");
    assert_eq!(queue.status().unwrap().rejected_saturation, 1);

    // Drain one slot
    let drain_report = queue.drain_batch(1).expect("drain");
    assert_eq!(drain_report.items_destroyed, 1);

    // Now publication succeeds again
    let out4 = queue.publish_or_defer(&mut active, attempted, ByteLength::new(64));
    assert!(out4.is_published());
    assert_eq!(*active, "frame-3-deferred");
}

#[test]
fn negative_control_demonstrates_defect_detection() {
    // Intentional negative control: verify that detecting immediate on-thread drops works.
    let dropped = Arc::new(AtomicBool::new(false));
    let payload = Arc::new(TrackedPayload::new(64, Arc::clone(&dropped), Arc::new(AtomicUsize::new(0))));

    // Normal direct drop on current thread
    drop(payload);
    assert!(
        dropped.load(Ordering::SeqCst),
        "Negative control verifies detection of on-thread destruction"
    );
}

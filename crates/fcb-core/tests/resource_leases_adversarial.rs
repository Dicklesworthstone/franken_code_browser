use fcb_core::{
    ArenaOwnerId, ByteLength, CoreError, ResourceAllocationId, ResourceBudget, ResourceKind,
};

fn owner(value: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(value).unwrap()
}

fn allocation(value: u64) -> ResourceAllocationId {
    ResourceAllocationId::new(value).unwrap()
}

#[test]
fn competing_owners_share_one_capacity_domain_and_reclaim_it() {
    let budget = ResourceBudget::new(owner(100), ByteLength::new(10)).unwrap();
    let managed = budget
        .try_reserve_managed(owner(101), allocation(1), ByteLength::new(6))
        .unwrap();
    let queue = budget
        .try_reserve_queue_bytes(owner(102), allocation(2), ByteLength::new(4))
        .unwrap();

    let saturated = budget.accounting();
    assert_eq!(saturated.reserved().get(), 10);
    assert_eq!(saturated.managed().get(), 6);
    assert_eq!(saturated.queue().get(), 4);
    assert_eq!(saturated.available().get(), 0);
    assert_eq!(saturated.active_allocations(), 2);
    assert_eq!(saturated.active_leases(), 2);

    assert!(matches!(
        budget.try_reserve_managed(owner(103), allocation(3), ByteLength::new(1)),
        Err(CoreError::LimitExceeded)
    ));
    assert_eq!(budget.accounting(), saturated);

    drop(managed);
    let after_reclaim = budget.accounting();
    assert_eq!(after_reclaim.reserved().get(), 4);
    assert_eq!(after_reclaim.managed().get(), 0);
    assert_eq!(after_reclaim.queue().get(), 4);
    assert_eq!(after_reclaim.available().get(), 6);

    let replacement = budget
        .try_reserve_managed(owner(103), allocation(3), ByteLength::new(6))
        .unwrap();
    assert_eq!(budget.accounting().reserved().get(), 10);
    drop(queue);
    assert_eq!(budget.accounting().reserved().get(), 6);
    drop(replacement);
    assert_eq!(budget.accounting().reserved().get(), 0);
    assert_eq!(budget.accounting().active_allocations(), 0);
}

#[test]
fn duplicate_raw_allocation_id_is_rejected_without_sharing() {
    let budget = ResourceBudget::new(owner(110), ByteLength::new(10)).unwrap();
    let original = budget
        .try_reserve_managed(owner(111), allocation(11), ByteLength::new(7))
        .unwrap();
    let before = budget.accounting();

    assert!(matches!(
        budget.try_reserve_managed(owner(112), allocation(11), ByteLength::new(7)),
        Err(CoreError::OwnershipMismatch)
    ));
    assert!(matches!(
        budget.try_reserve_managed(owner(113), allocation(11), ByteLength::new(6)),
        Err(CoreError::OwnershipMismatch)
    ));
    assert!(matches!(
        budget.try_reserve_queue_bytes(owner(114), allocation(11), ByteLength::new(7)),
        Err(CoreError::OwnershipMismatch)
    ));
    assert_eq!(budget.accounting(), before);
    assert_eq!(original.info().owner(), owner(111));
    drop(original);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn validated_lease_capability_shares_once_and_survives_budget_drop() {
    let budget = ResourceBudget::new(owner(115), ByteLength::new(10)).unwrap();
    let other_budget = ResourceBudget::new(owner(115), ByteLength::new(10)).unwrap();
    let original = budget
        .try_reserve_managed(owner(116), allocation(16), ByteLength::new(7))
        .unwrap();
    let shared = budget.try_share(owner(117), &original).unwrap();

    let sharing = original.accounting();
    assert_eq!(sharing.reserved().get(), 7);
    assert_eq!(sharing.managed().get(), 7);
    assert_eq!(sharing.active_allocations(), 1);
    assert_eq!(sharing.active_leases(), 2);
    assert_eq!(shared.info().owner(), owner(117));
    assert_eq!(shared.info().allocation(), allocation(16));

    assert!(matches!(
        other_budget.try_share(owner(118), &original),
        Err(CoreError::OwnershipMismatch)
    ));
    assert_eq!(other_budget.accounting().reserved().get(), 0);

    drop(budget);
    assert_eq!(original.accounting().reserved().get(), 7);
    drop(original);
    assert_eq!(shared.accounting().reserved().get(), 7);
    assert_eq!(shared.accounting().active_leases(), 1);
    shared.release();
    assert_eq!(other_budget.accounting().reserved().get(), 0);
}

#[test]
fn old_new_overlap_is_charged_until_the_old_generation_releases() {
    let budget = ResourceBudget::new(owner(120), ByteLength::new(10)).unwrap();
    let old = budget
        .try_reserve_managed(owner(121), allocation(21), ByteLength::new(6))
        .unwrap();
    let before_new = budget.accounting();
    let new = budget
        .try_reserve_queue_bytes(owner(122), allocation(22), ByteLength::new(4))
        .unwrap();

    let overlap = budget.accounting();
    assert_eq!(overlap.reserved().get(), 10);
    assert_eq!(overlap.peak_reserved().get(), 10);
    assert_eq!(overlap.active_allocations(), 2);
    assert_eq!(overlap.active_leases(), 2);
    assert!(overlap.reserved().get() > before_new.reserved().get());

    assert!(matches!(
        budget.try_reserve_managed(owner(123), allocation(23), ByteLength::new(1)),
        Err(CoreError::LimitExceeded)
    ));
    assert_eq!(budget.accounting(), overlap);

    drop(old);
    assert_eq!(budget.accounting().reserved().get(), 4);
    let replacement = budget
        .try_reserve_managed(owner(123), allocation(23), ByteLength::new(6))
        .unwrap();
    assert_eq!(budget.accounting().reserved().get(), 10);
    assert_eq!(budget.accounting().peak_reserved().get(), 10);

    drop(new);
    drop(replacement);
    assert_eq!(budget.accounting().reserved().get(), 0);
    assert_eq!(budget.accounting().active_leases(), 0);
}

#[test]
fn failed_reservations_leave_all_conservation_counters_unchanged() {
    let budget = ResourceBudget::new(owner(130), ByteLength::new(8)).unwrap();
    let existing = budget
        .try_reserve_managed(owner(131), allocation(31), ByteLength::new(5))
        .unwrap();
    let before = budget.accounting();

    assert!(matches!(
        budget.try_reserve_queue_bytes(owner(132), allocation(32), ByteLength::new(4)),
        Err(CoreError::LimitExceeded)
    ));
    assert!(matches!(
        budget.try_reserve_managed(owner(132), allocation(31), ByteLength::new(4)),
        Err(CoreError::OwnershipMismatch)
    ));
    assert!(matches!(
        budget.try_reserve_managed(owner(132), allocation(33), ByteLength::new(0)),
        Err(CoreError::InvalidId)
    ));
    assert_eq!(budget.accounting(), before);
    assert_eq!(existing.info().allocation().get(), 31);
    assert_eq!(existing.info().kind(), ResourceKind::Managed);

    drop(existing);
    assert_eq!(budget.accounting().reserved().get(), 0);
    assert_eq!(budget.accounting().active_allocations(), 0);
}

#[test]
fn reclamation_progresses_after_saturation_and_final_clone_release() {
    let budget = ResourceBudget::new(owner(140), ByteLength::new(10)).unwrap();
    let retained = budget
        .try_reserve_managed(owner(141), allocation(41), ByteLength::new(8))
        .unwrap();
    let completion = budget
        .try_reserve_queue_bytes(owner(142), allocation(42), ByteLength::new(2))
        .unwrap();
    let held_completion = completion.clone();

    let saturated = budget.accounting();
    assert_eq!(saturated.reserved().get(), 10);
    assert_eq!(saturated.available().get(), 0);
    assert_eq!(saturated.active_allocations(), 2);
    assert_eq!(saturated.active_leases(), 2);
    assert!(matches!(
        budget.try_reserve_managed(owner(143), allocation(43), ByteLength::new(1)),
        Err(CoreError::LimitExceeded)
    ));
    assert_eq!(budget.accounting(), saturated);

    drop(completion);
    assert_eq!(budget.accounting().reserved().get(), 10);
    drop(held_completion);
    let reclaimed = budget.accounting();
    assert_eq!(reclaimed.reserved().get(), 8);
    assert_eq!(reclaimed.managed().get(), 8);
    assert_eq!(reclaimed.queue().get(), 0);
    assert_eq!(reclaimed.available().get(), 2);

    let next_completion = budget
        .try_reserve_queue_bytes(owner(143), allocation(43), ByteLength::new(2))
        .unwrap();
    assert_eq!(budget.accounting().reserved().get(), 10);
    drop(retained);
    assert_eq!(budget.accounting().reserved().get(), 2);
    drop(next_completion);
    assert_eq!(budget.accounting().reserved().get(), 0);
    assert_eq!(budget.accounting().active_leases(), 0);
}

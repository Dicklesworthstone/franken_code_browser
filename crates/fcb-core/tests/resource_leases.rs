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
fn managed_and_queue_leases_share_one_capacity_ceiling() {
    let budget = ResourceBudget::new(owner(1), ByteLength::new(10)).unwrap();
    let managed = budget
        .try_reserve_managed(owner(2), allocation(1), ByteLength::new(6))
        .unwrap();
    let queue = budget
        .try_reserve_queue_bytes(owner(3), allocation(2), ByteLength::new(4))
        .unwrap();

    let accounting = budget.accounting();
    assert_eq!(accounting.capacity().get(), 10);
    assert_eq!(accounting.reserved().get(), 10);
    assert_eq!(accounting.managed().get(), 6);
    assert_eq!(accounting.queue().get(), 4);
    assert_eq!(accounting.available().get(), 0);
    assert_eq!(accounting.active_allocations(), 2);
    assert_eq!(accounting.active_leases(), 2);
    assert_eq!(accounting.peak_reserved().get(), 10);
    assert_eq!(managed.info().kind(), ResourceKind::Managed);
    assert_eq!(queue.info().kind(), ResourceKind::Queue);
    assert_eq!(managed.info().domain(), owner(1));
    assert_eq!(queue.info().owner(), owner(3));

    drop(queue);
    drop(managed);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn shared_allocation_is_charged_once_until_last_owner_releases() {
    let budget = ResourceBudget::new(owner(10), ByteLength::new(12)).unwrap();
    let first = budget
        .try_reserve(
            owner(11),
            allocation(22),
            ResourceKind::Managed,
            ByteLength::new(9),
        )
        .unwrap();
    let second = budget
        .try_share(owner(12), &first)
        .unwrap();

    let accounting = budget.accounting();
    assert_eq!(accounting.reserved().get(), 9);
    assert_eq!(accounting.managed().get(), 9);
    assert_eq!(accounting.active_allocations(), 1);
    assert_eq!(accounting.active_leases(), 2);

    drop(first);
    assert_eq!(budget.accounting().reserved().get(), 9);
    assert_eq!(budget.accounting().active_leases(), 1);

    drop(second);
    let accounting = budget.accounting();
    assert_eq!(accounting.reserved().get(), 0);
    assert_eq!(accounting.managed().get(), 0);
    assert_eq!(accounting.active_allocations(), 0);
    assert_eq!(accounting.active_leases(), 0);
    assert_eq!(accounting.peak_reserved().get(), 9);
}

#[test]
fn cloned_lease_does_not_release_shared_capacity_early() {
    let budget = ResourceBudget::new(owner(20), ByteLength::new(8)).unwrap();
    let lease = budget
        .try_reserve_managed(owner(21), allocation(23), ByteLength::new(5))
        .unwrap();
    let retained = lease.clone();

    drop(lease);
    assert_eq!(budget.accounting().reserved().get(), 5);
    assert_eq!(budget.accounting().active_leases(), 1);

    retained.release();
    assert_eq!(budget.accounting().reserved().get(), 0);
    assert_eq!(budget.accounting().active_leases(), 0);
}

#[test]
fn raw_allocation_id_collision_is_not_implicit_sharing() {
    let budget = ResourceBudget::new(owner(25), ByteLength::new(12)).unwrap();
    let first = budget
        .try_reserve_managed(owner(26), allocation(27), ByteLength::new(8))
        .unwrap();
    let before = budget.accounting();

    assert!(matches!(
        budget.try_reserve_managed(owner(28), allocation(27), ByteLength::new(8)),
        Err(CoreError::OwnershipMismatch)
    ));
    assert_eq!(budget.accounting(), before);

    drop(first);
}

#[test]
fn sharing_requires_the_receiving_budget_ledger_not_equal_domain_ids() {
    let source_budget = ResourceBudget::new(owner(35), ByteLength::new(10)).unwrap();
    let receiving_budget = ResourceBudget::new(owner(35), ByteLength::new(10)).unwrap();
    let source = source_budget
        .try_reserve_managed(owner(36), allocation(37), ByteLength::new(5))
        .unwrap();

    assert!(matches!(
        receiving_budget.try_share(owner(38), &source),
        Err(CoreError::OwnershipMismatch)
    ));
    assert_eq!(source_budget.accounting().reserved().get(), 5);
    assert_eq!(receiving_budget.accounting().reserved().get(), 0);
    drop(source);
}

#[test]
fn old_new_overlap_is_checked_before_mutating_accounting() {
    let budget = ResourceBudget::new(owner(30), ByteLength::new(10)).unwrap();
    let old = budget
        .try_reserve_managed(owner(31), allocation(31), ByteLength::new(6))
        .unwrap();
    let before = budget.accounting();

    let result = budget.try_reserve_queue_bytes(owner(32), allocation(32), ByteLength::new(5));
    assert!(matches!(result, Err(CoreError::LimitExceeded)));
    assert_eq!(budget.accounting(), before);
    assert_eq!(old.info().bytes().get(), 6);

    drop(old);
    let replacement = budget
        .try_reserve_queue_bytes(owner(32), allocation(32), ByteLength::new(5))
        .unwrap();
    assert_eq!(budget.accounting().reserved().get(), 5);
    drop(replacement);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn mismatched_shared_key_is_rejected_without_changing_conservation() {
    let budget = ResourceBudget::new(owner(40), ByteLength::new(20)).unwrap();
    let original = budget
        .try_reserve_managed(owner(41), allocation(41), ByteLength::new(7))
        .unwrap();
    let before = budget.accounting();

    assert!(matches!(
        budget.try_reserve_queue_bytes(owner(42), allocation(41), ByteLength::new(7)),
        Err(CoreError::OwnershipMismatch)
    ));
    assert!(matches!(
        budget.try_reserve_managed(owner(42), allocation(41), ByteLength::new(8)),
        Err(CoreError::OwnershipMismatch)
    ));
    assert_eq!(budget.accounting(), before);
    drop(original);
}

#[test]
fn invalid_zero_capacity_and_zero_reservations_are_refused() {
    assert!(matches!(
        ResourceBudget::new(owner(50), ByteLength::new(0)),
        Err(CoreError::InvalidId)
    ));
    let budget = ResourceBudget::new(owner(50), ByteLength::new(1)).unwrap();
    assert!(matches!(
        budget.try_reserve_managed(owner(51), allocation(51), ByteLength::new(0)),
        Err(CoreError::InvalidId)
    ));
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn budget_clone_shares_domain_but_drops_are_owner_scoped() {
    let budget = ResourceBudget::new(owner(60), ByteLength::new(10)).unwrap();
    let other_handle = budget.clone();
    let first = budget
        .try_reserve_managed(owner(61), allocation(61), ByteLength::new(4))
        .unwrap();
    let second = other_handle
        .try_reserve_queue_bytes(owner(62), allocation(62), ByteLength::new(3))
        .unwrap();

    drop(first);
    assert_eq!(other_handle.accounting().reserved().get(), 3);
    assert_eq!(other_handle.accounting().managed().get(), 0);
    assert_eq!(other_handle.accounting().queue().get(), 3);
    drop(second);
    assert_eq!(other_handle.accounting().reserved().get(), 0);
}

#[test]
fn lease_keeps_authoritative_accounting_alive_after_budget_drop() {
    let budget = ResourceBudget::new(owner(70), ByteLength::new(10)).unwrap();
    let lease = budget
        .try_reserve_queue_bytes(owner(71), allocation(71), ByteLength::new(6))
        .unwrap();

    drop(budget);
    assert_eq!(lease.accounting().reserved().get(), 6);
    assert_eq!(lease.accounting().queue().get(), 6);
    lease.release();
}

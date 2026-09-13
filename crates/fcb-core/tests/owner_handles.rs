use fcb_core::{ArenaOwnerId, CoreError, DeviceGeneration, DeviceId};
use fcb_core::handles::{ArenaTable, DeviceTable, HandleLimits};

#[test]
fn equal_slot_and_generation_cannot_cross_arena_owners() {
    let owner_a = ArenaOwnerId::new(101).unwrap();
    let owner_b = ArenaOwnerId::new(202).unwrap();
    let mut arena_a = ArenaTable::new(owner_a);
    let mut arena_b = ArenaTable::new(owner_b);

    let handle_a = arena_a.insert("a").unwrap();
    let handle_b = arena_b.insert("b").unwrap();

    assert_eq!((handle_a.slot(), handle_a.generation()), (0, 1));
    assert_eq!((handle_b.slot(), handle_b.generation()), (0, 1));
    assert_eq!(arena_a.lookup(handle_b), Err(CoreError::OwnershipMismatch));
    assert_eq!(arena_b.lookup(handle_a), Err(CoreError::OwnershipMismatch));
    assert_eq!(arena_a.lookup(handle_a), Ok(&"a"));
}

#[test]
fn arena_reuse_changes_generation_and_rejects_stale_handle() {
    let owner = ArenaOwnerId::new(303).unwrap();
    let mut arena = ArenaTable::new(owner);
    let first = arena.insert(7_u32).unwrap();
    assert_eq!(arena.remove(first), Ok(7));

    let second = arena.insert(8_u32).unwrap();
    assert_eq!(first.slot(), second.slot());
    assert_eq!(first.generation() + 1, second.generation());
    assert_eq!(arena.lookup(first), Err(CoreError::StalePublication));
    assert_eq!(arena.lookup(second), Ok(&8));
}

#[test]
fn equal_slot_and_generation_cannot_cross_device_authority() {
    let owner = ArenaOwnerId::new(404).unwrap();
    let device_a = DeviceId::new(owner, 1).unwrap();
    let device_b = DeviceId::new(owner, 2).unwrap();
    let generation = DeviceGeneration::new(owner, 1).unwrap();
    let mut table_a = DeviceTable::new(owner, device_a, generation).unwrap();
    let mut table_b = DeviceTable::new(owner, device_b, generation).unwrap();

    let handle_a = table_a.insert("device-a").unwrap();
    let handle_b = table_b.insert("device-b").unwrap();

    assert_eq!((handle_a.slot(), handle_a.generation()), (0, 1));
    assert_eq!((handle_b.slot(), handle_b.generation()), (0, 1));
    assert_eq!(table_a.lookup(handle_b), Err(CoreError::OwnershipMismatch));
    assert_eq!(table_b.lookup(handle_a), Err(CoreError::OwnershipMismatch));
}

#[test]
fn stale_device_generation_is_rejected_even_for_same_device() {
    let owner = ArenaOwnerId::new(505).unwrap();
    let device = DeviceId::new(owner, 1).unwrap();
    let old_generation = DeviceGeneration::new(owner, 1).unwrap();
    let new_generation = DeviceGeneration::new(owner, 2).unwrap();
    let mut old_table = DeviceTable::new(owner, device, old_generation).unwrap();
    let handle = old_table.insert("old").unwrap();
    let new_table = DeviceTable::<&str>::new(owner, device, new_generation).unwrap();

    assert_eq!(new_table.lookup(handle), Err(CoreError::StalePublication));
    assert_eq!(old_table.lookup(handle), Ok(&"old"));
}

#[test]
fn exhausted_generation_retires_slot_without_resurrection() {
    let owner = ArenaOwnerId::new(606).unwrap();
    let limits = HandleLimits::new(1, 2).unwrap();
    let mut arena = ArenaTable::with_limits(owner, limits);

    let first = arena.insert("first").unwrap();
    assert_eq!(arena.remove(first), Ok("first"));
    let second = arena.insert("second").unwrap();
    assert_eq!(arena.remove(second), Ok("second"));

    assert_eq!(arena.insert("must-not-resurrect"), Err(CoreError::Exhausted));
    assert_eq!(arena.lookup(first), Err(CoreError::StalePublication));
    assert_eq!(arena.lookup(second), Err(CoreError::StalePublication));
    assert!(arena.is_empty());
}

#[test]
fn malformed_and_mismatched_device_handles_fail_at_construction() {
    let owner = ArenaOwnerId::new(707).unwrap();
    let other_owner = ArenaOwnerId::new(808).unwrap();
    let device = DeviceId::new(owner, 1).unwrap();
    let foreign_generation = DeviceGeneration::new(other_owner, 1).unwrap();

    assert_eq!(HandleLimits::new(0, 1), Err(CoreError::InvalidId));
    assert_eq!(HandleLimits::new(1, 0), Err(CoreError::InvalidId));
    assert_eq!(
        fcb_core::handles::ArenaHandle::new(owner, 0, 0),
        Err(CoreError::InvalidId)
    );
    assert_eq!(
        fcb_core::handles::DeviceHandle::new(owner, device, foreign_generation, 0, 1),
        Err(CoreError::OwnershipMismatch)
    );
}

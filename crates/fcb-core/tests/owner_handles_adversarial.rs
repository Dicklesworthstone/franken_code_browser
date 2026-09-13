use fcb_core::{ArenaOwnerId, CoreError, DeviceGeneration, DeviceId};
use fcb_core::handles::{ArenaTable, DeviceTable, HandleLimits};

#[test]
fn equal_slot_generation_from_two_same_owner_tables_cannot_alias() {
    let owner = ArenaOwnerId::new(11).unwrap();
    let mut first = ArenaTable::new(owner);
    let mut second = ArenaTable::new(owner);

    let first_handle = first.insert("first").unwrap();
    let second_handle = second.insert("second").unwrap();
    assert_eq!(first_handle.slot(), second_handle.slot());
    assert_eq!(first_handle.generation(), second_handle.generation());
    assert_ne!(first_handle, second_handle);

    assert_eq!(first.lookup(first_handle), Ok(&"first"));
    assert_eq!(second.lookup(second_handle), Ok(&"second"));
    assert_eq!(second.lookup(first_handle), Err(CoreError::OwnershipMismatch));
}

#[test]
fn dropping_and_recreating_same_owner_table_does_not_resurrect_handle() {
    let owner = ArenaOwnerId::new(12).unwrap();
    let retired_handle = {
        let mut original = ArenaTable::new(owner);
        original.insert(17).unwrap()
    };

    let mut recreated = ArenaTable::new(owner);
    let recreated_handle = recreated.insert(23).unwrap();
    assert_ne!(retired_handle, recreated_handle);
    assert_eq!(recreated.lookup(retired_handle), Err(CoreError::OwnershipMismatch));
    assert_eq!(recreated.lookup(recreated_handle), Ok(&23));
}

#[test]
fn equal_slot_generation_from_two_same_device_tables_cannot_alias() {
    let owner = ArenaOwnerId::new(13).unwrap();
    let device = DeviceId::new(owner, 7).unwrap();
    let device_generation = DeviceGeneration::new(owner, 1).unwrap();
    let mut first = DeviceTable::new(owner, device, device_generation).unwrap();
    let mut second = DeviceTable::new(owner, device, device_generation).unwrap();

    let first_handle = first.insert("first-device").unwrap();
    let second_handle = second.insert("second-device").unwrap();
    assert_eq!(first_handle.slot(), second_handle.slot());
    assert_eq!(first_handle.generation(), second_handle.generation());
    assert_ne!(first_handle, second_handle);

    assert_eq!(first.lookup(first_handle), Ok(&"first-device"));
    assert_eq!(second.lookup(second_handle), Ok(&"second-device"));
    assert_eq!(second.lookup(first_handle), Err(CoreError::OwnershipMismatch));
}

#[test]
fn device_lookup_rejects_foreign_device_and_stale_device_generation() {
    let owner = ArenaOwnerId::new(14).unwrap();
    let device = DeviceId::new(owner, 8).unwrap();
    let other_device = DeviceId::new(owner, 9).unwrap();
    let generation = DeviceGeneration::new(owner, 3).unwrap();
    let stale_generation = DeviceGeneration::new(owner, 2).unwrap();
    let mut table = DeviceTable::new(owner, device, generation).unwrap();
    let handle = table.insert(99).unwrap();

    let foreign_device: DeviceTable<u32> = DeviceTable::new(owner, other_device, generation).unwrap();
    assert_eq!(foreign_device.validate(handle), Err(CoreError::OwnershipMismatch));
    let stale_handle = fcb_core::handles::DeviceHandle::new(
        owner,
        device,
        stale_generation,
        handle.slot(),
        handle.generation(),
    )
    .unwrap();
    assert_eq!(table.validate(stale_handle), Err(CoreError::StalePublication));
    assert_eq!(table.lookup(handle), Ok(&99));
}

#[test]
fn rejected_stale_operations_do_not_mutate_table_state() {
    let owner = ArenaOwnerId::new(15).unwrap();
    let mut table = ArenaTable::with_limits(owner, HandleLimits::new(2, 3).unwrap());
    let first = table.insert(31).unwrap();
    assert_eq!(table.remove(first), Ok(31));
    let current = table.insert(47).unwrap();
    assert_eq!(current.slot(), first.slot());
    assert_ne!(current.generation(), first.generation());

    let length_before_rejections = table.len();
    assert_eq!(table.lookup(first), Err(CoreError::StalePublication));
    assert_eq!(table.lookup_mut(first), Err(CoreError::StalePublication));
    assert_eq!(table.remove(first), Err(CoreError::StalePublication));
    assert_eq!(table.len(), length_before_rejections);
    assert_eq!(table.lookup(current), Ok(&47));
}

#[test]
fn generation_limit_retires_slot_without_resurrection() {
    let owner = ArenaOwnerId::new(16).unwrap();
    let limits = HandleLimits::new(1, 1).unwrap();
    let mut table = ArenaTable::with_limits(owner, limits);
    let first = table.insert(61).unwrap();
    assert_eq!(table.remove(first), Ok(61));

    assert_eq!(table.insert(73), Err(CoreError::Exhausted));
    assert_eq!(table.lookup(first), Err(CoreError::StalePublication));
    assert!(table.is_empty());
    assert_eq!(table.insert(89), Err(CoreError::Exhausted));
    assert!(table.is_empty());
}

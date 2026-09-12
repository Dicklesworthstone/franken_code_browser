use fcb_core::{
    ArenaOwnerId, CoreError, FileId, IdAllocator, ImmutableSnapshot, SnapshotCell, SourceRevision,
};

#[test]
fn independent_allocators_are_owner_qualified() {
    let owner_a = ArenaOwnerId::new(101).unwrap();
    let owner_b = ArenaOwnerId::new(202).unwrap();
    let mut allocator_a = IdAllocator::<FileId>::new(owner_a, 1).unwrap();
    let mut allocator_b = IdAllocator::<FileId>::new(owner_b, 1).unwrap();
    assert_ne!(allocator_a.allocate().unwrap(), allocator_b.allocate().unwrap());
}

#[test]
fn snapshot_cell_rejects_wrong_owner_and_stale_revision() {
    let owner = ArenaOwnerId::new(9).unwrap();
    let other_owner = ArenaOwnerId::new(10).unwrap();
    let newer = SourceRevision::new(owner, 2).unwrap();
    let older = SourceRevision::new(owner, 1).unwrap();
    let foreign_revision = SourceRevision::new(other_owner, 3).unwrap();
    let mut cell = SnapshotCell::new(owner);
    cell.publish(ImmutableSnapshot::new(owner, newer, "new").unwrap())
        .unwrap();
    assert_eq!(
        cell.publish(ImmutableSnapshot::new(owner, older, "old").unwrap()),
        Err(CoreError::StalePublication)
    );
    assert_eq!(
        cell.publish(ImmutableSnapshot::new(other_owner, foreign_revision, "foreign").unwrap()),
        Err(CoreError::OwnershipMismatch)
    );
    assert_eq!(cell.head().unwrap().get(), &"new");
}

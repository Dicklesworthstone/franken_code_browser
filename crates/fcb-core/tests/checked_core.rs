use fcb_core::{
    ArenaOwnerId, CoreError, DisplayGeneration, FileId, IdAllocator, ImmutableSnapshot,
    PublicationContext, PublicationToken, QueryGeneration, SnapshotCell, SnapshotDelta,
    SourceRevision,
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

#[test]
fn token_gates_snapshot_publication_against_current_context() {
    let owner = ArenaOwnerId::new(77).unwrap();
    let request = QueryGeneration::new(owner, 1).unwrap();
    let source = SourceRevision::new(owner, 2).unwrap();
    let display = DisplayGeneration::new(owner, 3).unwrap();
    let context = PublicationContext::new(owner, request, source, display).unwrap();
    let token = PublicationToken::new(context);
    let snapshot = ImmutableSnapshot::new(owner, source, "published").unwrap();
    let mut cell = SnapshotCell::new(owner);
    cell.publish_if_current(token, context, snapshot).unwrap();
    assert_eq!(cell.head().unwrap().get(), &"published");
}

#[test]
fn token_rejects_foreign_owner_and_delta_rejects_stale_base() {
    let owner = ArenaOwnerId::new(88).unwrap();
    let other_owner = ArenaOwnerId::new(89).unwrap();
    let request = QueryGeneration::new(owner, 1).unwrap();
    let source = SourceRevision::new(owner, 1).unwrap();
    let display = DisplayGeneration::new(owner, 1).unwrap();
    let foreign = PublicationContext::new(
        other_owner,
        QueryGeneration::new(other_owner, 1).unwrap(),
        SourceRevision::new(other_owner, 1).unwrap(),
        DisplayGeneration::new(other_owner, 1).unwrap(),
    )
    .unwrap();
    let token = PublicationToken::new(PublicationContext::new(owner, request, source, display).unwrap());
    assert_eq!(token.validate_against(foreign), Err(CoreError::OwnershipMismatch));
    assert!(matches!(
        SnapshotDelta::<u8>::new(owner, source, source, 1),
        Err(CoreError::StalePublication)
    ));
}

use fcb_core::{
    ArenaOwnerId, CoreError, DisplayGeneration, FileId, IdAllocator, ImmutableSnapshot,
    PersistedIdAuthority, PublicationContext, PublicationToken, QueryGeneration, SnapshotCell,
    SnapshotDelta, SourceRevision,
};

#[test]
fn independent_allocators_are_owner_qualified() {
    let owner_a = ArenaOwnerId::new(101).unwrap();
    let owner_b = ArenaOwnerId::new(202).unwrap();
    let mut allocator_a = IdAllocator::<FileId>::new(owner_a, 1).unwrap();
    let mut allocator_b = IdAllocator::<FileId>::new(owner_b, 1).unwrap();
    let file_a = allocator_a.allocate().unwrap();
    let file_b = allocator_b.allocate().unwrap();

    assert_eq!(file_a.get(), file_b.get());
    assert_ne!(file_a, file_b);
    assert_ne!(file_a.owner(), file_b.owner());
}

#[test]
fn allocator_rejects_invalid_start_and_keeps_full_ids_distinct() {
    let owner = ArenaOwnerId::new(203).unwrap();
    assert_eq!(IdAllocator::<FileId>::new(owner, 0), Err(CoreError::InvalidId));

    let mut allocator = IdAllocator::<FileId>::new(owner, 1).unwrap();
    let first = allocator.allocate().unwrap();
    let second = allocator.allocate().unwrap();
    assert_ne!(first, second);
    assert_eq!(first.owner(), second.owner());
    assert_ne!(first.get(), second.get());
}

#[test]
fn receiving_authority_rejects_duplicate_full_ids_and_foreign_owner() {
    let owner_a = ArenaOwnerId::new(204).unwrap();
    let owner_b = ArenaOwnerId::new(205).unwrap();
    let mut allocator_a = IdAllocator::<FileId>::new(owner_a, 1).unwrap();
    let mut authority = PersistedIdAuthority::new(owner_a);
    let first = allocator_a.allocate_into(&mut authority).unwrap();

    assert_eq!(authority.accept(first), Err(CoreError::DuplicateId));
    assert_eq!(
        authority.accept(FileId::new(owner_b, first.get()).unwrap()),
        Err(CoreError::OwnershipMismatch)
    );
}

#[test]
fn persisted_allocator_retires_at_u64_boundary_without_recycling() {
    let owner = ArenaOwnerId::new(206).unwrap();
    let mut allocator = IdAllocator::<FileId>::new(owner, u64::MAX).unwrap();

    assert_eq!(allocator.allocate().unwrap().get(), u64::MAX);
    assert_eq!(allocator.allocate(), Err(CoreError::Exhausted));
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

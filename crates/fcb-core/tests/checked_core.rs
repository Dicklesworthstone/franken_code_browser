use fcb_core::{
    decoded_utf8_to_byte_boundary, scalar_to_utf16_boundary, utf16_to_scalar_boundary,
    ArenaOwnerId, BidiBoundary, ByteOffset, CaretAffinity, CoreError, DecodedUtf8Offset,
    DisplayGeneration, FileId, GraphemeBoundary, GraphemeBoundaryRange, IdAllocator,
    ImmutableSnapshot, NATIVE_NOT_FOUND, PersistedIdAuthority, PublicationContext,
    PublicationToken, QueryGeneration, RangeEvidenceEvent, RangeEvidenceKind, RangeEvidenceRing,
    ScalarIndex, SnapshotCell, SnapshotDelta, SourceRevision, Utf16CodeUnitOffset,
    VisualPosition,
};

#[test]
fn typed_ranges_preserve_domains_and_checked_boundaries() {
    let bytes = fcb_core::ByteRange::new(ByteOffset::new(1), ByteOffset::new(4)).unwrap();
    let graphemes = GraphemeBoundaryRange::new(
        GraphemeBoundary::new(2),
        GraphemeBoundary::new(5),
    )
    .unwrap();

    assert_eq!(bytes.len().get(), 3);
    assert_eq!(graphemes.len(), 3);
    assert_eq!(
        fcb_core::ByteRange::new(ByteOffset::new(4), ByteOffset::new(1)),
        Err(CoreError::RangeReversed)
    );
    assert_eq!(
        ByteOffset::new(u64::MAX).checked_add(1),
        Err(CoreError::ArithmeticOverflow)
    );
}

#[test]
fn utf8_utf16_and_scalar_boundaries_reject_native_and_surrogate_errors() {
    let text = "a😀b";

    assert_eq!(
        decoded_utf8_to_byte_boundary(text, DecodedUtf8Offset::new(1)),
        Ok(ByteOffset::new(1))
    );
    assert_eq!(
        decoded_utf8_to_byte_boundary(text, DecodedUtf8Offset::new(2)),
        Err(CoreError::InvalidUtf8Boundary)
    );
    assert_eq!(
        scalar_to_utf16_boundary(text, ScalarIndex::new(2)),
        Ok(Utf16CodeUnitOffset::new(3))
    );
    assert_eq!(
        utf16_to_scalar_boundary(text, Utf16CodeUnitOffset::new(2)),
        Err(CoreError::InvalidUtf16)
    );
    assert_eq!(
        utf16_to_scalar_boundary(text, Utf16CodeUnitOffset::new(5)),
        Err(CoreError::LimitExceeded)
    );
    assert_eq!(
        Utf16CodeUnitOffset::from_native(NATIVE_NOT_FOUND),
        Err(CoreError::NativeSentinel)
    );
    assert_eq!(
        Utf16CodeUnitOffset::from_native(3),
        Ok(Utf16CodeUnitOffset::new(3))
    );
}

#[test]
fn semantic_nodes_and_bidi_boundaries_keep_owner_and_affinity_explicit() {
    let owner = ArenaOwnerId::new(301).unwrap();
    let foreign_owner = ArenaOwnerId::new(302).unwrap();
    let node = fcb_core::SemanticNodeId::new(owner, 1).unwrap();
    let same_node = fcb_core::SemanticNodeId::new(owner, 1).unwrap();
    let foreign_node = fcb_core::SemanticNodeId::new(foreign_owner, 1).unwrap();

    assert_eq!(node.validate_for(owner), Ok(()));
    assert_eq!(node.validate_for(foreign_owner), Err(CoreError::OwnershipMismatch));
    assert_eq!(node, same_node);
    assert_ne!(node, foreign_node);
    assert_eq!(
        fcb_core::SemanticNodeId::new(owner, 0),
        Err(CoreError::InvalidId)
    );

    let upstream = BidiBoundary::new(
        Utf16CodeUnitOffset::new(1),
        VisualPosition::new(7),
        CaretAffinity::Upstream,
    );
    let downstream = BidiBoundary::new(
        Utf16CodeUnitOffset::new(2),
        VisualPosition::new(7),
        CaretAffinity::Downstream,
    );
    assert_eq!(upstream.visual(), downstream.visual());
    assert_ne!(upstream.logical(), downstream.logical());
    assert_eq!(upstream.affinity().to_native(), 0);
    assert_eq!(CaretAffinity::from_native(1), Ok(CaretAffinity::Downstream));
    assert_eq!(CaretAffinity::from_native(2), Err(CoreError::InvalidBidiBoundary));
    assert_eq!(
        Utf16CodeUnitOffset::new(u64::MAX).checked_add(1),
        Err(CoreError::ArithmeticOverflow)
    );
}

#[test]
fn range_evidence_ring_is_bounded_and_aggregated_without_source_payloads() {
    let mut evidence = RangeEvidenceRing::<2>::new();
    evidence.record(RangeEvidenceEvent::new(
        RangeEvidenceKind::Utf16Boundary,
        Ok(()),
    ));
    evidence.record(RangeEvidenceEvent::new(
        RangeEvidenceKind::NativeUtf16,
        Err(CoreError::NativeSentinel),
    ));
    evidence.record(RangeEvidenceEvent::new(
        RangeEvidenceKind::BidiBoundary,
        Err(CoreError::InvalidBidiBoundary),
    ));

    assert_eq!(evidence.capacity(), 2);
    assert_eq!(evidence.accepted(), 1);
    assert_eq!(evidence.rejected(), 2);
    assert_eq!(evidence.events().len(), 2);
    assert_eq!(evidence.events()[0].kind(), RangeEvidenceKind::NativeUtf16);
    assert_eq!(
        evidence.events()[1].outcome(),
        Err(CoreError::InvalidBidiBoundary)
    );
}

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
    assert!(matches!(
        IdAllocator::<FileId>::new(owner, 0),
        Err(CoreError::InvalidId)
    ));

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

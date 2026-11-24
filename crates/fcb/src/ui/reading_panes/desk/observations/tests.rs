use super::*;

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn alloc(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn range(a: u64, b: u64) -> ByteRange { ByteRange::new(ByteOffset::new(a), ByteOffset::new(b)).unwrap() }
fn setup(limits: DeskLimits) -> (ReadingDesk, ResourceBudget) {
    let budget = ResourceBudget::new(owner(900), ByteLength::new(256 * 1024 * 1024)).unwrap();
    (ReadingDesk::new(owner(900), limits, &budget, alloc(1)).unwrap(), budget)
}
fn extent(base: u64, revision: u64, bytes: &[u8]) -> ObservedExtent {
    let budget = ResourceBudget::new(owner(77), ByteLength::new(4 * 1024 * 1024)).unwrap();
    let request = CaptureRequest::new(FileId::new(owner(77), 41).unwrap(), SourceRevision::new(owner(77), revision).unwrap())
        .unwrap().with_range(range(base, base + bytes.len() as u64)).unwrap();
    ObservedExtent::from_bytes(request, ByteLength::new(base + bytes.len() as u64), bytes, &budget, alloc(1)).unwrap()
}
fn import(desk: &mut ReadingDesk, extent: &ObservedExtent, selected: ByteRange) -> DeskObservationChange {
    let attempt = desk.last_attempt() + 1;
    desk.import_observation(desk.revision(), attempt, extent, b"src/\xff.rs", DetectedEncoding::Utf8 { has_bom: false },
        selected, 500, alloc(1000 + attempt), || false).unwrap()
}

#[test]
fn two_passages_share_one_immutable_observation_and_keep_absolute_byte_offsets() {
    let (mut desk, budget) = setup(DeskLimits::default());
    let base = 1u64 << 40;
    let extent = extent(base, 42, b"prefix__needle__suffix");
    let first = import(&mut desk, &extent, range(base + 8, base + 14));
    let second = import(&mut desk, &extent, range(base + 8, base + 11));
    assert_eq!(first.observation, second.observation);
    assert_ne!(first.passage, second.passage);
    assert_eq!(desk.retained_source_count(), 1);
    assert_eq!(desk.retained_source_bytes(), extent.bytes().len() as u64);
    assert!(desk.panes().is_empty()); assert!(desk.history().is_empty()); assert!(desk.bookmarks().is_empty());
    let observation = desk.observation(2, first.observation).unwrap();
    assert_eq!(observation.path_bytes(), b"src/\xff.rs");
    assert_eq!(observation.origin_owner(), owner(77)); assert_eq!(observation.origin_file(), 41);
    assert_eq!(observation.origin_observation(), 42);
    assert_eq!(observation.extent().request().file().owner(), owner(900));
    assert_eq!(observation.extent().request().file().get(), 500);
    assert_eq!(first.next_source_identity, 501);
    let read = desk.read_observed_passage(2, first.passage, alloc(4000), || false).unwrap();
    assert_eq!(read.text(), "needle"); assert_eq!(read.range(), range(base + 8, base + 14));
    desk.forget_observed_passage(2, 3, first.passage, 1, || false).unwrap();
    assert_eq!(desk.retained_source_count(), 1);
    desk.forget_observed_passage(3, 4, second.passage, 1, || false).unwrap();
    assert_eq!(desk.retained_source_count(), 0);
    let held = budget.accounting().reserved().get();
    assert_eq!(read.extent().bytes(), extent.bytes());
    drop(read);
    assert!(budget.accounting().reserved().get() < held);
    assert_eq!(desk.next_source_identity().unwrap(), 501);
}

#[test]
fn passage_edits_forgets_and_attempt_replay_are_transactional() {
    let (mut desk, _) = setup(DeskLimits::default());
    let observed = extent(0, 42, b"a saved passage");
    let imported = import(&mut desk, &observed, range(2, 7));
    let id = imported.passage;
    desk.edit_observed_passage(1, 2, id, 1, "Review\0", "Personal\r\ntext 😀", || false).unwrap();
    assert_eq!(desk.observed_passage(2, id).unwrap().update_sequence(), 2);
    assert_eq!(desk.edit_observed_passage(2, 3, id, 1, "stale", "discard", || false), Err(DeskError::StalePassage));
    assert_eq!(desk.forget_observed_passage(2, 4, id, 1, || false), Err(DeskError::StalePassage));
    assert_eq!(desk.edit_observed_passage(2, 5, id, 2, "cancel", "discard", || true), Err(DeskError::Canceled));
    assert_eq!(desk.forget_observed_passage(2, 6, id, 2, || true), Err(DeskError::Canceled));
    assert_eq!(desk.forget_observed_passage(1, 7, id, 2, || false), Err(DeskError::StaleRevision));
    let passage = desk.observed_passage(2, id).unwrap();
    assert_eq!(passage.label(), "Review\0"); assert_eq!(passage.body(), "Personal\r\ntext 😀");
    assert_eq!(passage.selection(), range(2, 7));
    desk.forget_observed_passage(2, 7, id, 2, || false).unwrap();
    assert_eq!(desk.forget_observed_passage(7, 7, id, 2, || false), Err(DeskError::StaleAttempt));
    assert!(desk.observed_passages().is_empty()); assert_eq!(desk.retained_source_bytes(), 0);
}

#[test]
fn complete_sources_and_observations_share_source_bytes_and_bookmark_limits() {
    let (mut desk, _) = setup(DeskLimits { sources: 1, ..DeskLimits::default() });
    let observed = extent(0, 42, b"a saved passage");
    import(&mut desk, &observed, range(2, 7));
    let capture = SourceCapture::from_bytes(owner(900), FileId::new(owner(900), 501).unwrap(),
        SourceRevision::new(owner(900), 501).unwrap(), "complete.rs", b"whole".to_vec()).unwrap();
    assert_eq!(desk.open(1, 2, capture, 0, None, alloc(3000), || false), Err(DeskError::SourceLimit));
    let (mut desk, _) = setup(DeskLimits { bookmarks: 1, ..DeskLimits::default() });
    import(&mut desk, &observed, range(2, 7));
    assert_eq!(desk.import_observation(1, 2, &observed, b"src/\xff.rs", DetectedEncoding::Utf8 { has_bom: false },
        range(2, 4), 501, alloc(3001), || false), Err(DeskError::BookmarkLimit));
    let capture = SourceCapture::from_bytes(owner(900), FileId::new(owner(900), 501).unwrap(),
        SourceRevision::new(owner(900), 501).unwrap(), "complete.rs", b"whole".to_vec()).unwrap();
    let pane = desk.open(1, 3, capture, 0, None, alloc(3002), || false).unwrap().active.unwrap();
    assert_eq!(desk.apply(3, 4, DeskCommand::Bookmark { pane, label: "extra".into() }, || false), Err(DeskError::BookmarkLimit));
    let (mut desk, _) = setup(DeskLimits { retained_bytes: 10, ..DeskLimits::default() });
    assert_eq!(desk.import_observation(0, 1, &observed, b"p", DetectedEncoding::Utf8 { has_bom: false },
        range(2, 7), 1, alloc(3003), || false), Err(DeskError::RetainedByteLimit));
}

#[test]
fn cancellation_conflicting_evidence_and_split_scalars_never_publish_a_passage() {
    let (mut desk, _) = setup(DeskLimits::default());
    let observed = extent(0, 42, b"a saved passage");
    for stop in 1..=3 {
        let mut checks = 0;
        let attempt = desk.last_attempt() + 1;
        assert_eq!(desk.import_observation(0, attempt, &observed, b"p", DetectedEncoding::Utf8 { has_bom: false },
            range(2, 7), 1, alloc(100 + attempt), || { checks += 1; checks == stop }), Err(DeskError::Canceled));
        assert_eq!(desk.revision(), 0); assert!(desk.observed_passages().is_empty());
        assert_eq!(desk.retained_source_count(), 0);
    }
    let accepted = import(&mut desk, &observed, range(2, 7));
    let conflicting = extent(0, 42, b"a NEWER passage");
    assert_eq!(desk.import_observation(desk.revision(), desk.last_attempt() + 1, &conflicting, b"src/\xff.rs",
        DetectedEncoding::Utf8 { has_bom: false }, range(2, 7), 501, alloc(9000), || false), Err(DeskError::IdentityConflict));
    let unicode = extent(0, 43, "xxéyyzz".as_bytes());
    assert!(desk.import_observation(desk.revision(), desk.last_attempt() + 1, &unicode, b"p",
        DetectedEncoding::Utf8 { has_bom: false }, range(3, 4), 501, alloc(9001), || false).is_err());
    assert_eq!(desk.observed_passages().len(), 1);
    assert_eq!(desk.read_observed_passage(desk.revision(), accepted.passage, alloc(9002), || false).unwrap().text(), "saved");
}


fn sliced_extent(source: &[u8], captured: ByteRange, requested: ByteRange,
    consistency: crate::search::ExtentConsistency) -> ObservedExtent {
    let budget = ResourceBudget::new(owner(77), ByteLength::new(4 * 1024 * 1024)).unwrap();
    let request = CaptureRequest::new(FileId::new(owner(77), 41).unwrap(),
        SourceRevision::new(owner(77), 42).unwrap()).unwrap().with_range(requested).unwrap();
    let bytes = &source[captured.start().get() as usize..captured.end().get() as usize];
    let lease = budget.try_reserve_managed(owner(77), alloc(1), ByteLength::new(bytes.len() as u64 + 4096)).unwrap();
    ObservedExtent::restore_recorded(request, captured, ByteLength::new(source.len() as u64),
        Some(ByteLength::new(source.len() as u64)), consistency, bytes, lease).unwrap()
}

#[test]
fn one_source_observation_keeps_distinct_compatible_extents_and_deduplicates_exact_repeats() {
    let (mut desk, budget) = setup(DeskLimits::default());
    let source: Vec<_> = (0..200).map(|i| b'a' + (i % 26) as u8).collect();
    let a = sliced_extent(&source, range(0, 80), range(0, 80), crate::search::ExtentConsistency::HostSupplied);
    let b = sliced_extent(&source, range(64, 144), range(64, 144), crate::search::ExtentConsistency::HostSupplied);
    let c = sliced_extent(&source, range(144, 200), range(144, 200), crate::search::ExtentConsistency::HostSupplied);
    let first = import(&mut desk, &a, range(10, 20));
    let second = import(&mut desk, &b, range(80, 90));
    let third = import(&mut desk, &c, range(160, 170));
    let repeated = import(&mut desk, &b, range(100, 110));
    assert_ne!(first.observation, second.observation);
    assert_ne!(second.observation, third.observation);
    assert_eq!(second.observation, repeated.observation);
    assert_ne!(second.passage, repeated.passage);
    assert_eq!(desk.retained_source_count(), 3);
    assert_eq!(desk.retained_source_bytes(), 216);
    for (id, selected) in [(first.passage, range(10, 20)), (second.passage, range(80, 90)),
        (third.passage, range(160, 170)), (repeated.passage, range(100, 110))] {
        let text = desk.read_observed_passage(desk.revision(), id, alloc(9000 + id), || false).unwrap();
        assert_eq!(text.extent().range_bytes(selected).unwrap(),
            &source[selected.start().get() as usize..selected.end().get() as usize]);
        let observation = desk.observation(desk.revision(),
            desk.observed_passage(desk.revision(), id).unwrap().observation()).unwrap();
        assert_eq!(observation.origin_observation(), 42);
        assert_eq!(observation.origin_file(), 41);
    }
    // Each local row owns its own bounded payload; removing one never drops a
    // sibling or an independently retained view.
    let held = desk.read_observed_passage(desk.revision(), second.passage, alloc(9900), || false).unwrap();
    let before = budget.accounting().reserved().get();
    for id in [second.passage, repeated.passage] {
        desk.forget_observed_passage(desk.revision(), desk.last_attempt() + 1, id, 1, || false).unwrap();
    }
    assert_eq!(desk.retained_source_count(), 2);
    assert_eq!(desk.retained_source_bytes(), 136);
    assert_eq!(held.extent().range(), range(64, 144));
    drop(held);
    assert!(budget.accounting().reserved().get() < before);
}

#[test]
fn conflicting_overlapping_extents_and_metadata_leave_old_passages_unchanged() {
    let (mut desk, _) = setup(DeskLimits::default());
    let source = vec![b'a'; 16 * 1024];
    let first = sliced_extent(&source, range(0, 8192), range(0, 8192),
        crate::search::ExtentConsistency::HostSupplied);
    let kept = import(&mut desk, &first, range(16, 32));
    desk.edit_observed_passage(1, 2, kept.passage, 1, "Keep", "original note", || false).unwrap();
    let mut changed = source.clone(); changed[5000] = b'b';
    let conflicts = [
        (sliced_extent(&changed, range(4096, 12_288), range(4096, 12_288),
            crate::search::ExtentConsistency::HostSupplied), b"src/\xff.rs".as_slice(), DetectedEncoding::Utf8 { has_bom: false }),
        (sliced_extent(&source, range(4096, 12_288), range(4096, 12_288),
            crate::search::ExtentConsistency::HostSupplied), b"different".as_slice(), DetectedEncoding::Utf8 { has_bom: false }),
        (sliced_extent(&source, range(4096, 12_288), range(4096, 12_288),
            crate::search::ExtentConsistency::HostSupplied), b"src/\xff.rs".as_slice(), DetectedEncoding::Utf16Le),
        (sliced_extent(&source, range(4096, 12_288), range(4096, 12_288),
            crate::search::ExtentConsistency::UnchangedMetadata), b"src/\xff.rs".as_slice(), DetectedEncoding::Utf8 { has_bom: false }),
        (sliced_extent(&source[..15_000], range(4096, 12_288), range(4096, 12_288),
            crate::search::ExtentConsistency::HostSupplied), b"src/\xff.rs".as_slice(), DetectedEncoding::Utf8 { has_bom: false }),
    ];
    for (extent, path, encoding) in conflicts {
        let attempt = desk.last_attempt() + 1;
        assert_eq!(desk.import_observation(2, attempt, &extent, path, encoding, range(6000, 6010),
            501, alloc(10_000 + attempt), || false), Err(DeskError::IdentityConflict));
        assert_eq!(desk.revision(), 2);
        assert_eq!(desk.retained_source_count(), 1);
        let note = desk.observed_passage(2, kept.passage).unwrap();
        assert_eq!(note.body(), "original note"); assert_eq!(note.update_sequence(), 2);
    }
    let compatible = sliced_extent(&source, range(4096, 12_288), range(4096, 12_288),
        crate::search::ExtentConsistency::HostSupplied);
    let attempt = desk.last_attempt() + 1;
    let mut polls = 0;
    assert_eq!(desk.import_observation(2, attempt, &compatible, b"src/\xff.rs",
        DetectedEncoding::Utf8 { has_bom: false }, range(6000, 6010), 501, alloc(20_000), || {
            polls += 1; polls >= 4
        }), Err(DeskError::Canceled));
    assert_eq!(desk.revision(), 2); assert_eq!(desk.observed_passages().len(), 1);
    let accepted = import(&mut desk, &compatible, range(6000, 6010));
    assert_ne!(accepted.observation, kept.observation);
}

#[test]
fn identical_captured_prefixes_with_different_requests_remain_separate_evidence() {
    let (mut desk, _) = setup(DeskLimits::default());
    let source = vec![b'a'; 200];
    let a = sliced_extent(&source, range(0, 80), range(0, 100), crate::search::ExtentConsistency::ShortRead);
    let b = sliced_extent(&source, range(0, 80), range(0, 120), crate::search::ExtentConsistency::ShortRead);
    let first = import(&mut desk, &a, range(10, 20));
    let second = import(&mut desk, &b, range(10, 20));
    assert_ne!(first.observation, second.observation);
    assert_eq!(desk.retained_source_count(), 2);
    for (id, requested) in [(first.observation, range(0, 100)), (second.observation, range(0, 120))] {
        let extent = desk.observation(desk.revision(), id).unwrap().extent();
        assert_eq!(extent.request().range(), Some(requested));
        assert_eq!(extent.range(), range(0, 80)); assert!(!extent.request_filled());
    }
}

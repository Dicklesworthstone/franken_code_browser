#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::io::Cursor;
use fcb::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb::search::{QueryGeneration, ResourceAllocationId, ResourceBudget, ReaderLimits, ReadingTarget, ReadingSeekState, ReadingWindowOptions};
use fcb::search::snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData, SnapshotLimits};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::trail::{TrailBytes, TrailView, TrailEntry, TrailNavigator, TrailReadError, TrailError,
    pin_selection, append_selection, resolve_selection, MAX_TRAIL_ITEMS};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(811).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn range(a: u64, b: u64) -> ByteRange { ByteRange::new(ByteOffset::new(a), ByteOffset::new(b)).unwrap() }
fn archive(bytes: &[u8], path: &[u8], budget: &ResourceBudget, id: u64) -> PagedSnapshot<Cursor<Vec<u8>>> {
    let encoded = SnapshotBytes::encode(owner(), true, "trail-tests-v1", &[SnapshotEntry { path,
        observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }], SnapshotLimits::default(), budget, allocation(1), || false).unwrap();
    PagedSnapshot::open(Cursor::new(encoded.bytes().to_vec()), owner(), SnapshotLimits::default(), budget, allocation(id), || false).unwrap()
}

#[test]
fn pin_append_navigate_and_read_exact_utf16_after_archive_handle_closes() {
    let budget = budget();
    let mut bytes = vec![0xff, 0xfe];
    for unit in "head\r\nbanana".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    let mut archive = archive(&bytes, b"src/Thing.rs", &budget, 2);
    let first = pin_selection(&mut archive, b"src/Thing.rs", range(16, 22), "first occurrence", &budget, allocation(3), || false).unwrap();
    let saved = append_selection(owner(), None, "Banana investigation", first, &budget, [allocation(4), allocation(5)], || false).unwrap();
    let before = saved.bytes().to_vec();
    let first_view = TrailView::open(saved.bytes(), || false).unwrap();
    let second = pin_selection(&mut archive, b"src/Thing.rs", range(20, 26), "overlapping occurrence", &budget, allocation(3), || false).unwrap();
    let next = append_selection(owner(), Some(first_view), "ignored new title", second, &budget, [allocation(4), allocation(6)], || false).unwrap();
    assert_eq!(saved.bytes(), before);
    let view = TrailView::open(next.bytes(), || false).unwrap();
    assert_eq!(view.title(), "Banana investigation"); assert_eq!(view.parent_digest(), Some(first_view.digest()));
    assert_eq!(view.reference_bytes(), 12);
    let mut nav = TrailNavigator::new(view, generation(1));
    let first_target = nav.target().unwrap().unwrap();
    assert!(nav.next().unwrap()); let second_target = nav.target().unwrap().unwrap();
    assert_eq!(first_target.validate_delivery(view.digest(), nav.generation()), Err(TrailReadError::StaleDelivery));
    assert_eq!(second_target.entry().rationale, "overlapping occurrence");
    let retained = second_target.open(&mut archive, view.digest(), nav.generation(), FileId::new(owner(), 100).unwrap(),
        SourceRevision::new(owner(), 200).unwrap(), &budget, [allocation(7), allocation(8)], || false).unwrap();
    drop(archive);
    assert_eq!(retained.selected_bytes(), &bytes[20..26]);
    let reader = retained.reader(ReaderLimits::default(), &budget, allocation(9)).unwrap();
    let mut seek = reader.seek(ReadingTarget::Range(range(20, 26)), nav.generation()).unwrap();
    while seek.state() == ReadingSeekState::Pending { seek.step(4, nav.generation(), || false).unwrap(); }
    let ReadingSeekState::Ready(at) = seek.state() else { panic!("expected exact anchor") };
    assert_eq!(at.line_number(), 2);
    let window = reader.window(at, nav.generation(), ReadingWindowOptions::default(), &budget, allocation(10), || false).unwrap();
    assert_eq!(window.text(), "ana");
    assert_eq!(window.text_selection(window.source_to_text(range(20, 26)).unwrap()).unwrap().original_bytes, retained.selected_bytes());
    assert!(nav.previous().unwrap()); assert!(!nav.previous().unwrap());
    assert_ne!(nav.generation(), first_target.generation(), "returning cannot resurrect an old delivery token");
}

#[test]
fn same_path_or_same_text_in_another_archive_cannot_receive_the_old_note() {
    let budget = budget(); let mut old = archive(b"needle", b"a.rs", &budget, 2);
    let entry = pin_selection(&mut old, b"a.rs", range(0, 6), "old note", &budget, allocation(3), || false).unwrap();
    let trail = append_selection(owner(), None, "", entry, &budget, [allocation(4), allocation(5)], || false).unwrap();
    let view = TrailView::open(trail.bytes(), || false).unwrap();
    let target = TrailNavigator::new(view, generation(1)).target().unwrap().unwrap();
    let mut changed = archive(b"newest", b"a.rs", &budget, 6);
    assert_eq!(resolve_selection(target.entry(), changed.directory()), Err(TrailReadError::OtherArchive));
    assert!(matches!(target.open(&mut changed, view.digest(), generation(1), FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 1).unwrap(), &budget, [allocation(7), allocation(8)], || false), Err(TrailReadError::OtherArchive)));
    let moved = archive(b"needle", b"renamed.rs", &budget, 9);
    assert_eq!(resolve_selection(target.entry(), moved.directory()), Err(TrailReadError::OtherArchive));
}

#[test]
fn multiple_archives_and_repeat_visits_preserve_explicit_order_and_rationale() {
    let budget = budget(); let mut a = archive(b"abc", b"\xff\\a", &budget, 2);
    let mut b = archive(b"xyz", b"b", &budget, 3);
    let first = pin_selection(&mut a, b"\xff\\a", range(0, 3), "inspect", &budget, allocation(4), || false).unwrap();
    let second = pin_selection(&mut b, b"b", range(1, 2), "compare", &budget, allocation(4), || false).unwrap();
    let bytes = TrailBytes::encode(owner(), "route", None, &[first, second, first], &budget, allocation(5), || false).unwrap();
    let view = TrailView::open(bytes.bytes(), || false).unwrap();
    assert_eq!(view.reference_bytes(), 7);
    assert_eq!(view.entries().map(|e| e.unwrap().path).collect::<Vec<_>>(), [b"\xff\\a".as_slice(), b"b", b"\xff\\a"]);
    let mut nav = TrailNavigator::new(view, generation(1));
    for expected in [0, 1, 2] {
        assert_eq!(nav.current_ordinal(), Some(expected));
        if expected != 2 { assert!(nav.next().unwrap()); }
    }
    assert!(!nav.next().unwrap()); assert!(nav.previous().unwrap());
    assert_eq!(nav.target().unwrap().unwrap().entry().rationale, "compare");
}

#[test]
fn missing_unavailable_and_out_of_bounds_members_do_not_generate_pins() {
    let budget = budget(); let mut a = archive(b"x", b"a", &budget, 2);
    assert!(matches!(pin_selection(&mut a, b"missing", range(0, 1), "", &budget, allocation(3), || false), Err(TrailReadError::MissingMember)));
    assert!(matches!(pin_selection(&mut a, b"a", range(0, 2), "", &budget, allocation(3), || false), Err(TrailReadError::InvalidRange)));
    let encoded = SnapshotBytes::encode(owner(), false, "test", &[SnapshotEntry { path: b"missing", observed_bytes: 0,
        data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }], SnapshotLimits::default(), &budget, allocation(1), || false).unwrap();
    let mut unavailable = PagedSnapshot::open(Cursor::new(encoded.bytes()), owner(), SnapshotLimits::default(), &budget, allocation(4), || false).unwrap();
    assert!(matches!(pin_selection(&mut unavailable, b"missing", range(0, 0), "", &budget, allocation(3), || false), Err(TrailReadError::UnavailableMember)));
}

#[test]
fn bounds_refuse_append_without_evicting_existing_user_notes() {
    let budget = budget(); let mut a = archive(b"x", b"a", &budget, 2);
    let selection = pin_selection(&mut a, b"a", range(0, 1), "preserve", &budget, allocation(3), || false).unwrap();
    let all = vec![selection; MAX_TRAIL_ITEMS];
    let saved = TrailBytes::encode(owner(), "notes", None, &all, &budget, allocation(4), || false).unwrap();
    let original = saved.bytes().to_vec(); let view = TrailView::open(saved.bytes(), || false).unwrap();
    assert!(matches!(append_selection(owner(), Some(view), "", selection, &budget, [allocation(5), allocation(6)], || false), Err(TrailError::Limit)));
    assert_eq!(saved.bytes(), original); assert_eq!(view.len(), MAX_TRAIL_ITEMS);
}

#[test]
fn identity_exhaustion_and_bad_ordinals_leave_navigation_unchanged() {
    let budget = budget(); let mut a = archive(b"", b"empty", &budget, 2);
    let entry = pin_selection(&mut a, b"empty", range(0, 0), "point", &budget, allocation(3), || false).unwrap();
    let saved = TrailBytes::encode(owner(), "", None, &[entry, entry], &budget, allocation(4), || false).unwrap();
    let view = TrailView::open(saved.bytes(), || false).unwrap();
    let mut nav = TrailNavigator::new(view, generation(u64::MAX));
    assert_eq!(nav.select(0), Ok(()));
    assert_eq!(nav.next(), Err(TrailReadError::GenerationExhausted));
    assert_eq!(nav.current_ordinal(), Some(0)); assert!(nav.select(2).is_err());
    let target = nav.target().unwrap().unwrap();
    let foreign = ArenaOwnerId::new(812).unwrap();
    assert!(matches!(target.open(&mut a, view.digest(), generation(u64::MAX), FileId::new(foreign, 1).unwrap(),
        SourceRevision::new(foreign, 1).unwrap(), &budget, [allocation(5), allocation(6)], || false), Err(TrailReadError::OwnerMismatch)));
}

#[test]
fn wrong_digest_in_a_checksum_valid_trail_does_not_authorize_source() {
    let budget = budget(); let mut a = archive(b"abc", b"a", &budget, 2);
    let selection = pin_selection(&mut a, b"a", range(0, 3), "", &budget, allocation(3), || false).unwrap();
    let corrupt = TrailEntry { source: fcb::search::trail::Sha256Digest::new([0; 32]), ..selection };
    let saved = TrailBytes::encode(owner(), "", None, &[corrupt], &budget, allocation(4), || false).unwrap();
    let view = TrailView::open(saved.bytes(), || false).unwrap();
    assert_eq!(resolve_selection(view.entry(0).unwrap(), a.directory()), Err(TrailReadError::ChangedSource));
}

#[test]
fn canceled_pin_and_append_release_only_candidate_reservations() {
    let budget = budget(); let mut a = archive(b"abc", b"a", &budget, 2);
    let before = budget.accounting().reserved().get();
    assert!(pin_selection(&mut a, b"a", range(0, 3), "", &budget, allocation(3), || true).is_err());
    assert_eq!(budget.accounting().reserved().get(), before);
    let entry = pin_selection(&mut a, b"a", range(0, 3), "", &budget, allocation(3), || false).unwrap();
    assert!(matches!(append_selection(owner(), None, "", entry, &budget, [allocation(4), allocation(5)],
        || budget.accounting().reserved().get() > before), Err(TrailError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), before);
}

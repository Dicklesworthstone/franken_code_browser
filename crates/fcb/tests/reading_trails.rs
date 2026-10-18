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

#[test]
fn export_cursor_preserves_raw_selections_repeated_visits_and_bounded_residency() {
    use fcb::search::trail::{TrailExportCursor, TrailExportLimits, TrailExportOutcome};
    let budget = budget(); let bytes = b"\xff\xfea\0\x00\xd8z\0";
    let mut archive = archive(bytes, b"src/\xff.rs", &budget, 2);
    let selection = pin_selection(&mut archive, b"src/\xff.rs", range(3, 7), "inside a UTF-16 unit", &budget, allocation(3), || false).unwrap();
    let saved = TrailBytes::encode(owner(), "raw evidence", None, &[selection, selection], &budget, allocation(4), || false).unwrap();
    let view = TrailView::open(saved.bytes(), || false).unwrap();
    let baseline = budget.accounting().reserved().get();
    let mut cursor = TrailExportCursor::new(view, generation(8), FileId::new(owner(), 90).unwrap(),
        SourceRevision::new(owner(), 100).unwrap(), TrailExportLimits::default()).unwrap();
    for ordinal in 0..2 {
        let item = cursor.next(&mut archive, &budget, [allocation(5), allocation(6)], || false).unwrap().unwrap();
        assert_eq!(item.target.ordinal(), ordinal);
        assert_eq!(item.target.entry().path, b"src/\xff.rs");
        assert_eq!(item.target.entry().rationale, "inside a UTF-16 unit");
        let TrailExportOutcome::Verified(capture) = item.outcome else { panic!("selection must verify") };
        assert_eq!(capture.selected_bytes(), &bytes[3..7]);
        drop(capture);
        assert_eq!(budget.accounting().reserved().get(), baseline);
    }
    assert!(cursor.next(&mut archive, &budget, [allocation(5), allocation(6)], || false).unwrap().is_none());
    let stats = cursor.stats(); assert!(stats.complete());
    assert_eq!(stats.verification_bytes_admitted, (bytes.len() * 2) as u64);
    assert_eq!(stats.source_bytes_read, (bytes.len() * 2) as u64);
    assert_eq!(stats.selected_bytes, 8);
}

#[test]
fn export_omits_oversized_payload_before_io_and_still_admits_later_small_selection() {
    use fcb::search::trail::{TrailExportCursor, TrailExportLimits, TrailExportOutcome};
    let budget = budget(); let mut archive = archive(b"abcdefgh", b"a", &budget, 2);
    let large = pin_selection(&mut archive, b"a", range(0, 8), "large", &budget, allocation(3), || false).unwrap();
    let small = TrailEntry { range: range(2, 3), rationale: "small", ..large };
    let saved = TrailBytes::encode(owner(), "priority", None, &[large, small, small], &budget, allocation(4), || false).unwrap();
    let view = TrailView::open(saved.bytes(), || false).unwrap();
    let mut cursor = TrailExportCursor::new(view, generation(1), FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 1).unwrap(), TrailExportLimits {
            max_entries: 2, max_source_bytes: 8, max_selected_bytes: 1 }).unwrap();
    let before = archive.load_stats();
    let first = cursor.next(&mut archive, &budget, [allocation(5), allocation(6)], || false).unwrap().unwrap();
    assert!(matches!(first.outcome, TrailExportOutcome::SelectionByteLimit));
    assert_eq!(archive.load_stats(), before, "output-ineligible payloads must not consume source work");
    let second = cursor.next(&mut archive, &budget, [allocation(5), allocation(6)], || false).unwrap().unwrap();
    let TrailExportOutcome::Verified(capture) = second.outcome else { panic!("later small selection must survive") };
    assert_eq!(capture.selected_bytes(), b"c"); drop(capture);
    assert!(cursor.next(&mut archive, &budget, [allocation(5), allocation(6)], || false).unwrap().is_none());
    let stats = cursor.stats(); assert!(!stats.complete());
    assert_eq!(stats.omitted_selection_limit, 1); assert_eq!(stats.omitted_entry_limit, 1);
    assert_eq!(stats.verification_bytes_admitted, 8); assert_eq!(stats.selected_bytes, 1);
}

#[test]
fn export_source_budget_charges_whole_members_including_repeated_visits() {
    use fcb::search::trail::{TrailExportCursor, TrailExportLimits, TrailExportOutcome};
    let budget = budget(); let mut archive = archive(b"abcdef", b"a", &budget, 2);
    let entry = pin_selection(&mut archive, b"a", range(0, 1), "point", &budget, allocation(3), || false).unwrap();
    let saved = TrailBytes::encode(owner(), "repeat", None, &[entry, entry], &budget, allocation(4), || false).unwrap();
    let view = TrailView::open(saved.bytes(), || false).unwrap();
    let mut cursor = TrailExportCursor::new(view, generation(1), FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 1).unwrap(), TrailExportLimits {
            max_entries: 2, max_source_bytes: 6, max_selected_bytes: 2 }).unwrap();
    let first = cursor.next(&mut archive, &budget, [allocation(5), allocation(6)], || false).unwrap().unwrap();
    assert!(matches!(&first.outcome, TrailExportOutcome::Verified(_))); drop(first);
    let before = archive.load_stats();
    let second = cursor.next(&mut archive, &budget, [allocation(5), allocation(6)], || false).unwrap().unwrap();
    assert!(matches!(second.outcome, TrailExportOutcome::SourceByteLimit));
    assert_eq!(archive.load_stats(), before); assert_eq!(cursor.stats().omitted_source_limit, 1);
}

#[test]
fn export_wrong_archive_and_forged_source_digest_stay_unavailable_without_source_reads() {
    use fcb::search::trail::{TrailExportCursor, TrailExportLimits, TrailExportOutcome};
    let budget = budget(); let mut a = archive(b"abc", b"a", &budget, 2);
    let good = pin_selection(&mut a, b"a", range(0, 3), "verified", &budget, allocation(3), || false).unwrap();
    let wrong_archive = TrailEntry { archive: fcb::search::trail::Sha256Digest::new([0; 32]), ..good };
    let wrong_source = TrailEntry { source: fcb::search::trail::Sha256Digest::new([0; 32]), ..good };
    let saved = TrailBytes::encode(owner(), "identity controls", None, &[wrong_archive, wrong_source, good], &budget, allocation(4), || false).unwrap();
    let view = TrailView::open(saved.bytes(), || false).unwrap();
    let mut cursor = TrailExportCursor::new(view, generation(1), FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 1).unwrap(), TrailExportLimits::default()).unwrap();
    let before = a.load_stats();
    let first = cursor.next(&mut a, &budget, [allocation(5), allocation(6)], || false).unwrap().unwrap();
    assert!(matches!(first.outcome, TrailExportOutcome::Unavailable(TrailReadError::OtherArchive)));
    let second = cursor.next(&mut a, &budget, [allocation(5), allocation(6)], || false).unwrap().unwrap();
    assert!(matches!(second.outcome, TrailExportOutcome::Unavailable(TrailReadError::ChangedSource)));
    assert_eq!(a.load_stats(), before);
    let third = cursor.next(&mut a, &budget, [allocation(5), allocation(6)], || false).unwrap().unwrap();
    let TrailExportOutcome::Verified(capture) = third.outcome else { panic!("near-identical authorized control must succeed") };
    assert_eq!(capture.selected_bytes(), b"abc"); assert_eq!(cursor.stats().unavailable_entries, 2);
}

#[test]
fn canceled_export_is_terminal_and_releases_candidate_capacity() {
    use fcb::search::trail::{TrailExportCursor, TrailExportLimits};
    let budget = budget(); let mut a = archive(b"abc", b"a", &budget, 2);
    let entry = pin_selection(&mut a, b"a", range(0, 3), "keep", &budget, allocation(3), || false).unwrap();
    let saved = TrailBytes::encode(owner(), "cancel", None, &[entry], &budget, allocation(4), || false).unwrap();
    let view = TrailView::open(saved.bytes(), || false).unwrap();
    let mut cursor = TrailExportCursor::new(view, generation(1), FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 1).unwrap(), TrailExportLimits::default()).unwrap();
    let baseline = budget.accounting().reserved().get();
    assert!(cursor.next(&mut a, &budget, [allocation(5), allocation(6)],
        || budget.accounting().reserved().get() > baseline).is_err());
    assert_eq!(budget.accounting().reserved().get(), baseline);
    assert!(!cursor.stats().complete()); assert!(cursor.stats().interrupted);
    assert!(matches!(cursor.next(&mut a, &budget, [allocation(5), allocation(6)], || false), Err(TrailReadError::StaleDelivery)));
    assert!(matches!(TrailExportCursor::new(view, generation(1), FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 1).unwrap(), TrailExportLimits { max_entries: MAX_TRAIL_ITEMS + 1,
            ..TrailExportLimits::default() }), Err(TrailReadError::Format(TrailError::Limit))));
}

#[test]
fn export_reverifies_mutated_archive_payload_and_counts_failed_verification_work() {
    use std::{io::{self, Read, Seek, SeekFrom}, sync::{Arc, Mutex}};
    use fcb::search::trail::{TrailExportCursor, TrailExportLimits, TrailExportOutcome};
    use fcb::search::paged_snapshot::{PagedMemberData, PagedSearchError, PagedSnapshotError};
    struct SharedInput(Arc<Mutex<Cursor<Vec<u8>>>>);
    impl Read for SharedInput {
        fn read(&mut self, into: &mut [u8]) -> io::Result<usize> { self.0.lock().unwrap().read(into) }
    }
    impl Seek for SharedInput {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> { self.0.lock().unwrap().seek(to) }
    }
    let budget = budget();
    let encoded = SnapshotBytes::encode(owner(), true, "mutation-control", &[SnapshotEntry {
        path: b"a", observed_bytes: 3, data: SnapshotData::Captured(b"abc") }],
        SnapshotLimits::default(), &budget, allocation(1), || false).unwrap();
    let backing = Arc::new(Mutex::new(Cursor::new(encoded.bytes().to_vec())));
    let mut archive = PagedSnapshot::open(SharedInput(backing.clone()), owner(), SnapshotLimits::default(),
        &budget, allocation(2), || false).unwrap();
    let selection = pin_selection(&mut archive, b"a", range(0, 3), "original capture", &budget, allocation(3), || false).unwrap();
    let saved = TrailBytes::encode(owner(), "mutation control", None, &[selection], &budget, allocation(4), || false).unwrap();
    let view = TrailView::open(saved.bytes(), || false).unwrap();
    let PagedMemberData::Captured { archive_offset, .. } = archive.directory().member(0).unwrap().data else { panic!("captured fixture") };
    backing.lock().unwrap().get_mut()[archive_offset as usize + 1] = b'X';
    let mut cursor = TrailExportCursor::new(view, generation(1), FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 1).unwrap(), TrailExportLimits::default()).unwrap();
    let baseline = budget.accounting().reserved().get();
    let item = cursor.next(&mut archive, &budget, [allocation(5), allocation(6)], || false).unwrap().unwrap();
    assert!(matches!(item.outcome, TrailExportOutcome::Unavailable(TrailReadError::Capture(
        PagedSearchError::Archive(PagedSnapshotError::Changed)))));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    assert_eq!(cursor.stats().verification_bytes_admitted, 3);
    assert_eq!(cursor.stats().source_bytes_read, 3);
    assert_eq!(cursor.stats().selected_bytes, 0); assert!(!cursor.stats().complete());
    // Restoring the actual bytes succeeds under a fresh export attempt. The
    // same path and trusted directory alone did not authorize changed content.
    backing.lock().unwrap().get_mut()[archive_offset as usize + 1] = b'b';
    let mut fresh = TrailExportCursor::new(view, generation(2), FileId::new(owner(), 2).unwrap(),
        SourceRevision::new(owner(), 2).unwrap(), TrailExportLimits::default()).unwrap();
    let item = fresh.next(&mut archive, &budget, [allocation(5), allocation(6)], || false).unwrap().unwrap();
    let TrailExportOutcome::Verified(capture) = item.outcome else { panic!("restored exact bytes should verify") };
    assert_eq!(capture.selected_bytes(), b"abc"); assert!(fresh.stats().complete());
}

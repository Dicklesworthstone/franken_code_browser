use super::*;
use crate::store::envelope::{Sha256, HEADER_LEN};

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn alloc(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn range(a: u64, b: u64) -> ByteRange { ByteRange::new(ByteOffset::new(a), ByteOffset::new(b)).unwrap() }
fn desk(n: u64, limits: DeskLimits) -> ReadingDesk {
    let budget = ResourceBudget::new(owner(n), ByteLength::new(256 * 1024 * 1024)).unwrap();
    ReadingDesk::new(owner(n), limits, &budget, alloc(1)).unwrap()
}
fn open(d: &mut ReadingDesk, file: u64, rev: u64, bytes: &[u8]) -> DeskPaneId {
    let source = SourceCapture::from_bytes(d.owner(), FileId::new(d.owner(), file).unwrap(),
        SourceRevision::new(d.owner(), rev).unwrap(), format!("file-{file}.rs"), bytes.to_vec()).unwrap();
    let n = d.last_attempt() + 1;
    d.open(d.revision(), n, source, 0, None, alloc(1000 + n), || false).unwrap().active.unwrap()
}
fn apply(d: &mut ReadingDesk, command: DeskCommand) -> DeskChange {
    d.apply(d.revision(), d.last_attempt() + 1, command, || false).unwrap()
}
fn save(d: &ReadingDesk, allocation: u64) -> DeskCheckpoint {
    d.checkpoint(d.revision(), alloc(allocation), || false).unwrap()
}

#[test]
fn full_roundtrip_keeps_pins_independent_views_closed_history_and_notes() {
    let mut original = desk(700, DeskLimits::default());
    let a = open(&mut original, 10, 20, b"one two");
    apply(&mut original, DeskCommand::Navigate { pane: a, offset: 0, selection: Some(range(0, 3)) });
    apply(&mut original, DeskCommand::Pin { pane: a, pinned: true });
    let duplicate = apply(&mut original, DeskCommand::Duplicate(a)).active.unwrap();
    apply(&mut original, DeskCommand::Navigate { pane: duplicate, offset: 4, selection: Some(range(4, 7)) });
    apply(&mut original, DeskCommand::Arrange { pane: duplicate, position: (23.5, 42.0), size: (900.0, 650.5) });
    let b = open(&mut original, 11, 21, b"closed but retained");
    apply(&mut original, DeskCommand::Navigate { pane: b, offset: 7, selection: Some(range(7, 10)) });
    apply(&mut original, DeskCommand::Bookmark { pane: b, label: "why\nthis matters\0".into() });
    apply(&mut original, DeskCommand::Close(b));
    apply(&mut original, DeskCommand::Back);
    let checkpoint = save(&original, 2000);
    let mut restored = desk(701, DeskLimits::default());
    restored.restore_checkpoint(0, 1, checkpoint.bytes(), alloc(2), || false).unwrap();
    assert_eq!(restored.retained_source_count(), 2);
    assert_eq!(restored.panes().len(), original.panes().len());
    assert_eq!(restored.history_cursor(), original.history_cursor());
    assert_eq!(restored.bookmarks()[0].label(), "why\nthis matters\0");
    let panes: Vec<_> = restored.panes().iter().map(|p| restored.pane_id(p.id).unwrap()).collect();
    let first = panes[0]; let second = panes[1];
    assert_eq!(restored.selected_bytes(first, 1).unwrap(), b"one");
    assert_eq!(restored.selected_bytes(second, 1).unwrap(), b"two");
    assert_eq!(restored.source(first, 1).unwrap().bytes().as_ptr(), restored.source(second, 1).unwrap().bytes().as_ptr());
    assert_eq!(restored.panes().get_pane(second.get()).unwrap().position, (23.5, 42.0));
    assert_eq!(restored.panes().get_pane(second.get()).unwrap().size, (900.0, 650.5));
    let bookmark = restored.bookmarks()[0].id();
    apply(&mut restored, DeskCommand::RecallBookmark(bookmark));
    assert_eq!(restored.selected_bytes(restored.active().unwrap(), restored.revision()).unwrap(), b"but");
    assert!(restored.panes().iter().filter(|p| p.is_pinned).count() >= 2);
}

#[test]
fn same_owner_restore_rebases_all_identities_and_preserves_exported_old_view() {
    let mut d = desk(1, DeskLimits::default());
    let old = open(&mut d, 40, 50, b"retained");
    apply(&mut d, DeskCommand::Bookmark { pane: old, label: "old id".into() });
    let old_mark = d.bookmarks()[0].id();
    let view = d.view(old, d.revision(), alloc(8000)).unwrap();
    let checkpoint = save(&d, 2000);
    let result = d.restore_checkpoint(d.revision(), 3, checkpoint.bytes(), alloc(2001), || false).unwrap();
    let new = result.change.active.unwrap();
    assert_ne!(new, old);
    assert!(d.source(new, 3).unwrap().file().get() > 50);
    assert!(d.source(new, 3).unwrap().revision().get() > 50);
    assert!(result.next_source_identity > d.source(new, 3).unwrap().revision().get());
    assert_ne!(d.bookmarks()[0].id(), old_mark);
    assert_eq!(d.location(old, 3), Err(DeskError::MissingPane));
    assert_eq!(view.source().bytes(), b"retained");
    assert_eq!(d.source(new, 3).unwrap().bytes(), b"retained");
}

#[test]
fn identity_high_water_survives_complete_source_eviction() {
    let mut d = desk(1, DeskLimits::default());
    let pane = open(&mut d, 9000, 9999, b"retire");
    let checkpoint = save(&d, 2000);
    apply(&mut d, DeskCommand::Close(pane));
    apply(&mut d, DeskCommand::ClearHistory);
    assert_eq!(d.retained_source_count(), 0);
    d.restore_checkpoint(3, 4, checkpoint.bytes(), alloc(2001), || false).unwrap();
    assert!(d.source(d.active().unwrap(), 4).unwrap().file().get() > 9999);
}

#[test]
fn empty_checkpoint_is_complete_state_and_never_implies_a_source() {
    let original = desk(1, DeskLimits::default()); let checkpoint = save(&original, 2);
    let mut restored = desk(2, DeskLimits::default());
    restored.restore_checkpoint(0, 1, checkpoint.bytes(), alloc(3), || false).unwrap();
    assert_eq!(restored.active(), None); assert!(restored.history().is_empty());
    assert!(restored.bookmarks().is_empty()); assert_eq!(restored.retained_source_bytes(), 0);
}

#[test]
fn byte_domains_survive_without_encoding_or_newline_conversion() {
    for bytes in [b"".as_slice(), b"\xff\xfea\0\r\0\n\0\xff", b"a\0\xff\r\n", "😀\r\n".as_bytes()] {
        let mut d = desk(1, DeskLimits::default()); let pane = open(&mut d, 1, 1, bytes);
        apply(&mut d, DeskCommand::Navigate { pane, offset: 0, selection: Some(range(0, bytes.len() as u64)) });
        let checkpoint = save(&d, 2000);
        let mut restored = desk(2, DeskLimits::default());
        restored.restore_checkpoint(0, 1, checkpoint.bytes(), alloc(2), || false).unwrap();
        assert_eq!(restored.selected_bytes(restored.active().unwrap(), 1).unwrap(), bytes);
    }
}

#[test]
fn same_file_different_revisions_remain_distinct_after_restore_and_back() {
    let mut d = desk(1, DeskLimits::default());
    open(&mut d, 10, 20, b"old"); open(&mut d, 10, 21, b"new");
    let checkpoint = save(&d, 2000);
    let mut r = desk(2, DeskLimits::default());
    r.restore_checkpoint(0, 1, checkpoint.bytes(), alloc(2), || false).unwrap();
    let at = r.location(r.active().unwrap(), 1).unwrap();
    apply(&mut r, DeskCommand::Back);
    let previous = r.location(r.active().unwrap(), 2).unwrap();
    assert_eq!(previous.file, at.file); assert_ne!(previous.revision, at.revision);
    assert_eq!(r.source(r.active().unwrap(), 2).unwrap().bytes(), b"old");
    apply(&mut r, DeskCommand::Forward);
    assert_eq!(r.source(r.active().unwrap(), 3).unwrap().bytes(), b"new");
}

#[test]
fn serialization_is_deterministic_and_malformed_input_cannot_replace_state() {
    let mut d = desk(1, DeskLimits::default()); let pane = open(&mut d, 1, 1, b"safe");
    let saved = save(&d, 2000); let second = save(&d, 2001);
    assert_eq!(saved.bytes(), second.bytes()); assert_eq!(saved.digest(), second.digest());
    let mut bad = saved.bytes().to_vec(); bad[80] ^= 1;
    assert_eq!(d.restore_checkpoint(1, 2, &bad, alloc(2002), || false), Err(CheckpointError::Integrity));
    for (i, n) in [0, 1, 24, saved.bytes().len() - 1].into_iter().enumerate() {
        assert!(d.restore_checkpoint(1, 3 + i as u64, &saved.bytes()[..n], alloc(3000 + i as u64), || false).is_err());
    }
    assert_eq!(d.revision(), 1); assert_eq!(d.active(), Some(pane));
    assert_eq!(d.source(pane, 1).unwrap().bytes(), b"safe");
}

fn resign(bytes: &mut [u8]) {
    let end = bytes.len() - 32; let hash = Sha256::digest(&bytes[..end]);
    bytes[end..].copy_from_slice(hash.as_bytes());
}
#[test]
fn checksummed_hostile_counts_versions_and_wide_lengths_are_rejected() {
    let mut d = desk(1, DeskLimits::default()); open(&mut d, 1, 1, b"safe");
    let saved = save(&d, 2000);
    for (attempt, offset, value) in [(2, HEADER_LEN, u64::MAX), (3, 16, u64::MAX),
        (4, HEADER_LEN + 48 + 16, u64::MAX)] {
        let mut bytes = saved.bytes().to_vec(); bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes()); resign(&mut bytes);
        assert!(d.restore_checkpoint(1, attempt, &bytes, alloc(3000 + attempt), || false).is_err());
    }
    let mut bytes = saved.bytes().to_vec(); bytes[10] = 1; resign(&mut bytes);
    assert_eq!(d.restore_checkpoint(1, 5, &bytes, alloc(3005), || false), Err(CheckpointError::UnsupportedVersion));
    assert_eq!(d.revision(), 1);
}

fn hostile(pinned: u8, size: f32, source_index: u64, offset: u64, tail: bool) -> Vec<u8> {
    let mut w = EnvelopeWriter::new(SCHEMA);
    for n in [1, 1, 0, 0, NONE, 1] { w.put_u64(n); }
    w.put_u64(1); w.put_u64(1); w.put_str("label"); w.put_bytes(b"abc");
    w.put_u64(1); w.put_u8(pinned);
    for f in [0.0, 0.0, size, 480.0, 0.0, 0.0] { w.put_f32(f); }
    w.put_u64(source_index); w.put_u64(offset); w.put_u8(0); w.put_u64(1);
    if tail { w.put_u8(0); }
    w.finish()
}
#[test]
fn schema_validation_is_not_replaced_by_a_valid_checksum() {
    let mut d = desk(1, DeskLimits::default());
    let cases = [hostile(2, 640.0, 0, 0, false), hostile(0, f32::NAN, 0, 0, false),
        hostile(0, -1.0, 0, 0, false), hostile(0, 640.0, 1, 0, false),
        hostile(0, 640.0, 0, 4, false), hostile(0, 640.0, 0, 0, true)];
    for (i, bytes) in cases.iter().enumerate() {
        assert!(d.restore_checkpoint(0, i as u64 + 1, bytes, alloc(100 + i as u64), || false).is_err());
        assert_eq!(d.active(), None); assert_eq!(d.revision(), 0);
    }
    d.restore_checkpoint(0, 7, &hostile(1, 640.0, 0, 0, false), alloc(200), || false).unwrap();
    assert!(d.panes().active_pane().unwrap().is_pinned);
}

#[test]
fn cancellation_at_final_publication_preserves_complete_previous_state() {
    let mut original = desk(1, DeskLimits::default()); open(&mut original, 1, 1, b"imported");
    let saved = save(&original, 2000);
    let mut probe = desk(2, DeskLimits::default());
    let mut count = 0;
    probe.restore_checkpoint(0, 1, saved.bytes(), alloc(2), || { count += 1; false }).unwrap();
    let mut target = desk(3, DeskLimits::default()); let pane = open(&mut target, 1, 1, b"previous");
    let mut calls = 0;
    assert_eq!(target.restore_checkpoint(1, 2, saved.bytes(), alloc(2), || { calls += 1; calls == count }),
        Err(CheckpointError::Desk(DeskError::Canceled)));
    assert_eq!(target.revision(), 1); assert_eq!(target.active(), Some(pane));
    assert_eq!(target.source(pane, 1).unwrap().bytes(), b"previous");
}

#[test]
fn import_limits_and_budget_refusal_are_atomic() {
    let mut source = desk(1, DeskLimits::default()); open(&mut source, 1, 1, b"four");
    let saved = save(&source, 2000);
    let mut limited = desk(2, DeskLimits { source_bytes: 3, ..DeskLimits::default() });
    assert_eq!(limited.restore_checkpoint(0, 1, saved.bytes(), alloc(2), || false), Err(CheckpointError::Limit));
    let mut target = desk(3, DeskLimits::default()); let pane = open(&mut target, 1, 1, b"previous");
    assert_eq!(target.restore_checkpoint(1, 2, saved.bytes(), alloc(1), || false), Err(CheckpointError::Desk(DeskError::ResourceDenied)));
    assert_eq!(target.active(), Some(pane)); assert_eq!(target.retained_source_count(), 1);
}

#[test]
fn stale_requests_and_identity_exhaustion_do_not_publish_a_checkpoint() {
    let mut source = desk(1, DeskLimits::default()); open(&mut source, 1, 1, b"x");
    let saved = save(&source, 2000);
    let mut target = desk(2, DeskLimits::default());
    assert_eq!(target.restore_checkpoint(1, 1, saved.bytes(), alloc(2), || false), Err(CheckpointError::Desk(DeskError::StaleRevision)));
    target.source_high_water = u64::MAX;
    assert_eq!(target.restore_checkpoint(0, 1, saved.bytes(), alloc(2), || false), Err(CheckpointError::Desk(DeskError::IdentityExhausted)));
    assert_eq!(target.revision(), 0);
    assert_eq!(target.restore_checkpoint(0, 1, saved.bytes(), alloc(2), || false), Err(CheckpointError::Desk(DeskError::StaleAttempt)));
}

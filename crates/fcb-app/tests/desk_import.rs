#![forbid(unsafe_code)]

use fcb::{ArenaOwnerId, ByteOffset, ByteRange, FileId, SourceCapture, SourceRevision};
use fcb_app::host::desk::{DeskSession, DeskLimits, DeskCommand, DeskError, DeskSessionError};
use fcb_app::host::desk::imports::{DeskImport, ImportedSourceId};

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn origin(o: u64, f: u64, r: u64) -> ImportedSourceId {
    ImportedSourceId { file: FileId::new(owner(o), f).unwrap(), revision: SourceRevision::new(owner(o), r).unwrap() }
}
fn range(a: u64, b: u64) -> ByteRange { ByteRange::new(ByteOffset::new(a), ByteOffset::new(b)).unwrap() }
fn session() -> DeskSession { DeskSession::new(owner(1), DeskLimits::default()).unwrap() }
fn import(d: &mut DeskSession, key: ImportedSourceId, bytes: &[u8], selection: ByteRange) -> DeskImport {
    d.import_source(d.model().revision(), d.model().last_attempt() + 1, key, "captured.rs", bytes,
        selection.start().get(), Some(selection), || false).unwrap()
}
fn apply(d: &mut DeskSession, command: DeskCommand) {
    d.apply(d.model().revision(), d.model().last_attempt() + 1, command, || false).unwrap();
}

#[test]
fn repeated_occurrences_share_one_capture_and_duplicate_panes_keep_selections() {
    let mut d = session(); let key = origin(2, 1, 1);
    let first = import(&mut d, key, b"one two", range(0, 3)); let pane = first.change.active.unwrap();
    assert!(!first.reused_capture); assert_ne!(first.file.owner(), key.file.owner());
    apply(&mut d, DeskCommand::Pin { pane, pinned: true });
    apply(&mut d, DeskCommand::Duplicate(pane));
    let other = d.model().active().unwrap();
    let next = import(&mut d, key, b"one two", range(4, 7));
    assert!(next.reused_capture); assert_eq!(next.change.active, Some(other));
    assert_eq!(first.file, next.file); assert_eq!(first.revision, next.revision);
    assert_eq!(d.model().retained_source_count(), 1);
    let rev = d.model().revision();
    assert_eq!(d.model().selected_bytes(pane, rev).unwrap(), b"one");
    assert_eq!(d.model().selected_bytes(other, rev).unwrap(), b"two");
    assert_eq!(d.model().source(pane, rev).unwrap().bytes().as_ptr(), d.model().source(other, rev).unwrap().bytes().as_ptr());
}

#[test]
fn closed_pane_history_only_capture_is_reused_without_a_live_path() {
    let mut d = session(); let key = origin(2, 8, 9);
    let first = import(&mut d, key, b"one two", range(0, 3));
    apply(&mut d, DeskCommand::Close(first.change.active.unwrap()));
    assert_eq!(d.model().active(), None);
    let next = import(&mut d, key, b"one two", range(4, 7));
    assert!(next.reused_capture); assert_eq!(d.model().retained_source_count(), 1);
    apply(&mut d, DeskCommand::Back);
    assert_eq!(d.model().selected_bytes(d.model().active().unwrap(), d.model().revision()).unwrap(), b"one");
}

#[test]
fn bookmark_only_sources_are_not_lost_or_duplicated_by_import() {
    let mut d = session(); let key = origin(2, 1, 1);
    let first = import(&mut d, key, b"note", range(0, 4)); let pane = first.change.active.unwrap();
    apply(&mut d, DeskCommand::Bookmark { pane, label: "why this matters".into() });
    apply(&mut d, DeskCommand::Close(pane)); apply(&mut d, DeskCommand::ClearHistory);
    assert!(d.model().history().is_empty()); assert_eq!(d.model().retained_source_count(), 1);
    assert!(import(&mut d, key, b"note", range(0, 4)).reused_capture);
    assert_eq!(d.model().bookmarks()[0].label(), "why this matters");
}

#[test]
fn provider_revisions_keep_file_relation_but_equal_ordinals_across_owners_do_not() {
    let mut d = session();
    let first = import(&mut d, origin(2, 1, 1), b"old", range(0, 3));
    let second = import(&mut d, origin(2, 1, 2), b"new", range(0, 3));
    let foreign = import(&mut d, origin(3, 1, 2), b"new", range(0, 3));
    assert_eq!(first.file, second.file); assert_ne!(first.revision, second.revision);
    assert_ne!(second.file, foreign.file); assert_eq!(d.model().retained_source_count(), 3);
    apply(&mut d, DeskCommand::Back); apply(&mut d, DeskCommand::Back);
    assert_eq!(d.model().selected_bytes(d.model().active().unwrap(), d.model().revision()).unwrap(), b"old");
}

#[test]
fn imported_ids_do_not_collide_with_already_adopted_local_source() {
    let mut d = session();
    let local = SourceCapture::from_bytes(owner(1), FileId::new(owner(1), 900).unwrap(),
        SourceRevision::new(owner(1), 950).unwrap(), "local.rs", b"local".to_vec()).unwrap();
    let pane = d.adopt(0, 1, local, 0, None, || false).unwrap().active.unwrap();
    apply(&mut d, DeskCommand::Pin { pane, pinned: true });
    let imported = import(&mut d, origin(2, 900, 950), b"remote", range(0, 6));
    assert!(imported.file.get() > 950); assert!(imported.revision.get() > 950);
    assert_eq!(d.model().source(pane, d.model().revision()).unwrap().bytes(), b"local");
}

#[test]
fn changed_bytes_or_label_under_same_identity_do_not_replace_the_accepted_source() {
    let mut d = session(); let key = origin(2, 1, 1); let first = import(&mut d, key, b"safe", range(0, 4));
    for (attempt, label, bytes) in [(2, "captured.rs", b"evil".as_slice()), (3, "renamed.rs", b"safe".as_slice())] {
        assert_eq!(d.import_source(1, attempt, key, label, bytes, 0, None, || false),
            Err(DeskSessionError::Desk(DeskError::IdentityConflict)));
        assert_eq!(d.model().revision(), 1);
    }
    assert_eq!(d.model().source(first.change.active.unwrap(), 1).unwrap().bytes(), b"safe");
}

#[test]
fn cancellation_at_each_import_checkpoint_preserves_old_desk_and_query() {
    let bytes = vec![b'x'; 128 * 1024 + 3]; let key = origin(2, 1, 1);
    let mut probe = session(); let mut calls = 0;
    probe.import_source(0, 1, key, "large", &bytes, 0, None, || { calls += 1; false }).unwrap();
    for cancel_at in 1..=calls {
        let mut d = session(); let old = import(&mut d, origin(3, 1, 1), b"needle", range(0, 6));
        let pane = old.change.active.unwrap(); d.search(1, pane, 1, "needle", 10, 100, || false).unwrap();
        let mut seen = 0;
        let result = d.import_source(1, 2, key, "large", &bytes, 0, None, || { seen += 1; seen == cancel_at });
        assert!(matches!(result, Err(DeskSessionError::Desk(DeskError::Canceled))));
        assert_eq!(d.model().revision(), 1); assert_eq!(d.model().active(), Some(pane));
        assert_eq!(d.accepted_query(), Some(1)); assert_eq!(d.model().retained_source_count(), 1);
    }
}

#[test]
fn eviction_releases_import_capacity_without_reusing_old_source_identities() {
    let mut d = session(); let key = origin(2, 1, 1);
    let first = import(&mut d, key, b"old", range(0, 3));
    apply(&mut d, DeskCommand::Close(first.change.active.unwrap())); apply(&mut d, DeskCommand::ClearHistory);
    assert_eq!(d.model().retained_source_bytes(), 0);
    let next = import(&mut d, key, b"old", range(0, 3));
    assert!(!next.reused_capture); assert_ne!(first.file, next.file); assert_ne!(first.revision, next.revision);
    assert_eq!(d.model().retained_source_count(), 1);
}

#[test]
fn import_bounds_and_mixed_owner_keys_are_rejected_without_mutation() {
    let mut d = DeskSession::new(owner(1), DeskLimits { source_bytes: 3, sources: 1, ..DeskLimits::default() }).unwrap();
    let key = origin(2, 1, 1);
    assert_eq!(d.import_source(0, 1, key, "a", b"four", 0, None, || false), Err(DeskSessionError::Desk(DeskError::SourceLimit)));
    let mixed = ImportedSourceId { file: key.file, revision: SourceRevision::new(owner(3), 1).unwrap() };
    assert_eq!(d.import_source(0, 2, mixed, "a", b"x", 0, None, || false), Err(DeskSessionError::Desk(DeskError::OwnerMismatch)));
    assert_eq!(d.import_source(0, 3, key, "a", b"x", 2, None, || false), Err(DeskSessionError::Desk(DeskError::InvalidLocation)));
    assert_eq!(d.model().revision(), 0);
    import(&mut d, key, b"one", range(0, 3));
    assert!(import(&mut d, key, b"one", range(1, 3)).reused_capture);
    assert_eq!(d.import_source(d.model().revision(), 10, origin(2, 2, 1), "b", b"x", 0, None, || false),
        Err(DeskSessionError::Desk(DeskError::SourceLimit)));
}

#[test]
fn retained_capture_lookup_checks_owner_revision_and_retirement() {
    let mut d = session(); let imported = import(&mut d, origin(2, 1, 1), b"source", range(0, 6));
    assert!(matches!(d.model().retained_capture(0, imported.file, imported.revision), Err(DeskError::StaleRevision)));
    assert!(matches!(d.model().retained_capture(1, origin(3, 1, 1).file, imported.revision), Err(DeskError::OwnerMismatch)));
    apply(&mut d, DeskCommand::Close(imported.change.active.unwrap()));
    assert_eq!(d.model().retained_capture(2, imported.file, imported.revision).unwrap().bytes(), b"source");
    apply(&mut d, DeskCommand::ClearHistory);
    assert!(matches!(d.model().retained_capture(3, imported.file, imported.revision), Err(DeskError::MissingSource)));
}

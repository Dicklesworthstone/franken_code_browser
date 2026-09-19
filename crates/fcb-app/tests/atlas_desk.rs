#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange};
use fcb_app::host::{atlas_session::{AtlasSession, AtlasSessionOptions},
    atlas_search::{RetainedAtlasSearch, AtlasSearchOptions, AtlasSearchError, AtlasDeskError},
    desk::{DeskSession, DeskLimits, DeskCommand, DeskError, DeskSessionError}};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-atlas-desk-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn desk(n: u64) -> DeskSession { DeskSession::new(owner(n), DeskLimits::default()).unwrap() }
fn atlas(root: &std::path::Path, n: u64) -> AtlasSession {
    AtlasSession::open(owner(n), root, AtlasSessionOptions::default(), || false).unwrap()
}
fn search(atlas: &AtlasSession) -> RetainedAtlasSearch {
    let mut s = RetainedAtlasSearch::new(atlas).unwrap();
    s.search(atlas, 1, "needle", AtlasSearchOptions::default(), || false).unwrap(); s
}
fn apply(d: &mut DeskSession, command: DeskCommand) {
    d.apply(d.model().revision(), d.model().last_attempt() + 1, command, || false).unwrap();
}
fn selected(d: &DeskSession) -> &[u8] {
    d.model().selected_bytes(d.model().active().unwrap(), d.model().revision()).unwrap()
}

#[test]
fn search_capture_enters_desk_and_checkpoint_without_reopening_replaced_source() {
    let root = fixture(); let path = root.join("a.rs"); let saved = root.join("reading.fcbk");
    fs::write(&path, b"old needle bytes").unwrap(); let a = atlas(&root, 2); let mut s = search(&a);
    fs::rename(&path, root.join("moved.rs")).unwrap(); fs::write(&path, b"different live file").unwrap();
    let mut d = desk(3); let imported = s.open_desk(&a, &mut d, 0, 1, 1, 1, || false).unwrap();
    let pane = imported.change.active.unwrap(); assert_eq!(selected(&d), b"needle");
    assert_eq!(d.model().source(pane, 1).unwrap().bytes(), b"old needle bytes");
    assert_ne!(imported.origin.file.owner(), imported.file.owner());
    apply(&mut d, DeskCommand::Pin { pane, pinned: true });
    apply(&mut d, DeskCommand::Bookmark { pane, label: "evidence from search".into() });
    s.clear(&a, 2, || false).unwrap(); drop(s); drop(a);
    assert_eq!(d.save_checkpoint(3, &saved, 100, || false).error(), None); drop(d);
    let mut reopened = desk(4); reopened.restore_checkpoint_file(0, 1, &saved, || false).unwrap();
    assert_eq!(selected(&reopened), b"needle");
    assert_eq!(reopened.model().bookmarks()[0].label(), "evidence from search");
    assert!(reopened.model().panes().active_pane().unwrap().is_pinned);
    assert_eq!(fs::read(&path).unwrap(), b"different live file");
}

#[test]
fn repeated_hit_activation_shares_capture_and_preserves_duplicate_selection() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle--needle").unwrap();
    let a = atlas(&root, 10); let s = search(&a); let mut d = desk(11);
    let first = s.open_desk(&a, &mut d, 0, 1, 1, 1, || false).unwrap(); let pane = first.change.active.unwrap();
    apply(&mut d, DeskCommand::Pin { pane, pinned: true }); apply(&mut d, DeskCommand::Duplicate(pane));
    let second = s.open_desk(&a, &mut d, 3, 4, 1, 2, || false).unwrap(); let other = second.change.active.unwrap();
    assert_ne!(pane, other); assert!(second.reused_capture); assert_eq!(first.file, second.file);
    assert_eq!(d.model().retained_source_count(), 1);
    assert_eq!(d.model().location(pane, 4).unwrap().selection.unwrap().start().get(), 0);
    assert_eq!(d.model().location(other, 4).unwrap().selection.unwrap().start().get(), 8);
    assert_eq!(d.model().source(pane, 4).unwrap().bytes().as_ptr(), d.model().source(other, 4).unwrap().bytes().as_ptr());
}

#[test]
fn provisional_occurrence_can_be_kept_even_when_the_running_query_is_canceled() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle A").unwrap(); fs::write(root.join("b.rs"), b"needle B").unwrap();
    let a = atlas(&root, 20); let mut s = RetainedAtlasSearch::new(&a).unwrap();
    s.begin(&a, 1, "needle", AtlasSearchOptions::default(), || false).unwrap();
    s.step(&a, 1, || false).unwrap();
    let progress = s.progress(&a, 1).unwrap(); assert!(progress.is_running()); assert_eq!(progress.retained_hits, 1);
    let mut d = desk(21); s.open_desk(&a, &mut d, 0, 1, 1, 1, || false).unwrap();
    assert!(s.cancel_pending()); assert_eq!(selected(&d), b"needle");
    assert!(s.open_desk(&a, &mut d, 1, 2, 1, 1, || false).is_err());
    assert_eq!(d.model().revision(), 1); assert_eq!(d.model().retained_source_count(), 1);
}

#[test]
fn new_query_revision_never_refreshes_an_old_pinned_reader() {
    let root = fixture(); let path = root.join("a.rs"); fs::write(&path, b"needle OLD").unwrap();
    let a = atlas(&root, 30); let mut s = search(&a); let mut d = desk(31);
    let old = s.open_desk(&a, &mut d, 0, 1, 1, 1, || false).unwrap(); let pane = old.change.active.unwrap();
    apply(&mut d, DeskCommand::Pin { pane, pinned: true }); fs::write(&path, b"needle NEW").unwrap();
    s.search(&a, 2, "needle", AtlasSearchOptions::default(), || false).unwrap();
    assert_eq!(s.open_desk(&a, &mut d, 2, 3, 1, 1, || false), Err(AtlasDeskError::Search(AtlasSearchError::StaleQuery)));
    let new = s.open_desk(&a, &mut d, 2, 4, 2, 1, || false).unwrap();
    assert_eq!(old.file, new.file); assert_ne!(old.revision, new.revision); assert_ne!(Some(pane), new.change.active);
    assert_eq!(d.model().source(pane, 4).unwrap().bytes(), b"needle OLD");
    assert_eq!(d.model().source(new.change.active.unwrap(), 4).unwrap().bytes(), b"needle NEW");
}

#[test]
fn wrong_atlas_missing_hit_and_stale_desk_revision_cannot_change_the_desk() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let a = atlas(&root, 40); let foreign = atlas(&root, 41); let s = search(&a); let mut d = desk(42);
    assert_eq!(s.open_desk(&foreign, &mut d, 0, 1, 1, 1, || false), Err(AtlasDeskError::Search(AtlasSearchError::WrongAtlas)));
    assert_eq!(s.open_desk(&a, &mut d, 0, 2, 1, 99, || false), Err(AtlasDeskError::Search(AtlasSearchError::MissingHit)));
    assert_eq!(s.open_desk(&a, &mut d, 5, 3, 1, 1, || false), Err(AtlasDeskError::Desk(DeskSessionError::Desk(DeskError::StaleRevision))));
    assert_eq!(d.model().revision(), 0); assert!(d.model().history().is_empty());
    s.open_desk(&a, &mut d, 0, 4, 1, 1, || false).unwrap(); assert_eq!(selected(&d), b"needle");
}

#[test]
fn utf16_hit_selection_and_entire_original_byte_capture_are_preserved() {
    let root = fixture(); let mut bytes = vec![0xff, 0xfe];
    for unit in "prefix\r\nneedle😀".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(root.join("wide.rs"), &bytes).unwrap(); let a = atlas(&root, 50); let s = search(&a); let mut d = desk(51);
    let result = s.open_desk(&a, &mut d, 0, 1, 1, 1, || false).unwrap();
    let expected: Vec<u8> = "needle".encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert_eq!(selected(&d), expected); assert_eq!(d.model().source(result.change.active.unwrap(), 1).unwrap().bytes(), bytes);
    assert_eq!(d.model().location(result.change.active.unwrap(), 1).unwrap().selection,
        Some(ByteRange::new(ByteOffset::new(18), ByteOffset::new(30)).unwrap()));
}

#[test]
fn final_transfer_cancellation_and_source_cap_preserve_existing_readers() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap(); let a = atlas(&root, 60); let s = search(&a);
    let mut probe = desk(61); let mut count = 0;
    s.open_desk(&a, &mut probe, 0, 1, 1, 1, || { count += 1; false }).unwrap();
    let mut target = desk(62); let mut calls = 0;
    assert_eq!(s.open_desk(&a, &mut target, 0, 1, 1, 1, || { calls += 1; calls == count }),
        Err(AtlasDeskError::Desk(DeskSessionError::Desk(DeskError::Canceled))));
    assert_eq!(target.model().revision(), 0); assert_eq!(target.model().retained_source_count(), 0);
    let mut small = DeskSession::new(owner(63), DeskLimits { source_bytes: 3, ..DeskLimits::default() }).unwrap();
    assert_eq!(s.open_desk(&a, &mut small, 0, 1, 1, 1, || false), Err(AtlasDeskError::Desk(DeskSessionError::Desk(DeskError::SourceLimit))));
    assert_eq!(small.model().revision(), 0);
}

#[test]
fn equal_catalog_ordinals_in_different_roots_do_not_alias_in_one_desk() {
    let left = fixture(); let right = fixture(); fs::write(left.join("same.rs"), b"needle LEFT").unwrap();
    fs::write(right.join("same.rs"), b"needle RIGHT").unwrap();
    let a = atlas(&left, 70); let b = atlas(&right, 71); let sa = search(&a); let sb = search(&b); let mut d = desk(72);
    let first = sa.open_desk(&a, &mut d, 0, 1, 1, 1, || false).unwrap(); let pane = first.change.active.unwrap();
    apply(&mut d, DeskCommand::Pin { pane, pinned: true });
    let second = sb.open_desk(&b, &mut d, 2, 3, 1, 1, || false).unwrap();
    assert_ne!(first.file, second.file); assert_eq!(d.model().retained_source_count(), 2);
    assert_eq!(d.model().source(pane, 3).unwrap().bytes(), b"needle LEFT");
    assert_eq!(d.model().source(second.change.active.unwrap(), 3).unwrap().bytes(), b"needle RIGHT");
}

#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, owner, entry};
use support::parse;
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::desk::{DeskSession, DeskLimits, DeskCommand, DeskError, DeskSessionError,
    document::{DeskDocument, DeskDocumentOptions}, comparison::{DeskComparison, DeskComparisonLimits}};
use fcb_app::host::saved_repository::{SavedRepositoryError, desk::SavedDeskError};

fn desk() -> DeskSession { DeskSession::new(owner(55100), DeskLimits::default()).unwrap() }
fn reads(saved: &mut fcb_app::host::saved_repository::SavedRepositorySession) -> u64 {
    parse(saved.info(|| false).unwrap().as_str().as_bytes()).unwrap().get("member_bytes_read").number()
}

#[test]
fn offline_hit_becomes_a_real_reader_preview_bookmark_and_restorable_checkpoint() {
    let raw = b"# Guide\n\nArchived **needle**.\n";
    let f = Fixture::new(&[entry(b"README.md", raw)], true);
    // No README.md exists in this directory; labels never become live reads.
    let mut saved = f.open(55101); let mut d = desk();
    saved.search(1, "needle", 10, || false).unwrap();
    let opened = saved.open_hit_desk(&mut d, 0, 1, 1, 1, || false).unwrap();
    let pane = opened.imported.change.active.unwrap();
    assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), b"needle");
    assert_eq!(d.model().source(pane, 1).unwrap().bytes(), raw);
    let receipt = saved.desk_open_response(&d, &opened).unwrap();
    let receipt = parse(receipt.as_str().as_bytes()).unwrap();
    assert!(!receipt.get("live_source_reopened").flag());
    assert_eq!(receipt.get("member_bytes_read").number(), raw.len() as u64);
    let doc = DeskDocument::prepare(&mut d, 1, pane, 1, DeskDocumentOptions::default(), || false).unwrap();
    assert_eq!(doc.layout(&d, 1, 1).unwrap().headings()[0].slug, "guide");
    d.apply(1, 2, DeskCommand::Bookmark { pane, label: "archived evidence".into() }, || false).unwrap();
    let path = f.root.join("desk.fcbk");
    assert_eq!(d.save_checkpoint(2, &path, 1024, || false).error(), None);
    drop(saved); drop(doc); drop(d);
    fs::rename(&f.path, f.root.join("moved.fcbs")).unwrap();
    let mut restored = desk();
    let pane = restored.restore_checkpoint_file(0, 1, &path, || false).unwrap().active.unwrap();
    assert_eq!(restored.model().selected_bytes(pane, 1).unwrap(), b"needle");
    assert_eq!(restored.model().bookmarks()[0].label(), "archived evidence");
    assert_eq!(parse(restored.state(|| false).unwrap().as_str().as_bytes()).unwrap().get("initial_source_bytes_read").number(), 0);
}

#[test]
fn repeated_hits_queries_and_direct_member_open_reuse_one_import_identity() {
    let f = Fixture::new(&[entry(b"a", b"needle needle")], true); let mut s = f.open(55101);
    let mut d = DeskSession::new(owner(55100), DeskLimits { sources: 1, ..Default::default() }).unwrap();
    s.search(1, "needle", 10, || false).unwrap();
    let first = s.open_hit_desk(&mut d, 0, 1, 1, 1, || false).unwrap();
    let second = s.open_hit_desk(&mut d, 1, 2, 1, 2, || false).unwrap();
    assert!(second.imported.reused_capture); assert_eq!(first.imported.file, second.imported.file);
    s.search(2, "needle", 10, || false).unwrap();
    let third = s.open_hit_desk(&mut d, 2, 3, 2, 1, || false).unwrap();
    assert!(third.imported.reused_capture);
    let fourth = s.open_member_desk(&mut d, 3, 4, 0, || false).unwrap();
    assert!(fourth.imported.reused_capture); assert_eq!(fourth.imported.file, first.imported.file);
    assert_eq!(d.model().retained_source_count(), 1); assert_eq!(d.model().retained_source_bytes(), 13);
    assert_eq!(fourth.selection(), None);
}

#[test]
fn stale_query_hit_and_desk_revision_fail_before_member_io() {
    let f = Fixture::new(&[entry(b"a", b"needle")], true); let mut s = f.open(55101); let mut d = desk();
    s.search(1, "needle", 10, || false).unwrap(); let before = reads(&mut s);
    assert_eq!(s.open_hit_desk(&mut d, 0, 1, 2, 1, || false).err(), Some(SavedDeskError::Saved(SavedRepositoryError::StaleQuery)));
    assert_eq!(s.open_hit_desk(&mut d, 0, 1, 1, 0, || false).err(), Some(SavedDeskError::Saved(SavedRepositoryError::MissingHit)));
    assert_eq!(s.open_member_desk(&mut d, 1, 1, 0, || false).err(), Some(SavedDeskError::Desk(DeskSessionError::Desk(DeskError::StaleRevision))));
    assert_eq!(reads(&mut s), before); assert_eq!(d.model().revision(), 0);
}

#[test]
fn changed_archive_member_cannot_replace_an_already_imported_source() {
    let raw = b"unique needle source"; let f = Fixture::new(&[entry(b"a", raw)], true);
    let mut s = f.open(55101); let mut d = desk(); s.search(1, "needle", 10, || false).unwrap();
    let pane = s.open_hit_desk(&mut d, 0, 1, 1, 1, || false).unwrap().imported.change.active.unwrap();
    let at = f.bytes.windows(raw.len()).position(|b| b == raw).unwrap();
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(at as u64)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(s.open_hit_desk(&mut d, 1, 2, 1, 1, || false).is_err());
    assert_eq!(d.model().revision(), 1); assert_eq!(d.model().source(pane, 1).unwrap().bytes(), raw);
    assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), b"needle"); assert_eq!(s.accepted_generation(), Some(1));
}

#[test]
fn canceled_import_at_final_checkpoint_preserves_old_reader_and_results() {
    let f = Fixture::new(&[entry(b"a", b"old"), entry(b"b", b"needle")], true);
    let mut probe = f.open(55101); let mut pd = desk(); let mut polls = 0;
    probe.open_member_desk(&mut pd, 0, 1, 1, || { polls += 1; false }).unwrap();
    let mut s = f.open(55102); let mut d = desk();
    let pane = s.open_member_desk(&mut d, 0, 1, 0, || false).unwrap().imported.change.active.unwrap();
    // Count this exact replacement route independently; no stateful pulse is assumed.
    let mut count_probe = f.open(55103); let mut cd = desk();
    count_probe.open_member_desk(&mut cd, 0, 1, 0, || false).unwrap();
    let mut replacement_polls = 0;
    count_probe.open_member_desk(&mut cd, 1, 2, 1, || { replacement_polls += 1; false }).unwrap();
    let mut seen = 0;
    let result = s.open_member_desk(&mut d, 1, 2, 1, || { seen += 1; seen == replacement_polls });
    assert!(polls > 0); assert!(matches!(result, Err(e) if e.is_canceled()));
    assert_eq!(d.model().revision(), 1); assert_eq!(d.model().source(pane, 1).unwrap().bytes(), b"old");
    assert_eq!(d.model().retained_source_count(), 1);
}

#[test]
fn unavailable_is_not_empty_and_oversized_members_are_refused_before_load() {
    let f = Fixture::new(&[entry(b"a", b""), SnapshotEntry { path: b"b", observed_bytes: 99,
        data: SnapshotData::Unavailable("READ_DENIED") }, entry(b"c", b"long")], true);
    let mut s = f.open(55101);
    let mut d = DeskSession::new(owner(55100), DeskLimits { source_bytes: 3, ..Default::default() }).unwrap();
    assert!(s.open_member_desk(&mut d, 0, 1, 1, || false).is_err());
    assert!(s.open_member_desk(&mut d, 0, 1, 2, || false).is_err());
    assert_eq!(reads(&mut s), 0);
    let pane = s.open_member_desk(&mut d, 0, 1, 0, || false).unwrap().imported.change.active.unwrap();
    assert_eq!(d.model().source(pane, 1).unwrap().bytes(), b"");
}

#[test]
fn utf16_overlapping_hits_preserve_original_offsets_and_raw_member_names() {
    let mut raw = vec![0xff, 0xfe]; for c in "banana".encode_utf16() { raw.extend(c.to_le_bytes()); }
    let f = Fixture::new(&[entry(b"raw-\xff.rs", &raw)], true); let mut s = f.open(55101); let mut d = desk();
    s.search(1, "ana", 10, || false).unwrap();
    for (id, start) in [(1, 4), (2, 8)] {
        let opened = s.open_hit_desk(&mut d, id - 1, id, 1, id, || false).unwrap();
        let pane = opened.imported.change.active.unwrap();
        assert_eq!(opened.selection().unwrap().start().get(), start);
        assert_eq!(d.model().selected_bytes(pane, id).unwrap(), b"a\0n\0a\0");
        assert_eq!(d.model().source(pane, id).unwrap().bytes(), raw);
    }
}

#[test]
fn different_archives_with_equal_ordinals_remain_independent_comparison_sources() {
    let a = Fixture::new(&[entry(b"same.rs", b"old needle")], true);
    let b = Fixture::new(&[entry(b"same.rs", b"new needle")], true);
    let mut sa = a.open(55101); let mut sb = b.open(55102); let mut d = desk();
    let old = sa.open_member_desk(&mut d, 0, 1, 0, || false).unwrap().imported.change.active.unwrap();
    d.apply(1, 2, DeskCommand::Pin { pane: old, pinned: true }, || false).unwrap();
    let new = sb.open_member_desk(&mut d, 2, 3, 0, || false).unwrap().imported.change.active.unwrap();
    assert_ne!(d.model().source(old, 3).unwrap().file(), d.model().source(new, 3).unwrap().file());
    drop(sa); drop(sb);
    let comparison = DeskComparison::prepare(&mut d, 3, old, new, 1, DeskComparisonLimits::default(), || false).unwrap();
    assert!(comparison.is_complete());
    assert_eq!(comparison.relation(), fcb::analysis::comparison::ComparisonRelation::Different);
}

#[test]
fn persisted_index_queries_import_exact_hits_and_survive_index_detachment() {
    let f = Fixture::new(&[entry(b"a", b"needle"), entry(b"b", b"unrelated")], true);
    let (path, pin) = f.index(); let mut s = f.open(55101); let mut d = desk();
    s.attach_index(1, &path, pin, || false).unwrap();
    let result = s.search(1, "needle", 10, || false).unwrap(); assert_eq!(result.exit_code(), EXIT_OK);
    assert_eq!(parse(result.as_str().as_bytes()).unwrap().get("member_bytes_read").number(), 6);
    s.detach_index(2, || false).unwrap();
    let pane = s.open_hit_desk(&mut d, 0, 1, 1, 1, || false).unwrap().imported.change.active.unwrap();
    assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), b"needle");
}

#[test]
fn partial_search_can_open_a_verified_hit_without_claiming_complete_coverage() {
    let f = Fixture::new(&[entry(b"a", b"needle needle")], false); let mut s = f.open(55101); let mut d = desk();
    let result = s.search(1, "needle", 1, || false).unwrap(); assert_eq!(result.exit_code(), EXIT_PARTIAL);
    assert!(!parse(result.as_str().as_bytes()).unwrap().get("search_complete").flag());
    let opened = s.open_hit_desk(&mut d, 0, 1, 1, 1, || false).unwrap();
    assert_eq!(d.model().selected_bytes(opened.imported.change.active.unwrap(), 1).unwrap(), b"needle");
    assert_eq!(s.results(1, 0, 10, || false).unwrap().exit_code(), EXIT_PARTIAL);
}

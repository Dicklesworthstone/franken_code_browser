#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Offline Quick Open and structural witnesses enter ordinary Desk history.
//! Authored for independent RCH execution; no live member paths are created.
mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, entry, owner};
use support::{Json, parse};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::host::{HostResponse, desk::{DeskSession, DeskLimits, DeskCommand, DeskError, DeskSessionError},
    saved_repository::{SavedRepositorySession, SavedRepositoryError, desk::SavedDeskError,
        paths::SavedPathOptions, structure::{StructureMode, SymbolNameMode}}};

fn desk() -> DeskSession { DeskSession::new(owner(57700), DeskLimits::default()).unwrap() }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn reads(saved: &mut SavedRepositorySession) -> u64 {
    json(&saved.info(|| false).unwrap()).get("member_bytes_read").number()
}
fn paths(saved: &mut SavedRepositorySession, needle: &[u8]) {
    saved.path_search(1, needle, SavedPathOptions::default(), || false).unwrap();
}
fn occurrences(saved: &mut SavedRepositorySession) {
    saved.structure_search(1, StructureMode::Occurrences, SymbolNameMode::Exact,
        "needle", 16, 4096, || false).unwrap();
}

#[test]
fn path_symbol_and_literal_share_one_capture_at_capacity_and_receipts_outlive_queries() {
    let raw = b"fn target() {}\n// target\n";
    let f = Fixture::new(&[entry(b"src/source.rs", raw)], true);
    let mut saved = f.open(57701);
    let mut d = DeskSession::new(owner(57700), DeskLimits { sources: 1, ..Default::default() }).unwrap();
    paths(&mut saved, b"source.rs");
    assert_eq!(reads(&mut saved), 0);
    let path = saved.open_path_hit_desk(&mut d, 0, 1, 1, 1, || false).unwrap();
    assert_eq!(path.selection(), None);
    let receipt = json(&saved.desk_open_response(&d, &path).unwrap());
    assert_eq!(receipt.get("selection_namespace").text(), "saved-paths");
    assert_eq!(receipt.get("path_generation").number(), 1);
    assert_eq!(receipt.get("path_hit_id").number(), 1);
    assert_eq!(receipt.get("query_generation"), &Json::Null);
    assert_eq!(receipt.get("selection"), &Json::Null);
    saved.structure_search(1, StructureMode::Symbols, SymbolNameMode::Exact,
        "target", 16, 4096, || false).unwrap();
    let symbol = saved.open_structure_hit_desk(&mut d, 1, 2, 1, 1, || false).unwrap();
    assert!(symbol.imported.reused_capture);
    assert_eq!(symbol.imported.origin, path.imported.origin);
    assert_eq!(symbol.imported.file, path.imported.file);
    let pane = symbol.imported.change.active.unwrap();
    assert_eq!(d.model().selected_bytes(pane, 2).unwrap(), b"target");
    // Encoding an accepted receipt must not consult a replacement query.
    saved.clear_structure(2, || false).unwrap();
    let receipt = json(&saved.desk_open_response(&d, &symbol).unwrap());
    assert_eq!(receipt.get("structure_generation").number(), 1);
    assert_eq!(receipt.get("structure_hit_id").number(), 1);
    assert_eq!(receipt.get("selection_namespace").text(), "saved-structure");
    assert_eq!(receipt.get("structure").get("requested_mode").text(), "symbols");
    assert_eq!(receipt.get("structure").get("evidence_level").text(), "heuristic-outline-candidate");
    assert!(!receipt.get("structure").get("semantic_complete").flag());
    assert!(receipt.get("structure").get("evidence_range").get("start").number()
        <= symbol.selection().unwrap().start().get());
    assert!(receipt.get("structure").get("evidence_range").get("end").number()
        >= symbol.selection().unwrap().end().get());
    saved.search(1, "target", 16, || false).unwrap();
    let literal = saved.open_hit_desk(&mut d, 2, 3, 1, 2, || false).unwrap();
    assert!(literal.imported.reused_capture);
    assert_eq!(literal.imported.file, symbol.imported.file);
    assert_eq!(d.model().retained_source_count(), 1);
    assert_eq!(d.model().retained_source_bytes(), raw.len() as u64);
    assert_eq!(saved.accepted_path_generation(), Some(1));
    assert_eq!(saved.accepted_generation(), Some(1));
}

#[test]
fn later_utf16_occurrence_and_native_filename_survive_archive_detachment_and_checkpoint() {
    for big in [false, true] {
        let mut raw = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
        for word in "needle needle".encode_utf16() {
            raw.extend_from_slice(&if big { word.to_be_bytes() } else { word.to_le_bytes() });
        }
        let f = Fixture::new(&[entry(b"raw-\xff.rs", &raw)], true);
        let mut saved = f.open(57702 + u64::from(big)); let mut d = desk();
        paths(&mut saved, b"raw-\xff.rs");
        let first = saved.open_path_hit_desk(&mut d, 0, 1, 1, 1, || false).unwrap();
        let result = json(&saved.structure_search_page(1, StructureMode::Occurrences,
            SymbolNameMode::Exact, "needle", 1, 4096, 1, || false).unwrap());
        assert_eq!(result.get("hits").array()[0].get("hit_id").number(), 1);
        let opened = saved.open_structure_hit_desk(&mut d, 1, 2, 1, 1, || false).unwrap();
        let pane = opened.imported.change.active.unwrap();
        assert!(opened.imported.reused_capture);
        assert_eq!(opened.imported.file, first.imported.file);
        assert_eq!(opened.selection().unwrap().start().get(), 16);
        assert_eq!(opened.selection().unwrap().end().get(), 28);
        assert_eq!(d.model().selected_bytes(pane, 2).unwrap(), &raw[16..28]);
        let receipt = json(&saved.desk_open_response(&d, &opened).unwrap());
        assert_eq!(receipt.get("structure").get("evidence_level").text(), "whole-token-text-candidate");
        d.apply(2, 3, DeskCommand::Bookmark { pane, label: "second occurrence".into() }, || false).unwrap();
        let checkpoint = f.root.join("desk.fcbk");
        assert_eq!(d.save_checkpoint(3, &checkpoint, 4096, || false).error(), None);
        drop(saved); drop(d);
        fs::rename(&f.path, f.root.join("detached.fcbs")).unwrap();
        let mut restored = desk();
        let pane = restored.restore_checkpoint_file(0, 1, &checkpoint, || false).unwrap().active.unwrap();
        assert_eq!(restored.model().selected_bytes(pane, 1).unwrap(), &raw[16..28]);
        assert_eq!(restored.model().source(pane, 1).unwrap().bytes(), raw);
        assert_eq!(restored.model().bookmarks()[0].label(), "second occurrence");
    }
}

#[test]
fn stale_missing_unavailable_and_oversized_navigation_reject_before_payload_io() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle"), SnapshotEntry { path: b"b.rs",
        observed_bytes: 10, data: SnapshotData::Unavailable("READ_DENIED") }], true);
    let mut saved = f.open(57704); let mut d = desk();
    paths(&mut saved, b".rs"); occurrences(&mut saved);
    let before = reads(&mut saved);
    assert_eq!(saved.open_path_hit_desk(&mut d, 0, 1, 2, 1, || false).err(),
        Some(SavedDeskError::Saved(SavedRepositoryError::StaleQuery)));
    assert_eq!(saved.open_structure_hit_desk(&mut d, 0, 1, 2, 1, || false).err(),
        Some(SavedDeskError::Saved(SavedRepositoryError::StaleQuery)));
    assert_eq!(saved.open_structure_hit_desk(&mut d, 0, 1, 1, 0, || false).err(),
        Some(SavedDeskError::Saved(SavedRepositoryError::MissingHit)));
    assert_eq!(saved.open_path_hit_desk(&mut d, 1, 1, 1, 1, || false).err(),
        Some(SavedDeskError::Desk(DeskSessionError::Desk(DeskError::StaleRevision))));
    assert!(saved.open_path_hit_desk(&mut d, 0, 1, 1, 2, || false).is_err());
    let mut small = DeskSession::new(owner(57705), DeskLimits { source_bytes: 3, ..Default::default() }).unwrap();
    assert!(saved.open_path_hit_desk(&mut small, 0, 1, 1, 1, || false).is_err());
    assert!(saved.open_structure_hit_desk(&mut small, 0, 1, 1, 1, || false).is_err());
    assert_eq!(reads(&mut saved), before);
    assert_eq!(d.model().revision(), 0); assert_eq!(small.model().revision(), 0);
}

#[test]
fn cancellation_before_acceptance_preserves_current_source_and_forward_history() {
    let f = Fixture::new(&[entry(b"a.rs", b"old"), entry(b"b.rs", b"needle")], true);
    let setup = || {
        let mut saved = f.open(57706); let mut d = desk();
        paths(&mut saved, b"b.rs"); occurrences(&mut saved);
        saved.open_member_desk(&mut d, 0, 1, 0, || false).unwrap();
        saved.open_member_desk(&mut d, 1, 2, 1, || false).unwrap();
        d.apply(2, 3, DeskCommand::Back, || false).unwrap();
        (saved, d)
    };
    for structural in [false, true] {
        let (mut probe, mut probe_desk) = setup(); let mut polls = 0;
        if structural {
            probe.open_structure_hit_desk(&mut probe_desk, 3, 4, 1, 1, || { polls += 1; false }).unwrap();
        } else {
            probe.open_path_hit_desk(&mut probe_desk, 3, 4, 1, 1, || { polls += 1; false }).unwrap();
        }
        assert!(polls > 0);
        let (mut saved, mut d) = setup(); let mut at = 0;
        let result = if structural {
            saved.open_structure_hit_desk(&mut d, 3, 4, 1, 1, || { at += 1; at == polls })
        } else {
            saved.open_path_hit_desk(&mut d, 3, 4, 1, 1, || { at += 1; at == polls })
        };
        assert!(matches!(result, Err(error) if error.is_canceled()));
        assert_eq!(d.model().revision(), 3);
        assert_eq!(d.model().source(d.model().active().unwrap(), 3).unwrap().bytes(), b"old");
        assert_eq!(saved.accepted_path_generation(), Some(1));
        assert_eq!(saved.accepted_structure_generation(), Some(1));
        d.apply(3, 5, DeskCommand::Forward, || false).unwrap();
        assert_eq!(d.model().source(d.model().active().unwrap(), 5).unwrap().bytes(), b"needle");
    }
}

#[test]
fn changed_member_and_full_desk_leave_previously_accepted_evidence_intact() {
    let raw = b"unique needle source";
    let f = Fixture::new(&[entry(b"a.rs", raw), entry(b"b.rs", b"other")], true);
    let mut saved = f.open(57707);
    let mut d = DeskSession::new(owner(57700), DeskLimits { sources: 1, ..Default::default() }).unwrap();
    occurrences(&mut saved);
    let opened = saved.open_structure_hit_desk(&mut d, 0, 1, 1, 1, || false).unwrap();
    let pane = opened.imported.change.active.unwrap();
    paths(&mut saved, b"b.rs");
    assert!(saved.open_path_hit_desk(&mut d, 1, 2, 1, 1, || false).is_err());
    assert_eq!(d.model().revision(), 1);
    let offset = f.bytes.windows(raw.len()).position(|bytes| bytes == raw).unwrap();
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(offset as u64)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(saved.open_structure_hit_desk(&mut d, 1, 3, 1, 1, || false).is_err());
    assert_eq!(d.model().revision(), 1);
    assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), b"needle");
    assert_eq!(d.model().source(pane, 1).unwrap().bytes(), raw);
    assert_eq!(d.model().retained_source_count(), 1);
    assert_eq!(saved.accepted_structure_generation(), Some(1));
}

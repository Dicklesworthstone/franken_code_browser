#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{parse, Json};
use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange, store::envelope::Sha256};
use fcb_app::{EXIT_OK, EXIT_PARTIAL, EXIT_CANCELED};
use fcb_app::host::desk::{DeskSession, DeskLimits, DeskCommand, DeskPaneId,
    export::DeskSourcePackLimits, persistence::{CheckpointIoError, CheckpointSaveEffect}};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-desk-export-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn session() -> DeskSession { DeskSession::new(ArenaOwnerId::new(811).unwrap(), DeskLimits::default()).unwrap() }
fn apply(s: &mut DeskSession, command: DeskCommand) {
    s.apply(s.model().revision(), s.model().last_attempt() + 1, command, || false).unwrap();
}
fn open(s: &mut DeskSession, path: &Path) -> DeskPaneId {
    s.open_file(s.model().revision(), s.model().last_attempt() + 1, path, || false).unwrap().active.unwrap()
}
fn select(s: &mut DeskSession, pane: DeskPaneId, start: u64, end: u64) {
    apply(s, DeskCommand::Navigate { pane, offset: start,
        selection: Some(ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()) });
}
fn note(s: &mut DeskSession, pane: DeskPaneId, label: &str, body: &str) {
    apply(s, DeskCommand::Annotate { pane, label: label.into(), body: body.into() });
}

#[test]
fn export_preserves_exact_closed_capture_selections_full_notes_and_provenance() {
    let root = fixture(); let source = root.join("evidence.rs"); let destination = root.join("pack.json");
    fs::write(&source, b"private-prefix\xff\0\xc3private-suffix").unwrap();
    let mut s = session(); let pane = open(&mut s, &source);
    select(&mut s, pane, 14, 17);
    let body = "Why e\u{301}?\n🙂\0full tail";
    note(&mut s, pane, "Selected evidence", body);
    note(&mut s, pane, "Repeated visit", "same selection, separate reason");
    let location = s.model().bookmarks()[0].location();
    apply(&mut s, DeskCommand::Close(pane));
    fs::rename(&source, root.join("moved.rs")).unwrap();
    fs::write(&source, b"unrelated replacement").unwrap();
    let revision = s.model().revision(); let attempt = s.model().last_attempt();
    let saved = s.export_source_pack(revision, &destination, DeskSourcePackLimits::default(), || false);
    assert_eq!(saved.error(), None); assert_eq!(saved.exit_code(), EXIT_OK);
    assert_eq!(saved.effect(), CheckpointSaveEffect::DirectorySynced);
    let bytes = fs::read(&destination).unwrap(); let pack = parse(&bytes).unwrap();
    assert_eq!(pack.get("schema").text(), "fcb.desk-source-pack/1");
    assert_eq!(saved.bytes_written(), bytes.len() as u64);
    assert_eq!(saved.document_scalars(), std::str::from_utf8(&bytes).unwrap().chars().count() as u64);
    assert_eq!(saved.digest(), Some(Sha256::digest(&bytes)));
    assert_eq!(saved.coverage().selected_original_bytes, 6);
    assert!(pack.get("complete").flag()); assert!(!pack.get("live_roots_accessed").flag());
    assert!(!pack.get("source_labels_are_path_authority").flag());
    let entries = pack.get("entries").array(); assert_eq!(entries.len(), 2);
    for (ordinal, entry) in entries.iter().enumerate() {
        assert_eq!(entry.get("ordinal").number(), ordinal as u64);
        assert_eq!(entry.get("selected_original_hex").text(), "ff00c3");
        assert_eq!(entry.get("source_owner").number(), s.model().owner().get());
        assert_eq!(entry.get("file_id").number(), location.file.get());
        assert_eq!(entry.get("source_revision").number(), location.revision.get());
        assert_eq!(entry.get("selection").get("start").number(), 14);
        assert_eq!(entry.get("selection").get("end").number(), 17);
        assert_eq!(entry.get("bookmark_update_sequence").number(), 1);
        assert_eq!(entry.get("reason"), &Json::Null);
    }
    assert_eq!(entries[0].get("body").text().as_bytes(), body.as_bytes());
    assert!(!std::str::from_utf8(&bytes).unwrap().contains("private-prefix"));
    assert!(!std::str::from_utf8(&bytes).unwrap().contains("unrelated replacement"));
    assert_eq!(s.model().revision(), revision); assert_eq!(s.model().last_attempt(), attempt);
    let response = s.source_pack_response(&saved).unwrap(); let receipt = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(receipt.get("command").text(), "export");
    assert_eq!(receipt.get("export_digest").text(), Sha256::digest(&bytes).to_hex());
    assert!(!receipt.get("power_loss_qualified").flag());
    apply(&mut s, DeskCommand::EditBookmark { bookmark: 1, label: "Edited later".into(), body: "new note".into() });
    let delayed = s.source_pack_response(&saved).unwrap();
    assert_eq!(parse(delayed.as_str().as_bytes()).unwrap().get("model_revision").number(), revision,
        "a delayed receipt must retain the exported model identity");
    let mut foreign = DeskSession::new(ArenaOwnerId::new(812).unwrap(), DeskLimits::default()).unwrap();
    assert!(foreign.source_pack_response(&saved).is_err(), "another desk cannot relabel an export receipt");
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&destination).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn source_budget_omits_whole_selections_but_keeps_every_note_and_later_smaller_selection() {
    let root = fixture(); let source = root.join("source"); fs::write(&source, b"0123456789").unwrap();
    let mut s = session(); let pane = open(&mut s, &source);
    note(&mut s, pane, "Caret only", "never infer the entire file");
    select(&mut s, pane, 0, 8); note(&mut s, pane, "Too large", "retain rationale");
    select(&mut s, pane, 8, 10); note(&mut s, pane, "Fits", "retain this too");
    let destination = root.join("bounded.json");
    let saved = s.export_source_pack(s.model().revision(), &destination,
        DeskSourcePackLimits { source_bytes: 2, ..Default::default() }, || false);
    assert_eq!(saved.error(), None); assert_eq!(saved.exit_code(), EXIT_PARTIAL);
    assert_eq!(saved.coverage().omitted_no_selection, 1); assert_eq!(saved.coverage().omitted_source_limit, 1);
    assert_eq!(saved.coverage().exported_selections, 1); assert_eq!(saved.coverage().selected_original_bytes, 2);
    let pack = parse(&fs::read(&destination).unwrap()).unwrap(); let entries = pack.get("entries").array();
    assert_eq!(entries[0].get("reason").text(), "DESK_EXPORT_NO_SAVED_SELECTION");
    assert_eq!(entries[1].get("reason").text(), "DESK_EXPORT_SOURCE_BYTE_LIMIT");
    assert_eq!(entries[1].get("selected_original_hex"), &Json::Null);
    assert_eq!(entries[1].get("body").text(), "retain rationale");
    assert_eq!(entries[2].get("selected_original_hex").text(), "3839");
    assert!(!pack.get("complete").flag());
}

#[test]
fn serialized_byte_and_scalar_limits_include_escaped_metadata_and_hex_expansion() {
    for scalar_limited in [false, true] {
        let root = fixture(); let source = root.join("source"); fs::write(&source, vec![0xff; 6000]).unwrap();
        let mut s = session(); let pane = open(&mut s, &source); select(&mut s, pane, 0, 6000);
        let body = "é\0\n".repeat(100); note(&mut s, pane, "Full note", &body);
        let limits = DeskSourcePackLimits { source_bytes: 6000,
            output_bytes: if scalar_limited { 64 * 1024 } else { 12 * 1024 },
            output_scalars: if scalar_limited { 12 * 1024 } else { 64 * 1024 } };
        let destination = root.join("budget.json");
        let saved = s.export_source_pack(s.model().revision(), &destination, limits, || false);
        assert_eq!(saved.error(), None); assert_eq!(saved.coverage().omitted_output_limit, 1);
        let bytes = fs::read(&destination).unwrap(); let pack = parse(&bytes).unwrap();
        assert!(bytes.len() <= limits.output_bytes);
        assert!(std::str::from_utf8(&bytes).unwrap().chars().count() <= limits.output_scalars);
        let entry = &pack.get("entries").array()[0]; assert_eq!(entry.get("body").text().as_bytes(), body.as_bytes());
        assert_eq!(entry.get("reason").text(), if scalar_limited { "DESK_EXPORT_OUTPUT_SCALAR_LIMIT" } else { "DESK_EXPORT_OUTPUT_BYTE_LIMIT" });
        assert_eq!(entry.get("selected_original_hex"), &Json::Null);
    }
}

#[test]
fn metadata_and_invalid_budget_refusals_create_no_destination() {
    let root = fixture(); let source = root.join("source"); fs::write(&source, b"x").unwrap();
    let mut s = session(); let pane = open(&mut s, &source);
    note(&mut s, pane, "Full body or no export", &"\0".repeat(16 * 1024));
    for (ordinal, limits) in [
        DeskSourcePackLimits { source_bytes: 0, output_bytes: 4096, output_scalars: 4096 },
        DeskSourcePackLimits { output_scalars: 0, ..Default::default() },
        DeskSourcePackLimits { output_bytes: 16 * 1024 * 1024 + 1, ..Default::default() },
    ].into_iter().enumerate() {
        let destination = root.join(format!("refused-{ordinal}.json"));
        let saved = s.export_source_pack(s.model().revision(), &destination, limits, || false);
        assert!(saved.error().is_some()); assert_eq!(saved.effect(), CheckpointSaveEffect::None);
        assert_eq!(saved.bytes_written(), 0); assert_eq!(saved.document_bytes(), 0); assert!(!destination.exists());
    }
    let destination = root.join("full-metadata.json");
    let saved = s.export_source_pack(s.model().revision(), &destination,
        DeskSourcePackLimits { source_bytes: 0, ..Default::default() }, || false);
    assert_eq!(saved.error(), None);
    let pack = parse(&fs::read(&destination).unwrap()).unwrap();
    assert_eq!(pack.get("entries").array()[0].get("body").text().len(), 16 * 1024);
}

#[test]
fn existing_destination_stale_review_and_precancellation_have_no_write_effect() {
    let root = fixture(); let destination = root.join("existing.json"); fs::write(&destination, b"prior evidence").unwrap();
    let mut s = session(); let limits = DeskSourcePackLimits::default();
    let existing = s.export_source_pack(0, &destination, limits, || false);
    assert_eq!(existing.error(), Some(CheckpointIoError::DestinationExists));
    assert_eq!(existing.effect(), CheckpointSaveEffect::None); assert_eq!(fs::read(&destination).unwrap(), b"prior evidence");
    #[cfg(unix)] {
        let link = root.join("symlink.json"); std::os::unix::fs::symlink(&destination, &link).unwrap();
        assert!(s.export_source_pack(0, &link, limits, || false).error().is_some());
        assert_eq!(fs::read(&destination).unwrap(), b"prior evidence");
    }
    let absent = root.join("not-created.json");
    assert!(s.export_source_pack(1, &absent, limits, || false).error().is_some());
    let canceled = s.export_source_pack(0, &absent, limits, || true);
    assert_eq!(canceled.exit_code(), EXIT_CANCELED); assert!(!absent.exists());
}

#[test]
fn cancellation_after_create_or_written_prefix_keeps_actual_effect_and_prevents_replay() {
    for after_prefix in [false, true] {
        let root = fixture(); let source = root.join("source"); fs::write(&source, vec![b'a'; 40_000]).unwrap();
        let mut s = session(); let pane = open(&mut s, &source); select(&mut s, pane, 0, 40_000);
        note(&mut s, pane, "large evidence", "all or explicit incomplete write");
        let destination = root.join("interrupted.json"); let revision = s.model().revision();
        let saved = s.export_source_pack(revision, &destination, DeskSourcePackLimits::default(), || {
            if after_prefix { fs::metadata(&destination).is_ok_and(|m| m.len() > 0) } else { destination.exists() }
        });
        assert_eq!(saved.exit_code(), EXIT_CANCELED); assert_eq!(saved.effect(), CheckpointSaveEffect::Created);
        assert_eq!(saved.bytes_written(), if after_prefix { 64 * 1024 } else { 0 });
        assert_eq!(saved.bytes_written(), fs::metadata(&destination).unwrap().len());
        assert!(saved.document_bytes() > saved.bytes_written());
        let response = s.source_pack_response(&saved).unwrap(); let receipt = parse(response.as_str().as_bytes()).unwrap();
        assert_eq!(receipt.get("status").text(), "error");
        assert_eq!(receipt.get("bytes_written").number(), saved.bytes_written());
        let retry = s.export_source_pack(revision, &destination, DeskSourcePackLimits::default(), || false);
        assert_eq!(retry.error(), Some(CheckpointIoError::DestinationExists));
        assert_eq!(fs::metadata(&destination).unwrap().len(), saved.bytes_written());
    }
}

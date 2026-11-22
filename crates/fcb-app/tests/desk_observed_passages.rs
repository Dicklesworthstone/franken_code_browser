#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real open-file observations, exact transfer and offline checkpoint recovery.
//! Fixtures remain available for diagnosis; execution belongs to strict RCH.
mod support;

use std::{ffi::OsString, fs::{self, OpenOptions}, os::unix::{ffi::OsStringExt, fs::FileExt},
    path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange};
use fcb_app::host::{HostResponse, desk::{DeskSession, DeskLimits, export::DeskSourcePackLimits},
    observed_reader::{ObservedReaderSession, ObservedReaderEncoding, MAX_OBSERVED_READER_OBSERVATIONS}};
use support::{Json, parse};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!("fcb-desk-observed-{}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn owner() -> ArenaOwnerId {
    static NEXT: AtomicU64 = AtomicU64::new(82000);
    ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap()
}
fn desk() -> DeskSession { DeskSession::new(owner(), DeskLimits::default()).unwrap() }
fn reader(path: &Path) -> ObservedReaderSession { ObservedReaderSession::open_file(owner(), path, || false).unwrap() }
fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn state(desk: &mut DeskSession) -> Json { json(&desk.state(|| false).unwrap()) }
fn read(desk: &mut DeskSession, passage: u64) -> Json {
    json(&desk.observed_passage(desk.model().revision(), passage, || false).unwrap())
}
fn keep(desk: &mut DeskSession, reader: &mut ObservedReaderSession, observation: u64,
    selected: ByteRange) -> u64 {
    desk.import_observation(desk.model().revision(), desk.model().last_attempt() + 1,
        reader, observation, selected, || false).unwrap().passage
}
fn observe(reader: &mut ObservedReaderSession, offset: u64, bytes: usize,
    encoding: ObservedReaderEncoding) -> Json {
    json(&reader.read_window(offset, bytes, encoding, || false).unwrap())
}

#[test]
fn passage_above_four_gib_keeps_raw_path_provenance_and_recovers_without_live_access() {
    let root = fixture();
    let path = root.join(OsString::from_vec(b"source-\xff".to_vec()));
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
    let offset = (1u64 << 32) + 71;
    file.set_len(offset + 128).unwrap();
    assert_eq!(file.write_at(b"needle", offset).unwrap(), 6);
    let mut source = reader(&path);
    let page = observe(&mut source, offset, 6, ObservedReaderEncoding::Utf8);
    let before_io = json(&source.info(|| false).unwrap()).get("payload_bytes_read").number();
    let mut kept = desk();
    let passage = keep(&mut kept, &mut source, page.get("observation").number(), range(offset, offset + 6));
    assert_eq!(json(&source.info(|| false).unwrap()).get("payload_bytes_read").number(), before_io);
    kept.edit_observed_passage(kept.model().revision(), kept.model().last_attempt() + 1,
        passage, 1, "Far-file evidence", "Remember\nthis exact\0passage", || false).unwrap();
    let accepted = state(&mut kept);
    assert!(accepted.get("panes").array().is_empty());
    assert!(accepted.get("bookmarks").array().is_empty());
    assert_eq!(accepted.get("retained_sources").number(), 1);
    assert_eq!(accepted.get("retained_source_bytes").number(), 22);
    let original = &accepted.get("observations").array()[0];
    assert_eq!(original.get("captured_range").get("start").number(), offset - 8);
    assert_eq!(original.get("extent_sha256"), page.get("extent_sha256"));
    assert!(original.get("path_hex").text().ends_with("736f757263652dff"));
    assert!(!original.get("whole_file_immutable").flag());

    let checkpoint = root.join("saved.fcbk");
    let saved = kept.save_checkpoint(kept.model().revision(), &checkpoint, 22, || false);
    assert_eq!(saved.error(), None);
    drop(source); drop(file);
    fs::rename(&path, root.join("old-source")).unwrap();
    fs::create_dir(&path).unwrap(); // Any live regular-file reopen must now fail.
    let current = read(&mut kept, passage);
    assert_eq!(current.get("text").text(), "needle");
    assert_eq!(current.get("original_hex").text(), "6e6565646c65");
    assert!(!current.get("source_reopened").flag());
    drop(kept);

    let mut restored = desk();
    restored.restore_checkpoint_file(0, 1, &checkpoint, || false).unwrap();
    let recovered = state(&mut restored);
    let note = &recovered.get("observed_passages").array()[0];
    assert_eq!(note.get("selection").get("start").number(), offset);
    assert_eq!(note.get("body").text(), "Remember\nthis exact\0passage");
    assert_eq!(note.get("update_sequence").number(), 2);
    let restored_observation = &recovered.get("observations").array()[0];
    for key in ["origin_owner", "origin_file", "origin_observation", "path_hex", "encoding",
        "captured_range", "observed_length", "final_observed_length", "consistency", "extent_sha256"] {
        assert_eq!(restored_observation.get(key), original.get(key), "{key}");
    }
    let replay = read(&mut restored, note.get("passage").number());
    assert_eq!(replay.get("text").text(), "needle");
    assert_eq!(replay.get("original_hex").text(), "6e6565646c65");
    assert_eq!(replay.get("additional_source_bytes_read").number(), 0);
    assert_eq!(recovered.get("initial_source_bytes_read").number(), 0);

    // The separate bookmark export must explicitly disclose its narrower scope.
    let pack = root.join("bookmark-pack.json");
    let export = restored.export_source_pack(restored.model().revision(), &pack,
        DeskSourcePackLimits::default(), || false);
    assert_eq!(export.error(), None);
    let manifest = parse(&fs::read(&pack).unwrap()).unwrap();
    assert!(!manifest.get("observed_passages_included").flag());
    assert_eq!(manifest.get("excluded_observed_passages").number(), 1);
    let receipt = json(&restored.source_pack_response(&export).unwrap());
    assert_eq!(receipt.get("excluded_observed_passages").number(), 1);
    assert_eq!(receipt.get("completeness_scope").text(), "all-bookmarked-selections");
}

#[test]
fn utf16_selections_share_one_observation_and_personal_edits_preserve_the_original_anchor() {
    let root = fixture(); let path = root.join("utf16");
    let bytes: Vec<u8> = "\u{feff}A🦀B\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(&path, &bytes).unwrap();
    let mut source = reader(&path); observe(&mut source, 0, 64, ObservedReaderEncoding::Utf16Le);
    let mut kept = desk();
    let crab = keep(&mut kept, &mut source, 1, range(4, 8));
    let first = keep(&mut kept, &mut source, 1, range(2, 4));
    assert_ne!(crab, first);
    let before = state(&mut kept);
    assert_eq!(before.get("observations").array().len(), 1);
    assert_eq!(before.get("retained_source_bytes").number(), bytes.len() as u64);
    for selected in [range(4, 6), range(5, 8), range(0, 2), range(0, 0), range(8, 100)] {
        assert!(kept.import_observation(kept.model().revision(), kept.model().last_attempt() + 1,
            &mut source, 1, selected, || false).is_err());
    }
    assert_eq!(state(&mut kept), before);
    fs::write(&path, b"live replacement").unwrap();
    let copied = read(&mut kept, crab);
    assert_eq!(copied.get("text").text(), "🦀");
    assert_eq!(copied.get("original_hex").text(), "3ed880dd");
    assert!(!copied.get("contains_replacements").flag());
    kept.edit_observed_passage(kept.model().revision(), 10, crab, 1,
        "Reason", "Line one\nline two\0", || false).unwrap();
    assert!(kept.edit_observed_passage(kept.model().revision(), 11, crab, 1,
        "Stale", "Cannot replace", || false).is_err());
    assert_eq!(read(&mut kept, crab).get("passage").get("selection"),
        before.get("observed_passages").array()[0].get("selection"));
    kept.forget_observed_passage(kept.model().revision(), 12, first, 1, || false).unwrap();
    assert_eq!(state(&mut kept).get("retained_sources").number(), 1);
    kept.forget_observed_passage(kept.model().revision(), 13, crab, 2, || false).unwrap();
    let empty = state(&mut kept);
    assert_eq!(empty.get("retained_source_bytes").number(), 0);
    assert!(empty.get("observations").array().is_empty());
}

#[test]
fn malformed_bytes_stay_exact_and_checkpointed_replacement_text_is_disclosed() {
    let root = fixture(); let path = root.join("malformed"); fs::write(&path, b"A\xffB").unwrap();
    let mut source = reader(&path); observe(&mut source, 0, 64, ObservedReaderEncoding::Utf8);
    let mut kept = desk(); let passage = keep(&mut kept, &mut source, 1, range(1, 2));
    let selected = read(&mut kept, passage);
    assert_eq!(selected.get("text").text(), "�");
    assert_eq!(selected.get("original_hex").text(), "ff");
    assert!(selected.get("contains_replacements").flag());
    let checkpoint = root.join("malformed.fcbk");
    assert_eq!(kept.save_checkpoint(1, &checkpoint, 3, || false).error(), None);
    let mut restored = desk(); restored.restore_checkpoint_file(0, 1, &checkpoint, || false).unwrap();
    let id = state(&mut restored).get("observed_passages").array()[0].get("passage").number();
    let selected = read(&mut restored, id);
    assert_eq!(selected.get("original_hex").text(), "ff");
    assert!(selected.get("contains_replacements").flag());
}

#[test]
fn canceled_unavailable_and_unadmitted_observations_cannot_change_existing_desk_work() {
    let root = fixture(); let path = root.join("source"); fs::write(&path, b"retained evidence").unwrap();
    let mut source = reader(&path); observe(&mut source, 0, 64, ObservedReaderEncoding::Utf8);
    let mut kept = desk(); let passage = keep(&mut kept, &mut source, 1, range(0, 8));
    let before = state(&mut kept);
    assert!(kept.import_observation(1, 2, &mut source, 1, range(9, 17), || true).is_err());
    for _ in 0..MAX_OBSERVED_READER_OBSERVATIONS {
        observe(&mut source, 0, 64, ObservedReaderEncoding::Utf8);
    }
    assert!(kept.import_observation(1, 3, &mut source, 1, range(9, 17), || false).is_err());
    assert_eq!(state(&mut kept), before);
    assert_eq!(read(&mut kept, passage).get("text").text(), "retained");
    let mut limits = DeskLimits::default(); limits.source_bytes = 8; limits.retained_bytes = 8;
    let mut denied = DeskSession::new(owner(), limits).unwrap();
    assert!(denied.import_observation(0, 1, &mut source,
        1 + MAX_OBSERVED_READER_OBSERVATIONS as u64, range(0, 8), || false).is_err());
    assert_eq!(denied.model().revision(), 0);
    assert!(state(&mut denied).get("observed_passages").array().is_empty());
}

#[test]
fn headless_recovery_can_read_edit_and_forget_observed_notes_without_live_sources() {
    let root = fixture(); let path = root.join("headless-source"); fs::write(&path, b"kept evidence").unwrap();
    let mut source = reader(&path); observe(&mut source, 0, 64, ObservedReaderEncoding::Utf8);
    let mut kept = desk(); keep(&mut kept, &mut source, 1, range(0, 4));
    let checkpoint = root.join("headless.fcbk");
    assert_eq!(kept.save_checkpoint(1, &checkpoint, 64, || false).error(), None);
    drop(source); drop(kept); fs::rename(&path, root.join("moved-source")).unwrap();
    let script = format!("0\trestore\t{}\n1\tobserved-passage\t1\n1\tedit-observed-passage-hex\t1\t1\t526561736f6e\t4e6f74650a00\n3\tobserved-passage\t1\n3\tforget-observed-passage\t1\t2\n5\tstate\n5\tquit\n", checkpoint.display());
    let mut input = script.as_bytes(); let mut output = Vec::new(); let mut errors = Vec::new();
    let exit = fcb_app::run(&["desk".into(), "--stdio".into()], &mut input, &mut output, &mut errors, || false);
    assert_eq!(exit, fcb_app::EXIT_OK, "{}", String::from_utf8_lossy(&errors));
    let records: Vec<_> = output.split(|byte| *byte == b'\n').filter(|line| !line.is_empty())
        .map(|line| parse(line).unwrap()).collect();
    assert_eq!(records.len(), 7);
    assert_eq!(records[1].get("result").get("text").text(), "kept");
    assert_eq!(records[3].get("result").get("passage").get("body").text(), "Note\n\0");
    assert_eq!(records[3].get("result").get("original_hex").text(), "6b657074");
    assert!(records[5].get("result").get("observed_passages").array().is_empty());
    assert_eq!(records[5].get("result").get("retained_source_bytes").number(), 0);
}

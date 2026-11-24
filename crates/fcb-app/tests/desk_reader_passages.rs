#![forbid(unsafe_code)]
#![cfg(unix)]

//! Complete retained source -> bounded passage -> notes/checkpoint/export.
//! Code-first regression sources; execution belongs to strict RCH.
mod support;

use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange};
use fcb::search::ExtentConsistency;
use fcb_app::host::{HostResponse, desk::{DeskLimits, DeskSession, export::DeskSourcePackLimits},
    reader::{ReaderSession, ReaderSessionError, MAX_READER_SOURCE_BYTES}};
use support::{Json, parse};

fn owner() -> ArenaOwnerId {
    static NEXT: AtomicU64 = AtomicU64::new(289_000);
    ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap()
}
fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("fcb-desk-reader-passage-{}-{}",
        std::process::id(), owner().get()));
    fs::create_dir(&root).unwrap(); root
}
fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
}
fn json(reply: &HostResponse) -> Json { parse(reply.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn large_capture_keeps_only_exact_passage_and_recovers_notes_without_source() {
    let root = fixture(); let path = root.join(OsString::from_vec(b"source-\xff.rs".to_vec()));
    let mut bytes = vec![b'x'; 5 * 1024 * 1024];
    let start = bytes.len() as u64;
    let selected = "🦀 evidence\r\n";
    bytes.extend_from_slice(selected.as_bytes()); bytes.extend_from_slice(b"tail context");
    let end = start + selected.len() as u64;
    fs::write(&path, &bytes).unwrap();
    let source_owner = owner();
    let mut reader = ReaderSession::open(source_owner, &path, MAX_READER_SOURCE_BYTES, || false).unwrap();
    let before = json(&reader.info(|| false).unwrap());
    fs::write(&path, b"changed live file").unwrap();
    let mut desk = DeskSession::new(owner(), DeskLimits::default()).unwrap();
    let excerpt = reader.retained_passage(range(start, end), || false).unwrap();
    assert_eq!(excerpt.extent().consistency(), ExtentConsistency::HostSupplied);
    assert_eq!(excerpt.extent().range(), range(start - 8, end + 8));
    assert_eq!(excerpt.extent().bytes(), &bytes[start as usize - 8..end as usize + 8]);
    assert_eq!(excerpt.extent().observed_length().get(), bytes.len() as u64);
    assert_eq!(excerpt.extent().request().file().owner(), source_owner);
    let kept = desk.import_reader_passage(0, 1, &excerpt, || false).unwrap();
    drop(excerpt);
    assert_eq!(json(&reader.info(|| false).unwrap()), before);
    drop(reader);
    assert!(json(&desk.state(|| false).unwrap()).get("panes").array().is_empty());
    assert_eq!(desk.model().retained_source_bytes(), selected.len() as u64 + 16);
    desk.edit_observed_passage(1, 2, kept.passage, 1, "Exact reason", "Keep this\0evidence", || false).unwrap();
    let state = json(&desk.state(|| false).unwrap());
    let evidence = &state.get("observations").array()[0];
    assert_eq!(evidence.get("origin_owner").number(), source_owner.get());
    assert_eq!(evidence.get("origin_file").number(), 1);
    assert_eq!(evidence.get("origin_observation").number(), 1);
    assert!(evidence.get("path_hex").text().ends_with("736f757263652dff2e7273"));
    assert_eq!(evidence.get("consistency").text(), "host-supplied-sequence");
    assert!(!evidence.get("whole_file_immutable").flag());
    assert!(!evidence.get("source_reopened").flag());

    let checkpoint = root.join("saved.fcbk");
    assert_eq!(desk.save_checkpoint(2, &checkpoint, selected.len() as u64 + 16, || false).error(), None);
    drop(desk);
    let mut restored = DeskSession::new(owner(), DeskLimits::default()).unwrap();
    restored.restore_checkpoint_file(0, 1, &checkpoint, || false).unwrap();
    let state = json(&restored.state(|| false).unwrap());
    let passage = &state.get("observed_passages").array()[0];
    assert_eq!(passage.get("selection").get("start").number(), start);
    assert_eq!(passage.get("body").text(), "Keep this\0evidence");
    let recovered = json(&restored.observed_passage(restored.model().revision(),
        passage.get("passage").number(), || false).unwrap());
    assert_eq!(recovered.get("observation"), evidence);
    assert_eq!(recovered.get("text").text(), selected);
    assert_eq!(recovered.get("original_hex").text(), hex(selected.as_bytes()));
    assert_eq!(recovered.get("additional_source_bytes_read").number(), 0);

    let destination = root.join("passage-pack.json");
    let exported = restored.export_source_pack(restored.model().revision(), &destination,
        DeskSourcePackLimits::default(), || false);
    assert_eq!(exported.error(), None);
    let pack = parse(&fs::read(&destination).unwrap()).unwrap();
    let entry = &pack.get("entries").array()[0];
    assert_eq!(entry.get("payload_scope").text(), "selected-range-only");
    assert_eq!(entry.get("selected_original_hex").text(), hex(selected.as_bytes()));
    assert_eq!(entry.get("selected_text").text(), selected);
    assert_eq!(entry.get("body").text(), "Keep this\0evidence");
    assert_eq!(entry.get("observation"), evidence);
}

#[test]
fn passage_admission_rejects_wide_empty_canceled_and_split_utf16_selections() {
    let bytes: Vec<u8> = "\u{feff}A🦀B\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut reader = ReaderSession::from_bytes(owner(), std::path::Path::new("unopened-source"),
        &bytes, || false).unwrap();
    for selected in [range(0, 0), range(0, 256 * 1024 + 1), range(0, bytes.len() as u64 + 1)] {
        assert!(matches!(reader.retained_passage(selected, || false), Err(ReaderSessionError::InvalidRange)));
    }
    assert!(matches!(reader.retained_passage(range(4, 8), || true), Err(ReaderSessionError::Canceled)));
    for selected in [range(4, 6), range(5, 8), range(0, 2), range(10, 12)] {
        // Rejected decoding consumes an attempt even without publishing a
        // revision. Each case must reach boundary validation independently.
        let mut desk = DeskSession::new(owner(), DeskLimits::default()).unwrap();
        let excerpt = reader.retained_passage(selected, || false).unwrap();
        assert!(desk.import_reader_passage(0, 1, &excerpt, || false).is_err());
        assert_eq!(desk.model().revision(), 0);
        assert!(desk.model().observed_passages().is_empty());
    }
    let mut desk = DeskSession::new(owner(), DeskLimits::default()).unwrap();
    let excerpt = reader.retained_passage(range(4, 8), || false).unwrap();
    assert!(desk.import_reader_passage(0, 1, &excerpt, || true).is_err_and(|e| e.is_canceled()));
    let first = desk.import_reader_passage(0, 2, &excerpt, || false).unwrap();
    let second = desk.import_reader_passage(2, 3, &excerpt, || false).unwrap();
    assert_eq!(first.observation, second.observation);
    assert_ne!(first.passage, second.passage);
    assert_eq!(desk.model().retained_source_bytes(), bytes.len() as u64);
    let result = json(&desk.observed_passage(3, first.passage, || false).unwrap());
    assert_eq!(result.get("text").text(), "🦀");
    assert_eq!(result.get("original_hex").text(), "3ed880dd");
}

#[test]
fn exact_selection_limit_includes_decoder_context_and_malformed_bytes_stay_original() {
    let limit = fcb::search::extents::MAX_EXTENT_TEXT_BYTES;
    let bytes = vec![b'a'; limit + 16];
    let mut reader = ReaderSession::from_bytes(owner(), std::path::Path::new("unopened-large-source"),
        &bytes, || false).unwrap();
    assert!(matches!(reader.retained_passage(range(8, 9 + limit as u64), || false),
        Err(ReaderSessionError::InvalidRange)));
    let excerpt = reader.retained_passage(range(8, 8 + limit as u64), || false).unwrap();
    assert_eq!(excerpt.extent().bytes().len(), limit + 16);
    let mut desk = DeskSession::new(owner(), DeskLimits::default()).unwrap();
    desk.import_reader_passage(0, 1, &excerpt, || false).unwrap();
    assert_eq!(desk.model().retained_source_bytes(), bytes.len() as u64);

    let mut malformed = ReaderSession::from_bytes(owner(), std::path::Path::new("unopened-malformed"),
        b"A\xffB", || false).unwrap();
    let excerpt = malformed.retained_passage(range(1, 2), || false).unwrap();
    let kept = desk.import_reader_passage(1, 2, &excerpt, || false).unwrap();
    let result = json(&desk.observed_passage(2, kept.passage, || false).unwrap());
    assert_eq!(result.get("original_hex").text(), "ff");
    assert_eq!(result.get("text").text(), "\u{fffd}");
    assert!(result.get("contains_replacements").flag());
}

#![forbid(unsafe_code)]

//! Exact Desk -> full reader production path. Source tests are authored during
//! the code-first wave; the independent verifier owns strict-RCH execution.
mod support;
use support::{Json, parse};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange, FileId, SourceCapture, SourceRevision};
use fcb::search::{ResourceAllocationId, SymbolNameMode};
use fcb::store::Sha256;
use fcb_app::host::{HostResponse, desk::{DeskCommand, DeskError, DeskLimits, DeskPaneId,
    DeskSession, DeskSessionError, reader::DeskReaderError},
    reader::{ReaderDocumentOptions, ReaderOutlineOptions, ReaderSession}};

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn desk(n: u64) -> DeskSession { DeskSession::new(owner(n), DeskLimits::default()).unwrap() }
fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
}
fn source(desk: &DeskSession, id: u64, label: &str, bytes: &[u8]) -> SourceCapture {
    let owner = desk.model().owner();
    SourceCapture::from_bytes(owner, FileId::new(owner, id).unwrap(),
        SourceRevision::new(owner, id).unwrap(), label, bytes.to_vec()).unwrap()
}
fn adopt(desk: &mut DeskSession, id: u64, label: &str, bytes: &[u8]) -> DeskPaneId {
    let capture = source(desk, id, label, bytes);
    desk.adopt(desk.model().revision(), desk.model().last_attempt() + 1,
        capture, 0, None, || false).unwrap().active.unwrap()
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn copy_all(reader: &mut ReaderSession) -> Json {
    json(&reader.copy_range(0, reader.capture().bytes().len() as u64, || false).unwrap())
}

#[test]
fn exact_handoff_binds_old_location_to_fresh_reader_without_changing_personal_state() {
    let raw = b"first\r\noriginal\0\xffcapture\n";
    let label = "not-a-live-file/retained source.rs";
    let mut desk = desk(9810);
    let pane = adopt(&mut desk, 7, label, raw);
    desk.apply(1, 2, DeskCommand::Navigate { pane, offset: 7,
        selection: Some(range(7, 15)) }, || false).unwrap();
    desk.apply(2, 3, DeskCommand::Bookmark { pane, label: "Why these bytes matter".into() }, || false).unwrap();
    desk.search(3, pane, 50, "original", 8, 4096, || false).unwrap();
    let before = json(&desk.state(|| false).unwrap());
    let (mut reader, response) = desk.open_reader(3, pane, owner(9811), || false).unwrap();
    let receipt = json(&response);
    assert_eq!(receipt.get("schema").text(), "fcb.reading-desk/1");
    assert_eq!(receipt.get("command").text(), "open-reader");
    assert_eq!(receipt.get("owner").number(), 9810);
    assert_eq!(receipt.get("model_revision").number(), 3);
    assert_eq!(receipt.get("last_attempt").number(), 3);
    assert_eq!(receipt.get("pane").number(), pane.get());
    assert_eq!(receipt.get("file_id").number(), 7);
    assert_eq!(receipt.get("source_revision").number(), 7);
    assert_eq!(receipt.get("offset").number(), 7);
    assert_eq!(receipt.get("selection").get("start").number(), 7);
    assert_eq!(receipt.get("selection").get("end").number(), 15);
    assert_eq!(receipt.get("label").text(), label);
    assert_eq!(receipt.get("captured_bytes").number(), raw.len() as u64);
    assert!(!receipt.get("source_reopened").flag());
    assert!(!receipt.get("native_presented").flag());
    assert_eq!(receipt.get("additional_source_bytes_read").number(), 0);
    let info = receipt.get("reader");
    assert_eq!(info.get("schema").text(), "fcb.reader-session/1");
    assert_eq!(info.get("command").text(), "info");
    assert_eq!(info.get("owner").number(), 9811);
    assert_eq!(info.get("capture_origin").text(), "host-supplied");
    assert_eq!(info.get("initial_source_bytes_read").number(), 0);
    assert_eq!(info.get("initial_read_calls").number(), 0);
    assert_eq!(info.get("capture_sha256").text(), Sha256::digest(raw).to_hex());
    assert_eq!(info.get("path").get("hex").text(), hex(label.as_bytes()));
    for key in ["accepted_query_generation", "accepted_outline_generation", "accepted_document_generation"] {
        assert_eq!(info.get(key), &Json::Null);
    }
    assert_eq!(json(&desk.state(|| false).unwrap()), before);
    assert_eq!(reader.capture().bytes(), raw);
    assert_ne!(reader.capture().request().file(), desk.model().source(pane, 3).unwrap().file());
    drop(desk);
    assert_eq!(copy_all(&mut reader).get("original_hex").text(), hex(raw));
}

#[test]
fn restored_capture_opens_after_its_original_desk_is_gone_and_survives_another_restore() {
    let mut original = desk(9820);
    let pane = adopt(&mut original, 9, "saved-only/README.md", b"# Retained title\n\nSaved **evidence**.\n");
    original.apply(1, 2, DeskCommand::Navigate { pane, offset: 18,
        selection: Some(range(18, 23)) }, || false).unwrap();
    let checkpoint = original.model().checkpoint(2, ResourceAllocationId::new(100_000).unwrap(), || false).unwrap();
    drop(original);
    let mut restored = desk(9821);
    restored.restore_checkpoint_bytes(0, 1, checkpoint.bytes(), || false).unwrap();
    let restored_pane = restored.model().active().unwrap();
    let restored_file = restored.model().location(restored_pane, 1).unwrap().file;
    let (mut reader, receipt) = restored.open_reader(1, restored_pane, owner(9822), || false).unwrap();
    assert_eq!(json(&receipt).get("file_id").number(), restored_file.get());
    assert_eq!(json(&receipt).get("offset").number(), 18);
    assert_eq!(reader.capture().bytes(), b"# Retained title\n\nSaved **evidence**.\n");
    restored.restore_checkpoint_bytes(1, 2, checkpoint.bytes(), || false).unwrap();
    assert_ne!(restored.model().location(restored.model().active().unwrap(), 2).unwrap().file, restored_file);
    assert!(restored.open_reader(1, restored_pane, owner(9823), || false).is_err());
    drop(restored);
    let preview = json(&reader.prepare_document(1, ReaderDocumentOptions::default(), || false).unwrap());
    assert_eq!(preview.get("headings").array()[0].get("slug").text(), "retained-title");
    assert_eq!(preview.get("rendering").text(), "logical-frankenmarkdown-flow");
    assert_eq!(preview.get("additional_source_bytes_read").number(), 0);
    assert!(reader.read_window(18, 64, || false).unwrap().as_str().contains("Saved **evidence**"));
}

#[test]
fn imported_reader_uses_existing_find_outline_and_fork_services_with_independent_generations() {
    let raw = b"pub fn retained_function() {\n    let retained = 1;\n}\n";
    let mut desk = desk(9830);
    let pane = adopt(&mut desk, 1, "retained.rs", raw);
    desk.search(1, pane, 900, "retained", 8, 4096, || false).unwrap();
    let (mut reader, _) = desk.open_reader(1, pane, owner(9831), || false).unwrap();
    reader.search(1, "retained", 8, 4096, || false).unwrap();
    assert_eq!(reader.accepted_generation(), Some(1));
    assert_eq!(desk.accepted_query(), Some(900));
    assert_eq!(json(&reader.copy_hit(1, 0, || false).unwrap()).get("original_hex").text(), hex(b"retained"));
    reader.prepare_outline(1, ReaderOutlineOptions::default(), || false).unwrap();
    let outline = json(&reader.symbol_page(1, "retained_function", SymbolNameMode::Exact, 0, 8, || false).unwrap());
    assert!(outline.get("symbols").array().iter().any(|s| s.get("name").text() == "retained_function"));
    let mut pin = reader.fork(owner(9832), || false).unwrap();
    assert_eq!(pin.accepted_generation(), None);
    assert_eq!(pin.outline_generation(), None);
    drop(reader);
    drop(desk);
    assert_eq!(copy_all(&mut pin).get("original_hex").text(), hex(raw));
}

#[test]
fn rejected_owners_stale_panes_and_cancellation_preserve_desk_navigation_and_query() {
    let mut source = desk(9840);
    let pane = adopt(&mut source, 1, "same-label.rs", b"old capture");
    source.search(1, pane, 2, "old", 8, 4096, || false).unwrap();
    let before = json(&source.state(|| false).unwrap());
    assert_eq!(source.open_reader(1, pane, owner(9840), || false).err(),
        Some(DeskReaderError::Desk(DeskSessionError::Desk(DeskError::OwnerMismatch))));
    assert_eq!(source.open_reader(0, pane, owner(9841), || false).err(),
        Some(DeskReaderError::Desk(DeskSessionError::Desk(DeskError::StaleRevision))));
    let mut foreign = desk(9842);
    let foreign_pane = adopt(&mut foreign, 1, "same-label.rs", b"different bytes");
    assert_eq!(pane.get(), foreign_pane.get(), "negative control must collide in the local ordinal");
    assert_eq!(source.open_reader(1, foreign_pane, owner(9843), || false).err(),
        Some(DeskReaderError::Desk(DeskSessionError::Desk(DeskError::OwnerMismatch))));
    assert!(source.open_reader(1, pane, owner(9844), || true).err().unwrap().is_canceled());
    assert_eq!(json(&source.state(|| false).unwrap()), before);
    source.apply(1, 2, DeskCommand::Close(pane), || false).unwrap();
    assert!(source.open_reader(2, pane, owner(9845), || false).is_err());
}

#[test]
fn cancellation_during_capture_preparation_releases_candidate_and_allows_a_fresh_handoff() {
    let raw = vec![b'x'; 512 * 1024];
    let mut desk = desk(9850);
    let pane = adopt(&mut desk, 1, "admitted.bin", &raw);
    let mut checks = 0;
    let error = desk.open_reader(1, pane, owner(9851), || {
        checks += 1;
        checks > 4
    }).err().unwrap();
    assert!(error.is_canceled());
    assert!(checks > 4, "negative control: cancellation followed admission and initial work");
    assert_eq!(desk.model().revision(), 1);
    assert_eq!(desk.model().source(pane, 1).unwrap().bytes(), raw);
    let (reader, _) = desk.open_reader(1, pane, owner(9852), || false).unwrap();
    assert_eq!(reader.capture().bytes(), raw);
}

#[test]
fn empty_and_original_utf16_sources_are_transferred_without_decoded_text_roundtrips() {
    let utf16 = [0xff, 0xfe, b'a', 0, b'\r', 0, b'\n', 0, b'g', 0, b'o', 0, b'a', 0, b'l', 0];
    for (index, raw) in [&[][..], &utf16[..]].into_iter().enumerate() {
        let base = 9860 + index as u64 * 2;
        let mut desk = desk(base);
        let pane = adopt(&mut desk, 1, "offline.txt", raw);
        let (mut reader, receipt) = desk.open_reader(1, pane, owner(base + 1), || false).unwrap();
        assert_eq!(reader.capture().bytes(), raw);
        assert_eq!(json(&receipt).get("reader").get("capture_sha256").text(), Sha256::digest(raw).to_hex());
        if !raw.is_empty() {
            reader.search(1, "goal", 8, 4096, || false).unwrap();
            assert_eq!(reader.hit_range(1, 0).unwrap(), range(8, 16));
            assert_eq!(json(&reader.copy_hit(1, 0, || false).unwrap()).get("original_hex").text(), "67006f0061006c00");
        } else {
            let page = json(&reader.read_window(0, 4, || false).unwrap());
            assert_eq!(page.get("text").text(), "");
            assert_eq!(page.get("captured_bytes").number(), 0);
        }
    }
}

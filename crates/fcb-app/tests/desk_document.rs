#![forbid(unsafe_code)]

mod support;
use support::{Json, parse};
use fcb::{ArenaOwnerId, FileId, SourceRevision, SourceCapture, ByteOffset, ByteRange};
use fcb::document::reader::DocumentReadError;
use fcb_app::host::{HostResponse, desk::{DeskSession, DeskLimits, DeskPaneId, DeskCommand,
    DeskError, DeskSessionError, document::{DeskDocument, DeskDocumentOptions, DeskDocumentError, DocumentCopyMode}}};
use fcb_app::EXIT_OK;

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn desk() -> DeskSession { DeskSession::new(owner(4300), DeskLimits::default()).unwrap() }
fn source(n: u64, bytes: &[u8]) -> SourceCapture {
    SourceCapture::from_bytes(owner(4300), FileId::new(owner(4300), n).unwrap(),
        SourceRevision::new(owner(4300), n).unwrap(), format!("document-{n}.md"), bytes.to_vec()).unwrap()
}
fn adopt(s: &mut DeskSession, n: u64, bytes: &[u8]) -> DeskPaneId {
    s.adopt(s.model().revision(), s.model().last_attempt() + 1, source(n, bytes), 0, None, || false).unwrap().active.unwrap()
}
fn apply(s: &mut DeskSession, command: DeskCommand) {
    s.apply(s.model().revision(), s.model().last_attempt() + 1, command, || false).unwrap();
}
fn prepare(s: &mut DeskSession, pane: DeskPaneId, generation: u64) -> DeskDocument {
    DeskDocument::prepare(s, s.model().revision(), pane, generation, DeskDocumentOptions::default(), || false).unwrap()
}
fn json(response: &HostResponse) -> Json {
    assert_eq!(response.exit_code(), EXIT_OK);
    let value = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(value.get("schema").text(), "fcb.reading-desk/1");
    assert!(!value.get("native_presented").flag()); value
}
fn span(document: &DeskDocument, s: &DeskSession, needle: &str) -> (usize, usize) {
    let text = document.layout(s, s.model().revision(), document.generation()).unwrap().rendered_text();
    let at = text.find(needle).unwrap(); (at, at + needle.len())
}

#[test]
fn preparation_shares_exact_backing_and_pages_without_changing_the_desk() {
    let mut s = desk(); let pane = adopt(&mut s, 1, b"# Title\n\nHello **world**.\n");
    let doc = prepare(&mut s, pane, 1);
    assert!(std::ptr::eq(doc.layout(&s, 1, 1).unwrap().capture().bytes(), s.model().source(pane, 1).unwrap().bytes()));
    let page = json(&doc.overview(&mut s, 1, 1, || false).unwrap());
    assert_eq!(page.get("rendering").text(), "logical-frankenmarkdown-flow");
    assert_eq!(page.get("source_mapping").text(), "enclosing-regions-not-glyph-exact");
    assert!(!page.get("native_shaped").flag());
    assert_eq!(page.get("headings").array()[0].get("slug").text(), "title");
    assert!(!page.get("flow_lines").array().is_empty());
    assert_eq!(s.model().revision(), 1); assert_eq!(s.model().history().len(), 1);
    assert_eq!(page.get("additional_source_bytes_read").number(), 0);
}

#[test]
fn rendered_and_original_copy_domains_are_different_and_source_selection_is_real() {
    let mut s = desk(); let raw = b"Hello **world**.\r\n"; let pane = adopt(&mut s, 1, raw);
    let doc = prepare(&mut s, pane, 1); let (start, end) = span(&doc, &s, "world");
    let rendered = json(&doc.copy(&mut s, 1, 1, start, end, DocumentCopyMode::RenderedText, || false).unwrap());
    assert_eq!(rendered.get("text").text(), "world");
    assert_eq!(rendered.get("copy_domain").text(), "rendered-text-utf8");
    assert!(!rendered.get("clipboard_written").flag());
    let original = json(&doc.copy(&mut s, 1, 1, start, end, DocumentCopyMode::EnclosingMarkdown, || false).unwrap());
    assert_eq!(original.get("copy_domain").text(), "enclosing-original-markdown");
    assert!(original.get("original_hex").text().contains("2a2a776f726c642a2a"));
    doc.select_source(&mut s, 1, 2, 1, start, end, || false).unwrap();
    assert!(s.model().selected_bytes(pane, 2).unwrap().windows(9).any(|b| b == b"**world**"));
    assert_eq!(s.model().source(pane, 2).unwrap().bytes(), raw);
    assert_eq!(s.model().history().len(), 2);
}

#[test]
fn heading_navigation_round_trips_through_history_and_bookmarks() {
    let mut s = desk(); let pane = adopt(&mut s, 1, b"# One\n\nfirst\n\n## Two\n\nsecond\n");
    let doc = prepare(&mut s, pane, 1);
    doc.seek_heading(&mut s, 1, 2, 1, "two", || false).unwrap();
    let selected = s.model().location(pane, 2).unwrap();
    assert!(selected.offset > 0); assert!(selected.selection.is_some());
    apply(&mut s, DeskCommand::Bookmark { pane, label: "documentation context".into() });
    apply(&mut s, DeskCommand::Back);
    assert_eq!(s.model().location(pane, 4).unwrap().offset, 0);
    apply(&mut s, DeskCommand::Forward);
    assert_eq!(s.model().location(pane, 5).unwrap().selection, selected.selection);
    assert_eq!(s.model().bookmarks()[0].location().selection, selected.selection);
    assert!(doc.validate_source(&s, 5).is_ok());
}

#[test]
fn source_search_selection_drives_a_span_linked_split_payload() {
    let mut s = desk(); let pane = adopt(&mut s, 1, b"# Title\n\nNeedle **evidence**.\n");
    let doc = prepare(&mut s, pane, 1);
    s.search(1, pane, 1, "evidence", 10, 1000, || false).unwrap();
    s.activate_hit(1, 2, pane, 1, 0, || false).unwrap();
    let offset = s.model().location(pane, 2).unwrap().offset;
    let split = json(&doc.split(&mut s, 2, 1, offset, 16, || false).unwrap());
    assert_eq!(split.get("synchronization").text(), "enclosing-source-region");
    assert_eq!(split.get("original_anchor").number(), offset);
    assert_eq!(split.get("source").get("source_revision").number(), split.get("preview").get("source_revision").number());
    assert!(split.get("source").get("text").text().starts_with("evidence"));
    assert!(!split.get("preview").get("flow_lines").array().is_empty());
    assert_eq!(s.model().revision(), 2); assert_eq!(s.accepted_query(), Some(1));
}

#[test]
fn successful_reflow_rejects_old_coordinates_without_moving_source_selection() {
    let mut s = desk(); let pane = adopt(&mut s, 1, b"one two three four five six seven eight nine ten\n");
    let mut doc = prepare(&mut s, pane, 1);
    let (start, end) = span(&doc, &s, "three"); doc.select_source(&mut s, 1, 2, 1, start, end, || false).unwrap();
    let original = s.model().location(pane, 2).unwrap();
    let response = doc.reflow(&mut s, 2, 2, DeskDocumentOptions { width_columns: 8, ..Default::default() }, || false).unwrap();
    assert_eq!(json(&response).get("width_columns").number(), 8);
    assert_eq!(doc.generation(), 2); assert_eq!(s.model().location(pane, 2).unwrap(), original);
    assert_eq!(doc.select_source(&mut s, 2, 3, 1, start, end, || false), Err(DeskDocumentError::Document(DocumentReadError::StaleGeneration)));
    assert_eq!(s.model().revision(), 2);
    assert!(doc.at_source(&mut s, 2, 2, original.offset, 8, || false).is_ok());
}

#[test]
fn final_reflow_cancellation_preserves_old_layout_and_consumes_attempt() {
    let build = || {
        let mut s = desk(); let p = adopt(&mut s, 1, b"# Title\n\nSome body text.\n");
        let doc = prepare(&mut s, p, 1); (s, doc)
    };
    let (mut probe, mut pdoc) = build(); let mut calls = 0;
    pdoc.reflow(&mut probe, 1, 2, DeskDocumentOptions::default(), || { calls += 1; false }).unwrap();
    let (mut s, mut doc) = build(); let mut seen = 0;
    let result = doc.reflow(&mut s, 1, 2, DeskDocumentOptions::default(), || { seen += 1; seen == calls });
    assert!(matches!(result, Err(e) if e.is_canceled()));
    assert_eq!(doc.generation(), 1); assert_eq!(doc.last_attempt(), 2); assert_eq!(s.model().revision(), 1);
    assert!(doc.overview(&mut s, 1, 1, || false).is_ok());
    assert_eq!(doc.reflow(&mut s, 1, 2, DeskDocumentOptions::default(), || false).err(),
        Some(DeskDocumentError::Document(DocumentReadError::StaleGeneration)));
}

#[test]
fn replaced_source_cannot_receive_old_preview_navigation_but_exact_back_can_reuse_it() {
    let mut s = desk(); let pane = adopt(&mut s, 1, b"# Old\n"); let doc = prepare(&mut s, pane, 1);
    adopt(&mut s, 2, b"# New\n");
    assert_eq!(doc.seek_heading(&mut s, 2, 3, 1, "old", || false), Err(DeskDocumentError::Document(DocumentReadError::StaleCapture)));
    assert_eq!(s.model().source(pane, 2).unwrap().bytes(), b"# New\n");
    apply(&mut s, DeskCommand::Back);
    assert!(doc.overview(&mut s, 3, 1, || false).is_ok());
    apply(&mut s, DeskCommand::Close(pane));
    assert_eq!(doc.validate_source(&s, 4), Err(DeskDocumentError::Desk(DeskSessionError::Desk(DeskError::MissingPane))));
}

#[test]
fn duplicate_panes_have_independent_layout_widths_and_navigation() {
    let mut s = desk(); let first = adopt(&mut s, 1, b"# One\n\nfirst paragraph\n\n## Two\n\nsecond paragraph\n");
    let one = prepare(&mut s, first, 1);
    let second = s.apply(1, 2, DeskCommand::Duplicate(first), || false).unwrap().active.unwrap();
    let two = DeskDocument::prepare(&mut s, 2, second, 2,
        DeskDocumentOptions { width_columns: 8, ..Default::default() }, || false).unwrap();
    two.seek_heading(&mut s, 2, 3, 2, "two", || false).unwrap();
    assert_eq!(s.model().location(first, 3).unwrap().offset, 0);
    assert!(s.model().location(second, 3).unwrap().offset > 0);
    assert_eq!(one.layout(&s, 3, 1).unwrap().options().width_columns, 100);
    assert_eq!(two.layout(&s, 3, 2).unwrap().options().width_columns, 8);
    assert!(std::ptr::eq(one.layout(&s, 3, 1).unwrap().capture().bytes(), two.layout(&s, 3, 2).unwrap().capture().bytes()));
}

#[test]
fn utf8_bom_and_multibyte_rendered_selection_keep_original_coordinates() {
    let mut s = desk(); let pane = adopt(&mut s, 1, "\u{feff}# Title\r\n\r\nHello 😀.\r\n".as_bytes());
    let doc = prepare(&mut s, pane, 1); let (start, end) = span(&doc, &s, "😀");
    assert_eq!(doc.layout(&s, 1, 1).unwrap().source_base(), 3);
    assert!(doc.at_source(&mut s, 1, 1, 0, 8, || false).is_err());
    assert_eq!(doc.copy(&mut s, 1, 1, start + 1, end, DocumentCopyMode::RenderedText, || false).err(),
        Some(DeskDocumentError::Document(DocumentReadError::InvalidRange)));
    let copied = json(&doc.copy(&mut s, 1, 1, start, end, DocumentCopyMode::RenderedText, || false).unwrap());
    assert_eq!(copied.get("text").text(), "😀");
    doc.select_source(&mut s, 1, 2, 1, start, end, || false).unwrap();
    assert!(s.model().location(pane, 2).unwrap().selection.unwrap().start().get() >= 3);
    assert!(std::str::from_utf8(s.model().selected_bytes(pane, 2).unwrap()).unwrap().contains('😀'));
}

#[test]
fn unsupported_markdown_encoding_does_not_break_original_byte_reading() {
    for bytes in [b"\xff# invalid".as_slice(), b"\xff\xfe#\0 \0A\0\n\0"] {
        let mut s = desk(); let pane = adopt(&mut s, 1, bytes);
        assert_eq!(DeskDocument::prepare(&mut s, 1, pane, 1, DeskDocumentOptions::default(), || false).err(),
            Some(DeskDocumentError::Document(DocumentReadError::InvalidUtf8)));
        apply(&mut s, DeskCommand::Navigate { pane, offset: 0,
            selection: Some(ByteRange::new(ByteOffset::new(0), ByteOffset::new(bytes.len() as u64)).unwrap()) });
        assert_eq!(s.model().selected_bytes(pane, 2).unwrap(), bytes);
    }
}

#[test]
fn invalid_limits_and_stale_revisions_leave_the_existing_document_usable() {
    let mut s = desk(); let pane = adopt(&mut s, 1, b"# Title\n\nbody\n"); let mut doc = prepare(&mut s, pane, 1);
    assert_eq!(doc.reflow(&mut s, 1, 2, DeskDocumentOptions { max_source_bytes: 1, ..Default::default() }, || false).err(),
        Some(DeskDocumentError::Document(DocumentReadError::SourceLimit)));
    assert_eq!(doc.window(&mut s, 1, 1, 0, usize::MAX, || false).err(),
        Some(DeskDocumentError::Document(DocumentReadError::InvalidLimits)));
    assert_eq!(doc.window(&mut s, 0, 1, 0, 1, || false).err(),
        Some(DeskDocumentError::Desk(DeskSessionError::Desk(DeskError::StaleRevision))));
    assert!(doc.headings(&mut s, 1, 1, 99, 1, || false).is_err());
    assert_eq!(doc.seek_heading(&mut s, 1, 2, 1, "missing", || false).err(),
        Some(DeskDocumentError::Document(DocumentReadError::HeadingNotFound)));
    assert_eq!(doc.generation(), 1); assert_eq!(s.model().revision(), 1);
    assert!(doc.overview(&mut s, 1, 1, || false).is_ok());
}

#[test]
fn empty_document_and_eof_are_complete_empty_windows_not_missing_sources() {
    let mut s = desk(); let pane = adopt(&mut s, 1, b""); let doc = prepare(&mut s, pane, 1);
    let total = doc.layout(&s, 1, 1).unwrap().total_lines();
    let eof = json(&doc.window(&mut s, 1, 1, total, 4, || false).unwrap());
    assert!(eof.get("flow_lines").array().is_empty());
    assert!(doc.at_source(&mut s, 1, 1, 0, 4, || false).is_ok());
    assert_eq!(s.model().source(pane, 1).unwrap().bytes(), b"");
}

#[cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
mod native {
    use super::*;
    use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
    use fcb_app::host::{atlas_session::AtlasSessionOptions, atlas_search::AtlasSearchOptions, desk::repository::DeskRepository};
    fn fixture() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-desk-doc-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap(); root
    }
    #[test]
    fn repository_hit_preview_survives_live_replacement_and_repository_detachment() {
        let root = fixture(); let path = root.join("README.md"); fs::write(&path, b"# Title\n\nold needle **evidence**\n").unwrap();
        let mut repo = DeskRepository::open(owner(4301), &root, AtlasSessionOptions::default(), || false).unwrap();
        repo.search(1, "needle", AtlasSearchOptions::default(), || false).unwrap();
        let mut s = desk(); let pane = repo.open_hit(&mut s, 0, 1, 1, 1, || false).unwrap().change.active.unwrap();
        fs::rename(&path, root.join("moved.md")).unwrap(); fs::write(&path, b"new live source").unwrap(); drop(repo);
        let doc = prepare(&mut s, pane, 1); let (start, end) = span(&doc, &s, "evidence");
        assert_eq!(json(&doc.copy(&mut s, 1, 1, start, end, DocumentCopyMode::RenderedText, || false).unwrap()).get("text").text(), "evidence");
        assert_eq!(s.model().source(pane, 1).unwrap().bytes(), b"# Title\n\nold needle **evidence**\n");
    }
    #[test]
    fn restored_checkpoint_rebuilds_preview_without_reopening_source_or_reusing_old_binding() {
        let root = fixture(); let path = root.join("README.md"); let saved = root.join("reading.fcbk");
        fs::write(&path, b"# One\n\nfirst\n\n## Two\n\nsecond\n").unwrap(); let mut s = desk();
        let pane = s.open_file(0, 1, &path, || false).unwrap().active.unwrap(); let doc = prepare(&mut s, pane, 1);
        doc.seek_heading(&mut s, 1, 2, 1, "two", || false).unwrap();
        apply(&mut s, DeskCommand::Bookmark { pane, label: "reading context".into() });
        assert_eq!(s.save_checkpoint(3, &saved, 1024, || false).error(), None);
        fs::rename(&path, root.join("moved.md")).unwrap();
        let restored = s.restore_checkpoint_file(3, 4, &saved, || false).unwrap().active.unwrap();
        assert!(doc.validate_source(&s, 4).is_err()); assert_ne!(restored, pane);
        let new_doc = prepare(&mut s, restored, 2); let at = s.model().location(restored, 4).unwrap().offset;
        assert!(new_doc.split(&mut s, 4, 2, at, 16, || false).is_ok());
        assert_eq!(s.model().bookmarks()[0].label(), "reading context"); assert!(!path.exists());
    }
}

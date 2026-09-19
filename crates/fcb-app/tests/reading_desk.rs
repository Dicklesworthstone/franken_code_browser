#![forbid(unsafe_code)]

mod support;
use support::{Json, parse};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange, FileId, SourceCapture, SourceRevision};
use fcb::search::{ReadingTarget, ReadingWindowOptions, LineNumber};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, desk::{DeskSession, DeskSessionError, DeskLimits, DeskCommand, DeskError, DeskPaneId}};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(777).unwrap() }
fn source(id: u64, rev: u64, bytes: &[u8]) -> SourceCapture {
    SourceCapture::from_bytes(owner(), FileId::new(owner(), id).unwrap(), SourceRevision::new(owner(), rev).unwrap(),
        format!("file-{id}.rs"), bytes.to_vec()).unwrap()
}
fn range(a: u64, b: u64) -> ByteRange { ByteRange::new(ByteOffset::new(a), ByteOffset::new(b)).unwrap() }
fn session() -> DeskSession { DeskSession::new(owner(), DeskLimits::default()).unwrap() }
fn json(response: &HostResponse) -> Json {
    let result = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(result.get("schema").text(), "fcb.reading-desk/1");
    assert!(!result.get("native_presented").flag()); result
}
fn adopt(s: &mut DeskSession, capture: SourceCapture) -> DeskPaneId {
    s.adopt(s.model().revision(), s.model().last_attempt() + 1, capture, 0, None, || false).unwrap().active.unwrap()
}
fn apply(s: &mut DeskSession, command: DeskCommand) {
    s.apply(s.model().revision(), s.model().last_attempt() + 1, command, || false).unwrap();
}
fn window(s: &mut DeskSession, pane: DeskPaneId) -> Json {
    json(&s.window(s.model().revision(), pane, None, ReadingWindowOptions::default(), || false).unwrap())
}

#[test]
fn adopted_capture_flows_through_window_find_activation_copy_and_history() {
    let mut s = session();
    let first = adopt(&mut s, source(1, 1, b"first\r\nneedle\r\nlast"));
    let find = s.search(1, first, 1, "needle", 10, 1000, || false).unwrap();
    assert_eq!(find.exit_code(), EXIT_OK);
    assert_eq!(json(&find).get("hits").array().len(), 1);
    s.activate_hit(1, 2, first, 1, 0, || false).unwrap();
    let copy = json(&s.copy_selection(2, first, || false).unwrap());
    assert_eq!(copy.get("original_hex").text(), "6e6565646c65");
    assert_eq!(window(&mut s, first).get("text").text(), "needle\r\nlast");
    adopt(&mut s, source(2, 2, b"other"));
    apply(&mut s, DeskCommand::Back);
    assert_eq!(window(&mut s, first).get("text").text(), "needle\r\nlast");
    assert_eq!(json(&s.state(|| false).unwrap()).get("initial_source_bytes_read").number(), 0);
}

#[test]
fn duplicate_views_reuse_shared_capture_and_independent_selections() {
    let mut s = session(); let first = adopt(&mut s, source(1, 1, b"one two"));
    let duplicate = s.apply(1, 2, DeskCommand::Duplicate(first), || false).unwrap().active.unwrap();
    apply(&mut s, DeskCommand::Navigate { pane: first, offset: 0, selection: Some(range(0, 3)) });
    apply(&mut s, DeskCommand::Navigate { pane: duplicate, offset: 4, selection: Some(range(4, 7)) });
    let a = s.model().source(first, s.model().revision()).unwrap();
    let b = s.model().source(duplicate, s.model().revision()).unwrap();
    assert_eq!(a.bytes().as_ptr(), b.bytes().as_ptr());
    assert_eq!(s.model().retained_source_bytes(), 7);
    assert_eq!(json(&s.copy_selection(s.model().revision(), first, || false).unwrap()).get("original_hex").text(), "6f6e65");
    assert_eq!(json(&s.copy_selection(s.model().revision(), duplicate, || false).unwrap()).get("original_hex").text(), "74776f");
}

#[test]
fn utf16_search_and_window_keep_original_offsets_and_crlf() {
    let mut s = session();
    let mut bytes = vec![0xff, 0xfe];
    for unit in "a\r\n😀 goal".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    let pane = adopt(&mut s, source(1, 1, &bytes));
    let result = s.search(1, pane, 1, "goal", 10, 1000, || false).unwrap();
    assert_eq!(json(&result).get("hits").array()[0].get("original_range").get("start").number(), 14);
    s.activate_hit(1, 2, pane, 1, 0, || false).unwrap();
    assert_eq!(window(&mut s, pane).get("text").text(), "goal");
    assert_eq!(json(&s.copy_selection(2, pane, || false).unwrap()).get("original_hex").text(), "67006f0061006c00");
}

#[test]
fn pane_replacement_cannot_activate_an_old_search_result() {
    let mut s = session(); let pane = adopt(&mut s, source(1, 1, b"same"));
    s.search(1, pane, 1, "same", 10, 100, || false).unwrap();
    adopt(&mut s, source(1, 2, b"same but different revision"));
    assert_eq!(s.activate_hit(2, 3, pane, 1, 0, || false), Err(DeskSessionError::StaleQuery));
    apply(&mut s, DeskCommand::Back);
    s.activate_hit(3, 4, pane, 1, 0, || false).unwrap();
    assert_eq!(json(&s.copy_selection(4, pane, || false).unwrap()).get("original_hex").text(), "73616d65");
}

#[test]
fn canceled_query_preserves_previous_rows_and_generation() {
    let mut s = session(); let pane = adopt(&mut s, source(1, 1, b"first second"));
    s.search(1, pane, 1, "first", 10, 100, || false).unwrap();
    let result = s.search(1, pane, 2, "second", 10, 100, || true);
    assert!(matches!(result, Err(error) if error.is_canceled()));
    assert_eq!(s.accepted_query(), Some(1));
    s.activate_hit(1, 2, pane, 1, 0, || false).unwrap();
    assert_eq!(json(&s.copy_selection(2, pane, || false).unwrap()).get("original_hex").text(), "6669727374");
}

#[test]
fn truncated_and_byte_limited_queries_are_not_complete_negatives() {
    let mut s = session(); let pane = adopt(&mut s, source(1, 1, b"a a a"));
    let result = s.search(1, pane, 1, "a", 1, 100, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL);
    assert!(!json(&result).get("search_complete").flag());
    assert_eq!(json(&result).get("hits").array().len(), 1);
    let result = s.search(1, pane, 2, "missing", 10, 0, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL);
    assert!(!json(&result).get("search_complete").flag());
}

#[test]
fn physical_source_bytes_are_never_reconstructed_from_replacement_text() {
    let mut s = session(); let pane = adopt(&mut s, source(1, 1, &[b'a', 0, 0xff, b'\r', b'\n']));
    apply(&mut s, DeskCommand::Navigate { pane, offset: 0, selection: Some(range(0, 5)) });
    let response = window(&mut s, pane);
    assert!(response.get("has_replacements").flag());
    assert_eq!(json(&s.copy_selection(2, pane, || false).unwrap()).get("original_hex").text(), "6100ff0d0a");
}

#[test]
fn line_navigation_uses_reader_editor_rows_and_returns_continuation() {
    let mut s = session(); let pane = adopt(&mut s, source(1, 1, b"a\r\nb\rc\n"));
    let response = s.window(1, pane, Some(ReadingTarget::Line(LineNumber::new(2).unwrap())),
        ReadingWindowOptions { max_bytes: 32, max_lines: 1 }, || false).unwrap();
    let result = json(&response);
    assert_eq!(result.get("text").text(), "b\r");
    assert_eq!(result.get("rows").array()[0].get("line").number(), 2);
    assert_eq!(result.get("next_offset").number(), 5);
    assert!(!result.get("reaches_eof").flag());
    let empty = adopt(&mut s, source(2, 2, b""));
    assert_eq!(window(&mut s, empty).get("rows").array()[0].get("line").number(), 1);
}

#[test]
fn bookmark_recall_and_pin_escape_are_a_real_source_workflow() {
    let mut s = session(); let first = adopt(&mut s, source(1, 1, b"saved"));
    apply(&mut s, DeskCommand::Navigate { pane: first, offset: 0, selection: Some(range(0, 5)) });
    let mark = s.apply(2, 3, DeskCommand::Bookmark { pane: first, label: "context".into() }, || false)
        .unwrap().created_bookmark.unwrap();
    apply(&mut s, DeskCommand::Pin { pane: first, pinned: true });
    let other = adopt(&mut s, source(2, 2, b"other"));
    assert_ne!(first, other);
    apply(&mut s, DeskCommand::Escape);
    apply(&mut s, DeskCommand::Close(first));
    apply(&mut s, DeskCommand::ClearHistory);
    assert_eq!(s.model().retained_source_count(), 1);
    apply(&mut s, DeskCommand::RecallBookmark(mark));
    let restored = s.model().active().unwrap();
    assert_eq!(json(&s.copy_selection(s.model().revision(), restored, || false).unwrap()).get("original_hex").text(), "7361766564");
}

#[test]
fn stale_window_and_foreign_capture_do_not_change_active_source() {
    let mut s = session(); let pane = adopt(&mut s, source(1, 1, b"safe"));
    assert!(matches!(s.window(0, pane, None, ReadingWindowOptions::default(), || false),
        Err(DeskSessionError::Desk(DeskError::StaleRevision))));
    let alien = ArenaOwnerId::new(999).unwrap();
    let source = SourceCapture::from_bytes(alien, FileId::new(alien, 1).unwrap(),
        SourceRevision::new(alien, 1).unwrap(), "foreign", b"bad".to_vec()).unwrap();
    assert_eq!(s.adopt(1, 2, source, 0, None, || false), Err(DeskSessionError::Desk(DeskError::OwnerMismatch)));
    assert_eq!(window(&mut s, pane).get("text").text(), "safe");
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
fn file_open_then_replace_and_rename_never_changes_pinned_source() {
    use std::{fs, time::{SystemTime, UNIX_EPOCH}};
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let directory = std::env::temp_dir().join(format!("fcb-desk-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("live.rs"); fs::write(&path, b"original").unwrap();
    let mut s = session();
    let first = s.open_file(0, 1, &path, || false).unwrap().active.unwrap();
    apply(&mut s, DeskCommand::Pin { pane: first, pinned: true });
    fs::write(&path, b"replacement").unwrap();
    let second = s.open_file(2, 3, &path, || false).unwrap().active.unwrap();
    fs::rename(&path, directory.join("moved.rs")).unwrap();
    assert_eq!(window(&mut s, first).get("text").text(), "original");
    assert_eq!(window(&mut s, second).get("text").text(), "replacement");
    let state = json(&s.state(|| false).unwrap());
    assert_eq!(state.get("initial_source_bytes_read").number(), 19);
    assert_eq!(state.get("additional_source_bytes_read").number(), 0);
}

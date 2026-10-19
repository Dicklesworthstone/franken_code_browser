#![forbid(unsafe_code)]

mod support;
use support::{Json, parse};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange, FileId, SourceCapture, SourceRevision};
use fcb::ui::reading_panes::desk::{MAX_ANNOTATION_BODY_BYTES, DeskChange};
use fcb_app::host::desk::{DeskSession, DeskSessionError, DeskCommand, DeskError, DeskLimits, DeskPaneId};

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn session(n: u64) -> DeskSession { DeskSession::new(owner(n), DeskLimits::default()).unwrap() }
fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
}
fn adopt(session: &mut DeskSession, bytes: &[u8]) -> DeskPaneId {
    let domain = session.model().owner();
    let capture = SourceCapture::from_bytes(domain, FileId::new(domain, 1).unwrap(),
        SourceRevision::new(domain, 1).unwrap(), "retained.rs", bytes.to_vec()).unwrap();
    session.adopt(0, 1, capture, 0, None, || false).unwrap().active.unwrap()
}
fn apply(session: &mut DeskSession, command: DeskCommand) -> DeskChange {
    session.apply(session.model().revision(), session.model().last_attempt() + 1, command, || false).unwrap()
}
fn state(session: &mut DeskSession) -> Json {
    let reply = session.state(|| false).unwrap();
    let value = parse(reply.as_str().as_bytes()).unwrap();
    assert_eq!(value.get("schema").text(), "fcb.reading-desk/1");
    assert_eq!(value.get("additional_source_bytes_read").number(), 0);
    assert!(!value.get("native_presented").flag());
    value
}

#[test]
fn annotations_round_trip_personal_text_without_changing_their_source_anchor() {
    let mut desk = session(401);
    let pane = adopt(&mut desk, b"old\r\nexact\0\xff");
    apply(&mut desk, DeskCommand::Navigate { pane, offset: 5, selection: Some(range(5, 10)) });
    let note = "Reason\n\tquoted: \"yes\"; emoji: 😀; embedded\0NUL";
    let bookmark = apply(&mut desk, DeskCommand::Annotate {
        pane, label: "Why this range".into(), body: note.into(),
    }).created_bookmark.unwrap();
    let original = desk.model().bookmarks()[0].location();
    let first = state(&mut desk);
    let mark = &first.get("bookmarks").array()[0];
    assert_eq!(mark.get("body").text(), note);
    assert_eq!(mark.get("update_sequence").number(), 1);
    assert_eq!(mark.get("selection").get("start").number(), 5);
    assert_eq!(mark.get("selection").get("end").number(), 10);

    apply(&mut desk, DeskCommand::Navigate { pane, offset: 0, selection: None });
    apply(&mut desk, DeskCommand::EditBookmark {
        bookmark, label: "Revised rationale".into(), body: "Keep the original anchor.".into(),
    });
    assert_eq!(desk.model().bookmarks()[0].location(), original);
    let edited = state(&mut desk);
    let mark = &edited.get("bookmarks").array()[0];
    assert_eq!(mark.get("bookmark").number(), bookmark);
    assert_eq!(mark.get("label").text(), "Revised rationale");
    assert_eq!(mark.get("update_sequence").number(), 2);
    assert_eq!(mark.get("body").text(), "Keep the original anchor.");

    apply(&mut desk, DeskCommand::Close(pane));
    apply(&mut desk, DeskCommand::ClearHistory);
    assert_eq!(desk.model().retained_source_count(), 1);
    apply(&mut desk, DeskCommand::RecallBookmark(bookmark));
    let recalled = desk.model().active().unwrap();
    let copied = desk.copy_selection(desk.model().revision(), recalled, || false).unwrap();
    assert_eq!(parse(copied.as_str().as_bytes()).unwrap().get("original_hex").text(), "6578616374");
    assert_eq!(state(&mut desk).get("initial_source_bytes_read").number(), 0);
}

#[test]
fn ordinary_bookmarks_can_gain_or_clear_notes_without_becoming_new_bookmarks() {
    let mut desk = session(402); let pane = adopt(&mut desk, b"retained");
    let id = apply(&mut desk, DeskCommand::Bookmark { pane, label: "Landmark".into() }).created_bookmark.unwrap();
    assert_eq!(state(&mut desk).get("bookmarks").array()[0].get("body").text(), "");
    apply(&mut desk, DeskCommand::EditBookmark { bookmark: id, label: "Renamed".into(), body: "Explanation".into() });
    apply(&mut desk, DeskCommand::EditBookmark { bookmark: id, label: "Renamed".into(), body: String::new() });
    let snapshot = state(&mut desk);
    assert_eq!(snapshot.get("bookmarks").array().len(), 1);
    assert_eq!(snapshot.get("bookmarks").array()[0].get("bookmark").number(), id);
    assert_eq!(snapshot.get("bookmarks").array()[0].get("body").text(), "");
    assert_eq!(snapshot.get("bookmarks").array()[0].get("update_sequence").number(), 3);
    apply(&mut desk, DeskCommand::ForgetBookmark(id));
    assert!(state(&mut desk).get("bookmarks").array().is_empty());
}

#[test]
fn canceled_or_replayed_annotation_changes_preserve_accepted_text() {
    let mut desk = session(403); let pane = adopt(&mut desk, b"capture");
    let bookmark = apply(&mut desk, DeskCommand::Annotate {
        pane, label: "Accepted".into(), body: "Keep me".into(),
    }).created_bookmark.unwrap();
    let revision = desk.model().revision();
    assert_eq!(desk.apply(revision, revision, DeskCommand::Annotate {
        pane, label: "Duplicate".into(), body: "Must not appear".into(),
    }, || false), Err(DeskSessionError::Desk(DeskError::StaleAttempt)));
    assert!(matches!(desk.apply(revision, revision + 1, DeskCommand::EditBookmark {
        bookmark, label: "Canceled".into(), body: "Must not appear".into(),
    }, || true), Err(error) if error.is_canceled()));
    let snapshot = state(&mut desk);
    assert_eq!(snapshot.get("model_revision").number(), revision);
    let marks = snapshot.get("bookmarks").array();
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].get("body").text(), "Keep me");
    assert_eq!(marks[0].get("update_sequence").number(), 1);
    apply(&mut desk, DeskCommand::EditBookmark { bookmark, label: "After cancel".into(), body: "Next accepted edit".into() });
    assert_eq!(state(&mut desk).get("bookmarks").array()[0].get("body").text(), "Next accepted edit");
}

#[test]
fn full_admitted_note_set_fits_wire_budget_even_with_maximum_json_escaping() {
    let mut desk = session(404); let pane = adopt(&mut desk, b"source");
    let body = "\0".repeat(MAX_ANNOTATION_BODY_BYTES);
    for index in 0..DeskLimits::default().bookmarks {
        apply(&mut desk, DeskCommand::Annotate {
            pane, label: format!("Note {index}"), body: body.clone(),
        });
    }
    let response = desk.state(|| false).unwrap();
    assert!(response.as_str().len() < fcb_app::output::MAX_RESPONSE_BYTES);
    let snapshot = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(snapshot.get("bookmarks").array().len(), DeskLimits::default().bookmarks);
    for mark in snapshot.get("bookmarks").array() { assert_eq!(mark.get("body").text(), body); }
    let original = desk.model().revision();
    assert_eq!(desk.apply(original, desk.model().last_attempt() + 1, DeskCommand::EditBookmark {
        bookmark: 1, label: "Too large".into(), body: "x".repeat(MAX_ANNOTATION_BODY_BYTES + 1),
    }, || false), Err(DeskSessionError::Desk(DeskError::InvalidBody)));
    assert_eq!(desk.model().revision(), original);
    assert_eq!(desk.model().bookmarks()[0].body(), body);
}

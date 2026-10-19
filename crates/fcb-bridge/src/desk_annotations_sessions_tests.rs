//! Personal-note operations through the production native session registry.
use super::*;
use fcb::{FileId, SourceCapture, SourceRevision};

fn populated() -> (DeskSessions, u64, u64) {
    let desks = DeskSessions::new(); let handle = desks.create().unwrap();
    desks.start(handle, || false).unwrap();
    let pane = desks.with_session(handle, || false, |desk, stop| {
        let owner = desk.model().owner();
        let source = SourceCapture::from_bytes(owner, FileId::new(owner, 1).unwrap(),
            SourceRevision::new(owner, 1).unwrap(), "kept.rs", b"exact source".to_vec()).unwrap();
        Ok(desk.adopt(0, 1, source, 0, Some(range(0, 5)?), &mut *stop)?.active.unwrap().get())
    }).unwrap();
    (desks, handle, pane)
}
fn number(json: &str, field: &str) -> u64 {
    let marker = format!("\"{field}\":\"");
    json.split_once(&marker).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

#[test]
fn native_note_create_edit_recall_and_forget_keep_the_original_anchor() {
    let (desks, handle, pane) = populated();
    let added = desks.execute(handle, Command::Annotate { expected: 1, attempt: 2,
        pane, label: "Rationale", body: "Multiline\nnote\0with exact controls" }, || false).unwrap();
    assert!(added.as_str().contains("\"body\":\"Multiline\\nnote\\u0000with exact controls\""));
    assert!(added.as_str().contains("\"update_sequence\":\"1\""));
    let id = number(added.as_str(), "bookmark");
    desks.execute(handle, Command::Apply { expected: 2, attempt: 3, kind: 1,
        subject: pane, start: 6, end: 0, label: "" }, || false).unwrap();
    let edited = desks.execute(handle, Command::EditBookmark { expected: 3, attempt: 4,
        bookmark: id, label: "Edited", body: "This still refers to exact." }, || false).unwrap();
    assert!(edited.as_str().contains("\"body\":\"This still refers to exact.\""));
    assert!(edited.as_str().contains("\"update_sequence\":\"2\""));
    desks.execute(handle, Command::Apply { expected: 4, attempt: 5, kind: 4,
        subject: id, start: 0, end: 0, label: "" }, || false).unwrap();
    let page = desks.execute(handle, Command::Window { expected: 5, pane, offset: 0, bytes: 64 }, || false).unwrap();
    assert!(page.as_str().contains("\"selection\":{\"start\":\"0\",\"end\":\"5\"}"));
    assert!(page.as_str().contains("\"text\":\"exact source\""));
    let forgotten = desks.execute(handle, Command::Apply { expected: 5, attempt: 6, kind: 5,
        subject: id, start: 0, end: 0, label: "" }, || false).unwrap();
    assert!(forgotten.as_str().contains("\"bookmarks\":[]"));
    assert!(forgotten.as_str().contains("\"initial_source_bytes_read\":\"0\""));
}

#[test]
fn canceled_delivery_after_personal_state_acceptance_is_reconciled_without_replaying_the_note() {
    let (desks, handle, pane) = populated();
    let result = desks.with_session(handle, || false, |desk, _| {
        let pane = desk.model().pane_id(pane).map_err(DeskSessionError::from)?;
        desk.apply(1, 2, DeskCommand::Annotate { pane, label: "Accepted".into(), body: "Keep once".into() }, || false)?;
        desks.cancel(handle)?;
        Ok(desk.state(|| false)?)
    });
    assert!(matches!(result, Err(AccessError::Canceled)));
    let recovered = desks.execute(handle, Command::State, || false).unwrap();
    assert_eq!(number(recovered.as_str(), "model_revision"), 2);
    assert_eq!(number(recovered.as_str(), "last_attempt"), 2);
    assert!(recovered.as_str().contains("\"body\":\"Keep once\""));
    assert!(desks.execute(handle, Command::Annotate { expected: 2, attempt: 2,
        pane, label: "Duplicate", body: "Do not add" }, || false).is_err());
    let after = desks.execute(handle, Command::State, || false).unwrap();
    assert_eq!(after.as_str().matches("\"bookmark\":").count(), 1);
    assert!(!after.as_str().contains("Do not add"));
    desks.execute(handle, Command::EditBookmark { expected: 2, attempt: 3,
        bookmark: 1, label: "Continued", body: "Next operation" }, || false).unwrap();
}

#[test]
fn note_admission_errors_do_not_remove_prior_notes_or_capture_bytes() {
    let (desks, handle, pane) = populated();
    desks.execute(handle, Command::Annotate { expected: 1, attempt: 2,
        pane, label: "Keep", body: "Preserved" }, || false).unwrap();
    let too_large = "x".repeat(fcb::ui::reading_panes::desk::MAX_ANNOTATION_BODY_BYTES + 1);
    assert!(desks.execute(handle, Command::EditBookmark { expected: 2, attempt: 3,
        bookmark: 1, label: "Too much", body: &too_large }, || false).is_err());
    assert!(desks.execute(handle, Command::EditBookmark { expected: 2, attempt: 4,
        bookmark: 1, label: "Canceled", body: "Do not apply" }, || true).is_err());
    let state = desks.execute(handle, Command::State, || false).unwrap();
    assert!(state.as_str().contains("\"body\":\"Preserved\""));
    assert_eq!(number(state.as_str(), "model_revision"), 2);
    assert_eq!(number(state.as_str(), "retained_source_bytes"), 12);
    assert!(state.as_str().contains("\"update_sequence\":\"1\""));
}

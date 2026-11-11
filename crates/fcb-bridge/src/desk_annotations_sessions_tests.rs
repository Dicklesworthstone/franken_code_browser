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


#[test]
fn native_apply_kind_twelve_maps_only_the_explicit_captured_selection() {
    let (desks, handle, pane) = populated();
    let added = desks.execute(handle, Command::Annotate { expected: 1, attempt: 2,
        pane, label: "Evidence", body: "Preserve rationale\0exactly." }, || false).unwrap();
    let bookmark = number(added.as_str(), "bookmark");
    let target = desks.with_session(handle, || false, |desk, stop| {
        let owner = desk.model().owner();
        let source = SourceCapture::from_bytes(owner, FileId::new(owner, 2).unwrap(),
            SourceRevision::new(owner, 2).unwrap(), "chosen.rs", b"prefix exact".to_vec()).unwrap();
        Ok(desk.adopt(2, 3, source, 7, Some(range(7, 12)?), &mut *stop)?.active.unwrap().get())
    }).unwrap();
    let mapped = desks.execute(handle, Command::Apply { expected: 3, attempt: 4, kind: 12,
        subject: bookmark, start: target, end: 1, label: "" }, || false).unwrap();
    assert_eq!(number(mapped.as_str(), "model_revision"), 4);
    assert!(mapped.as_str().contains("\"body\":\"Preserve rationale\\u0000exactly.\""));
    assert!(mapped.as_str().contains("\"update_sequence\":\"2\""));
    assert!(mapped.as_str().contains("\"initial_source_bytes_read\":\"0\""));
    desks.with_session(handle, || false, |desk, _| {
        let mark = &desk.model().bookmarks()[0];
        assert_eq!(mark.id(), bookmark);
        assert_eq!(mark.location().selection, Some(range(7, 12)?));
        assert_eq!(mark.location().revision.get(), 2);
        assert_eq!(mark.body(), "Preserve rationale\0exactly.");
        Ok(())
    }).unwrap();
    assert!(matches!(desks.execute(handle, Command::Apply { expected: 4, attempt: 5, kind: 12,
        subject: bookmark, start: target, end: 1, label: "" }, || false),
        Err(AccessError::Desk(DeskSessionError::Desk(DeskError::StaleBookmark)))));
    let state = desks.execute(handle, Command::State, || false).unwrap();
    assert_eq!(number(state.as_str(), "model_revision"), 4);
    assert_eq!(number(state.as_str(), "last_attempt"), 5);
    assert!(state.as_str().contains("\"update_sequence\":\"2\""));
}

#[test]
fn lost_reattachment_delivery_keeps_one_committed_mapping_for_reconciliation() {
    let (desks, handle, pane) = populated();
    desks.execute(handle, Command::Annotate { expected: 1, attempt: 2,
        pane, label: "Keep", body: "Saved once" }, || false).unwrap();
    let target = desks.with_session(handle, || false, |desk, stop| {
        let owner = desk.model().owner();
        let source = SourceCapture::from_bytes(owner, FileId::new(owner, 2).unwrap(),
            SourceRevision::new(owner, 2).unwrap(), "new.rs", b"new exact".to_vec()).unwrap();
        Ok(desk.adopt(2, 3, source, 4, Some(range(4, 9)?), &mut *stop)?.active.unwrap())
    }).unwrap();
    let lost = desks.with_session(handle, || false, |desk, _| {
        desk.apply(3, 4, DeskCommand::ReattachBookmark {
            bookmark: 1, expected_update_sequence: 1, pane: target,
        }, || false)?;
        desks.cancel(handle)?;
        Ok(desk.state(|| false)?)
    });
    assert!(matches!(lost, Err(AccessError::Canceled)));
    let recovered = desks.execute(handle, Command::State, || false).unwrap();
    assert_eq!(number(recovered.as_str(), "model_revision"), 4);
    assert_eq!(number(recovered.as_str(), "last_attempt"), 4);
    assert!(recovered.as_str().contains("\"update_sequence\":\"2\""));
    assert!(matches!(desks.execute(handle, Command::Apply { expected: 4, attempt: 4, kind: 12,
        subject: 1, start: target.get(), end: 1, label: "" }, || false),
        Err(AccessError::Desk(DeskSessionError::Desk(DeskError::StaleAttempt)))));
    desks.with_session(handle, || false, |desk, _| {
        let mark = &desk.model().bookmarks()[0];
        assert_eq!(mark.location().selection, Some(range(4, 9)?));
        assert_eq!(mark.body(), "Saved once");
        assert_eq!(mark.update_sequence(), 2);
        Ok(())
    }).unwrap();
}


#[test]
fn guided_proposal_is_readonly_and_lost_atomic_confirmation_is_reconciled_once() {
    let (desks, handle, pane) = populated();
    desks.execute(handle, Command::Annotate { expected: 1, attempt: 2,
        pane, label: "Keep", body: "Exact personal text" }, || false).unwrap();
    let target = desks.with_session(handle, || false, |desk, stop| {
        let owner = desk.model().owner();
        let source = SourceCapture::from_bytes(owner, FileId::new(owner, 2).unwrap(),
            SourceRevision::new(owner, 2).unwrap(), "updated.rs", b"prefix exact".to_vec()).unwrap();
        Ok(desk.adopt(2, 3, source, 1, None, &mut *stop)?.active.unwrap().get())
    }).unwrap();
    let before = desks.execute(handle, Command::State, || false).unwrap();
    let partial = desks.execute(handle, Command::SuggestReattachment { expected: 3, bookmark: 1,
        sequence: 1, pane: target, max_work: 0, max_edits: 256 }, || false).unwrap();
    assert!(partial.as_str().contains("\"outcome\":\"unresolved\""));
    assert!(partial.as_str().contains("\"target_range\":null"));
    assert!(desks.execute(handle, Command::SuggestReattachment { expected: 3, bookmark: 1,
        sequence: 1, pane: target, max_work: 1024, max_edits: 256 }, || true).is_err());
    let after = desks.execute(handle, Command::State, || false).unwrap();
    assert_eq!(after.as_str(), before.as_str());

    let lost = desks.with_session(handle, || false, |desk, stop| {
        let pane = desk.model().pane_id(target).map_err(DeskSessionError::from)?;
        let location = desk.model().location(pane, 3).map_err(DeskSessionError::from)?;
        desk.apply(3, 4, DeskCommand::ReattachBookmarkRange { bookmark: 1, expected_update_sequence: 1,
            pane, file: location.file, revision: location.revision, selection: range(7, 12)? }, &mut *stop)?;
        desks.cancel(handle)?;
        Ok(desk.state(|| false)?)
    });
    assert!(matches!(lost, Err(AccessError::Canceled)));
    let recovered = desks.execute(handle, Command::State, || false).unwrap();
    assert_eq!(number(recovered.as_str(), "model_revision"), 4);
    assert!(recovered.as_str().contains("\"update_sequence\":\"2\""));
    assert!(recovered.as_str().contains("\"body\":\"Exact personal text\""));
    desks.with_session(handle, || false, |desk, _| {
        let pane = desk.model().pane_id(target).map_err(DeskSessionError::from)?;
        let location = desk.model().location(pane, 4).map_err(DeskSessionError::from)?;
        assert_eq!(location.offset, 1); assert_eq!(location.selection, None);
        assert_eq!(desk.model().bookmarks()[0].location().selection, Some(range(7, 12)?));
        Ok(())
    }).unwrap();
    assert!(desks.execute(handle, Command::ReattachRange { expected: 4, attempt: 4,
        bookmark: 1, sequence: 1, pane: target, file: 2, revision: 2, start: 7, end: 12 }, || false).is_err());
    assert_eq!(number(desks.execute(handle, Command::State, || false).unwrap().as_str(), "model_revision"), 4);
}

use super::*;

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn range(a: u64, b: u64) -> ByteRange { ByteRange::new(ByteOffset::new(a), ByteOffset::new(b)).unwrap() }
fn capture(file: u64, rev: u64, text: &[u8]) -> SourceCapture {
    SourceCapture::from_bytes(owner(900), FileId::new(owner(900), file).unwrap(),
        SourceRevision::new(owner(900), rev).unwrap(), format!("file-{file}.rs"), text.to_vec()).unwrap()
}
fn setup(limits: DeskLimits) -> (ReadingDesk, ResourceBudget) {
    let budget = ResourceBudget::new(owner(900), ByteLength::new(16 * 1024 * 1024)).unwrap();
    (ReadingDesk::new(owner(900), limits, &budget, allocation(1)).unwrap(), budget)
}
fn open(d: &mut ReadingDesk, c: SourceCapture, select: Option<ByteRange>) -> DeskPaneId {
    let n = d.last_attempt() + 1;
    d.open(d.revision(), n, c, 0, select, allocation(1000 + n), || false).unwrap().active.unwrap()
}
fn command(d: &mut ReadingDesk, cmd: DeskCommand) -> DeskChange {
    d.apply(d.revision(), d.last_attempt() + 1, cmd, || false).unwrap()
}

#[test]
fn empty_capture_is_a_real_readable_pane_not_a_refusal() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b""), None);
    assert_eq!(desk.source(pane, desk.revision()).unwrap().bytes(), b"");
    assert_eq!(desk.location(pane, desk.revision()).unwrap().offset, 0);
    assert_eq!(desk.panes().active_pane().unwrap().revision, Some(SourceRevision::new(owner(900), 1).unwrap()));
    assert_eq!(desk.panes().active_pane().unwrap().target_line, None);
}

#[test]
fn back_and_forward_reopen_the_exact_old_capture() {
    let (mut desk, _) = setup(DeskLimits::default());
    let first = open(&mut desk, capture(1, 1, b"old\r\n"), Some(range(0, 3)));
    open(&mut desk, capture(1, 2, b"NEW!"), Some(range(1, 4)));
    assert_eq!(desk.panes().len(), 1);
    assert_eq!(desk.retained_source_count(), 2);
    command(&mut desk, DeskCommand::Back);
    assert_eq!(desk.active(), Some(first));
    assert_eq!(desk.selected_bytes(first, desk.revision()).unwrap(), b"old");
    command(&mut desk, DeskCommand::Forward);
    assert_eq!(desk.selected_bytes(first, desk.revision()).unwrap(), b"EW!");
}

#[test]
fn pins_prevent_other_files_from_replacing_the_pane() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pinned = open(&mut desk, capture(1, 1, b"pinned"), Some(range(0, 6)));
    command(&mut desk, DeskCommand::Pin { pane: pinned, pinned: true });
    let current = open(&mut desk, capture(2, 2, b"current"), None);
    assert_ne!(pinned, current);
    command(&mut desk, DeskCommand::Escape);
    assert_eq!(desk.panes().len(), 1);
    assert_eq!(desk.active(), Some(pinned));
    assert_eq!(desk.selected_bytes(pinned, desk.revision()).unwrap(), b"pinned");
}

#[test]
fn duplicate_panes_share_backing_and_have_independent_source_positions() {
    let (mut desk, _) = setup(DeskLimits::default());
    let original = open(&mut desk, capture(1, 1, b"one two"), Some(range(0, 3)));
    let copy = command(&mut desk, DeskCommand::Duplicate(original)).active.unwrap();
    assert_ne!(original, copy);
    command(&mut desk, DeskCommand::Navigate { pane: copy, offset: 4, selection: Some(range(4, 7)) });
    let a = desk.source(original, desk.revision()).unwrap();
    let b = desk.source(copy, desk.revision()).unwrap();
    assert_eq!(a.bytes().as_ptr(), b.bytes().as_ptr());
    assert_eq!(desk.retained_source_bytes(), 7);
    assert_eq!(desk.selected_bytes(original, desk.revision()).unwrap(), b"one");
    assert_eq!(desk.selected_bytes(copy, desk.revision()).unwrap(), b"two");
}

#[test]
fn bookmark_keeps_closed_source_after_history_is_cleared() {
    let (mut desk, _) = setup(DeskLimits::default());
    let first = open(&mut desk, capture(1, 1, b"saved selection"), Some(range(6, 15)));
    let mark = command(&mut desk, DeskCommand::Bookmark { pane: first, label: "why this matters".into() })
        .created_bookmark.unwrap();
    command(&mut desk, DeskCommand::Close(first));
    command(&mut desk, DeskCommand::ClearHistory);
    assert_eq!(desk.retained_source_count(), 1);
    let restored = command(&mut desk, DeskCommand::RecallBookmark(mark)).active.unwrap();
    assert_ne!(first, restored);
    assert_eq!(desk.selected_bytes(restored, desk.revision()).unwrap(), b"selection");
    assert_eq!(desk.bookmarks()[0].label(), "why this matters");
}

#[test]
fn canceled_open_does_not_publish_source_pane_or_history() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"old"), None);
    let rev = desk.revision();
    let mut calls = 0;
    let result = desk.open(rev, 2, capture(2, 2, b"candidate"), 0, None, allocation(20), || {
        calls += 1; calls == 2
    });
    assert_eq!(result, Err(DeskError::Canceled));
    assert_eq!(desk.revision(), rev);
    assert_eq!(desk.active(), Some(pane));
    assert_eq!(desk.retained_source_count(), 1);
    assert_eq!(desk.history().len(), 1);
    assert_eq!(desk.last_attempt(), 2);
    assert_eq!(desk.apply(rev, 2, DeskCommand::Back, || false), Err(DeskError::StaleAttempt));
}

#[test]
fn mismatched_owner_revision_and_changed_bytes_are_rejected_atomically() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"unchanged"), None);
    assert_eq!(desk.open(1, 2, capture(1, 1, b"mutated"), 0, None, allocation(22), || false), Err(DeskError::IdentityConflict));
    assert_eq!(desk.apply(0, 3, DeskCommand::Close(pane), || false), Err(DeskError::StaleRevision));
    let foreign = DeskPaneId { owner: owner(901), value: pane.get() };
    assert_eq!(desk.apply(1, 3, DeskCommand::Close(foreign), || false), Err(DeskError::OwnerMismatch));
    assert_eq!(desk.source(pane, 1).unwrap().bytes(), b"unchanged");
    assert_eq!(desk.history().len(), 1);
}

#[test]
fn all_pinned_capacity_failure_preserves_focus_and_old_captures() {
    let (mut desk, _) = setup(DeskLimits { panes: 1, ..DeskLimits::default() });
    let pane = open(&mut desk, capture(1, 1, b"old"), None);
    command(&mut desk, DeskCommand::Pin { pane, pinned: true });
    assert_eq!(desk.open(2, 3, capture(2, 2, b"new"), 0, None, allocation(30), || false), Err(DeskError::PaneLimit));
    assert_eq!(desk.active(), Some(pane));
    assert_eq!(desk.revision(), 2);
    assert_eq!(desk.retained_source_count(), 1);
}

#[test]
fn retained_byte_and_source_limits_are_distinct_from_pane_limits() {
    let (mut desk, _) = setup(DeskLimits { retained_bytes: 5, ..DeskLimits::default() });
    open(&mut desk, capture(1, 1, b"123"), None);
    assert_eq!(desk.open(1, 2, capture(2, 2, b"456"), 0, None, allocation(20), || false), Err(DeskError::RetainedByteLimit));
    assert_eq!(desk.retained_source_bytes(), 3);
    let (mut limited, _) = setup(DeskLimits { sources: 1, ..DeskLimits::default() });
    open(&mut limited, capture(1, 1, b"a"), None);
    assert_eq!(limited.open(1, 2, capture(2, 2, b"b"), 0, None, allocation(20), || false), Err(DeskError::SourceLimit));
}

#[test]
fn failed_source_admission_does_not_weaken_existing_state() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"old"), None);
    // Allocation 1 is still the desk's descriptor reservation.
    assert_eq!(desk.open(1, 2, capture(2, 2, b"new"), 0, None, allocation(1), || false), Err(DeskError::ResourceDenied));
    assert_eq!(desk.source(pane, 1).unwrap().bytes(), b"old");
    assert_eq!(desk.retained_source_count(), 1);
}

#[test]
fn scrolling_does_not_flood_history_and_back_restores_viewport() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"0123456789"), None);
    for offset in 1..8 { command(&mut desk, DeskCommand::Scroll { pane, offset }); }
    assert_eq!(desk.history().len(), 1);
    open(&mut desk, capture(2, 2, b"next"), None);
    command(&mut desk, DeskCommand::Back);
    assert_eq!(desk.location(desk.active().unwrap(), desk.revision()).unwrap().offset, 7);
}

#[test]
fn branching_after_back_discards_only_forward_history_and_reclaims_it() {
    let (mut desk, _) = setup(DeskLimits::default());
    open(&mut desk, capture(1, 1, b"a"), None);
    open(&mut desk, capture(2, 2, b"bb"), None);
    command(&mut desk, DeskCommand::Back);
    open(&mut desk, capture(3, 3, b"ccc"), None);
    assert!(!desk.can_go_forward());
    assert_eq!(desk.history().len(), 2);
    assert_eq!(desk.retained_source_count(), 2);
    assert_eq!(desk.retained_source_bytes(), 4);
}

#[test]
fn bounded_history_evicts_old_visits_but_never_a_bookmarked_capture() {
    let (mut desk, _) = setup(DeskLimits { history: 2, ..DeskLimits::default() });
    let pane = open(&mut desk, capture(1, 1, b"a"), None);
    let id = command(&mut desk, DeskCommand::Bookmark { pane, label: "keep".into() }).created_bookmark.unwrap();
    open(&mut desk, capture(2, 2, b"bb"), None);
    open(&mut desk, capture(3, 3, b"ccc"), None);
    assert_eq!(desk.history().len(), 2);
    assert_eq!(desk.retained_source_count(), 3);
    command(&mut desk, DeskCommand::ForgetBookmark(id));
    assert_eq!(desk.retained_source_count(), 2);
}

#[test]
fn exported_view_keeps_the_source_charge_after_desk_drop() {
    let (mut desk, budget) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"retained"), None);
    let view = desk.view(pane, desk.revision(), allocation(3000)).unwrap();
    drop(desk);
    assert_eq!(view.source().bytes(), b"retained");
    assert!(budget.try_reserve_managed(owner(900), allocation(50), ByteLength::new(16 * 1024 * 1024)).is_err());
    drop(view);
    assert!(budget.try_reserve_managed(owner(900), allocation(51), ByteLength::new(16 * 1024 * 1024)).is_ok());
}

#[test]
fn selections_preserve_utf16_nul_invalid_bytes_and_crlf_verbatim() {
    let (mut desk, _) = setup(DeskLimits::default());
    let raw = [0xff, 0xfe, b'a', 0, b'\r', 0, b'\n', 0, 0, 0, 0xff];
    let pane = open(&mut desk, capture(1, 1, &raw), Some(range(2, 11)));
    assert_eq!(desk.selected_bytes(pane, desk.revision()).unwrap(), &raw[2..]);
    assert_eq!(desk.apply(1, 2, DeskCommand::Navigate { pane, offset: 12, selection: None }, || false), Err(DeskError::InvalidLocation));
    assert_eq!(desk.selected_bytes(pane, 1).unwrap(), &raw[2..]);
}

#[test]
fn finite_layout_changes_leave_source_selection_and_history_untouched() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"source"), Some(range(0, 6)));
    command(&mut desk, DeskCommand::Arrange { pane, position: (20.0, 30.0), size: (800.0, 600.0) });
    assert_eq!(desk.panes().active_pane().unwrap().size, (800.0, 600.0));
    assert_eq!(desk.history().len(), 1);
    assert_eq!(desk.selected_bytes(pane, 2).unwrap(), b"source");
    assert_eq!(desk.apply(2, 3, DeskCommand::Arrange { pane, position: (f32::NAN, 0.0), size: (1.0, 1.0) }, || false), Err(DeskError::InvalidLocation));
    assert_eq!(desk.revision(), 2);
}

#[test]
fn bookmark_and_identity_exhaustion_are_transactional() {
    let (mut desk, _) = setup(DeskLimits { bookmarks: 1, ..DeskLimits::default() });
    let pane = open(&mut desk, capture(1, 1, b"a"), None);
    command(&mut desk, DeskCommand::Bookmark { pane, label: "first".into() });
    assert_eq!(desk.apply(2, 3, DeskCommand::Bookmark { pane, label: "second".into() }, || false), Err(DeskError::BookmarkLimit));
    desk.state.panes.next_id = u64::MAX;
    assert_eq!(desk.apply(2, 4, DeskCommand::Duplicate(pane), || false), Err(DeskError::IdentityExhausted));
    assert_eq!(desk.panes().len(), 1);
    assert_eq!(desk.bookmarks().len(), 1);
}

#[test]
fn closing_then_clearing_history_releases_unreferenced_sources() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"released"), None);
    command(&mut desk, DeskCommand::Close(pane));
    assert_eq!(desk.retained_source_count(), 1);
    command(&mut desk, DeskCommand::ClearHistory);
    assert_eq!(desk.retained_source_count(), 0);
    assert_eq!(desk.retained_source_bytes(), 0);
    assert_eq!(desk.pane_id(pane.get()), Err(DeskError::MissingPane));
}

#[test]
fn annotation_keeps_original_bytes_when_the_same_path_gets_unrelated_source() {
    let (mut desk, _) = setup(DeskLimits::default());
    let original = open(&mut desk, capture(1, 1, b"old exact\xff\r\n"), Some(range(0, 9)));
    let id = command(&mut desk, DeskCommand::Annotate {
        pane: original, label: "original evidence".into(), body: "Keep this rationale.\n\0Still personal.".into(),
    }).created_bookmark.unwrap();
    let at = desk.bookmarks()[0].location();
    // The same logical path now refers to different bytes. Neither navigation
    // nor clearing derived history is permission to move or discard the note.
    let replacement = open(&mut desk, capture(1, 2, b"unrelated replacement"), None);
    command(&mut desk, DeskCommand::Close(replacement));
    command(&mut desk, DeskCommand::ClearHistory);
    assert!(desk.panes().is_empty());
    assert!(desk.history().is_empty());
    assert_eq!(desk.retained_source_count(), 1);
    assert_eq!(desk.retained_capture(desk.revision(), at.file, at.revision).unwrap().bytes(), b"old exact\xff\r\n");
    assert_eq!(desk.bookmarks()[0].location(), at);
    assert_eq!(desk.bookmarks()[0].body(), "Keep this rationale.\n\0Still personal.");
    let recalled = command(&mut desk, DeskCommand::RecallBookmark(id)).active.unwrap();
    assert_eq!(desk.selected_bytes(recalled, desk.revision()).unwrap(), b"old exact");
    command(&mut desk, DeskCommand::Close(recalled));
    command(&mut desk, DeskCommand::ClearHistory);
    command(&mut desk, DeskCommand::ForgetBookmark(id));
    assert_eq!(desk.retained_source_count(), 0);
    assert!(desk.bookmarks().is_empty());
}

#[test]
fn bookmark_relabel_and_note_edit_preserve_identity_anchor_and_selection() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"one two"), Some(range(0, 3)));
    let id = command(&mut desk, DeskCommand::Bookmark { pane, label: "first".into() }).created_bookmark.unwrap();
    let original = desk.bookmarks()[0].location();
    assert_eq!(desk.bookmarks()[0].body(), "");
    assert_eq!(desk.bookmarks()[0].update_sequence(), 1);
    command(&mut desk, DeskCommand::Navigate { pane, offset: 4, selection: Some(range(4, 7)) });
    command(&mut desk, DeskCommand::EditBookmark {
        bookmark: id, label: "revised label".into(), body: "Explain the original selection.\r\n".into(),
    });
    assert_eq!(desk.bookmarks()[0].id(), id);
    assert_eq!(desk.bookmarks()[0].location(), original);
    assert_eq!(desk.bookmarks()[0].label(), "revised label");
    assert_eq!(desk.bookmarks()[0].body(), "Explain the original selection.\r\n");
    assert_eq!(desk.bookmarks()[0].update_sequence(), 2);
    assert_eq!(desk.selected_bytes(pane, desk.revision()).unwrap(), b"two");
    command(&mut desk, DeskCommand::RecallBookmark(id));
    assert_eq!(desk.selected_bytes(pane, desk.revision()).unwrap(), b"one");
    command(&mut desk, DeskCommand::EditBookmark { bookmark: id, label: "label only".into(), body: String::new() });
    assert_eq!(desk.bookmarks()[0].body(), "");
    assert_eq!(desk.bookmarks()[0].update_sequence(), 3);
}

#[test]
fn canceled_annotation_edits_and_deletes_publish_neither_text_nor_retirement() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"source"), None);
    let id = command(&mut desk, DeskCommand::Annotate { pane, label: "keep".into(), body: "original note".into() })
        .created_bookmark.unwrap();
    command(&mut desk, DeskCommand::Close(pane));
    command(&mut desk, DeskCommand::ClearHistory);
    let revision = desk.revision();
    for command in [DeskCommand::EditBookmark { bookmark: id, label: "lost".into(), body: "unpublished".into() },
        DeskCommand::ForgetBookmark(id)] {
        let attempt = desk.last_attempt() + 1;
        let mut calls = 0;
        assert_eq!(desk.apply(revision, attempt, command, || { calls += 1; calls == 2 }), Err(DeskError::Canceled));
        assert_eq!(desk.revision(), revision);
        assert_eq!(desk.last_attempt(), attempt);
        assert_eq!(desk.bookmarks()[0].label(), "keep");
        assert_eq!(desk.bookmarks()[0].body(), "original note");
        assert_eq!(desk.bookmarks()[0].update_sequence(), 1);
        assert_eq!(desk.retained_source_count(), 1);
        assert_eq!(desk.apply(revision, attempt, DeskCommand::ForgetBookmark(id), || false), Err(DeskError::StaleAttempt));
    }
    command(&mut desk, DeskCommand::EditBookmark { bookmark: id, label: "accepted".into(), body: "saved once".into() });
    let committed = desk.revision();
    assert_eq!(desk.apply(committed, committed, DeskCommand::EditBookmark {
        bookmark: id, label: "replayed".into(), body: "duplicate".into(),
    }, || false), Err(DeskError::StaleAttempt));
    assert_eq!(desk.bookmarks()[0].body(), "saved once");
    assert_eq!(desk.bookmarks()[0].update_sequence(), 2);
}

#[test]
fn annotation_creation_cancellation_consumes_attempt_without_creating_a_note() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b""), None);
    for stop in [1, 2] {
        let attempt = desk.last_attempt() + 1;
        let mut calls = 0;
        assert_eq!(desk.apply(1, attempt, DeskCommand::Annotate {
            pane, label: "empty source note".into(), body: "Pending.".into(),
        }, || { calls += 1; calls == stop }), Err(DeskError::Canceled));
        assert!(desk.bookmarks().is_empty());
        assert_eq!(desk.revision(), 1);
        assert_eq!(desk.retained_source_count(), 1);
    }
    let created = command(&mut desk, DeskCommand::Annotate { pane, label: "accepted".into(), body: "Empty is still evidence.".into() });
    assert_eq!(created.created_bookmark, Some(1));
    assert_eq!(desk.bookmarks()[0].body(), "Empty is still evidence.");
}

#[test]
fn annotation_and_bookmark_share_count_limit_but_full_capacity_allows_editing() {
    let (mut desk, _) = setup(DeskLimits { bookmarks: 1, ..DeskLimits::default() });
    let pane = open(&mut desk, capture(1, 1, b"source"), None);
    let id = command(&mut desk, DeskCommand::Bookmark { pane, label: "existing".into() }).created_bookmark.unwrap();
    assert_eq!(desk.apply(2, 3, DeskCommand::Annotate { pane, label: "extra".into(), body: "refused".into() }, || false),
        Err(DeskError::BookmarkLimit));
    command(&mut desk, DeskCommand::EditBookmark { bookmark: id, label: "same slot".into(), body: "personal note".into() });
    assert_eq!(desk.bookmarks().len(), 1);
    assert_eq!(desk.bookmarks()[0].body(), "personal note");
    command(&mut desk, DeskCommand::ForgetBookmark(id));
    let next = command(&mut desk, DeskCommand::Annotate { pane, label: "new slot".into(), body: "new note".into() })
        .created_bookmark.unwrap();
    assert!(next > id);
}

#[test]
fn personal_text_bounds_count_utf8_bytes_and_preserve_full_boundary_values() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"source"), None);
    let label = "é".repeat(MAX_BOOKMARK_LABEL_BYTES / 2);
    let body = "😀".repeat(MAX_ANNOTATION_BODY_BYTES / 4);
    let id = command(&mut desk, DeskCommand::Annotate { pane, label: label.clone(), body: body.clone() })
        .created_bookmark.unwrap();
    for (label, body, expected) in [(String::new(), body.clone(), DeskError::InvalidLabel),
        (format!("{label}x"), body.clone(), DeskError::InvalidLabel),
        (label.clone(), format!("{body}x"), DeskError::InvalidBody)] {
        assert_eq!(desk.apply(2, desk.last_attempt() + 1, DeskCommand::EditBookmark { bookmark: id, label, body }, || false),
            Err(expected));
    }
    assert_eq!(desk.bookmarks()[0].label(), label);
    assert_eq!(desk.bookmarks()[0].body(), body);
    assert_eq!(desk.bookmarks()[0].update_sequence(), 1);
    assert_eq!(desk.revision(), 2);
}

#[test]
fn oversized_caller_string_capacities_are_not_retained_by_annotations() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"source"), None);
    let mut label = String::with_capacity(1024 * 1024);
    let mut body = String::with_capacity(1024 * 1024);
    label.push_str("small"); body.push_str("short note\0");
    command(&mut desk, DeskCommand::Annotate { pane, label, body });
    assert!(desk.state.bookmarks[0].label.capacity() <= MAX_BOOKMARK_LABEL_BYTES);
    assert!(desk.state.bookmarks[0].body.capacity() <= MAX_ANNOTATION_BODY_BYTES);
    assert_eq!(desk.bookmarks()[0].body(), "short note\0");
}

#[test]
fn missing_foreign_and_exhausted_note_edits_are_transactional() {
    let (mut desk, _) = setup(DeskLimits::default());
    let pane = open(&mut desk, capture(1, 1, b"source"), None);
    let foreign = DeskPaneId { owner: owner(901), value: pane.get() };
    assert_eq!(desk.apply(1, 2, DeskCommand::Annotate { pane: foreign, label: "foreign".into(), body: "no".into() }, || false),
        Err(DeskError::OwnerMismatch));
    assert_eq!(desk.apply(1, 3, DeskCommand::EditBookmark { bookmark: 1, label: "missing".into(), body: "no".into() }, || false),
        Err(DeskError::MissingBookmark));
    let id = command(&mut desk, DeskCommand::Annotate { pane, label: "keep".into(), body: "original".into() })
        .created_bookmark.unwrap();
    desk.state.bookmarks[0].update_sequence = u64::MAX;
    let before = desk.revision();
    assert_eq!(desk.apply(before, desk.last_attempt() + 1, DeskCommand::EditBookmark {
        bookmark: id, label: "overflow".into(), body: "not published".into(),
    }, || false), Err(DeskError::IdentityExhausted));
    assert_eq!(desk.revision(), before);
    assert_eq!(desk.bookmarks()[0].label(), "keep");
    assert_eq!(desk.bookmarks()[0].body(), "original");
    assert_eq!(desk.bookmarks()[0].update_sequence(), u64::MAX);
    command(&mut desk, DeskCommand::RecallBookmark(id));
    assert_eq!(desk.source(desk.active().unwrap(), desk.revision()).unwrap().bytes(), b"source");
}

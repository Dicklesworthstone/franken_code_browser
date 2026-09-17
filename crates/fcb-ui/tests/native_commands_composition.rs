#![forbid(unsafe_code)]

//! Oracle tests for fcb-8bqc.1 (FCB-053.A): real IME multi-stage
//! composition, standard command routing with menu predicates, and
//! open/drop validation including canceled dialogs and malicious URLs.

use fcb_core::{CoreError, Utf16CodeUnitOffset, Utf16CodeUnitRange};
use fcb_ui::{
    EditorCommand, EditorState, CompositionError, CompositionState, OpenDecision,
    OpenRequest, RefusalReason, validate_open_request,
};

fn range(start: u64, end: u64) -> Utf16CodeUnitRange {
    Utf16CodeUnitRange::new(Utf16CodeUnitOffset::new(start), Utf16CodeUnitOffset::new(end))
        .expect("ordered range valid")
}

#[test]
fn ime_multistage_composition_stages_and_commits() {
    let mut state = CompositionState::new(1 << 16);

    // The user typed `abc` as plain document content first.
    state
        .insert_text("abc", Some(range(0, 0)))
        .expect("initial insert");
    assert_eq!(state.text(), "abc");
    assert!(state.marked().is_none());

    // Multi-stage romaji IME: k -> ka -> か -> 漢, each restaging the same
    // marked span in place.
    state
        .set_marked_text("k", range(3, 3), range(4, 4))
        .expect("stage k");
    assert_eq!(state.text(), "abck");
    let marked1 = state.marked().expect("marked after stage 1");
    assert_eq!(
        (marked1.range.start().get(), marked1.range.end().get()),
        (3, 4)
    );

    state
        .set_marked_text("ka", range(0, 0), range(0, 0))
        .expect("stage ka");
    assert_eq!(state.text(), "abcka", "stage 2 replaces the marked span");
    let marked2 = state.marked().expect("marked after stage 2");
    assert_eq!(
        (marked2.range.start().get(), marked2.range.end().get()),
        (3, 5)
    );

    state
        .set_marked_text("\u{304b}", range(0, 0), range(0, 0))
        .expect("stage kana");
    assert_eq!(state.text(), "abc\u{304b}", "kana replaces romaji in place");

    state
        .set_marked_text("\u{6f22}", range(0, 0), range(0, 0))
        .expect("stage kanji");
    assert_eq!(state.text(), "abc\u{6f22}");

    // Commit converts the marked span into ordinary document content.
    let committed = state
        .insert_text("\u{6f22}\u{5b57}", None)
        .expect("commit");
    assert_eq!(state.text(), "abc\u{6f22}\u{5b57}");
    assert!(state.marked().is_none(), "commit clears the marked region");
    assert_eq!(
        (committed.start().get(), committed.end().get()),
        (3, 5),
        "漢 and 字 are BMP chars: two UTF-16 units total"
    );

    // The selection lands after the committed text.
    assert_eq!(
        (state.selection().start().get(), state.selection().end().get()),
        (3, 5)
    );
}

#[test]
fn ime_commit_replaces_selected_text_and_unmark_confirms() {
    let mut state = CompositionState::new(1 << 16);
    state.insert_text("hello world", Some(range(0, 0))).unwrap();

    // Select `world` (units 6..11) and commit an emoji over it.
    state.set_selection(range(6, 11)).unwrap();
    let committed = state
        .insert_text("\u{1f30d}", Some(range(6, 11)))
        .expect("commit over selection");
    assert_eq!(state.text(), "hello \u{1f30d}");
    assert_eq!(
        (committed.start().get(), committed.end().get()),
        (6, 8),
        "astral emoji occupies two UTF-16 units"
    );

    // unmarkText: confirms an active composition without new text.
    state
        .set_marked_text("ny", range(6, 8), range(8, 8))
        .expect("stage");
    state.unmark_text().expect("unmark confirms");
    assert_eq!(state.text(), "hello ny");
    assert!(state.marked().is_none());

    // unmark with no composition is an explicit error, not a silent no-op.
    let mut fresh = CompositionState::new(1024);
    assert_eq!(fresh.unmark_text(), Err(CompositionError::NothingToCommit));

    // A keyboard selection can never land inside a surrogate pair.
    let mut emoji_doc = CompositionState::new(1024);
    emoji_doc.insert_text("\u{1f6a6}", Some(range(0, 0))).unwrap();
    assert_eq!(
        emoji_doc.set_selection(range(1, 1)),
        Err(CompositionError::MidSurrogate),
        "unit 1 is the low surrogate of the traffic light"
    );

    // The byte budget is enforced across restaging.
    let mut tiny = CompositionState::new(8);
    assert_eq!(
        tiny.set_marked_text("0123456789", range(0, 0), range(0, 0)),
        Err(CompositionError::TextBudgetExceeded)
    );
}

#[test]
fn menu_commands_route_with_menu_validator_predicates() {
    let mut editor = EditorState::new(1 << 16, 8);
    editor.set_document("caf\u{e9} \u{1f6a6} world").unwrap();

    // Copy with empty selection is disabled, exactly like a native menu.
    assert!(!editor.is_enabled(EditorCommand::Copy));
    assert_eq!(
        editor.route(EditorCommand::Copy),
        Ok(fcb_ui::CommandEffect::Disabled)
    );

    // Select the first four units (`caf\u{e9}`) and copy.
    editor.set_selection(range(0, 4)).unwrap();
    assert!(editor.is_enabled(EditorCommand::Copy));
    editor.route(EditorCommand::Copy).unwrap();
    assert_eq!(editor.pasteboard().get(), Some("caf\u{e9}"));

    // Move the caret to the end and paste; the astral emoji round-trips.
    let len = editor.text().chars().map(|c| c.len_utf16() as u64).sum();
    editor.set_selection(range(len, len)).unwrap();
    assert!(editor.is_enabled(EditorCommand::Paste));
    editor
        .route(EditorCommand::Paste)
        .expect("paste applies");
    assert!(editor.text().ends_with("caf\u{e9}"));

    // Undo restores the pre-paste document; redo reapplies it.
    assert!(editor.is_enabled(EditorCommand::Undo));
    editor.route(EditorCommand::Undo).unwrap();
    assert!(!editor.text().ends_with("caf\u{e9}"));
    assert!(editor.is_enabled(EditorCommand::Redo));
    editor.route(EditorCommand::Redo).unwrap();
    assert!(editor.text().ends_with("caf\u{e9}"));

    // The undo stack is bounded: oldest steps fall off.
    for round in 0..12 {
        editor.set_selection(range(0, 1)).unwrap();
        let _ = round;
        editor.route(EditorCommand::Cut).unwrap();
        // Keep the document non-empty for cutting: repaste each round.
        editor.set_selection(range(0, 0)).unwrap();
        editor.route(EditorCommand::Paste).unwrap();
    }
    let mut undo_depth = 0;
    while editor.is_enabled(EditorCommand::Undo) {
        editor.route(EditorCommand::Undo).unwrap();
        undo_depth += 1;
        assert!(undo_depth <= 8, "undo stack must stay bounded");
    }
    assert_eq!(undo_depth, 8, "bounded by max_undo_steps");
}

#[test]
fn open_drop_accepts_spaces_and_unicode_and_refuses_hostile_payloads() {
    // Ordinary filenames with spaces and Unicode are accepted as content.
    let spaced = validate_open_request(OpenRequest::Path(
        "/Users/dev/My Projects/会議 メモ 📄.rs".to_string(),
    ));
    assert_eq!(
        spaced,
        OpenDecision::Accept("/Users/dev/My Projects/会議 メモ 📄.rs".to_string())
    );

    // Canonical file URL form.
    let url = validate_open_request(OpenRequest::Url(
        "file:///Users/dev/notes/%E3%83%89%E3%82%AD%E3%83%A5%E3%83%A1%E3%83%B3%E3%83%88.md"
            .to_string(),
    ));
    assert_eq!(
        url,
        OpenDecision::Accept("/Users/dev/notes/ドキュメント.md".to_string())
    );

    // `file://localhost/...` is the accepted authority form.
    let localhost = validate_open_request(OpenRequest::Url(
        "file://localhost/tmp/a.rs".to_string(),
    ));
    assert_eq!(localhost, OpenDecision::Accept("/tmp/a.rs".to_string()));

    // Canceled dialogs are an explicit terminal state.
    assert_eq!(
        fcb_ui::canceled_dialog(),
        OpenDecision::Refuse(RefusalReason::Canceled)
    );

    // Malicious path payloads are refused, never clamped and never executed.
    for (payload, reason) in [
        ("", RefusalReason::Empty),
        ("relative/path.rs", RefusalReason::NotAbsolute),
        ("/a/../../etc/passwd", RefusalReason::Traversal),
        ("/safe/../../..", RefusalReason::Traversal),
        ("/embedded\0null.rs", RefusalReason::ControlCharacters),
    ] {
        assert_eq!(
            validate_open_request(OpenRequest::Path(payload.to_string())),
            OpenDecision::Refuse(reason),
            "path payload {payload:?}"
        );
    }

    // Malicious URL payloads: non-file schemes are refused before any path
    // logic runs.
    for (payload, reason) in [
        ("", RefusalReason::Empty),
        ("relative/path.rs", RefusalReason::UnsupportedScheme),
        ("/a/../../etc/passwd", RefusalReason::UnsupportedScheme),
        ("x-man-page://ls", RefusalReason::UnsupportedScheme),
        ("exec:///bin/sh", RefusalReason::UnsupportedScheme),
        ("https://example.invalid/file", RefusalReason::UnsupportedScheme),
        ("file://evil.host/etc/passwd", RefusalReason::UnsupportedScheme),
    ] {
        assert_eq!(
            validate_open_request(OpenRequest::Url(payload.to_string())),
            OpenDecision::Refuse(reason),
            "url payload {payload:?}"
        );
    }

    // Encoded traversal cannot smuggle through percent-decoding.
    assert_eq!(
        validate_open_request(OpenRequest::Url(
            "file:///safe/%2e%2e/%2e%2e/etc/passwd".to_string()
        )),
        OpenDecision::Refuse(RefusalReason::Traversal)
    );
    // Malformed escapes are refused rather than partially decoded.
    assert_eq!(
        validate_open_request(OpenRequest::Url("file:///a%zz/b".to_string())),
        OpenDecision::Refuse(RefusalReason::ControlCharacters)
    );
    // Encoded control characters are refused.
    assert_eq!(
        validate_open_request(OpenRequest::Url("file:///a%00b".to_string())),
        OpenDecision::Refuse(RefusalReason::ControlCharacters)
    );

    // Dot components normalize away; interior `..` resolves lexically.
    assert_eq!(
        validate_open_request(OpenRequest::Path(
            "/repo/./src/../src/lib.rs".to_string()
        )),
        OpenDecision::Accept("/repo/src/lib.rs".to_string())
    );
}

#[test]
fn budget_and_state_guards_return_core_errors() {
    let mut editor = EditorState::new(8, 4);
    assert_eq!(
        editor.set_document("0123456789"),
        Err(CoreError::LimitExceeded),
        "document budget is enforced"
    );
    // Selection bounds beyond the document refuse.
    editor.set_document("short").unwrap();
    assert_eq!(
        editor.set_selection(range(0, 99)),
        Err(CoreError::LimitExceeded)
    );
    // Undo/redo on empty stacks are Disabled, not errors.
    assert_eq!(
        editor.route(EditorCommand::Undo),
        Ok(fcb_ui::CommandEffect::Disabled)
    );
    assert_eq!(
        editor.route(EditorCommand::Redo),
        Ok(fcb_ui::CommandEffect::Disabled)
    );
}

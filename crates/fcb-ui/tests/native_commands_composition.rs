#![forbid(unsafe_code)]

//! Oracle tests for fcb-8bqc.1 (FCB-053.A): real IME multi-stage
//! composition, standard command routing with menu predicates, and
//! open/drop validation including canceled dialogs and malicious URLs.

use std::fs;
use std::path::PathBuf;

use fcb_core::{CoreError, Utf16CodeUnitOffset, Utf16CodeUnitRange};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;
use fcb_ui::{
    EditorCommand, EditorState, CompositionError, CompositionState, OpenDecision,
    OpenRequest, RefusalReason, validate_open_request,
};

const RUN_ID_ENV: &str = "FCB_053_RUN_ID";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        PathBuf::from(dir)
    } else {
        let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
        std::env::temp_dir().join(format!("fcb-053-receipts-{run_id}"))
    }
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = fs::create_dir_all(&run_dir);

    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_53_00_01),
        pin: SourcePin::new("0530005300053000530005300053000530005301").expect("pin valid"),
        route: RouteId::new("headless:ui:native-commands").expect("route valid"),
        corpus_digest: ContentDigest::of(detail.as_bytes()),
        corpus_count: 1,
        outcome: TerminalOutcome::new(
            Some(if effect == Effect::Succeeded { 0 } else { 1 }),
            effect,
            None,
        ),
        comparison: Some(ExpectedVsActual::new(
            &Redactor::new(),
            "oracle holds",
            detail,
        )),
        ring: EventRing::new(16),
        artifacts: vec![],
    };

    let receipt = ScenarioReceipt::from_draft(&Redactor::new(), draft);
    let encoded = receipt.encode();
    let parsed = ScenarioReceipt::decode(&encoded).expect("receipt round-trips");
    assert_eq!(parsed.outcome().effect(), receipt.outcome().effect());
    let _ = fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

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

    record_receipt(
        "ime_multistage_composition_stages_and_commits",
        Effect::Succeeded,
        "multi-stage IME staging and commit oracle holds",
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

    record_receipt(
        "ime_commit_replaces_selected_text_and_unmark_confirms",
        Effect::Succeeded,
        "commit replaces selection and unmark confirms",
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

    record_receipt(
        "menu_commands_route_with_menu_validator_predicates",
        Effect::Succeeded,
        "standard command routing with menu validator predicates holds",
    );
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

    record_receipt(
        "open_drop_accepts_spaces_and_unicode_and_refuses_hostile_payloads",
        Effect::Succeeded,
        "open drop path/url validation with traversal refusal holds",
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

    record_receipt(
        "budget_and_state_guards_return_core_errors",
        Effect::Succeeded,
        "budget and state guards enforce core limit errors",
    );
}

#[test]
fn ordinary_unicode_copy_vs_exact_byte_export_contracts() {
    use fcb_ui::{ClipboardFlavor, ClipboardPayload};

    // 1. Ordinary Unicode copy: preserves CRLF, bidi controls, and UTF-16 BOM without silent alteration.
    let unicode_source = "\u{FEFF}Line 1\r\n\u{202A}Hebrew / English\u{202C}\r\nLine 2";
    let unicode_payload = ClipboardPayload::new_unicode(unicode_source, Some((10, 50)));
    assert_eq!(unicode_payload.flavor, ClipboardFlavor::PlainTextUtf8);
    assert_eq!(unicode_payload.flavor.identifier(), "public.utf8-plain-text");
    assert!(unicode_payload.has_bom);
    assert!(unicode_payload.has_crlf);
    assert!(unicode_payload.has_bidi);
    assert!(!unicode_payload.escaped_malformed);
    assert_eq!(unicode_payload.source_anchor, Some((10, 50)));
    assert_eq!(
        std::str::from_utf8(&unicode_payload.data).unwrap(),
        unicode_source,
        "declared decoded Unicode matches without silent line-ending normalization or bidi reordering"
    );

    // 2. Exact byte export: preserves raw bytes including invalid UTF-8 and BOMs under opaque flavor.
    let raw_malformed_bytes: &[u8] = &[0xEF, 0xBB, 0xBF, b'a', b'b', 0xFF, 0xFE, b'c', b'\r', b'\n'];
    let exact_payload = ClipboardPayload::new_exact_bytes(raw_malformed_bytes, Some((100, 110)));
    assert_eq!(exact_payload.flavor, ClipboardFlavor::ExactBytes);
    assert_eq!(exact_payload.flavor.identifier(), "com.franken.fcb.exact-bytes");
    assert!(exact_payload.has_bom);
    assert!(exact_payload.has_crlf);
    assert!(exact_payload.escaped_malformed);
    assert_eq!(exact_payload.source_anchor, Some((100, 110)));
    assert_eq!(&exact_payload.data[..], raw_malformed_bytes);

    record_receipt(
        "ordinary_unicode_copy_vs_exact_byte_export_contracts",
        Effect::Succeeded,
        "ordinary unicode copy vs exact byte export contracts hold",
    );
}

#[test]
fn clipboard_budget_refusal_cancellation_and_external_change() {
    use fcb_ui::{ClipboardError, ClipboardPayload, NativePasteboard};

    let mut pasteboard = NativePasteboard::new(64);

    // 1. Successful publication.
    let p1 = ClipboardPayload::new_unicode("initial content", None);
    let gen1 = pasteboard.publish(p1.clone(), 0).expect("first publication succeeds");
    assert_eq!(gen1, 1);
    assert_eq!(pasteboard.published_payloads().len(), 1);

    // 2. Oversized payload is refused; old clipboard is preserved untouched.
    let huge_text = "x".repeat(128);
    let huge_payload = ClipboardPayload::new_unicode(&huge_text, None);
    assert_eq!(
        pasteboard.publish(huge_payload, 1),
        Err(ClipboardError::BudgetRefusal {
            requested: 128,
            max: 64
        })
    );
    // Preserves old clipboard.
    assert_eq!(pasteboard.published_payloads(), &[p1.clone()]);
    assert_eq!(pasteboard.current_generation(), 1);

    // 3. Grant revocation refuses publication and preserves old clipboard.
    pasteboard.set_grant_valid(false);
    let p2 = ClipboardPayload::new_unicode("new text", None);
    assert_eq!(pasteboard.publish(p2, 1), Err(ClipboardError::GrantRevoked));
    assert_eq!(pasteboard.published_payloads(), &[p1.clone()]);
    pasteboard.set_grant_valid(true);

    // 4. Concurrent external change aborts publication to prevent overwriting newer external data.
    pasteboard.simulate_external_change(); // generation advances to 2
    let p3 = ClipboardPayload::new_unicode("stale client text", None);
    assert_eq!(
        pasteboard.publish(p3, 1), // expected generation 1, actual generation 2
        Err(ClipboardError::ConcurrentExternalChange {
            expected_generation: 1,
            actual_generation: 2
        })
    );

    // 5. Injected native publication failure is reported accurately without unsafe rollback.
    pasteboard.set_simulate_native_failure(true);
    let p4 = ClipboardPayload::new_unicode("retry text", None);
    assert_eq!(
        pasteboard.publish(p4, 2),
        Err(ClipboardError::NativePublicationFailure)
    );

    record_receipt(
        "clipboard_budget_refusal_cancellation_and_external_change",
        Effect::Succeeded,
        "clipboard budget refusal, cancellation, and external change detection hold",
    );
}

#[test]
fn editor_and_web_link_handoff_structured_os_arguments() {
    use fcb_ui::{
        ActionRefusalReason, StructuredOsCommand, validate_editor_handoff, validate_web_link_handoff,
    };

    // 1. Valid editor handoff produces structured argv, no shell string.
    let editor_cmd = validate_editor_handoff(
        "/usr/local/bin/cursor",
        "/repo/src/lib.rs",
        Some(42),
        Some(10),
        Some("/repo"),
    )
    .expect("valid editor handoff");
    assert_eq!(
        editor_cmd,
        StructuredOsCommand {
            program: "/usr/local/bin/cursor".to_string(),
            args: vec!["-g".to_string(), "/repo/src/lib.rs:42:10".to_string()],
        }
    );

    // 2. Traversal escaping root is refused.
    assert_eq!(
        validate_editor_handoff(
            "vim",
            "/repo/../etc/passwd",
            None,
            None,
            Some("/repo"),
        ),
        Err(ActionRefusalReason::PathTraversal)
    );

    // 3. Newline injection refused.
    assert_eq!(
        validate_editor_handoff(
            "vim\nrm -rf /",
            "/repo/src/lib.rs",
            None,
            None,
            Some("/repo"),
        ),
        Err(ActionRefusalReason::NewlineInjection)
    );

    // 4. Bidi spoofing refused.
    assert_eq!(
        validate_editor_handoff(
            "vim",
            "/repo/src/\u{202E}txt.sh",
            None,
            None,
            Some("/repo"),
        ),
        Err(ActionRefusalReason::BidiSpoofing)
    );

    // 5. Web link handoff: allowlisted scheme produces structured open command.
    let web_cmd = validate_web_link_handoff("https://docs.rs/fcb", &["https", "http"])
        .expect("valid web link");
    assert_eq!(
        web_cmd,
        StructuredOsCommand {
            program: "/usr/bin/open".to_string(),
            args: vec!["-u".to_string(), "https://docs.rs/fcb".to_string()],
        }
    );

    // 6. Dangerous/unsupported schemes refused.
    for dangerous in [
        "exec:///bin/sh",
        "x-man-page://ls",
        "javascript:alert(1)",
        "file://evil.host/etc/passwd",
    ] {
        assert!(matches!(
            validate_web_link_handoff(dangerous, &["https", "http"]),
            Err(ActionRefusalReason::DisallowedScheme(_))
        ));
    }

    record_receipt(
        "editor_and_web_link_handoff_structured_os_arguments",
        Effect::Succeeded,
        "structured OS argument generation and traversal/injection refusal hold",
    );
}


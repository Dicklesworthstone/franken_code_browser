#![forbid(unsafe_code)]

//! Focused production unit and boundary tests for FCB-053.B (fcb-8bqc.2):
//! - Raw paths and --root/open FILE behavior with spaces, CJK, and Unicode.
//! - Scope disclosure: accessing outside-root files never silently widens root.
//! - Symlink and path traversal confinement refusals.
//! - Focus return tracker restoring prior focus across modal/search dismissals.
//! - Strict host input non-interception: Cmd+Q, Cmd+H, Cmd+M, Cmd+Tab and unhandled keys pass through.
//! - Negative controls demonstrating oracle defect detection.
//! - Emits structured [`ScenarioReceipt`]s.

use std::fs;
use std::path::PathBuf;

use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;
use fcb_ui::{
    dispatch_key_input, evaluate_open_file, normalize_lexical_path, validate_editor_handoff,
    validate_symlink_confinement, ActionRefusalReason, FocusNode, FocusReturnTracker, GrantedRoot,
    InputDispatchResult, KeyAction, KeyModifiers, ScopeEvaluation, ScopeRefusal,
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
        seed: ScenarioSeed(0x0C_53_00_02),
        pin: SourcePin::new("0530005300053000530005300053000530005302").expect("pin valid"),
        route: RouteId::new("headless:ui:native-actions-scope").expect("route valid"),
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

#[test]
fn raw_paths_and_root_confinement_with_spaces_and_unicode() {
    let root = GrantedRoot::new("/Users/dev/My Projects/プロジェクト 🚀", "Primary Workspace")
        .expect("valid root with spaces, CJK, and emoji");
    let roots = vec![root];

    // 1. Exact root path opening.
    let eval_root = evaluate_open_file("/Users/dev/My Projects/プロジェクト 🚀", &roots)
        .expect("evaluation succeeds");
    assert_eq!(
        eval_root,
        ScopeEvaluation::WithinGrant {
            root: "/Users/dev/My Projects/プロジェクト 🚀".to_string(),
            relative_path: "".to_string(),
            full_path: "/Users/dev/My Projects/プロジェクト 🚀".to_string(),
        }
    );

    // 2. File within root with spaces and Unicode.
    let target = "/Users/dev/My Projects/プロジェクト 🚀/src/ドキュメント 📄.rs";
    let eval_file = evaluate_open_file(target, &roots).expect("evaluation succeeds");
    assert_eq!(
        eval_file,
        ScopeEvaluation::WithinGrant {
            root: "/Users/dev/My Projects/プロジェクト 🚀".to_string(),
            relative_path: "src/ドキュメント 📄.rs".to_string(),
            full_path: target.to_string(),
        }
    );

    // 3. Interior lexical dot components normalize within the granted root.
    let target_dots = "/Users/dev/My Projects/プロジェクト 🚀/./sub/../src/ドキュメント 📄.rs";
    let eval_dots = evaluate_open_file(target_dots, &roots).expect("evaluation succeeds");
    assert_eq!(
        eval_dots,
        ScopeEvaluation::WithinGrant {
            root: "/Users/dev/My Projects/プロジェクト 🚀".to_string(),
            relative_path: "src/ドキュメント 📄.rs".to_string(),
            full_path: target.to_string(),
        }
    );

    record_receipt(
        "raw_paths_and_root_confinement_with_spaces_and_unicode",
        Effect::Succeeded,
        "raw paths with spaces, CJK, and emoji confine within granted root",
    );
}

#[test]
fn outside_root_never_silently_widens_and_requires_disclosure() {
    let root = GrantedRoot::new("/Users/dev/repo_a", "Repo A").unwrap();
    let roots = vec![root];

    // Target outside granted root /Users/dev/repo_a:
    let outside_target = "/Users/dev/repo_b/secret/config.toml";
    let eval = evaluate_open_file(outside_target, &roots).expect("evaluation succeeds");

    assert!(
        matches!(eval, ScopeEvaluation::RequiresDisclosure(_)),
        "SECURITY DEFECT: Target outside granted root was silently admitted!"
    );

    if let ScopeEvaluation::RequiresDisclosure(disclosure) = eval {
        assert_eq!(disclosure.requested_path, outside_target);
        assert_eq!(disclosure.suggested_root, "/Users/dev/repo_b/secret");
        assert_eq!(disclosure.existing_roots, vec!["/Users/dev/repo_a".to_string()]);
        assert!(disclosure.explanation.contains("outside granted roots"));
    }

    record_receipt(
        "outside_root_never_silently_widens_and_requires_disclosure",
        Effect::Succeeded,
        "outside-root requests produce explicit ScopeDisclosure, never silently widening root",
    );
}

#[test]
fn root_escape_and_symlink_confinement_refusals() {
    // 1. Path traversal climbing above root.
    assert_eq!(
        normalize_lexical_path("/repo/../../etc/passwd"),
        Err(ScopeRefusal::TraversalEscape)
    );

    // 2. Relative paths rejected.
    assert_eq!(
        normalize_lexical_path("relative/path.rs"),
        Err(ScopeRefusal::NotAbsolute)
    );

    // 3. Control characters rejected.
    assert_eq!(
        normalize_lexical_path("/repo/file\0name.rs"),
        Err(ScopeRefusal::ControlCharacters)
    );

    // 4. Symlink confinement inside root.
    let valid_symlink = validate_symlink_confinement(
        "/repo/links/helper.rs",
        "../src/helper.rs",
        "/repo",
    ).expect("internal relative symlink within root");
    assert_eq!(valid_symlink, "/repo/src/helper.rs");

    // 5. Symlink escaping root via relative traversal.
    let escaping_rel = validate_symlink_confinement(
        "/repo/links/escape.rs",
        "../../outside/secret.rs",
        "/repo",
    );
    assert!(matches!(escaping_rel, Err(ScopeRefusal::SymlinkEscape { .. })));

    // 6. Symlink escaping root via absolute path.
    let escaping_abs = validate_symlink_confinement(
        "/repo/links/etc_passwd",
        "/etc/passwd",
        "/repo",
    );
    assert!(matches!(escaping_abs, Err(ScopeRefusal::SymlinkEscape { .. })));

    record_receipt(
        "root_escape_and_symlink_confinement_refusals",
        Effect::Succeeded,
        "root traversal escapes, relative paths, control chars, and escaping symlinks strictly refused",
    );
}

#[test]
fn focus_return_tracker_preserves_and_restores_prior_focus() {
    let mut tracker = FocusReturnTracker::new(FocusNode::Atlas);
    assert_eq!(tracker.current(), FocusNode::Atlas);
    assert_eq!(tracker.history_depth(), 0);

    // 1. User triggers search (focus moves to SearchField).
    tracker.focus(FocusNode::SearchField);
    assert_eq!(tracker.current(), FocusNode::SearchField);
    assert_eq!(tracker.history_depth(), 1);

    // 2. From search, user triggers a dialog (focus moves to Dialog).
    tracker.focus(FocusNode::Dialog);
    assert_eq!(tracker.current(), FocusNode::Dialog);
    assert_eq!(tracker.history_depth(), 2);

    // 3. Dialog is dismissed/canceled -> focus returns to SearchField.
    let restored1 = tracker.return_focus();
    assert_eq!(restored1, FocusNode::SearchField);
    assert_eq!(tracker.current(), FocusNode::SearchField);
    assert_eq!(tracker.history_depth(), 1);

    // 4. Search is dismissed -> focus returns to Atlas.
    let restored2 = tracker.return_focus();
    assert_eq!(restored2, FocusNode::Atlas);
    assert_eq!(tracker.current(), FocusNode::Atlas);
    assert_eq!(tracker.history_depth(), 0);

    // 5. Excessive return_focus safely retains fallback, never null or panics.
    let fallback = tracker.return_focus();
    assert_eq!(fallback, FocusNode::Atlas);
    assert_eq!(tracker.current(), FocusNode::Atlas);

    record_receipt(
        "focus_return_tracker_preserves_and_restores_prior_focus",
        Effect::Succeeded,
        "focus stack unwinds cleanly across dialogs and search dismissals with safe fallback",
    );
}

#[test]
fn host_input_transparency_and_non_interception() {
    // Test that macOS system shortcuts are NEVER intercepted by any node.
    let system_shortcuts = [
        ('q', "Application quit (Cmd+Q) passed to host"),
        ('h', "Application hide (Cmd+H) passed to host"),
        ('m', "Window minimize (Cmd+M) passed to host"),
    ];

    for node in [
        FocusNode::Atlas,
        FocusNode::SourceViewer,
        FocusNode::SearchField,
        FocusNode::Dialog,
    ] {
        for (char_key, expected_reason) in system_shortcuts {
            let res = dispatch_key_input(
                node,
                &KeyAction::Character(char_key),
                KeyModifiers::cmd(),
            );
            assert_eq!(
                res,
                InputDispatchResult::PassedToHost {
                    reason: expected_reason.to_string(),
                },
                "Node {node:?} must pass Cmd+{char_key} to host"
            );
        }

        // Cmd+Tab must pass to host:
        let tab_res = dispatch_key_input(node, &KeyAction::Tab, KeyModifiers::cmd());
        assert_eq!(
            tab_res,
            InputDispatchResult::PassedToHost {
                reason: "Application switcher (Cmd+Tab) passed to host".to_string(),
            }
        );

        // Unhandled function key (e.g. F12) must pass to host:
        let f12_res = dispatch_key_input(node, &KeyAction::FKey(12), KeyModifiers::NONE);
        assert!(matches!(f12_res, InputDispatchResult::PassedToHost { .. }));
    }

    // In-app actions must be handled properly:
    // Copy in SourceViewer:
    let copy_res = dispatch_key_input(
        FocusNode::SourceViewer,
        &KeyAction::Character('c'),
        KeyModifiers::cmd(),
    );
    assert_eq!(
        copy_res,
        InputDispatchResult::Handled {
            target: FocusNode::SourceViewer,
            action: "Copy".to_string(),
        }
    );

    // Type in SearchField:
    let type_res = dispatch_key_input(
        FocusNode::SearchField,
        &KeyAction::Character('x'),
        KeyModifiers::NONE,
    );
    assert_eq!(
        type_res,
        InputDispatchResult::Handled {
            target: FocusNode::SearchField,
            action: "Type 'x'".to_string(),
        }
    );

    // Arrow navigation in Atlas:
    let pan_res = dispatch_key_input(
        FocusNode::Atlas,
        &KeyAction::ArrowRight,
        KeyModifiers::NONE,
    );
    assert_eq!(
        pan_res,
        InputDispatchResult::Handled {
            target: FocusNode::Atlas,
            action: "Pan Atlas".to_string(),
        }
    );

    record_receipt(
        "host_input_transparency_and_non_interception",
        Effect::Succeeded,
        "system shortcuts (Cmd+Q/H/M/Tab) and unhandled keys pass to host without interception",
    );
}

#[test]
fn negative_controls_detect_defect_conditions() {
    // Negative Control 1: An oracle asserting that outside-root file access is admitted
    // without disclosure MUST fail.
    let root = GrantedRoot::new("/repo", "Repo").unwrap();
    let roots = vec![root];
    let outside = "/var/log/system.log";
    let eval = evaluate_open_file(outside, &roots).expect("eval succeeds");

    let defect_admitted_without_disclosure = matches!(eval, ScopeEvaluation::WithinGrant { .. });
    assert!(
        !defect_admitted_without_disclosure,
        "Defect detected: outside path must not be admitted without disclosure!"
    );

    // Negative Control 2: An oracle asserting that Cmd+Q is handled by FCB must fail.
    let quit_dispatch = dispatch_key_input(
        FocusNode::SearchField,
        &KeyAction::Character('q'),
        KeyModifiers::cmd(),
    );
    let defect_swallowed_quit = matches!(quit_dispatch, InputDispatchResult::Handled { .. });
    assert!(
        !defect_swallowed_quit,
        "Defect detected: application quit must not be intercepted/handled by FCB!"
    );

    // Negative Control 3: An oracle asserting that escaping symlinks are admitted must fail.
    let symlink_check = validate_symlink_confinement("/repo/bad_link", "../../etc/shadow", "/repo");
    let defect_admitted_escaping_symlink = symlink_check.is_ok();
    assert!(
        !defect_admitted_escaping_symlink,
        "Defect detected: escaping symlink must not be permitted!"
    );

    // Negative Control 4: An oracle asserting that shell command injection in editor handoff is permitted must fail.
    let editor_injection = validate_editor_handoff(
        "code; rm -rf /",
        "/repo/src/lib.rs",
        None,
        None,
        Some("/repo"),
    );
    assert!(
        matches!(editor_injection, Err(ActionRefusalReason::UntrustedEditor(_))),
        "Defect detected: shell injection in editor handoff must be refused as UntrustedEditor!"
    );

    record_receipt(
        "negative_controls_detect_defect_conditions",
        Effect::Succeeded,
        "oracle reliably flags silent root widening, host shortcut interception, symlink escapes, and injection",
    );
}

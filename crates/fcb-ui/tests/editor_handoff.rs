#![forbid(unsafe_code)]

//! Structured editor arguments and lexical authority validation. These tests
//! execute the public validator; they do not claim a native application launch.
//! Execution belongs to the independent strict-RCH verification lane.

use fcb_ui::{ActionRefusalReason, StructuredOsCommand, validate_editor_handoff};
use fcb_ui::editor_link::{MAX_EDITOR_ARGUMENT_BYTES, MAX_EDITOR_INPUT_BYTES};

fn command(path: &str, root: Option<&str>) -> Result<StructuredOsCommand, ActionRefusalReason> {
    validate_editor_handoff("vim", path, None, None, root)
}

#[test]
fn root_authority_is_an_exact_component_prefix_including_filesystem_root() {
    for root in ["/repo", "/repo/"] {
        for path in ["/repo", "/repo/file.rs", "/repo/nested/source.rs"] {
            assert_eq!(command(path, Some(root)).unwrap().args, [path]);
        }
        for path in ["/repository/file.rs", "/repo-old/file.rs", "/repo.rs", "/repo\\sibling/file.rs", "/elsewhere/file.rs"] {
            assert_eq!(command(path, Some(root)), Err(ActionRefusalReason::PathEscapesRoot {
                path: path.into(), root: root.into(),
            }), "sibling prefixes must not acquire root authority");
        }
    }
    assert_eq!(command("/anywhere/file.rs", Some("/")).unwrap().args, ["/anywhere/file.rs"]);
    assert_eq!(command("/anywhere/file.rs", None).unwrap().args, ["/anywhere/file.rs"]);
    assert_eq!(command("/repo/file.rs", Some("")), Err(ActionRefusalReason::Empty));
    for root in ["repo", "//", "///", "/repo//", "/repo//nested"] {
        assert_eq!(command("/repo/file.rs", Some(root)), Err(ActionRefusalReason::InvalidPath));
    }
}

#[test]
fn native_path_identity_is_not_case_folded_or_unicode_normalized() {
    for (root, other) in [("/Repo", "/repo"), ("/r\u{e9}po", "/re\u{301}po")] {
        let exact = format!("{root}/source.rs");
        assert!(command(&exact, Some(root)).is_ok());
        let different = format!("{other}/source.rs");
        assert!(matches!(command(&different, Some(root)), Err(ActionRefusalReason::PathEscapesRoot { .. })));
    }
}

#[test]
fn decoded_native_paths_refuse_traversal_uris_and_ambient_relative_targets() {
    for path in ["/repo/../outside.rs", "/repo/..", "/repo/./file.rs"] {
        assert_eq!(command(path, Some("/repo")), Err(ActionRefusalReason::PathTraversal));
    }
    for root in ["/repo/..", "/repo/./nested", "/repo/../repo"] {
        assert_eq!(command("/repo/file.rs", Some(root)), Err(ActionRefusalReason::PathTraversal));
    }
    for path in ["relative.rs", "-option.rs", "file:///repo/file.rs", "file://remote/repo/file.rs",
        "https://example.test/file.rs", "/", "//remote/repo/file.rs", "/repo//file.rs", "/repo/file.rs/"] {
        assert_eq!(command(path, None), Err(ActionRefusalReason::InvalidPath));
    }
    // No URI decoding occurs at this boundary. These are literal native names,
    // so encoded-looking bytes do not acquire a second path interpretation.
    let literal = "/repo/%2e%2e/%2F/file%00.rs";
    assert_eq!(command(literal, Some("/repo")).unwrap().args, [literal]);
}

#[test]
fn controls_and_directional_spoofing_are_refused_in_each_editor_input() {
    for ch in ['\0', '\t', '\u{1b}', '\u{7f}', '\u{85}', '\u{9f}', '\n', '\r', '\u{2028}', '\u{2029}',
        '\u{061c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}',
        '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{206f}'] {
        let expected = if matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            ActionRefusalReason::NewlineInjection
        } else if ch.is_control() { ActionRefusalReason::ControlCharacter }
        else { ActionRefusalReason::BidiSpoofing };
        let program = format!("vi{ch}m");
        assert_eq!(validate_editor_handoff(&program, "/repo/file.rs", None, None, Some("/repo")), Err(expected.clone()));
        let path = format!("/repo/a{ch}.rs");
        assert_eq!(command(&path, Some("/repo")), Err(expected.clone()));
        let root = format!("/repo{ch}");
        assert_eq!(command("/repo/file.rs", Some(&root)), Err(expected));
    }
}

#[test]
fn input_and_argument_byte_limits_bound_successes_and_refusal_diagnostics() {
    let oversized = "x".repeat(MAX_EDITOR_INPUT_BYTES + 1);
    for (program, path, root) in [
        (oversized.as_str(), "/repo/a.rs", Some("/repo")),
        ("vim", oversized.as_str(), Some("/repo")),
        ("vim", "/repo/a.rs", Some(oversized.as_str())),
    ] {
        let error = validate_editor_handoff(program, path, None, None, root).unwrap_err();
        assert_eq!(error, ActionRefusalReason::InputLimit);
        assert!(error.to_string().len() < 128, "oversize inputs must not be copied into error payloads");
    }
    let path = format!("/{}", "x".repeat(MAX_EDITOR_INPUT_BYTES - 1));
    let positioned = validate_editor_handoff("code", &path, Some(u32::MAX), Some(u32::MAX), Some("/")).unwrap();
    assert_eq!(positioned.args[1], format!("{path}:4294967295:4294967295"));
    assert_eq!(positioned.args[1].len(), MAX_EDITOR_ARGUMENT_BYTES);
    assert!(command(&path, Some(&path)).is_ok(), "exact-limit root authority remains admissible");
    let program = "e".repeat(MAX_EDITOR_INPUT_BYTES);
    assert_eq!(validate_editor_handoff(&program, "/repo/a.rs", None, None, Some("/repo")).unwrap().program, program);
    let unicode = format!("/{}", "\u{e9}".repeat(MAX_EDITOR_INPUT_BYTES / 2));
    assert!(unicode.chars().count() < MAX_EDITOR_INPUT_BYTES);
    assert_eq!(command(&unicode, None), Err(ActionRefusalReason::InputLimit), "limits count bytes, not scalars");
}

#[test]
fn literal_filename_metacharacters_and_legacy_position_arguments_remain_structured() {
    let path = r#"/repo/a;$(ignored)&`literal`|<tag>! (copy)\ "quoted".rs"#;
    let plain = validate_editor_handoff("vim", path, None, None, Some("/repo")).unwrap();
    assert_eq!(plain.program, "vim");
    assert_eq!(plain.args, [path]);
    let line = validate_editor_handoff("vim", path, Some(42), None, Some("/repo")).unwrap();
    assert_eq!(line.args, ["+42", path]);
    let position = validate_editor_handoff("/Applications/Editor Tools/editor", path, Some(42), Some(10), Some("/repo")).unwrap();
    assert_eq!(position.program, "/Applications/Editor Tools/editor");
    assert_eq!(position.args, ["-g".to_string(), format!("{path}:42:10")]);
    assert!(matches!(validate_editor_handoff("code; command", path, None, None, Some("/repo")),
        Err(ActionRefusalReason::UntrustedEditor(_))), "source arguments do not become an editor command string");
}

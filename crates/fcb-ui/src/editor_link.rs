//! Explicit external editor and web-link handoff validation (FCB-053.A, fcb-8bqc.1).
//!
//! Enforces plan §22.6:
//! - Opening an editor or external link requires an explicit user action and a configured trusted target.
//! - Arguments are passed structurally (`StructuredOsCommand`), NEVER interpolated into a shell string.
//! - Scheme allowlist (`https`, `http` for links; file paths for editor).
//! - Reject nonlocal file authorities, encoded traversal, newline injection, and bidi label spoofing.
//! - Confinement within granted root: `--root` never silently widens.

use std::fmt;

/// Per-field byte bound for configured editor, decoded file path and optional
/// root authority. Refuse before copying any input into arguments or errors.
pub const MAX_EDITOR_INPUT_BYTES: usize = 16_384;
/// A file argument can append two colons and two decimal `u32` coordinates.
pub const MAX_EDITOR_ARGUMENT_BYTES: usize = MAX_EDITOR_INPUT_BYTES + 22;

/// Refusal reasons for untrusted or malformed external actions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionRefusalReason {
    Empty,
    InputLimit,
    InvalidPath,
    ControlCharacter,
    DisallowedScheme(String),
    NonlocalAuthority(String),
    PathTraversal,
    NewlineInjection,
    BidiSpoofing,
    PathEscapesRoot { path: String, root: String },
    UntrustedEditor(String),
}

impl fmt::Display for ActionRefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("action payload is empty"),
            Self::InputLimit => f.write_str("editor action input exceeds its byte limit"),
            Self::InvalidPath => f.write_str("editor action requires an unambiguous absolute native path"),
            Self::ControlCharacter => f.write_str("control character in editor action input"),
            Self::DisallowedScheme(s) => write!(f, "scheme '{s}' is not in the allowlist"),
            Self::NonlocalAuthority(a) => write!(f, "nonlocal file authority '{a}' rejected"),
            Self::PathTraversal => f.write_str("path traversal attempt detected"),
            Self::NewlineInjection => f.write_str("newline injection attempt detected"),
            Self::BidiSpoofing => f.write_str("bidirectional text spoofing control character detected"),
            Self::PathEscapesRoot { path, root } => {
                write!(f, "path '{path}' escapes granted root '{root}'")
            }
            Self::UntrustedEditor(e) => write!(f, "editor binary '{e}' is not trusted"),
        }
    }
}

impl std::error::Error for ActionRefusalReason {}

/// Structured arguments passed directly to OS `exec` boundary without shell interpolation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuredOsCommand {
    pub program: String,
    pub args: Vec<String>,
}

fn contains_bidi_controls(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(
            c,
            '\u{200E}' | '\u{200F}' | '\u{202A}' | '\u{202B}' | '\u{202C}' | '\u{202D}' | '\u{202E}'
        )
    })
}

fn contains_newlines(s: &str) -> bool {
    s.contains('\n') || s.contains('\r')
}

fn contains_shell_metachars(s: &str) -> bool {
    s.chars().any(|c| matches!(c, ';' | '&' | '|' | '`' | '$' | '<' | '>' | '(' | ')' | '!' | '\\'))
}

fn validate_editor_text(text: &str) -> Result<(), ActionRefusalReason> {
    if text.len() > MAX_EDITOR_INPUT_BYTES {
        return Err(ActionRefusalReason::InputLimit);
    }
    if text.trim().is_empty() {
        return Err(ActionRefusalReason::Empty);
    }
    if contains_newlines(text) || text.contains(['\u{2028}', '\u{2029}']) {
        return Err(ActionRefusalReason::NewlineInjection);
    }
    if text.chars().any(|ch| ch.is_control()) {
        return Err(ActionRefusalReason::ControlCharacter);
    }
    if contains_bidi_controls(text) || text.chars().any(|ch|
        matches!(ch, '\u{061c}' | '\u{2066}'..='\u{206f}')) {
        return Err(ActionRefusalReason::BidiSpoofing);
    }
    Ok(())
}

fn editor_absolute_path(path: &str, is_root: bool) -> Result<&str, ActionRefusalReason> {
    // These are already-decoded Unix paths, never URLs or shell words. Do not
    // decode percent sequences, fold case/Unicode, or consult the filesystem.
    if !path.starts_with('/') || path.starts_with("//") {
        return Err(ActionRefusalReason::InvalidPath);
    }
    if path == "/" {
        return if is_root { Ok(path) } else { Err(ActionRefusalReason::InvalidPath) };
    }
    let path = if is_root { path.strip_suffix('/').unwrap_or(path) } else { path };
    for component in path[1..].split('/') {
        if component == "." || component == ".." {
            return Err(ActionRefusalReason::PathTraversal);
        }
        if component.is_empty() {
            return Err(ActionRefusalReason::InvalidPath);
        }
    }
    Ok(path)
}

/// Validate and build a structured command for opening an external editor.
///
/// The caller supplies an explicitly trusted editor and already-decoded absolute
/// native source/root paths, each at most `MAX_EDITOR_INPUT_BYTES`. URI strings
/// are not accepted; literal percent sequences and filename shell metacharacters
/// remain literal. Program names and the existing `-g`/`+line` conventions remain
/// supported; this helper neither resolves editor-specific syntax nor launches it.
///
/// Root checking is lexical component ancestry with exact bytes and optional
/// trailing root slash. It performs no I/O and cannot establish native grants or
/// confinement against symlink/ancestor replacement. The host retains and
/// revalidates its actual source authority before the explicit OS handoff.
pub fn validate_editor_handoff(
    configured_editor: &str,
    file_path: &str,
    line: Option<u32>,
    column: Option<u32>,
    root_confinement: Option<&str>,
) -> Result<StructuredOsCommand, ActionRefusalReason> {
    // Check all lengths before diagnostic copies, including the root supplied
    // as authority. One oversized field must not produce an oversized error.
    if [Some(configured_editor), Some(file_path), root_confinement].into_iter()
        .flatten().any(|text| text.len() > MAX_EDITOR_INPUT_BYTES) {
        return Err(ActionRefusalReason::InputLimit);
    }
    validate_editor_text(configured_editor)?;
    validate_editor_text(file_path)?;
    if contains_shell_metachars(configured_editor) {
        return Err(ActionRefusalReason::UntrustedEditor(configured_editor.to_string()));
    }
    let file_path = editor_absolute_path(file_path, false)?;

    // A component prefix is authority; a string prefix is not. In particular,
    // /repo grants neither /repository nor /repo-old, and an empty root is not /.
    if let Some(root) = root_confinement {
        validate_editor_text(root)?;
        let clean_root = editor_absolute_path(root, true)?;
        let within_root = clean_root == "/" || file_path == clean_root
            || file_path.strip_prefix(clean_root).is_some_and(|rest| rest.starts_with('/'));
        if !within_root {
            return Err(ActionRefusalReason::PathEscapesRoot {
                path: file_path.to_string(),
                root: root.to_string(),
            });
        }
    }

    let mut args = Vec::with_capacity(2);
    if let (Some(l), Some(c)) = (line, column) {
        args.push("-g".to_string());
        args.push(format!("{file_path}:{l}:{c}"));
    } else if let Some(l) = line {
        args.push(format!("+{l}"));
        args.push(file_path.to_string());
    } else {
        args.push(file_path.to_string());
    }

    Ok(StructuredOsCommand {
        program: configured_editor.to_string(),
        args,
    })
}

/// Validate and build a structured command for opening an external web link.
pub fn validate_web_link_handoff(
    url: &str,
    allowed_schemes: &[&str],
) -> Result<StructuredOsCommand, ActionRefusalReason> {
    if url.trim().is_empty() {
        return Err(ActionRefusalReason::Empty);
    }
    if contains_newlines(url) {
        return Err(ActionRefusalReason::NewlineInjection);
    }
    if contains_bidi_controls(url) {
        return Err(ActionRefusalReason::BidiSpoofing);
    }

    // Extract scheme.
    let Some(scheme_end) = url.find("://") else {
        return Err(ActionRefusalReason::DisallowedScheme(url.to_string()));
    };
    let scheme = url
        .get(..scheme_end)
        .ok_or_else(|| ActionRefusalReason::DisallowedScheme(url.to_string()))?;
    if !allowed_schemes.iter().any(|&s| s.eq_ignore_ascii_case(scheme)) {
        return Err(ActionRefusalReason::DisallowedScheme(scheme.to_string()));
    }

    // If file scheme, reject nonlocal authority.
    if scheme.eq_ignore_ascii_case("file") {
        if let Some(remainder) = url.get(scheme_end + 3..) {
            if let Some(slash_idx) = remainder.find('/') {
                if let Some(authority) = remainder.get(..slash_idx) {
                    if !authority.is_empty() && authority != "localhost" {
                        return Err(ActionRefusalReason::NonlocalAuthority(authority.to_string()));
                    }
                }
            }
        }
    }

    // Arguments passed structurally without shell interpolation.
    Ok(StructuredOsCommand {
        program: "/usr/bin/open".to_string(),
        args: vec!["-u".to_string(), url.to_string()],
    })
}

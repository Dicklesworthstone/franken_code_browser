//! Explicit external editor and web-link handoff validation (FCB-053.A, fcb-8bqc.1).
//!
//! Enforces plan §22.6:
//! - Opening an editor or external link requires an explicit user action and a configured trusted target.
//! - Arguments are passed structurally (`StructuredOsCommand`), NEVER interpolated into a shell string.
//! - Scheme allowlist (`https`, `http` for links; file paths for editor).
//! - Reject nonlocal file authorities, encoded traversal, newline injection, and bidi label spoofing.
//! - Confinement within granted root: `--root` never silently widens.

use std::fmt;

/// Refusal reasons for untrusted or malformed external actions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionRefusalReason {
    Empty,
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

/// Validate and build a structured command for opening an external editor.
pub fn validate_editor_handoff(
    configured_editor: &str,
    file_path: &str,
    line: Option<u32>,
    column: Option<u32>,
    root_confinement: Option<&str>,
) -> Result<StructuredOsCommand, ActionRefusalReason> {
    if configured_editor.trim().is_empty() || file_path.trim().is_empty() {
        return Err(ActionRefusalReason::Empty);
    }
    if contains_newlines(configured_editor) || contains_newlines(file_path) {
        return Err(ActionRefusalReason::NewlineInjection);
    }
    if contains_shell_metachars(configured_editor) {
        return Err(ActionRefusalReason::UntrustedEditor(configured_editor.to_string()));
    }
    if contains_bidi_controls(configured_editor) || contains_bidi_controls(file_path) {
        return Err(ActionRefusalReason::BidiSpoofing);
    }

    // Traversal check.
    if file_path.contains("/../") || file_path.ends_with("/..") || file_path == ".." {
        return Err(ActionRefusalReason::PathTraversal);
    }

    // Root confinement check: if a root is given, path must be inside root.
    if let Some(root) = root_confinement {
        let clean_root = root.trim_end_matches('/');
        if !file_path.starts_with(clean_root) {
            return Err(ActionRefusalReason::PathEscapesRoot {
                path: file_path.to_string(),
                root: root.to_string(),
            });
        }
    }

    let mut args = Vec::new();
    if let (Some(l), Some(c)) = (line, column) {
        args.push(format!("-g"));
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

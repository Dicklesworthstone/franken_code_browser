//! Open-panel and drag-and-drop request validation (fcb-8bqc.1).
//!
//! Every file/folder the app opens arrives as an open-panel outcome or a
//! pasteboard URL. Both are validated here before any engine sees the path:
//! only absolute local `file` URLs and absolute paths are acceptable,
//! traversal and control characters are refused, and a canceled dialog is an
//! explicit terminal state. Nothing here ever executes a path: validation is
//! pure string geometry.

/// A request to open something, from a dialog or a drop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenRequest {
    /// An `NSURL` string from NSOpenPanel or the pasteboard.
    Url(String),
    /// A raw filesystem path (drag-and-drop pathname variant).
    Path(String),
}

/// Why an open request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalReason {
    /// The dialog was dismissed; no request exists.
    Canceled,
    /// Empty request payload.
    Empty,
    /// Only local `file:` URLs are acceptable.
    UnsupportedScheme,
    /// Relative paths have no meaning without a caller-chosen root.
    NotAbsolute,
    /// The path escapes upward (`..`) after normalization.
    Traversal,
    /// NUL or other control characters in the payload.
    ControlCharacters,
}

/// The terminal decision for one open request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenDecision {
    /// Accept the local path (already normalized, still absolute).
    Accept(String),
    /// Refuse with the reason; the caller surfaces this, never shells out.
    Refuse(RefusalReason),
}

/// Validates one open request. Spaces, CJK, emoji, and combining marks in
/// filenames are ordinary content and accepted.
pub fn validate_open_request(request: OpenRequest) -> OpenDecision {
    match request {
        OpenRequest::Url(url) => validate_url(&url),
        OpenRequest::Path(path) => validate_path(&path),
    }
}

/// The explicit outcome of a canceled dialog, kept as its own constructor so
/// callers cannot confuse it with an empty accept.
pub fn canceled_dialog() -> OpenDecision {
    OpenDecision::Refuse(RefusalReason::Canceled)
}

fn validate_url(url: &str) -> OpenDecision {
    if url.is_empty() {
        return OpenDecision::Refuse(RefusalReason::Empty);
    }
    // Percent-decode first so encoded traversal cannot smuggle through
    // (`file:///safe/%2e%2e/%2e%2e/etc`). Decoding happens before scheme
    // checks are finalized: a decoded value that changes the scheme is
    // itself hostile.
    let Some(decoded) = percent_decode(url) else {
        return OpenDecision::Refuse(RefusalReason::ControlCharacters);
    };
    let Some(rest) = decoded.strip_prefix("file://") else {
        return OpenDecision::Refuse(RefusalReason::UnsupportedScheme);
    };
    // `file://localhost/...` is the canonical host form; any other
    // authority is refused (remote mounts arrive through explicit roots).
    let path = match rest.strip_prefix("localhost/") {
        Some(p) => format!("/{p}"),
        None if rest.starts_with('/') => rest.to_string(),
        None => return OpenDecision::Refuse(RefusalReason::UnsupportedScheme),
    };
    validate_path(&path)
}

fn validate_path(path: &str) -> OpenDecision {
    if path.is_empty() {
        return OpenDecision::Refuse(RefusalReason::Empty);
    }
    if path.chars().any(|ch| ch.is_control()) {
        return OpenDecision::Refuse(RefusalReason::ControlCharacters);
    }
    if !path.starts_with('/') {
        return OpenDecision::Refuse(RefusalReason::NotAbsolute);
    }
    // Lexical normalization: collapse `.` and resolve `..` against the
    // components already seen. Anything that would climb above the root is
    // refused rather than clamped.
    let mut stack: Vec<&str> = Vec::new();
    for component in path.split('/').filter(|c| !c.is_empty() && *c != ".") {
        if component == ".." {
            if stack.pop().is_none() {
                return OpenDecision::Refuse(RefusalReason::Traversal);
            }
        } else {
            stack.push(component);
        }
    }
    let mut normalized = String::with_capacity(path.len());
    for component in &stack {
        normalized.push('/');
        normalized.push_str(component);
    }
    if normalized.is_empty() {
        normalized.push('/');
    }
    OpenDecision::Accept(normalized)
}

/// Percent-decodes a URL string; `None` when a `%` escape is malformed or
/// would decode to a control character.
fn percent_decode(url: &str) -> Option<String> {
    let bytes = url.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0_usize;
    while idx < bytes.len() {
        match bytes.get(idx).copied()? {
            b'%' => {
                let hex = bytes.get(idx + 1..idx + 3)?;
                let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
                if value < 0x20 || value == 0x7f {
                    return None;
                }
                out.push(value);
                idx += 3;
            }
            byte => {
                out.push(byte);
                idx += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

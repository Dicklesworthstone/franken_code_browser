#![forbid(unsafe_code)]

//! Raw path identities, normalized relative paths, escaped presentation,
//! and URI decoding (FCB-009.A).
//!
//! # Invariants
//!
//! 1. **Raw path identity is byte-authoritative**: paths preserve original raw
//!    bytes without lossy Unicode conversions. Two paths differing in case
//!    (e.g., `test.rs` and `Test.rs`) or raw byte representations have distinct
//!    identities.
//! 2. **Root boundary confinement**: a [`NormalizedPath`] is always relative to
//!    an authorized root. It strictly rejects `..` upward traversal, absolute
//!    prefixes, empty segments (`//`), and null bytes (`\0`).
//! 3. **Escape presentation for untrusted filenames**: filenames containing
//!    newlines, control characters, ANSI escapes, or Unicode bidirectional (bidi)
//!    formatting controls (`\u{202A}`–`\u{202E}`, `\u{2066}`–`\u{2069}`, etc.)
//!    are escaped in display strings to prevent line forgery or layout tampering.
//! 4. **No escalation via encoded paths**: URI decoding resolves percent-encoded
//!    bytes once before confinement checks. Encoded `..` (e.g. `%2e%2e`) and
//!    remote `file:` authorities are refused.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::SourceError;

/// Byte-authoritative representation of a filesystem path.
///
/// Unlike `String`, `RawPath` does not assume valid UTF-8 and does not
/// perform lossy conversions. Equality and hashing are exact over raw bytes.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RawPath {
    bytes: Arc<[u8]>,
}

impl RawPath {
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        Self::from_bytes(s.as_bytes())
    }

    #[cfg(unix)]
    pub fn from_path(path: &Path) -> Self {
        use std::os::unix::ffi::OsStrExt;
        Self::from_bytes(path.as_os_str().as_bytes())
    }

    #[cfg(not(unix))]
    pub fn from_path(path: &Path) -> Self {
        Self::from_str(&path.to_string_lossy())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Convert to a native `PathBuf` without loss on Unix.
    #[cfg(unix)]
    pub fn to_path_buf(&self) -> PathBuf {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(OsStr::from_bytes(&self.bytes))
    }

    #[cfg(not(unix))]
    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(String::from_utf8_lossy(&self.bytes).as_ref())
    }

    /// Returns a display adapter that safely escapes control characters,
    /// bidi direction overrides, and non-UTF8 bytes.
    pub fn display_escaped(&self) -> EscapedPathDisplay<'_> {
        EscapedPathDisplay {
            bytes: &self.bytes,
        }
    }
}

impl fmt::Debug for RawPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RawPath({:?})", self.display_escaped())
    }
}

impl fmt::Display for RawPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_escaped())
    }
}

impl From<&[u8]> for RawPath {
    fn from(bytes: &[u8]) -> Self {
        Self::from_bytes(bytes)
    }
}

impl From<&str> for RawPath {
    fn from(s: &str) -> Self {
        Self::from_str(s)
    }
}

impl std::str::FromStr for RawPath {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_bytes(s.as_bytes()))
    }
}

impl From<&Path> for RawPath {
    fn from(p: &Path) -> Self {
        Self::from_path(p)
    }
}

impl From<&PathBuf> for RawPath {
    fn from(p: &PathBuf) -> Self {
        Self::from_path(p.as_path())
    }
}

impl From<PathBuf> for RawPath {
    fn from(p: PathBuf) -> Self {
        Self::from_path(&p)
    }
}

/// A validated, normalized relative path strictly confined within a root.
///
/// Guarantees:
/// - Relative path: does not begin with `/` or Windows drive letter.
/// - Does not contain empty segments (`//`), `.` segments, or `..` upward traversals.
/// - Contains no null bytes (`\0`).
/// - Uses `/` as canonical segment separator.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NormalizedPath {
    raw: RawPath,
    segments: Vec<RawPath>,
}

impl NormalizedPath {
    /// Validates and normalizes raw path bytes into a root-relative path.
    pub fn new(raw: impl Into<RawPath>) -> Result<Self, SourceError> {
        let raw = raw.into();
        let bytes = raw.as_bytes();

        if bytes.is_empty() {
            return Err(SourceError::PathEscape);
        }

        // Refuse null bytes
        if bytes.contains(&0) {
            return Err(SourceError::EncodingError);
        }

        // Refuse absolute paths (starts with / or \)
        if bytes.starts_with(b"/") || bytes.starts_with(b"\\") {
            return Err(SourceError::PathEscape);
        }

        // Refuse Windows drive prefixes like `C:`
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return Err(SourceError::PathEscape);
        }

        let mut segments = Vec::new();
        let mut canonical_bytes = Vec::with_capacity(bytes.len());

        for chunk in bytes.split(|b| *b == b'/' || *b == b'\\') {
            if chunk.is_empty() {
                // Empty segments from leading/trailing or repeated separators (e.g. `foo//bar`)
                // are rejected to avoid ambiguity or canonicalization bypasses.
                return Err(SourceError::PathEscape);
            }
            if chunk == b"." {
                // Current directory markers in paths are rejected to enforce clean paths.
                return Err(SourceError::PathEscape);
            }
            if chunk == b".." {
                // Upward traversal is strictly forbidden in root-confined paths.
                return Err(SourceError::PathEscape);
            }

            if !canonical_bytes.is_empty() {
                canonical_bytes.push(b'/');
            }
            canonical_bytes.extend_from_slice(chunk);
            segments.push(RawPath::from_bytes(chunk));
        }

        if segments.is_empty() {
            return Err(SourceError::PathEscape);
        }

        Ok(Self {
            raw: RawPath::from_bytes(canonical_bytes),
            segments,
        })
    }

    pub fn raw(&self) -> &RawPath {
        &self.raw
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.raw.as_bytes()
    }

    pub fn segments(&self) -> &[RawPath] {
        &self.segments
    }

    /// Append one directory-entry name, rejecting separators and traversal names.
    pub fn join_segment(&self, segment: &[u8]) -> Result<Self, SourceError> {
        validate_dirent_name(segment)?;
        let mut bytes = Vec::with_capacity(self.raw.len() + 1 + segment.len());
        bytes.extend_from_slice(self.raw.as_bytes());
        bytes.push(b'/');
        bytes.extend_from_slice(segment);
        Self::new(RawPath::from_bytes(bytes))
    }

    /// Build a root-relative path from a single directory-entry name.
    pub fn from_dirent_name(segment: &[u8]) -> Result<Self, SourceError> {
        validate_dirent_name(segment)?;
        Self::new(RawPath::from_bytes(segment.to_vec()))
    }

    pub fn as_str(&self) -> Result<&str, SourceError> {
        std::str::from_utf8(self.raw.as_bytes()).map_err(|_| SourceError::EncodingError)
    }

    pub fn display_escaped(&self) -> EscapedPathDisplay<'_> {
        self.raw.display_escaped()
    }
}

fn validate_dirent_name(segment: &[u8]) -> Result<(), SourceError> {
    if segment.is_empty()
        || segment == b"."
        || segment == b".."
        || segment.contains(&0)
        || segment.contains(&b'/')
        || segment.contains(&b'\\')
    {
        return Err(SourceError::PathEscape);
    }
    Ok(())
}

impl fmt::Debug for NormalizedPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NormalizedPath({:?})", self.display_escaped())
    }
}

impl fmt::Display for NormalizedPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_escaped())
    }
}

/// Formatter that renders untrusted path bytes safely.
///
/// Escapes:
/// - ASCII control characters (`\n`, `\r`, `\t`, `\0`, `\x01`..`\x1f`, `\x7f`)
/// - ANSI escape sequence initiator `\x1b`
/// - Unicode bidirectional control characters (RLO, LRO, RLE, LRE, PDF, RLI, LRI, FSI, PDI, ALM, LRM, RLM)
/// - Invalid UTF-8 byte sequences
pub struct EscapedPathDisplay<'a> {
    bytes: &'a [u8],
}

impl<'a> fmt::Display for EscapedPathDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut i = 0;
        while i < self.bytes.len() {
            let byte = self.bytes[i];

            // 1. Check standard ASCII control characters
            match byte {
                b'\n' => {
                    write!(f, "\\n")?;
                    i += 1;
                    continue;
                }
                b'\r' => {
                    write!(f, "\\r")?;
                    i += 1;
                    continue;
                }
                b'\t' => {
                    write!(f, "\\t")?;
                    i += 1;
                    continue;
                }
                b'\0' => {
                    write!(f, "\\0")?;
                    i += 1;
                    continue;
                }
                b'\\' => {
                    write!(f, "\\\\")?;
                    i += 1;
                    continue;
                }
                0x01..=0x1f | 0x7f => {
                    write!(f, "\\x{:02x}", byte)?;
                    i += 1;
                    continue;
                }
                _ => {}
            }

            // 2. Check UTF-8 scalar values and Unicode directional controls
            match std::str::from_utf8(&self.bytes[i..]) {
                Ok(valid_str) => {
                    let ch = valid_str.chars().next().unwrap();
                    let ch_len = ch.len_utf8();
                    if is_bidi_or_formatting_control(ch) {
                        write!(f, "\\u{{{:x}}}", ch as u32)?;
                    } else {
                        write!(f, "{}", ch)?;
                    }
                    i += ch_len;
                }
                Err(e) => {
                    let valid_up_to = e.valid_up_to();
                    if valid_up_to > 0 {
                        let valid_slice = &self.bytes[i..i + valid_up_to];
                        for ch in std::str::from_utf8(valid_slice).unwrap().chars() {
                            if is_bidi_or_formatting_control(ch) {
                                write!(f, "\\u{{{:x}}}", ch as u32)?;
                            } else {
                                write!(f, "{}", ch)?;
                            }
                        }
                        i += valid_up_to;
                    } else {
                        // Invalid byte
                        write!(f, "\\x{:02x}", byte)?;
                        i += 1;
                    }
                }
            }
        }
        Ok(())
    }
}

impl<'a> fmt::Debug for EscapedPathDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", self)
    }
}

/// Checks whether a character is a bidi control or dangerous formatting code point.
fn is_bidi_or_formatting_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{200E}' // Left-to-Right Mark (LRM)
        | '\u{200F}' // Right-to-Left Mark (RLM)
        | '\u{061C}' // Arabic Letter Mark (ALM)
        | '\u{202A}' // Left-to-Right Embedding (LRE)
        | '\u{202B}' // Right-to-Left Embedding (RLE)
        | '\u{202C}' // Pop Directional Formatting (PDF)
        | '\u{202D}' // Left-to-Right Override (LRO)
        | '\u{202E}' // Right-to-Left Override (RLO)
        | '\u{2066}' // Left-to-Right Isolate (LRI)
        | '\u{2067}' // Right-to-Left Isolate (RLI)
        | '\u{2068}' // First Strong Isolate (FSI)
        | '\u{2069}' // Pop Directional Isolate (PDI)
    )
}

/// Decodes a percent-encoded path or URI and validates that it remains confined.
///
/// Rejects:
/// - Non-local `file:` URIs (e.g. `file://remote-host/repo`).
/// - Relative traversal upward (`..` or `%2e%2e` or `%2E%2E`).
/// - Encoded null bytes (`%00`).
/// - Unresolved double-encoding attempts.
pub fn decode_uri_path(uri: &str) -> Result<NormalizedPath, SourceError> {
    let mut path_str = uri;

    // Handle file: URI prefix
    if let Some(rest) = path_str.strip_prefix("file://") {
        if rest.starts_with("localhost/") {
            path_str = rest.strip_prefix("localhost").unwrap();
        } else if rest.starts_with('/') {
            // Local file URI like `file:///path`
            path_str = rest;
        } else {
            // Non-local authority like `file://foreign-host/path` -> reject
            return Err(SourceError::PathEscape);
        }
    } else if let Some(rest) = path_str.strip_prefix("file:") {
        path_str = rest;
    }

    // Strip optional leading slash for relative root evaluation
    if let Some(rest) = path_str.strip_prefix('/') {
        path_str = rest;
    }

    // Percent-decode bytes
    let decoded_bytes = percent_decode(path_str.as_bytes())?;

    // Refuse embedded nulls
    if decoded_bytes.contains(&0) {
        return Err(SourceError::EncodingError);
    }

    // Attempt normalized path construction from decoded bytes
    NormalizedPath::new(RawPath::from_bytes(decoded_bytes))
}

/// Percent-decodes a byte sequence once.
fn percent_decode(input: &[u8]) -> Result<Vec<u8>, SourceError> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' {
            if i + 2 >= input.len() {
                return Err(SourceError::EncodingError);
            }
            let h1 = hex_val(input[i + 1]).ok_or(SourceError::EncodingError)?;
            let h2 = hex_val(input[i + 2]).ok_or(SourceError::EncodingError)?;
            out.push((h1 << 4) | h2);
            i += 3;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_path_preserves_bytes_and_case() {
        let p1 = RawPath::from_str("src/Main.rs");
        let p2 = RawPath::from_str("src/main.rs");
        assert_ne!(p1, p2, "Distinct case must not be conflated");
        assert_eq!(p1.as_bytes(), b"src/Main.rs");
        assert_eq!(p2.as_bytes(), b"src/main.rs");
    }

    #[test]
    fn raw_path_handles_arbitrary_bytes() {
        let arbitrary = RawPath::from_bytes(vec![0xff, 0xfe, 0x80, 0x00]);
        assert_eq!(arbitrary.len(), 4);
        let escaped = arbitrary.display_escaped().to_string();
        assert!(escaped.contains("\\x"), "Arbitrary bytes must be escaped: {}", escaped);
        assert!(escaped.contains("\\0"), "Null byte must be escaped: {}", escaped);
    }

    #[test]
    fn normalized_path_accepts_clean_relative_paths() {
        let norm = NormalizedPath::new("src/foo/bar.rs").unwrap();
        assert_eq!(norm.as_bytes(), b"src/foo/bar.rs");
        assert_eq!(norm.segments().len(), 3);
        assert_eq!(norm.segments()[0].as_bytes(), b"src");
        assert_eq!(norm.segments()[1].as_bytes(), b"foo");
        assert_eq!(norm.segments()[2].as_bytes(), b"bar.rs");
    }

    #[test]
    fn normalized_path_rejects_escapes_and_special_segments() {
        assert_eq!(NormalizedPath::new("/etc/passwd"), Err(SourceError::PathEscape));
        assert_eq!(NormalizedPath::new("../parent.rs"), Err(SourceError::PathEscape));
        assert_eq!(NormalizedPath::new("foo/../../bar"), Err(SourceError::PathEscape));
        assert_eq!(NormalizedPath::new("foo/./bar"), Err(SourceError::PathEscape));
        assert_eq!(NormalizedPath::new("foo//bar"), Err(SourceError::PathEscape));
        assert_eq!(NormalizedPath::new(""), Err(SourceError::PathEscape));
    }

    #[test]
    fn escaped_path_display_neutralizes_newlines_and_bidi() {
        let newline_path = RawPath::from_str("line1\nline2\rline3\tline4");
        let rendered = newline_path.display_escaped().to_string();
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\r'));
        assert_eq!(rendered, "line1\\nline2\\rline3\\tline4");

        // Bidi override injection attempt (RLO = \u{202E})
        let bidi_path = RawPath::from_str("safe_\u{202E}txt.exe");
        let bidi_rendered = bidi_path.display_escaped().to_string();
        assert!(!bidi_rendered.contains('\u{202E}'));
        assert!(bidi_rendered.contains("\\u{202e}"));
    }

    #[test]
    fn decode_uri_path_resolves_and_confines() {
        let valid = decode_uri_path("src%2Fmodel%2Fstate.rs").unwrap();
        assert_eq!(valid.as_bytes(), b"src/model/state.rs");

        let file_uri = decode_uri_path("file:///workspace/project/Cargo.toml").unwrap();
        assert_eq!(file_uri.as_bytes(), b"workspace/project/Cargo.toml");

        // Encoded traversal attempts
        assert_eq!(decode_uri_path("%2e%2e/secret.key"), Err(SourceError::PathEscape));
        assert_eq!(decode_uri_path("foo/%2E%2E/secret.key"), Err(SourceError::PathEscape));
        assert_eq!(decode_uri_path("file://remote-host/repo/file.rs"), Err(SourceError::PathEscape));

        // Embedded null
        assert_eq!(decode_uri_path("foo%00bar.rs"), Err(SourceError::EncodingError));
    }
}

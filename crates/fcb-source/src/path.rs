#![forbid(unsafe_code)]

//! Raw path identities, normalized relative paths, escaped presentation,
//! and URI decoding (FCB-009.A).
//!
//! Raw paths preserve bytes and case. External/URI path normalization is a
//! separate operation from composing native directory entries: on Unix a
//! backslash or a colon is an ordinary filename byte, not a path separator.
//! Neither representation establishes race-safe native filesystem confinement.

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

/// A validated root-relative path and its authoritative component boundaries.
/// No component is empty, `.` or `..`, and there are no NUL bytes. External
/// paths passed to `new` normalize slash/backslash separators and refuse drive
/// prefixes. Native Unix directory-entry constructors preserve backslashes and
/// drive-looking names as literal bytes and NEVER normalize them into paths.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NormalizedPath {
    raw: RawPath,
    segments: Vec<RawPath>,
}

impl NormalizedPath {
    /// Validates and normalizes external path bytes into a root-relative path.
    /// Filesystem enumeration must use the native directory-entry constructors.
    pub fn new(raw: impl Into<RawPath>) -> Result<Self, SourceError> {
        let raw = raw.into();
        let bytes = raw.as_bytes();

        if bytes.is_empty() {
            return Err(SourceError::PathEscape);
        }
        if bytes.contains(&0) {
            return Err(SourceError::EncodingError);
        }
        if bytes.starts_with(b"/") || bytes.starts_with(b"\\") {
            return Err(SourceError::PathEscape);
        }
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return Err(SourceError::PathEscape);
        }

        let mut segments = Vec::new();
        let mut canonical_bytes = Vec::with_capacity(bytes.len());

        for chunk in bytes.split(|b| *b == b'/' || *b == b'\\') {
            if chunk.is_empty() || chunk == b"." || chunk == b".." {
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

    /// Append one native directory-entry name, without URI/path normalization.
    /// On Unix only slash separates components; backslashes remain filename data.
    pub fn join_segment(&self, segment: &[u8]) -> Result<Self, SourceError> {
        validate_dirent_name(segment)?;
        let length = self.raw.len().checked_add(1).and_then(|n| n.checked_add(segment.len()))
            .ok_or(SourceError::PayloadTooLarge)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|_| SourceError::PayloadTooLarge)?;
        bytes.extend_from_slice(self.raw.as_bytes());
        bytes.push(b'/');
        bytes.extend_from_slice(segment);
        #[cfg(unix)]
        {
            let mut segments = Vec::new();
            segments.try_reserve_exact(self.segments.len() + 1).map_err(|_| SourceError::PayloadTooLarge)?;
            segments.extend(self.segments.iter().cloned());
            segments.push(RawPath::from_bytes(segment));
            Ok(Self { raw: RawPath::from_bytes(bytes), segments })
        }
        #[cfg(not(unix))]
        { Self::new(RawPath::from_bytes(bytes)) }
    }

    /// Build a root-relative path from a native directory-entry name. Never
    /// reinterpret a Unix filename such as `a\b` as the nested path `a/b`.
    pub fn from_dirent_name(segment: &[u8]) -> Result<Self, SourceError> {
        validate_dirent_name(segment)?;
        #[cfg(unix)]
        {
            let raw = RawPath::from_bytes(segment);
            Ok(Self { raw: raw.clone(), segments: vec![raw] })
        }
        #[cfg(not(unix))]
        { Self::new(RawPath::from_bytes(segment.to_vec())) }
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
        || (!cfg!(unix) && segment.contains(&b'\\'))
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

/// Formatter that renders untrusted path bytes safely. Control characters,
/// backslashes, Unicode direction controls and invalid bytes are escaped even
/// in valid UTF-8 runs followed by a malformed byte. Each run is scanned once.
pub struct EscapedPathDisplay<'a> {
    bytes: &'a [u8],
}

impl fmt::Display for EscapedPathDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut rest = self.bytes;
        while !rest.is_empty() {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    for ch in text.chars() { write_path_char(f, ch)?; }
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    // valid_up_to is a verified UTF-8 boundary; no unchecked cast.
                    if let Ok(text) = std::str::from_utf8(&rest[..valid]) {
                        for ch in text.chars() { write_path_char(f, ch)?; }
                    }
                    let bad = error.error_len().unwrap_or(rest.len() - valid);
                    for byte in &rest[valid..valid + bad] { write!(f, "\\x{byte:02x}")?; }
                    rest = &rest[valid + bad..];
                }
            }
        }
        Ok(())
    }
}

fn write_path_char(f: &mut fmt::Formatter<'_>, ch: char) -> fmt::Result {
    match ch {
        '\n' => f.write_str("\\n"), '\r' => f.write_str("\\r"),
        '\t' => f.write_str("\\t"), '\0' => f.write_str("\\0"),
        '\\' => f.write_str("\\\\"),
        ch if ch.is_ascii_control() => write!(f, "\\x{:02x}", ch as u32),
        ch if ch.is_control() || is_bidi_or_formatting_control(ch) => write!(f, "\\u{{{:x}}}", ch as u32),
        ch => write!(f, "{ch}"),
    }
}

impl fmt::Debug for EscapedPathDisplay<'_> {
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
        assert_eq!(decode_uri_path("%2e%2e/secret.key"), Err(SourceError::PathEscape));
        assert_eq!(decode_uri_path("foo/%2E%2E/secret.key"), Err(SourceError::PathEscape));
        assert_eq!(decode_uri_path("file://remote-host/repo/file.rs"), Err(SourceError::PathEscape));
        assert_eq!(decode_uri_path("foo%00bar.rs"), Err(SourceError::EncodingError));
    }

    #[test]
    fn malformed_suffix_cannot_bypass_control_escaping_in_a_valid_prefix() {
        let raw = RawPath::from_bytes(b"a\n\x1b[0m\\x\xff\r\x80".as_slice());
        let display = raw.display_escaped().to_string();
        assert_eq!(display, "a\\n\\x1b[0m\\\\x\\xff\\r\\x80");
        assert!(!display.chars().any(char::is_control));
    }

    #[cfg(unix)]
    #[test]
    fn unix_directory_entries_do_not_alias_external_path_syntax() {
        let native = NormalizedPath::from_dirent_name(b"a\\b.rs").unwrap();
        let nested = NormalizedPath::new("a/b.rs").unwrap();
        assert_ne!(native, nested);
        assert_eq!(native.segments().len(), 1);
        assert_eq!(native.as_bytes(), b"a\\b.rs");
        let drive_looking = NormalizedPath::from_dirent_name(b"C:source.rs").unwrap();
        assert_eq!(drive_looking.as_bytes(), b"C:source.rs");
        assert!(NormalizedPath::new("C:source.rs").is_err());
        let joined = native.join_segment(b"child\\name").unwrap();
        assert_eq!(joined.as_bytes(), b"a\\b.rs/child\\name");
        assert_eq!(joined.segments().len(), 2);
        for invalid in [b"".as_slice(), b".", b"..", b"a/b", b"nul\0"] {
            assert!(NormalizedPath::from_dirent_name(invalid).is_err());
            assert!(native.join_segment(invalid).is_err());
        }
    }
}

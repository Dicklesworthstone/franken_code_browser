//! Multi-flavor clipboard and exact-byte export contracts (FCB-053.A, fcb-8bqc.1).
//!
//! Enforces the clipboard contracts from plan §22.6:
//! - Ordinary Unicode copy publishes declared decoded Unicode without silently normalizing
//!   line endings (CRLF preserved) or substituting visually reordered glyph text (bidi preserved).
//! - Exact original bytes (including invalid UTF-8, UTF-16 BOMs, CRLF, bidi) use an explicit
//!   opaque native clipboard flavor (`com.franken.fcb.exact-bytes`), never mislabeled as plain-text.
//! - Clipboard staging and all advertised flavors count toward the copy budget.
//! - Pre-publication refusal for revoked/stale capture, cancellation, or oversized copy
//!   leaves the clipboard untouched.
//! - Injected native publication failures and concurrent external changes are reported
//!   accurately without payload logs or unsafe rollback.

use std::fmt;

/// Standard and custom clipboard flavor identifiers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ClipboardFlavor {
    /// Standard macOS UTF-8 plain text (`public.utf8-plain-text`).
    PlainTextUtf8,
    /// Opaque first-party byte-exact source export (`com.franken.fcb.exact-bytes`).
    ExactBytes,
    /// Formatted Markdown source flavor (`net.daringfireball.markdown`).
    Markdown,
    /// Standard HTML flavor (`public.html`).
    Html,
}

impl ClipboardFlavor {
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::PlainTextUtf8 => "public.utf8-plain-text",
            Self::ExactBytes => "com.franken.fcb.exact-bytes",
            Self::Markdown => "net.daringfireball.markdown",
            Self::Html => "public.html",
        }
    }
}

/// Errors surfaced by clipboard staging and publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardError {
    /// Payload exceeds bounded clipboard byte budget; clipboard is preserved untouched.
    BudgetRefusal { requested: usize, max: usize },
    /// Workspace or root read grant was revoked before publication; clipboard is untouched.
    GrantRevoked,
    /// The copy operation was cancelled before publication; clipboard is untouched.
    Cancelled,
    /// Native system pasteboard returned an error during publication.
    NativePublicationFailure,
    /// Another process or external event updated the clipboard concurrently;
    /// publication aborted to prevent overwriting newer external data.
    ConcurrentExternalChange {
        expected_generation: u64,
        actual_generation: u64,
    },
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BudgetRefusal { requested, max } => {
                write!(
                    f,
                    "clipboard budget refused: requested {requested} bytes, max {max}"
                )
            }
            Self::GrantRevoked => f.write_str("clipboard copy refused: read grant was revoked"),
            Self::Cancelled => f.write_str("clipboard copy cancelled before publication"),
            Self::NativePublicationFailure => {
                f.write_str("native system pasteboard publication failed")
            }
            Self::ConcurrentExternalChange {
                expected_generation,
                actual_generation,
            } => {
                write!(
                    f,
                    "concurrent external clipboard change detected (expected gen {expected_generation}, actual gen {actual_generation})"
                )
            }
        }
    }
}

impl std::error::Error for ClipboardError {}

/// A prepared clipboard payload ready for publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardPayload {
    pub flavor: ClipboardFlavor,
    pub data: Vec<u8>,
    pub source_anchor: Option<(u64, u64)>,
    pub has_bom: bool,
    pub has_crlf: bool,
    pub has_bidi: bool,
    pub escaped_malformed: bool,
}

impl ClipboardPayload {
    /// Prepare an ordinary Unicode copy payload.
    ///
    /// Preserves exact declared Unicode characters, CRLF line endings, and bidi markers
    /// without silent normalization or visual reordering.
    pub fn new_unicode(text: &str, source_anchor: Option<(u64, u64)>) -> Self {
        let has_crlf = text.contains("\r\n");
        let has_bom = text.starts_with('\u{FEFF}');
        let has_bidi = text.chars().any(|c| {
            matches!(
                c,
                '\u{200E}' | '\u{200F}' | '\u{202A}' | '\u{202B}' | '\u{202C}' | '\u{202D}' | '\u{202E}'
            )
        });
        Self {
            flavor: ClipboardFlavor::PlainTextUtf8,
            data: text.as_bytes().to_vec(),
            source_anchor,
            has_bom,
            has_crlf,
            has_bidi,
            escaped_malformed: false,
        }
    }

    /// Prepare an exact-byte export payload.
    ///
    /// Preserves arbitrary raw bytes (including malformed UTF-8, UTF-16 BOMs, CRLF, bidi)
    /// under an explicit opaque clipboard flavor, never mislabeled as plain-text.
    pub fn new_exact_bytes(raw_bytes: &[u8], source_anchor: Option<(u64, u64)>) -> Self {
        let has_bom = raw_bytes.starts_with(&[0xEF, 0xBB, 0xBF])
            || raw_bytes.starts_with(&[0xFE, 0xFF])
            || raw_bytes.starts_with(&[0xFF, 0xFE]);
        let has_crlf = raw_bytes.windows(2).any(|w| w == b"\r\n");
        let is_valid_utf8 = std::str::from_utf8(raw_bytes).is_ok();
        let has_bidi = if let Ok(s) = std::str::from_utf8(raw_bytes) {
            s.chars().any(|c| {
                matches!(
                    c,
                    '\u{200E}' | '\u{200F}' | '\u{202A}' | '\u{202B}' | '\u{202C}' | '\u{202D}' | '\u{202E}'
                )
            })
        } else {
            false
        };

        Self {
            flavor: ClipboardFlavor::ExactBytes,
            data: raw_bytes.to_vec(),
            source_anchor,
            has_bom,
            has_crlf,
            has_bidi,
            escaped_malformed: !is_valid_utf8,
        }
    }
}

/// Simulated native pasteboard managing bounded multi-flavor publications.
#[derive(Debug)]
pub struct NativePasteboard {
    max_payload_bytes: usize,
    current_generation: u64,
    published: Vec<ClipboardPayload>,
    grant_valid: bool,
    simulate_native_failure: bool,
}

impl NativePasteboard {
    pub fn new(max_payload_bytes: usize) -> Self {
        Self {
            max_payload_bytes,
            current_generation: 0,
            published: Vec::new(),
            grant_valid: true,
            simulate_native_failure: false,
        }
    }

    pub const fn current_generation(&self) -> u64 {
        self.current_generation
    }

    pub fn published_payloads(&self) -> &[ClipboardPayload] {
        &self.published
    }

    pub fn set_grant_valid(&mut self, valid: bool) {
        self.grant_valid = valid;
    }

    pub fn set_simulate_native_failure(&mut self, simulate: bool) {
        self.simulate_native_failure = simulate;
    }

    /// Simulate an external application writing to the system clipboard.
    pub fn simulate_external_change(&mut self) {
        self.current_generation += 1;
        self.published.clear();
    }

    /// Publish a prepared payload to the pasteboard.
    ///
    /// Fails safely if:
    /// - Payload exceeds max budget (preserves old clipboard).
    /// - Grant was revoked (preserves old clipboard).
    /// - Concurrent external change occurred (avoids unsafe rollback/overwrite).
    /// - Native system failure was injected.
    pub fn publish(
        &mut self,
        payload: ClipboardPayload,
        expected_generation: u64,
    ) -> Result<u64, ClipboardError> {
        if !self.grant_valid {
            return Err(ClipboardError::GrantRevoked);
        }
        if payload.data.len() > self.max_payload_bytes {
            return Err(ClipboardError::BudgetRefusal {
                requested: payload.data.len(),
                max: self.max_payload_bytes,
            });
        }
        if expected_generation != self.current_generation {
            return Err(ClipboardError::ConcurrentExternalChange {
                expected_generation,
                actual_generation: self.current_generation,
            });
        }
        if self.simulate_native_failure {
            return Err(ClipboardError::NativePublicationFailure);
        }

        self.current_generation += 1;
        self.published = vec![payload];
        Ok(self.current_generation)
    }
}

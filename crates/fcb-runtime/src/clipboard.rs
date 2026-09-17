#![forbid(unsafe_code)]

//! Native clipboard staging, publication, and round-trip verification (§10.8, §22.6).
//!
//! Provides atomic, multi-flavor clipboard operations for actual source data:
//! - [`ClipboardFlavor::PlainTextUtf8`]: Decoded Unicode text.
//! - [`ClipboardFlavor::ExactRawBytes`]: Verbatim captured bytes (preserving BOM,
//!   CRLF, surrogate pairs, combining marks, and malformed sequences).
//! - [`ClipboardFlavor::LocationProvenance`]: Location header (file, lines, range).
//!
//! Budget overruns are refused before clipboard mutation occurs. External concurrent
//! modifications are detected and prevent stale overwrites.

use std::ops::Range;

/// Maximum limits permitted for clipboard operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClipboardLimits {
    /// Maximum bytes permitted for clipboard publication (e.g. 4 MiB).
    pub max_clipboard_bytes: usize,
}

impl Default for ClipboardLimits {
    fn default() -> Self {
        Self {
            max_clipboard_bytes: 4 * 1024 * 1024, // 4 MiB
        }
    }
}

/// Clipboard data flavors staged for OS pasteboard publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardFlavor {
    PlainTextUtf8,
    ExactRawBytes,
    LocationProvenance,
}

/// Detailed errors returned during clipboard preparation or publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardError {
    BudgetExceeded {
        requested_bytes: usize,
        max_budget_bytes: usize,
    },
    ConcurrentExternalChange {
        expected_seq: u64,
        actual_seq: u64,
    },
    InvalidRange,
    PublicationFailed,
    Canceled,
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExceeded {
                requested_bytes,
                max_budget_bytes,
            } => {
                write!(
                    f,
                    "CLIPBOARD_BUDGET_EXCEEDED: requested {requested_bytes} bytes, limit is {max_budget_bytes}"
                )
            }
            Self::ConcurrentExternalChange {
                expected_seq,
                actual_seq,
            } => {
                write!(
                    f,
                    "CLIPBOARD_CONCURRENT_EXTERNAL_CHANGE: expected seq {expected_seq}, current is {actual_seq}"
                )
            }
            Self::InvalidRange => write!(f, "CLIPBOARD_INVALID_RANGE"),
            Self::PublicationFailed => write!(f, "CLIPBOARD_PUBLICATION_FAILED"),
            Self::Canceled => write!(f, "CLIPBOARD_CANCELED"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// Pre-staged multi-flavor clipboard data verified against budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardPayload {
    pub plain_text: String,
    pub exact_bytes: Vec<u8>,
    pub provenance: String,
    pub has_replacement_characters: bool,
    pub total_staged_bytes: usize,
}

impl ClipboardPayload {
    /// Stages an exact source byte range into multi-flavor clipboard representations.
    ///
    /// Preserves exact bytes verbatim (including BOM, CRLF, astral emojis, combining marks).
    /// Refuses with [`ClipboardError::BudgetExceeded`] if `range.len() > limits.max_clipboard_bytes`.
    pub fn stage(
        source_bytes: &[u8],
        range: Range<usize>,
        file_path: &str,
        revision: u64,
        start_line: usize,
        end_line: usize,
        limits: ClipboardLimits,
    ) -> Result<Self, ClipboardError> {
        if range.start > range.end || range.end > source_bytes.len() {
            return Err(ClipboardError::InvalidRange);
        }

        let slice = source_bytes
            .get(range.clone())
            .ok_or(ClipboardError::InvalidRange)?;
        let byte_len = slice.len();

        if byte_len > limits.max_clipboard_bytes {
            return Err(ClipboardError::BudgetExceeded {
                requested_bytes: byte_len,
                max_budget_bytes: limits.max_clipboard_bytes,
            });
        }

        let exact_bytes = slice.to_vec();

        let (plain_text, has_replacement_characters) = match std::str::from_utf8(slice) {
            Ok(valid) => (valid.to_string(), false),
            Err(_) => (String::from_utf8_lossy(slice).into_owned(), true),
        };

        let provenance = format!(
            "File: {file_path}\nRevision: {revision}\nLines: {start_line}-{end_line}\nByteRange: {}..{}\n",
            range.start, range.end
        );

        let total_staged_bytes = exact_bytes.len() + plain_text.len() + provenance.len();
        if total_staged_bytes > limits.max_clipboard_bytes.saturating_mul(3) {
            return Err(ClipboardError::BudgetExceeded {
                requested_bytes: total_staged_bytes,
                max_budget_bytes: limits.max_clipboard_bytes,
            });
        }

        Ok(Self {
            plain_text,
            exact_bytes,
            provenance,
            has_replacement_characters,
            total_staged_bytes,
        })
    }
}

/// Simulated native OS clipboard with generation tracking and atomic publication.
#[derive(Clone, Debug)]
pub struct NativeClipboard {
    plain_text: Option<String>,
    exact_bytes: Option<Vec<u8>>,
    provenance: Option<String>,
    generation_seq: u64,
    fail_next: bool,
}

impl Default for NativeClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeClipboard {
    pub const fn new() -> Self {
        Self {
            plain_text: None,
            exact_bytes: None,
            provenance: None,
            generation_seq: 1,
            fail_next: false,
        }
    }

    pub const fn generation_seq(&self) -> u64 {
        self.generation_seq
    }

    pub fn plain_text(&self) -> Option<&str> {
        self.plain_text.as_deref()
    }

    pub fn exact_bytes(&self) -> Option<&[u8]> {
        self.exact_bytes.as_deref()
    }

    pub fn provenance(&self) -> Option<&str> {
        self.provenance.as_deref()
    }

    /// Injects an intentional OS publication failure (for negative control testing).
    pub fn inject_failure(&mut self, fail: bool) {
        self.fail_next = fail;
    }

    /// Simulates a concurrent external modification from another application.
    pub fn simulate_external_change(&mut self, text: &str) {
        self.plain_text = Some(text.to_string());
        self.exact_bytes = Some(text.as_bytes().to_vec());
        self.provenance = None;
        self.generation_seq = self.generation_seq.wrapping_add(1);
    }

    /// Publishes staged multi-flavor clipboard data atomically.
    ///
    /// Validates `expected_seq == self.generation_seq`.
    /// On failure, the prior clipboard content is preserved without partial mutation.
    pub fn publish(
        &mut self,
        payload: ClipboardPayload,
        expected_seq: u64,
    ) -> Result<(), ClipboardError> {
        if self.generation_seq != expected_seq {
            return Err(ClipboardError::ConcurrentExternalChange {
                expected_seq,
                actual_seq: self.generation_seq,
            });
        }

        if self.fail_next {
            self.fail_next = false;
            return Err(ClipboardError::PublicationFailed);
        }

        self.plain_text = Some(payload.plain_text);
        self.exact_bytes = Some(payload.exact_bytes);
        self.provenance = Some(payload.provenance);
        self.generation_seq = self.generation_seq.wrapping_add(1);
        Ok(())
    }
}

/// Helper for verifying end-to-end source clipboard round-trips.
pub struct ClipboardRoundTrip;

impl ClipboardRoundTrip {
    /// Verifies that clipboard contents match the exact original slice byte-for-byte.
    pub fn verify_round_trip(
        original_source: &[u8],
        range: Range<usize>,
        clipboard: &NativeClipboard,
    ) -> bool {
        let expected = match original_source.get(range) {
            Some(s) => s,
            None => return false,
        };

        match clipboard.exact_bytes() {
            Some(actual) => actual == expected,
            None => false,
        }
    }
}

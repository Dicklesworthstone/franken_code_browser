//! Qualified huge-line visual access routes (FCB-082.B / fcb-o9v5.2).
//!
//! §10.9 Horizontal virtualization has a shaping-context boundary:
//! - Visible-only glyph materialization is valid; arbitrary substring shaping is
//!   not always equivalent to shaping the full context. Bidirectional ordering may
//!   depend on the paragraph, ligatures and joining may cross a viewport edge,
//!   tabs depend on preceding advances, and a grapheme may contain many combining
//!   characters. Unicode's bidi and grapheme algorithms operate on their defined
//!   context, not arbitrary byte tiles.
//! - Provide a qualified fixed-pitch ASCII/code fast path with checkpoints for tabs
//!   and display controls.
//! - For general text, obtain necessary paragraph/directional/shaping context through
//!   a bounded resumable preparation pass, retain reusable context checkpoints where
//!   sound, and materialize only the visible runs after that context exists. A run
//!   cannot be labeled exact because it merely looks plausible.
//! - For a pathological line whose required context exceeds the active work budget
//!   (e.g. 500 MB line, bidi paragraph, long combining sequence), keep the UI
//!   responsive and state `context pending`, or offer an explicitly labeled
//!   logical/escaped view. Exact byte access and search remain available.
//! - CoreText calls themselves are foreign operations; never hand them an unbounded
//!   line on the main thread.

#![forbid(unsafe_code)]

use crate::old_anchor::VisualContextReadiness;

/// Maximum safe combining character count before triggering logical/escaped fallback.
pub const MAX_SAFE_COMBINING_SEQUENCE: usize = 32;

/// Default context preparation budget in bytes (64 KiB).
pub const DEFAULT_MAX_CONTEXT_BYTES: u64 = 64 * 1024;

/// Threshold in bytes beyond which a line is classified as a huge/pathological line (500 MB).
pub const HUGE_LINE_THRESHOLD_BYTES: u64 = 500 * 1024 * 1024;

/// Tab stop configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabConfig {
    /// Number of columns per tab stop (typically 4 or 8).
    pub tab_width: u32,
}

impl Default for TabConfig {
    fn default() -> Self {
        Self { tab_width: 4 }
    }
}

impl TabConfig {
    pub const fn new(tab_width: u32) -> Self {
        let width = if tab_width == 0 { 4 } else { tab_width };
        Self { tab_width: width }
    }

    /// Calculate the next column position after a tab at `current_col`.
    pub const fn advance_tab(&self, current_col: u64) -> u64 {
        let width = self.tab_width as u64;
        let rem = current_col % width;
        current_col + (width - rem)
    }
}

/// Horizontal checkpoint in an ASCII fast-path line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HorizontalCheckpoint {
    pub byte_offset: u64,
    pub visual_column: u64,
    pub is_tab: bool,
    pub is_control: bool,
}

/// Text run classification for visual materialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextRunKind {
    /// Plain ASCII text (1 byte = 1 column, fixed pitch).
    AsciiText,
    /// Tab advance run.
    Tab,
    /// Control character displayed in escaped representation (e.g. `\r`, `\0`, `^C`).
    ControlEscaped,
    /// Multi-byte UTF-8 or complex script requiring font shaping.
    ComplexShaped,
    /// Long combining sequence exceeding safe shaping limits.
    CombiningSequence,
}

impl TextRunKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::AsciiText => "ASCII_TEXT",
            Self::Tab => "TAB",
            Self::ControlEscaped => "CONTROL_ESCAPED",
            Self::ComplexShaped => "COMPLEX_SHAPED",
            Self::CombiningSequence => "COMBINING_SEQUENCE",
        }
    }
}

/// Direction of a text run for bidirectional layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BidiDirection {
    LeftToRight,
    RightToLeft,
    Neutral,
}

impl BidiDirection {
    pub const fn code(self) -> &'static str {
        match self {
            Self::LeftToRight => "LTR",
            Self::RightToLeft => "RTL",
            Self::Neutral => "NEUTRAL",
        }
    }
}

/// State of horizontal shaping context preparation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextPreparationState {
    /// Context preparation completed within budget; visual layout is exact.
    Ready {
        is_pure_ascii: bool,
        has_bidi: bool,
        total_columns: u64,
    },
    /// Pathological line exceeded work budget; visual context is pending.
    /// UI remains responsive; exact byte access remains available.
    Pending {
        scanned_bytes: u64,
        total_declared_bytes: u64,
        reason: &'static str,
    },
    /// Pathological line routed to explicit logical/escaped view
    /// (e.g. combining sequence overload or giant line fallback).
    LogicalFallback {
        reason: &'static str,
    },
}

impl ContextPreparationState {
    pub fn readiness(&self) -> VisualContextReadiness {
        match self {
            Self::Ready { .. } => VisualContextReadiness::ExactVisual,
            Self::Pending { .. } => VisualContextReadiness::ContextPending,
            Self::LogicalFallback { .. } => VisualContextReadiness::LogicalEscaped,
        }
    }
}

/// Work budget for horizontal context preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextPreparationBudget {
    /// Maximum bytes to scan for context before marking `ContextPending`.
    pub max_bytes: u64,
    /// Maximum step iterations.
    pub max_steps: usize,
}

impl Default for ContextPreparationBudget {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_CONTEXT_BYTES,
            max_steps: 10_000,
        }
    }
}

/// A visible text run materialized for viewport display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializedRun {
    /// Byte range within the source line `[start, end)`.
    pub byte_range: (u64, u64),
    /// Visual column range `[start_col, end_col)`.
    pub visual_col_range: (u64, u64),
    /// Classification of this run.
    pub kind: TextRunKind,
    /// Text direction (LTR, RTL, Neutral).
    pub direction: BidiDirection,
    /// Readiness state (ExactVisual, ContextPending, LogicalEscaped).
    pub readiness: VisualContextReadiness,
    /// Display text representation for rendering.
    pub display_text: String,
}

/// Viewport column window for horizontal virtualization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VisibleColumnWindow {
    pub start_col: u64,
    pub end_col: u64,
}

impl VisibleColumnWindow {
    pub const fn new(start_col: u64, end_col: u64) -> Self {
        Self { start_col, end_col }
    }

    pub const fn width(&self) -> u64 {
        if self.end_col >= self.start_col {
            self.end_col - self.start_col
        } else {
            0
        }
    }

    pub const fn contains_col(&self, col: u64) -> bool {
        col >= self.start_col && col < self.end_col
    }

    pub const fn overlaps(&self, start: u64, end: u64) -> bool {
        start < self.end_col && end > self.start_col
    }
}

/// Qualified router for huge-line visual access.
pub struct HugeLineVisualRouter;

impl HugeLineVisualRouter {
    /// Prepare context for a given line under budget.
    ///
    /// If `total_declared_bytes` exceeds `budget.max_bytes`, marks `ContextPending`
    /// without blocking the caller or executing foreign CoreText shaping calls.
    pub fn prepare_context(
        line_bytes: &[u8],
        total_declared_bytes: u64,
        tab_config: TabConfig,
        budget: ContextPreparationBudget,
    ) -> ContextPreparationState {
        // 1. Pathological line exceeding byte budget
        if total_declared_bytes > budget.max_bytes {
            return ContextPreparationState::Pending {
                scanned_bytes: line_bytes.len() as u64,
                total_declared_bytes,
                reason: "line byte length exceeds context preparation work budget",
            };
        }

        let mut is_pure_ascii = true;
        let mut has_bidi = false;
        let mut max_combining = 0usize;
        let mut current_combining = 0usize;
        let mut current_col = 0u64;
        let mut step_count = 0usize;

        let mut idx = 0;
        let bytes_len = line_bytes.len();

        while idx < bytes_len {
            if step_count >= budget.max_steps {
                return ContextPreparationState::Pending {
                    scanned_bytes: idx as u64,
                    total_declared_bytes,
                    reason: "context preparation step budget exhausted",
                };
            }
            step_count += 1;

            let b = line_bytes[idx];
            if b < 0x80 {
                // ASCII byte
                current_combining = 0;
                if b == b'\t' {
                    current_col = tab_config.advance_tab(current_col);
                } else if b < 0x20 || b == 0x7F {
                    // Control character: rendered as ^X or \x.. (2 columns)
                    current_col = current_col.saturating_add(2);
                } else {
                    current_col = current_col.saturating_add(1);
                }
                idx += 1;
            } else {
                // Multi-byte UTF-8 character
                is_pure_ascii = false;
                let s = match std::str::from_utf8(&line_bytes[idx..]) {
                    Ok(valid) => valid,
                    Err(err) => {
                        let valid_up_to = err.valid_up_to();
                        if valid_up_to == 0 {
                            // Invalid UTF-8: route to logical fallback
                            return ContextPreparationState::LogicalFallback {
                                reason: "invalid UTF-8 bytes in line",
                            };
                        }
                        std::str::from_utf8(&line_bytes[idx..idx + valid_up_to])
                            .unwrap_or("")
                    }
                };

                let ch = match s.chars().next() {
                    Some(c) => c,
                    None => {
                        idx += 1;
                        continue;
                    }
                };
                let ch_len = ch.len_utf8();

                // Check for Bidi characters (Hebrew, Arabic, etc.)
                if is_bidi_char(ch) {
                    has_bidi = true;
                }

                // Check for combining characters
                if is_combining_char(ch) {
                    current_combining += 1;
                    if current_combining > max_combining {
                        max_combining = current_combining;
                    }
                } else {
                    current_combining = 0;
                    current_col = current_col.saturating_add(1);
                }

                idx += ch_len;
            }
        }

        // Long combining sequences fall back to LogicalFallback
        if max_combining > MAX_SAFE_COMBINING_SEQUENCE {
            return ContextPreparationState::LogicalFallback {
                reason: "combining character sequence exceeds safe shaping limit",
            };
        }

        ContextPreparationState::Ready {
            is_pure_ascii,
            has_bidi,
            total_columns: current_col,
        }
    }

    /// Fast-path calculation of byte offset to column for pure ASCII lines with tab stops.
    pub fn ascii_byte_to_column(
        line_bytes: &[u8],
        target_byte_offset: u64,
        tab_config: TabConfig,
    ) -> u64 {
        let limit = (target_byte_offset as usize).min(line_bytes.len());
        let mut col = 0u64;
        for &b in &line_bytes[..limit] {
            if b == b'\t' {
                col = tab_config.advance_tab(col);
            } else if b < 0x20 || b == 0x7F {
                col = col.saturating_add(2);
            } else {
                col = col.saturating_add(1);
            }
        }
        col
    }

    /// Fast-path calculation of column to byte offset for pure ASCII lines with tab stops.
    pub fn ascii_column_to_byte(
        line_bytes: &[u8],
        target_column: u64,
        tab_config: TabConfig,
    ) -> u64 {
        let mut col = 0u64;
        for (idx, &b) in line_bytes.iter().enumerate() {
            if col >= target_column {
                return idx as u64;
            }
            if b == b'\t' {
                col = tab_config.advance_tab(col);
            } else if b < 0x20 || b == 0x7F {
                col = col.saturating_add(2);
            } else {
                col = col.saturating_add(1);
            }
        }
        line_bytes.len() as u64
    }

    /// Materialize visible text runs for the given column window.
    ///
    /// Only materializes glyphs within the viewport column window! Never builds
    /// full-file or full-line strings for pathological inputs.
    pub fn materialize_visible_runs(
        line_bytes: &[u8],
        state: &ContextPreparationState,
        window: VisibleColumnWindow,
        tab_config: TabConfig,
    ) -> Vec<MaterializedRun> {
        let mut runs = Vec::new();

        match state {
            ContextPreparationState::Ready { is_pure_ascii, has_bidi, .. } => {
                if *is_pure_ascii {
                    // Fast ASCII path
                    Self::materialize_ascii_runs(line_bytes, window, tab_config, &mut runs);
                } else if *has_bidi {
                    // Bidi text path: segmented directional runs
                    Self::materialize_bidi_runs(line_bytes, window, tab_config, &mut runs);
                } else {
                    // General Unicode text path
                    Self::materialize_unicode_runs(line_bytes, window, tab_config, &mut runs);
                }
            }
            ContextPreparationState::Pending { scanned_bytes, total_declared_bytes, reason } => {
                // Context pending: return placeholder run with exact byte boundary
                runs.push(MaterializedRun {
                    byte_range: (0, *scanned_bytes),
                    visual_col_range: (window.start_col, window.end_col),
                    kind: TextRunKind::ComplexShaped,
                    direction: BidiDirection::Neutral,
                    readiness: VisualContextReadiness::ContextPending,
                    display_text: format!("[CONTEXT PENDING: {} ({} of {} bytes)]", reason, scanned_bytes, total_declared_bytes),
                });
            }
            ContextPreparationState::LogicalFallback { reason } => {
                // Logical escaped view: render escaped characters for the visible window
                Self::materialize_logical_escaped_runs(line_bytes, window, reason, &mut runs);
            }
        }

        runs
    }

    fn materialize_ascii_runs(
        line_bytes: &[u8],
        window: VisibleColumnWindow,
        tab_config: TabConfig,
        runs: &mut Vec<MaterializedRun>,
    ) {
        let mut col = 0u64;
        let mut run_start_byte: Option<u64> = None;
        let mut run_start_col = 0u64;
        let mut current_str = String::new();

        for (idx, &b) in line_bytes.iter().enumerate() {
            let b_idx = idx as u64;
            let next_col = if b == b'\t' {
                tab_config.advance_tab(col)
            } else if b < 0x20 || b == 0x7F {
                col.saturating_add(2)
            } else {
                col.saturating_add(1)
            };

            // Check if this character intersects the visible window
            if next_col > window.start_col && col < window.end_col {
                if b == b'\t' {
                    // Flush existing run
                    if let Some(start_b) = run_start_byte.take() {
                        runs.push(MaterializedRun {
                            byte_range: (start_b, b_idx),
                            visual_col_range: (run_start_col, col),
                            kind: TextRunKind::AsciiText,
                            direction: BidiDirection::LeftToRight,
                            readiness: VisualContextReadiness::ExactVisual,
                            display_text: std::mem::take(&mut current_str),
                        });
                    }
                    let tab_len = (next_col - col) as usize;
                    runs.push(MaterializedRun {
                        byte_range: (b_idx, b_idx + 1),
                        visual_col_range: (col, next_col),
                        kind: TextRunKind::Tab,
                        direction: BidiDirection::Neutral,
                        readiness: VisualContextReadiness::ExactVisual,
                        display_text: " ".repeat(tab_len),
                    });
                } else if b < 0x20 || b == 0x7F {
                    if let Some(start_b) = run_start_byte.take() {
                        runs.push(MaterializedRun {
                            byte_range: (start_b, b_idx),
                            visual_col_range: (run_start_col, col),
                            kind: TextRunKind::AsciiText,
                            direction: BidiDirection::LeftToRight,
                            readiness: VisualContextReadiness::ExactVisual,
                            display_text: std::mem::take(&mut current_str),
                        });
                    }
                    let esc = format!("^{}", (b ^ 0x40) as char);
                    runs.push(MaterializedRun {
                        byte_range: (b_idx, b_idx + 1),
                        visual_col_range: (col, next_col),
                        kind: TextRunKind::ControlEscaped,
                        direction: BidiDirection::Neutral,
                        readiness: VisualContextReadiness::ExactVisual,
                        display_text: esc,
                    });
                } else {
                    if run_start_byte.is_none() {
                        run_start_byte = Some(b_idx);
                        run_start_col = col;
                    }
                    current_str.push(b as char);
                }
            } else if col >= window.end_col {
                break;
            }

            col = next_col;
        }

        if let Some(start_b) = run_start_byte {
            runs.push(MaterializedRun {
                byte_range: (start_b, line_bytes.len() as u64),
                visual_col_range: (run_start_col, col),
                kind: TextRunKind::AsciiText,
                direction: BidiDirection::LeftToRight,
                readiness: VisualContextReadiness::ExactVisual,
                display_text: current_str,
            });
        }
    }

    fn materialize_bidi_runs(
        line_bytes: &[u8],
        window: VisibleColumnWindow,
        tab_config: TabConfig,
        runs: &mut Vec<MaterializedRun>,
    ) {
        // Segment into directional runs
        if let Ok(s) = std::str::from_utf8(line_bytes) {
            let mut col = 0u64;
            let mut byte_offset = 0u64;

            for ch in s.chars() {
                let ch_len = ch.len_utf8() as u64;
                let next_col = if ch == '\t' {
                    tab_config.advance_tab(col)
                } else {
                    col + 1
                };

                if next_col > window.start_col && col < window.end_col {
                    let dir = if is_bidi_char(ch) {
                        BidiDirection::RightToLeft
                    } else if ch.is_alphabetic() {
                        BidiDirection::LeftToRight
                    } else {
                        BidiDirection::Neutral
                    };

                    let kind = if ch == '\t' {
                        TextRunKind::Tab
                    } else if is_bidi_char(ch) {
                        TextRunKind::ComplexShaped
                    } else {
                        TextRunKind::AsciiText
                    };

                    let display = if ch == '\t' {
                        " ".repeat((next_col - col) as usize)
                    } else {
                        ch.to_string()
                    };

                    runs.push(MaterializedRun {
                        byte_range: (byte_offset, byte_offset + ch_len),
                        visual_col_range: (col, next_col),
                        kind,
                        direction: dir,
                        readiness: VisualContextReadiness::ExactVisual,
                        display_text: display,
                    });
                } else if col >= window.end_col {
                    break;
                }

                col = next_col;
                byte_offset += ch_len;
            }
        }
    }

    fn materialize_unicode_runs(
        line_bytes: &[u8],
        window: VisibleColumnWindow,
        tab_config: TabConfig,
        runs: &mut Vec<MaterializedRun>,
    ) {
        if let Ok(s) = std::str::from_utf8(line_bytes) {
            let mut col = 0u64;
            let mut byte_offset = 0u64;

            for ch in s.chars() {
                let ch_len = ch.len_utf8() as u64;
                let next_col = if ch == '\t' {
                    tab_config.advance_tab(col)
                } else {
                    col + 1
                };

                if next_col > window.start_col && col < window.end_col {
                    let kind = if ch.is_ascii() {
                        TextRunKind::AsciiText
                    } else {
                        TextRunKind::ComplexShaped
                    };
                    runs.push(MaterializedRun {
                        byte_range: (byte_offset, byte_offset + ch_len),
                        visual_col_range: (col, next_col),
                        kind,
                        direction: BidiDirection::LeftToRight,
                        readiness: VisualContextReadiness::ExactVisual,
                        display_text: ch.to_string(),
                    });
                } else if col >= window.end_col {
                    break;
                }

                col = next_col;
                byte_offset += ch_len;
            }
        }
    }

    fn materialize_logical_escaped_runs(
        line_bytes: &[u8],
        window: VisibleColumnWindow,
        _reason: &'static str,
        runs: &mut Vec<MaterializedRun>,
    ) {
        let mut col = 0u64;
        for (idx, &b) in line_bytes.iter().enumerate() {
            let esc = if b.is_ascii_graphic() {
                (b as char).to_string()
            } else if b == b' ' {
                " ".to_string()
            } else {
                format!("\\x{:02X}", b)
            };
            let width = esc.len() as u64;
            let next_col = col + width;

            if next_col > window.start_col && col < window.end_col {
                runs.push(MaterializedRun {
                    byte_range: (idx as u64, (idx + 1) as u64),
                    visual_col_range: (col, next_col),
                    kind: TextRunKind::ControlEscaped,
                    direction: BidiDirection::Neutral,
                    readiness: VisualContextReadiness::LogicalEscaped,
                    display_text: esc,
                });
            } else if col >= window.end_col {
                break;
            }

            col = next_col;
        }
    }
}

/// Helper to classify bidirectional characters (Hebrew, Arabic, etc.).
fn is_bidi_char(c: char) -> bool {
    let u = c as u32;
    matches!(
        u,
        0x0590..=0x08FF | // Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan
        0xFB1D..=0xFDFF | // Hebrew/Arabic presentation forms
        0xFE70..=0xFEFF   // Arabic presentation forms B
    )
}

/// Helper to classify combining characters.
fn is_combining_char(c: char) -> bool {
    let u = c as u32;
    matches!(
        u,
        0x0300..=0x036F | // Combining Diacritical Marks
        0x1DC0..=0x1DFF | // Combining Diacritical Marks Supplement
        0x20D0..=0x20FF | // Combining Diacritical Marks for Symbols
        0xFE20..=0xFE2F   // Combining Half Marks
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tab_advance_calculation() {
        let config = TabConfig::new(4);
        assert_eq!(config.advance_tab(0), 4);
        assert_eq!(config.advance_tab(1), 4);
        assert_eq!(config.advance_tab(3), 4);
        assert_eq!(config.advance_tab(4), 8);

        let config8 = TabConfig::new(8);
        assert_eq!(config8.advance_tab(0), 8);
        assert_eq!(config8.advance_tab(5), 8);
        assert_eq!(config8.advance_tab(8), 16);
    }

    #[test]
    fn test_ascii_fast_path_context_ready() {
        let line = b"pub fn hello() -> u32 {\n";
        let state = HugeLineVisualRouter::prepare_context(
            line,
            line.len() as u64,
            TabConfig::default(),
            ContextPreparationBudget::default(),
        );

        if let ContextPreparationState::Ready { is_pure_ascii, has_bidi, .. } = state {
            assert!(is_pure_ascii);
            assert!(!has_bidi);
        } else {
            assert!(false, "expected Ready state");
        }
        assert_eq!(state.readiness(), VisualContextReadiness::ExactVisual);
    }

    #[test]
    fn test_500mb_pathological_line_marks_context_pending() {
        let small_slice = b"beginning of giant line...";
        let state = HugeLineVisualRouter::prepare_context(
            small_slice,
            HUGE_LINE_THRESHOLD_BYTES, // declared 500 MB
            TabConfig::default(),
            ContextPreparationBudget::default(),
        );

        if let ContextPreparationState::Pending { total_declared_bytes, .. } = state {
            assert_eq!(total_declared_bytes, HUGE_LINE_THRESHOLD_BYTES);
        } else {
            assert!(false, "expected Pending state for 500MB line");
        }
        assert_eq!(state.readiness(), VisualContextReadiness::ContextPending);
    }

    #[test]
    fn test_bidi_detection_within_budget() {
        let bidi_line = "let text = \"שלום עולם\";".as_bytes();
        let state = HugeLineVisualRouter::prepare_context(
            bidi_line,
            bidi_line.len() as u64,
            TabConfig::default(),
            ContextPreparationBudget::default(),
        );

        if let ContextPreparationState::Ready { is_pure_ascii, has_bidi, .. } = state {
            assert!(!is_pure_ascii);
            assert!(has_bidi);
        } else {
            assert!(false, "expected Ready with has_bidi=true");
        }
        assert_eq!(state.readiness(), VisualContextReadiness::ExactVisual);
    }

    #[test]
    fn test_excessive_combining_sequence_falls_back_to_logical() {
        let mut complex_str = String::from("e");
        // Add 40 combining accents (> 32)
        for _ in 0..40 {
            complex_str.push('\u{0301}');
        }
        let bytes = complex_str.as_bytes();
        let state = HugeLineVisualRouter::prepare_context(
            bytes,
            bytes.len() as u64,
            TabConfig::default(),
            ContextPreparationBudget::default(),
        );

        assert!(matches!(state, ContextPreparationState::LogicalFallback { .. }));
        assert_eq!(state.readiness(), VisualContextReadiness::LogicalEscaped);
    }

    #[test]
    fn test_materialize_visible_ascii_viewport() {
        let line = b"0123456789abcdefghij";
        let state = ContextPreparationState::Ready {
            is_pure_ascii: true,
            has_bidi: false,
            total_columns: 20,
        };

        // Window selecting columns 5..10 ("56789")
        let window = VisibleColumnWindow::new(5, 10);
        let runs = HugeLineVisualRouter::materialize_visible_runs(
            line,
            &state,
            window,
            TabConfig::default(),
        );

        assert!(!runs.is_empty());
        let total_text: String = runs.iter().map(|r| r.display_text.as_str()).collect();
        assert_eq!(total_text, "56789");
    }

    #[test]
    fn test_materialize_context_pending_viewport() {
        let line = b"partial bytes";
        let state = ContextPreparationState::Pending {
            scanned_bytes: 13,
            total_declared_bytes: HUGE_LINE_THRESHOLD_BYTES,
            reason: "huge line",
        };

        let window = VisibleColumnWindow::new(0, 50);
        let runs = HugeLineVisualRouter::materialize_visible_runs(
            line,
            &state,
            window,
            TabConfig::default(),
        );

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].readiness, VisualContextReadiness::ContextPending);
        assert!(runs[0].display_text.contains("CONTEXT PENDING"));
    }
}

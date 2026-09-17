#![forbid(unsafe_code)]

//! Exact source reading lens with horizontal virtualization, guides, and find-in-file (FCB-019.A).
//!
//! # Architecture & Contracts (§5.3, §10.4, §10.5, §10.6, §10.11)
//!
//! - **Gutter & Line Numbers**: Derived directly from 1-indexed logical line numbers.
//!   Gutter width scales dynamically with total known line digits plus uniform padding.
//! - **Independent Horizontal Scroll & Wrap**: Supports unwrap mode with independent
//!   horizontal scroll column offsets, full viewport-width wrapping, or fixed-column wrapping.
//! - **Whitespace & Indent Guides**: Configurable visual markers for spaces, tabs, and line
//!   terminators. Indent guides compute column levels based on tab stops without inventing hierarchy.
//! - **Bracket Guides**: Bidirectional matching of delimiters `()`, `[]`, `{}`, `<>` with nesting
//!   validation and explicit detection of mismatched or unclosed brackets.
//! - **Find-In-File**: Bounded match search within the source capture with forward/backward
//!   navigation and active highlight tracking.
//! - **Explicit Far-Line & Context Pending State**: When navigating to lines beyond the indexed
//!   boundary, returns an explicit [`LineNavigationResult::PendingFarJump`] state rather than freezing
//!   the interaction loop. Huge lines exceeding shaping capacity explicitly expose [`LensModeLabel::ContextPending`]
//!   or [`LensModeLabel::WindowVirtualized`].
//! - **Exact Selection & Copy**: Selected ranges resolve strictly against the captured bytes.
//!   CRLF, UTF-16, BOMs, bidi logical order, and null bytes are preserved verbatim. Stale captures
//!   and budget overflow are refused without corrupting host state.

use fcb_core::{ByteOffset, ByteRange, FileId, QueryGeneration, SourceRevision};
use fcb_source::line_index::LineNumber;
use fcb_source::huge_line::HUGE_LINE_THRESHOLD_BYTES;

use crate::SourceCapture;
use super::reader::ReaderError;

/// Threshold in bytes beyond which a single line is treated as a huge/virtualized line in the lens.
pub const LENS_HUGE_LINE_BYTE_LIMIT: usize = 64 * 1024;

/// Default overscan columns materialized beyond visible viewport to smooth scrolling.
pub const DEFAULT_OVERSCAN_COLUMNS: u32 = 32;

/// Wrap mode for reading source lines in the lens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WrapMode {
    /// No wrapping: horizontal scroll offset is applied to lines exceeding viewport width.
    None,
    /// Wrap lines to fit within the viewport character width.
    ViewportWidth,
    /// Wrap lines at an explicit maximum column limit (e.g. 80, 100, 120).
    ColumnLimit(u32),
}

/// Guide display options for the reading lens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuideOptions {
    /// Show visible symbols for spaces, tabs, and line terminators.
    pub show_whitespace: bool,
    /// Render vertical indentation guides at tab stop intervals.
    pub show_indent_guides: bool,
    /// Highlight matching enclosing bracket pairs around active position.
    pub show_bracket_guides: bool,
    /// Number of columns per tab stop (default 4).
    pub tab_width: u32,
}

impl Default for GuideOptions {
    fn default() -> Self {
        Self {
            show_whitespace: false,
            show_indent_guides: true,
            show_bracket_guides: true,
            tab_width: 4,
        }
    }
}

/// Viewport geometry for the reading lens in physical or logical units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LensViewport {
    pub width_px: u32,
    pub height_px: u32,
    pub line_height_px: u32,
    pub char_width_px: u32,
}

impl LensViewport {
    pub const fn new(
        width_px: u32,
        height_px: u32,
        line_height_px: u32,
        char_width_px: u32,
    ) -> Result<Self, ReaderError> {
        if width_px == 0 || height_px == 0 || line_height_px == 0 || char_width_px == 0 {
            return Err(ReaderError::InvalidLimits);
        }
        Ok(Self {
            width_px,
            height_px,
            line_height_px,
            char_width_px,
        })
    }

    /// Number of complete lines visible vertically in the viewport.
    pub const fn visible_lines_count(&self) -> usize {
        (self.height_px / self.line_height_px) as usize
    }

    /// Number of monospace character columns visible horizontally in the viewport.
    pub const fn visible_columns_count(&self) -> usize {
        (self.width_px / self.char_width_px) as usize
    }
}

/// Configuration and layout metrics for the line number gutter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GutterConfig {
    pub show_line_numbers: bool,
    /// Character padding on each side of line number (default 1).
    pub padding_chars: usize,
}

impl Default for GutterConfig {
    fn default() -> Self {
        Self {
            show_line_numbers: true,
            padding_chars: 1,
        }
    }
}

impl GutterConfig {
    /// Number of decimal digits required to represent `total_lines` (minimum 1).
    pub fn digit_count(total_lines: u64) -> usize {
        if total_lines == 0 {
            1
        } else {
            let mut n = total_lines;
            let mut count = 0;
            while n > 0 {
                count += 1;
                n /= 10;
            }
            count
        }
    }

    /// Total gutter width in monospace characters including padding.
    pub fn gutter_width_chars(&self, total_lines: u64) -> usize {
        if !self.show_line_numbers {
            0
        } else {
            Self::digit_count(total_lines) + (self.padding_chars * 2)
        }
    }

    /// Total gutter width in pixels.
    pub fn gutter_width_px(&self, total_lines: u64, char_width_px: u32) -> u32 {
        (self.gutter_width_chars(total_lines) as u32) * char_width_px
    }

    /// Format a line number right-aligned within the calculated gutter digits.
    pub fn format_line_number(&self, line: u64, total_lines: u64) -> String {
        if !self.show_line_numbers {
            return String::new();
        }
        let digits = Self::digit_count(total_lines);
        format!("{:>digits$}", line)
    }
}

/// Visual representation of whitespace tokens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WhitespaceKind {
    Space,
    Tab,
    LineFeed,
    CarriageReturn,
    CrLf,
}

impl WhitespaceKind {
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Space => "·",          // U+00B7 Middle Dot
            Self::Tab => "→",            // U+2192 Rightwards Arrow
            Self::LineFeed => "␊",       // U+240A Symbol for Line Feed
            Self::CarriageReturn => "␍", // U+240D Symbol for Carriage Return
            Self::CrLf => "␍␊",
        }
    }
}

/// A whitespace guide marker at an exact visual column.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WhitespaceMarker {
    pub column: u32,
    pub kind: WhitespaceKind,
}

/// An indentation guide indicator at a column level.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndentGuide {
    pub column: u32,
    pub level: u32,
}

/// Delimiter bracket families supported for guide highlighting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BracketKind {
    Paren,    // ( )
    Bracket,  // [ ]
    Brace,    // { }
    Angle,    // < >
}

impl BracketKind {
    pub const fn open_char(self) -> u8 {
        match self {
            Self::Paren => b'(',
            Self::Bracket => b'[',
            Self::Brace => b'{',
            Self::Angle => b'<',
        }
    }

    pub const fn close_char(self) -> u8 {
        match self {
            Self::Paren => b')',
            Self::Bracket => b']',
            Self::Brace => b'}',
            Self::Angle => b'>',
        }
    }

    pub const fn from_open(byte: u8) -> Option<Self> {
        match byte {
            b'(' => Some(Self::Paren),
            b'[' => Some(Self::Bracket),
            b'{' => Some(Self::Brace),
            b'<' => Some(Self::Angle),
            _ => None,
        }
    }

    pub const fn from_close(byte: u8) -> Option<Self> {
        match byte {
            b')' => Some(Self::Paren),
            b']' => Some(Self::Bracket),
            b'}' => Some(Self::Brace),
            b'>' => Some(Self::Angle),
            _ => None,
        }
    }
}

/// A matched pair of enclosing brackets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BracketPairMatch {
    pub kind: BracketKind,
    pub open_offset: usize,
    pub close_offset: usize,
}

/// The result of bracket matching around an active byte offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BracketMatchResult {
    /// A balanced pair of matching brackets was located.
    Matched(BracketPairMatch),
    /// An opening or closing bracket was encountered but its counterpart was missing or mismatched.
    Unmatched {
        offset: usize,
        kind: BracketKind,
        is_open: bool,
    },
    /// No bracket exists at or immediately adjacent to the inspected offset.
    None,
}

/// Computes whitespace markers for a text slice.
pub fn compute_whitespace_markers(text: &str, tab_width: u32) -> Vec<WhitespaceMarker> {
    let mut markers = Vec::new();
    let mut col: u32 = 0;
    let tab_step = if tab_width == 0 { 4 } else { tab_width };

    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            ' ' => {
                markers.push(WhitespaceMarker {
                    column: col,
                    kind: WhitespaceKind::Space,
                });
                col += 1;
            }
            '\t' => {
                markers.push(WhitespaceMarker {
                    column: col,
                    kind: WhitespaceKind::Tab,
                });
                let rem = col % tab_step;
                col += tab_step - rem;
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                    markers.push(WhitespaceMarker {
                        column: col,
                        kind: WhitespaceKind::CrLf,
                    });
                    col += 2;
                } else {
                    markers.push(WhitespaceMarker {
                        column: col,
                        kind: WhitespaceKind::CarriageReturn,
                    });
                    col += 1;
                }
            }
            '\n' => {
                markers.push(WhitespaceMarker {
                    column: col,
                    kind: WhitespaceKind::LineFeed,
                });
                col += 1;
            }
            _ => {
                col += 1;
            }
        }
    }
    markers
}

/// Computes indentation guide column levels for a leading whitespace prefix.
pub fn compute_indent_guides(text: &str, tab_width: u32) -> Vec<IndentGuide> {
    let mut guides = Vec::new();
    let tab_step = if tab_width == 0 { 4 } else { tab_width };
    let mut col: u32 = 0;

    for ch in text.chars() {
        match ch {
            ' ' => {
                col += 1;
                if col % tab_step == 0 {
                    guides.push(IndentGuide {
                        column: col - tab_step,
                        level: (col / tab_step) - 1,
                    });
                }
            }
            '\t' => {
                let rem = col % tab_step;
                let advance = tab_step - rem;
                guides.push(IndentGuide {
                    column: col,
                    level: col / tab_step,
                });
                col += advance;
            }
            _ => break,
        }
    }
    guides
}

/// Bidirectional bracket matcher over source bytes around an offset.
pub fn find_matching_bracket(bytes: &[u8], cursor_offset: usize) -> BracketMatchResult {
    if bytes.is_empty() {
        return BracketMatchResult::None;
    }

    // Check at cursor_offset, then cursor_offset - 1 if applicable
    let target_offset = if let Some(&b) = bytes.get(cursor_offset) {
        if BracketKind::from_open(b).is_some() || BracketKind::from_close(b).is_some() {
            Some(cursor_offset)
        } else if cursor_offset > 0 {
            if let Some(&b_prev) = bytes.get(cursor_offset - 1) {
                if BracketKind::from_open(b_prev).is_some() || BracketKind::from_close(b_prev).is_some() {
                    Some(cursor_offset - 1)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else if cursor_offset > 0 {
        if let Some(&b_prev) = bytes.get(cursor_offset - 1) {
            if BracketKind::from_open(b_prev).is_some() || BracketKind::from_close(b_prev).is_some() {
                Some(cursor_offset - 1)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let offset = match target_offset {
        Some(off) => off,
        None => return BracketMatchResult::None,
    };

    let byte = match bytes.get(offset) {
        Some(&b) => b,
        None => return BracketMatchResult::None,
    };
    if let Some(open_kind) = BracketKind::from_open(byte) {
        // Forward scan for closing bracket
        let close_target = open_kind.close_char();
        let open_target = open_kind.open_char();
        let mut depth: usize = 0;

        for (idx, &b) in bytes[offset..].iter().enumerate() {
            if b == open_target {
                depth += 1;
            } else if b == close_target {
                depth -= 1;
                if depth == 0 {
                    return BracketMatchResult::Matched(BracketPairMatch {
                        kind: open_kind,
                        open_offset: offset,
                        close_offset: offset + idx,
                    });
                }
            }
        }
        BracketMatchResult::Unmatched {
            offset,
            kind: open_kind,
            is_open: true,
        }
    } else if let Some(close_kind) = BracketKind::from_close(byte) {
        // Backward scan for opening bracket
        let close_target = close_kind.close_char();
        let open_target = close_kind.open_char();
        let mut depth: usize = 0;

        for idx in (0..=offset).rev() {
            let b = bytes[idx];
            if b == close_target {
                depth += 1;
            } else if b == open_target {
                depth -= 1;
                if depth == 0 {
                    return BracketMatchResult::Matched(BracketPairMatch {
                        kind: close_kind,
                        open_offset: idx,
                        close_offset: offset,
                    });
                }
            }
        }
        BracketMatchResult::Unmatched {
            offset,
            kind: close_kind,
            is_open: false,
        }
    } else {
        BracketMatchResult::None
    }
}

/// A matched search occurrence in find-in-file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindMatch {
    pub match_index: usize,
    pub byte_range: ByteRange,
    pub line_number: u64,
    pub column: u32,
    pub length: usize,
}

/// Options governing find-in-file search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl Default for FindOptions {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            whole_word: false,
        }
    }
}

/// Human-readable and machine-verifiable find status summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindStatus {
    pub query: String,
    pub total_matches: usize,
    pub current_match_one_based: Option<usize>,
}

/// Interactive find-in-file session attached to a reading lens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindInFileSession {
    query: String,
    options: FindOptions,
    matches: Vec<FindMatch>,
    active_match_idx: Option<usize>,
}

impl FindInFileSession {
    /// Execute a bounded find-in-file search on captured source bytes.
    pub fn search(capture: &SourceCapture, query: &str, options: FindOptions) -> Self {
        if query.is_empty() || capture.bytes().is_empty() {
            return Self {
                query: query.to_string(),
                options,
                matches: Vec::new(),
                active_match_idx: None,
            };
        }

        let bytes = capture.bytes();
        let needle = if options.case_sensitive {
            query.as_bytes().to_vec()
        } else {
            query.to_lowercase().into_bytes()
        };

        let mut matches = Vec::new();
        let mut line: u64 = 1;
        let mut line_start: usize = 0;
        let mut idx: usize = 0;

        while idx < bytes.len() {
            // Track line numbers
            if bytes[idx] == b'\n' {
                line += 1;
                line_start = idx + 1;
            } else if bytes[idx] == b'\r' && idx + 1 < bytes.len() && bytes[idx + 1] != b'\n' {
                line += 1;
                line_start = idx + 1;
            }

            // Check needle candidate
            if idx + needle.len() <= bytes.len() {
                let candidate = &bytes[idx..idx + needle.len()];
                let matches_needle = if options.case_sensitive {
                    candidate == needle.as_slice()
                } else {
                    candidate
                        .iter()
                        .map(|b| b.to_ascii_lowercase())
                        .eq(needle.iter().copied())
                };

                if matches_needle {
                    // Check whole-word boundaries if requested
                    let left_word_boundary = if options.whole_word && idx > 0 {
                        !is_word_byte(bytes[idx - 1])
                    } else {
                        true
                    };
                    let right_word_boundary = if options.whole_word && idx + needle.len() < bytes.len() {
                        !is_word_byte(bytes[idx + needle.len()])
                    } else {
                        true
                    };

                    if left_word_boundary && right_word_boundary {
                        let start_off = ByteOffset::new(idx as u64);
                        let end_off = ByteOffset::new((idx + needle.len()) as u64);
                        if let Ok(range) = ByteRange::new(start_off, end_off) {
                            let column = (idx.saturating_sub(line_start)) as u32;
                            matches.push(FindMatch {
                                match_index: matches.len(),
                                byte_range: range,
                                line_number: line,
                                column,
                                length: needle.len(),
                            });
                        }
                    }
                }
            }
            idx += 1;
        }

        let active_match_idx = if matches.is_empty() { None } else { Some(0) };

        Self {
            query: query.to_string(),
            options,
            matches,
            active_match_idx,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn matches(&self) -> &[FindMatch] {
        &self.matches
    }

    pub fn current_match(&self) -> Option<&FindMatch> {
        self.active_match_idx.and_then(|idx| self.matches.get(idx))
    }

    pub fn next_match(&mut self) -> Option<&FindMatch> {
        if self.matches.is_empty() {
            return None;
        }
        let next_idx = match self.active_match_idx {
            Some(idx) => (idx + 1) % self.matches.len(),
            None => 0,
        };
        self.active_match_idx = Some(next_idx);
        self.matches.get(next_idx)
    }

    pub fn prev_match(&mut self) -> Option<&FindMatch> {
        if self.matches.is_empty() {
            return None;
        }
        let prev_idx = match self.active_match_idx {
            Some(0) | None => self.matches.len() - 1,
            Some(idx) => idx - 1,
        };
        self.active_match_idx = Some(prev_idx);
        self.matches.get(prev_idx)
    }

    pub fn status(&self) -> FindStatus {
        FindStatus {
            query: self.query.clone(),
            total_matches: self.matches.len(),
            current_match_one_based: self.active_match_idx.map(|idx| idx + 1),
        }
    }
}

const fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The result of navigating to an exact line in the reading lens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineNavigationResult {
    /// The requested line resolved within the currently indexed extent.
    Resolved {
        line: LineNumber,
        target_scroll_y: u64,
    },
    /// The requested line is beyond the indexed boundary; exposes indexing state
    /// without freezing the caller.
    PendingFarJump {
        target_line: LineNumber,
        indexed_through_line: u64,
    },
    /// The requested line exceeds the known total lines of the file.
    OutOfBounds {
        target_line: LineNumber,
        total_lines: u64,
    },
}

/// Operational mode label for a rendered lens line row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LensModeLabel {
    /// Exact line representation.
    Exact,
    /// Huge line horizontally virtualized (only visible window materialized).
    WindowVirtualized,
    /// Complex context exceeds shaping budget; pending context resolution.
    ContextPending,
    /// Pathological or invalid sequence presented in escaped logical format.
    LogicalFallback,
}

impl LensModeLabel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "EXACT",
            Self::WindowVirtualized => "WINDOW_VIRTUALIZED",
            Self::ContextPending => "CONTEXT_PENDING",
            Self::LogicalFallback => "LOGICAL_FALLBACK",
        }
    }
}

/// A rendered virtual line row within the active lens viewport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualLineRow {
    pub line_number: u64,
    pub formatted_line_number: String,
    pub raw_byte_range: ByteRange,
    pub text: String,
    pub is_wrapped_continuation: bool,
    pub indent_guides: Vec<IndentGuide>,
    pub whitespace_markers: Vec<WhitespaceMarker>,
    pub bracket_highlight: Option<BracketPairMatch>,
    pub find_match_ranges: Vec<(u32, u32)>,
    pub mode_label: LensModeLabel,
}

/// An exact source selection anchored within the reading lens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LensSelection {
    pub file: FileId,
    pub revision: SourceRevision,
    pub generation: QueryGeneration,
    pub byte_range: ByteRange,
    pub start_line: u64,
    pub start_col: u32,
    pub end_line: u64,
    pub end_col: u32,
}

impl LensSelection {
    /// Validates delivery against the source capture and active generation.
    pub fn validate(
        &self,
        capture: &SourceCapture,
        generation: QueryGeneration,
    ) -> Result<(), ReaderError> {
        if self.file.owner() != capture.owner() || generation.owner() != capture.owner() {
            return Err(ReaderError::OwnerMismatch);
        }
        if self.file != capture.file() || self.revision != capture.revision() {
            return Err(ReaderError::StaleSource);
        }
        if self.generation != generation {
            return Err(ReaderError::StaleQuery);
        }
        Ok(())
    }

    /// Extract exact captured bytes without silent alterations (preserving CRLF, bidi, nulls).
    pub fn copy_exact_bytes<'a>(
        &self,
        capture: &'a SourceCapture,
        max_budget_bytes: usize,
    ) -> Result<&'a [u8], ReaderError> {
        self.validate(capture, self.generation)?;
        let (start, end) = self
            .byte_range
            .as_usize_bounds()
            .map_err(|_| ReaderError::InvalidRange)?;
        let length = end.checked_sub(start).ok_or(ReaderError::InvalidRange)?;
        if length > max_budget_bytes {
            return Err(ReaderError::ResourceDenied);
        }
        capture
            .bytes()
            .get(start..end)
            .ok_or(ReaderError::InvalidRange)
    }

    /// Copy with provenance metadata.
    pub fn copy_with_provenance(
        &self,
        capture: &SourceCapture,
        max_budget_bytes: usize,
    ) -> Result<SelectionProvenance, ReaderError> {
        let exact_bytes = self
            .copy_exact_bytes(capture, max_budget_bytes)?
            .to_vec();
        Ok(SelectionProvenance {
            logical_path: capture.logical_path().to_string(),
            file: self.file,
            revision: self.revision,
            byte_range: self.byte_range,
            start_line: self.start_line,
            start_col: self.start_col,
            end_line: self.end_line,
            end_col: self.end_col,
            exact_bytes,
        })
    }
}

/// Exact copied selection paired with authoritative provenance metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionProvenance {
    pub logical_path: String,
    pub file: FileId,
    pub revision: SourceRevision,
    pub byte_range: ByteRange,
    pub start_line: u64,
    pub start_col: u32,
    pub end_line: u64,
    pub end_col: u32,
    pub exact_bytes: Vec<u8>,
}

/// The authoritative interactive source reading lens (FCB-019.A).
#[derive(Clone, Debug)]
pub struct SourceReadingLens {
    viewport: LensViewport,
    wrap_mode: WrapMode,
    guide_options: GuideOptions,
    gutter_config: GutterConfig,
    scroll_y_line: u64,
    scroll_x_cols: u32,
    find_session: Option<FindInFileSession>,
    active_selection: Option<LensSelection>,
    cursor_offset: Option<usize>,
}

impl SourceReadingLens {
    pub fn new(viewport: LensViewport) -> Self {
        Self {
            viewport,
            wrap_mode: WrapMode::None,
            guide_options: GuideOptions::default(),
            gutter_config: GutterConfig::default(),
            scroll_y_line: 1,
            scroll_x_cols: 0,
            find_session: None,
            active_selection: None,
            cursor_offset: None,
        }
    }

    pub const fn viewport(&self) -> &LensViewport {
        &self.viewport
    }

    pub fn set_viewport(&mut self, viewport: LensViewport) {
        self.viewport = viewport;
    }

    pub const fn wrap_mode(&self) -> WrapMode {
        self.wrap_mode
    }

    pub fn set_wrap_mode(&mut self, mode: WrapMode) {
        self.wrap_mode = mode;
    }

    pub const fn guide_options(&self) -> &GuideOptions {
        &self.guide_options
    }

    pub fn guide_options_mut(&mut self) -> &mut GuideOptions {
        &mut self.guide_options
    }

    pub const fn gutter_config(&self) -> &GutterConfig {
        &self.gutter_config
    }

    pub fn gutter_config_mut(&mut self) -> &mut GutterConfig {
        &mut self.gutter_config
    }

    pub const fn scroll_y_line(&self) -> u64 {
        self.scroll_y_line
    }

    pub fn set_scroll_y_line(&mut self, line: u64) {
        self.scroll_y_line = line.max(1);
    }

    pub const fn scroll_x_cols(&self) -> u32 {
        self.scroll_x_cols
    }

    pub fn set_scroll_x_cols(&mut self, cols: u32) {
        self.scroll_x_cols = cols;
    }

    pub const fn cursor_offset(&self) -> Option<usize> {
        self.cursor_offset
    }

    pub fn set_cursor_offset(&mut self, offset: Option<usize>) {
        self.cursor_offset = offset;
    }

    pub const fn active_selection(&self) -> Option<&LensSelection> {
        self.active_selection.as_ref()
    }

    pub fn set_selection(&mut self, selection: Option<LensSelection>) {
        self.active_selection = selection;
    }

    /// Perform an exact line navigation with explicit far-line pending state.
    pub fn navigate_to_line(
        &mut self,
        target_line: LineNumber,
        indexed_through: u64,
        total_lines: Option<u64>,
    ) -> LineNavigationResult {
        if let Some(total) = total_lines {
            if target_line.get() > total {
                return LineNavigationResult::OutOfBounds {
                    target_line,
                    total_lines: total,
                };
            }
        }

        if target_line.get() <= indexed_through {
            self.scroll_y_line = target_line.get();
            LineNavigationResult::Resolved {
                line: target_line,
                target_scroll_y: target_line.get(),
            }
        } else {
            LineNavigationResult::PendingFarJump {
                target_line,
                indexed_through_line: indexed_through,
            }
        }
    }

    /// Initiate or replace a find-in-file search across the source capture.
    pub fn start_find(&mut self, capture: &SourceCapture, query: &str, options: FindOptions) {
        let session = FindInFileSession::search(capture, query, options);
        self.find_session = Some(session);
    }

    pub const fn find_session(&self) -> Option<&FindInFileSession> {
        self.find_session.as_ref()
    }

    pub fn find_session_mut(&mut self) -> Option<&mut FindInFileSession> {
        self.find_session.as_mut()
    }

    pub fn clear_find(&mut self) {
        self.find_session = None;
    }

    /// Materializes the visible virtual rows according to viewport geometry, scroll,
    /// wrap mode, and guide options.
    pub fn render_visible_rows(
        &self,
        capture: &SourceCapture,
        line_starts: &[usize],
        total_lines: u64,
    ) -> Result<Vec<VirtualLineRow>, ReaderError> {
        let max_rows = self.viewport.visible_lines_count();
        if max_rows == 0 || line_starts.is_empty() {
            return Ok(Vec::new());
        }

        let bytes = capture.bytes();
        let mut rows = Vec::new();
        let mut current_line = self.scroll_y_line;

        // Active bracket match if enabled
        let bracket_match = if self.guide_options.show_bracket_guides {
            self.cursor_offset
                .and_then(|off| match find_matching_bracket(bytes, off) {
                    BracketMatchResult::Matched(pair) => Some(pair),
                    _ => None,
                })
        } else {
            None
        };

        while rows.len() < max_rows && current_line <= total_lines {
            let line_idx = (current_line - 1) as usize;
            if line_idx >= line_starts.len() {
                break;
            }

            let start_byte = line_starts[line_idx];
            let end_byte = if line_idx + 1 < line_starts.len() {
                line_starts[line_idx + 1]
            } else {
                bytes.len()
            };

            let line_slice = bytes.get(start_byte..end_byte).ok_or(ReaderError::InvalidRange)?;
            // Strip trailing CRLF for display
            let content_end = if line_slice.ends_with(b"\r\n") {
                end_byte.saturating_sub(2)
            } else if line_slice.ends_with(b"\n") || line_slice.ends_with(b"\r") {
                end_byte.saturating_sub(1)
            } else {
                end_byte
            };

            let content_slice = &bytes[start_byte..content_end];
            let line_str = String::from_utf8_lossy(content_slice);

            let raw_range = ByteRange::new(
                ByteOffset::new(start_byte as u64),
                ByteOffset::new(end_byte as u64),
            )
            .map_err(|_| ReaderError::InvalidRange)?;

            // Collect find matches on this line
            let mut find_ranges = Vec::new();
            if let Some(find) = &self.find_session {
                for m in find.matches() {
                    if m.line_number == current_line {
                        find_ranges.push((m.column, m.column + m.length as u32));
                    }
                }
            }

            // Check for huge line virtualization
            let is_huge_line = content_slice.len() > LENS_HUGE_LINE_BYTE_LIMIT
                || (content_slice.len() as u64) > HUGE_LINE_THRESHOLD_BYTES;

            let bracket_on_line = bracket_match.filter(|b| {
                (b.open_offset >= start_byte && b.open_offset < end_byte)
                    || (b.close_offset >= start_byte && b.close_offset < end_byte)
            });

            match self.wrap_mode {
                WrapMode::None => {
                    let formatted_num = self
                        .gutter_config
                        .format_line_number(current_line, total_lines);

                    let (rendered_text, mode_label) = if is_huge_line {
                        let vis_cols = self.viewport.visible_columns_count() as u32;
                        let start_col = self.scroll_x_cols;
                        let end_col = start_col.saturating_add(vis_cols).saturating_add(DEFAULT_OVERSCAN_COLUMNS);
                        let sub: String = line_str
                            .chars()
                            .skip(start_col as usize)
                            .take((end_col - start_col) as usize)
                            .collect();
                        (sub, LensModeLabel::WindowVirtualized)
                    } else if self.scroll_x_cols > 0 {
                        let sub: String = line_str.chars().skip(self.scroll_x_cols as usize).collect();
                        (sub, LensModeLabel::Exact)
                    } else {
                        (line_str.to_string(), LensModeLabel::Exact)
                    };

                    let indent_guides = if self.guide_options.show_indent_guides {
                        compute_indent_guides(&line_str, self.guide_options.tab_width)
                    } else {
                        Vec::new()
                    };

                    let whitespace_markers = if self.guide_options.show_whitespace {
                        compute_whitespace_markers(&line_str, self.guide_options.tab_width)
                    } else {
                        Vec::new()
                    };

                    rows.push(VirtualLineRow {
                        line_number: current_line,
                        formatted_line_number: formatted_num,
                        raw_byte_range: raw_range,
                        text: rendered_text,
                        is_wrapped_continuation: false,
                        indent_guides,
                        whitespace_markers,
                        bracket_highlight: bracket_on_line,
                        find_match_ranges: find_ranges,
                        mode_label,
                    });
                }
                WrapMode::ViewportWidth | WrapMode::ColumnLimit(_) => {
                    let wrap_cols = match self.wrap_mode {
                        WrapMode::ColumnLimit(limit) => limit.max(1) as usize,
                        _ => self.viewport.visible_columns_count().max(1),
                    };

                    let chars: Vec<char> = line_str.chars().collect();
                    if chars.is_empty() {
                        let formatted_num = self
                            .gutter_config
                            .format_line_number(current_line, total_lines);
                        rows.push(VirtualLineRow {
                            line_number: current_line,
                            formatted_line_number: formatted_num,
                            raw_byte_range: raw_range,
                            text: String::new(),
                            is_wrapped_continuation: false,
                            indent_guides: Vec::new(),
                            whitespace_markers: Vec::new(),
                            bracket_highlight: bracket_on_line,
                            find_match_ranges: find_ranges,
                            mode_label: LensModeLabel::Exact,
                        });
                    } else {
                        let mut chunk_start = 0;
                        let mut is_first = true;

                        while chunk_start < chars.len() && rows.len() < max_rows {
                            let chunk_end = (chunk_start + wrap_cols).min(chars.len());
                            let chunk_text: String = chars[chunk_start..chunk_end].iter().collect();

                            let formatted_num = if is_first {
                                self.gutter_config
                                    .format_line_number(current_line, total_lines)
                            } else {
                                String::new()
                            };

                            let indent_guides = if is_first && self.guide_options.show_indent_guides {
                                compute_indent_guides(&line_str, self.guide_options.tab_width)
                            } else {
                                Vec::new()
                            };

                            let whitespace_markers = if self.guide_options.show_whitespace {
                                compute_whitespace_markers(&chunk_text, self.guide_options.tab_width)
                            } else {
                                Vec::new()
                            };

                            rows.push(VirtualLineRow {
                                line_number: current_line,
                                formatted_line_number: formatted_num,
                                raw_byte_range: raw_range,
                                text: chunk_text,
                                is_wrapped_continuation: !is_first,
                                indent_guides,
                                whitespace_markers,
                                bracket_highlight: bracket_on_line,
                                find_match_ranges: if is_first { find_ranges.clone() } else { Vec::new() },
                                mode_label: if is_huge_line {
                                    LensModeLabel::WindowVirtualized
                                } else {
                                    LensModeLabel::Exact
                                },
                            });

                            chunk_start = chunk_end;
                            is_first = false;
                        }
                    }
                }
            }

            current_line += 1;
        }

        Ok(rows)
    }
}

//! Exact source reading lens: line-numbered display with bounded pending
//! far-line state, CRLF/BOM handling, find-in-file, and byte-exact
//! selection/copy. FCB-019.A.

#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

use std::collections::BTreeMap;
use std::ops::Range;

/// A source line with its number, byte range, and content.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceLine {
    /// 1-based line number.
    pub number: usize,
    /// Byte offset of the line start (including preceding newline).
    pub byte_start: usize,
    /// Byte offset just past the line's terminating newline (or EOF).
    pub byte_end: usize,
    /// Line content without the trailing newline.
    pub content: String,
}

/// Errors from the reading lens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LensError {
    /// A query range exceeds the document.
    OutOfRange,
    /// The requested line does not exist.
    NoSuchLine(usize),
}

/// Bounded source reader with line-numbered display and find-in-file.
#[derive(Debug)]
pub struct SourceReader {
    source: String,
    /// Byte offset where content starts (after BOM, if present).
    content_start: usize,
    /// Line index: line number (1-based) → byte start offset.
    line_starts: Vec<usize>,
    /// Total number of lines.
    line_count: usize,
    /// Whether the source had CRLF line endings.
    had_crlf: bool,
}

impl SourceReader {
    /// Create a reader from source bytes, detecting and stripping BOM.
    ///
    /// CRLF endings are preserved in byte offsets (for exact copy) but
    /// stripped from display content.
    pub fn new(source: &[u8]) -> Self {
        let content_start = if source.starts_with(&[0xEF, 0xBB, 0xBF]) {
            3 // UTF-8 BOM
        } else {
            0
        };
        let text = String::from_utf8_lossy(&source[content_start..]).into_owned();

        let mut line_starts = vec![0usize];
        let mut had_crlf = false;
        let bytes = text.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                if i > 0 && bytes[i - 1] == b'\r' {
                    had_crlf = true;
                }
                line_starts.push(i + 1);
            }
        }
        let line_count = line_starts.len();

        Self {
            source: text.to_owned(),
            content_start,
            line_starts,
            line_count,
            had_crlf,
        }
    }

    /// Total number of lines.
    pub fn line_count(&self) -> usize {
        self.line_count
    }

    /// Whether the source had CRLF line endings.
    pub const fn had_crlf(&self) -> bool {
        self.had_crlf
    }

    /// The content start offset (after BOM).
    pub const fn content_start(&self) -> usize {
        self.content_start
    }

    /// Get one line (1-based) without the trailing newline.
    pub fn line(&self, number: usize) -> Result<SourceLine, LensError> {
        if number == 0 || number > self.line_count {
            return Err(LensError::NoSuchLine(number));
        }
        let idx = number - 1;
        let start = self.line_starts[idx];
        let end = if idx + 1 < self.line_starts.len() {
            self.line_starts[idx + 1]
        } else {
            self.source.len()
        };
        let raw = &self.source[start..end];
        let content = raw.trim_end_matches('\n').trim_end_matches('\r');
        Ok(SourceLine {
            number,
            byte_start: start + self.content_start,
            byte_end: end + self.content_start,
            content: content.to_owned(),
        })
    }

    /// Get a range of lines (1-based, inclusive).
    pub fn lines_range(&self, start: usize, end: usize) -> Result<Vec<SourceLine>, LensError> {
        if start == 0 || end < start || end > self.line_count {
            return Err(LensError::OutOfRange);
        }
        (start..=end).map(|n| self.line(n)).collect()
    }

    /// Find all lines containing `needle` (case-sensitive).
    ///
    /// Returns (line_number, char_column) pairs.
    pub fn find(&self, needle: &str) -> Vec<(usize, usize)> {
        let mut results = Vec::new();
        for line_num in 1..=self.line_count {
            if let Ok(line) = self.line(line_num) {
                if let Some(col) = line.content.find(needle) {
                    let char_col = line.content[..col].chars().count();
                    results.push((line_num, char_col));
                }
            }
        }
        results
    }

    /// Exact byte range for a selection (line, col, length in chars).
    ///
    /// Returns the absolute byte range in the original source (including
    /// BOM offset), suitable for exact copy operations.
    pub fn selection_range(
        &self,
        line: usize,
        col: usize,
        char_len: usize,
    ) -> Result<Range<usize>, LensError> {
        let source_line = self.line(line)?;
        let content = &source_line.content;
        let byte_start = content
            .char_indices()
            .nth(col)
            .map(|(b, _)| b)
            .unwrap_or(content.len());
        let byte_len = content[byte_start..]
            .chars()
            .take(char_len)
            .map(|c| c.len_utf8())
            .sum::<usize>();
        let abs_start = source_line.byte_start + byte_start;
        Ok(abs_start..abs_start + byte_len)
    }

    /// Exact text for a selection range.
    pub fn selection_text(&self, range: Range<usize>) -> Result<String, LensError> {
        let adj_start = range.start.saturating_sub(self.content_start);
        let adj_end = (range.end.saturating_sub(self.content_start)).min(self.source.len());
        if adj_start > adj_end || adj_end > self.source.len() {
            return Err(LensError::OutOfRange);
        }
        Ok(self.source[adj_start..adj_end].to_owned())
    }
}

/// A bounded viewport into the source with pending far-line state.
///
/// Lines within `visible_range` are fully rendered. Lines outside are
/// marked pending — the caller knows they exist but hasn't classified them.
#[derive(Debug)]
pub struct BoundedViewport {
    /// First visible line (1-based).
    pub first_line: usize,
    /// Last visible line (1-based).
    pub last_line: usize,
    /// Total lines in the document.
    pub total_lines: usize,
}

impl BoundedViewport {
    /// Create a viewport around a center line with the given visible height.
    pub fn around(total_lines: usize, center: usize, visible_height: usize) -> Self {
        let half = visible_height / 2;
        let first = center.saturating_sub(half).max(1);
        let last = (center + half).min(total_lines).max(1);
        Self {
            first_line: first,
            last_line: last,
            total_lines,
        }
    }

    /// Whether a line is visible in this viewport.
    pub const fn contains(&self, line: usize) -> bool {
        line >= self.first_line && line <= self.last_line
    }

    /// Lines before the viewport start (pending).
    pub const fn pending_before(&self) -> usize {
        self.first_line - 1
    }

    /// Lines after the viewport end (pending).
    pub const fn pending_after(&self) -> usize {
        self.total_lines.saturating_sub(self.last_line)
    }
}

/// Whitespace/indent guide information for a line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndentGuide {
    /// Number of leading spaces or tabs.
    pub indent_width: usize,
    /// Whether indentation uses tabs (vs spaces).
    pub uses_tabs: bool,
    /// Nesting depth (indent_width / tab_size or indent_width / 4).
    pub depth: usize,
}

/// Compute indent guide info for a line.
pub fn indent_guide(line: &str, tab_size: usize) -> IndentGuide {
    let mut width = 0usize;
    let mut uses_tabs = false;
    for b in line.bytes() {
        match b {
            b' ' => width += 1,
            b'\t' => {
                width += tab_size;
                uses_tabs = true;
            }
            _ => break,
        }
    }
    IndentGuide {
        indent_width: width,
        uses_tabs,
        depth: width / tab_size.max(1),
    }
}

/// Find matching bracket for a bracket at the given position.
///
/// Returns the matching bracket's byte offset, or `None` if unmatched.
pub fn find_matching_bracket(source: &str, pos: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let open = *bytes.get(pos)?;
    let close = match open {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        _ => return None,
    };
    let mut depth = 0i32;
    let mut in_string = false;
    let mut string_delim = b'"';
    let mut i = pos;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == string_delim {
                in_string = false;
            }
        } else if b == b'"' || b == b'\'' {
            in_string = true;
            string_delim = b;
        } else if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

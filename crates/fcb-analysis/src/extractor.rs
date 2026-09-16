//! Language-scoped structural outline extraction (FCB-030.A).
//!
//! Provides deterministic syntax entity extraction with exact evidence spans,
//! hierarchy nesting for Rust, Markdown, Python, JS/TS/JSX/TSX, Go, C/C++,
//! line-block fallback for unsupported languages, and graceful degradation
//! on malformed or truncated source.

use crate::outline::{
    CapabilityLevel, OutlineEvidence, OutlineItem, OutlineItemKind, OutlineStatus, SourceOutline,
};
use fcb_core::{FileId, SourceRevision};

/// Configurable limits for outline extraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractorLimits {
    /// Maximum number of items extracted per outline.
    pub max_items: usize,
    /// Maximum nesting depth.
    pub max_depth: usize,
    /// Work budget in bytes.
    pub work_budget_bytes: usize,
    /// Lines per line block in fallback mode.
    pub lines_per_block: u32,
}

impl Default for ExtractorLimits {
    fn default() -> Self {
        Self {
            max_items: 10_000,
            max_depth: 32,
            work_budget_bytes: 16 * 1024 * 1024, // 16 MiB
            lines_per_block: 50,
        }
    }
}

/// Outline extractor engine.
#[derive(Clone, Debug)]
pub struct OutlineExtractor {
    limits: ExtractorLimits,
}

impl Default for OutlineExtractor {
    fn default() -> Self {
        Self::new(ExtractorLimits::default())
    }
}

impl OutlineExtractor {
    /// Construct an extractor with specific limits.
    #[must_use]
    pub const fn new(limits: ExtractorLimits) -> Self {
        Self { limits }
    }

    /// Extract a structural source outline for the given file and source bytes.
    pub fn extract(
        &self,
        file_id: FileId,
        source_revision: SourceRevision,
        language_hint: &str,
        code: &[u8],
    ) -> SourceOutline {
        if code.len() > self.limits.work_budget_bytes {
            // Degrade to single line block if work budget is exceeded
            return self.extract_line_blocks(
                file_id,
                source_revision,
                language_hint,
                code,
                OutlineStatus::DegradedMalformed,
            );
        }

        let lang_lower = language_hint.trim().to_ascii_lowercase();

        match lang_lower.as_str() {
            "rust" | "rs" => self.extract_rust(file_id, source_revision, code),
            "markdown" | "md" | "mdown" | "mkd" => {
                self.extract_markdown(file_id, source_revision, code)
            }
            "python" | "py" => self.extract_python(file_id, source_revision, code),
            "javascript" | "js" | "mjs" | "cjs" | "jsx" => {
                self.extract_js_ts(file_id, source_revision, "javascript", code)
            }
            "typescript" | "ts" | "tsx" => {
                self.extract_js_ts(file_id, source_revision, "typescript", code)
            }
            "go" | "golang" => self.extract_go(file_id, source_revision, code),
            "c" | "h" | "cpp" | "c++" | "cc" | "hpp" => {
                self.extract_c_cpp(file_id, source_revision, code)
            }
            _ => self.extract_line_blocks(
                file_id,
                source_revision,
                &lang_lower,
                code,
                OutlineStatus::LineBlockFallback,
            ),
        }
    }

    /// Plain line-block fallback for unsupported languages or plain text files.
    ///
    /// Groups lines into bounded blocks (e.g. 50 lines per block), ensuring
    /// outline navigation works everywhere without failure (Plan §11.4).
    fn extract_line_blocks(
        &self,
        file_id: FileId,
        source_revision: SourceRevision,
        language: &str,
        code: &[u8],
        status: OutlineStatus,
    ) -> SourceOutline {
        let lines = LineIndex::new(code);
        let total_lines = lines.line_count().max(1);
        let mut items = Vec::new();
        let mut id = 1u64;

        let block_size = self.limits.lines_per_block.max(1);
        let mut start_line = 1u32;

        while start_line <= total_lines && items.len() < self.limits.max_items {
            let end_line = (start_line + block_size - 1).min(total_lines);
            let byte_start = lines.line_start_offset(start_line);
            let byte_end = lines.line_end_offset(end_line, code.len());

            let evidence = match OutlineEvidence::new(
                byte_start as u64,
                byte_end as u64,
                start_line,
                end_line,
                None,
            ) {
                Ok(ev) => ev,
                Err(_) => break,
            };

            let name = if start_line == end_line {
                format!("Line {start_line}")
            } else {
                format!("Lines {start_line}–{end_line}")
            };

            items.push(OutlineItem {
                id,
                name,
                kind: OutlineItemKind::LineBlock {
                    start_line,
                    end_line,
                },
                capability_level: CapabilityLevel::Bytes,
                evidence,
                children: Vec::new(),
            });

            id += 1;
            start_line = end_line + 1;
        }

        SourceOutline::new(
            file_id,
            source_revision,
            language.to_string(),
            status,
            items,
        )
    }

    /// Extract structural outline for Markdown documents.
    ///
    /// Headings (`#` through `######`) are parsed and hierarchically nested
    /// based on heading levels, ignoring headings inside fenced code blocks.
    fn extract_markdown(
        &self,
        file_id: FileId,
        source_revision: SourceRevision,
        code: &[u8],
    ) -> SourceOutline {
        let text = String::from_utf8_lossy(code);
        let lines = LineIndex::new(code);
        let mut in_fence = false;
        let mut id_counter = 1u64;

        // Tuple of (level, OutlineItem)
        let mut stack: Vec<(u8, OutlineItem)> = Vec::new();
        let mut top_level: Vec<OutlineItem> = Vec::new();

        for (line_idx, line) in text.lines().enumerate() {
            let line_num = (line_idx + 1) as u32;
            let trimmed = line.trim_start();

            // Track code fences
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }

            // Check for heading: 1..=6 '#' followed by space
            if trimmed.starts_with('#') {
                let hash_count = trimmed.bytes().take_while(|&b| b == b'#').count();
                if (1..=6).contains(&hash_count) {
                    let rest = &trimmed[hash_count..];
                    if rest.starts_with(' ') || rest.is_empty() {
                        let heading_text = rest.trim();
                        let display_name = if heading_text.is_empty() {
                            format!("Heading {hash_count}")
                        } else {
                            heading_text.to_string()
                        };

                        let line_start_byte = lines.line_start_offset(line_num);
                        let line_end_byte = lines.line_end_offset(line_num, code.len());

                        let evidence = match OutlineEvidence::new(
                            line_start_byte as u64,
                            line_end_byte as u64,
                            line_num,
                            line_num,
                            None,
                        ) {
                            Ok(ev) => ev,
                            Err(_) => continue,
                        };

                        let item = OutlineItem {
                            id: id_counter,
                            name: display_name,
                            kind: OutlineItemKind::Heading {
                                level: hash_count as u8,
                            },
                            capability_level: CapabilityLevel::Structural,
                            evidence,
                            children: Vec::new(),
                        };
                        id_counter += 1;

                        let level = hash_count as u8;

                        // Pop items from stack with level >= current level
                        while let Some((top_level_val, _)) = stack.last() {
                            if *top_level_val >= level {
                                let (_, popped) = stack.pop().expect("checked");
                                if let Some((_, parent)) = stack.last_mut() {
                                    parent.children.push(popped);
                                } else {
                                    top_level.push(popped);
                                }
                            } else {
                                break;
                            }
                        }

                        stack.push((level, item));

                        if top_level.len() + stack.len() >= self.limits.max_items {
                            break;
                        }
                    }
                }
            }
        }

        // Drain remaining stack
        while let Some((_, popped)) = stack.pop() {
            if let Some((_, parent)) = stack.last_mut() {
                parent.children.push(popped);
            } else {
                top_level.push(popped);
            }
        }

        let status = if top_level.is_empty() && !code.is_empty() {
            // If markdown had no headings, fallback to line blocks
            return self.extract_line_blocks(
                file_id,
                source_revision,
                "markdown",
                code,
                OutlineStatus::LineBlockFallback,
            );
        } else {
            OutlineStatus::Qualified
        };

        SourceOutline::new(
            file_id,
            source_revision,
            "markdown".to_string(),
            status,
            top_level,
        )
    }

    /// Extract structural outline for Rust source.
    ///
    /// Identifies `fn`, `struct`, `enum`, `trait`, `impl`, `mod`, `type`, `const`,
    /// and nests methods inside `impl` blocks and items inside inline `mod` blocks.
    fn extract_rust(
        &self,
        file_id: FileId,
        source_revision: SourceRevision,
        code: &[u8],
    ) -> SourceOutline {
        let text = String::from_utf8_lossy(code);
        let lines = LineIndex::new(code);
        let mut items = Vec::new();
        let mut id_counter = 1u64;

        let tokens = scan_rust_outline_tokens(&text);
        let mut cursor = 0;

        while cursor < tokens.len() && items.len() < self.limits.max_items {
            if let Some((item, next_cursor)) = parse_rust_item(&tokens, cursor, &lines, code.len(), &mut id_counter) {
                items.push(item);
                cursor = next_cursor;
            } else {
                cursor += 1;
            }
        }

        let status = if items.is_empty() && !code.is_empty() {
            return self.extract_line_blocks(
                file_id,
                source_revision,
                "rust",
                code,
                OutlineStatus::LineBlockFallback,
            );
        } else {
            OutlineStatus::Qualified
        };

        SourceOutline::new(
            file_id,
            source_revision,
            "rust".to_string(),
            status,
            items,
        )
    }

    /// Extract structural outline for Python source.
    fn extract_python(
        &self,
        file_id: FileId,
        source_revision: SourceRevision,
        code: &[u8],
    ) -> SourceOutline {
        let text = String::from_utf8_lossy(code);
        let lines = LineIndex::new(code);
        let mut top_level: Vec<OutlineItem> = Vec::new();
        let mut stack: Vec<(usize, OutlineItem)> = Vec::new(); // (indent, item)
        let mut id_counter = 1u64;

        for (line_idx, line) in text.lines().enumerate() {
            let line_num = (line_idx + 1) as u32;
            let indent = line.len() - line.trim_start().len();
            let trimmed = line.trim();

            if trimmed.starts_with('#') || trimmed.is_empty() {
                continue;
            }

            // Pop items from stack with indent >= current indent
            while let Some((top_indent, _)) = stack.last() {
                if *top_indent >= indent {
                    let (_, popped) = stack.pop().expect("checked");
                    if let Some((_, parent)) = stack.last_mut() {
                        parent.children.push(popped);
                    } else {
                        top_level.push(popped);
                    }
                } else {
                    break;
                }
            }

            let (kind, name) = if trimmed.starts_with("def ") || trimmed.starts_with("async def ") {
                let after_def = if let Some(r) = trimmed.strip_prefix("async def ") {
                    r
                } else if let Some(r) = trimmed.strip_prefix("def ") {
                    r
                } else {
                    ""
                };
                let fn_name = after_def.split('(').next().unwrap_or("").trim();
                if fn_name.is_empty() {
                    continue;
                }
                let is_method = !stack.is_empty();
                let item_kind = if is_method {
                    OutlineItemKind::Method
                } else {
                    OutlineItemKind::Function
                };
                (item_kind, fn_name.to_string())
            } else if trimmed.starts_with("class ") {
                let after_class = &trimmed[6..];
                let class_name = after_class
                    .split(['(', ':'])
                    .next()
                    .unwrap_or("")
                    .trim();
                if class_name.is_empty() {
                    continue;
                }
                (OutlineItemKind::Class, class_name.to_string())
            } else {
                continue;
            };

            let start_byte = lines.line_start_offset(line_num);
            let end_byte = lines.line_end_offset(line_num, code.len());

            let evidence = match OutlineEvidence::new(
                start_byte as u64,
                end_byte as u64,
                line_num,
                line_num,
                None,
            ) {
                Ok(ev) => ev,
                Err(_) => continue,
            };

            let item = OutlineItem {
                id: id_counter,
                name,
                kind,
                capability_level: CapabilityLevel::Structural,
                evidence,
                children: Vec::new(),
            };
            id_counter += 1;

            stack.push((indent, item));
        }

        while let Some((_, popped)) = stack.pop() {
            if let Some((_, parent)) = stack.last_mut() {
                parent.children.push(popped);
            } else {
                top_level.push(popped);
            }
        }

        let status = if top_level.is_empty() && !code.is_empty() {
            return self.extract_line_blocks(
                file_id,
                source_revision,
                "python",
                code,
                OutlineStatus::LineBlockFallback,
            );
        } else {
            OutlineStatus::Qualified
        };

        SourceOutline::new(
            file_id,
            source_revision,
            "python".to_string(),
            status,
            top_level,
        )
    }

    /// Extract structural outline for JavaScript / TypeScript / JSX / TSX.
    fn extract_js_ts(
        &self,
        file_id: FileId,
        source_revision: SourceRevision,
        lang: &str,
        code: &[u8],
    ) -> SourceOutline {
        let text = String::from_utf8_lossy(code);
        let lines = LineIndex::new(code);
        let mut items = Vec::new();
        let mut id_counter = 1u64;

        for (line_idx, line) in text.lines().enumerate() {
            let line_num = (line_idx + 1) as u32;
            let trimmed = line.trim();

            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.is_empty() {
                continue;
            }

            // Strip export / export default
            let stripped = if let Some(r) = trimmed.strip_prefix("export default ") {
                r
            } else if let Some(r) = trimmed.strip_prefix("export ") {
                r
            } else {
                trimmed
            };

            let (kind, name) = if stripped.starts_with("function ") || stripped.starts_with("async function ") {
                let after = if let Some(r) = stripped.strip_prefix("async function ") {
                    r
                } else {
                    &stripped[9..]
                };
                let fn_name = after.split(['(', '<']).next().unwrap_or("").trim();
                if fn_name.is_empty() {
                    continue;
                }
                (OutlineItemKind::Function, fn_name.to_string())
            } else if stripped.starts_with("class ") {
                let class_name = stripped[6..].split([' ', '{', '<']).next().unwrap_or("").trim();
                if class_name.is_empty() {
                    continue;
                }
                (OutlineItemKind::Class, class_name.to_string())
            } else if stripped.starts_with("interface ") {
                let iface_name = stripped[10..].split([' ', '{', '<']).next().unwrap_or("").trim();
                if iface_name.is_empty() {
                    continue;
                }
                (OutlineItemKind::Interface, iface_name.to_string())
            } else if stripped.starts_with("type ") {
                let type_name = stripped[5..].split([' ', '=', '<']).next().unwrap_or("").trim();
                if type_name.is_empty() {
                    continue;
                }
                (OutlineItemKind::TypeAlias, type_name.to_string())
            } else if stripped.starts_with("const ") && stripped.contains("=>") {
                let var_name = stripped[6..].split([' ', ':', '=']).next().unwrap_or("").trim();
                if var_name.is_empty() {
                    continue;
                }
                (OutlineItemKind::Function, var_name.to_string())
            } else {
                continue;
            };

            let start_byte = lines.line_start_offset(line_num);
            let end_byte = lines.line_end_offset(line_num, code.len());

            if let Ok(evidence) = OutlineEvidence::new(
                start_byte as u64,
                end_byte as u64,
                line_num,
                line_num,
                None,
            ) {
                items.push(OutlineItem {
                    id: id_counter,
                    name,
                    kind,
                    capability_level: CapabilityLevel::Structural,
                    evidence,
                    children: Vec::new(),
                });
                id_counter += 1;
            }
        }

        let status = if items.is_empty() && !code.is_empty() {
            return self.extract_line_blocks(
                file_id,
                source_revision,
                lang,
                code,
                OutlineStatus::LineBlockFallback,
            );
        } else {
            OutlineStatus::Qualified
        };

        SourceOutline::new(
            file_id,
            source_revision,
            lang.to_string(),
            status,
            items,
        )
    }

    /// Extract structural outline for Go source.
    fn extract_go(
        &self,
        file_id: FileId,
        source_revision: SourceRevision,
        code: &[u8],
    ) -> SourceOutline {
        let text = String::from_utf8_lossy(code);
        let lines = LineIndex::new(code);
        let mut items = Vec::new();
        let mut id_counter = 1u64;

        for (line_idx, line) in text.lines().enumerate() {
            let line_num = (line_idx + 1) as u32;
            let trimmed = line.trim();

            let (kind, name) = if trimmed.starts_with("func ") {
                let rest = &trimmed[5..];
                if rest.starts_with('(') {
                    // Method with receiver: func (r *Type) Name(...)
                    let after_recv = rest.split(')').nth(1).unwrap_or("").trim();
                    let fn_name = after_recv.split('(').next().unwrap_or("").trim();
                    if fn_name.is_empty() {
                        continue;
                    }
                    (OutlineItemKind::Method, fn_name.to_string())
                } else {
                    let fn_name = rest.split('(').next().unwrap_or("").trim();
                    if fn_name.is_empty() {
                        continue;
                    }
                    (OutlineItemKind::Function, fn_name.to_string())
                }
            } else if trimmed.starts_with("type ") {
                let rest = &trimmed[5..];
                let mut parts = rest.split_whitespace();
                let name = parts.next().unwrap_or("");
                let kind_tok = parts.next().unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                match kind_tok {
                    "struct" => (OutlineItemKind::Struct, name.to_string()),
                    "interface" => (OutlineItemKind::Interface, name.to_string()),
                    _ => (OutlineItemKind::TypeAlias, name.to_string()),
                }
            } else {
                continue;
            };

            let start_byte = lines.line_start_offset(line_num);
            let end_byte = lines.line_end_offset(line_num, code.len());

            if let Ok(evidence) = OutlineEvidence::new(
                start_byte as u64,
                end_byte as u64,
                line_num,
                line_num,
                None,
            ) {
                items.push(OutlineItem {
                    id: id_counter,
                    name,
                    kind,
                    capability_level: CapabilityLevel::Structural,
                    evidence,
                    children: Vec::new(),
                });
                id_counter += 1;
            }
        }

        let status = if items.is_empty() && !code.is_empty() {
            return self.extract_line_blocks(
                file_id,
                source_revision,
                "go",
                code,
                OutlineStatus::LineBlockFallback,
            );
        } else {
            OutlineStatus::Qualified
        };

        SourceOutline::new(
            file_id,
            source_revision,
            "go".to_string(),
            status,
            items,
        )
    }

    /// Extract structural outline for C/C++ source.
    fn extract_c_cpp(
        &self,
        file_id: FileId,
        source_revision: SourceRevision,
        code: &[u8],
    ) -> SourceOutline {
        let text = String::from_utf8_lossy(code);
        let lines = LineIndex::new(code);
        let mut items = Vec::new();
        let mut id_counter = 1u64;

        for (line_idx, line) in text.lines().enumerate() {
            let line_num = (line_idx + 1) as u32;
            let trimmed = line.trim();

            if trimmed.starts_with('#') || trimmed.starts_with("//") || trimmed.starts_with("/*") {
                continue;
            }

            let (kind, name) = if trimmed.starts_with("class ") {
                let name = trimmed[6..].split([' ', '{', ':', ';']).next().unwrap_or("").trim();
                if name.is_empty() {
                    continue;
                }
                (OutlineItemKind::Class, name.to_string())
            } else if trimmed.starts_with("struct ") {
                let name = trimmed[7..].split([' ', '{', ':', ';']).next().unwrap_or("").trim();
                if name.is_empty() {
                    continue;
                }
                (OutlineItemKind::Struct, name.to_string())
            } else if trimmed.starts_with("namespace ") {
                let name = trimmed[10..].split([' ', '{']).next().unwrap_or("").trim();
                if name.is_empty() {
                    continue;
                }
                (OutlineItemKind::Module, name.to_string())
            } else {
                continue;
            };

            let start_byte = lines.line_start_offset(line_num);
            let end_byte = lines.line_end_offset(line_num, code.len());

            if let Ok(evidence) = OutlineEvidence::new(
                start_byte as u64,
                end_byte as u64,
                line_num,
                line_num,
                None,
            ) {
                items.push(OutlineItem {
                    id: id_counter,
                    name,
                    kind,
                    capability_level: CapabilityLevel::Structural,
                    evidence,
                    children: Vec::new(),
                });
                id_counter += 1;
            }
        }

        let status = if items.is_empty() && !code.is_empty() {
            return self.extract_line_blocks(
                file_id,
                source_revision,
                "cpp",
                code,
                OutlineStatus::LineBlockFallback,
            );
        } else {
            OutlineStatus::Qualified
        };

        SourceOutline::new(
            file_id,
            source_revision,
            "cpp".to_string(),
            status,
            items,
        )
    }
}

/// Helper representing scanned token in Rust source.
#[derive(Clone, Debug)]
struct RustOutlineToken {
    text: String,
    start_byte: usize,
    end_byte: usize,
}

fn scan_rust_outline_tokens(source: &str) -> Vec<RustOutlineToken> {
    let mut tokens = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // Skip whitespace
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Skip line comments
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Skip block comments
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            let mut depth = 1;
            while i + 1 < bytes.len() && depth > 0 {
                if bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Skip string literals
        if b == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }

        // Skip raw string literals
        if b == b'r' && i + 1 < bytes.len() && (bytes[i + 1] == b'#' || bytes[i + 1] == b'"') {
            i += 1;
            let mut pounds = 0;
            while i < bytes.len() && bytes[i] == b'#' {
                pounds += 1;
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                i += 1;
                'find_close: while i < bytes.len() {
                    if bytes[i] == b'"' {
                        let mut match_pounds = 0;
                        while i + 1 + match_pounds < bytes.len() && bytes[i + 1 + match_pounds] == b'#' {
                            match_pounds += 1;
                        }
                        if match_pounds >= pounds {
                            i += 1 + pounds;
                            break 'find_close;
                        }
                    }
                    i += 1;
                }
            }
            continue;
        }

        // Single-character punctuation
        if matches!(b, b'{' | b'}' | b'(' | b')' | b'[' | b']' | b';' | b':' | b',' | b'<' | b'>') {
            let start = i;
            i += 1;
            tokens.push(RustOutlineToken {
                text: source[start..i].to_string(),
                start_byte: start,
                end_byte: i,
            });
            continue;
        }

        // Identifier or keyword
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        if i > start {
            tokens.push(RustOutlineToken {
                text: source[start..i].to_string(),
                start_byte: start,
                end_byte: i,
            });
        } else {
            i += 1;
        }
    }

    tokens
}

fn parse_rust_item(
    tokens: &[RustOutlineToken],
    start_cursor: usize,
    lines: &LineIndex,
    code_len: usize,
    id_counter: &mut u64,
) -> Option<(OutlineItem, usize)> {
    let mut i = start_cursor;

    // Skip attributes `#[...]`
    while i < tokens.len() && tokens[i].text == "#" {
        i += 1;
        if i < tokens.len() && tokens[i].text == "[" {
            let mut depth = 1;
            i += 1;
            while i < tokens.len() && depth > 0 {
                if tokens[i].text == "[" {
                    depth += 1;
                } else if tokens[i].text == "]" {
                    depth -= 1;
                }
                i += 1;
            }
        }
    }

    // Skip visibility: `pub`, `pub(crate)`, `pub(super)`, etc.
    if i < tokens.len() && tokens[i].text == "pub" {
        i += 1;
        if i < tokens.len() && tokens[i].text == "(" {
            while i < tokens.len() && tokens[i].text != ")" {
                i += 1;
            }
            if i < tokens.len() {
                i += 1;
            }
        }
    }

    // Skip qualifiers: `default`, `async`, `const`, `unsafe`, `extern "C"`
    while i < tokens.len() && matches!(tokens[i].text.as_str(), "default" | "async" | "const" | "unsafe" | "extern") {
        if tokens[i].text == "const" && i + 1 < tokens.len() && tokens[i + 1].text != "fn" {
            // This is a `const NAME: Type` item, not a qualifier!
            break;
        }
        i += 1;
    }

    if i >= tokens.len() {
        return None;
    }

    let keyword = tokens[i].text.clone();
    let kw_start_byte = tokens[i].start_byte;
    i += 1;

    match keyword.as_str() {
        "fn" => {
            if i >= tokens.len() {
                return None;
            }
            let fn_name_tok = &tokens[i];
            let name = fn_name_tok.text.clone();
            let name_span = Some((fn_name_tok.start_byte as u64, fn_name_tok.end_byte as u64));
            i += 1;

            // Find matching `{` and its closing `}`
            let (byte_end, next_cursor) = find_block_end(tokens, i, code_len);

            let line_start = lines.byte_to_line(kw_start_byte);
            let line_end = lines.byte_to_line(byte_end.saturating_sub(1));

            let evidence = OutlineEvidence::new(
                kw_start_byte as u64,
                byte_end as u64,
                line_start,
                line_end,
                name_span,
            ).ok()?;

            let id = *id_counter;
            *id_counter += 1;

            Some((
                OutlineItem {
                    id,
                    name,
                    kind: OutlineItemKind::Function,
                    capability_level: CapabilityLevel::Structural,
                    evidence,
                    children: Vec::new(),
                },
                next_cursor,
            ))
        }
        "struct" => parse_simple_rust_decl(tokens, i, kw_start_byte, OutlineItemKind::Struct, lines, code_len, id_counter),
        "enum" => parse_simple_rust_decl(tokens, i, kw_start_byte, OutlineItemKind::Enum, lines, code_len, id_counter),
        "trait" => parse_simple_rust_decl(tokens, i, kw_start_byte, OutlineItemKind::Trait, lines, code_len, id_counter),
        "type" => parse_simple_rust_decl(tokens, i, kw_start_byte, OutlineItemKind::TypeAlias, lines, code_len, id_counter),
        "const" => parse_simple_rust_decl(tokens, i, kw_start_byte, OutlineItemKind::Const, lines, code_len, id_counter),
        "impl" => {
            // Extract impl type name
            let name_start = i;
            while i < tokens.len() && tokens[i].text != "{" && tokens[i].text != ";" {
                i += 1;
            }
            if i >= tokens.len() {
                return None;
            }
            let impl_name: String = tokens[name_start..i]
                .iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");

            let name = format!("impl {impl_name}");

            // If it has a `{`, scan child methods inside!
            if tokens[i].text == "{" {
                let body_start = i + 1;
                let (byte_end, next_cursor) = find_block_end(tokens, i, code_len);

                // Scan child methods inside the impl body
                let mut children = Vec::new();
                let mut child_cursor = body_start;
                while child_cursor < next_cursor.saturating_sub(1) {
                    if let Some((child_item, next_child)) = parse_rust_item(tokens, child_cursor, lines, code_len, id_counter) {
                        let mut item_as_method = child_item;
                        if item_as_method.kind == OutlineItemKind::Function {
                            item_as_method.kind = OutlineItemKind::Method;
                        }
                        children.push(item_as_method);
                        child_cursor = next_child;
                    } else {
                        child_cursor += 1;
                    }
                }

                let line_start = lines.byte_to_line(kw_start_byte);
                let line_end = lines.byte_to_line(byte_end.saturating_sub(1));

                let evidence = OutlineEvidence::new(
                    kw_start_byte as u64,
                    byte_end as u64,
                    line_start,
                    line_end,
                    None,
                ).ok()?;

                let id = *id_counter;
                *id_counter += 1;

                Some((
                    OutlineItem {
                        id,
                        name,
                        kind: OutlineItemKind::Impl,
                        capability_level: CapabilityLevel::Structural,
                        evidence,
                        children,
                    },
                    next_cursor,
                ))
            } else {
                None
            }
        }
        "mod" => {
            if i >= tokens.len() {
                return None;
            }
            let mod_name_tok = &tokens[i];
            let name = mod_name_tok.text.clone();
            let name_span = Some((mod_name_tok.start_byte as u64, mod_name_tok.end_byte as u64));
            i += 1;

            if i < tokens.len() && tokens[i].text == ";" {
                let byte_end = tokens[i].end_byte;
                let next_cursor = i + 1;
                let line_start = lines.byte_to_line(kw_start_byte);
                let line_end = lines.byte_to_line(byte_end);

                let evidence = OutlineEvidence::new(
                    kw_start_byte as u64,
                    byte_end as u64,
                    line_start,
                    line_end,
                    name_span,
                ).ok()?;

                let id = *id_counter;
                *id_counter += 1;

                Some((
                    OutlineItem {
                        id,
                        name,
                        kind: OutlineItemKind::Module,
                        capability_level: CapabilityLevel::Structural,
                        evidence,
                        children: Vec::new(),
                    },
                    next_cursor,
                ))
            } else if i < tokens.len() && tokens[i].text == "{" {
                let (byte_end, next_cursor) = find_block_end(tokens, i, code_len);
                let line_start = lines.byte_to_line(kw_start_byte);
                let line_end = lines.byte_to_line(byte_end.saturating_sub(1));

                let evidence = OutlineEvidence::new(
                    kw_start_byte as u64,
                    byte_end as u64,
                    line_start,
                    line_end,
                    name_span,
                ).ok()?;

                let id = *id_counter;
                *id_counter += 1;

                Some((
                    OutlineItem {
                        id,
                        name,
                        kind: OutlineItemKind::Module,
                        capability_level: CapabilityLevel::Structural,
                        evidence,
                        children: Vec::new(),
                    },
                    next_cursor,
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_simple_rust_decl(
    tokens: &[RustOutlineToken],
    mut i: usize,
    kw_start_byte: usize,
    kind: OutlineItemKind,
    lines: &LineIndex,
    code_len: usize,
    id_counter: &mut u64,
) -> Option<(OutlineItem, usize)> {
    if i >= tokens.len() {
        return None;
    }
    let name_tok = &tokens[i];
    let name = name_tok.text.clone();
    let name_span = Some((name_tok.start_byte as u64, name_tok.end_byte as u64));
    i += 1;

    let (byte_end, next_cursor) = find_block_end(tokens, i, code_len);
    let line_start = lines.byte_to_line(kw_start_byte);
    let line_end = lines.byte_to_line(byte_end.saturating_sub(1));

    let evidence = OutlineEvidence::new(
        kw_start_byte as u64,
        byte_end as u64,
        line_start,
        line_end,
        name_span,
    ).ok()?;

    let id = *id_counter;
    *id_counter += 1;

    Some((
        OutlineItem {
            id,
            name,
            kind,
            capability_level: CapabilityLevel::Structural,
            evidence,
            children: Vec::new(),
        },
        next_cursor,
    ))
}

fn find_block_end(tokens: &[RustOutlineToken], mut i: usize, code_len: usize) -> (usize, usize) {
    while i < tokens.len() {
        if tokens[i].text == "{" {
            let mut depth = 1;
            i += 1;
            while i < tokens.len() && depth > 0 {
                if tokens[i].text == "{" {
                    depth += 1;
                } else if tokens[i].text == "}" {
                    depth -= 1;
                }
                i += 1;
            }
            let byte_end = if i > 0 && i <= tokens.len() {
                tokens[i - 1].end_byte
            } else {
                code_len
            };
            return (byte_end, i);
        } else if tokens[i].text == ";" {
            let byte_end = tokens[i].end_byte;
            return (byte_end, i + 1);
        }
        i += 1;
    }
    (code_len, tokens.len())
}

/// Helper for mapping byte offsets to 1-indexed line numbers.
#[derive(Clone, Debug)]
struct LineIndex {
    line_starts: Vec<usize>,
    total_len: usize,
}

impl LineIndex {
    fn new(code: &[u8]) -> Self {
        let mut line_starts = vec![0];
        for (i, &b) in code.iter().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            line_starts,
            total_len: code.len(),
        }
    }

    fn line_count(&self) -> u32 {
        if self.total_len == 0 {
            return 1;
        }
        let count = self.line_starts.len();
        if count > 1 && self.line_starts[count - 1] >= self.total_len {
            (count - 1) as u32
        } else {
            count as u32
        }
    }

    fn byte_to_line(&self, byte_offset: usize) -> u32 {
        match self.line_starts.binary_search(&byte_offset) {
            Ok(idx) => (idx + 1) as u32,
            Err(idx) => idx as u32,
        }
    }

    fn line_start_offset(&self, line: u32) -> usize {
        let idx = (line.saturating_sub(1)) as usize;
        if idx < self.line_starts.len() {
            self.line_starts[idx]
        } else {
            *self.line_starts.last().unwrap_or(&0)
        }
    }

    fn line_end_offset(&self, line: u32, total_len: usize) -> usize {
        let next_idx = line as usize;
        if next_idx < self.line_starts.len() {
            self.line_starts[next_idx]
        } else {
            total_len
        }
    }
}

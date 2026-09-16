//! Structural source outline types and hierarchy (FCB-030.A).
//!
//! Provides language-scoped structural outline nodes with exact evidence spans,
//! hierarchy nesting, line-block fallback for unsupported or uncolored files,
//! and strict alignment with the Plan §11.5 capability ladder.

use fcb_core::{FileId, SourceRevision};

/// Capability level of an outline item or extracted fact (Plan §11.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityLevel {
    /// Exact source representation (range copy, exact literal match, line navigation).
    Bytes = 0,
    /// Qualified token classification (comment/string boundaries, keyword coloring).
    Lexical = 1,
    /// Parser-supported syntax entities (Rust item outline, Markdown heading tree).
    Structural = 2,
    /// Proven relationship within the parser's modeled scope (unique import link).
    ResolvedLocal = 3,
    /// Facts supplied by an explicitly enabled, independently qualified provider.
    ExternalSemantic = 4,
    /// A candidate only (same-name identifier link, approximate suggestion).
    Heuristic = 5,
}

impl CapabilityLevel {
    /// Canonical display name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bytes => "Bytes",
            Self::Lexical => "Lexical",
            Self::Structural => "Structural",
            Self::ResolvedLocal => "ResolvedLocal",
            Self::ExternalSemantic => "ExternalSemantic",
            Self::Heuristic => "Heuristic",
        }
    }

    /// Whether this level permits claiming compiler or semantic definitions.
    #[must_use]
    pub const fn allows_compiler_claim(self) -> bool {
        matches!(self, Self::ResolvedLocal | Self::ExternalSemantic)
    }

    /// Whether this level is confined to structural or below.
    #[must_use]
    pub const fn is_structural_or_below(self) -> bool {
        matches!(self, Self::Bytes | Self::Lexical | Self::Structural)
    }
}

/// Category of an outline item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutlineItemKind {
    /// Module declaration (e.g. `mod foo;` or `mod foo { ... }`).
    Module,
    /// Struct declaration.
    Struct,
    /// Class declaration.
    Class,
    /// Interface declaration.
    Interface,
    /// Trait declaration.
    Trait,
    /// Implementation block (e.g. `impl Foo` or `impl Bar for Foo`).
    Impl,
    /// Enum declaration.
    Enum,
    /// Enum variant.
    Variant,
    /// Top-level function.
    Function,
    /// Method within a class, struct, or impl block.
    Method,
    /// Markdown heading with 1-based level (1..=6).
    Heading { level: u8 },
    /// Constant or static item.
    Const,
    /// Type alias.
    TypeAlias,
    /// Plain line block fallback for unsupported languages or plain text.
    LineBlock { start_line: u32, end_line: u32 },
}

impl OutlineItemKind {
    /// Human-readable label for the item kind.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Struct => "struct",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Enum => "enum",
            Self::Variant => "variant",
            Self::Function => "function",
            Self::Method => "method",
            Self::Heading { .. } => "heading",
            Self::Const => "const",
            Self::TypeAlias => "type",
            Self::LineBlock { .. } => "lines",
        }
    }
}

/// Exact evidence byte and line span for an outline item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutlineEvidence {
    /// Start byte offset (0-indexed, inclusive).
    pub byte_start: u64,
    /// End byte offset (0-indexed, exclusive).
    pub byte_end: u64,
    /// Start line number (1-indexed, inclusive).
    pub line_start: u32,
    /// End line number (1-indexed, inclusive).
    pub line_end: u32,
    /// Optional byte offset range of the item's name/identifier.
    pub name_range: Option<(u64, u64)>,
}

impl OutlineEvidence {
    /// Construct a verified evidence span.
    pub fn new(
        byte_start: u64,
        byte_end: u64,
        line_start: u32,
        line_end: u32,
        name_range: Option<(u64, u64)>,
    ) -> Result<Self, String> {
        if byte_start > byte_end {
            return Err(format!(
                "invalid byte range: start {byte_start} > end {byte_end}"
            ));
        }
        if line_start > line_end {
            return Err(format!(
                "invalid line range: start {line_start} > end {line_end}"
            ));
        }
        if let Some((n_start, n_end)) = name_range {
            if n_start > n_end {
                return Err(format!(
                    "invalid name range: start {n_start} > end {n_end}"
                ));
            }
            if n_start < byte_start || n_end > byte_end {
                return Err(format!(
                    "name range [{n_start}..{n_end}] outside item byte bounds [{byte_start}..{byte_end}]"
                ));
            }
        }
        Ok(Self {
            byte_start,
            byte_end,
            line_start,
            line_end,
            name_range,
        })
    }
}

/// A node in a structural source outline tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    /// 1-based unique identifier within this outline.
    pub id: u64,
    /// Display name of the item (identifier name, heading text, or line range).
    pub name: String,
    /// Item kind.
    pub kind: OutlineItemKind,
    /// Capability tier (strictly `Structural` for parsed items, `Bytes` for line blocks).
    pub capability_level: CapabilityLevel,
    /// Exact evidence byte and line range.
    pub evidence: OutlineEvidence,
    /// Nested child items (e.g. methods within an impl, subheadings within a heading).
    pub children: Vec<OutlineItem>,
}

impl OutlineItem {
    /// Total count of items in this subtree (including self).
    #[must_use]
    pub fn subtree_count(&self) -> usize {
        1 + self.children.iter().map(|c| c.subtree_count()).sum::<usize>()
    }

    /// Maximum depth of this subtree (1 if leaf).
    #[must_use]
    pub fn subtree_depth(&self) -> usize {
        1 + self.children.iter().map(|c| c.subtree_depth()).max().unwrap_or(0)
    }

    /// Find an item in this subtree spanning the given byte offset.
    #[must_use]
    pub fn find_at_offset(&self, offset: u64) -> Option<&OutlineItem> {
        if offset < self.evidence.byte_start || offset >= self.evidence.byte_end {
            return None;
        }
        for child in &self.children {
            if let Some(found) = child.find_at_offset(offset) {
                return Some(found);
            }
        }
        Some(self)
    }
}

/// Qualification status of a source outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutlineStatus {
    /// Qualified structural outline parsed with language-specific grammar.
    Qualified,
    /// Plain line-block fallback for unsupported languages or plain text files.
    LineBlockFallback,
    /// Degraded recovery due to malformed or truncated syntax.
    DegradedMalformed,
}

/// The complete structural outline for a single source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceOutline {
    /// Logical file identity.
    pub file_id: FileId,
    /// Source revision observed.
    pub source_revision: SourceRevision,
    /// Language route used for extraction.
    pub language: String,
    /// Qualification status.
    pub status: OutlineStatus,
    /// Top-level outline items.
    pub items: Vec<OutlineItem>,
    /// Total number of items across all levels.
    pub total_items: usize,
    /// Maximum nesting depth.
    pub max_depth: usize,
}

impl SourceOutline {
    /// Construct a verified `SourceOutline`.
    pub fn new(
        file_id: FileId,
        source_revision: SourceRevision,
        language: String,
        status: OutlineStatus,
        items: Vec<OutlineItem>,
    ) -> Self {
        let total_items = items.iter().map(|i| i.subtree_count()).sum();
        let max_depth = items.iter().map(|i| i.subtree_depth()).max().unwrap_or(0);
        Self {
            file_id,
            source_revision,
            language,
            status,
            items,
            total_items,
            max_depth,
        }
    }

    /// Find the most specific outline item containing the given byte offset.
    #[must_use]
    pub fn find_at_offset(&self, offset: u64) -> Option<&OutlineItem> {
        for item in &self.items {
            if let Some(found) = item.find_at_offset(offset) {
                return Some(found);
            }
        }
        None
    }

    /// Collect a flat list of references to all items in depth-first order.
    #[must_use]
    pub fn all_items_flat(&self) -> Vec<&OutlineItem> {
        let mut out = Vec::with_capacity(self.total_items);
        for item in &self.items {
            collect_flat(item, &mut out);
        }
        out
    }
}

fn collect_flat<'a>(item: &'a OutlineItem, out: &mut Vec<&'a OutlineItem>) {
    out.push(item);
    for child in &item.children {
        collect_flat(child, out);
    }
}

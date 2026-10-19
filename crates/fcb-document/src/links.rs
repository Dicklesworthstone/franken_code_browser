#![forbid(unsafe_code)]

//! Captured-source identities attached to upstream-parsed Markdown links.
//! FrankenMarkdown owns parsing and URL resolution. Its pinned public parser
//! does not yet guarantee correct spans for every accepted AST shape. Every
//! result therefore identifies the WHOLE original captured Markdown document;
//! neither a link-token span nor an enclosing block span is fabricated.
//! Reference definitions, escaped destinations and nested containers therefore
//! use the parser's interpretation without a second Markdown scanner.

use std::mem::size_of;
use fcb_core::{ByteLength, ByteOffset, ByteRange, FileId, QueryGeneration,
    ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_source::CaptureRequest;
use franken_markdown::{Block, Inline};

pub const MAX_DOCUMENT_LINK_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_DOCUMENT_LINKS: usize = 4096;
pub const MAX_DOCUMENT_LINK_DESTINATION_BYTES: usize = 4096;
const MAX_DEPTH: usize = 128;
const MAX_NODES: usize = 131_072;
const MAX_PARSE_WORKSPACE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentLinkError {
    InvalidLimits, SourceLimit, InvalidUtf8, InvalidEvidence, ComplexityLimit,
    OwnerMismatch, StaleSource, StaleGeneration, ResourceDenied, Canceled,
}
impl DocumentLinkError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "DOCUMENT_LINK_INVALID_LIMITS",
            Self::SourceLimit => "DOCUMENT_LINK_SOURCE_LIMIT",
            Self::InvalidUtf8 => "DOCUMENT_LINK_INVALID_UTF8",
            Self::InvalidEvidence => "DOCUMENT_LINK_INVALID_EVIDENCE",
            Self::ComplexityLimit => "DOCUMENT_LINK_COMPLEXITY_LIMIT",
            Self::OwnerMismatch => "DOCUMENT_LINK_OWNER_MISMATCH",
            Self::StaleSource => "DOCUMENT_LINK_STALE_SOURCE",
            Self::StaleGeneration => "DOCUMENT_LINK_STALE_GENERATION",
            Self::ResourceDenied => "DOCUMENT_LINK_RESOURCE_DENIED",
            Self::Canceled => "DOCUMENT_LINK_CANCELED",
        }
    }
}
impl std::fmt::Display for DocumentLinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for DocumentLinkError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentLink {
    id: u64,
    block_index: usize,
    evidence: ByteRange,
    destination: String,
}
impl DocumentLink {
    /// One-based parser traversal occurrence. Equal destinations and equal block
    /// ranges remain distinct occurrences; this is not a document-global ID.
    pub const fn id(&self) -> u64 { self.id }
    pub const fn block_index(&self) -> usize { self.block_index }
    /// Complete original bytes, including any BOM and frontmatter. Block and
    /// link-token coordinates are not claimed by the pinned parser adapter.
    pub const fn original_document_range(&self) -> ByteRange { self.evidence }
    /// Parser-decoded URL text, never a claim of literal source-token bytes.
    pub fn destination(&self) -> &str { &self.destination }
}

/// Borrows the exact complete original source. The result and its resource
/// charge cannot escape the source's host lifetime by cloning the capture.
pub struct CapturedDocumentLinks<'source> {
    source: &'source [u8],
    request: CaptureRequest,
    generation: QueryGeneration,
    links: Vec<DocumentLink>,
    links_seen: u64,
    diagnostics: usize,
    truncated: bool,
    _lease: ResourceLease,
}
impl<'source> CapturedDocumentLinks<'source> {
    /// Bounded-input worker operation. Cancellation brackets the upstream
    /// synchronous parser and is checked throughout the bounded AST traversal.
    /// A zero display limit is valid for counting and explicitly omits all links.
    pub fn build(source: &'source [u8], request: CaptureRequest,
        generation: QueryGeneration, max_links: usize, budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
        -> Result<Self, DocumentLinkError> {
        if request.file().owner() != generation.owner() { return Err(DocumentLinkError::OwnerMismatch); }
        if request.range().is_some() { return Err(DocumentLinkError::InvalidEvidence); }
        if max_links > MAX_DOCUMENT_LINKS { return Err(DocumentLinkError::InvalidLimits); }
        if source.len() > MAX_DOCUMENT_LINK_SOURCE_BYTES { return Err(DocumentLinkError::SourceLimit); }
        check(&mut canceled)?;
        let parse_charge = parse_workspace(source)?;
        let charge = Some(parse_charge)
            .and_then(|n| max_links.checked_mul(size_of::<DocumentLink>() + MAX_DOCUMENT_LINK_DESTINATION_BYTES)
                .and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(256 * 1024 + size_of::<Self>()))
            .ok_or(DocumentLinkError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(request.file().owner(), allocation,
            ByteLength::new(charge as u64)).map_err(|_| DocumentLinkError::ResourceDenied)?;
        let original = std::str::from_utf8(source).map_err(|_| DocumentLinkError::InvalidUtf8)?;
        let base = if source.starts_with(&[0xef, 0xbb, 0xbf]) { 3 } else { 0 };
        let text = &original[base..];
        let parsed = franken_markdown::parse::parse_document_spanned(text);
        check(&mut canceled)?;
        if parsed.source_len != text.len() { return Err(DocumentLinkError::InvalidEvidence); }
        let evidence = ByteRange::new(ByteOffset::new(0), ByteOffset::new(source.len() as u64))
            .map_err(|_| DocumentLinkError::InvalidEvidence)?;
        let mut links = Vec::new();
        links.try_reserve_exact(max_links).map_err(|_| DocumentLinkError::ResourceDenied)?;
        if links.capacity() > max_links { return Err(DocumentLinkError::ResourceDenied); }
        let mut walk = Walk { links, max_links, seen: 0, nodes: 0, truncated: false };
        for (index, block) in parsed.blocks.iter().enumerate() {
            check(&mut canceled)?;
            // The upstream AST is useful, but its independently collected
            // block spans can be misaligned by frontmatter, lazy continuations,
            // definitions and math. Do not read or interpret block.span.
            // Every occurrence instead keeps exact complete source evidence.
            walk.block(&block.node, index, evidence, 0, &mut canceled)?;
        }
        check(&mut canceled)?;
        Ok(Self { source, request, generation, links: walk.links, links_seen: walk.seen,
            diagnostics: parsed.diagnostics.len(), truncated: walk.truncated, _lease: lease })
    }
    pub const fn file(&self) -> FileId { self.request.file() }
    pub const fn revision(&self) -> SourceRevision { self.request.revision() }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub fn links(&self) -> &[DocumentLink] { &self.links }
    pub const fn links_seen(&self) -> u64 { self.links_seen }
    pub const fn diagnostics(&self) -> usize { self.diagnostics }
    pub const fn truncated(&self) -> bool { self.truncated }
    pub fn validate_delivery(&self, source: &[u8], request: CaptureRequest,
        generation: QueryGeneration) -> Result<(), DocumentLinkError> {
        if request.file().owner() != self.file().owner() || generation.owner() != self.file().owner() {
            return Err(DocumentLinkError::OwnerMismatch);
        }
        if request != self.request || source != self.source { return Err(DocumentLinkError::StaleSource); }
        if generation != self.generation { return Err(DocumentLinkError::StaleGeneration); }
        Ok(())
    }
}

/// Pure FCB adapter to the pinned upstream URL policy. The returned path is a
/// logical candidate only. Membership and original capture identity belong to
/// the caller's closed source manifest, never to a live filesystem lookup.
pub fn resolve_document_destination(source_path: &str, destination: &str) -> Option<(String, String)> {
    franken_markdown::book::resolve_book_destination(source_path, destination)
}

struct Walk { links: Vec<DocumentLink>, max_links: usize, seen: u64, nodes: usize, truncated: bool }
impl Walk {
    fn node(&mut self, depth: usize, canceled: &mut impl FnMut() -> bool) -> Result<(), DocumentLinkError> {
        check(canceled)?;
        if depth > MAX_DEPTH || self.nodes >= MAX_NODES { return Err(DocumentLinkError::ComplexityLimit); }
        self.nodes += 1;
        Ok(())
    }
    fn block(&mut self, block: &Block, index: usize, evidence: ByteRange, depth: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<(), DocumentLinkError> {
        self.node(depth, canceled)?;
        match block {
            Block::Paragraph(inlines) | Block::Heading { inlines, .. } =>
                self.inlines(inlines, index, evidence, depth + 1, canceled)?,
            Block::BlockQuote(blocks) | Block::FootnoteDefinition { blocks, .. } =>
                for child in blocks { self.block(child, index, evidence, depth + 1, canceled)?; },
            Block::List(list) => for item in &list.items {
                for child in &item.blocks { self.block(child, index, evidence, depth + 1, canceled)?; }
            },
            Block::Table(table) => {
                for cell in &table.head { self.inlines(cell, index, evidence, depth + 1, canceled)?; }
                for row in &table.rows { for cell in row { self.inlines(cell, index, evidence, depth + 1, canceled)?; } }
            }
            Block::DefinitionList(items) => for item in items {
                for line in item.terms.iter().chain(item.definitions.iter()) {
                    self.inlines(line, index, evidence, depth + 1, canceled)?;
                }
            },
            Block::CodeBlock { .. } | Block::HtmlBlock(_) | Block::MathBlock(_)
                | Block::ThematicBreak | Block::PageBreak => {}
        }
        Ok(())
    }
    fn inlines(&mut self, inlines: &[Inline], index: usize, evidence: ByteRange, depth: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<(), DocumentLinkError> {
        for inline in inlines {
            self.node(depth, canceled)?;
            match inline {
                Inline::Link { dest, content, .. } => {
                    self.seen += 1; // Bounded by MAX_NODES.
                    if self.links.len() < self.max_links && dest.len() <= MAX_DOCUMENT_LINK_DESTINATION_BYTES {
                        let mut destination = String::new();
                        destination.try_reserve_exact(dest.len()).map_err(|_| DocumentLinkError::ResourceDenied)?;
                        if destination.capacity() > dest.len() { return Err(DocumentLinkError::ResourceDenied); }
                        destination.push_str(dest);
                        self.links.push(DocumentLink { id: self.seen, block_index: index, evidence, destination });
                    } else { self.truncated = true; }
                    self.inlines(content, index, evidence, depth + 1, canceled)?;
                }
                Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strikethrough(inner) =>
                    self.inlines(inner, index, evidence, depth + 1, canceled)?,
                Inline::Text(_) | Inline::Code(_) | Inline::Image { .. } | Inline::SoftBreak
                    | Inline::HardBreak | Inline::Html(_) | Inline::FootnoteRef { .. }
                    | Inline::Math(_) | Inline::DisplayMath(_) => {}
            }
        }
        Ok(())
    }
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), DocumentLinkError> {
    if canceled() { Err(DocumentLinkError::Canceled) } else { Ok(()) }
}

// The pinned FMD parser's own reference-collection gate is src.contains("]:").
// Reference destinations AND titles are cloned once per use before our output
// limits apply. Count only possible uses for conservative allocation admission;
// this is not Markdown parsing, and false-positive syntax only reduces admission.
fn parse_workspace(source: &[u8]) -> Result<usize, DocumentLinkError> {
    let ordinary = source.len().checked_mul(512).ok_or(DocumentLinkError::ComplexityLimit)?;
    let reference_overlap = if source.windows(2).any(|bytes| bytes == b"]:") {
        let uses = source.iter().filter(|&&byte| byte == b'[').count();
        source.len().checked_mul(uses.checked_add(1).ok_or(DocumentLinkError::ComplexityLimit)?)
            .and_then(|n| n.checked_mul(8)).ok_or(DocumentLinkError::ComplexityLimit)?
    } else { 0 };
    let charge = ordinary.checked_add(reference_overlap).ok_or(DocumentLinkError::ComplexityLimit)?;
    if charge > MAX_PARSE_WORKSPACE_BYTES { return Err(DocumentLinkError::ComplexityLimit); }
    Ok(charge)
}

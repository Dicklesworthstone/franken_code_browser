#![forbid(unsafe_code)]

//! Bounded lexical import evidence from one exact capture (FCB-048.A).
//!
//! FrankenMarkdown owns string/comment/keyword classification. This module
//! consumes its validated byte spans and recognizes a small source-specific
//! statement grammar: Rust external mod/use and JS/TS static import/export-from.
//! It is not a compiler, build graph, package resolver, or replacement lexer.
//! Every result remains lexical evidence; closed-catalog navigation is explicit.
//! UTF-8/UTF-16 decoding and evidence conversion use the common source map.

mod resolve;
pub use resolve::{resolve_import, ImportResolution, ImportResolutionKind};

use std::mem::size_of;
use fcb_core::{ByteLength, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, FileId,
    QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_source::{CaptureEncodingMap, CaptureRequest, DetectedEncoding, SpanKind, detect_encoding};
use franken_markdown::highlight::{self, Span, Tok};

pub const MAX_IMPORT_SOURCE_BYTES: usize = 128 * 1024;
pub const MAX_IMPORT_ITEMS: usize = 4096;
pub const MAX_IMPORT_SPECIFIER_BYTES: usize = 1024;
const MAX_STATEMENT_TERMINALS: usize = 1024;
const MAX_STATEMENT_BYTES: usize = 16 * 1024;
pub const IMPORT_EXTRACTOR: &str = "franken-markdown-d6fbd1d5/import-statements-v1";
pub const IMPORT_POLICY: &str = "rust-mod-use-and-js-static-import-export-v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportLanguage { Rust, JavaScript, TypeScript, Jsx, Tsx }
impl ImportLanguage {
    pub const fn name(self) -> &'static str {
        match self { Self::Rust => "rust", Self::JavaScript => "javascript", Self::TypeScript => "typescript",
            Self::Jsx => "jsx", Self::Tsx => "tsx" }
    }
    pub fn from_path(path: &[u8]) -> Option<Self> {
        let file = path.rsplit(|&byte| byte == b'/').next()?;
        let suffix = file.get(file.iter().rposition(|&byte| byte == b'.')? + 1..)?;
        if suffix.len() > 4 { return None; }
        let mut lower = [0u8; 4];
        for (to, &from) in lower.iter_mut().zip(suffix) { *to = from.to_ascii_lowercase(); }
        match &lower[..suffix.len()] {
            b"rs" => Some(Self::Rust), b"js" | b"mjs" | b"cjs" => Some(Self::JavaScript),
            b"ts" | b"mts" | b"cts" => Some(Self::TypeScript), b"jsx" => Some(Self::Jsx), b"tsx" => Some(Self::Tsx),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportKind { RustModule, RustUse, JavaScriptImport, JavaScriptExport }
impl ImportKind {
    pub const fn name(self) -> &'static str {
        match self { Self::RustModule => "rust-module", Self::RustUse => "rust-use",
            Self::JavaScriptImport => "javascript-import", Self::JavaScriptExport => "javascript-export" }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportError {
    SourceLimit, InvalidLimits, UnsupportedEncoding, InvalidEvidence, ResourceDenied,
    Canceled, StaleSource, StaleQuery, OwnerMismatch, NotFound,
}
impl ImportError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SourceLimit => "IMPORT_SOURCE_LIMIT", Self::InvalidLimits => "IMPORT_INVALID_LIMITS",
            Self::UnsupportedEncoding => "IMPORT_UNSUPPORTED_ENCODING", Self::InvalidEvidence => "IMPORT_INVALID_EVIDENCE",
            Self::ResourceDenied => "IMPORT_RESOURCE_DENIED", Self::Canceled => "IMPORT_CANCELED",
            Self::StaleSource => "IMPORT_STALE_SOURCE", Self::StaleQuery => "IMPORT_STALE_QUERY",
            Self::OwnerMismatch => "IMPORT_OWNER_MISMATCH", Self::NotFound => "IMPORT_NOT_FOUND",
        }
    }
}
impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for ImportError {}

#[derive(Debug, Eq, PartialEq)]
pub struct ImportFact {
    id: u64, kind: ImportKind, destination: String, original_range: ByteRange,
    specifier_range: ByteRange, resolution_allowed: bool,
}
impl ImportFact {
    pub const fn id(&self) -> u64 { self.id }
    pub const fn kind(&self) -> ImportKind { self.kind }
    pub fn destination(&self) -> &str { &self.destination }
    pub const fn original_range(&self) -> ByteRange { self.original_range }
    pub const fn specifier_range(&self) -> ByteRange { self.specifier_range }
    pub const fn resolution_allowed(&self) -> bool { self.resolution_allowed }
}
pub struct CapturedImports<'source> {
    source: &'source [u8], request: CaptureRequest, generation: QueryGeneration,
    language: ImportLanguage, facts: Vec<ImportFact>, seen: u64, diagnostics: usize,
    truncated: bool, _lease: ResourceLease,
}
impl<'source> CapturedImports<'source> {
    /// Worker operation; source, decoder, lexical terminals, output and cancellation
    /// are bounded. The upstream whole-block lexer receives at most 128KiB original
    /// source and its admitted decoded expansion; no lexer runs on a UI callback.
    #[allow(clippy::too_many_arguments)]
    pub fn build(source: &'source [u8], request: CaptureRequest, generation: QueryGeneration,
        language: ImportLanguage, max_items: usize, budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool) -> Result<Self, ImportError> {
        if request.file().owner() != generation.owner() { return Err(ImportError::OwnerMismatch); }
        if request.range().is_some() { return Err(ImportError::InvalidEvidence); }
        if source.len() > MAX_IMPORT_SOURCE_BYTES { return Err(ImportError::SourceLimit); }
        if max_items > MAX_IMPORT_ITEMS { return Err(ImportError::InvalidLimits); }
        check(&mut canceled)?;
        // This covers decoder old/new overlap, full decoded lexical span/terminal
        // buffers, all retained specifiers and result rows before allocation.
        let charge = source.len().checked_add(1).and_then(|n| n.checked_mul(768))
            .and_then(|n| max_items.checked_mul(size_of::<ImportFact>() + MAX_IMPORT_SPECIFIER_BYTES).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(size_of::<Self>() + 4096)).ok_or(ImportError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(generation.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ImportError::ResourceDenied)?;
        let encoding = detect_encoding(source);
        if encoding == DetectedEncoding::Unsupported { return Err(ImportError::UnsupportedEncoding); }
        let map = CaptureEncodingMap::build_with_encoding(source, encoding).map_err(|_| ImportError::UnsupportedEncoding)?;
        if map.spans().iter().any(|span| matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
            return Err(ImportError::UnsupportedEncoding);
        }
        let text = map.decoded_text();
        if text.len() > MAX_IMPORT_SOURCE_BYTES * 4 { return Err(ImportError::SourceLimit); }
        check(&mut canceled)?;
        let capacity = text.len().max(1);
        let mut spans = reserve::<Span>(capacity)?;
        highlight::highlight_into(language.name(), text, &mut spans);
        if spans.capacity() > capacity { return Err(ImportError::InvalidEvidence); }
        let terminals = terminals(text, &spans, &mut canceled)?;
        let mut parser = Statements { text, map: &map, tokens: &terminals,
            facts: reserve(max_items)?, max_items, seen: 0, diagnostics: 0, truncated: false };
        match language {
            ImportLanguage::Rust => parser.rust(&mut canceled)?,
            _ => parser.javascript(&mut canceled)?,
        }
        check(&mut canceled)?;
        Ok(Self { source, request, generation, language, facts: parser.facts, seen: parser.seen,
            diagnostics: parser.diagnostics, truncated: parser.truncated, _lease: lease })
    }
    pub fn facts(&self) -> &[ImportFact] { &self.facts }
    pub const fn language(&self) -> ImportLanguage { self.language }
    pub const fn links_seen(&self) -> u64 { self.seen }
    pub const fn diagnostics(&self) -> usize { self.diagnostics }
    pub const fn truncated(&self) -> bool { self.truncated }
    pub const fn is_complete(&self) -> bool { !self.truncated && self.diagnostics == 0 }
    pub fn validate_delivery(&self, file: FileId, revision: SourceRevision,
        generation: QueryGeneration) -> Result<(), ImportError> {
        if file.owner() != self.request.file().owner() || revision.owner() != file.owner()
            || generation.owner() != file.owner() { return Err(ImportError::OwnerMismatch); }
        if file != self.request.file() || revision != self.request.revision() { return Err(ImportError::StaleSource); }
        if generation != self.generation { return Err(ImportError::StaleQuery); }
        Ok(())
    }
    pub fn validate_source(&self, source: &[u8], file: FileId, revision: SourceRevision,
        generation: QueryGeneration) -> Result<(), ImportError> {
        self.validate_delivery(file, revision, generation)?;
        if source != self.source { return Err(ImportError::StaleSource); }
        Ok(())
    }
    pub fn source_bytes(&self, id: u64) -> Result<&'source [u8], ImportError> {
        let ordinal = id.checked_sub(1).and_then(|n| usize::try_from(n).ok()).ok_or(ImportError::NotFound)?;
        let fact = self.facts.get(ordinal).ok_or(ImportError::NotFound)?;
        let (start, end) = fact.original_range.as_usize_bounds().map_err(|_| ImportError::InvalidEvidence)?;
        self.source.get(start..end).ok_or(ImportError::InvalidEvidence)
    }
}
#[derive(Clone, Copy)]
struct Terminal { start: usize, end: usize, kind: Tok }
impl Terminal {
    fn text<'a>(self, source: &'a str) -> &'a str { &source[self.start..self.end] }
}
// Grammar terminals are split ONLY inside validated upstream non-comment,
// non-string spans. We never classify a quote, comment delimiter or language
// escape locally. String spans remain opaque until a literal-specifier rule.
fn terminals(text: &str, spans: &[Span], canceled: &mut impl FnMut() -> bool) -> Result<Vec<Terminal>, ImportError> {
    let mut result = reserve(text.len())?;
    let mut next = 0;
    for span in spans {
        check(canceled)?;
        if span.start != next || span.end < span.start || text.get(span.start..span.end).is_none() {
            return Err(ImportError::InvalidEvidence);
        }
        next = span.end;
        if span.kind == Tok::Comment { continue; }
        if span.kind == Tok::Str {
            if span.start != span.end { result.push(Terminal { start: span.start, end: span.end, kind: span.kind }); }
            continue;
        }
        let mut at = span.start;
        while at < span.end {
            if at & 4095 == 0 { check(canceled)?; }
            let scalar = text[at..span.end].chars().next().ok_or(ImportError::InvalidEvidence)?;
            if scalar.is_whitespace() { at += scalar.len_utf8(); continue; }
            let start = at; at += scalar.len_utf8();
            if !punctuation(scalar) {
                while at < span.end {
                    let scalar = text[at..span.end].chars().next().ok_or(ImportError::InvalidEvidence)?;
                    if scalar.is_whitespace() || punctuation(scalar) { break; }
                    at += scalar.len_utf8();
                }
            }
            result.push(Terminal { start, end: at, kind: span.kind });
        }
    }
    if next != text.len() { return Err(ImportError::InvalidEvidence); }
    Ok(result)
}
fn punctuation(scalar: char) -> bool { scalar.is_ascii_punctuation() && scalar != '_' && scalar != '$' }
struct Statements<'a> {
    text: &'a str, map: &'a CaptureEncodingMap, tokens: &'a [Terminal], facts: Vec<ImportFact>,
    max_items: usize, seen: u64, diagnostics: usize, truncated: bool,
}
impl<'a> Statements<'a> {
    fn word(&self, at: usize) -> &'a str { self.tokens.get(at).map_or("", |token| token.text(self.text)) }
    fn keyword(&self, at: usize, word: &str) -> bool {
        self.tokens.get(at).is_some_and(|token| token.kind == Tok::Keyword && token.text(self.text) == word)
    }
    fn range(&self, start: usize, end: usize) -> Result<ByteRange, ImportError> {
        let range = DecodedUtf8Range::new(DecodedUtf8Offset::new(start as u64), DecodedUtf8Offset::new(end as u64))
            .map_err(|_| ImportError::InvalidEvidence)?;
        self.map.decoded_utf8_range_to_byte_range(range).map_err(|_| ImportError::InvalidEvidence)
    }
    fn add(&mut self, kind: ImportKind, start: usize, end: usize, spec_start: usize,
        spec_end: usize, destination: &str, allowed: bool) -> Result<(), ImportError> {
        if destination.is_empty() || destination.len() > MAX_IMPORT_SPECIFIER_BYTES {
            self.diagnostics += 1; return Ok(());
        }
        self.seen += 1;
        if self.facts.len() == self.max_items { self.truncated = true; return Ok(()); }
        let original_range = self.range(start, end)?;
        let specifier_range = self.range(spec_start, spec_end)?;
        if original_range.is_empty() || specifier_range.is_empty() { return Err(ImportError::InvalidEvidence); }
        let mut owned = String::new();
        owned.try_reserve_exact(destination.len()).map_err(|_| ImportError::ResourceDenied)?;
        if owned.capacity() > destination.len() { return Err(ImportError::ResourceDenied); }
        owned.push_str(destination);
        self.facts.push(ImportFact { id: self.facts.len() as u64 + 1, kind, destination: owned,
            original_range, specifier_range, resolution_allowed: allowed });
        Ok(())
    }
    fn rust(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
        // Explicit path attributes may change module layout. This initial policy
        // conservatively withholds ALL mod targets in such a file; facts remain.
        let overrides = (0..self.tokens.len()).any(|i| self.word(i) == "#" && self.word(i + 1) == "["
            && matches!(self.word(i + 2), "path" | "cfg_attr"));
        let mut depth = 0usize;
        let mut i = 0;
        while i < self.tokens.len() {
            check(canceled)?;
            if self.keyword(i, "mod") {
                let raw = self.word(i + 1) == "r" && self.word(i + 2) == "#" && identifier(self.word(i + 3))
                    && self.tokens[i + 1].end == self.tokens[i + 2].start
                    && self.tokens[i + 2].end == self.tokens[i + 3].start;
                let name_end = i + if raw { 3 } else { 1 };
                let semicolon = name_end + 1;
                if self.word(semicolon) == ";" && (raw || identifier(self.word(i + 1))) {
                    let start = self.tokens[i + 1].start; let end = self.tokens[name_end].end;
                    let text = self.text;
                    self.add(ImportKind::RustModule, self.tokens[i].start, self.tokens[semicolon].end,
                        start, end, &text[start..end], depth == 0 && !overrides)?;
                    i = semicolon + 1; continue;
                }
            }
            if self.keyword(i, "use") {
                let mut j = i + 1; let mut braces = 0usize;
                while j < self.tokens.len() && j - i <= MAX_STATEMENT_TERMINALS
                    && self.tokens[j].end - self.tokens[i].start <= MAX_STATEMENT_BYTES {
                    let word = self.word(j);
                    if word == ";" && braces == 0 { break; }
                    if word == "}" && braces == 0 { break; }
                    if j > i + 1 && (self.keyword(j, "use") || self.keyword(j, "mod")) { break; }
                    if word == "{" { braces += 1; } else if word == "}" { braces -= 1; }
                    j += 1;
                }
                if self.word(j) == ";" && j > i + 1 {
                    let start = self.tokens[i + 1].start; let end = self.tokens[j - 1].end;
                    let text = self.text; let destination = &text[start..end];
                    self.add(ImportKind::RustUse, self.tokens[i].start, self.tokens[j].end, start, end, destination, false)?;
                    i = j + 1; continue;
                }
                self.diagnostics += 1;
            }
            match self.word(i) {
                "{" | "(" | "[" => depth = depth.saturating_add(1),
                "}" | ")" | "]" => depth = depth.saturating_sub(1), _ => {}
            }
            i += 1;
        }
        Ok(())
    }
    fn javascript(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
        let mut i = 0;
        while i < self.tokens.len() {
            check(canceled)?;
            let import = self.keyword(i, "import"); let export = self.keyword(i, "export");
            if (!import && !export) || (i > 0 && matches!(self.word(i - 1), "." | "?")) { i += 1; continue; }
            let kind = if import { ImportKind::JavaScriptImport } else { ImportKind::JavaScriptExport };
            let next = self.word(i + 1);
            if import && next == "." { i += 1; continue; } // import.meta is not an import edge.
            if import && next == "(" { self.diagnostics += 1; i += 1; continue; }
            if export && !matches!(next, "*" | "{" | "type") { i += 1; continue; }
            let mut literal = None;
            let mut requires_source = import || next == "*";
            if import && self.tokens.get(i + 1).is_some_and(|token| token.kind == Tok::Str) { literal = Some(i + 1); }
            else {
                let mut depth = 0usize; let mut j = i + 1;
                while j < self.tokens.len() && j - i <= MAX_STATEMENT_TERMINALS
                    && self.tokens[j].end - self.tokens[i].start <= MAX_STATEMENT_BYTES {
                    let word = self.word(j);
                    if depth == 0 && word == "from" {
                        requires_source = true;
                        if self.tokens.get(j + 1).is_some_and(|token| token.kind == Tok::Str) { literal = Some(j + 1); }
                        break;
                    }
                    if depth == 0 && matches!(word, ";" | "(" | ")" | "=") { break; }
                    if j > i + 1 && depth == 0 && (self.keyword(j, "import") || self.keyword(j, "export")
                        || self.keyword(j, "const") || self.keyword(j, "function") || self.keyword(j, "class")) { break; }
                    if word == "{" { depth += 1; }
                    else if word == "}" { if depth == 0 { break; } depth -= 1; }
                    j += 1;
                }
            }
            if let Some(literal) = literal {
                let token = self.tokens[literal]; let text = token.text(self.text);
                let quote = text.as_bytes().first().copied();
                if matches!(quote, Some(b'\'' | b'"')) && text.len() >= 2 && text.as_bytes().last().copied() == quote {
                    let destination = &text[1..text.len() - 1];
                    let allowed = !destination.contains('\\') && !destination.chars().any(char::is_control)
                        && !destination.as_bytes().contains(&quote.ok_or(ImportError::InvalidEvidence)?);
                    let end = if self.word(literal + 1) == ";" { self.tokens[literal + 1].end } else { token.end };
                    self.add(kind, self.tokens[i].start, end, token.start + 1, token.end - 1, destination, allowed)?;
                    i = literal + 1; continue;
                }
                self.diagnostics += 1;
            } else if requires_source {
                self.diagnostics += 1;
            }
            i += 1;
        }
        Ok(())
    }
}
fn identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|scalar| scalar == '_' || scalar.is_alphabetic())
        && chars.all(|scalar| scalar == '_' || scalar.is_alphanumeric())
}
fn reserve<T>(capacity: usize) -> Result<Vec<T>, ImportError> {
    let mut result = Vec::new(); result.try_reserve_exact(capacity).map_err(|_| ImportError::ResourceDenied)?;
    if result.capacity() > capacity { return Err(ImportError::ResourceDenied); }
    Ok(result)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
    if canceled() { Err(ImportError::Canceled) } else { Ok(()) }
}

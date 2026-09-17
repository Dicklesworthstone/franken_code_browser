#![forbid(unsafe_code)]

//! Navigation adapter for the existing outline extractor, not another parser.
//!
//! The legacy language routes recognize only a subset of declarations and some
//! use line heuristics. Every non-fallback result here is therefore a CANDIDATE,
//! never compiler resolution or an exhaustive definition inventory. Source and
//! name ranges are validated against the decoded capture and mapped back to its
//! original bytes. Markdown stays with its upstream document owner.
//!
//! Worker work is bounded before invoking the non-resumable legacy extractor:
//! 64 KiB original/decoded source, 4096 lines, at most 128 opening braces for
//! Rust, and at most 128 bytes of Python indentation. These conservative guards
//! bound recursive extraction/destruction without trusting legacy max_depth or
//! max_items settings. Rejected files remain available to the ordinary reader.

use std::mem::size_of;
use fcb_core::{ByteLength, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, FileId,
    QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_source::{CaptureEncodingMap, CaptureRequest, DetectedEncoding, SpanKind, detect_encoding};
use crate::{ExtractorLimits, OutlineExtractor, OutlineItem, OutlineItemKind};

pub const MAX_SYMBOL_SOURCE_BYTES: usize = 64 * 1024;
pub const MAX_SYMBOL_ITEMS: usize = 4096;
const MAX_LINES: usize = 4096;
const MAX_RECURSIVE_SCOPES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolLanguage { Rust, Python, JavaScript, TypeScript, Go, Cpp }
impl SymbolLanguage {
    pub const fn name(self) -> &'static str {
        match self { Self::Rust => "rust", Self::Python => "python", Self::JavaScript => "javascript",
            Self::TypeScript => "typescript", Self::Go => "go", Self::Cpp => "cpp" }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "rust" | "rs" => Some(Self::Rust), "python" | "py" => Some(Self::Python),
            "javascript" | "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            "typescript" | "ts" | "tsx" => Some(Self::TypeScript), "go" => Some(Self::Go),
            "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" => Some(Self::Cpp), _ => None,
        }
    }
    /// Only inspect a raw filename suffix. No lossy path conversion or source I/O.
    pub fn from_path(path: &[u8]) -> Option<Self> {
        let name = path.rsplit(|&byte| byte == b'/').next()?;
        let dot = name.iter().rposition(|&byte| byte == b'.')?;
        let extension = name.get(dot + 1..)?;
        if extension.len() > 8 { return None; }
        let mut lowered = [0u8; 8];
        for (out, &byte) in lowered.iter_mut().zip(extension) { *out = byte.to_ascii_lowercase(); }
        Self::from_name(std::str::from_utf8(&lowered[..extension.len()]).ok()?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolError {
    SourceLimit, ComplexityLimit, InvalidLimits, UnsupportedEncoding, InvalidEvidence,
    ResourceDenied, Canceled, StaleSource, StaleQuery, OwnerMismatch, NotFound,
}
impl SymbolError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SourceLimit => "SYMBOL_SOURCE_LIMIT", Self::ComplexityLimit => "SYMBOL_COMPLEXITY_LIMIT",
            Self::InvalidLimits => "SYMBOL_INVALID_LIMITS", Self::UnsupportedEncoding => "SYMBOL_UNSUPPORTED_ENCODING",
            Self::InvalidEvidence => "SYMBOL_INVALID_EVIDENCE", Self::ResourceDenied => "SYMBOL_RESOURCE_DENIED",
            Self::Canceled => "SYMBOL_CANCELED", Self::StaleSource => "SYMBOL_STALE_SOURCE",
            Self::StaleQuery => "SYMBOL_STALE_QUERY", Self::OwnerMismatch => "SYMBOL_OWNER_MISMATCH",
            Self::NotFound => "SYMBOL_NOT_FOUND",
        }
    }
}
impl std::fmt::Display for SymbolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for SymbolError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolNameMode { Exact, Prefix, Contains }
#[derive(Clone, Debug)]
pub struct SymbolCandidate {
    id: u64,
    parent: Option<u64>,
    depth: usize,
    name: String,
    kind: OutlineItemKind,
    range: ByteRange,
    name_range: Option<ByteRange>,
    line: u64,
}
impl SymbolCandidate {
    pub const fn id(&self) -> u64 { self.id }
    pub const fn parent_id(&self) -> Option<u64> { self.parent }
    pub const fn depth(&self) -> usize { self.depth }
    pub fn name(&self) -> &str { &self.name }
    pub fn kind(&self) -> &OutlineItemKind { &self.kind }
    pub const fn original_range(&self) -> ByteRange { self.range }
    pub const fn name_range(&self) -> Option<ByteRange> { self.name_range }
    pub const fn line(&self) -> u64 { self.line }
    pub const fn evidence_level(&self) -> &'static str { "heuristic-outline-candidate" }
    pub fn matches_name(&self, needle: &str, mode: SymbolNameMode) -> bool {
        match mode { SymbolNameMode::Exact => self.name == needle,
            SymbolNameMode::Prefix => self.name.starts_with(needle), SymbolNameMode::Contains => self.name.contains(needle) }
    }
}

/// Borrows the exact source; results cannot outlive it. IDs are outline-local.
/// Retain the object for activation rather than applying an ordinal to another
/// capture with the same pathname. No FileId is ever inferred from a path.
pub struct CapturedSymbols<'source> {
    source: &'source [u8],
    request: CaptureRequest,
    generation: QueryGeneration,
    language: SymbolLanguage,
    encoding: DetectedEncoding,
    candidates: Vec<SymbolCandidate>,
    limited: bool,
    fallback: bool,
    _lease: ResourceLease,
}
impl<'source> CapturedSymbols<'source> {
    /// `source` is the complete immutable observation beginning at original
    /// byte zero. Extent requests are refused, never relabelled as whole files.
    pub fn build(source: &'source [u8], request: CaptureRequest, generation: QueryGeneration,
        language: SymbolLanguage, encoding: Option<DetectedEncoding>, max_items: usize,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, SymbolError> {
        if generation.owner() != request.file().owner() { return Err(SymbolError::OwnerMismatch); }
        if request.range().is_some() { return Err(SymbolError::InvalidEvidence); }
        if source.len() > MAX_SYMBOL_SOURCE_BYTES { return Err(SymbolError::SourceLimit); }
        if !(1..=MAX_SYMBOL_ITEMS).contains(&max_items) { return Err(SymbolError::InvalidLimits); }
        if canceled() { return Err(SymbolError::Canceled); }
        // Conservative envelope: decoder spans and Vec reallocation overlap,
        // token/name buffers, both outline trees, mapping traversal and output.
        // Borrowed source bytes retain their host's separate source reservation.
        let charge = source.len().checked_add(1).and_then(|n| n.checked_mul(768))
            .and_then(|n| max_items.checked_mul(size_of::<SymbolCandidate>() + 32).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(size_of::<Self>() + MAX_LINES * size_of::<usize>() + 4096))
            .ok_or(SymbolError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(request.file().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| SymbolError::ResourceDenied)?;
        let encoding = encoding.unwrap_or_else(|| detect_encoding(source));
        if encoding == DetectedEncoding::Unsupported { return Err(SymbolError::UnsupportedEncoding); }
        let map = CaptureEncodingMap::build_with_encoding(source, encoding).map_err(|_| SymbolError::UnsupportedEncoding)?;
        if map.spans().iter().any(|span| matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
            return Err(SymbolError::UnsupportedEncoding);
        }
        let text = map.decoded_text();
        if text.len() > MAX_SYMBOL_SOURCE_BYTES { return Err(SymbolError::SourceLimit); }
        let line_count = text.bytes().filter(|&b| b == b'\n').count() + 1;
        if line_count > MAX_LINES { return Err(SymbolError::ComplexityLimit); }
        if language == SymbolLanguage::Rust && text.bytes().filter(|&b| b == b'{').count() > MAX_RECURSIVE_SCOPES {
            return Err(SymbolError::ComplexityLimit);
        }
        if language == SymbolLanguage::Python && text.lines().any(|line| line.len() - line.trim_start().len() > MAX_RECURSIVE_SCOPES) {
            return Err(SymbolError::ComplexityLimit);
        }
        let mut line_starts = Vec::new();
        line_starts.try_reserve_exact(line_count).map_err(|_| SymbolError::ResourceDenied)?;
        if line_starts.capacity() > line_count { return Err(SymbolError::ResourceDenied); }
        line_starts.push(0);
        line_starts.extend(text.bytes().enumerate().filter_map(|(i, b)| (b == b'\n').then_some(i + 1)));
        if canceled() { return Err(SymbolError::Canceled); }
        // Do not pass the caller's display limit into extraction: nested items
        // outside earlier top-level results must not disappear during filtering.
        let extracted = OutlineExtractor::new(ExtractorLimits { max_items: MAX_SYMBOL_SOURCE_BYTES,
            max_depth: MAX_RECURSIVE_SCOPES, work_budget_bytes: MAX_SYMBOL_SOURCE_BYTES, lines_per_block: 50 })
            .extract(request.file(), request.revision(), language.name(), text.as_bytes());
        if canceled() { return Err(SymbolError::Canceled); }
        let mut candidates = Vec::new();
        candidates.try_reserve_exact(max_items).map_err(|_| SymbolError::ResourceDenied)?;
        if candidates.capacity() > max_items { return Err(SymbolError::ResourceDenied); }
        let mut limited = false;
        for item in &extracted.items {
            append(item, None, 0, &map, &line_starts, &mut candidates, max_items, &mut limited, &mut canceled)?;
        }
        let fallback = candidates.is_empty() && !source.is_empty();
        if canceled() { return Err(SymbolError::Canceled); }
        Ok(Self { source, request, generation, language, encoding, candidates, limited, fallback, _lease: lease })
    }
    pub const fn file(&self) -> FileId { self.request.file() }
    pub const fn revision(&self) -> SourceRevision { self.request.revision() }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn language(&self) -> SymbolLanguage { self.language }
    pub const fn encoding(&self) -> DetectedEncoding { self.encoding }
    pub fn candidates(&self) -> &[SymbolCandidate] { &self.candidates }
    pub const fn output_limited(&self) -> bool { self.limited }
    /// No recognized declaration, not proof that the file contains no symbols.
    pub const fn no_recognized_declarations(&self) -> bool { self.fallback }
    pub fn candidate(&self, id: u64) -> Option<&SymbolCandidate> {
        let index = usize::try_from(id.checked_sub(1)?).ok()?;
        self.candidates.get(index)
    }
    pub fn validate_delivery(&self, file: FileId, revision: SourceRevision, generation: QueryGeneration) -> Result<(), SymbolError> {
        if file.owner() != self.file().owner() || revision.owner() != self.file().owner()
            || generation.owner() != self.file().owner() { return Err(SymbolError::OwnerMismatch); }
        if file != self.file() || revision != self.revision() { return Err(SymbolError::StaleSource); }
        if generation != self.generation { return Err(SymbolError::StaleQuery); }
        Ok(())
    }
    /// Worker-side activation also checks the bytes, catching a caller that
    /// improperly reused a source identity for changed content. No hidden I/O.
    pub fn validate_source(&self, source: &[u8], file: FileId, revision: SourceRevision,
        generation: QueryGeneration) -> Result<(), SymbolError> {
        self.validate_delivery(file, revision, generation)?;
        if self.source != source { return Err(SymbolError::StaleSource); }
        Ok(())
    }
    pub fn source_bytes(&self, id: u64) -> Result<&'source [u8], SymbolError> {
        let candidate = self.candidate(id).ok_or(SymbolError::NotFound)?;
        let (start, end) = candidate.range.as_usize_bounds().map_err(|_| SymbolError::InvalidEvidence)?;
        self.source.get(start..end).ok_or(SymbolError::InvalidEvidence)
    }
    pub fn name_bytes(&self, id: u64) -> Result<Option<&'source [u8]>, SymbolError> {
        let candidate = self.candidate(id).ok_or(SymbolError::NotFound)?;
        candidate.name_range.map(|range| {
            let (start, end) = range.as_usize_bounds().map_err(|_| SymbolError::InvalidEvidence)?;
            self.source.get(start..end).ok_or(SymbolError::InvalidEvidence)
        }).transpose()
    }
}

fn mapped(map: &CaptureEncodingMap, start: u64, end: u64) -> Result<ByteRange, SymbolError> {
    let start_index = usize::try_from(start).map_err(|_| SymbolError::InvalidEvidence)?;
    let end_index = usize::try_from(end).map_err(|_| SymbolError::InvalidEvidence)?;
    if start_index >= end_index || map.decoded_text().get(start_index..end_index).is_none() {
        return Err(SymbolError::InvalidEvidence);
    }
    let decoded = DecodedUtf8Range::new(DecodedUtf8Offset::new(start), DecodedUtf8Offset::new(end))
        .map_err(|_| SymbolError::InvalidEvidence)?;
    map.decoded_utf8_range_to_byte_range(decoded).map_err(|_| SymbolError::InvalidEvidence)
}
#[allow(clippy::too_many_arguments)]
fn append(item: &OutlineItem, parent: Option<u64>, depth: usize, map: &CaptureEncodingMap, line_starts: &[usize],
    out: &mut Vec<SymbolCandidate>, limit: usize, limited: &mut bool, canceled: &mut impl FnMut() -> bool)
    -> Result<(), SymbolError> {
    if canceled() { return Err(SymbolError::Canceled); }
    if depth > MAX_RECURSIVE_SCOPES { return Err(SymbolError::ComplexityLimit); }
    if matches!(item.kind, OutlineItemKind::LineBlock { .. } | OutlineItemKind::Heading { .. }) { return Ok(()); }
    if out.len() == limit { *limited = true; return Ok(()); }
    let range = mapped(map, item.evidence.byte_start, item.evidence.byte_end)?;
    let name_range = match item.evidence.name_range {
        Some((start, end)) => {
            if start < item.evidence.byte_start || end > item.evidence.byte_end { return Err(SymbolError::InvalidEvidence); }
            let begin = usize::try_from(start).map_err(|_| SymbolError::InvalidEvidence)?;
            let finish = usize::try_from(end).map_err(|_| SymbolError::InvalidEvidence)?;
            if map.decoded_text().get(begin..finish) != Some(item.name.as_str()) { return Err(SymbolError::InvalidEvidence); }
            Some(mapped(map, start, end)?)
        }
        None => None,
    };
    // LF-delimited logical position, never a visual column or byte-as-UTF16 index.
    let line = line_starts.partition_point(|&start| start as u64 <= item.evidence.byte_start) as u64;
    let id = out.len() as u64 + 1;
    out.push(SymbolCandidate { id, parent, depth, name: item.name.clone(), kind: item.kind.clone(), range, name_range, line });
    for child in &item.children { append(child, Some(id), depth + 1, map, line_starts, out, limit, limited, canceled)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{ArenaOwnerId, ByteOffset};
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(831).unwrap() }
    fn request(rev: u64) -> CaptureRequest {
        CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), rev).unwrap()).unwrap()
    }
    fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
    fn build<'a>(source: &'a [u8], language: SymbolLanguage, limit: usize, budget: &ResourceBudget) -> CapturedSymbols<'a> {
        CapturedSymbols::build(source, request(1), generation(), language, None, limit, budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap()
    }
    #[test]
    fn rust_candidates_have_real_original_bytes_and_parent_identity() {
        let source = b"// fn fake() {}\npub struct Thing {}\nimpl Thing { fn run(&self) {} }\nfn run() {}";
        let budget = budget(); let outline = build(source, SymbolLanguage::Rust, 32, &budget);
        let runs: Vec<_> = outline.candidates().iter().filter(|c| c.matches_name("run", SymbolNameMode::Exact)).collect();
        assert_eq!(runs.len(), 2); assert!(runs[0].parent_id().is_some()); assert!(runs[1].parent_id().is_none());
        for candidate in runs {
            assert_eq!(outline.name_bytes(candidate.id()).unwrap().unwrap(), b"run");
            assert!(outline.source_bytes(candidate.id()).unwrap().starts_with(b"fn run"));
            assert_eq!(candidate.evidence_level(), "heuristic-outline-candidate");
        }
        assert!(!outline.candidates().iter().any(|c| c.name() == "fake"));
    }
    #[test]
    fn utf16_is_decoded_before_extraction_and_ranges_map_to_original_units() {
        for little in [false, true] {
            let text = "// 😀\r\nfn hello() {}\n";
            let mut raw = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
            for unit in text.encode_utf16() { raw.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
            let budget = budget(); let outline = build(&raw, SymbolLanguage::Rust, 32, &budget);
            let item = &outline.candidates()[0]; assert_eq!(item.name(), "hello"); assert_eq!(item.line(), 2);
            let expected: Vec<u8> = "hello".encode_utf16().flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
            assert_eq!(outline.name_bytes(item.id()).unwrap().unwrap(), expected);
            assert_eq!(item.original_range().start().get(), 16);
        }
    }
    #[test]
    fn python_and_typescript_routes_remain_candidates_not_compiler_answers() {
        for (language, source, name) in [(SymbolLanguage::Python, b"class Thing:\n    def work(self):\n        pass\n".as_slice(), "work"),
            (SymbolLanguage::TypeScript, b"export function work() {}\n", "work")] {
            let budget = budget(); let outline = build(source, language, 32, &budget);
            let candidate = outline.candidates().iter().find(|candidate| candidate.name() == name).unwrap();
            assert!(outline.source_bytes(candidate.id()).unwrap().windows(name.len()).any(|window| window == name.as_bytes()));
            assert_eq!(candidate.name_range(), None);
        }
    }
    #[test]
    fn malformed_bytes_unknown_extensions_and_recursive_inputs_are_not_fabricated_symbols() {
        assert_eq!(SymbolLanguage::from_path(b"docs/README.md"), None);
        assert_eq!(SymbolLanguage::from_path(b"src/\xffname.RS"), Some(SymbolLanguage::Rust));
        let budget = budget();
        assert!(matches!(CapturedSymbols::build(b"fn x() {}\xff", request(1), generation(), SymbolLanguage::Rust, None, 32,
            &budget, ResourceAllocationId::new(1).unwrap(), || false), Err(SymbolError::UnsupportedEncoding)));
        let source = b"impl X {".repeat(129);
        assert!(matches!(CapturedSymbols::build(&source, request(1), generation(), SymbolLanguage::Rust, None, 32,
            &budget, ResourceAllocationId::new(1).unwrap(), || false), Err(SymbolError::ComplexityLimit)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
    #[test]
    fn output_limit_stale_delivery_and_cancellation_are_separate() {
        let budget = budget(); let source = b"fn a() {}\nfn b() {}\n";
        let outline = build(source, SymbolLanguage::Rust, 1, &budget);
        assert_eq!(outline.candidates().len(), 1); assert!(outline.output_limited());
        assert_eq!(outline.validate_delivery(request(2).file(), request(2).revision(), generation()), Err(SymbolError::StaleSource));
        assert_eq!(outline.validate_delivery(request(1).file(), request(1).revision(), QueryGeneration::new(owner(), 2).unwrap()), Err(SymbolError::StaleQuery));
        drop(outline);
        assert!(matches!(CapturedSymbols::build(source, request(1), generation(), SymbolLanguage::Rust, None, 32,
            &budget, ResourceAllocationId::new(1).unwrap(), || budget.accounting().reserved().get() > 0), Err(SymbolError::Canceled)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
    #[test]
    fn extent_requests_cannot_be_interpreted_as_whole_source() {
        let budget = budget();
        let range = ByteRange::new(ByteOffset::new(100), ByteOffset::new(109)).unwrap();
        assert!(matches!(CapturedSymbols::build(b"fn a() {}", request(1).with_range(range).unwrap(), generation(),
            SymbolLanguage::Rust, None, 32, &budget, ResourceAllocationId::new(1).unwrap(), || false), Err(SymbolError::InvalidEvidence)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
    #[test]
    fn reused_id_with_different_bytes_is_not_a_valid_navigation_target() {
        let budget = budget(); let outline = build(b"fn old() {}", SymbolLanguage::Rust, 32, &budget);
        assert_eq!(outline.validate_source(b"fn new() {}", request(1).file(), request(1).revision(), generation()), Err(SymbolError::StaleSource));
        assert!(outline.validate_source(b"fn old() {}", request(1).file(), request(1).revision(), generation()).is_ok());
    }
}

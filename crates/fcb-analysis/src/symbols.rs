#![forbid(unsafe_code)]

//! Navigation adapters over shared upstream lexers.
//!
//! Every result is a heuristic declaration candidate, never compiler resolution
//! or an exhaustive definition inventory. Source and name ranges are validated
//! against the decoded capture and mapped back to its original bytes. Markdown
//! stays with its upstream document owner.
//!
//! Worker work is bounded by 1 MiB original/decoded source and 65,536 lines.
//! Rejected files remain available to the ordinary reader. Rust, Python,
//! JavaScript/JSX, TypeScript/TSX, Go, C/C++, Swift, Java and C# use pinned upstream
//! lexers and bounded declaration recognizers; they do not introduce lexers or
//! compiler-semantic claims. Python bounds declaration scopes and indentation
//! iteratively without inspecting the contents of opaque multiline strings.

use std::mem::size_of;
use fcb_core::{ByteLength, ByteRange, FileId,
    QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_source::{CaptureRequest, DetectedEncoding, detect_encoding};
use crate::OutlineItemKind;

mod source_map;
use source_map::SourceMap;

mod swift;
mod managed;
mod rust;
mod javascript;
mod go;
mod python;
mod cpp;

pub const MAX_SYMBOL_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_SYMBOL_ITEMS: usize = 4096;
const MAX_LINES: usize = 65_536;
const MAX_RECURSIVE_SCOPES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolLanguage { Rust, Python, JavaScript, TypeScript, Jsx, Tsx, Go, Cpp, Swift, Java, CSharp }
impl SymbolLanguage {
    pub const fn name(self) -> &'static str {
        match self { Self::Rust => "rust", Self::Python => "python", Self::JavaScript => "javascript",
            Self::TypeScript => "typescript", Self::Jsx => "jsx", Self::Tsx => "tsx",
            Self::Go => "go", Self::Cpp => "cpp", Self::Swift => "swift",
            Self::Java => "java", Self::CSharp => "csharp" }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "rust" | "rs" => Some(Self::Rust), "python" | "py" => Some(Self::Python),
            "javascript" | "js" | "mjs" | "cjs" => Some(Self::JavaScript),
            "typescript" | "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "jsx" => Some(Self::Jsx), "tsx" => Some(Self::Tsx), "go" => Some(Self::Go),
            "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" => Some(Self::Cpp),
            "swift" => Some(Self::Swift), "java" => Some(Self::Java),
            "csharp" | "cs" | "c#" => Some(Self::CSharp), _ => None,
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
    SourceLimit, ComplexityLimit, InvalidLimits, UnsupportedEncoding, UnsupportedLexicalContext, InvalidEvidence,
    ResourceDenied, Canceled, StaleSource, StaleQuery, OwnerMismatch, NotFound,
}
impl SymbolError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SourceLimit => "SYMBOL_SOURCE_LIMIT", Self::ComplexityLimit => "SYMBOL_COMPLEXITY_LIMIT",
            Self::InvalidLimits => "SYMBOL_INVALID_LIMITS", Self::UnsupportedEncoding => "SYMBOL_UNSUPPORTED_ENCODING",
            Self::UnsupportedLexicalContext => "SYMBOL_UNSUPPORTED_LEXICAL_CONTEXT",
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
        let encoding = encoding.unwrap_or_else(|| detect_encoding(source));
        if encoding == DetectedEncoding::Unsupported { return Err(SymbolError::UnsupportedEncoding); }
        let decoded_capacity = SourceMap::decoded_capacity(source.len(), encoding)?;
        let charge = build_charge(source.len(), decoded_capacity, max_items)?;
        // Borrowed source bytes retain their host's separate source reservation.
        // This lease admits transient decode/lexical work and output together;
        // it shrinks to retained output only after all scratch has been dropped.
        let lease = budget.try_reserve_managed(request.file().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| SymbolError::ResourceDenied)?;
        let map = SourceMap::build(source, encoding, &mut canceled)?;
        let line_starts = source_line_starts(map.decoded_text(), &mut canceled)?;
        if canceled() { return Err(SymbolError::Canceled); }
        let (candidates, limited) = match language {
            SymbolLanguage::Rust => rust::extract(&map, &line_starts, max_items, &mut canceled)?,
            SymbolLanguage::JavaScript | SymbolLanguage::TypeScript | SymbolLanguage::Jsx | SymbolLanguage::Tsx =>
                javascript::extract(&map, language, &line_starts, max_items, &mut canceled)?,
            SymbolLanguage::Swift => swift::extract(&map, &line_starts, max_items, &mut canceled)?,
            SymbolLanguage::Java | SymbolLanguage::CSharp =>
                managed::extract(&map, language, &line_starts, max_items, &mut canceled)?,
            SymbolLanguage::Go => go::extract(&map, &line_starts, max_items, &mut canceled)?,
            SymbolLanguage::Python => python::extract(&map, &line_starts, max_items, &mut canceled)?,
            SymbolLanguage::Cpp => cpp::extract(&map, &line_starts, max_items, &mut canceled)?,
        };
        let fallback = candidates.is_empty() && !source.is_empty();
        if canceled() { return Err(SymbolError::Canceled); }
        drop(line_starts);
        drop(map);
        let retained = candidates.iter().try_fold(
            candidates.capacity().checked_mul(size_of::<SymbolCandidate>())
                .and_then(|bytes| bytes.checked_add(size_of::<Self>()))
                .ok_or(SymbolError::ResourceDenied)?,
            |bytes, candidate| bytes.checked_add(candidate.name.capacity()).ok_or(SymbolError::ResourceDenied))?;
        if retained > charge { return Err(SymbolError::ResourceDenied); }
        lease.reconcile(ByteLength::new(retained as u64)).map_err(|_| SymbolError::ResourceDenied)?;
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

/// Conservative peak for the pinned upstream lexers and these adapters.
/// The 128-byte lexical allowance per decoded byte covers a pre-reserved Span
/// vector, JSX/TSX mode, interpolation and template stacks with old/new growth overlap,
/// and the separate Span/Terminal or two-Span validation phases. Upstream
/// whole-block calls remain bounded worker jobs with cancellation around them;
/// this adapter does not claim to make those calls resumable.
fn build_charge(source_bytes: usize, decoded_capacity: usize, max_items: usize)
    -> Result<usize, SymbolError> {
    SourceMap::workspace_bytes(source_bytes, decoded_capacity)?
        .checked_add(decoded_capacity.max(1).checked_mul(128).ok_or(SymbolError::ResourceDenied)?)
        // The common name allocator enforces this aggregate output allowance,
        // including synthesized Rust impl labels, before allocating any name.
        .and_then(|bytes| decoded_capacity.checked_mul(2).and_then(|names| bytes.checked_add(names)))
        .and_then(|bytes| max_items.checked_mul(size_of::<SymbolCandidate>() + 32)
            .and_then(|items| bytes.checked_add(items)))
        .and_then(|bytes| MAX_LINES.checked_mul(size_of::<usize>()).and_then(|lines| bytes.checked_add(lines)))
        .and_then(|bytes| (MAX_RECURSIVE_SCOPES + 1).checked_mul(12 * size_of::<usize>())
            .and_then(|scopes| bytes.checked_add(scopes)))
        .and_then(|bytes| bytes.checked_add(size_of::<CapturedSymbols<'_>>() + 4096))
        .ok_or(SymbolError::ResourceDenied)
}

/// Decoded offsets for the same physical source lines as fcb-source's reader:
/// LF, CRLF and bare CR terminate one line; a terminal delimiter does not create
/// another line without a following byte. These are location receipts, not
/// language statement boundaries, so lexer-specific CR guards remain in force.
fn source_line_starts(text: &str, canceled: &mut impl FnMut() -> bool)
    -> Result<Vec<usize>, SymbolError> {
    // The outer admission already reserves MAX_LINES entries. No source can
    // contain more nonempty physical-line records than decoded bytes.
    let capacity = text.len().min(MAX_LINES);
    let mut starts = Vec::new();
    starts.try_reserve_exact(capacity).map_err(|_| SymbolError::ResourceDenied)?;
    if starts.capacity() > capacity { return Err(SymbolError::ResourceDenied); }
    let mut pending = true;
    let mut previous_cr = false;
    for (offset, byte) in text.bytes().enumerate() {
        if offset % 1024 == 0 && canceled() { return Err(SymbolError::Canceled); }
        if previous_cr {
            previous_cr = false;
            if byte == b'\n' { continue; }
        }
        if pending {
            if starts.len() == MAX_LINES { return Err(SymbolError::ComplexityLimit); }
            starts.push(offset);
            pending = false;
        }
        match byte {
            b'\r' => { pending = true; previous_cr = true; }
            b'\n' => pending = true,
            _ => {}
        }
    }
    if canceled() { return Err(SymbolError::Canceled); }
    Ok(starts)
}

fn mapped(map: &SourceMap<'_>, start: u64, end: u64) -> Result<ByteRange, SymbolError> {
    map.mapped(start, end)
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
            assert_eq!(outline.name_bytes(candidate.id()).unwrap().unwrap(), name.as_bytes());
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
    #[test]
    fn scalar_dense_source_uses_bounded_scratch_and_retains_only_candidate_capacity() {
        let limit = 64 * 1024;
        let tail = b"*/\nfn kept() {}\n";
        let mut source = b"/*".to_vec();
        while source.len() + "中".len() + tail.len() <= limit {
            source.extend_from_slice("中".as_bytes());
        }
        source.resize(limit - tail.len(), b' ');
        source.extend_from_slice(tail);
        let budget = ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap();
        // The old whole-capture map charged over 48 MiB for these same 64 KiB.
        let outline = build(&source, SymbolLanguage::Rust, 32, &budget);
        assert_eq!(outline.candidates().len(), 1);
        assert_eq!(outline.candidates()[0].name(), "kept");
        assert_eq!(outline.name_bytes(1).unwrap().unwrap(), b"kept");
        let retained = size_of::<CapturedSymbols<'_>>()
            + outline.candidates.capacity() * size_of::<SymbolCandidate>()
            + outline.candidates.iter().map(|candidate| candidate.name.capacity()).sum::<usize>();
        assert_eq!(budget.accounting().managed().get(), retained as u64);
        assert!(budget.accounting().peak_reserved().get() > retained as u64 * 100);
        assert!(budget.accounting().peak_reserved().get() <= 16 * 1024 * 1024);
        drop(outline);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }

    #[test]
    fn limited_results_still_validate_the_complete_malformed_suffix() {
        let mut source = b"fn first() {}\n".to_vec();
        source.extend_from_slice(&b" ".repeat(8192));
        source.push(0xf0);
        let budget = budget();
        assert!(matches!(CapturedSymbols::build(&source, request(1), generation(),
            SymbolLanguage::Rust, None, 1, &budget, ResourceAllocationId::new(1).unwrap(), || false),
            Err(SymbolError::UnsupportedEncoding)));
        assert_eq!(budget.accounting().reserved().get(), 0);
        let outline = build(b"fn retry() {}", SymbolLanguage::Rust, 1, &budget);
        assert_eq!(outline.candidates()[0].name(), "retry");
    }

    #[test]
    fn admission_and_decode_cancellation_release_the_allocation_for_retry() {
        let source = b" ".repeat(64 * 1024);
        let budget = ResourceBudget::new(owner(), ByteLength::new(2 * 1024 * 1024)).unwrap();
        assert!(matches!(CapturedSymbols::build(&source, request(1), generation(),
            SymbolLanguage::Rust, None, 1, &budget, ResourceAllocationId::new(1).unwrap(), || false),
            Err(SymbolError::ResourceDenied)));
        assert_eq!(budget.accounting().reserved().get(), 0);
        let mut polls = 0;
        assert!(matches!(CapturedSymbols::build(b"fn retry() {}", request(1), generation(),
            SymbolLanguage::Rust, None, 1, &budget, ResourceAllocationId::new(1).unwrap(), || {
                polls += 1; polls == 4
            }), Err(SymbolError::Canceled)));
        assert_eq!(budget.accounting().reserved().get(), 0);
        let outline = build(b"fn retry() {}", SymbolLanguage::Rust, 1, &budget);
        assert_eq!(outline.candidates()[0].name(), "retry");
        drop(outline);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }

    fn encoded_source(text: &str, little: Option<bool>) -> Vec<u8> {
        let Some(little) = little else { return text.as_bytes().to_vec(); };
        let mut source = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in text.encode_utf16() {
            source.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
        }
        source
    }

    #[test]
    fn every_language_reaches_declarations_beyond_the_old_byte_and_line_caps() {
        let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
        let cases = [
            (SymbolLanguage::Rust, "//", "fn distant() {}\n", "distant"),
            (SymbolLanguage::Python, "#", "def distant():\n    pass\n", "distant"),
            (SymbolLanguage::JavaScript, "//", "function distant() {}\n", "distant"),
            (SymbolLanguage::TypeScript, "//", "function distant() {}\n", "distant"),
            (SymbolLanguage::Jsx, "//", "function distant() {}\n", "distant"),
            (SymbolLanguage::Tsx, "//", "function distant() {}\n", "distant"),
            (SymbolLanguage::Go, "//", "func distant() {}\n", "distant"),
            (SymbolLanguage::Cpp, "//", "void distant() {}\n", "distant"),
            (SymbolLanguage::Swift, "//", "func distant() {}\n", "distant"),
            (SymbolLanguage::Java, "//", "class Distant {}\n", "Distant"),
            (SymbolLanguage::CSharp, "//", "class Distant {}\n", "Distant"),
        ];
        for (language, comment, declaration, name) in cases {
            let text = format!("{}{}", format!("{comment} retained padding\n").repeat(5000), declaration);
            assert!(text.len() > 64 * 1024);
            for little in [None, Some(false), Some(true)] {
                let source = encoded_source(&text, little);
                let outline = build(&source, language, 32, &budget);
                let candidate = outline.candidates().iter().find(|item| item.name() == name)
                    .unwrap_or_else(|| panic!("missing {name} in {}", language.name()));
                assert_eq!(candidate.line(), 5001);
                assert_eq!(candidate.original_range().start().get(),
                    encoded_source(&text[..text.len() - declaration.len()], little).len() as u64);
                let expected = encoded_source(name, little);
                let expected = if little.is_some() { &expected[2..] } else { &expected[..] };
                assert_eq!(outline.name_bytes(candidate.id()).unwrap().unwrap(), expected);
                assert!(!outline.output_limited());
                drop(outline);
                assert_eq!(budget.accounting().reserved().get(), 0);
            }
        }
    }

    #[test]
    fn exact_original_and_decoded_mebibyte_boundaries_are_admitted_independently() {
        let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
        let tail = "*/\nfn edge() {}\n";
        let mut text = String::from("/*");
        while text.len() + "中".len() + tail.len() <= MAX_SYMBOL_SOURCE_BYTES {
            text.push('中');
        }
        while text.len() + tail.len() < MAX_SYMBOL_SOURCE_BYTES { text.push(' '); }
        text.push_str(tail);
        assert_eq!(text.len(), MAX_SYMBOL_SOURCE_BYTES);
        for little in [None, Some(false), Some(true)] {
            let source = encoded_source(&text, little);
            assert!(source.len() <= MAX_SYMBOL_SOURCE_BYTES);
            let outline = build(&source, SymbolLanguage::Rust, 1, &budget);
            assert_eq!(outline.candidates()[0].name(), "edge");
            assert_eq!(outline.candidates()[0].line(), 2);
            assert!(!outline.output_limited());
            assert!(budget.accounting().peak_reserved().get() <= 160 * 1024 * 1024);
            drop(outline);
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
        let over = b" ".repeat(MAX_SYMBOL_SOURCE_BYTES + 1);
        assert!(matches!(CapturedSymbols::build(&over, request(1), generation(),
            SymbolLanguage::Rust, None, 1, &budget, ResourceAllocationId::new(1).unwrap(), || false),
            Err(SymbolError::SourceLimit)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }

    #[test]
    fn larger_line_ceiling_counts_crlf_and_bare_cr_without_a_phantom_final_line() {
        for delimiter in ["\n", "\r\n", "\r"] {
            let mut text = delimiter.repeat(MAX_LINES - 1);
            text.push('x');
            let starts = source_line_starts(&text, &mut || false).unwrap();
            assert_eq!(starts.len(), MAX_LINES);
            assert_eq!(starts[MAX_LINES - 1], text.len() - 1);
            text.push_str(delimiter);
            assert_eq!(source_line_starts(&text, &mut || false).unwrap().len(), MAX_LINES);
            text.push('x');
            assert_eq!(source_line_starts(&text, &mut || false), Err(SymbolError::ComplexityLimit));
        }
    }

}

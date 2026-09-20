#![forbid(unsafe_code)]

//! Source-bound code navigation for persistent reading desks. Uses the supported
//! symbol/reference facade, not another parser, lexer or definition resolver.
//! These objects retain exact source pins plus bounded presentation records.
//! Filtering/paging never extracts again; selecting uses ordinary desk history.
//! Prepare/encode/drop are worker operations, not native input/paint callbacks.

use std::mem::size_of;
use fcb::search::symbols::{SymbolCandidate, SymbolOptions, SymbolError, ReferenceCandidate,
    ReferenceOptions, ReferenceError, MAX_SYMBOL_ITEMS};
pub use fcb::search::{SymbolLanguage, SymbolNameMode};
use super::{DeskSession, DeskSessionError, DeskPaneId, DeskView, DeskError, DeskChange,
    DeskCommand, ByteLength, QueryGeneration, ResourceLease,
    Output, OutputError, HostResponse, EXIT_OK, EXIT_PARTIAL, check};

pub const MAX_CODE_PAGE: usize = 128;
pub const MAX_CODE_NAME_QUERY_BYTES: usize = 1024;
const MAX_CODE_RECORD_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeskCodeError {
    Desk(DeskSessionError), Symbol(SymbolError), Reference(ReferenceError),
    UnsupportedLanguage, StaleSource, StaleGeneration, InvalidPage,
}
impl std::fmt::Display for DeskCodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Desk(e) => write!(f, "{e}"), Self::Symbol(e) => write!(f, "{e}"),
            Self::Reference(e) => write!(f, "{e}"),
            Self::UnsupportedLanguage => f.write_str("DESK_CODE_UNSUPPORTED_LANGUAGE"),
            Self::StaleSource => f.write_str("DESK_CODE_STALE_SOURCE"),
            Self::StaleGeneration => f.write_str("DESK_CODE_STALE_GENERATION"),
            Self::InvalidPage => f.write_str("DESK_CODE_INVALID_PAGE"),
        }
    }
}
impl std::error::Error for DeskCodeError {}
impl From<DeskSessionError> for DeskCodeError { fn from(e: DeskSessionError) -> Self { Self::Desk(e) } }
impl From<DeskError> for DeskCodeError { fn from(e: DeskError) -> Self { Self::Desk(e.into()) } }
impl From<SymbolError> for DeskCodeError { fn from(e: SymbolError) -> Self { Self::Symbol(e) } }
impl From<ReferenceError> for DeskCodeError { fn from(e: ReferenceError) -> Self { Self::Reference(e) } }
impl From<OutputError> for DeskCodeError { fn from(e: OutputError) -> Self { Self::Desk(e.into()) } }
impl DeskCodeError {
    pub fn is_canceled(self) -> bool {
        match self { Self::Desk(e) => e.is_canceled(), Self::Symbol(SymbolError::Canceled)
            | Self::Reference(ReferenceError::Canceled) => true, _ => false }
    }
}

struct CodeSource { pane: DeskPaneId, view: DeskView }
impl CodeSource {
    fn pin(desk: &mut DeskSession, expected: u64, pane: DeskPaneId) -> Result<Self, DeskCodeError> {
        desk.model().source(pane, expected)?;
        let id = desk.next_id()?;
        Ok(Self { pane, view: desk.model().view(pane, expected, id)? })
    }
    fn validate(&self, desk: &DeskSession, expected: u64) -> Result<(), DeskCodeError> {
        let current = desk.model().source(self.pane, expected)?;
        let captured = self.view.source();
        if current.file() != captured.file() || current.revision() != captured.revision()
            || !std::ptr::eq(current.bytes(), captured.bytes()) { return Err(DeskCodeError::StaleSource); }
        Ok(())
    }
    fn output(&self, desk: &mut DeskSession, command: &str) -> Result<Output, DeskCodeError> {
        let mut out = desk.output(command)?;
        out.literal(",\"pane\":")?; out.integer(self.pane.get())?;
        out.literal(",\"file_id\":")?; out.integer(self.view.source().file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(self.view.source().revision().get())?;
        out.literal(",\"scope\":\"one-retained-source\",\"coordinate_domain\":\"original-source-bytes\",\"line_semantics\":\"LF-counted-source-lines\",\"encoding_policy\":\"reader-auto-detected\",\"compiler_resolved\":false")?;
        Ok(out)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DeskOutlineOptions { pub language: Option<SymbolLanguage>, pub max_items: usize }
impl Default for DeskOutlineOptions {
    fn default() -> Self { Self { language: None, max_items: MAX_SYMBOL_ITEMS } }
}

/// Immutable declaration-candidate inventory. Hosts supply fresh, non-reused
/// generations and retain the old object until replacement/response acceptance.
/// The CLI implements that lifecycle independently for outlines and references.
pub struct DeskOutline {
    source: CodeSource,
    generation: u64,
    language: SymbolLanguage,
    symbols: Vec<SymbolCandidate>,
    limited: bool,
    no_declarations: bool,
    _lease: ResourceLease,
}
impl DeskOutline {
    pub fn prepare(desk: &mut DeskSession, expected: u64, pane: DeskPaneId,
        generation: u64, options: DeskOutlineOptions, mut canceled: impl FnMut() -> bool)
        -> Result<Self, DeskCodeError> {
        let capture = desk.model().source(pane, expected)?;
        let language = options.language.or_else(|| SymbolLanguage::from_path(capture.logical_path().as_bytes()))
            .ok_or(DeskCodeError::UnsupportedLanguage)?;
        let qgen = QueryGeneration::new(desk.model().owner(), generation).map_err(|_| DeskCodeError::StaleGeneration)?;
        check(&mut canceled)?;
        let source = CodeSource::pin(desk, expected, pane)?;
        let [extract_id, rows_id] = desk.ids()?;
        let extracted = source.view.view().symbols(SymbolOptions { generation: qgen, language,
            encoding: None, max_items: options.max_items }, &desk.budget, extract_id, &mut canceled)?;
        let count = extracted.candidates().len();
        let mut charge = size_of::<Self>() + count * size_of::<SymbolCandidate>();
        for item in extracted.candidates() {
            check(&mut canceled)?;
            charge = charge.checked_add(2 * item.name().len() + 32).ok_or(DeskError::ResourceDenied)?;
        }
        if charge > MAX_CODE_RECORD_BYTES { return Err(DeskError::ResourceDenied.into()); }
        let lease = desk.budget.try_reserve_managed(desk.model().owner(), rows_id, ByteLength::new(charge as u64))
            .map_err(|_| DeskError::ResourceDenied)?;
        let mut symbols = reserve(count)?;
        // Clone only the engine's small owned candidate records, not source or
        // decoder maps. The original extraction reservation is released below.
        for item in extracted.candidates() { check(&mut canceled)?; symbols.push(item.clone()); }
        let limited = extracted.output_limited();
        let no_declarations = extracted.no_recognized_declarations();
        drop(extracted);
        check(&mut canceled)?;
        Ok(Self { source, generation, language, symbols, limited, no_declarations, _lease: lease })
    }
    pub fn pane(&self) -> DeskPaneId { self.source.pane }
    pub const fn generation(&self) -> u64 { self.generation }
    pub const fn output_limited(&self) -> bool { self.limited }
    pub fn validate_source(&self, desk: &DeskSession, expected: u64) -> Result<(), DeskCodeError> {
        self.source.validate(desk, expected)
    }
    pub fn candidates(&self, desk: &DeskSession, expected: u64, generation: u64)
        -> Result<&[SymbolCandidate], DeskCodeError> {
        self.validate(desk, expected, generation)?; Ok(&self.symbols)
    }
    fn validate(&self, desk: &DeskSession, expected: u64, generation: u64) -> Result<(), DeskCodeError> {
        self.source.validate(desk, expected)?;
        if generation != self.generation { return Err(DeskCodeError::StaleGeneration); }
        Ok(())
    }
    fn candidate(&self, id: u64) -> Result<&SymbolCandidate, DeskCodeError> {
        let at = id.checked_sub(1).and_then(|n| usize::try_from(n).ok()).ok_or(SymbolError::NotFound)?;
        self.symbols.get(at).filter(|s| s.id() == id).ok_or_else(|| SymbolError::NotFound.into())
    }
    /// Empty needle lists all candidates. Filtering retains original parent/ID
    /// relationships; an absent filtered parent is not silently reparented.
    pub fn page(&self, desk: &mut DeskSession, expected: u64, generation: u64,
        needle: &str, mode: SymbolNameMode, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskCodeError> {
        self.validate(desk, expected, generation)?; check(&mut canceled)?;
        if needle.len() > MAX_CODE_NAME_QUERY_BYTES { return Err(DeskCodeError::InvalidPage); }
        let matches = |s: &&SymbolCandidate| needle.is_empty() || s.matches_name(needle, mode);
        let mut count = 0;
        for item in &self.symbols { check(&mut canceled)?; if matches(&item) { count += 1; } }
        page_limits(start, limit, count)?;
        let end = start.saturating_add(limit).min(count);
        let mut out = self.source.output(desk, "code-symbols")?;
        out.literal(",\"outline_generation\":")?; out.integer(self.generation)?;
        out.literal(",\"language\":")?; out.quoted(self.language.name())?;
        out.literal(",\"evidence_level\":\"heuristic-outline-candidate\",\"semantic_complete\":false,\"count_basis\":\"retained-candidates\",\"output_limited\":")?;
        out.boolean(self.limited)?;
        out.literal(",\"no_recognized_declarations\":")?; out.boolean(self.no_declarations)?;
        out.literal(",\"retained_symbols\":")?; out.integer(self.symbols.len() as u64)?;
        out.literal(",\"matched_symbols\":")?; out.integer(count as u64)?;
        out.literal(",\"needle\":")?; out.quoted(needle)?;
        out.literal(",\"name_mode\":")?;
        out.quoted(match mode { SymbolNameMode::Exact => "exact", SymbolNameMode::Prefix => "prefix", SymbolNameMode::Contains => "contains" })?;
        out.literal(",\"case_sensitive\":true,\"symbols\":[")?;
        for (i, item) in self.symbols.iter().filter(matches).skip(start).take(limit).enumerate() {
            check(&mut canceled)?;
            if i > 0 { out.literal(",")?; }
            out.literal("{\"symbol_id\":")?; out.integer(item.id())?;
            out.literal(",\"parent_id\":")?; optional(&mut out, item.parent_id())?;
            out.literal(",\"depth\":")?; out.integer(item.depth() as u64)?;
            out.literal(",\"name\":")?; out.quoted(item.name())?;
            out.literal(",\"kind\":")?; out.quoted(item.kind().label())?;
            out.literal(",\"original_range\":")?; out.range(item.original_range())?;
            out.literal(",\"name_range\":")?;
            match item.name_range() { Some(r) => out.range(r)?, None => out.literal("null")? }
            out.literal(",\"line\":")?; out.integer(item.line())?; out.literal("}")?;
        }
        out.literal("],\"next_offset\":")?; optional(&mut out, (end < count).then_some(end as u64))?;
        out.literal("}\n")?;
        let exit = if self.limited || self.no_declarations { EXIT_PARTIAL } else { EXIT_OK };
        Ok(desk.finish(out, exit, &mut canceled)?)
    }
    /// Evidence is the recorded declaration region, not a compiler-proven body.
    /// Name selection falls back to that region only when no exact name exists.
    pub fn select(&self, desk: &mut DeskSession, expected: u64, attempt: u64,
        generation: u64, id: u64, whole_evidence: bool, mut canceled: impl FnMut() -> bool)
        -> Result<DeskChange, DeskCodeError> {
        desk.validate_mutation(expected, attempt)?;
        self.validate(desk, expected, generation)?; check(&mut canceled)?;
        let item = self.candidate(id)?;
        let range = if whole_evidence { item.original_range() } else { item.name_range().unwrap_or(item.original_range()) };
        Ok(desk.apply(expected, attempt, DeskCommand::Navigate { pane: self.pane(),
            offset: range.start().get(), selection: Some(range) }, &mut canceled)?)
    }
    /// Find same-token text occurrences of this exact declaration candidate's
    /// name. This is NOT binding-aware reference resolution. Composite/non-token
    /// names are explicitly refused by the existing reference engine.
    pub fn references(&self, desk: &mut DeskSession, expected: u64, outline_generation: u64,
        symbol: u64, reference_generation: u64, max_items: usize, canceled: impl FnMut() -> bool)
        -> Result<DeskReferences, DeskCodeError> {
        self.validate(desk, expected, outline_generation)?;
        let mut result = DeskReferences::prepare(desk, expected, self.pane(), reference_generation,
            self.candidate(symbol)?.name(), max_items, canceled)?;
        result.origin_symbol = Some((outline_generation, symbol));
        Ok(result)
    }
}

/// Independent whole-token text-candidate search in one retained pane. A source
/// larger than the outline guard may still support this route. It requires no
/// selected language or installed compiler, and includes comments and literals.
pub struct DeskReferences {
    source: CodeSource,
    generation: u64,
    name: String,
    hits: Vec<ReferenceCandidate>,
    counted: usize,
    limited: bool,
    origin_symbol: Option<(u64, u64)>,
    _lease: ResourceLease,
}
impl DeskReferences {
    pub fn prepare(desk: &mut DeskSession, expected: u64, pane: DeskPaneId, generation: u64,
        name: &str, max_items: usize, mut canceled: impl FnMut() -> bool) -> Result<Self, DeskCodeError> {
        desk.model().source(pane, expected)?;
        let qgen = QueryGeneration::new(desk.model().owner(), generation).map_err(|_| DeskCodeError::StaleGeneration)?;
        check(&mut canceled)?;
        let source = CodeSource::pin(desk, expected, pane)?;
        let [extract_id, rows_id] = desk.ids()?;
        let extracted = source.view.view().references(name, ReferenceOptions { generation: qgen,
            encoding: None, max_items }, &desk.budget, extract_id, &mut canceled)?;
        let count = extracted.candidates().len();
        let charge = size_of::<Self>() + count * size_of::<ReferenceCandidate>() + name.len() + 128;
        let lease = desk.budget.try_reserve_managed(desk.model().owner(), rows_id, ByteLength::new(charge as u64))
            .map_err(|_| DeskError::ResourceDenied)?;
        let mut hits = reserve(count)?;
        hits.extend_from_slice(extracted.candidates());
        let counted = extracted.total_matches_counted(); let limited = extracted.output_limited();
        let mut label = String::new();
        label.try_reserve_exact(name.len()).map_err(|_| DeskError::ResourceDenied)?;
        if label.capacity() > name.len() { return Err(DeskError::ResourceDenied.into()); }
        label.push_str(name);
        drop(extracted);
        check(&mut canceled)?;
        Ok(Self { source, generation, name: label, hits, counted, limited, origin_symbol: None, _lease: lease })
    }
    pub fn pane(&self) -> DeskPaneId { self.source.pane }
    pub const fn generation(&self) -> u64 { self.generation }
    pub fn name(&self) -> &str { &self.name }
    pub const fn is_complete(&self) -> bool { !self.limited }
    pub fn validate_source(&self, desk: &DeskSession, expected: u64) -> Result<(), DeskCodeError> {
        self.source.validate(desk, expected)
    }
    fn validate(&self, desk: &DeskSession, expected: u64, generation: u64) -> Result<(), DeskCodeError> {
        self.source.validate(desk, expected)?;
        if generation != self.generation { return Err(DeskCodeError::StaleGeneration); }
        Ok(())
    }
    pub fn candidates(&self, desk: &DeskSession, expected: u64, generation: u64)
        -> Result<&[ReferenceCandidate], DeskCodeError> {
        self.validate(desk, expected, generation)?; Ok(&self.hits)
    }
    pub fn page(&self, desk: &mut DeskSession, expected: u64, generation: u64,
        start: usize, limit: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskCodeError> {
        self.validate(desk, expected, generation)?; check(&mut canceled)?;
        page_limits(start, limit, self.hits.len())?;
        let end = start.saturating_add(limit).min(self.hits.len());
        let mut out = self.source.output(desk, "code-ref-page")?;
        out.literal(",\"reference_generation\":")?; out.integer(self.generation)?;
        out.literal(",\"name\":")?; out.quoted(&self.name)?;
        out.literal(",\"evidence_level\":\"whole-token-text-candidate\",\"includes_comments_and_literals\":true,\"search_complete\":")?;
        out.boolean(self.is_complete())?;
        out.literal(",\"count_complete\":")?; out.boolean(self.is_complete())?;
        out.literal(",\"output_limited\":")?; out.boolean(self.limited)?;
        out.literal(",\"retained_references\":")?; out.integer(self.hits.len() as u64)?;
        out.literal(",\"matches_counted\":")?; out.integer(self.counted as u64)?;
        out.literal(",\"origin_symbol\":")?;
        if let Some((generation, symbol)) = self.origin_symbol {
            out.literal("{\"outline_generation\":")?; out.integer(generation)?;
            out.literal(",\"symbol_id\":")?; out.integer(symbol)?; out.literal("}")?;
        } else { out.literal("null")?; }
        out.literal(",\"references\":[")?;
        for (i, item) in self.hits[start..end].iter().enumerate() {
            check(&mut canceled)?;
            if i > 0 { out.literal(",")?; }
            out.literal("{\"reference_id\":")?; out.integer(item.id())?;
            out.literal(",\"original_range\":")?; out.range(item.original_range())?;
            out.literal(",\"line\":")?; out.integer(item.line())?; out.literal("}")?;
        }
        out.literal("],\"next_offset\":")?; optional(&mut out, (end < self.hits.len()).then_some(end as u64))?;
        out.literal("}\n")?;
        Ok(desk.finish(out, if self.limited { EXIT_PARTIAL } else { EXIT_OK }, &mut canceled)?)
    }
    pub fn select(&self, desk: &mut DeskSession, expected: u64, attempt: u64,
        generation: u64, id: u64, mut canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskCodeError> {
        desk.validate_mutation(expected, attempt)?;
        self.validate(desk, expected, generation)?; check(&mut canceled)?;
        let at = id.checked_sub(1).and_then(|n| usize::try_from(n).ok()).ok_or(ReferenceError::NotFound)?;
        let item = self.hits.get(at).filter(|h| h.id() == id).ok_or(ReferenceError::NotFound)?;
        let range = item.original_range();
        Ok(desk.apply(expected, attempt, DeskCommand::Navigate { pane: self.pane(),
            offset: range.start().get(), selection: Some(range) }, &mut canceled)?)
    }
}

fn reserve<T>(count: usize) -> Result<Vec<T>, DeskCodeError> {
    let mut items = Vec::new();
    items.try_reserve_exact(count).map_err(|_| DeskError::ResourceDenied)?;
    if items.capacity() > count { return Err(DeskError::ResourceDenied.into()); }
    Ok(items)
}
fn page_limits(start: usize, limit: usize, count: usize) -> Result<(), DeskCodeError> {
    if !(1..=MAX_CODE_PAGE).contains(&limit) || start > count { Err(DeskCodeError::InvalidPage) } else { Ok(()) }
}
fn optional(out: &mut Output, value: Option<u64>) -> Result<(), OutputError> {
    match value { Some(n) => out.integer(n), None => out.literal("null") }
}

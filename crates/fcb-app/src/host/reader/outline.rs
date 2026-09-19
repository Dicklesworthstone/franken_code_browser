#![forbid(unsafe_code)]

//! Retained-reader composition of the existing source-symbol extractor.
//! No parser, live source lookup, or semantic definition resolver lives here.
//! The reader's immutable capture owns all source; this index owns only bounded
//! presentation records. Repeated filtering, navigation and copies reuse them.

use std::mem::size_of;
use fcb::search::{CapturedSymbols, SymbolLanguage, SymbolNameMode, MAX_SYMBOL_ITEMS};
use super::{check, range, reserve, AppError, ByteLength, ByteRange, HostResponse,
    Output, OutputError, QueryGeneration, ReaderSession, ReaderSessionError,
    ResourceLease, EXIT_OK, EXIT_PARTIAL, MAX_READER_CONTEXT_BYTES, MAX_READER_WINDOW_BYTES};

pub const MAX_READER_SYMBOL_PAGE: usize = 128;
pub const MAX_READER_SYMBOL_QUERY_BYTES: usize = 256;
const MAX_OUTLINE_RECORD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct ReaderOutlineOptions {
    /// None infers only the retained label's suffix, never a live filesystem type.
    pub language: Option<SymbolLanguage>,
    pub max_items: usize,
}
impl Default for ReaderOutlineOptions {
    fn default() -> Self { Self { language: None, max_items: MAX_SYMBOL_ITEMS } }
}

struct Row {
    id: u64,
    parent: Option<u64>,
    depth: usize,
    name: String,
    kind: &'static str,
    evidence: ByteRange,
    name_range: Option<ByteRange>,
    line: u64,
}
impl Row {
    fn matches(&self, needle: &str, mode: SymbolNameMode) -> bool {
        if needle.is_empty() { return true; }
        match mode {
            SymbolNameMode::Exact => self.name == needle,
            SymbolNameMode::Prefix => self.name.starts_with(needle),
            SymbolNameMode::Contains => self.name.contains(needle),
        }
    }
    fn selection(&self, whole_declaration: bool) -> ByteRange {
        if whole_declaration { self.evidence } else { self.name_range.unwrap_or(self.evidence) }
    }
}

pub(super) struct AcceptedOutline {
    generation: u64,
    language: SymbolLanguage,
    rows: Vec<Row>,
    limited: bool,
    no_declarations: bool,
    _lease: ResourceLease,
}

/// Search occurrences, outline symbols and document selections have disjoint
/// identities, even when their independent generation counters are equal.
#[derive(Clone, Copy)]
pub(super) enum SelectionIdentity {
    Search(u64),
    Outline { generation: u64, symbol_id: u64, line: u64 },
    Document { generation: u64, rendered_start: u64, rendered_end: u64 },
}
impl SelectionIdentity {
    pub(super) fn encode(self, out: &mut Output) -> Result<(), OutputError> {
        match self {
            Self::Search(generation) => {
                out.literal("\"query_generation\":")?; out.integer(generation)?;
            }
            Self::Outline { generation, symbol_id, line } => {
                out.literal("\"outline_generation\":")?; out.integer(generation)?;
                out.literal(",\"symbol_id\":")?; out.integer(symbol_id)?;
                out.literal(",\"declaration_line\":")?; out.integer(line)?;
                out.literal(",\"selection_namespace\":\"outline\",\"evidence_level\":\"heuristic-outline-candidate\",\"semantic_resolution\":false")?;
            }
            Self::Document { generation, rendered_start, rendered_end } => {
                out.literal("\"document_generation\":")?; out.integer(generation)?;
                out.literal(",\"selection_namespace\":\"document\",\"source_mapping\":\"enclosing-regions-not-glyph-exact\",\"rendered_utf8_range\":{\"start\":")?;
                out.integer(rendered_start)?; out.literal(",\"end\":")?; out.integer(rendered_end)?;
                out.literal("}")?;
            }
        }
        Ok(())
    }
}

impl ReaderSession {
    pub fn outline_generation(&self) -> Option<u64> { self.outline.as_ref().map(|s| s.generation) }

    /// Extract once from the SAME capture used by reading/search/copy. The
    /// existing extractor's source/complexity/encoding guards remain in force.
    /// Failure does not replace an older outline, search, or the source capture.
    /// Every admitted attempt consumes its independent outline generation.
    pub fn prepare_outline(&mut self, generation: u64, options: ReaderOutlineOptions,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        self.outline_attempt(generation)?;
        check(&mut canceled)?;
        let language = options.language.or_else(|| {
            // Encoded OS bytes are used only for ASCII suffix inference. No path
            // is opened, normalized or reconstructed from a display string.
            SymbolLanguage::from_path(self.path.as_os_str().as_encoded_bytes())
        }).ok_or(ReaderSessionError::UnsupportedOutlineLanguage)?;
        let [extract_id, rows_id] = self.allocations()?;
        let qgen = QueryGeneration::new(self.owner(), generation)
            .map_err(|_| ReaderSessionError::IdentityExhausted)?;
        let symbols = CapturedSymbols::build(self.capture.bytes(), *self.capture.request(),
            qgen, language, Some(self.encoding), options.max_items, &self.budget,
            extract_id, &mut canceled)?;
        let count = symbols.candidates().len();
        let mut charge = count.checked_mul(size_of::<Row>())
            .and_then(|n| n.checked_add(size_of::<AcceptedOutline>())).ok_or(AppError::Admission)?;
        for symbol in symbols.candidates() {
            check(&mut canceled)?;
            charge = charge.checked_add(symbol.name().len()).ok_or(AppError::Admission)?;
        }
        if charge > MAX_OUTLINE_RECORD_BYTES { return Err(AppError::Admission.into()); }
        let lease = self.budget.try_reserve_managed(self.owner(), rows_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut rows = reserve(count)?;
        for symbol in symbols.candidates() {
            check(&mut canceled)?;
            let mut name = String::new();
            name.try_reserve_exact(symbol.name().len()).map_err(|_| AppError::Admission)?;
            if name.capacity() > symbol.name().len() { return Err(AppError::Admission.into()); }
            name.push_str(symbol.name());
            rows.push(Row { id: symbol.id(), parent: symbol.parent_id(), depth: symbol.depth(),
                name, kind: symbol.kind().label(), evidence: symbol.original_range(),
                name_range: symbol.name_range(), line: symbol.line() });
        }
        let candidate = AcceptedOutline { generation, language, rows,
            limited: symbols.output_limited(), no_declarations: symbols.no_recognized_declarations(), _lease: lease };
        drop(symbols); // End the source borrow and release extraction scratch.
        let mut out = self.output("outline")?;
        encode_page(&mut out, &candidate, "", SymbolNameMode::Exact, 0, 64, &mut canceled)?;
        let exit = outline_exit(&candidate);
        let response = self.finish_output(out, exit, &mut canceled)?;
        check(&mut canceled)?;
        self.outline = Some(candidate);
        Ok(response)
    }

    /// Filter the admitted candidate inventory without extraction or source I/O.
    /// Empty needle lists all rows. Names use exact, case-sensitive Unicode text;
    /// parent IDs and symbol IDs stay outline-local across filters and pages.
    /// Start is an offset in the filtered view, NOT an activation identity.
    pub fn symbol_page(&mut self, generation: u64, needle: &str, mode: SymbolNameMode,
        start: usize, limit: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        if needle.len() > MAX_READER_SYMBOL_QUERY_BYTES || !(1..=MAX_READER_SYMBOL_PAGE).contains(&limit) {
            return Err(ReaderSessionError::InvalidLimits);
        }
        check(&mut canceled)?;
        self.outline_snapshot(generation)?;
        let mut out = self.output("symbols")?;
        let outline = self.outline_snapshot(generation)?;
        encode_page(&mut out, outline, needle, mode, start, limit, &mut canceled)?;
        let exit = outline_exit(outline);
        self.finish_output(out, exit, &mut canceled)
    }

    /// A missing exact identifier span selects the extractor's declaration
    /// evidence instead. The caller can inspect the name_range field to tell.
    pub fn symbol_range(&self, generation: u64, id: u64, whole_declaration: bool)
        -> Result<ByteRange, ReaderSessionError> {
        Ok(self.outline_row(generation, id)?.selection(whole_declaration))
    }

    /// Use the ordinary scalar/CRLF-aware reader decoder and exact selection
    /// round trip. No guessed columns, source reopening, or camera motion.
    pub fn symbol_window(&mut self, generation: u64, id: u64, context: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        if context > MAX_READER_CONTEXT_BYTES { return Err(ReaderSessionError::InvalidLimits); }
        check(&mut canceled)?;
        let row = self.outline_row(generation, id)?;
        let selected = row.selection(false);
        let identity = SelectionIdentity::Outline { generation, symbol_id: id, line: row.line };
        let requested = range(selected.start().get().saturating_sub(context as u64 + 4),
            selected.end().get().saturating_add(context as u64 + 4).min(self.length()))?;
        self.window("symbol", requested, MAX_READER_WINDOW_BYTES, Some((identity, selected)), None, &mut canceled)
    }

    /// Return exact original bytes, not a clipboard write or reconstructed code.
    /// Whole declaration means the recorded evidence span, not a semantic body.
    pub fn copy_symbol(&mut self, generation: u64, id: u64, whole_declaration: bool,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?;
        let row = self.outline_row(generation, id)?;
        let selected = row.selection(whole_declaration);
        let identity = SelectionIdentity::Outline { generation, symbol_id: id, line: row.line };
        self.copy("copy-symbol", selected, Some(identity), &mut canceled)
    }

    /// Release derived outline state only. Literal-search generations, captured
    /// bytes, and already-delivered response buffers keep their own lifetimes.
    pub fn clear_outline(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        self.outline_attempt(generation)?;
        check(&mut canceled)?;
        let mut out = self.output("outline-clear")?;
        out.literal(",\"outline_generation\":")?; out.integer(generation)?;
        out.literal(",\"retained_symbols\":\"0\"}\n")?;
        let response = self.finish_output(out, EXIT_OK, &mut canceled)?;
        check(&mut canceled)?;
        self.outline = None;
        Ok(response)
    }
    fn outline_attempt(&mut self, generation: u64) -> Result<(), ReaderSessionError> {
        if generation == 0 || generation <= self.last_outline_attempt {
            return Err(fcb::search::SymbolError::StaleQuery.into());
        }
        self.last_outline_attempt = generation;
        Ok(())
    }
    fn outline_snapshot(&self, generation: u64) -> Result<&AcceptedOutline, ReaderSessionError> {
        let outline = self.outline.as_ref().ok_or(ReaderSessionError::MissingOutline)?;
        if outline.generation != generation { return Err(fcb::search::SymbolError::StaleQuery.into()); }
        Ok(outline)
    }
    fn outline_row(&self, generation: u64, id: u64) -> Result<&Row, ReaderSessionError> {
        let outline = self.outline_snapshot(generation)?;
        let index = id.checked_sub(1).and_then(|n| usize::try_from(n).ok())
            .ok_or(fcb::search::SymbolError::NotFound)?;
        outline.rows.get(index).filter(|row| row.id == id)
            .ok_or_else(|| fcb::search::SymbolError::NotFound.into())
    }
}

fn outline_exit(outline: &AcceptedOutline) -> u8 {
    if outline.limited || outline.no_declarations { EXIT_PARTIAL } else { EXIT_OK }
}
fn encode_page(out: &mut Output, outline: &AcceptedOutline, needle: &str, mode: SymbolNameMode,
    start: usize, limit: usize, canceled: &mut impl FnMut() -> bool) -> Result<(), ReaderSessionError> {
    let mut matches = 0usize;
    for row in &outline.rows { check(canceled)?; if row.matches(needle, mode) { matches += 1; } }
    if start > matches { return Err(ReaderSessionError::InvalidRange); }
    out.literal(",\"outline_generation\":")?; out.integer(outline.generation)?;
    out.literal(",\"language\":")?; out.quoted(outline.language.name())?;
    out.literal(",\"evidence_level\":\"heuristic-outline-candidate\",\"semantic_complete\":false,\"output_limited\":")?;
    out.boolean(outline.limited)?;
    out.literal(",\"no_recognized_declarations\":")?; out.boolean(outline.no_declarations)?;
    out.literal(",\"retained_symbols\":")?; out.integer(outline.rows.len() as u64)?;
    out.literal(",\"matched_symbols\":")?; out.integer(matches as u64)?;
    out.literal(",\"count_basis\":\"retained-candidates\",\"name_case\":\"sensitive\",\"needle\":")?; out.quoted(needle)?;
    out.literal(",\"name_mode\":")?;
    out.quoted(match mode { SymbolNameMode::Exact => "exact", SymbolNameMode::Prefix => "prefix", SymbolNameMode::Contains => "contains" })?;
    out.literal(",\"symbols\":[")?;
    let end = start.saturating_add(limit).min(matches);
    let mut ordinal = 0usize;
    for row in &outline.rows {
        check(canceled)?;
        if !row.matches(needle, mode) { continue; }
        let position = ordinal; ordinal += 1;
        if position < start { continue; }
        if position >= end { break; }
        if position != start { out.literal(",")?; }
        out.literal("{\"symbol_id\":")?; out.integer(row.id)?;
        out.literal(",\"parent_id\":")?;
        if let Some(id) = row.parent { out.integer(id)?; } else { out.literal("null")?; }
        out.literal(",\"depth\":")?; out.integer(row.depth as u64)?;
        out.literal(",\"name\":")?; out.quoted(&row.name)?;
        out.literal(",\"kind\":")?; out.quoted(row.kind)?;
        out.literal(",\"declaration_line\":")?; out.integer(row.line)?;
        out.literal(",\"evidence_range\":")?; out.range(row.evidence)?;
        out.literal(",\"name_range\":")?;
        if let Some(range) = row.name_range { out.range(range)?; } else { out.literal("null")?; }
        out.literal("}")?;
    }
    out.literal("],\"next_offset\":")?;
    if end < matches { out.integer(end as u64)?; } else { out.literal("null")?; }
    out.literal("}\n")?;
    Ok(())
}

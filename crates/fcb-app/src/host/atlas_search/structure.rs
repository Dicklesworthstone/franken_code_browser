#![forbid(unsafe_code)]

//! Workspace declaration and Markdown-heading navigation. The supported public
//! symbol/document engines own extraction; this module owns only frozen scope,
//! bounded scheduling and retained-capture result publication. One step examines
//! at most one admitted file. Legacy extraction is bounded synchronous worker
//! work, bracketed by cancellation, rather than a fixed-duration UI callback.

use fcb::document::reader::{DocumentReadError, DocumentReadOptions, DocumentReader,
    MAX_DOCUMENT_SOURCE_BYTES};
use fcb::search::{CaptureRequest, CapturedSymbols, QueryGeneration, SymbolError,
    SymbolLanguage, SymbolNameMode, MAX_SYMBOL_ITEMS, MAX_SYMBOL_SOURCE_BYTES};
use fcb_core::{DocumentGeneration, DocumentId};
use crate::workspace;
use super::*;
use super::progressive::SearchExecution;

pub const MAX_ATLAS_STRUCTURE_NAME_BYTES: usize = 4096;
pub const MAX_ATLAS_STRUCTURE_METADATA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasStructureMode { Symbols, Headings }
impl AtlasStructureMode {
    pub const fn requested(self) -> &'static str {
        match self { Self::Symbols => "symbols", Self::Headings => "headings" }
    }
    pub const fn realized(self) -> &'static str {
        match self { Self::Symbols => "heuristic-outline-candidate", Self::Headings => "fmd-heading" }
    }
    pub const fn source_limit(self) -> usize {
        match self { Self::Symbols => MAX_SYMBOL_SOURCE_BYTES, Self::Headings => MAX_DOCUMENT_SOURCE_BYTES }
    }
}

/// Query matching is exact, case-sensitive Unicode text over extracted names
/// or upstream heading titles. Scope is frozen before source or match quotas.
/// The file-byte limit is intersected with the owning extractor's source cap.
#[derive(Clone, Copy, Debug)]
pub struct AtlasStructureOptions {
    pub mode: AtlasStructureMode,
    pub name_mode: SymbolNameMode,
    pub limits: AtlasSearchOptions,
}
impl AtlasStructureOptions {
    pub fn new(mode: AtlasStructureMode) -> Self {
        Self { mode, name_mode: SymbolNameMode::Contains, limits: AtlasSearchOptions::default() }
    }
}

pub(super) struct StructureQuery {
    mode: AtlasStructureMode,
    name_mode: SymbolNameMode,
    source_limit: usize,
    metadata_bytes: usize,
    metadata_omitted: usize,
    unsupported_files: usize,
    extraction_limited_files: usize,
}
impl StructureQuery {
    pub(super) fn encode(&self, out: &mut Output) -> Result<(), AtlasSearchError> {
        out.literal(",\"mode\":")?;
        out.quoted(match self.mode { AtlasStructureMode::Symbols => "symbol-name",
            AtlasStructureMode::Headings => "markdown-heading" })?;
        out.literal(",\"requested_mode\":")?; out.quoted(self.mode.requested())?;
        out.literal(",\"realized_mode\":")?; out.quoted(self.mode.realized())?;
        out.literal(",\"evidence_level\":")?; out.quoted(self.mode.realized())?;
        out.literal(",\"semantic_complete\":false,\"name_case\":\"sensitive\",\"name_mode\":")?;
        out.quoted(match self.name_mode { SymbolNameMode::Exact => "exact",
            SymbolNameMode::Prefix => "prefix", SymbolNameMode::Contains => "contains" })?;
        // Complete means all admitted extraction work finished. It never says
        // this heuristic extractor found every compiler definition/reference.
        out.literal(",\"completeness_basis\":\"admitted-extraction-processing\"")?;
        for (key, value) in [("structure_file_byte_limit", self.source_limit),
            ("unsupported_structure_files", self.unsupported_files),
            ("extraction_limited_files", self.extraction_limited_files),
            ("structure_metadata_bytes", self.metadata_bytes),
            ("structure_metadata_omitted", self.metadata_omitted)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value as u64)?;
        }
        Ok(())
    }
}

pub(super) struct StructureRow {
    name: String,
    kind: &'static str,
    language: &'static str,
    candidate_id: u64,
    heading_slug: Option<String>,
    evidence: ByteRange,
    level: &'static str,
}
impl StructureRow {
    pub(super) fn encode(&self, out: &mut Output) -> Result<(), AtlasSearchError> {
        out.literal(",\"structure\":{\"name\":")?; out.quoted(&self.name)?;
        out.literal(",\"kind\":")?; out.quoted(self.kind)?;
        out.literal(",\"language\":")?; out.quoted(self.language)?;
        out.literal(",\"candidate_id\":")?; out.integer(self.candidate_id)?;
        out.literal(",\"heading_slug\":")?;
        match &self.heading_slug { Some(slug) => out.quoted(slug)?, None => out.literal("null")? }
        out.literal(",\"evidence_range\":")?; out.range(self.evidence)?;
        out.literal(",\"evidence_level\":")?; out.quoted(self.level)?;
        out.literal("}")?;
        Ok(())
    }
}
pub(super) struct StructureWork {
    options: AtlasStructureOptions,
    extract_id: ResourceAllocationId,
    io: workspace::IoCounts,
}

impl RetainedAtlasSearch {
    /// Admit a heading or symbol-name query against the existing atlas catalog.
    /// No source reads or extraction occur until step(). Ordinary page, overlay,
    /// focus and open_reader operations work on the same retained hit identities.
    /// Unsupported languages are explicit unavailable files, never text fallback.
    pub fn begin_structure(&mut self, atlas: &AtlasSession, generation: u64, needle: &str,
        scope: AtlasSearchScope, mut options: AtlasStructureOptions,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?;
        options.limits.validate()?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES {
            return Err(AtlasSearchError::InvalidLimits);
        }
        check(&mut canceled)?;
        options.limits.max_file_bytes = options.limits.max_file_bytes.min(options.mode.source_limit());
        let eligible_files = scope.eligible_count(atlas, &mut canceled)?;
        let scope_bytes = if scope.is_all() { 0 } else {
            eligible_files.checked_mul(size_of::<FileId>()).ok_or(AppError::Admission)?
        };
        let capacity = options.limits.max_matches.min(options.limits.max_files);
        let charge = (2 * options.limits.max_source_bytes)
            .checked_add(capacity * (size_of::<RetainedFile>() + 64))
            .and_then(|n| n.checked_add(options.limits.max_matches
                * (size_of::<AtlasSearchHit>() + size_of::<StructureRow>())))
            .and_then(|n| n.checked_add(MAX_ATLAS_STRUCTURE_METADATA_BYTES))
            .and_then(|n| n.checked_add(MAX_DIAGNOSTICS * size_of::<Unavailable>()
                + needle.len() + size_of::<SearchWork>() + scope_bytes)).ok_or(AppError::Admission)?;
        let state_id = self.next_id()?;
        let extract_id = self.next_id()?;
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), state_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let snapshot = Snapshot { generation, needle: copy_text(needle)?,
            structure: Some(StructureQuery { mode: options.mode, name_mode: options.name_mode,
                source_limit: options.limits.max_file_bytes, metadata_bytes: 0, metadata_omitted: 0,
                unsupported_files: 0, extraction_limited_files: 0 }),
            structure_rows: reserve(options.limits.max_matches)?, files: reserve(capacity)?,
            scope, scope_files: scope.freeze(atlas, eligible_files, &mut canceled)?, eligible_files,
            hits: reserve(options.limits.max_matches)?, diagnostics: reserve(MAX_DIAGNOSTICS)?,
            examined: 0, scanned: 0, unavailable: 0, pending: eligible_files,
            matches_seen: 0, source_bytes_read: 0, read_calls: 0, retained_bytes: 0,
            truncated: false, complete: false, stop_reason: None, step_count: 0,
            last_step_files: 0, last_step_bytes: 0, last_step_calls: 0, index_usage: None, _lease: lease };
        let work = SearchWork { snapshot, execution: SearchExecution::Structure(StructureWork {
            options, extract_id, io: workspace::IoCounts::default() }) };
        let response = self.work_response(atlas, &work, "begin-structure", &mut canceled)?;
        self.validate(atlas)?; check(&mut canceled)?;
        self.pending = Some(work);
        Ok(response)
    }

    pub(super) fn advance_structure_work(&self, atlas: &AtlasSession, candidate: &mut Snapshot,
        work: &mut StructureWork, canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        update_coverage(atlas, candidate, work);
        if candidate.stop_reason.is_none() { self.structure_one_file(atlas, candidate, work, canceled)?; }
        update_coverage(atlas, candidate, work);
        Ok(())
    }

    fn structure_one_file(&self, atlas: &AtlasSession, candidate: &mut Snapshot,
        work: &mut StructureWork, canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        let catalog = atlas.atlas().catalog();
        let file = match &candidate.scope_files {
            Some(files) => files.get(candidate.examined).copied(),
            None => catalog.file_id(candidate.examined),
        }.ok_or(AtlasSearchError::WrongAtlas)?;
        let entry = catalog.entry(file).ok_or(AtlasSearchError::WrongAtlas)?;
        candidate.examined += 1;
        let language = SymbolLanguage::from_path(entry.path().raw().as_bytes());
        let supported = match work.options.mode {
            AtlasStructureMode::Symbols => language.is_some(),
            AtlasStructureMode::Headings => is_markdown(entry.path().raw().as_bytes()),
        };
        if !supported {
            candidate.structure.as_mut().ok_or(AtlasSearchError::WrongAtlas)?.unsupported_files += 1;
            candidate.unavailable(file, "STRUCTURE_UNSUPPORTED_LANGUAGE"); return Ok(());
        }
        if entry.observed_bytes() > work.options.limits.max_file_bytes as u64 {
            candidate.unavailable(file, "STRUCTURE_FILE_BYTE_LIMIT"); return Ok(());
        }
        let owner = self.manifest.owner();
        let revision = SourceRevision::new(owner, candidate.generation)
            .map_err(|_| AtlasSearchError::IdentityExhausted)?;
        let request = CaptureRequest::new(file, revision).map_err(|_| AtlasSearchError::WrongAtlas)?;
        let root = catalog.grant().root_path().to_path_buf();
        let captured = workspace::read_capture(&root, request, entry.path(), work.options.limits.max_file_bytes,
            work.options.limits.max_source_bytes as u64, &mut work.io, &self.budget,
            &mut || canceled() || atlas.validate_active().is_err());
        self.validate(atlas)?; check(canceled)?;
        let capture = match captured {
            Ok(capture) => capture,
            Err(fcb::source::SourceError::Canceled) => return Err(AtlasSearchError::Canceled),
            Err(error) => { candidate.unavailable(file, error.code()); return Ok(()); }
        };
        let node = atlas.atlas().node_for_file(file).map_err(AtlasSessionError::from)?;
        let before_hits = candidate.hits.len();
        let before_unavailable = candidate.unavailable;
        let before_omitted = candidate.structure.as_ref().ok_or(AtlasSearchError::WrongAtlas)?.metadata_omitted;
        match work.options.mode {
            AtlasStructureMode::Symbols => {
                let language = language.ok_or(AtlasSearchError::WrongAtlas)?;
                let generation = QueryGeneration::new(owner, candidate.generation)
                    .map_err(|_| AtlasSearchError::IdentityExhausted)?;
                let symbols = match CapturedSymbols::build(capture.bytes(), *capture.request(), generation,
                    language, None, MAX_SYMBOL_ITEMS, &self.budget, work.extract_id,
                    || canceled() || atlas.validate_active().is_err()) {
                    Ok(symbols) => symbols,
                    Err(SymbolError::Canceled) => return Err(AtlasSearchError::Canceled),
                    Err(error) => { candidate.unavailable(file, error.code()); return Ok(()); }
                };
                candidate.scanned += 1;
                if symbols.output_limited() {
                    candidate.structure.as_mut().ok_or(AtlasSearchError::WrongAtlas)?.extraction_limited_files += 1;
                    candidate.unavailable(file, "STRUCTURE_EXTRACTION_ITEM_LIMIT");
                }
                for symbol in symbols.candidates() {
                    check(canceled)?;
                    if !symbol.matches_name(&candidate.needle, work.options.name_mode) { continue; }
                    let selection = symbol.name_range().unwrap_or(symbol.original_range());
                    if !append_row(candidate, work, file, revision, node, capture.bytes().len(), selection,
                        symbol.original_range(), symbol.name(), symbol.kind().label(), language.name(),
                        symbol.id(), None)? { break; }
                }
            }
            AtlasStructureMode::Headings => {
                let document = match DocumentReader::prepare(&capture,
                    DocumentId::new(owner, file.get()).map_err(|_| AtlasSearchError::IdentityExhausted)?,
                    DocumentGeneration::new(owner, candidate.generation).map_err(|_| AtlasSearchError::IdentityExhausted)?,
                    DocumentReadOptions { max_source_bytes: work.options.limits.max_file_bytes,
                        ..DocumentReadOptions::default() }, &self.budget, work.extract_id,
                    || canceled() || atlas.validate_active().is_err()) {
                    Ok(document) => document,
                    Err(DocumentReadError::Canceled) => return Err(AtlasSearchError::Canceled),
                    Err(error) => { candidate.unavailable(file, error.code()); return Ok(()); }
                };
                candidate.scanned += 1;
                for (ordinal, heading) in document.headings().iter().enumerate() {
                    check(canceled)?;
                    if !matches_name(&heading.title, &candidate.needle, work.options.name_mode) { continue; }
                    let range = document.original_span(heading.source_span)
                        .map_err(|_| AtlasSearchError::WrongAtlas)?;
                    if !append_row(candidate, work, file, revision, node, capture.bytes().len(), range,
                        range, &heading.title, "heading", "markdown", ordinal as u64 + 1, Some(&heading.slug))? { break; }
                }
            }
        }
        self.validate(atlas)?; check(canceled)?;
        let metadata_omitted = candidate.structure.as_ref().ok_or(AtlasSearchError::WrongAtlas)?.metadata_omitted;
        if metadata_omitted > before_omitted {
            // Count a file once even when extraction and metadata both lost work.
            if candidate.unavailable == before_unavailable {
                candidate.unavailable(file, "STRUCTURE_METADATA_LIMIT");
            }
        }
        let hits = candidate.hits.len() - before_hits;
        if hits != 0 {
            candidate.retained_bytes += capture.bytes().len();
            let witness = CaptureWitness::new(capture.bytes(),
                &mut || canceled() || atlas.validate_active().is_err())?;
            candidate.files.push(RetainedFile { capture, node, hits, witness });
        }
        Ok(())
    }
}

// Metadata admission precedes copying either variable-size field. The total
// metadata envelope was reserved with the query before any source is read.
#[allow(clippy::too_many_arguments)]
fn append_row(candidate: &mut Snapshot, work: &StructureWork, file: FileId,
    revision: SourceRevision, node: AtlasNodeId, source_bytes: usize, selected: ByteRange,
    evidence: ByteRange, name: &str, kind: &'static str, language: &'static str,
    candidate_id: u64, slug: Option<&str>) -> Result<bool, AtlasSearchError> {
    candidate.matches_seen = candidate.matches_seen.checked_add(1).ok_or(AtlasSearchError::IdentityExhausted)?;
    if candidate.hits.len() == work.options.limits.max_matches {
        candidate.truncated = true; return Ok(false);
    }
    let query = candidate.structure.as_mut().ok_or(AtlasSearchError::WrongAtlas)?;
    let bytes = name.len().checked_add(slug.map_or(0, str::len)).ok_or(AppError::Admission)?;
    if name.len() > MAX_ATLAS_STRUCTURE_NAME_BYTES || slug.is_some_and(|slug| slug.len() > MAX_ATLAS_STRUCTURE_NAME_BYTES)
        || query.metadata_bytes.checked_add(bytes).is_none_or(|n| n > MAX_ATLAS_STRUCTURE_METADATA_BYTES)
        || selected.is_empty() || selected.end().get() > source_bytes as u64
        || evidence.is_empty() || evidence.end().get() > source_bytes as u64
        || evidence.start() > selected.start() || evidence.end() < selected.end()
        || candidate_id == 0 || candidate_id > MAX_SYMBOL_ITEMS as u64 {
        query.metadata_omitted += 1; return Ok(true);
    }
    let row = StructureRow { name: copy_text(name)?, kind, language, candidate_id,
        heading_slug: slug.map(copy_text).transpose()?, evidence, level: work.options.mode.realized() };
    query.metadata_bytes += bytes;
    candidate.structure_rows.push(row);
    candidate.hits.push(AtlasSearchHit { id: candidate.hits.len() as u64 + 1,
        file, revision, node, original_range: selected, source_slot: candidate.files.len() });
    Ok(true)
}
fn matches_name(name: &str, needle: &str, mode: SymbolNameMode) -> bool {
    match mode { SymbolNameMode::Exact => name == needle,
        SymbolNameMode::Prefix => name.starts_with(needle), SymbolNameMode::Contains => name.contains(needle) }
}
fn is_markdown(path: &[u8]) -> bool {
    let basename = path.rsplit(|&byte| byte == b'/').next().unwrap_or(path);
    let Some(dot) = basename.iter().rposition(|&byte| byte == b'.') else { return false; };
    let extension = &basename[dot + 1..];
    [b"md".as_slice(), b"markdown", b"mdown", b"mkd"].iter().any(|suffix| extension.eq_ignore_ascii_case(suffix))
}
fn update_coverage(atlas: &AtlasSession, query: &mut Snapshot, work: &StructureWork) {
    query.pending = query.eligible_files - query.examined;
    query.source_bytes_read = work.io.bytes; query.read_calls = work.io.calls;
    query.stop_reason = if query.pending == 0 {
        Some(if query.truncated { AtlasSearchStop::MatchLimit } else { AtlasSearchStop::AllFilesExamined })
    } else if query.hits.len() == work.options.limits.max_matches {
        query.truncated = true; Some(AtlasSearchStop::MatchLimit)
    } else if query.examined == work.options.limits.max_files { Some(AtlasSearchStop::FileLimit) }
    else if work.io.bytes >= work.options.limits.max_source_bytes as u64 { Some(AtlasSearchStop::SourceByteLimit) }
    else { None };
    query.complete = query.stop_reason.is_some() && atlas.atlas().discovery_complete()
        && query.pending == 0 && query.unavailable == 0 && !query.truncated;
}

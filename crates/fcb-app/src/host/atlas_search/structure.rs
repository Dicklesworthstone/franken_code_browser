#![forbid(unsafe_code)]

//! Workspace declaration, Markdown-heading and identifier-occurrence navigation.
//! The supported public symbol/document/reference engines own extraction; this module owns only frozen scope,
//! bounded scheduling and retained-capture result publication. One step examines
//! at most one admitted file. Legacy extraction is bounded synchronous worker
//! work, bracketed by cancellation, rather than a fixed-duration UI callback.

use fcb::document::reader::{DocumentReadError, DocumentReadOptions, DocumentReader,
    MAX_DOCUMENT_SOURCE_BYTES};
use fcb::search::{CaptureRequest, CapturedSymbols, QueryGeneration, SymbolError,
    SymbolLanguage, SymbolNameMode, MAX_SYMBOL_ITEMS, MAX_SYMBOL_SOURCE_BYTES};
use fcb::search::symbols::{CapturedReferences, ReferenceError, MAX_REFERENCE_ITEMS,
    MAX_REFERENCE_SOURCE_BYTES, validate_reference_name};
use fcb_core::{DocumentGeneration, DocumentId};
use crate::workspace;
use super::*;
use super::progressive::SearchExecution;
use super::indexed::IndexedStructureWork;

pub const MAX_ATLAS_STRUCTURE_NAME_BYTES: usize = 4096;
pub const MAX_ATLAS_STRUCTURE_METADATA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasStructureMode { Symbols, Headings, Occurrences }
impl AtlasStructureMode {
    pub const fn requested(self) -> &'static str {
        match self { Self::Symbols => "symbols", Self::Headings => "headings",
            Self::Occurrences => "occurrences" }
    }
    pub const fn realized(self) -> &'static str {
        match self { Self::Symbols => "heuristic-outline-candidate", Self::Headings => "fmd-heading",
            Self::Occurrences => "whole-token-text-candidate" }
    }
    pub const fn source_limit(self) -> usize {
        match self { Self::Symbols => MAX_SYMBOL_SOURCE_BYTES, Self::Headings => MAX_DOCUMENT_SOURCE_BYTES,
            Self::Occurrences => MAX_REFERENCE_SOURCE_BYTES }
    }
}

/// Query matching is exact, case-sensitive Unicode text over extracted names
/// or upstream heading titles. Occurrences require one exact whole token under
/// the shared reference engine's declared text policy, including comments and
/// literals. Scope is frozen before source or match quotas. The file-byte limit
/// is intersected with the owning extractor's source cap.
#[derive(Clone, Copy, Debug)]
pub struct AtlasStructureOptions {
    pub mode: AtlasStructureMode,
    pub name_mode: SymbolNameMode,
    pub limits: AtlasSearchOptions,
}
impl AtlasStructureOptions {
    pub fn new(mode: AtlasStructureMode) -> Self {
        Self { mode, name_mode: if mode == AtlasStructureMode::Occurrences { SymbolNameMode::Exact }
            else { SymbolNameMode::Contains }, limits: AtlasSearchOptions::default() }
    }
}

/// Structural extraction over an already retained index capture set. The
/// byte budget charges complete extraction inputs, never source I/O, and may
/// be zero. Row storage is bounded independently from captured-source residency.
#[derive(Clone, Copy, Debug)]
pub struct AtlasIndexedStructureOptions {
    pub mode: AtlasStructureMode,
    pub name_mode: SymbolNameMode,
    pub max_matches: usize,
    pub max_scan_bytes: u64,
}
impl AtlasIndexedStructureOptions {
    pub fn new(mode: AtlasStructureMode) -> Self {
        let live = AtlasStructureOptions::new(mode);
        Self { mode, name_mode: live.name_mode, max_matches: live.limits.max_matches,
            max_scan_bytes: MAX_ATLAS_SEARCH_SOURCE_BYTES as u64 }
    }
}

pub(super) struct StructureQuery {
    mode: AtlasStructureMode,
    name_mode: SymbolNameMode,
    source_limit: usize,
    metadata_limit: usize,
    metadata_bytes: usize,
    metadata_omitted: usize,
    unsupported_files: usize,
    extraction_limited_files: usize,
}
impl StructureQuery {
    pub(super) fn encode(&self, out: &mut Output) -> Result<(), AtlasSearchError> {
        out.literal(",\"mode\":")?;
        out.quoted(match self.mode { AtlasStructureMode::Symbols => "symbol-name",
            AtlasStructureMode::Headings => "markdown-heading",
            AtlasStructureMode::Occurrences => "identifier-occurrence" })?;
        out.literal(",\"requested_mode\":")?; out.quoted(self.mode.requested())?;
        out.literal(",\"realized_mode\":")?; out.quoted(self.mode.realized())?;
        out.literal(",\"evidence_level\":")?; out.quoted(self.mode.realized())?;
        out.literal(",\"semantic_complete\":false,\"name_case\":\"sensitive\",\"name_mode\":")?;
        out.quoted(match self.name_mode { SymbolNameMode::Exact => "exact",
            SymbolNameMode::Prefix => "prefix", SymbolNameMode::Contains => "contains" })?;
        // Complete means all admitted extraction work finished. It never says
        // this heuristic extractor found every compiler definition/reference.
        out.literal(",\"completeness_basis\":\"admitted-extraction-processing\"")?;
        if self.mode == AtlasStructureMode::Occurrences {
            out.literal(",\"includes_comments_and_literals\":true,\"compiler_resolved\":false")?;
            out.literal(",\"token_policy\":\"ascii-alphanumeric-underscore-dollar-plus-nonascii-nonwhitespace-noncontrol-v1\"")?;
            out.literal(",\"name_syntax\":\"single-token-no-leading-ascii-digit\",\"unicode_normalization\":\"none\"")?;
        }
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
    qualification_id: ResourceAllocationId,
    io: workspace::IoCounts,
    indexed: Option<IndexedStructureWork>,
}

impl RetainedAtlasSearch {
    /// Admit a heading, symbol-name or whole-token occurrence query against the
    /// existing atlas catalog.
    /// No source reads or extraction occur until step(). Ordinary page, overlay,
    /// focus and open_reader operations work on the same retained hit identities.
    /// Symbols/headings reject unsupported languages explicitly. Occurrences
    /// use the declared whole-token text policy for every admitted source type.
    pub fn begin_structure(&mut self, atlas: &AtlasSession, generation: u64, needle: &str,
        scope: AtlasSearchScope, options: AtlasStructureOptions,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.begin_structure_inner(atlas, generation, needle, scope, options, None, canceled)
    }

    /// Search selected captured sources without reopening any live path. Name
    /// extraction is the same qualified engine used by begin_structure; gram
    /// negatives never exclude structural candidates. Uncaptured scope members
    /// remain pending and failed captures remain unavailable. Existing captured
    /// result activation survives later index replacement or clearing.
    pub fn begin_indexed_structure(&mut self, atlas: &AtlasSession, generation: u64,
        index_generation: u64, needle: &str, scope: AtlasSearchScope,
        options: AtlasIndexedStructureOptions, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let limits = AtlasSearchOptions { max_matches: options.max_matches,
            max_files: MAX_ATLAS_SEARCH_FILES, max_file_bytes: options.mode.source_limit(),
            max_source_bytes: MAX_ATLAS_SEARCH_SOURCE_BYTES };
        self.begin_structure_inner(atlas, generation, needle, scope,
            AtlasStructureOptions { mode: options.mode, name_mode: options.name_mode, limits },
            Some((index_generation, options.max_scan_bytes)), canceled)
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_structure_inner(&mut self, atlas: &AtlasSession, generation: u64, needle: &str,
        scope: AtlasSearchScope, mut options: AtlasStructureOptions,
        indexed: Option<(u64, u64)>, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?;
        // The text-only lowercase option cannot silently change or mislabel
        // the existing case-sensitive symbol/heading-name contract.
        if !scope.case_sensitive() { return Err(AtlasSearchError::InvalidScope); }
        self.attempt(generation)?;
        options.limits.validate()?;
        if let Some((index_generation, max_scan_bytes)) = indexed {
            if max_scan_bytes > MAX_ATLAS_SEARCH_SOURCE_BYTES as u64 { return Err(AtlasSearchError::InvalidLimits); }
            match self.index_generation() {
                None => return Err(AtlasSearchError::MissingIndex),
                Some(current) if current != index_generation => return Err(AtlasSearchError::StaleIndex),
                Some(_) => {}
            }
        }
        if options.mode == AtlasStructureMode::Occurrences {
            if options.name_mode != SymbolNameMode::Exact { return Err(AtlasSearchError::InvalidLimits); }
            validate_reference_name(needle).map_err(ReaderSessionError::from)?;
        }
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES {
            return Err(AtlasSearchError::InvalidLimits);
        }
        check(&mut canceled)?;
        options.limits.max_file_bytes = options.limits.max_file_bytes.min(options.mode.source_limit());
        let (eligible_files, scope_region) = scope.admission_counts(atlas, &mut canceled)?;
        let scope_bytes = if scope.is_all() { 0 } else {
            eligible_files.checked_mul(size_of::<FileId>()).ok_or(AppError::Admission)?
        };
        let scope_bytes = scope_bytes.checked_add(if indexed.is_some() {
            MAX_ATLAS_SEARCH_FILES * size_of::<FileId>()
        } else { 0 }).ok_or(AppError::Admission)?;
        let capacity = options.limits.max_matches.min(options.limits.max_files);
        // Occurrence rows retain only the exact query name and no slug. One
        // source is captured at a time, so the source budget plus one file
        // covers retained captures and the in-flight capture copy. The extent
        // and reference decoder keep their own separate managed reservations.
        // Preserve the existing symbol/heading envelopes unchanged.
        let (live_source_charge, metadata_limit) = if options.mode == AtlasStructureMode::Occurrences {
            (options.limits.max_source_bytes.checked_add(options.limits.max_file_bytes)
                .ok_or(AppError::Admission)?,
             options.limits.max_matches.checked_mul(needle.len()).ok_or(AppError::Admission)?)
        } else { (2 * options.limits.max_source_bytes, MAX_ATLAS_STRUCTURE_METADATA_BYTES) };
        // Shared capture pins are charged to the snapshot too, so retiring or
        // replacing the index cannot release the accepted reader's admission.
        let source_charge = if indexed.is_some() { usize::try_from(self.indexed_source_bytes())
            .map_err(|_| AppError::Admission)? } else { live_source_charge };
        let charge = source_charge
            .checked_add(capacity * (size_of::<RetainedFile>() + 64))
            .and_then(|n| n.checked_add(options.limits.max_matches
                * (size_of::<AtlasSearchHit>() + size_of::<StructureRow>())))
            .and_then(|n| n.checked_add(metadata_limit))
            .and_then(|n| n.checked_add(MAX_DIAGNOSTICS * size_of::<Unavailable>()
                + needle.len() + size_of::<SearchWork>() + scope_bytes)).ok_or(AppError::Admission)?;
        let state_id = self.next_id()?;
        let extract_id = self.next_id()?;
        let qualification_id = self.next_id()?;
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), state_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let scope_files = scope.freeze(atlas, eligible_files, &mut canceled)?;
        let (indexed, index_usage, diagnostics) = match indexed {
            Some((index_generation, max_scan_bytes)) => {
                let (work, usage, diagnostics) = self.prepare_structure_sources(atlas, index_generation,
                    &scope, &scope_files, max_scan_bytes, &mut canceled)?;
                (Some(work), Some(usage), diagnostics)
            }
            None => (None, None, reserve(MAX_DIAGNOSTICS)?),
        };
        let unavailable = index_usage.as_ref().map_or(0, IndexQueryUsage::capture_unavailable);
        let pending = eligible_files.checked_sub(unavailable).ok_or(AtlasSearchError::WrongAtlas)?;
        let snapshot = Snapshot { generation, needle: copy_text(needle)?, expression: None,
            structure: Some(StructureQuery { mode: options.mode, name_mode: options.name_mode,
                source_limit: options.limits.max_file_bytes, metadata_limit, metadata_bytes: 0, metadata_omitted: 0,
                unsupported_files: 0, extraction_limited_files: 0 }),
            structure_rows: reserve(options.limits.max_matches)?, files: reserve(capacity)?,
            scope, scope_files, scope_region, eligible_files,
            hits: reserve(options.limits.max_matches)?, diagnostics,
            matched_texts: Vec::new(), matched_text_capacity: 0,
            examined: unavailable, scanned: 0, unavailable, pending,
            matches_seen: 0, matching_files: 0, source_bytes_read: 0, read_calls: 0, retained_bytes: 0, count_all_matches: false,
            truncated: false, complete: false, stop_reason: None, step_count: 0,
            last_step_files: 0, last_step_bytes: 0, last_step_calls: 0, index_usage, _lease: lease };
        let command = if indexed.is_some() { "begin-indexed-structure" } else { "begin-structure" };
        let work = SearchWork { snapshot, execution: SearchExecution::Structure(StructureWork {
            options, extract_id, qualification_id, io: workspace::IoCounts::default(), indexed }) };
        let response = self.work_response(atlas, &work, command, &mut canceled)?;
        self.validate(atlas)?; check(&mut canceled)?;
        self.pending = Some(work);
        Ok(response)
    }

    pub(super) fn advance_structure_work(&self, atlas: &AtlasSession, candidate: &mut Snapshot,
        work: &mut StructureWork, canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        if let Some(indexed) = &work.indexed {
            match self.index_generation() {
                None => return Err(AtlasSearchError::MissingIndex),
                Some(current) if current != indexed.generation => return Err(AtlasSearchError::StaleIndex),
                Some(_) => {}
            }
        }
        update_coverage(atlas, candidate, work);
        if candidate.stop_reason.is_none() { self.structure_one_file(atlas, candidate, work, canceled)?; }
        update_coverage(atlas, candidate, work);
        Ok(())
    }

    fn structure_one_file(&self, atlas: &AtlasSession, candidate: &mut Snapshot,
        work: &mut StructureWork, canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        let catalog = atlas.atlas().catalog();
        let file = if let Some(indexed) = &work.indexed {
            indexed.files.get(indexed.cursor).copied()
        } else {
            match &candidate.scope_files {
                Some(files) => files.get(candidate.examined).copied(),
                None => catalog.file_id(candidate.examined),
            }
        }.ok_or(AtlasSearchError::WrongAtlas)?;
        let entry = catalog.entry(file).ok_or(AtlasSearchError::WrongAtlas)?;
        let language = SymbolLanguage::from_path(entry.path().raw().as_bytes());
        let supported = match work.options.mode {
            AtlasStructureMode::Symbols => language.is_some(),
            AtlasStructureMode::Headings => is_markdown(entry.path().raw().as_bytes()),
            AtlasStructureMode::Occurrences => true,
        };
        if !supported {
            examine_file(candidate, work);
            candidate.structure.as_mut().ok_or(AtlasSearchError::WrongAtlas)?.unsupported_files += 1;
            candidate.unavailable(file, "STRUCTURE_UNSUPPORTED_LANGUAGE"); return Ok(());
        }
        if work.indexed.is_none() && entry.observed_bytes() > work.options.limits.max_file_bytes as u64 {
            examine_file(candidate, work);
            candidate.unavailable(file, "STRUCTURE_FILE_BYTE_LIMIT"); return Ok(());
        }
        let owner = self.manifest.owner();
        let capture = if let Some(indexed) = work.indexed.as_mut() {
            let capture = self.structure_capture(indexed.generation, file)?;
            if capture.bytes().len() > work.options.limits.max_file_bytes {
                candidate.examined += 1; indexed.cursor += 1;
                candidate.unavailable(file, "STRUCTURE_FILE_BYTE_LIMIT"); return Ok(());
            }
            let next_bytes = indexed.scanned_bytes.checked_add(capture.bytes().len() as u64)
                .ok_or(AppError::Admission)?;
            if next_bytes > indexed.max_scan_bytes {
                // Extraction consumes whole captures. Leave this member pending
                // when its complete input cannot be admitted, without parsing
                // a prefix and falsely reporting an empty name inventory.
                candidate.stop_reason = Some(AtlasSearchStop::VerificationByteLimit);
                return Ok(());
            }
            indexed.scanned_bytes = next_bytes;
            indexed.cursor += 1; candidate.examined += 1;
            capture
        } else {
            examine_file(candidate, work);
            let revision = SourceRevision::new(owner, candidate.generation)
                .map_err(|_| AtlasSearchError::IdentityExhausted)?;
            let request = CaptureRequest::new(file, revision).map_err(|_| AtlasSearchError::WrongAtlas)?;
            let root = catalog.grant().root_path().to_path_buf();
            let captured = workspace::read_capture(&root, request, entry.path(), work.options.limits.max_file_bytes,
                work.options.limits.max_source_bytes as u64, &mut work.io, &self.budget,
                &mut || canceled() || atlas.validate_active().is_err());
            self.validate(atlas)?; check(canceled)?;
            match captured {
                Ok(capture) => capture,
                Err(fcb::source::SourceError::Canceled) => return Err(AtlasSearchError::Canceled),
                Err(error) => { candidate.unavailable(file, error.code()); return Ok(()); }
            }
        };
        // Query generations can differ from the frozen index generation. Every
        // row and reader must preserve its actual captured source revision.
        let revision = capture.request().revision();
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
            AtlasStructureMode::Occurrences => {
                let generation = QueryGeneration::new(owner, candidate.generation)
                    .map_err(|_| AtlasSearchError::IdentityExhausted)?;
                // The shared engine counts one unstored lookahead occurrence.
                // A zero remaining allowance still probes later files, so an
                // exactly-full inventory can finish complete after negatives.
                let remaining = work.options.limits.max_matches.checked_sub(candidate.hits.len())
                    .ok_or(AtlasSearchError::WrongAtlas)?;
                let references = match CapturedReferences::build(capture.bytes(), *capture.request(),
                    generation, &candidate.needle, None, remaining, &self.budget, work.extract_id,
                    || canceled() || atlas.validate_active().is_err()) {
                    Ok(references) => references,
                    Err(ReferenceError::Canceled) => return Err(AtlasSearchError::Canceled),
                    // A replacement may overlap the old accepted snapshot's
                    // reservation. Refuse it transactionally; do not replace
                    // accepted evidence with a resource-starved partial result.
                    Err(error @ ReferenceError::ResourceDenied) => return Err(ReaderSessionError::from(error).into()),
                    Err(error) => { candidate.unavailable(file, error.code()); return Ok(()); }
                };
                candidate.scanned += 1;
                for occurrence in references.candidates() {
                    check(canceled)?;
                    let selected = occurrence.original_range();
                    if !append_row(candidate, work, file, revision, node, capture.bytes().len(), selected,
                        selected, references.name(), "identifier-occurrence", "text", occurrence.id(), None)? { break; }
                }
                if references.output_limited() {
                    candidate.matches_seen = candidate.matches_seen.checked_add(1)
                        .ok_or(AtlasSearchError::IdentityExhausted)?;
                    candidate.truncated = true;
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
                // A rendered heading is not yet source-coordinate evidence.
                // Qualify the whole target before retaining any result row;
                // otherwise a shifted earlier block can point a later heading
                // at unrelated bytes even when its range is within the capture.
                let qualified = document.qualify_heading_source_ranges(&self.budget, work.qualification_id,
                    || canceled() || atlas.validate_active().is_err());
                self.validate(atlas)?; check(canceled)?;
                if let Err(error) = qualified {
                    if error == DocumentReadError::Canceled { return Err(AtlasSearchError::Canceled); }
                    let query = candidate.structure.as_mut().ok_or(AtlasSearchError::WrongAtlas)?;
                    match error {
                        DocumentReadError::EngineBudget | DocumentReadError::ResourceDenied
                            | DocumentReadError::SourceLimit => query.extraction_limited_files += 1,
                        _ => query.unsupported_files += 1,
                    }
                    candidate.unavailable(file, error.code());
                    return Ok(());
                }
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
    let candidate_limit = if work.options.mode == AtlasStructureMode::Occurrences {
        MAX_REFERENCE_ITEMS
    } else { MAX_SYMBOL_ITEMS };
    if name.len() > MAX_ATLAS_STRUCTURE_NAME_BYTES || slug.is_some_and(|slug| slug.len() > MAX_ATLAS_STRUCTURE_NAME_BYTES)
        || query.metadata_bytes.checked_add(bytes).is_none_or(|n| n > query.metadata_limit)
        || selected.is_empty() || selected.end().get() > source_bytes as u64
        || evidence.is_empty() || evidence.end().get() > source_bytes as u64
        || evidence.start() > selected.start() || evidence.end() < selected.end()
        || candidate_id == 0 || candidate_id > candidate_limit as u64 {
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
fn examine_file(query: &mut Snapshot, work: &mut StructureWork) {
    query.examined += 1;
    if let Some(indexed) = work.indexed.as_mut() { indexed.cursor += 1; }
}
fn update_coverage(atlas: &AtlasSession, query: &mut Snapshot, work: &StructureWork) {
    query.pending = query.eligible_files - query.examined;
    query.source_bytes_read = work.io.bytes; query.read_calls = work.io.calls;
    query.stop_reason = if let Some(indexed) = &work.indexed {
        if let Some(usage) = query.index_usage.as_mut() {
            usage.record_structure_work(query.scanned, indexed.scanned_bytes);
        }
        if query.stop_reason == Some(AtlasSearchStop::VerificationByteLimit) {
            query.stop_reason
        } else if query.truncated { Some(AtlasSearchStop::MatchLimit) }
        else if indexed.cursor == indexed.files.len() {
            Some(if query.pending == 0 { AtlasSearchStop::AllFilesExamined }
                else if query.pending == indexed.outside_capture_scope { AtlasSearchStop::CaptureScope }
                else { indexed.capture_stop })
        } else { None }
    } else if query.pending == 0 {
        Some(if query.truncated { AtlasSearchStop::MatchLimit } else { AtlasSearchStop::AllFilesExamined })
    } else if query.truncated {
        // Reaching capacity alone is not evidence of another match. Continue
        // bounded extraction through later negative files until append_row
        // observes a real extra result, just as occurrence mode does.
        Some(AtlasSearchStop::MatchLimit)
    } else if query.examined == work.options.limits.max_files { Some(AtlasSearchStop::FileLimit) }
    else if work.io.bytes >= work.options.limits.max_source_bytes as u64 { Some(AtlasSearchStop::SourceByteLimit) }
    else { None };
    query.complete = query.stop_reason.is_some() && atlas.atlas().discovery_complete()
        && query.scope_region.is_none_or(|region| region.missing == 0) && query.pending == 0 && query.unavailable == 0 && !query.truncated;
}

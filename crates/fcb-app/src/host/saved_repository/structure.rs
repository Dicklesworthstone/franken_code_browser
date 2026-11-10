#![forbid(unsafe_code)]

//! Structural search of an explicitly opened saved repository. Shared facade
//! engines own extraction and exact byte witnesses; this host owns generation
//! attempts, bounded JSON pages and transactional independent-reader handoffs.
//! Archived paths are labels only. Results retain metadata, never source files.

use std::mem::size_of;
use fcb_core::ResourceLease;
use fcb::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, SourceRevision};
pub use fcb::search::SymbolNameMode;
pub use fcb::search::snapshot_structure::StructureMode;
use fcb::search::snapshot_structure::{StructureError, StructureHit, StructureOptions,
    StructureQuery, StructureReport, StructureState};
use super::{AppError, HostResponse, Output, PagedCapture, RawPath, ReaderSession,
    SavedRepositoryError, SavedRepositorySession, check, optional, page_limits, query_attempt};

/// A short exact source selection can accompany the first reader page. Larger
/// heading/declaration spans remain navigation targets without implying that
/// the bounded initial window contains the entire target.
const MAX_SELECTION_BYTES: u64 = 2048;
const CONTEXT_BYTES: u64 = 132;
const INITIAL_WINDOW_BYTES: usize = 16 * 1024;

pub(super) struct AcceptedStructure {
    generation: u64,
    report: StructureReport,
    max_hits: usize,
    max_source_bytes: u64,
    member_bytes_read: u64,
    member_read_calls: u64,
    _lease: ResourceLease,
}
impl AcceptedStructure {
    #[allow(clippy::too_many_arguments)]
    fn prepare(saved: &mut SavedRepositorySession, generation: u64, mode: StructureMode,
        name_mode: SymbolNameMode, needle: &str, max_hits: usize, max_source_bytes: u64,
        canceled: &mut impl FnMut() -> bool) -> Result<Self, SavedRepositoryError> {
        check(canceled)?;
        let owner = saved.owner();
        let qgen = QueryGeneration::new(owner, generation).map_err(|_| SavedRepositoryError::StaleQuery)?;
        let [object_id] = saved.allocations()?;
        let allocations = saved.allocations()?;
        let lease = saved.budget.try_reserve_managed(owner, object_id,
            ByteLength::new(size_of::<Self>() as u64)).map_err(|_| AppError::Admission)?;
        let first_file = FileId::new(owner, 1).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        let first_revision = SourceRevision::new(owner, 1).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        let before = saved.archive.load_stats();
        let mut work = StructureQuery::new(&mut saved.archive, needle, StructureOptions {
            generation: qgen, first_file, first_revision, mode, name_mode,
            max_matches: max_hits, max_source_bytes,
        }, &saved.budget, allocations)?;
        while work.state() == StructureState::Pending {
            work.step(qgen, &saved.budget, &mut *canceled)?;
        }
        let report = work.finish()?;
        let after = saved.archive.load_stats();
        check(canceled)?;
        Ok(Self { generation, report, max_hits, max_source_bytes,
            member_bytes_read: after.bytes_read - before.bytes_read,
            member_read_calls: after.read_calls - before.read_calls, _lease: lease })
    }

    fn validate(&self, saved: &SavedRepositorySession, generation: u64) -> Result<(), SavedRepositoryError> {
        if generation != self.generation { return Err(SavedRepositoryError::StaleQuery); }
        let qgen = QueryGeneration::new(saved.owner(), generation).map_err(|_| SavedRepositoryError::StaleQuery)?;
        self.report.validate_delivery(saved.archive_digest(), qgen)?;
        Ok(())
    }

    fn hit(&self, hit_id: u64) -> Result<&StructureHit, SavedRepositoryError> {
        let position = hit_id.checked_sub(1).and_then(|n| usize::try_from(n).ok())
            .ok_or(SavedRepositoryError::MissingHit)?;
        self.report.hits().get(position).ok_or(SavedRepositoryError::MissingHit)
    }

    fn page(&self, saved: &mut SavedRepositorySession, generation: u64, start: usize,
        limit: usize, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, SavedRepositoryError> {
        self.validate(saved, generation)?; check(canceled)?;
        page_limits(start, limit, self.report.hits().len())?;
        let mut out = saved.output("structure")?;
        self.encode_page(saved, start, limit, &mut out, canceled)?;
        saved.finish(out, !self.report.is_complete(), canceled)
    }

    fn query_fields(&self, out: &mut Output) -> Result<(), SavedRepositoryError> {
        out.literal(",\"selection_namespace\":\"saved-structure\",\"structure_generation\":")?;
        out.integer(self.generation)?;
        out.literal(",\"needle\":")?; out.quoted(self.report.needle())?;
        out.literal(",\"requested_mode\":")?; out.quoted(self.report.mode().requested())?;
        out.literal(",\"name_mode\":")?; out.quoted(match self.report.name_mode() {
            SymbolNameMode::Exact => "exact", SymbolNameMode::Prefix => "prefix", SymbolNameMode::Contains => "contains",
        })?;
        Ok(())
    }

    fn encode_page(&self, saved: &SavedRepositorySession, start: usize, limit: usize,
        out: &mut Output, canceled: &mut impl FnMut() -> bool) -> Result<(), SavedRepositoryError> {
        self.query_fields(out)?;
        let mode = self.report.mode();
        out.literal(",\"mode\":")?; out.quoted(match mode {
            StructureMode::Symbols => "symbol-name", StructureMode::Headings => "markdown-heading",
            StructureMode::Occurrences => "identifier-occurrence",
        })?;
        out.literal(",\"realized_mode\":")?; out.quoted(mode.realized())?;
        out.literal(",\"evidence_level\":")?; out.quoted(mode.realized())?;
        out.literal(",\"semantic_complete\":false,\"name_case\":\"sensitive\",\"scope\":\"whole-saved-archive\",\"completeness_basis\":\"admitted-extraction-processing\",\"index_used\":false,\"index_policy\":\"direct-verified-member-extraction\"")?;
        if mode == StructureMode::Occurrences {
            out.literal(",\"includes_comments_and_literals\":true,\"compiler_resolved\":false,\"token_policy\":\"ascii-alphanumeric-underscore-dollar-plus-nonascii-nonwhitespace-noncontrol-v1\",\"name_syntax\":\"single-token-no-leading-ascii-digit\",\"unicode_normalization\":\"none\"")?;
        }
        out.literal(",\"state\":")?; out.quoted(match self.report.state() {
            StructureState::Complete => "complete", StructureState::Truncated => "truncated",
            StructureState::WorkLimit => "work-limit", _ => return Err(StructureError::Pending.into()),
        })?;
        out.literal(",\"complete\":")?; out.boolean(self.report.is_complete())?;
        out.literal(",\"matches_seen_exact\":")?; out.boolean(self.report.is_complete())?;
        out.literal(",\"truncated\":")?; out.boolean(self.report.state() == StructureState::Truncated)?;
        let s = self.report.stats();
        for (key, value) in [
            ("max_hits", self.max_hits as u64), ("max_source_bytes", self.max_source_bytes),
            ("retained_hits", self.report.hits().len() as u64), ("matches_seen", self.report.matches_seen()),
            ("members_visited", s.members_visited as u64), ("members_loaded", s.members_loaded as u64),
            ("files_searched", s.files_searched as u64), ("unsupported_files", s.unsupported_files as u64),
            ("unavailable_files", s.unavailable_files as u64), ("source_limited_files", s.source_limited_files as u64),
            ("extraction_limited_files", s.extraction_limited_files as u64), ("metadata_omitted", s.metadata_omitted as u64),
            ("loaded_bytes", s.loaded_bytes), ("peak_source_bytes", s.peak_source_bytes as u64),
            ("metadata_bytes", s.metadata_bytes as u64), ("member_bytes_read", self.member_bytes_read),
            ("member_read_calls", self.member_read_calls),
        ] { out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?; }
        out.literal(",\"retained_source_payload_bytes\":\"0\",\"hits\":[")?;
        let end = start.saturating_add(limit).min(self.report.hits().len());
        for (i, hit) in self.report.hits()[start..end].iter().enumerate() {
            check(canceled)?;
            if i > 0 { out.literal(",")?; }
            let member = saved.archive.directory().member(hit.ordinal()).ok_or(SavedRepositoryError::MissingHit)?;
            out.literal("{\"hit_id\":")?; out.integer((start + i + 1) as u64)?;
            out.literal(",\"member\":")?; out.integer(hit.ordinal() as u64)?;
            out.literal(",\"file_id\":")?; out.integer(hit.file().get())?;
            out.literal(",\"source_revision\":")?; out.integer(hit.revision().get())?;
            out.literal(",\"source_digest\":")?; out.quoted(&hit.source_digest().to_hex())?;
            out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(member.path).to_path_buf())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
            structure_fields(out, hit, mode)?;
            out.literal("}")?;
        }
        out.literal("],\"next_offset\":")?;
        optional(out, (end < self.report.hits().len()).then_some(end as u64))?;
        out.literal("}\n")?;
        Ok(())
    }

    fn encode_reader(&self, prepared: &StructureReader, hit_id: u64, out: &mut Output,
        canceled: &mut impl FnMut() -> bool) -> Result<(), SavedRepositoryError> {
        check(canceled)?;
        let hit = self.hit(hit_id)?;
        let capture = &prepared.capture;
        self.query_fields(out)?;
        out.literal(",\"member\":")?; out.integer(hit.ordinal() as u64)?;
        out.literal(",\"file_id\":")?; out.integer(capture.file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(capture.revision().get())?;
        out.literal(",\"source_digest\":")?; out.quoted(&capture.source_digest().to_hex())?;
        out.literal(",\"source_observation\":\"verified-saved-member\",\"live_source_reopened\":false,\"reader_owner\":")?;
        out.integer(prepared.reader_owner.get())?;
        structure_fields(out, hit, self.report.mode())?;
        out.literal(",\"selection\":{\"structure_generation\":")?; out.integer(self.generation)?;
        out.literal(",\"hit_id\":")?; out.integer(hit_id)?;
        out.literal(",\"domain\":\"original-source-bytes\",\"original_range\":")?; out.range(hit.original_range())?;
        out.literal(",\"original_hex\":")?;
        if hit.original_range().len().get() <= MAX_SELECTION_BYTES {
            let (start, end) = hit.original_range().as_usize_bounds().map_err(|_| StructureError::InvalidHit)?;
            out.hex(capture.bytes().get(start..end).ok_or(StructureError::InvalidHit)?)?;
        } else { out.literal("null")?; }
        out.literal("},\"reader\":")?; out.literal(prepared.info.as_str())?;
        out.literal(",\"window\":")?; out.literal(prepared.window.as_str())?; out.literal("}\n")?;
        check(canceled)?;
        Ok(())
    }
}

fn structure_fields(out: &mut Output, hit: &StructureHit, mode: StructureMode) -> Result<(), SavedRepositoryError> {
    out.literal(",\"structure\":{\"name\":")?; out.quoted(hit.name())?;
    out.literal(",\"kind\":")?; out.quoted(hit.kind())?;
    out.literal(",\"language\":")?; out.quoted(hit.language())?;
    out.literal(",\"candidate_id\":")?; out.integer(hit.candidate_id())?;
    out.literal(",\"heading_slug\":")?;
    if let Some(slug) = hit.heading_slug() { out.quoted(slug)?; } else { out.literal("null")?; }
    out.literal(",\"evidence_range\":")?; out.range(hit.evidence_range())?;
    out.literal(",\"evidence_level\":")?; out.quoted(mode.realized())?; out.literal("}")?;
    Ok(())
}

struct StructureReader {
    reader_owner: ArenaOwnerId,
    capture: PagedCapture,
    reader: ReaderSession,
    info: HostResponse,
    window: HostResponse,
}

impl SavedRepositorySession {
    pub fn accepted_structure_generation(&self) -> Option<u64> {
        self.structure.as_ref().map(|query| query.generation)
    }
    pub const fn last_structure_attempt(&self) -> u64 { self.last_structure_attempt }

    /// Independent monotonic attempts preserve accepted literal/expression and
    /// structural results on failure. Accept only after the first response is
    /// complete; canceled work never publishes a partially prepared report.
    #[allow(clippy::too_many_arguments)]
    pub fn structure_search(&mut self, generation: u64, mode: StructureMode, name_mode: SymbolNameMode,
        needle: &str, max_hits: usize, max_source_bytes: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedRepositoryError> {
        query_attempt(&mut self.last_structure_attempt, generation)?;
        check(&mut canceled)?;
        let candidate = AcceptedStructure::prepare(self, generation, mode, name_mode, needle,
            max_hits, max_source_bytes, &mut canceled)?;
        let response = candidate.page(self, generation, 0, 64, &mut canceled)?;
        self.structure = Some(candidate);
        Ok(response)
    }

    /// Retained result pagination has no source reads or extraction calls.
    pub fn structure_results(&mut self, generation: u64, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedRepositoryError> {
        check(&mut canceled)?;
        let accepted = self.accepted_structure(generation)?;
        page_limits(start, limit, accepted.report.hits().len())?;
        let partial = !accepted.report.is_complete();
        let mut out = self.output("structure")?;
        self.accepted_structure(generation)?.encode_page(self, start, limit, &mut out, &mut canceled)?;
        self.finish(out, partial, &mut canceled)
    }

    pub fn clear_structure(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedRepositoryError> {
        query_attempt(&mut self.last_structure_attempt, generation)?;
        check(&mut canceled)?;
        let mut out = self.output("structure-clear")?;
        out.literal(",\"selection_namespace\":\"saved-structure\",\"structure_generation\":")?;
        out.integer(generation)?;
        out.literal(",\"accepted_structure_generation\":null,\"retained_hits\":\"0\"}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        self.structure = None;
        Ok(response)
    }

    /// Reverify the entire saved member, then prepare all independently owned
    /// reader state and receipt before publication. Destination admission is
    /// the bridge's responsibility and precedes this operation's source I/O.
    pub fn open_structure_hit_reader(&mut self, reader_owner: ArenaOwnerId, generation: u64,
        hit_id: u64, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), SavedRepositoryError> {
        check(&mut canceled)?;
        self.accepted_structure(generation)?.hit(hit_id)?;
        let prepared = self.prepare_structure_reader(reader_owner, generation, hit_id, &mut canceled)?;
        let mut out = self.output("structure-open-reader")?;
        self.accepted_structure(generation)?.encode_reader(&prepared, hit_id, &mut out, &mut canceled)?;
        let response = self.finish(out, false, &mut canceled)?;
        Ok((prepared.reader, response))
    }

    fn accepted_structure(&self, generation: u64) -> Result<&AcceptedStructure, SavedRepositoryError> {
        let query = self.structure.as_ref().ok_or(SavedRepositoryError::MissingQuery)?;
        query.validate(self, generation)?;
        Ok(query)
    }

    fn prepare_structure_reader(&mut self, reader_owner: ArenaOwnerId, generation: u64, hit_id: u64,
        canceled: &mut impl FnMut() -> bool) -> Result<StructureReader, SavedRepositoryError> {
        check(canceled)?;
        if reader_owner == self.owner() { return Err(super::PagedSearchError::OwnerMismatch.into()); }
        let qgen = QueryGeneration::new(self.owner(), generation).map_err(|_| SavedRepositoryError::StaleQuery)?;
        let allocations = self.allocations()?;
        let hit = self.structure.as_ref().ok_or(SavedRepositoryError::MissingQuery)?.hit(hit_id)?;
        let capture = hit.open(&mut self.archive, qgen, &self.budget, allocations, &mut *canceled)?;
        let member = self.archive.directory().member(hit.ordinal()).ok_or(SavedRepositoryError::MissingHit)?;
        let label = RawPath::from_bytes(member.path).to_path_buf();
        let range = hit.original_range();
        let mut reader = ReaderSession::from_bytes(reader_owner, &label, capture.bytes(), &mut *canceled)?;
        let info = reader.info(&mut *canceled)?;
        let start = range.start().get().saturating_sub(CONTEXT_BYTES);
        let bytes = if range.len().get() <= MAX_SELECTION_BYTES {
            let end = range.end().get().saturating_add(CONTEXT_BYTES).min(capture.bytes().len() as u64);
            ((end - start) as usize).max(4)
        } else { INITIAL_WINDOW_BYTES };
        let window = reader.read_window(start, bytes, &mut *canceled)?;
        check(canceled)?;
        Ok(StructureReader { reader_owner, capture, reader, info, window })
    }
}

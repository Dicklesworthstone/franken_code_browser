#![forbid(unsafe_code)]

//! Explicit capture/index preparation followed by zero-source-I/O queries.
//! Synchronous and one-file resumable queries use the same owning cursor and
//! verifier. Native paths stay with the atlas. Explicit expression admission
//! shares this pipeline while the original text methods remain literal.

#[path = "index_build.rs"]
mod build;
#[path = "index_source.rs"]
mod source;
pub use build::AtlasIndexBuildProgress;
pub(super) use build::IndexBuildWork;
#[path = "relationships.rs"]
mod relationships;
pub use relationships::{AtlasRelationshipDirection, AtlasRelationshipSide, MAX_ATLAS_RELATIONSHIPS,
    MAX_ATLAS_RELATIONSHIP_PARSE_BYTES, MAX_ATLAS_RELATIONSHIP_METADATA_BYTES};
pub(super) use relationships::{RelationshipSnapshot, RelationshipsWork};

use std::mem::size_of;
use fcb::search::{CaptureRequest, IndexError, IndexLimits, IndexedQueryState,
    MembershipState, ParsedQuery, QueryError, QueryGeneration, QueryOptions, SearchCoverage,
    SearchMode, UnicodeNormalization};
use fcb::search::index::export::{OwnedEphemeralIndex, SourceRetentionLimits};
use fcb::search::indexed_query::OwnedIndexedQuery;
use crate::workspace;
use super::*;
use super::progressive::SearchExecution;

pub const MAX_ATLAS_INDEX_GRAMS: usize = 2 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasIndexOptions {
    pub max_files: usize,
    pub max_file_bytes: usize,
    /// Actual read work, including unsuccessful captures, not index size.
    pub max_source_bytes: usize,
    /// A gram quota refusal leaves the captured file on exact-scan fallback.
    pub max_index_grams: usize,
}
impl Default for AtlasIndexOptions {
    fn default() -> Self {
        Self { max_files: MAX_ATLAS_SEARCH_FILES, max_file_bytes: 1024 * 1024,
            max_source_bytes: MAX_ATLAS_SEARCH_SOURCE_BYTES, max_index_grams: MAX_ATLAS_INDEX_GRAMS }
    }
}
impl AtlasIndexOptions {
    fn validate(self) -> Result<(), AtlasSearchError> {
        AtlasSearchOptions { max_matches: 1, max_files: self.max_files,
            max_file_bytes: self.max_file_bytes, max_source_bytes: self.max_source_bytes }.validate()?;
        if self.max_index_grams > MAX_ATLAS_INDEX_GRAMS { return Err(AtlasSearchError::InvalidLimits); }
        Ok(())
    }
}

/// Immutable capture membership, distinct from every later query's scope.
/// Fixed descriptor storage is charged inside its retaining index/query; a
/// visible manifest is shared with its own lease until its last owner retires.
#[derive(Clone)]
struct IndexCaptureScope {
    scope: AtlasSearchScope,
    eligible: usize,
    excluded: usize,
    region: Option<scope::RegionCoverage>,
}
impl IndexCaptureScope {
    fn new(atlas: &AtlasSession, scope: AtlasSearchScope, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, AtlasSearchError> {
        // Matching options never affect which source files are captured.
        let scope = scope.with_case_sensitive(true);
        let (eligible, region) = scope.admission_counts(atlas, canceled)?;
        let excluded = atlas.atlas().file_count().checked_sub(eligible).ok_or(AtlasSearchError::WrongAtlas)?;
        Ok(Self { scope, eligible, excluded, region })
    }
    fn all_requested_catalogued(&self) -> bool { self.region.is_none_or(|region| region.missing == 0) }
    fn encode(&self, out: &mut Output) -> Result<(), AtlasSearchError> {
        self.scope.encode_prefixed(out, "capture_")?;
        out.literal(",\"capture_eligible_files\":")?; out.integer(self.eligible as u64)?;
        out.literal(",\"capture_excluded_files\":")?; out.integer(self.excluded as u64)?;
        if let Some(region) = self.region { region.encode(out, "capture_")?; }
        Ok(())
    }
    fn query_members_outside(&self, atlas: &AtlasSession, query: &AtlasSearchScope,
        canceled: &mut impl FnMut() -> bool) -> Result<usize, AtlasSearchError> {
        check(canceled)?;
        if self.scope.is_all() { return Ok(0); }
        if query.is_all() { return Ok(self.excluded); }
        let mut outside = 0;
        for entry in atlas.atlas().catalog().entries() {
            check(canceled)?;
            let path = entry.path().raw().as_bytes();
            if query.includes_path(path) && !self.scope.includes_path(path) { outside += 1; }
        }
        Ok(outside)
    }
}

pub(super) struct RetainedIndex {
    generation: u64,
    engine: OwnedEphemeralIndex,
    capture_scope: IndexCaptureScope,
    diagnostics: Vec<Unavailable>,
    examined: usize,
    pending: usize,
    read_bytes: u64,
    read_calls: u64,
    stop: AtlasSearchStop,
    build_steps: u64,
    last_build_files: usize,
    last_build_bytes: u64,
    last_build_calls: u64,
    _lease: ResourceLease,
}
pub(super) struct IndexedWork {
    query: OwnedIndexedQuery,
    index_generation: u64,
    outside_capture_scope: usize,
    unsupported_seen: usize,
}
/// A structural query walks only selected complete captures. Its cursor is
/// separate from examined_files, which already includes capture failures.
pub(super) struct IndexedStructureWork {
    pub(super) generation: u64,
    pub(super) files: Vec<FileId>,
    pub(super) cursor: usize,
    pub(super) max_scan_bytes: u64,
    pub(super) scanned_bytes: u64,
    pub(super) outside_capture_scope: usize,
    pub(super) capture_stop: AtlasSearchStop,
}
pub(super) struct IndexQueryUsage {
    generation: u64,
    manifest: u64,
    capture_scope: IndexCaptureScope,
    skipped: usize,
    verified: usize,
    fallback: usize,
    scanned_bytes: u64,
    eligible_captured: usize,
    capture_unavailable: usize,
}
impl IndexQueryUsage {
    pub(super) fn capture_unavailable(&self) -> usize { self.capture_unavailable }
    pub(super) fn record_structure_work(&mut self, verified: usize, scanned_bytes: u64) {
        self.verified = verified;
        self.scanned_bytes = scanned_bytes;
    }
    pub(super) fn encode(&self, out: &mut Output) -> Result<(), AtlasSearchError> {
        self.capture_scope.encode(out)?;
        out.literal(",\"index_generation\":")?; out.integer(self.generation)?;
        out.literal(",\"capture_manifest\":")?; out.integer(self.manifest)?;
        for (key, value) in [("skipped_by_index", self.skipped as u64),
            ("eligible_captured_files", self.eligible_captured as u64),
            ("scoped_capture_unavailable_files", self.capture_unavailable as u64),
            ("verified_files", self.verified as u64), ("fallback_files", self.fallback as u64),
            ("verification_source_bytes", self.scanned_bytes)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        Ok(())
    }
}
impl From<IndexError> for AtlasSearchError {
    fn from(error: IndexError) -> Self {
        match error {
            IndexError::Canceled | IndexError::Query(QueryError::Canceled) => Self::Canceled,
            other => Self::Index(other),
        }
    }
}
impl RetainedAtlasSearch {
    pub fn index_generation(&self) -> Option<u64> { self.index.as_ref().map(|i| i.generation) }
    pub fn indexed_source_bytes(&self) -> u64 { self.index.as_ref().map_or(0, |i| i.engine.source_bytes()) }

    /// Metadata-only admission after the structural query reserves its source,
    /// file-ID and diagnostic envelopes. No trigram exclusion is valid for
    /// extracted symbol names or rendered heading titles: inspect every
    /// selected complete capture, including gram-quota fallback captures.
    pub(super) fn prepare_structure_sources(&self, atlas: &AtlasSession, index_generation: u64,
        scope: &AtlasSearchScope, scope_files: &Option<Vec<FileId>>, max_scan_bytes: u64,
        canceled: &mut impl FnMut() -> bool)
        -> Result<(IndexedStructureWork, IndexQueryUsage, Vec<Unavailable>), AtlasSearchError> {
        let retained = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        if retained.generation != index_generation { return Err(AtlasSearchError::StaleIndex); }
        let mut files = reserve(retained.engine.captured_files())?;
        let catalog = atlas.atlas().catalog();
        for ordinal in 0..catalog.entries().len() {
            check(canceled)?;
            let file = catalog.file_id(ordinal).ok_or(AtlasSearchError::WrongAtlas)?;
            if scope::includes_file(scope_files, file) && retained.engine.capture(file).is_some() {
                files.push(file);
            }
        }
        let mut capture_unavailable = 0;
        for &file in retained.engine.unavailable_files() {
            check(canceled)?;
            if scope::includes_file(scope_files, file) { capture_unavailable += 1; }
        }
        let mut diagnostics = reserve(MAX_DIAGNOSTICS)?;
        for diagnostic in &retained.diagnostics {
            check(canceled)?;
            if scope::includes_file(scope_files, diagnostic.file) {
                diagnostics.push(Unavailable { file: diagnostic.file, reason: diagnostic.reason });
            }
        }
        let outside_capture_scope = retained.capture_scope.query_members_outside(atlas, scope, canceled)?;
        let usage = IndexQueryUsage { generation: index_generation, manifest: retained.engine.id().revision(),
            capture_scope: retained.capture_scope.clone(), skipped: 0, verified: 0, fallback: 0,
            scanned_bytes: 0, eligible_captured: files.len(), capture_unavailable };
        Ok((IndexedStructureWork { generation: index_generation, files, cursor: 0, max_scan_bytes,
            scanned_bytes: 0, outside_capture_scope, capture_stop: retained.stop }, usage, diagnostics))
    }

    pub(super) fn structure_capture(&self, index_generation: u64, file: FileId)
        -> Result<CompleteCapture, AtlasSearchError> {
        let retained = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        if retained.generation != index_generation { return Err(AtlasSearchError::StaleIndex); }
        retained.engine.capture(file).cloned().ok_or(AtlasSearchError::WrongAtlas)
    }

    /// Compatibility worker call over the SAME one-file preparation pipeline.
    /// Use begin_index/step_index to yield between captured/indexed members.
    pub fn prepare_index(&mut self, atlas: &AtlasSession, generation: u64,
        options: AtlasIndexOptions, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.prepare_index_scoped(atlas, generation, AtlasSearchScope::all(), options, canceled)
    }

    /// Capture only the frozen type/path membership before spending any source,
    /// file or index quota. Later queries reuse these exact bytes until rebuild.
    pub fn prepare_index_scoped(&mut self, atlas: &AtlasSession, generation: u64, scope: AtlasSearchScope,
        options: AtlasIndexOptions, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let mut work = self.prepare_index_build(atlas, generation, scope, options, &mut canceled)?;
        loop {
            if let Some(stop) = self.advance_index_build(atlas, &mut work, &mut canceled)? {
                return self.publish_index_build(atlas, work, stop, "index-prepare", &mut canceled);
            }
        }
    }

    /// Compatibility worker call: finish the SAME one-file cursor used by
    /// begin_indexed/step, publishing only its terminal response.
    pub fn search_indexed(&mut self, atlas: &AtlasSession, generation: u64, index_generation: u64,
        needle: &str, max_matches: usize, max_scan_bytes: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.search_indexed_scoped(atlas, generation, index_generation, needle, AtlasSearchScope::all(),
            max_matches, max_scan_bytes, canceled)
    }

    /// Query a frozen file selection over the same retained source universe.
    /// Excluded files never consume gram, verification or occurrence quotas.
    pub fn search_indexed_scoped(&mut self, atlas: &AtlasSession, generation: u64, index_generation: u64,
        needle: &str, scope: AtlasSearchScope, max_matches: usize, max_scan_bytes: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        let mut work = self.prepare_indexed_work(atlas, generation, index_generation, needle,
            scope, max_matches, max_scan_bytes, false, false, &mut canceled)?;
        while work.snapshot.stop_reason.is_none() { self.advance_work(atlas, &mut work, &mut canceled)?; }
        let response = self.work_response(atlas, &work, "search-indexed", &mut canceled)?;
        self.accepted = Some(work.snapshot);
        Ok(response)
    }

    /// Count every decoded-text occurrence in the admitted capture scope while
    /// retaining at most `max_matches` navigable rows, including zero. Verification still obeys
    /// the query-wide byte budget; an exhausted budget is not an exact total.
    pub fn search_indexed_counted_scoped(&mut self, atlas: &AtlasSession, generation: u64,
        index_generation: u64, needle: &str, scope: AtlasSearchScope, max_matches: usize,
        max_scan_bytes: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let mut work = self.prepare_indexed_counted_work(atlas, generation, index_generation, needle,
            scope, max_matches, max_scan_bytes, &mut canceled)?;
        while work.snapshot.stop_reason.is_none() { self.advance_work(atlas, &mut work, &mut canceled)?; }
        let response = self.work_response(atlas, &work, "search-indexed", &mut canceled)?;
        self.accepted = Some(work.snapshot);
        Ok(response)
    }

    /// Admit a literal query over an explicitly prepared index. No source scan
    /// or source I/O occurs here; bounded metadata is inspected for admission.
    /// Call the ordinary step method to continue. Early pages/overlays/activation
    /// use this generation and exact captures, not live bytes or ranked offsets.
    pub fn begin_indexed(&mut self, atlas: &AtlasSession, generation: u64, index_generation: u64,
        needle: &str, max_matches: usize, max_scan_bytes: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.begin_indexed_scoped(atlas, generation, index_generation, needle, AtlasSearchScope::all(),
            max_matches, max_scan_bytes, canceled)
    }

    /// Scope is evaluated at admission against the original native-path catalog,
    /// then retained as file IDs. Changing it starts a new query generation.
    pub fn begin_indexed_scoped(&mut self, atlas: &AtlasSession, generation: u64, index_generation: u64,
        needle: &str, scope: AtlasSearchScope, max_matches: usize, max_scan_bytes: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        let work = self.prepare_indexed_work(atlas, generation, index_generation, needle,
            scope, max_matches, max_scan_bytes, false, false, &mut canceled)?;
        let response = self.work_response(atlas, &work, "begin-indexed", &mut canceled)?;
        self.pending = Some(work);
        Ok(response)
    }

    /// Progressive counting over all retained captures. Admission does no
    /// source verification or I/O; ordinary `step` calls visit one member.
    pub fn begin_indexed_counted(&mut self, atlas: &AtlasSession, generation: u64,
        index_generation: u64, needle: &str, max_matches: usize, max_scan_bytes: u64,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.begin_indexed_counted_scoped(atlas, generation, index_generation, needle,
            AtlasSearchScope::all(), max_matches, max_scan_bytes, canceled)
    }

    /// The type/path/visible selection is frozen before any counting work.
    /// Omitted rows do not stop the query or weaken capture-coverage checks.
    pub fn begin_indexed_counted_scoped(&mut self, atlas: &AtlasSession, generation: u64,
        index_generation: u64, needle: &str, scope: AtlasSearchScope, max_matches: usize,
        max_scan_bytes: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let work = self.prepare_indexed_counted_work(atlas, generation, index_generation, needle,
            scope, max_matches, max_scan_bytes, &mut canceled)?;
        let response = self.work_response(atlas, &work, "begin-indexed", &mut canceled)?;
        self.pending = Some(work);
        Ok(response)
    }

    fn prepare_indexed_counted_work(&mut self, atlas: &AtlasSession, generation: u64,
        index_generation: u64, needle: &str, scope: AtlasSearchScope, max_matches: usize,
        max_scan_bytes: u64, canceled: &mut impl FnMut() -> bool)
        -> Result<SearchWork, AtlasSearchError> {
        // The shared counted verifier honors both exact and Unicode lowercase
        // matching while preserving the selected scope and bounded row storage.
        self.prepare_indexed_work_inner(atlas, generation, index_generation, needle, scope,
            max_matches, max_scan_bytes, false, true, canceled)
    }

    pub(super) fn prepare_indexed_work(&mut self, atlas: &AtlasSession, generation: u64, index_generation: u64,
        text: &str, scope: AtlasSearchScope, max_matches: usize, max_scan_bytes: u64, expression: bool,
        count_all_matches: bool, canceled: &mut impl FnMut() -> bool)
        -> Result<SearchWork, AtlasSearchError> {
        self.prepare_indexed_work_inner(atlas, generation, index_generation, text, scope,
            max_matches, max_scan_bytes, expression, count_all_matches, canceled)
    }

    fn prepare_indexed_work_inner(&mut self, atlas: &AtlasSession, generation: u64, index_generation: u64,
        text: &str, scope: AtlasSearchScope, max_matches: usize, max_scan_bytes: u64, expression: bool,
        count_all_matches: bool, canceled: &mut impl FnMut() -> bool)
        -> Result<SearchWork, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?; check(canceled)?;
        let scan_limit = if expression { MAX_ATLAS_EXPRESSION_SCAN_BYTES } else { MAX_ATLAS_SEARCH_SOURCE_BYTES as u64 };
        if max_matches > MAX_ATLAS_SEARCH_HITS || (max_matches == 0 && !count_all_matches)
            || max_scan_bytes > scan_limit
            || (!expression && (text.is_empty() || text.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES)) {
            return Err(AtlasSearchError::InvalidLimits);
        }
        // Expression metadata predicates freeze membership before the owning
        // query inspects captures/unavailable members or spends scan quotas.
        let admitted = if expression { Some(self.admit_expression(atlas, text, &scope, max_scan_bytes, canceled)?) }
            else { None };
        let [state_id, ast_id, query_id] = [self.next_id()?, self.next_id()?, self.next_id()?];
        let owner = self.manifest.owner();
        let retained = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        if retained.generation != index_generation { return Err(AtlasSearchError::StaleIndex); }
        let (eligible_files, scope_region) = if let Some(admitted) = &admitted {
            (admitted.eligible, admitted.region)
        } else { scope.admission_counts(atlas, canceled)? };
        let outside_capture_scope = if let Some(admitted) = &admitted {
            let mut outside = 0;
            for &file in &admitted.files {
                check(canceled)?;
                let entry = atlas.atlas().catalog().entry(file).ok_or(AtlasSearchError::WrongAtlas)?;
                if !retained.capture_scope.scope.includes_path(entry.path().raw().as_bytes()) { outside += 1; }
            }
            outside
        } else { retained.capture_scope.query_members_outside(atlas, &scope, canceled)? };
        // Expression admission already owns its selected-ID reservation.
        let scope_bytes = if expression || scope.is_all() { 0 } else {
            eligible_files.checked_mul(size_of::<FileId>()).ok_or(AppError::Admission)?
        };
        let needle = admitted.as_ref().map_or(text, |admitted| admitted.parsed.primary_needle.as_str());
        let capacity = retained.engine.captured_files().min(max_matches);
        let text_capacity = matched_text_capacity(needle, &scope)?;
        let scratch = if scope.case_sensitive() { 2 * 1024 * 1024 }
            else { text_matching::LOWERCASE_SCAN_SCRATCH_BYTES };
        let charge = retained.engine.source_bytes() + (scratch
            + capacity * (size_of::<RetainedFile>() + 64) + max_matches * size_of::<AtlasSearchHit>()
            + MAX_DIAGNOSTICS * size_of::<Unavailable>() + 3 * needle.len() + size_of::<SearchWork>()) as u64;
        let charge = charge.checked_add(scope_bytes as u64).ok_or(AppError::Admission)?;
        let charge = charge.checked_add(retained_text_bytes(max_matches, text_capacity)? as u64)
            .ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(owner, state_id, ByteLength::new(charge))
            .map_err(|_| AppError::Admission)?;
        let (mut parsed, expression, scope_files) = match admitted {
            Some(admitted) => (admitted.parsed, Some(admitted.summary), Some(admitted.files)),
            None => (ParsedQuery { primary_needle: copy_text(text)?, raw_query: copy_text(text)?,
                is_phrase: true, conjunction_terms: Vec::new(), exclusion_terms: Vec::new(),
                path_filters: Vec::new(), lang_filters: Vec::new() }, None,
                scope.freeze(atlas, eligible_files, canceled)?),
        };
        if expression.is_some() {
            // Native index captures deliberately carry no display path. The
            // atlas already applied these exact shared predicates to its
            // UTF-8/escaped search keys and froze BOTH captured and unavailable
            // membership above. Reapplying them to the index's empty path
            // would reject authorized members and corrupt examined counters.
            // Content predicates remain intact for same-capture verification.
            parsed.path_filters.clear();
            parsed.lang_filters.clear();
        }
        let needle = copy_text(&parsed.primary_needle)?;
        let qgen = QueryGeneration::new(owner, generation).map_err(|_| AtlasSearchError::IdentityExhausted)?;
        let mut options = QueryOptions::new(qgen).with_mode(SearchMode::DecodedText {
            case_sensitive: scope.case_sensitive(), normalization: UnicodeNormalization::Exact,
        });
        options.max_matches = max_matches; options.max_bytes_scanned = Some(max_scan_bytes);
        let query = if count_all_matches && expression.is_none() && scope.is_all() {
            OwnedIndexedQuery::new_counting(&retained.engine, parsed, options, &self.budget,
                [ast_id, query_id], || canceled() || atlas.validate_active().is_err())?
        } else if count_all_matches {
            OwnedIndexedQuery::new_scoped_counting(&retained.engine, parsed, options,
                |file| scope::includes_file(&scope_files, file), &self.budget,
                [ast_id, query_id], || canceled() || atlas.validate_active().is_err())?
        } else if expression.is_none() && scope.is_all() {
            OwnedIndexedQuery::new(&retained.engine, parsed, options, &self.budget,
                [ast_id, query_id], || canceled() || atlas.validate_active().is_err())?
        } else {
            OwnedIndexedQuery::new_scoped(&retained.engine, parsed, options,
                |file| scope::includes_file(&scope_files, file), &self.budget,
                [ast_id, query_id], || canceled() || atlas.validate_active().is_err())?
        };
        let unavailable = query.report().unavailable_files().len();
        let eligible_captured = query.report().known_files() - unavailable;
        let examined = unavailable.checked_add(expression.as_ref().map_or(0, |expression| expression.excluded))
            .ok_or(AtlasSearchError::WrongAtlas)?;
        let pending = eligible_files.checked_sub(examined).ok_or(AtlasSearchError::WrongAtlas)?;
        let case_sensitive = scope.case_sensitive();
        let mut snapshot = Snapshot { generation, needle, expression, structure: None,
            structure_rows: Vec::new(), files: reserve(capacity)?,
            scope, scope_files, scope_region, eligible_files,
            hits: reserve(max_matches)?, diagnostics: reserve(MAX_DIAGNOSTICS)?, examined, scanned: 0,
            matched_texts: reserve(if case_sensitive { 0 } else { max_matches })?,
            matched_text_capacity: text_capacity,
            unavailable, pending, matches_seen: 0, matching_files: 0,
            source_bytes_read: 0, read_calls: 0, retained_bytes: 0, count_all_matches,
            truncated: false, complete: false, stop_reason: None, step_count: 0,
            last_step_files: 0, last_step_bytes: 0, last_step_calls: 0,
            index_usage: Some(IndexQueryUsage { generation: index_generation, manifest: retained.engine.id().revision(),
                capture_scope: retained.capture_scope.clone(),
                skipped: 0, verified: 0, fallback: 0, scanned_bytes: 0,
                eligible_captured, capture_unavailable: unavailable }), _lease: lease };
        for diagnostic in &retained.diagnostics {
            check(canceled)?;
            if scope::includes_file(&snapshot.scope_files, diagnostic.file) {
                snapshot.diagnostics.push(Unavailable { file: diagnostic.file, reason: diagnostic.reason });
            }
        }
        self.validate(atlas)?; check(canceled)?;
        Ok(SearchWork { snapshot, execution: SearchExecution::Indexed(IndexedWork {
            query, index_generation, outside_capture_scope, unsupported_seen: 0 }) })
    }

    pub(super) fn advance_indexed_work(&self, atlas: &AtlasSession, candidate: &mut Snapshot,
        work: &mut IndexedWork, canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        let retained = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        if retained.generation != work.index_generation { return Err(AtlasSearchError::StaleIndex); }
        let qgen = QueryGeneration::new(self.manifest.owner(), candidate.generation)
            .map_err(|_| AtlasSearchError::IdentityExhausted)?;
        let report = work.query.step(&retained.engine, 1, qgen,
            || canceled() || atlas.validate_active().is_err())?;
        if report.state() == IndexedQueryState::Canceled { return Err(AtlasSearchError::Canceled); }
        if let Some(error) = report.failure() { return Err(error.into()); }
        if report.state() == IndexedQueryState::Failed { return Err(AtlasSearchError::Index(IndexError::InvalidManifest)); }
        report.validate_delivery(retained.engine.id(), qgen)?;
        self.validate(atlas)?; check(canceled)?;
        let results = report.capture_results();
        let expression_excluded = candidate.expression.as_ref().map_or(0, |expression| expression.excluded);
        candidate.examined = report.examined_files() + report.unavailable_files().len() + expression_excluded;
        candidate.pending = candidate.eligible_files.checked_sub(candidate.examined).ok_or(AtlasSearchError::WrongAtlas)?;
        candidate.scanned = report.scan_attempts();
        candidate.matches_seen = results.total_matches_counted as u64;
        candidate.truncated = results.total_matches_counted > results.matches.len()
            || matches!(results.coverage, SearchCoverage::TruncatedAtLimit { .. });
        candidate.stop_reason = if report.state() == IndexedQueryState::Running { None } else {
            Some(match results.coverage {
                SearchCoverage::TruncatedAtLimit { .. } => AtlasSearchStop::MatchLimit,
                SearchCoverage::BudgetExhausted { .. } => AtlasSearchStop::VerificationByteLimit,
                SearchCoverage::CanceledEarly => return Err(AtlasSearchError::Canceled),
                SearchCoverage::Exhaustive => if candidate.pending == 0 {
                    AtlasSearchStop::AllFilesExamined
                } else if candidate.pending == work.outside_capture_scope {
                    AtlasSearchStop::CaptureScope
                } else { retained.stop },
            })
        };
        // Keep the admitted capture descriptor even after a future index
        // replacement; accepted pages must never borrow the replacement basis.
        let usage = candidate.index_usage.as_mut().ok_or(AtlasSearchError::WrongAtlas)?;
        if usage.generation != work.index_generation || usage.manifest != report.manifest().revision() {
            return Err(AtlasSearchError::WrongAtlas);
        }
        usage.skipped = report.skipped_by_index();
        usage.verified = report.scan_attempts();
        usage.fallback = report.fallback_attempts();
        usage.scanned_bytes = results.scanned_bytes;
        if let Some(expression) = candidate.expression.as_mut() {
            if results.scanned_bytes > expression.max_scan_bytes { return Err(AtlasSearchError::WrongAtlas); }
            expression.scanned_input_bytes = results.scanned_bytes;
        }
        usage.eligible_captured = report.known_files() - report.unavailable_files().len();
        usage.capture_unavailable = report.unavailable_files().len();
        for &file in &results.unsupported_files[work.unsupported_seen..] {
            check(canceled)?; candidate.unavailable(file, "UNSUPPORTED_TEXT");
        }
        work.unsupported_seen = results.unsupported_files.len();
        // A closed original catalog can prove a selected scope exhaustive even
        // when the retained index omitted only excluded files. Selected files
        // absent from that index remain pending; unavailable is never exclusion.
        candidate.complete = report.state() == IndexedQueryState::Finished
            && matches!(results.coverage, SearchCoverage::Exhaustive)
            && atlas.atlas().discovery_complete() && candidate.scope_region.is_none_or(|region| region.missing == 0)
            && candidate.pending == 0 && candidate.unavailable == 0;
        // Only newly verified hits acquire presentation/source pins. Earlier
        // pages are never recopied or renumbered when another file finishes.
        for hit in &results.matches[candidate.hits.len()..] {
            check(canceled)?;
            let source = retained.engine.capture(hit.file_id).ok_or(AtlasSearchError::WrongAtlas)?;
            if hit.revision != source.request().revision() || hit.original_byte_range.is_empty()
                || hit.original_byte_range.end().get() > source.bytes().len() as u64 {
                return Err(AtlasSearchError::WrongAtlas);
            }
            let node = atlas.atlas().node_for_file(hit.file_id).map_err(AtlasSessionError::from)?;
            if candidate.files.last().is_none_or(|f| f.capture.request().file() != hit.file_id) {
                if candidate.files.last().is_some_and(|f| f.capture.request().file() >= hit.file_id) {
                    return Err(AtlasSearchError::WrongAtlas);
                }
                candidate.retained_bytes += source.bytes().len();
                let witness = CaptureWitness::new(source.bytes(),
                    &mut || canceled() || atlas.validate_active().is_err())?;
                candidate.files.push(RetainedFile { capture: source.clone(), node, hits: 0, witness });
            }
            let source_slot = candidate.files.len() - 1;
            if !candidate.scope.case_sensitive() {
                if hit.matched_text.is_empty() || hit.matched_text.len() > candidate.matched_text_capacity {
                    return Err(AtlasSearchError::WrongAtlas);
                }
                candidate.matched_texts.push(copy_text(&hit.matched_text)?);
            }
            candidate.files[source_slot].hits += 1;
            candidate.hits.push(AtlasSearchHit { id: candidate.hits.len() as u64 + 1,
                node, file: hit.file_id, revision: hit.revision,
                original_range: hit.original_byte_range, source_slot });
        }
        Ok(())
    }

    pub fn index_info(&mut self, atlas: &AtlasSession, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; check(&mut canceled)?;
        let mut out = self.output(atlas, "index-info")?;
        let index = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        encode_index(&mut out, atlas, index)?;
        let partial = !capture_complete(atlas, index) || index.engine.statistics().uncovered_files != 0;
        self.finish(atlas, out, partial, &mut canceled)
    }
    pub fn clear_index(&mut self, atlas: &AtlasSession, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?;
        self.building = None;
        check(&mut canceled)?;
        let mut out = self.output(atlas, "index-clear")?;
        out.literal(",\"generation\":")?; out.integer(generation)?;
        out.literal(",\"index_generation\":null,\"indexed_source_bytes\":\"0\"}\n")?;
        let response = self.finish(atlas, out, false, &mut canceled)?;
        self.cancel_relationships_pending();
        self.index = None;
        Ok(response)
    }
}
fn capture_complete(atlas: &AtlasSession, index: &RetainedIndex) -> bool {
    atlas.atlas().discovery_complete() && index.capture_scope.all_requested_catalogued()
        && index.pending == 0 && index.engine.unavailable_files().is_empty()
}
fn encode_index(out: &mut Output, atlas: &AtlasSession, index: &RetainedIndex) -> Result<(), AtlasSearchError> {
    let stats = index.engine.statistics();
    index.capture_scope.encode(out)?;
    out.literal(",\"index_generation\":")?; out.integer(index.generation)?;
    out.literal(",\"index_build_in_progress\":false,\"build_steps\":")?; out.integer(index.build_steps)?;
    out.literal(",\"last_step_files\":")?; out.integer(index.last_build_files as u64)?;
    out.literal(",\"last_step_source_bytes\":")?; out.integer(index.last_build_bytes)?;
    out.literal(",\"last_step_read_calls\":")?; out.integer(index.last_build_calls)?;
    out.literal(",\"capture_manifest\":")?; out.integer(index.engine.id().revision())?;
    out.literal(",\"index_kind\":\"retained-ephemeral-trigrams\",\"source_observation\":\"per-file-captures-not-atomic-workspace\",\"capture_complete\":")?;
    out.boolean(capture_complete(atlas, index))?;
    out.literal(",\"discovery_complete\":")?; out.boolean(atlas.atlas().discovery_complete())?;
    out.literal(",\"stop_reason\":")?; out.quoted(index.stop.code())?;
    for (key, value) in [("captured_files", index.engine.captured_files() as u64),
        ("unavailable_files", index.engine.unavailable_files().len() as u64),
        ("examined_files", index.examined as u64), ("pending_files", index.pending as u64),
        ("indexed_source_bytes", index.engine.source_bytes()), ("initial_source_bytes_read", index.read_bytes),
        ("initial_read_calls", index.read_calls), ("indexed_files", stats.indexed_files as u64),
        ("uncovered_files", stats.uncovered_files as u64), ("unique_grams", stats.unique_grams as u64),
        ("index_retained_bytes", stats.retained_bytes)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    out.literal(",\"diagnostics\":[")?;
    for (i, diagnostic) in index.diagnostics.iter().enumerate() {
        if i != 0 { out.literal(",")?; }
        out.literal("{\"file_id\":")?; out.integer(diagnostic.file.get())?;
        out.literal(",\"reason\":")?; out.quoted(diagnostic.reason)?; out.literal("}")?;
    }
    out.literal("],\"diagnostics_truncated\":")?;
    out.boolean(index.engine.unavailable_files().len() > index.diagnostics.len())?;
    out.literal("}\n")?;
    Ok(())
}

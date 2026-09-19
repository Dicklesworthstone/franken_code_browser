#![forbid(unsafe_code)]

//! Explicit capture/index preparation followed by zero-source-I/O queries.
//! The existing ephemeral engine owns prefiltering and exact verification. All
//! captured members survive between queries, not only previously matching files.
//! The frozen atlas owns native names; this literal-only route needs no path
//! filter keys and never converts an escaped label into source authority.

use std::{cell::{Cell, RefCell}, mem::size_of};
use fcb::search::{CaptureRequest, EphemeralIndex, IndexError, IndexLimits, IndexedQueryState,
    ManifestLimits, MembershipState, ParsedQuery, QueryError, QueryGeneration, QueryOptions,
    SearchCoverage, SearchDocument, SearchManifest};
use fcb::search::index::export::{OwnedEphemeralIndex, SourceRetentionLimits};
use crate::workspace;
use super::*;

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
pub(super) struct RetainedIndex {
    generation: u64,
    engine: OwnedEphemeralIndex,
    diagnostics: Vec<Unavailable>,
    examined: usize,
    pending: usize,
    read_bytes: u64,
    read_calls: u64,
    stop: AtlasSearchStop,
    _lease: ResourceLease,
}
#[derive(Clone, Copy)]
pub(super) struct IndexQueryUsage {
    generation: u64,
    manifest: u64,
    skipped: usize,
    verified: usize,
    fallback: usize,
    scanned_bytes: u64,
}
impl IndexQueryUsage {
    pub(super) fn encode(self, out: &mut Output) -> Result<(), AtlasSearchError> {
        out.literal(",\"index_generation\":")?; out.integer(self.generation)?;
        out.literal(",\"capture_manifest\":")?; out.integer(self.manifest)?;
        for (key, value) in [("skipped_by_index", self.skipped as u64),
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

    /// Capture this catalog once and prepare reusable immutable segments. This
    /// synchronous worker operation may read multiple files; it is NOT the
    /// one-file begin/step route. Generation shares the search attempt allocator,
    /// preventing a new observation from reusing a direct-scan source revision.
    /// Only a fully prepared receipt publishes the candidate. Existing accepted
    /// results and the previous index survive all failed/canceled preparations.
    pub fn prepare_index(&mut self, atlas: &AtlasSession, generation: u64,
        options: AtlasIndexOptions, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?; options.validate()?; check(&mut canceled)?;
        // Catalog revision is fixed for this atlas. A separate capture manifest
        // must never alias it, even on the first index preparation.
        let manifest_revision = generation.checked_add(self.manifest.revision())
            .ok_or(AtlasSearchError::IdentityExhausted)?;
        let manifest_id = SearchManifestId::new(self.manifest.owner(), manifest_revision)?;
        let revision = SourceRevision::new(self.manifest.owner(), generation)
            .map_err(|_| AtlasSearchError::IdentityExhausted)?;
        let [scratch_id, metadata_id, index_id, source_id] =
            [self.next_id()?, self.next_id()?, self.next_id()?, self.next_id()?];
        let catalog = atlas.atlas().catalog();
        let capacity = catalog.entries().len().min(options.max_files);
        let scratch_bytes = 2 * options.max_source_bytes + 256 * 1024
            + capacity * (size_of::<CompleteCapture>() + size_of::<SearchDocument<'_>>() + size_of::<FileId>() + 32);
        let _scratch = self.budget.try_reserve_managed(self.manifest.owner(), scratch_id, ByteLength::new(scratch_bytes as u64))
            .map_err(|_| AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), metadata_id,
            ByteLength::new((size_of::<RetainedIndex>() + MAX_DIAGNOSTICS * size_of::<Unavailable>()) as u64))
            .map_err(|_| AppError::Admission)?;
        let mut captures = reserve(capacity)?;
        let mut unavailable = reserve(capacity)?;
        let mut diagnostics = reserve(MAX_DIAGNOSTICS)?;
        let mut io = workspace::IoCounts::default();
        let mut examined = 0;
        let root = catalog.grant().root_path().to_path_buf();
        for (ordinal, entry) in catalog.entries().iter().take(capacity).enumerate() {
            self.validate(atlas)?; check(&mut canceled)?;
            if io.bytes >= options.max_source_bytes as u64 { break; }
            let file = catalog.file_id(ordinal).ok_or(AtlasSearchError::WrongAtlas)?;
            examined += 1;
            let result = if entry.observed_bytes() > options.max_file_bytes as u64 {
                Err(fcb::source::SourceError::PayloadTooLarge)
            } else {
                let request = CaptureRequest::new(file, revision).map_err(|_| AtlasSearchError::WrongAtlas)?;
                workspace::read_capture(&root, request, entry.path(), options.max_file_bytes,
                    options.max_source_bytes as u64, &mut io, &self.budget,
                    &mut || canceled() || atlas.validate_active().is_err())
            };
            self.validate(atlas)?; check(&mut canceled)?;
            match result {
                Ok(capture) => captures.push(capture),
                Err(fcb::source::SourceError::Canceled) => return Err(AtlasSearchError::Canceled),
                Err(error) => {
                    unavailable.push(file);
                    if diagnostics.len() < MAX_DIAGNOSTICS { diagnostics.push(Unavailable { file, reason: error.code() }); }
                }
            }
        }
        let pending = catalog.entries().len() - examined;
        let stop = if pending == 0 { AtlasSearchStop::AllFilesExamined }
            else if examined == options.max_files { AtlasSearchStop::FileLimit }
            else { AtlasSearchStop::SourceByteLimit };
        let membership = if pending == 0 && atlas.atlas().discovery_complete() {
            MembershipState::Closed
        } else { MembershipState::Discovering };
        let mut documents = reserve(captures.len())?;
        for capture in &captures {
            check(&mut canceled)?;
            // No filter syntax in search_indexed: FileId carries membership and
            // the catalog supplies reversible native paths only when encoding.
            documents.push(SearchDocument::new(capture.request().file(), "", capture));
        }
        let manifest = SearchManifest::new(manifest_id, &documents, &unavailable, membership,
            ManifestLimits { max_files: options.max_files, max_path_bytes: 0 })?;
        let engine = EphemeralIndex::build(manifest, IndexLimits {
            max_source_bytes_per_file: options.max_file_bytes,
            max_source_bytes_total: options.max_source_bytes as u64,
            max_total_grams: options.max_index_grams, max_scratch_bytes: 4 * 1024 * 1024,
            ..IndexLimits::default()
        }, &self.budget, index_id, || canceled() || atlas.validate_active().is_err())?;
        let engine = engine.into_owned(SourceRetentionLimits { max_files: options.max_files,
            max_source_bytes: options.max_source_bytes as u64, max_path_bytes: 0 },
            &self.budget, source_id, || canceled() || atlas.validate_active().is_err())?;
        let candidate = RetainedIndex { generation, engine, diagnostics, examined, pending,
            read_bytes: io.bytes, read_calls: io.calls, stop, _lease: lease };
        let mut out = self.output("index-prepare")?;
        encode_index(&mut out, atlas, &candidate)?;
        let partial = !capture_complete(atlas, &candidate) || candidate.engine.statistics().uncovered_files != 0;
        let response = self.finish(atlas, out, partial, &mut canceled)?;
        self.index = Some(candidate);
        Ok(response)
    }

    /// Repeatable literal search against the explicitly named captured index,
    /// never current disk bytes. Uses the existing exact verifier and fallback
    /// rules (including UTF-16 and short needles). Preparing temporary metadata
    /// is O(captured files); this is not a global inverted-index latency claim.
    /// Output uses the ordinary page/overlay/focus/reader activation workflow.
    pub fn search_indexed(&mut self, atlas: &AtlasSession, generation: u64, index_generation: u64,
        needle: &str, max_matches: usize, max_scan_bytes: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?; check(&mut canceled)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES
            || !(1..=MAX_ATLAS_SEARCH_HITS).contains(&max_matches)
            || max_scan_bytes > MAX_ATLAS_SEARCH_SOURCE_BYTES as u64 { return Err(AtlasSearchError::InvalidLimits); }
        let [state_id, view_id, query_id] = [self.next_id()?, self.next_id()?, self.next_id()?];
        let owner = self.manifest.owner();
        let retained = self.index.as_mut().ok_or(AtlasSearchError::MissingIndex)?;
        if retained.generation != index_generation { return Err(AtlasSearchError::StaleIndex); }
        let capacity = retained.engine.captured_files().min(max_matches);
        // Retained matching sources outlive later index replacement/clear. Query
        // output and existing decoder scratch are separately charged as well.
        let charge = retained.engine.source_bytes() + (2 * 1024 * 1024
            + capacity * (size_of::<RetainedFile>() + 64) + max_matches * size_of::<AtlasSearchHit>()
            + MAX_DIAGNOSTICS * size_of::<Unavailable>() + 3 * needle.len() + size_of::<Snapshot>()) as u64;
        let lease = self.budget.try_reserve_managed(owner, state_id, ByteLength::new(charge))
            .map_err(|_| AppError::Admission)?;
        let parsed = ParsedQuery { primary_needle: copy_text(needle)?, raw_query: copy_text(needle)?,
            is_phrase: true, conjunction_terms: Vec::new(), exclusion_terms: Vec::new(),
            path_filters: Vec::new(), lang_filters: Vec::new() };
        let mut options = QueryOptions::new(QueryGeneration::new(owner, generation)
            .map_err(|_| AtlasSearchError::IdentityExhausted)?);
        options.max_matches = max_matches; options.max_bytes_scanned = Some(max_scan_bytes);
        let mut candidate = Snapshot { generation, needle: copy_text(needle)?, files: reserve(capacity)?,
            hits: reserve(max_matches)?, diagnostics: reserve(MAX_DIAGNOSTICS)?, examined: 0, scanned: 0,
            unavailable: retained.engine.unavailable_files().len(), pending: 0, matches_seen: 0,
            source_bytes_read: 0, read_calls: 0, retained_bytes: 0, truncated: false, complete: false,
            stop_reason: None, step_count: 1, last_step_files: 0, last_step_bytes: 0, last_step_calls: 0,
            index_usage: None, _lease: lease };
        for diagnostic in &retained.diagnostics {
            candidate.diagnostics.push(Unavailable { file: diagnostic.file, reason: diagnostic.reason });
        }
        // A cancellation pulse is sticky across descriptor preparation, engine
        // verification, capture retention and final result preparation.
        let stopped = Cell::new(false);
        let callback = RefCell::new(&mut canceled);
        let stop = || {
            if !stopped.get() { stopped.set((*callback.borrow_mut())() || atlas.validate_active().is_err()); }
            stopped.get()
        };
        let build_stop = retained.stop;
        let result = retained.engine.with_index(&self.budget, view_id, || stop(), |view| {
            let report = view.search(&parsed, options, &self.budget, query_id, || stop())?;
            if report.state() == IndexedQueryState::Canceled || stop() { return Err(AtlasSearchError::Canceled); }
            if let Some(error) = report.failure() { return Err(error.into()); }
            let results = report.capture_results();
            candidate.examined = report.examined_files() + report.unavailable_files().len();
            candidate.pending = atlas.atlas().file_count().checked_sub(candidate.examined).ok_or(AtlasSearchError::WrongAtlas)?;
            candidate.scanned = report.scan_attempts();
            candidate.last_step_files = report.examined_files();
            candidate.matches_seen = results.total_matches_counted as u64;
            candidate.truncated = matches!(results.coverage, SearchCoverage::TruncatedAtLimit { .. });
            candidate.complete = report.is_complete() && candidate.pending == 0;
            candidate.stop_reason = Some(match results.coverage {
                SearchCoverage::TruncatedAtLimit { .. } => AtlasSearchStop::MatchLimit,
                SearchCoverage::BudgetExhausted { .. } => AtlasSearchStop::VerificationByteLimit,
                SearchCoverage::CanceledEarly => return Err(AtlasSearchError::Canceled),
                SearchCoverage::Exhaustive => build_stop,
            });
            candidate.index_usage = Some(IndexQueryUsage { generation: index_generation,
                manifest: report.manifest().revision(), skipped: report.skipped_by_index(),
                verified: report.scan_attempts(), fallback: report.fallback_attempts(), scanned_bytes: results.scanned_bytes });
            for &file in &results.unsupported_files { candidate.unavailable(file, "UNSUPPORTED_TEXT"); }
            for hit in &results.matches {
                if stop() { return Err(AtlasSearchError::Canceled); }
                let documents = view.manifest().documents();
                let ordinal = documents.binary_search_by_key(&hit.file_id, |doc| doc.file_id)
                    .map_err(|_| AtlasSearchError::WrongAtlas)?;
                let source = documents[ordinal].capture;
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
                    candidate.files.push(RetainedFile { capture: source.clone(), node, hits: 0 });
                }
                let source_slot = candidate.files.len() - 1;
                candidate.files[source_slot].hits += 1;
                candidate.hits.push(AtlasSearchHit { id: candidate.hits.len() as u64 + 1,
                    node, file: hit.file_id, revision: hit.revision,
                    original_range: hit.original_byte_range, source_slot });
            }
            Ok::<(), AtlasSearchError>(())
        });
        drop(callback);
        self.validate(atlas)?;
        result??;
        check(&mut canceled)?;
        let mut out = self.output("search-indexed")?;
        encode_page(&mut out, atlas, &candidate, 0, 64, &mut canceled)?;
        let response = self.finish(atlas, out, !candidate.complete, &mut canceled)?;
        self.accepted = Some(candidate);
        Ok(response)
    }

    pub fn index_info(&mut self, atlas: &AtlasSession, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; check(&mut canceled)?;
        let mut out = self.output("index-info")?;
        let index = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        encode_index(&mut out, atlas, index)?;
        let partial = !capture_complete(atlas, index) || index.engine.statistics().uncovered_files != 0;
        self.finish(atlas, out, partial, &mut canceled)
    }
    /// Source/result snapshots already accepted by a query keep their own pins.
    /// This retires only reusable index state; it does not clear accepted rows.
    pub fn clear_index(&mut self, atlas: &AtlasSession, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?; check(&mut canceled)?;
        let mut out = self.output("index-clear")?;
        out.literal(",\"generation\":")?; out.integer(generation)?;
        out.literal(",\"index_generation\":null,\"indexed_source_bytes\":\"0\"}\n")?;
        let response = self.finish(atlas, out, false, &mut canceled)?;
        self.index = None;
        Ok(response)
    }
}
fn capture_complete(atlas: &AtlasSession, index: &RetainedIndex) -> bool {
    atlas.atlas().discovery_complete() && index.pending == 0 && index.engine.unavailable_files().is_empty()
}
fn encode_index(out: &mut Output, atlas: &AtlasSession, index: &RetainedIndex) -> Result<(), AtlasSearchError> {
    let stats = index.engine.statistics();
    out.literal(",\"index_generation\":")?; out.integer(index.generation)?;
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

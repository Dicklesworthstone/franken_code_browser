#![forbid(unsafe_code)]

//! Metadata-only Quick Open for a retained saved archive. The public path
//! engine owns native-byte keys, exact/prefix/fuzzy matching and ranking. This
//! adapter never treats archived names as live paths and never loads a source
//! member while finding files. A selected captured member is independently
//! digest-verified by the existing saved reader operation.

use std::mem::size_of;
use fcb::{ArenaOwnerId, ByteLength, FileId, QueryGeneration};
use fcb::search::{MembershipState, PathEntry, PathIndex, PathIndexLimits, PathRank,
    PathSearch, PathSearchError, PathSearchOptions, PathSearchState, PathStepBudget,
    RootId, SearchManifestId};
pub use fcb::search::{PathCase, PathMatchMode};
use fcb_core::ResourceLease;
use super::{AppError, HostResponse, Output, OutputError, PagedMemberData, RawPath, ReaderSession,
    SavedRepositoryError, SavedRepositorySession, Sha256Digest, check, optional,
    page_limits, query_attempt};

pub const MAX_SAVED_PATH_QUERY_BYTES: usize = fcb::search::paths::MAX_PATH_QUERY_BYTES;
pub const MAX_SAVED_PATH_RESULTS: usize = 256;
pub const MAX_SAVED_PATH_WORK_UNITS: u64 = 1u64 << 32;

/// Work units are the shared ranker's conservative candidate/key/ordering
/// charges, not elapsed time. Metadata index construction is separately bounded
/// by the opened archive's directory limits and managed memory admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SavedPathOptions {
    pub case: PathCase,
    pub mode: PathMatchMode,
    pub max_results: usize,
    pub max_work_units: u64,
}
impl Default for SavedPathOptions {
    fn default() -> Self {
        Self { case: PathCase::UnicodeLowercase, mode: PathMatchMode::Fuzzy,
            max_results: MAX_SAVED_PATH_RESULTS, max_work_units: 64 * 1024 * 1024 }
    }
}

struct PathHit { member: usize, rank: PathRank }

/// Accepted results retain only query text, ordinals and ranks. The immutable
/// archive directory remains the sole authority for each row's raw name and
/// capture status. A new query always starts from the whole directory, never
/// from the previous query's truncated top-k.
pub(super) struct AcceptedPaths {
    generation: QueryGeneration,
    archive: Sha256Digest,
    needle: Vec<u8>,
    options: SavedPathOptions,
    hits: Vec<PathHit>,
    matches_seen: usize,
    files_examined: usize,
    candidates_examined: usize,
    work_units: u64,
    metadata_path_bytes: usize,
    complete: bool,
    _lease: ResourceLease,
}

impl AcceptedPaths {
    fn prepare(saved: &mut SavedRepositorySession, generation: u64, needle: &[u8],
        options: SavedPathOptions, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, SavedRepositoryError> {
        check(canceled)?;
        if needle.is_empty() { return Err(path_error(PathSearchError::EmptyQuery)); }
        if needle.len() > MAX_SAVED_PATH_QUERY_BYTES { return Err(path_error(PathSearchError::QueryTooLong)); }
        if needle.contains(&0) || options.max_results > MAX_SAVED_PATH_RESULTS
            || options.max_work_units > MAX_SAVED_PATH_WORK_UNITS {
            return Err(SavedRepositoryError::InvalidLimits);
        }
        let owner = saved.owner();
        let generation = QueryGeneration::new(owner, generation).map_err(|_| SavedRepositoryError::StaleQuery)?;
        let [result_id, scratch_id, index_id, query_id] = saved.allocations()?;
        let count = saved.archive.directory().len();
        let capacity = options.max_results.min(count);
        let charge = capacity.checked_mul(size_of::<PathHit>())
            .and_then(|n| n.checked_add(needle.len() + size_of::<Self>()))
            .ok_or(AppError::Admission)?;
        let lease = saved.budget.try_reserve_managed(owner, result_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut query_bytes = reserve(needle.len())?;
        query_bytes.extend_from_slice(needle);
        let mut hits = reserve(capacity)?;

        // Reserve raw-path scratch before copying any metadata. PathIndex owns
        // a separate reservation for its prepared keys and sorted components;
        // it and all candidate scratch disappear before result publication.
        let mut metadata_path_bytes = 0usize;
        for ordinal in 0..count {
            check(canceled)?;
            let member = saved.archive.directory().member(ordinal).ok_or(SavedRepositoryError::MissingHit)?;
            metadata_path_bytes = metadata_path_bytes.checked_add(member.path.len()).ok_or(AppError::Admission)?;
        }
        let scratch_bytes = count.checked_mul(size_of::<RawPath>() + size_of::<PathEntry<'_>>() + 4 * size_of::<usize>())
            .and_then(|n| n.checked_add(metadata_path_bytes))
            .and_then(|n| n.checked_add(4096)).ok_or(AppError::Admission)?;
        let _scratch = saved.budget.try_reserve_managed(owner, scratch_id, ByteLength::new(scratch_bytes as u64))
            .map_err(|_| AppError::Admission)?;
        let mut paths = reserve(count)?;
        for ordinal in 0..count {
            check(canceled)?;
            let member = saved.archive.directory().member(ordinal).ok_or(SavedRepositoryError::MissingHit)?;
            paths.push(RawPath::from_bytes(member.path));
        }
        let mut entries = reserve(count)?;
        let root = RootId::new(owner, 1).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        for (ordinal, path) in paths.iter().enumerate() {
            check(canceled)?;
            let file = FileId::new(owner, ordinal as u64 + 1).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
            entries.push(PathEntry::new(file, root, path));
        }
        let manifest = SearchManifestId::new(owner, 1).map_err(AppError::from)?;
        // This directory is closed even when the historical live discovery was
        // incomplete. Both facts remain separate on the wire. Unavailable
        // source members still have searchable metadata and participate fully.
        let index = PathIndex::build(manifest, MembershipState::Closed, &entries,
            PathIndexLimits { max_files: count,
                max_total_path_bytes: metadata_path_bytes, max_components: 4_000_000,
                ..PathIndexLimits::default() }, &saved.budget, index_id, &mut *canceled)
            .map_err(path_error)?;
        let mut search_options = PathSearchOptions::new(generation);
        search_options.case = options.case;
        search_options.mode = options.mode;
        search_options.max_results = options.max_results;
        let mut work = PathSearch::new(&index, needle, search_options, &saved.budget, query_id)
            .map_err(path_error)?;
        while work.state() == PathSearchState::Running {
            check(canceled)?;
            let remaining = options.max_work_units - work.work_units();
            let next = work.next_work_units();
            if next as u64 > remaining { break; }
            // An empty candidate range can finish with zero charged work.
            // Otherwise never spend more than the remaining aggregate budget.
            let step = PathStepBudget { max_candidates: 256,
                max_units: if next == 0 { 1 } else {
                    remaining.min(PathStepBudget::default().max_units as u64) as usize
                } };
            work.step(step, generation, &mut *canceled).map_err(path_error)?;
            // Cancellation callbacks may be one-shot. Preserve the engine's
            // terminal state even if a subsequent callback returns false.
            if work.state() == PathSearchState::Canceled { return Err(SavedRepositoryError::Canceled); }
        }
        check(canceled)?;
        for hit in work.ranked_matches() {
            let member = usize::try_from(hit.file_id().get() - 1).map_err(|_| SavedRepositoryError::MissingHit)?;
            let source = saved.archive.directory().member(member).ok_or(SavedRepositoryError::MissingHit)?;
            if source.path != hit.path().raw_path().as_bytes() { return Err(SavedRepositoryError::MissingHit); }
            hits.push(PathHit { member, rank: hit.rank() });
        }
        Ok(Self { generation, archive: saved.archive_digest(), needle: query_bytes,
            options, hits, matches_seen: work.matches_seen(), files_examined: work.files_examined(),
            candidates_examined: work.candidates_examined(), work_units: work.work_units(),
            metadata_path_bytes, complete: work.is_complete(), _lease: lease })
    }

    fn validate(&self, saved: &SavedRepositorySession, generation: u64) -> Result<(), SavedRepositoryError> {
        if self.generation.get() != generation || self.generation.owner() != saved.owner()
            || self.archive != saved.archive_digest() { return Err(SavedRepositoryError::StaleQuery); }
        Ok(())
    }
    fn hit(&self, hit_id: u64) -> Result<&PathHit, SavedRepositoryError> {
        let position = hit_id.checked_sub(1).and_then(|n| usize::try_from(n).ok())
            .ok_or(SavedRepositoryError::MissingHit)?;
        self.hits.get(position).ok_or(SavedRepositoryError::MissingHit)
    }
    fn page(&self, saved: &mut SavedRepositorySession, generation: u64,
        start: usize, limit: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, SavedRepositoryError> {
        self.validate(saved, generation)?;
        page_limits(start, limit, self.hits.len())?;
        let mut out = saved.output("paths")?;
        self.encode_page(saved, start, limit, &mut out, canceled)?;
        saved.finish(out, !self.complete, canceled)
    }
    fn encode_page(&self, saved: &SavedRepositorySession, start: usize, limit: usize,
        out: &mut Output, canceled: &mut impl FnMut() -> bool) -> Result<(), SavedRepositoryError> {
        out.literal(",\"selection_namespace\":\"saved-paths\",\"path_generation\":")?;
        out.integer(self.generation.get())?;
        out.literal(",\"query_hex\":")?; out.hex(&self.needle)?;
        out.literal(",\"mode\":")?;
        out.quoted(match self.options.mode { PathMatchMode::Exact => "exact",
            PathMatchMode::Prefix => "prefix", PathMatchMode::Fuzzy => "fuzzy" })?;
        out.literal(",\"case\":")?;
        out.quoted(match self.options.case { PathCase::Sensitive => "sensitive",
            PathCase::UnicodeLowercase => "unicode-lowercase" })?;
        out.literal(",\"key_version\":")?; out.integer(fcb::search::paths::PATH_KEY_VERSION as u64)?;
        out.literal(",\"metadata_scope\":\"archive-directory\",\"identity_scope\":\"retained-saved-query\",\"ordering\":\"ranked-archive-paths\",\"unicode_normalization\":\"none\",\"source_payload_read\":false,\"live_roots_accessed\":false,\"persistent_index_used\":false,\"retained_source_payload_bytes\":\"0\",\"member_bytes_read\":\"0\",\"member_read_calls\":\"0\",\"search_complete\":")?;
        out.boolean(self.complete)?;
        out.literal(",\"matches_seen_exact\":")?; out.boolean(self.complete)?;
        out.literal(",\"top_k_complete\":")?; out.boolean(self.complete)?;
        out.literal(",\"truncated\":")?; out.boolean(self.matches_seen > self.hits.len())?;
        out.literal(",\"work_limited\":")?; out.boolean(!self.complete)?;
        out.literal(",\"state\":")?; out.quoted(if self.complete { "complete" } else { "work-limit" })?;
        for (key, value) in [
            ("max_results", self.options.max_results as u64), ("max_work_units", self.options.max_work_units),
            ("matches_seen", self.matches_seen as u64), ("retained_hits", self.hits.len() as u64),
            ("files_examined", self.files_examined as u64), ("candidates_examined", self.candidates_examined as u64),
            ("work_units", self.work_units), ("metadata_path_bytes", self.metadata_path_bytes as u64),
            ("offset", start as u64), ("limit", limit as u64),
        ] { out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?; }
        out.literal(",\"hits\":[")?;
        let end = start.saturating_add(limit).min(self.hits.len());
        let mut emitted = 0usize;
        for (position, hit) in self.hits[start..end].iter().enumerate() {
            check(canceled)?;
            let checkpoint = out.checkpoint();
            let encoded: Result<(), SavedRepositoryError> = (|| {
                if emitted > 0 { out.literal(",")?; }
                let member = saved.archive.directory().member(hit.member).ok_or(SavedRepositoryError::MissingHit)?;
                out.literal("{\"hit_id\":")?; out.integer((start + position) as u64 + 1)?;
                out.literal(",\"member\":")?; out.integer(hit.member as u64)?;
                out.literal(",\"file_id\":")?; out.integer(hit.member as u64 + 1)?;
                out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(member.path).to_path_buf())?;
                out.literal(",\"observed_bytes\":")?; out.integer(member.observed_bytes)?;
                match member.data {
                    PagedMemberData::Captured { digest, .. } => {
                        out.literal(",\"captured\":true,\"source_digest\":")?; out.quoted(&digest.to_hex())?;
                    }
                    PagedMemberData::Unavailable(reason) => {
                        out.literal(",\"captured\":false,\"reason\":")?; out.quoted(reason)?;
                    }
                }
                out.literal(",\"match_kind\":")?; out.quoted(match_kind(hit.rank))?;
                out.literal("}")?;
                // Native names can expand when encoded as both reversible hex
                // and escaped display. Keep the complete footer admitted and
                // continue at the first unencoded retained row on the next page.
                if out.remaining_bytes() < 128 { return Err(OutputError::Limit.into()); }
                Ok(())
            })();
            match encoded {
                Ok(()) => emitted += 1,
                Err(SavedRepositoryError::App(AppError::Output(OutputError::Limit))) if emitted > 0 => {
                    out.restore(checkpoint)?;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        let next = start.checked_add(emitted).ok_or(AppError::Admission)?;
        out.literal("],\"next_offset\":")?;
        optional(out, (next < self.hits.len()).then_some(next as u64))?;
        out.literal("}\n")?;
        Ok(())
    }
}

impl SavedRepositorySession {
    pub fn accepted_path_generation(&self) -> Option<u64> {
        self.paths.as_ref().map(|query| query.generation.get())
    }
    pub const fn last_path_attempt(&self) -> u64 { self.last_path_attempt }

    /// Independent monotonic path generations preserve earlier accepted rows
    /// on errors/cancellation. Source, expression and relationship queries are
    /// not replaced. Publication occurs only after the first response is ready.
    pub fn path_search(&mut self, generation: u64, needle: &[u8], options: SavedPathOptions,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedRepositoryError> {
        query_attempt(&mut self.last_path_attempt, generation)?;
        let candidate = AcceptedPaths::prepare(self, generation, needle, options, &mut canceled)?;
        let response = candidate.page(self, generation, 0, 64, &mut canceled)?;
        self.paths = Some(candidate);
        Ok(response)
    }

    /// Paging never reranks and never reads any source or index artifact.
    pub fn path_results(&mut self, generation: u64, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedRepositoryError> {
        check(&mut canceled)?;
        let accepted = self.accepted_paths(generation)?;
        page_limits(start, limit, accepted.hits.len())?;
        let partial = !accepted.complete;
        let mut out = self.output("paths")?;
        self.accepted_paths(generation)?.encode_page(self, start, limit, &mut out, &mut canceled)?;
        self.finish(out, partial, &mut canceled)
    }

    pub fn clear_paths(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedRepositoryError> {
        query_attempt(&mut self.last_path_attempt, generation)?;
        check(&mut canceled)?;
        let mut out = self.output("paths-clear")?;
        out.literal(",\"selection_namespace\":\"saved-paths\",\"path_generation\":")?;
        out.integer(generation)?;
        out.literal(",\"accepted_path_generation\":null,\"retained_hits\":\"0\"}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        self.paths = None;
        Ok(response)
    }

    /// Resolve the selected ordinal from the retained query, then reuse the
    /// ordinary verified saved-member reader. The standard open-reader receipt
    /// identifies that exact member with a null content-search selection. No
    /// live-root lookup or source-text search is implied by a filename result.
    pub fn open_path_hit_reader(&mut self, reader_owner: ArenaOwnerId, generation: u64,
        hit_id: u64, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), SavedRepositoryError> {
        check(&mut canceled)?;
        let member = self.accepted_paths(generation)?.hit(hit_id)?.member;
        self.open_member_reader(reader_owner, member, &mut canceled)
    }

    fn accepted_paths(&self, generation: u64) -> Result<&AcceptedPaths, SavedRepositoryError> {
        let query = self.paths.as_ref().ok_or(SavedRepositoryError::MissingQuery)?;
        query.validate(self, generation)?;
        Ok(query)
    }
}

fn match_kind(rank: PathRank) -> &'static str {
    use fcb::search::PathMatchKind::*;
    match rank.kind {
        ExactFilename => "exact-filename", ExactPath => "exact-path", ExactComponent => "exact-component",
        FilenamePrefix => "filename-prefix", ComponentPrefix => "component-prefix", PathPrefix => "path-prefix",
        FilenameSubsequence => "filename-subsequence", PathSubsequence => "path-subsequence",
    }
}
fn path_error(error: PathSearchError) -> SavedRepositoryError { AppError::Path(error).into() }
fn reserve<T>(count: usize) -> Result<Vec<T>, SavedRepositoryError> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| AppError::Admission)?;
    if values.capacity() > count { return Err(AppError::Admission.into()); }
    Ok(values)
}

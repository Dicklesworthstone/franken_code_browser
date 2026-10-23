#![forbid(unsafe_code)]

//! Progressive filename navigation over an independently bounded metadata walk.
//! Each immutable query owns a fresh traversal. The shared path index/ranker is
//! built only for one discovery batch, then its top-k contribution is merged
//! into retained raw-path hits. Neither visited metadata nor prepared keys grow
//! with workspace size. No source or ignore-rule payload is read while finding.
//!
//! Consuming a directory iterator cannot be rolled back. The catalog retains a
//! pending batch until matching and the entire result merge commit together.
//! Each committed batch advances the result revision before response delivery.
//! A retry of the last advance's predecessor replays accepted results; it never
//! authorizes another walk. A current revision authorizes the next bounded chunk.

use std::{cmp::Ordering, mem::size_of, path::Path};
use fcb::{ArenaOwnerId, ByteLength, FileId, QueryGeneration};
use fcb::search::{MembershipState, PathEntry, PathIndex, PathIndexLimits, PathMatch,
    PathRank, PathSearch, PathSearchError, PathSearchOptions, PathSearchState, SearchManifestId};
pub use fcb::search::{PathCase, PathMatchMode};
use fcb::source::{NormalizedPath, RootGrant};
use fcb_core::ResourceLease;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL};
use crate::output::{Output, OutputError};
use super::{CatalogSession, CatalogSessionError, MAX_CATALOG_PAGE_ENTRIES,
    MAX_CATALOG_PAGE_PATH_BYTES, MAX_CATALOG_PATH_BYTES};
use super::super::{HostResponse, MAX_HOST_TEXT_BYTES, reader::ReaderSession};

pub const MAX_FILE_FINDER_QUERY_BYTES: usize = fcb::search::paths::MAX_PATH_QUERY_BYTES;
pub const MAX_FILE_FINDER_RESULTS: usize = 256;
pub const MAX_FILE_FINDER_BATCHES: usize = 16;
pub const MAX_FILE_FINDER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FILE_FINDER_RETAINED_PATH_BYTES: usize = MAX_FILE_FINDER_RESULTS * MAX_CATALOG_PATH_BYTES;
// Retained hits + candidate merge + normalized-path component vectors + query
// and batch-entry scratch. The shared index/ranker and output reserve their own
// charges from the SAME catalog budget. This never depends on visited total.
const FINDER_STATE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileFinderSessionError {
    Catalog(CatalogSessionError), Search(PathSearchError), App(AppError),
    InvalidLimits, ForeignOwner, StaleQuery, StaleRevision, MissingHit,
    NoMoreResults, Canceled, IdentityExhausted,
}
impl FileFinderSessionError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Search(PathSearchError::Canceled) => true,
            Self::Catalog(error) => error.is_canceled(), Self::App(error) => error.is_canceled(),
            _ => false,
        }
    }
}
impl std::fmt::Display for FileFinderSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Catalog(e) => write!(f, "{e}"), Self::Search(e) => write!(f, "{e}"), Self::App(e) => write!(f, "{e}"),
            Self::InvalidLimits => f.write_str("FILE_FINDER_INVALID_LIMITS"),
            Self::ForeignOwner => f.write_str("FILE_FINDER_FOREIGN_OWNER"),
            Self::StaleQuery => f.write_str("FILE_FINDER_STALE_QUERY"),
            Self::StaleRevision => f.write_str("FILE_FINDER_STALE_REVISION"),
            Self::MissingHit => f.write_str("FILE_FINDER_MISSING_HIT"),
            Self::NoMoreResults => f.write_str("FILE_FINDER_NO_MORE_RESULTS"),
            Self::Canceled => f.write_str("FILE_FINDER_CANCELED"),
            Self::IdentityExhausted => f.write_str("FILE_FINDER_IDENTITY_EXHAUSTED"),
        }
    }
}
impl std::error::Error for FileFinderSessionError {}
impl From<CatalogSessionError> for FileFinderSessionError { fn from(e: CatalogSessionError) -> Self { Self::Catalog(e) } }
impl From<PathSearchError> for FileFinderSessionError { fn from(e: PathSearchError) -> Self { Self::Search(e) } }
impl From<AppError> for FileFinderSessionError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<OutputError> for FileFinderSessionError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileFinderOptions { pub max_results: usize, pub case: PathCase, pub mode: PathMatchMode }
impl Default for FileFinderOptions {
    fn default() -> Self { Self { max_results: MAX_FILE_FINDER_RESULTS,
        case: PathCase::UnicodeLowercase, mode: PathMatchMode::Fuzzy } }
}
#[derive(Clone)]
struct Hit { file: FileId, path: NormalizedPath, rank: PathRank }

pub struct FileFinderSession {
    catalog: CatalogSession,
    generation: QueryGeneration,
    needle: Vec<u8>,
    options: FileFinderOptions,
    hits: Vec<Hit>,
    revision: u64,
    processed_page: u64,
    replay_after_revision: Option<u64>,
    searched_files: u64,
    matches_seen: u64,
    files_examined: u64,
    candidates_examined: u64,
    work_units: u64,
    paused: bool,
    _lease: ResourceLease,
}
impl FileFinderSession {
    /// Fix one query for this session's lifetime. Changing text/mode/case must
    /// create another session and traverse from the root; retained top-k is not
    /// a candidate universe for refinements. Initial revision1 deliberately has
    /// an unprocessed first batch, so even a finished walk is not search-complete.
    pub fn open(owner: ArenaOwnerId, root: &Path, generation: u64, needle: &[u8],
        options: FileFinderOptions, mut canceled: impl FnMut() -> bool)
        -> Result<Self, FileFinderSessionError> {
        if needle.is_empty() { return Err(PathSearchError::EmptyQuery.into()); }
        if needle.len() > MAX_FILE_FINDER_QUERY_BYTES { return Err(PathSearchError::QueryTooLong.into()); }
        if !(1..=MAX_FILE_FINDER_RESULTS).contains(&options.max_results) || needle.contains(&0) {
            return Err(FileFinderSessionError::InvalidLimits);
        }
        let generation = QueryGeneration::new(owner, generation).map_err(|_| FileFinderSessionError::StaleQuery)?;
        check(&mut canceled)?;
        let mut catalog = CatalogSession::open(owner, root, &mut canceled)?;
        let allocation = catalog.next_id()?;
        let lease = catalog.budget.try_reserve_managed(owner, allocation,
            ByteLength::new(FINDER_STATE_BYTES + size_of::<Self>() as u64)).map_err(|_| AppError::Admission)?;
        let mut raw = reserve(needle.len())?; raw.extend_from_slice(needle);
        let hits = reserve(options.max_results)?;
        check(&mut canceled)?; catalog.validate_active()?;
        Ok(Self { catalog, generation, needle: raw, options, hits, revision: 1, processed_page: 0,
            replay_after_revision: None, searched_files: 0, matches_seen: 0, files_examined: 0,
            candidates_examined: 0, work_units: 0, paused: false, _lease: lease })
    }

    pub fn owner(&self) -> ArenaOwnerId { self.catalog.owner() }
    pub fn grant(&self) -> &RootGrant { self.catalog.grant() }
    pub fn validate_active(&self) -> Result<(), FileFinderSessionError> { Ok(self.catalog.validate_active()?) }

    /// Recovery always includes the current accepted rows and revision; it does
    /// not consume discovery or pending matching work.
    pub fn info(&mut self, canceled: impl FnMut() -> bool) -> Result<HostResponse, FileFinderSessionError> {
        self.snapshot("info", canceled)
    }
    pub fn page(&mut self, generation: u64, revision: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, FileFinderSessionError> {
        self.validate_revision(generation, revision)?;
        self.snapshot("page", canceled)
    }
    pub fn stop(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, FileFinderSessionError> {
        self.validate_generation(generation)?;
        check(&mut canceled)?; self.validate_active()?;
        self.paused = self.more();
        self.snapshot("stop", canceled)
    }

    /// At most sixteen bounded discovery batches per explicit worker request.
    /// Stop early when the first usable hits appear so the UI can present them
    /// before spending the rest of the chunk. Later Continue requests improve
    /// rankings/counts without an implicit global catalog limit.
    pub fn advance(&mut self, generation: u64, after_revision: u64, max_batches: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, FileFinderSessionError> {
        self.validate_generation(generation)?;
        if !(1..=MAX_FILE_FINDER_BATCHES).contains(&max_batches) { return Err(FileFinderSessionError::InvalidLimits); }
        if self.replay_after_revision == Some(after_revision) && after_revision < self.revision {
            return self.snapshot("page", canceled);
        }
        if after_revision != self.revision { return Err(FileFinderSessionError::StaleRevision); }
        if !self.more() { return Err(FileFinderSessionError::NoMoreResults); }
        check(&mut canceled)?; self.validate_active()?;
        self.paused = false;
        let had_hits = !self.hits.is_empty();
        for _ in 0..max_batches {
            check(&mut canceled)?;
            if !self.pending_batch() {
                // The catalog stores its consumed page BEFORE any fallible
                // rank/index/merge work below. processed_page stays unchanged.
                self.catalog.consume_page(&mut canceled)?;
            }
            self.merge_pending(after_revision, &mut canceled)?;
            if !self.more() || (!had_hits && !self.hits.is_empty()) { break; }
        }
        self.snapshot("page", canceled)
    }

    /// Metadata hits authorize a new capture, not an old observed file length.
    /// All caller proofs are checked before root/path metadata or source I/O.
    pub fn open_reader(&mut self, reader_owner: ArenaOwnerId, generation: u64, revision: u64,
        file: FileId, max_source_bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), FileFinderSessionError> {
        if file.owner() != self.owner() || reader_owner == self.owner() { return Err(FileFinderSessionError::ForeignOwner); }
        self.validate_revision(generation, revision)?;
        let index = self.hits.iter().position(|hit| hit.file == file).ok_or(FileFinderSessionError::MissingHit)?;
        if max_source_bytes == 0 || max_source_bytes > MAX_HOST_TEXT_BYTES { return Err(FileFinderSessionError::InvalidLimits); }
        let relative = self.hits[index].path.clone();
        let (reader, info, window, source_bytes, source_calls) =
            self.catalog.capture_reader(reader_owner, &relative, max_source_bytes, &mut canceled)?;
        let mut out = self.output("open-reader")?;
        out.literal(",\"selection\":")?; encode_hit(&mut out, &self.hits[index])?;
        out.literal(",\"reader_owner\":")?; out.integer(reader_owner.get())?;
        out.literal(",\"source_observation\":\"new-capture-after-metadata-selection\",\"source_bytes_read\":")?;
        out.integer(source_bytes)?;
        out.literal(",\"source_read_calls\":")?; out.integer(source_calls)?;
        out.literal(",\"reader\":")?; out.literal(info.as_str())?;
        out.literal(",\"window\":")?; out.literal(window.as_str())?;
        out.literal("}\n")?;
        let response = self.finish(out, &mut canceled)?;
        Ok((reader, response))
    }

    fn validate_generation(&self, generation: u64) -> Result<(), FileFinderSessionError> {
        if generation != self.generation.get() { Err(FileFinderSessionError::StaleQuery) } else { Ok(()) }
    }
    fn validate_revision(&self, generation: u64, revision: u64) -> Result<(), FileFinderSessionError> {
        self.validate_generation(generation)?;
        if revision != self.revision { Err(FileFinderSessionError::StaleRevision) } else { Ok(()) }
    }
    fn pending_batch(&self) -> bool { self.processed_page < self.catalog.latest_page }
    fn more(&self) -> bool { self.pending_batch() || self.catalog.discovery.is_some() }
    fn complete(&self) -> bool { self.catalog.complete && !self.pending_batch() }

    fn merge_pending(&mut self, after_revision: u64, canceled: &mut impl FnMut() -> bool)
        -> Result<(), FileFinderSessionError> {
        check(canceled)?; self.validate_active()?;
        let next_revision = self.revision.checked_add(1).ok_or(FileFinderSessionError::IdentityExhausted)?;
        let index_allocation = self.catalog.next_id()?;
        let query_allocation = self.catalog.next_id()?;
        let page = self.catalog.pages.back().ok_or(FileFinderSessionError::NoMoreResults)?;
        if page.id <= self.processed_page { return Err(FileFinderSessionError::NoMoreResults); }
        let mut entries = reserve(page.files.len())?;
        for file in &page.files { entries.push(PathEntry::new(file.id, self.catalog.grant.root_id(), file.path.raw())); }
        let index_id = SearchManifestId::new(self.owner(), page.id).map_err(|_| FileFinderSessionError::IdentityExhausted)?;
        let index = PathIndex::build(index_id, MembershipState::Closed, &entries,
            PathIndexLimits { max_files: MAX_CATALOG_PAGE_ENTRIES, max_path_bytes: MAX_CATALOG_PATH_BYTES,
                max_total_path_bytes: MAX_CATALOG_PAGE_PATH_BYTES,
                max_components: MAX_CATALOG_PAGE_PATH_BYTES + MAX_CATALOG_PAGE_ENTRIES },
            &self.catalog.budget, index_allocation, || canceled() || self.validate_active().is_err())?;
        let mut options = PathSearchOptions::new(self.generation);
        options.case = self.options.case; options.mode = self.options.mode; options.max_results = self.options.max_results;
        let mut query = PathSearch::new(&index, &self.needle, options, &self.catalog.budget, query_allocation)?;
        query.run_to_completion(|| canceled() || self.validate_active().is_err())?;
        if query.state() != PathSearchState::Finished { return Err(FileFinderSessionError::Canceled); }
        let mut candidate = reserve(self.options.max_results)?;
        let mut old = 0; let mut incoming = 0;
        let new = query.ranked_matches();
        while candidate.len() < self.options.max_results && (old < self.hits.len() || incoming < new.len()) {
            check(canceled)?;
            let take_old = match (self.hits.get(old), new.get(incoming)) {
                (Some(left), Some(right)) => compare_hit(left, *right) != Ordering::Greater,
                (Some(_), None) => true, _ => false,
            };
            if take_old { candidate.push(self.hits[old].clone()); old += 1; }
            else {
                let hit = new[incoming];
                let position = page.files.binary_search_by_key(&hit.file_id(), |file| file.id)
                    .map_err(|_| FileFinderSessionError::MissingHit)?;
                candidate.push(Hit { file: hit.file_id(), path: page.files[position].path.clone(), rank: hit.rank() });
                incoming += 1;
            }
        }
        let searched_files = add(self.searched_files, page.files.len() as u64)?;
        let matches_seen = add(self.matches_seen, query.matches_seen() as u64)?;
        let files_examined = add(self.files_examined, query.files_examined() as u64)?;
        let candidates_examined = add(self.candidates_examined, query.candidates_examined() as u64)?;
        let work_units = add(self.work_units, query.work_units())?;
        check(canceled)?; self.validate_active()?;
        // Atomic accepted-batch publication. Nothing below this point can fail
        // before all rows/counters/replay authority refer to this same revision.
        self.hits = candidate; self.searched_files = searched_files;
        self.matches_seen = matches_seen; self.files_examined = files_examined;
        self.candidates_examined = candidates_examined; self.work_units = work_units;
        self.processed_page = page.id; self.revision = next_revision;
        self.replay_after_revision = Some(after_revision);
        Ok(())
    }

    fn snapshot(&mut self, command: &str, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, FileFinderSessionError> {
        check(&mut canceled)?; self.validate_active()?;
        let mut out = self.output(command)?;
        out.literal(",\"hits\":[")?;
        for (i, hit) in self.hits.iter().enumerate() {
            check(&mut canceled)?; if i != 0 { out.literal(",")?; } encode_hit(&mut out, hit)?;
        }
        out.literal("]}\n")?;
        self.finish(out, &mut canceled)
    }
    fn output(&mut self, command: &str) -> Result<Output, FileFinderSessionError> {
        let allocation = self.catalog.next_id()?;
        let mut out = Output::new(self.owner(), MAX_FILE_FINDER_RESPONSE_BYTES, &self.catalog.budget, allocation)?;
        out.literal("{\"schema\":\"fcb.file-finder-session/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        for (key, value) in [("owner", self.owner().get()), ("query_generation", self.generation.get()),
            ("revision", self.revision), ("key_version", fcb::search::paths::PATH_KEY_VERSION as u64)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"query_hex\":")?; out.hex(&self.needle)?;
        out.literal(",\"mode\":")?; out.quoted(match self.options.mode { PathMatchMode::Exact => "exact", PathMatchMode::Prefix => "prefix", PathMatchMode::Fuzzy => "fuzzy" })?;
        out.literal(",\"case\":")?; out.quoted(match self.options.case { PathCase::Sensitive => "sensitive", PathCase::UnicodeLowercase => "unicode-lowercase" })?;
        out.literal(",\"identity_scope\":\"retained-query-session\",\"ordering\":\"ranked-observed-paths\",\"policy\":\"product-defaults/no-rule-files-v1\",\"source_payload_read\":")?;
        out.boolean(command == "open-reader")?;
        out.literal(",\"discovery_source_bytes_read\":\"0\",\"discovery_source_read_calls\":\"0\",\"state\":")?;
        out.quoted(if self.complete() { "complete" } else if !self.more() { "partial" }
            else if self.paused { "paused" } else { "searching" })?;
        for (key, value) in [("more", self.more()), ("paused", self.paused && self.more()),
            ("discovery_complete", self.catalog.complete), ("search_complete", self.complete()),
            ("pending_batch", self.pending_batch()), ("truncated", self.matches_seen > self.hits.len() as u64)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.boolean(value)?;
        }
        for (key, value) in [("catalogued_files", self.catalog.catalogued_files), ("searched_files", self.searched_files),
            ("matches_seen", self.matches_seen), ("files_examined", self.files_examined),
            ("candidates_examined", self.candidates_examined), ("work_units", self.work_units),
            ("retained_hits", self.hits.len() as u64),
            ("retained_path_bytes", self.hits.iter().map(|hit| hit.path.as_bytes().len() as u64).sum()),
            ("processed_batches", self.processed_page)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"replay_after_revision\":")?;
        match self.replay_after_revision { Some(value) => out.integer(value)?, None => out.literal("null")? }
        out.literal(",\"limits\":{")?;
        for (i, (key, value)) in [("max_results", self.options.max_results), ("max_path_bytes", MAX_CATALOG_PATH_BYTES),
            ("max_retained_path_bytes", MAX_FILE_FINDER_RETAINED_PATH_BYTES),
            ("max_batches_per_advance", MAX_FILE_FINDER_BATCHES), ("response_bytes", MAX_FILE_FINDER_RESPONSE_BYTES)].into_iter().enumerate() {
            if i != 0 { out.literal(",")?; } out.quoted(key)?; out.literal(":")?; out.integer(value as u64)?;
        }
        out.literal("},\"discovery\":{")?;
        let a = self.catalog.aggregate;
        for (i, (key, value)) in [("files", a.files), ("directories", a.directories),
            ("excluded_entries", a.excluded), ("symlinks_not_followed", a.symlinks),
            ("special_objects", a.special), ("unavailable", a.unavailable), ("cycles", a.cycles),
            ("path_limited", a.path_limited), ("depth_limited", a.depth_limited), ("queue_refused", a.queue_refused)].into_iter().enumerate() {
            if i != 0 { out.literal(",")?; } out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal("}")?;
        Ok(out)
    }
    fn finish(&mut self, out: Output, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, FileFinderSessionError> {
        self.validate_active()?; check(canceled)?;
        let charge = out.as_bytes().len().checked_mul(3).and_then(|n| n.checked_add(4096)).ok_or(AppError::Admission)?;
        let allocation = self.catalog.next_id()?;
        let lease = self.catalog.budget.try_reserve_managed(self.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut text = String::new(); text.try_reserve_exact(out.as_bytes().len()).map_err(|_| AppError::Admission)?;
        if text.capacity() > out.as_bytes().len() { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        self.validate_active()?; check(canceled)?;
        Ok(HostResponse { text, exit_code: if self.complete() { EXIT_OK } else { EXIT_PARTIAL }, _lease: lease })
    }
}

fn compare_hit(left: &Hit, right: PathMatch<'_>) -> Ordering {
    // This session has exactly one RootId, so the shared comparator's root tie
    // is equal. Reuse the actual PathRank (no host scoring), then its exact raw
    // path/FileId tie order. Batch partitioning cannot change ranked results.
    left.rank.cmp(&right.rank()).then_with(|| left.path.raw().cmp(right.path().raw_path()))
        .then_with(|| left.file.cmp(&right.file_id()))
}
fn encode_hit(out: &mut Output, hit: &Hit) -> Result<(), FileFinderSessionError> {
    use fcb::search::PathMatchKind::*;
    let kind = match hit.rank.kind { ExactFilename => "exact-filename", ExactPath => "exact-path",
        ExactComponent => "exact-component", FilenamePrefix => "filename-prefix", ComponentPrefix => "component-prefix",
        PathPrefix => "path-prefix", FilenameSubsequence => "filename-subsequence", PathSubsequence => "path-subsequence" };
    out.literal("{\"file_id\":")?; out.integer(hit.file.get())?;
    out.literal(",\"match_kind\":")?; out.quoted(kind)?;
    out.literal(",\"path\":")?; out.path(&hit.path.raw().to_path_buf())?;
    out.literal("}")?; Ok(())
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), FileFinderSessionError> {
    if canceled() { Err(FileFinderSessionError::Canceled) } else { Ok(()) }
}
fn add(left: u64, right: u64) -> Result<u64, FileFinderSessionError> {
    left.checked_add(right).ok_or(FileFinderSessionError::IdentityExhausted)
}
fn reserve<T>(count: usize) -> Result<Vec<T>, FileFinderSessionError> {
    let mut values = Vec::new(); values.try_reserve_exact(count).map_err(|_| AppError::Admission)?;
    if values.capacity() > count { return Err(AppError::Admission.into()); }
    Ok(values)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    fn session(id: u64, count: usize) -> FileFinderSession {
        let root = std::env::temp_dir().join(format!("fcb-finder-admission-{}-{id}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&root).unwrap();
        for i in 0..count { fs::write(root.join(format!("file-{i:03}.rs")), b"").unwrap(); }
        FileFinderSession::open(ArenaOwnerId::new(id).unwrap(), &root, 1, b"file",
            FileFinderOptions { max_results: 2, ..Default::default() }, || false).unwrap()
    }
    fn occupy(session: &mut FileFinderSession, leave: u64) -> ResourceLease {
        let remaining = session.catalog.budget.accounting().ordinary_available().get();
        let allocation = session.catalog.next_id().unwrap();
        session.catalog.budget.try_reserve_managed(session.owner(), allocation, ByteLength::new(remaining - leave)).unwrap()
    }

    #[test]
    fn finder_rank_admission_failure_keeps_consumed_batch_pending_for_retry() {
        let mut session = session(73901, 300);
        session.advance(1, 1, 1, || false).unwrap();
        assert_eq!(session.revision, 2); assert_eq!(session.searched_files, 256);
        let denied = occupy(&mut session, 0);
        assert!(matches!(session.advance(1, 2, 1, || false),
            Err(FileFinderSessionError::Search(PathSearchError::ResourceDenied))));
        assert_eq!(session.revision, 2); assert_eq!(session.searched_files, 256);
        assert_eq!(session.catalog.catalogued_files, 300); assert!(session.pending_batch());
        assert_eq!(session.catalog.latest_page, 2); assert_eq!(session.processed_page, 1);
        drop(denied);
        session.advance(1, 1, 16, || false).unwrap(); // Replay original accepted predecessor only.
        assert!(session.pending_batch()); assert_eq!(session.searched_files, 256);
        session.advance(1, 2, 16, || false).unwrap();
        assert_eq!(session.searched_files, 300); assert_eq!(session.matches_seen, 300);
        assert_eq!(session.revision, 3); assert!(!session.pending_batch()); assert!(session.complete());
    }

    #[test]
    fn finder_output_admission_failure_replays_the_accepted_merge_without_scanning_again() {
        let mut session = session(73902, 300);
        // Enough for this small batch's real index/ranker, insufficient for the
        // separate4MiB output envelope. The accepted result must remain usable.
        let denied = occupy(&mut session, 1024 * 1024);
        assert!(matches!(session.advance(1, 1, 1, || false),
            Err(FileFinderSessionError::App(AppError::Output(OutputError::Allocation)))));
        assert_eq!(session.revision, 2); assert_eq!(session.searched_files, 256);
        assert_eq!(session.catalog.latest_page, 1); assert!(!session.pending_batch());
        let before: Vec<_> = session.hits.iter().map(|hit| hit.file).collect();
        drop(denied);
        session.advance(1, 1, 16, || false).unwrap();
        assert_eq!(session.revision, 2); assert_eq!(session.searched_files, 256);
        assert_eq!(session.catalog.latest_page, 1);
        assert_eq!(session.hits.iter().map(|hit| hit.file).collect::<Vec<_>>(), before);
        session.advance(1, 2, 16, || false).unwrap();
        assert_eq!(session.searched_files, 300); assert_eq!(session.matches_seen, 300);
        assert!(session.complete());
    }
}

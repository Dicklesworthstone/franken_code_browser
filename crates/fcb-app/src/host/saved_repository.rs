#![forbid(unsafe_code)]

//! Retained offline repositories over FCBS source archives and optional FCBD
//! posting pages. Reuses the production archive/index/query engines. Only one
//! source member is resident during verification; accepted results own metadata,
//! not all matching source files. Paths inside the archive are labels, not grants.
//! All operations (including final drop) belong on a host worker.

use std::{fs::File, mem::size_of, path::{Path, PathBuf}};
use fcb::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, SourceRevision};
use fcb::search::{RawPath, ResourceAllocationId, ResourceBudget, StreamingNeedle, StreamReadStep};
use fcb::search::paged_snapshot::{IndexedNeedle, PagedCapture, PagedHit, PagedQuery,
    PagedQueryOptions, PagedQueryState, PagedReport, PagedSearchError, PagedSnapshot,
    PagedSnapshotError, PagedMemberData};
use fcb::search::snapshot_index::SnapshotIndexError;
use fcb::search::snapshot_index::paged::{PagedPostings, PagedPostingError};
use fcb_core::ResourceLease;
pub use fcb::search::snapshot::{SnapshotLimits, Sha256Digest};
use crate::{AppError, input, MANAGED_BYTES, EXIT_OK, EXIT_PARTIAL};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};
use super::{HostResponse, MAX_HOST_TEXT_BYTES, reader::{ReaderSession, ReaderSessionError}};

pub const MAX_SAVED_HITS: usize = 4096;
pub const MAX_SAVED_PAGE: usize = 128;
pub const MAX_SAVED_NEEDLE_BYTES: usize = 1024;
const INDEX_CACHE_PAGES: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavedRepositoryError {
    App(AppError), Archive(PagedSnapshotError), Query(PagedSearchError),
    Index(PagedPostingError), Reader(ReaderSessionError),
    InvalidLimits, MissingQuery, StaleQuery, MissingHit, StaleIndex,
    IdentityExhausted, Canceled,
}
impl SavedRepositoryError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Archive(PagedSnapshotError::Canceled)
                | Self::Query(PagedSearchError::Canceled)
                | Self::Query(PagedSearchError::Archive(PagedSnapshotError::Canceled))
                | Self::Index(PagedPostingError::Index(SnapshotIndexError::Canceled))
                | Self::Reader(ReaderSessionError::Canceled) => true,
            Self::App(error) => error.is_canceled(),
            Self::Reader(ReaderSessionError::Host(super::HostError::App(error))
                | ReaderSessionError::App(error)) => error.is_canceled(),
            Self::Reader(ReaderSessionError::View(error)) => AppError::View(error).is_canceled(),
            _ => false,
        }
    }
}
impl std::fmt::Display for SavedRepositoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "{e}"), Self::Archive(e) => write!(f, "{e}"),
            Self::Query(e) => write!(f, "{e}"), Self::Index(e) => write!(f, "{e}"),
            Self::Reader(e) => write!(f, "{e}"),
            Self::InvalidLimits => f.write_str("SAVED_SESSION_INVALID_LIMITS"),
            Self::MissingQuery => f.write_str("SAVED_SESSION_NO_QUERY"),
            Self::StaleQuery => f.write_str("SAVED_SESSION_STALE_QUERY"),
            Self::MissingHit => f.write_str("SAVED_SESSION_NO_HIT"),
            Self::StaleIndex => f.write_str("SAVED_SESSION_STALE_INDEX"),
            Self::IdentityExhausted => f.write_str("SAVED_SESSION_IDENTITY_EXHAUSTED"),
            Self::Canceled => f.write_str("SAVED_SESSION_CANCELED"),
        }
    }
}
impl std::error::Error for SavedRepositoryError {}
impl From<AppError> for SavedRepositoryError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<PagedSnapshotError> for SavedRepositoryError { fn from(e: PagedSnapshotError) -> Self { Self::Archive(e) } }
impl From<PagedSearchError> for SavedRepositoryError { fn from(e: PagedSearchError) -> Self { Self::Query(e) } }
impl From<PagedPostingError> for SavedRepositoryError { fn from(e: PagedPostingError) -> Self { Self::Index(e) } }
impl From<ReaderSessionError> for SavedRepositoryError { fn from(e: ReaderSessionError) -> Self { Self::Reader(e) } }
impl From<OutputError> for SavedRepositoryError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

struct AcceptedIndex { generation: u64, pin: Sha256Digest, pages: PagedPostings<File> }
struct AcceptedQuery {
    generation: u64, needle: String, report: PagedReport,
    index_generation: Option<u64>, index_pin: Option<Sha256Digest>,
    member_bytes_read: u64, member_read_calls: u64, _label_lease: ResourceLease,
}

/// One explicitly opened archive, never rebound using its pathname. Index
/// attachment and query replacement prepare their response before acceptance.
/// Returned readers/strings have independent ownership. No implicit disk writes,
/// workspace discovery, native presentation, executor or external process.
pub struct SavedRepositorySession {
    archive: PagedSnapshot<File>,
    index: Option<AcceptedIndex>,
    query: Option<AcceptedQuery>,
    path: PathBuf,
    last_query_attempt: u64,
    last_index_attempt: u64,
    next_allocation: u64,
    budget: ResourceBudget,
    _lease: ResourceLease,
}
impl SavedRepositorySession {
    /// Validate the complete bounded archive once while retaining only its
    /// directory. Member bytes are independently verified again on every load.
    /// This is not the trusted-catalog cold-open route or a cheap UI callback.
    pub fn open(owner: ArenaOwnerId, path: &Path, limits: SnapshotLimits,
        mut canceled: impl FnMut() -> bool) -> Result<Self, SavedRepositoryError> {
        check(&mut canceled)?;
        if path.as_os_str().is_empty() || path.as_os_str().len() > 16_384
            || limits.max_file_bytes > MAX_HOST_TEXT_BYTES {
            return Err(SavedRepositoryError::InvalidLimits);
        }
        let budget = ResourceBudget::new(owner, ByteLength::new(MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
        let lease = budget.try_reserve_managed(owner, allocation(1)?,
            ByteLength::new((128 * 1024 + size_of::<Self>()) as u64)).map_err(|_| AppError::Admission)?;
        let path = input::absolute(path)?;
        let (file, metadata) = input::open_regular(&path)?;
        if metadata.len() > limits.max_document_bytes as u64 { return Err(SavedRepositoryError::InvalidLimits); }
        let archive = PagedSnapshot::open(file, owner, limits, &budget, allocation(2)?, &mut canceled)?;
        check(&mut canceled)?;
        Ok(Self { archive, index: None, query: None, path, last_query_attempt: 0,
            last_index_attempt: 0, next_allocation: 3, budget, _lease: lease })
    }
    pub fn owner(&self) -> ArenaOwnerId { self.archive.directory().owner() }
    pub fn archive_digest(&self) -> Sha256Digest { self.archive.directory().digest() }
    pub fn accepted_generation(&self) -> Option<u64> { self.query.as_ref().map(|q| q.generation) }
    pub fn index_generation(&self) -> Option<u64> { self.index.as_ref().map(|i| i.generation) }

    pub fn info(&mut self, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedRepositoryError> {
        check(&mut canceled)?;
        let mut out = self.output("info")?;
        out.literal(",\"archive_path\":")?; out.path(&self.path)?;
        out.literal(",\"accepted_query_generation\":")?; optional(&mut out, self.accepted_generation())?;
        out.literal(",\"accepted_index_generation\":")?; optional(&mut out, self.index_generation())?;
        let directory = self.archive.directory();
        out.literal(",\"initial_archive_bytes_read\":")?; out.integer(directory.validation_stats().bytes_read)?;
        out.literal(",\"directory_reserved_bytes\":")?; out.integer(directory.retained_charge() as u64)?;
        out.literal(",\"member_bytes_read\":")?; out.integer(self.archive.load_stats().bytes_read)?;
        out.literal(",\"member_read_calls\":")?; out.integer(self.archive.load_stats().read_calls)?;
        if let Some(index) = &self.index { index_fields(&mut out, index)?; }
        out.literal("}\n")?;
        self.finish(out, false, &mut canceled)
    }

    /// Metadata-only member pagination. IDs are this session's ordinal+1, never
    /// trusted persistent FileIds; raw archived names remain labels only.
    pub fn members(&mut self, start: usize, limit: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedRepositoryError> {
        check(&mut canceled)?; page_limits(start, limit, self.archive.directory().len())?;
        let mut out = self.output("members")?;
        out.literal(",\"members\":[")?;
        let end = start.saturating_add(limit).min(self.archive.directory().len());
        for ordinal in start..end {
            check(&mut canceled)?;
            if ordinal != start { out.literal(",")?; }
            let member = self.archive.directory().member(ordinal).ok_or(SavedRepositoryError::MissingHit)?;
            out.literal("{\"member\":")?; out.integer(ordinal as u64)?;
            out.literal(",\"file_id\":")?; out.integer(ordinal as u64 + 1)?;
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
            out.literal("}")?;
        }
        out.literal("],\"next_offset\":")?;
        optional(&mut out, (end < self.archive.directory().len()).then_some(end as u64))?;
        out.literal("}\n")?;
        self.finish(out, false, &mut canceled)
    }

    /// The pin must come from the original trusted build receipt, NOT this
    /// input file. Validates archive binding and FCBD metadata; posting pages
    /// are verified on demand in a retained four-page cache. No source loads.
    pub fn attach_index(&mut self, generation: u64, path: &Path, trusted_pin: Sha256Digest,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedRepositoryError> {
        index_attempt(&mut self.last_index_attempt, generation)?; check(&mut canceled)?;
        if path.as_os_str().is_empty() || path.as_os_str().len() > 16_384 { return Err(SavedRepositoryError::InvalidLimits); }
        let [id] = self.allocations()?;
        let path = input::absolute(path)?;
        let (file, _) = input::open_regular(&path)?;
        let pages = PagedPostings::open_pinned(file, trusted_pin, self.archive.directory(),
            INDEX_CACHE_PAGES, &self.budget, id, &mut canceled)?;
        let candidate = AcceptedIndex { generation, pin: trusted_pin, pages };
        let mut out = self.output("index-attach")?;
        index_fields(&mut out, &candidate)?; out.literal("}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        self.index = Some(candidate);
        Ok(response)
    }
    /// Revert future searches to the existing direct scan; accepted result
    /// identity is unchanged and remains available under its query generation.
    pub fn detach_index(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedRepositoryError> {
        index_attempt(&mut self.last_index_attempt, generation)?; check(&mut canceled)?;
        let mut out = self.output("index-detach")?;
        out.literal(",\"accepted_index_generation\":null}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        self.index = None; Ok(response)
    }

    /// Search the fixed archive using the existing paged-source verifier.
    /// Only hit metadata survives; source residency does not grow with matching
    /// files. This API runs to completion on the caller's worker, with engine
    /// cancellation checkpoints, not continuation across native calls.
    pub fn search(&mut self, generation: u64, needle: &str, max_matches: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedRepositoryError> {
        query_attempt(&mut self.last_query_attempt, generation)?; check(&mut canceled)?;
        if needle.is_empty() || needle.len() > MAX_SAVED_NEEDLE_BYTES
            || !(1..=MAX_SAVED_HITS).contains(&max_matches) { return Err(SavedRepositoryError::InvalidLimits); }
        let [label_id, pattern_id, result_id, member_id, scan_id] = self.allocations()?;
        let lease = self.budget.try_reserve_managed(self.owner(), label_id,
            ByteLength::new((needle.len() + size_of::<AcceptedQuery>()) as u64)).map_err(|_| AppError::Admission)?;
        let label = copy_text(needle)?;
        let owner = self.owner();
        let qgen = QueryGeneration::new(owner, generation).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        let opts = PagedQueryOptions { generation: qgen,
            first_file: FileId::new(owner, 1).map_err(|_| SavedRepositoryError::IdentityExhausted)?,
            first_revision: SourceRevision::new(owner, 1).map_err(|_| SavedRepositoryError::IdentityExhausted)?, max_matches };
        let before = self.archive.load_stats();
        let ids = [result_id, member_id, scan_id];
        let report = if let Some(index) = self.index.as_mut() {
            let pattern = IndexedNeedle::text(owner, needle, &self.budget, pattern_id)
                .map_err(PagedSearchError::from)?;
            let query = PagedQuery::new_paged(&mut self.archive, &pattern, &mut index.pages, opts, &self.budget, ids)?;
            finish_query(query, qgen, &self.budget, &mut canceled)?
        } else {
            let pattern = StreamingNeedle::text(owner, needle, &self.budget, pattern_id)
                .map_err(PagedSearchError::from)?;
            let query = PagedQuery::new(&mut self.archive, &pattern, opts, &self.budget, ids)?;
            finish_query(query, qgen, &self.budget, &mut canceled)?
        };
        check(&mut canceled)?;
        report.validate_delivery(self.archive_digest(), qgen)?;
        let after = self.archive.load_stats();
        let candidate = AcceptedQuery { generation, needle: label, report,
            index_generation: self.index_generation(), index_pin: self.index.as_ref().map(|i| i.pin),
            member_bytes_read: after.bytes_read - before.bytes_read,
            member_read_calls: after.read_calls - before.read_calls, _label_lease: lease };
        let mut out = self.output("search")?;
        self.encode_page(&mut out, &candidate, 0, 64, &mut canceled)?;
        let response = self.finish(out, !candidate.report.is_complete(), &mut canceled)?;
        self.query = Some(candidate);
        Ok(response)
    }
    pub fn results(&mut self, generation: u64, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedRepositoryError> {
        check(&mut canceled)?;
        page_limits(start, limit, self.accepted(generation)?.report.hits().len())?;
        let mut out = self.output("results")?;
        let query = self.accepted(generation)?;
        self.encode_page(&mut out, query, start, limit, &mut canceled)?;
        let partial = !query.report.is_complete();
        self.finish(out, partial, &mut canceled)
    }
    pub fn clear_results(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedRepositoryError> {
        query_attempt(&mut self.last_query_attempt, generation)?; check(&mut canceled)?;
        let mut out = self.output("clear-results")?;
        out.literal(",\"accepted_query_generation\":null,\"retained_hits\":\"0\"}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        self.query = None; Ok(response)
    }

    pub fn open_hit_reader(&mut self, reader_owner: ArenaOwnerId, generation: u64, hit_id: u64,
        mut canceled: impl FnMut() -> bool) -> Result<(ReaderSession, HostResponse), SavedRepositoryError> {
        check(&mut canceled)?;
        let position = hit_id.checked_sub(1).and_then(|v| usize::try_from(v).ok()).ok_or(SavedRepositoryError::MissingHit)?;
        let hit = *self.accepted(generation)?.report.hits().get(position).ok_or(SavedRepositoryError::MissingHit)?;
        self.open_reader(reader_owner, hit.ordinal(), Some((hit_id, hit)), &mut canceled)
    }
    pub fn open_member_reader(&mut self, reader_owner: ArenaOwnerId, ordinal: usize,
        mut canceled: impl FnMut() -> bool) -> Result<(ReaderSession, HostResponse), SavedRepositoryError> {
        self.open_reader(reader_owner, ordinal, None, &mut canceled)
    }
    fn open_reader(&mut self, reader_owner: ArenaOwnerId, ordinal: usize, selected: Option<(u64, PagedHit)>,
        canceled: &mut impl FnMut() -> bool) -> Result<(ReaderSession, HostResponse), SavedRepositoryError> {
        check(canceled)?;
        if reader_owner == self.owner() { return Err(PagedSearchError::OwnerMismatch.into()); }
        let ids = self.allocations()?;
        let capture = if let Some((_, hit)) = selected {
            PagedCapture::open_hit(&mut self.archive, hit, hit.generation(), &self.budget, ids, &mut *canceled)?
        } else {
            let value = (ordinal as u64).checked_add(1).ok_or(SavedRepositoryError::IdentityExhausted)?;
            let file = FileId::new(self.owner(), value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
            let revision = SourceRevision::new(self.owner(), value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
            PagedCapture::load(&mut self.archive, ordinal, file, revision, &self.budget, ids, &mut *canceled)?
        };
        let member = self.archive.directory().member(ordinal).ok_or(SavedRepositoryError::MissingHit)?;
        let label = RawPath::from_bytes(member.path).to_path_buf();
        let mut reader = ReaderSession::from_bytes(reader_owner, &label, capture.bytes(), &mut *canceled)?;
        let info = reader.info(&mut *canceled)?;
        let (start, bytes) = if let Some((_, hit)) = selected {
            let range = hit.original_range();
            let start = range.start().get().saturating_sub(132);
            let end = range.end().get().saturating_add(132).min(capture.bytes().len() as u64);
            (start, ((end - start) as usize).max(4))
        } else { (0, 16 * 1024) };
        let window = reader.read_window(start, bytes, &mut *canceled)?;
        let mut out = self.output("open-reader")?;
        out.literal(",\"member\":")?; out.integer(ordinal as u64)?;
        out.literal(",\"file_id\":")?; out.integer(capture.file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(capture.revision().get())?;
        out.literal(",\"source_digest\":")?; out.quoted(&capture.source_digest().to_hex())?;
        out.literal(",\"source_observation\":\"verified-saved-member\",\"live_source_reopened\":false,\"reader_owner\":")?;
        out.integer(reader_owner.get())?;
        out.literal(",\"selection\":")?;
        if let Some((id, hit)) = selected {
            out.literal("{\"query_generation\":")?; out.integer(hit.generation().get())?;
            out.literal(",\"hit_id\":")?; out.integer(id)?;
            out.literal(",\"domain\":\"original-source-bytes\",\"original_range\":")?; out.range(hit.original_range())?;
            out.literal(",\"original_hex\":")?; out.hex(capture.hit_bytes(hit)?)?; out.literal("}")?;
        } else { out.literal("null")?; }
        out.literal(",\"reader\":")?; out.literal(info.as_str())?;
        out.literal(",\"window\":")?; out.literal(window.as_str())?; out.literal("}\n")?;
        let response = self.finish(out, false, canceled)?;
        Ok((reader, response))
    }
    fn accepted(&self, generation: u64) -> Result<&AcceptedQuery, SavedRepositoryError> {
        let query = self.query.as_ref().ok_or(SavedRepositoryError::MissingQuery)?;
        if query.generation != generation { return Err(SavedRepositoryError::StaleQuery); }
        let qgen = QueryGeneration::new(self.owner(), generation).map_err(|_| SavedRepositoryError::StaleQuery)?;
        query.report.validate_delivery(self.archive_digest(), qgen)?;
        Ok(query)
    }
    fn encode_page(&self, out: &mut Output, query: &AcceptedQuery, start: usize, limit: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<(), SavedRepositoryError> {
        let report = &query.report; let stats = report.stats();
        out.literal(",\"query_generation\":")?; out.integer(query.generation)?;
        out.literal(",\"needle\":")?; out.quoted(&query.needle)?;
        out.literal(",\"index_generation\":")?; optional(out, query.index_generation)?;
        out.literal(",\"index_pin\":")?;
        if let Some(pin) = query.index_pin { out.quoted(&pin.to_hex())?; } else { out.literal("null")?; }
        out.literal(",\"mode\":\"exact-decoded-literal\",\"search_complete\":")?; out.boolean(report.is_complete())?;
        out.literal(",\"truncated\":")?; out.boolean(report.truncated())?;
        for (key, value) in [("retained_hits", report.hits().len() as u64), ("matches_seen", report.matches_seen()),
            ("unavailable_files", report.unavailable_files() as u64), ("unsupported_files", stats.unsupported_files as u64),
            ("member_bytes_read", query.member_bytes_read), ("member_read_calls", query.member_read_calls),
            ("scanned_bytes", stats.scanned_bytes), ("peak_source_bytes", stats.peak_source_bytes as u64),
            ("index_eliminated_files", stats.index_eliminated_files as u64), ("fallback_files", stats.index_fallback_files as u64),
            ("posting_entries_visited", stats.posting_entries_visited as u64)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"retained_source_payload_bytes\":\"0\",\"hits\":[")?;
        let end = start.saturating_add(limit).min(report.hits().len());
        for (position, &hit) in report.hits()[start..end].iter().enumerate() {
            check(canceled)?;
            if position > 0 { out.literal(",")?; }
            let member = self.archive.directory().member(hit.ordinal()).ok_or(SavedRepositoryError::MissingHit)?;
            out.literal("{\"hit_id\":")?; out.integer((start + position) as u64 + 1)?;
            out.literal(",\"member\":")?; out.integer(hit.ordinal() as u64)?;
            out.literal(",\"file_id\":")?; out.integer(hit.file().get())?;
            out.literal(",\"source_revision\":")?; out.integer(hit.revision().get())?;
            out.literal(",\"source_digest\":")?; out.quoted(&hit.source_digest().to_hex())?;
            out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(member.path).to_path_buf())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?; out.literal("}")?;
        }
        out.literal("],\"next_offset\":")?;
        optional(out, (end < report.hits().len()).then_some(end as u64))?; out.literal("}\n")?;
        Ok(())
    }
    fn allocations<const N: usize>(&mut self) -> Result<[ResourceAllocationId; N], SavedRepositoryError> {
        let next = self.next_allocation.checked_add(N as u64).ok_or(SavedRepositoryError::IdentityExhausted)?;
        let start = self.next_allocation; self.next_allocation = next;
        Ok(std::array::from_fn(|n| ResourceAllocationId::new(start + n as u64).expect("checked nonzero allocation")))
    }
    fn output(&mut self, command: &str) -> Result<Output, SavedRepositoryError> {
        let [id] = self.allocations()?;
        let mut out = Output::new(self.owner(), MAX_RESPONSE_BYTES, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.saved-repository/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.owner().get())?;
        out.literal(",\"archive_digest\":")?; out.quoted(&self.archive_digest().to_hex())?;
        out.literal(",\"members_total\":")?; out.integer(self.archive.directory().len() as u64)?;
        out.literal(",\"captured_members\":")?; out.integer(self.archive.directory().captured_files() as u64)?;
        out.literal(",\"discovery_complete\":")?; out.boolean(self.archive.directory().discovery_complete())?;
        out.literal(",\"live_root_authority\":false,\"native_presented\":false")?;
        Ok(out)
    }
    fn finish(&mut self, out: Output, partial: bool, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, SavedRepositoryError> {
        check(canceled)?;
        let [id] = self.allocations()?;
        let charge = out.as_bytes().len().checked_mul(3).and_then(|n| n.checked_add(4096)).ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.owner(), id, ByteLength::new(charge as u64)).map_err(|_| AppError::Admission)?;
        let text = copy_text(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?)?;
        check(canceled)?;
        Ok(HostResponse { text, exit_code: if partial { EXIT_PARTIAL } else { EXIT_OK }, _lease: lease })
    }
}
fn finish_query(mut query: PagedQuery<'_, '_, File>, generation: QueryGeneration,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<PagedReport, SavedRepositoryError> {
    while query.state() == PagedQueryState::Pending {
        query.step(StreamReadStep::default(), generation, budget, &mut *canceled)?;
    }
    Ok(query.finish()?)
}
fn index_fields(out: &mut Output, index: &AcceptedIndex) -> Result<(), SavedRepositoryError> {
    let io = index.pages.io_stats();
    out.literal(",\"index_generation\":")?; out.integer(index.generation)?;
    out.literal(",\"index_pin\":")?; out.quoted(&index.pin.to_hex())?;
    out.literal(",\"index_kind\":\"demand-paged-postings-v1\",\"index_pin_scope\":\"page-manifest-envelope-sha256\",\"index_body_verified_on_open\":false")?;
    for (key, value) in [("index_page_bytes_read", io.page_bytes_read), ("index_page_loads", io.page_loads),
        ("index_cache_hits", io.cache_hits), ("index_cache_capacity_bytes", index.pages.cache_capacity_bytes() as u64)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    Ok(())
}
fn query_attempt(last: &mut u64, next: u64) -> Result<(), SavedRepositoryError> {
    if next == 0 || next <= *last { return Err(SavedRepositoryError::StaleQuery); }
    *last = next; Ok(())
}
fn index_attempt(last: &mut u64, next: u64) -> Result<(), SavedRepositoryError> {
    if next == 0 || next <= *last { return Err(SavedRepositoryError::StaleIndex); }
    *last = next; Ok(())
}
fn allocation(id: u64) -> Result<ResourceAllocationId, SavedRepositoryError> {
    ResourceAllocationId::new(id).map_err(|_| SavedRepositoryError::IdentityExhausted)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), SavedRepositoryError> {
    if canceled() { Err(SavedRepositoryError::Canceled) } else { Ok(()) }
}
fn page_limits(start: usize, limit: usize, count: usize) -> Result<(), SavedRepositoryError> {
    if !(1..=MAX_SAVED_PAGE).contains(&limit) || start > count { Err(SavedRepositoryError::InvalidLimits) } else { Ok(()) }
}
fn optional(out: &mut Output, value: Option<u64>) -> Result<(), OutputError> {
    if let Some(value) = value { out.integer(value) } else { out.literal("null") }
}
fn copy_text(text: &str) -> Result<String, SavedRepositoryError> {
    let mut copy = String::new(); copy.try_reserve_exact(text.len()).map_err(|_| AppError::Admission)?;
    if copy.capacity() > text.len() { return Err(AppError::Admission.into()); }
    copy.push_str(text); Ok(copy)
}

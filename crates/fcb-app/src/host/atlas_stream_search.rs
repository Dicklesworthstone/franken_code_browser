#![forbid(unsafe_code)]

//! Whole-file native workspace search over the atlas's existing frozen catalog.
//! The public stream engine owns decoding, matching and I/O limits. This host
//! retains only compact receipts and exact literal witnesses. Page/hit delivery
//! never reopens source or presents a witness as a complete source capture.

use std::{mem::size_of, path::Path, sync::Arc};
use fcb::{ByteLength, QueryGeneration, SourceRevision};
use fcb::map::LayoutRevision;
use fcb::map::workspace::retained::RetainedWorkspaceAtlas;
use fcb::map::workspace::stream_search::{AtlasStreamError, AtlasStreamFile,
    AtlasStreamFileState, AtlasStreamHit, AtlasStreamLimits, AtlasStreamReport,
    AtlasStreamSearch, AtlasStreamState, AtlasStreamStats, AtlasStreamStop};
use fcb::search::{DetectedEncoding, ExtentConsistency, ResourceAllocationId,
    ResourceBudget, SearchManifestId, StreamReadState, StreamReadStep, StreamingNeedle};
use fcb::search::workspace::RootGrant;
use fcb::source::SourceError;
use fcb::store::{Sha256, Sha256Digest};
use fcb_core::ResourceLease;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL, MANAGED_BYTES, input, workspace};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};
use super::{HostResponse, atlas_session::{AtlasSession, AtlasSessionError}};
use super::atlas_search::{AtlasSearchError, AtlasSearchScope, RegionCoverage};

pub const MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES: usize = 1024;
pub const MAX_ATLAS_STREAM_SEARCH_HITS: usize = 256;
pub const MAX_ATLAS_STREAM_SEARCH_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_ATLAS_STREAM_SEARCH_READ_CALLS: u64 = 1_048_576;
/// Rows carry lossless native paths. A bounded page, not the hit limit, bounds
/// worst-case escaped path output under the ordinary response-byte allowance.
pub const MAX_ATLAS_STREAM_SEARCH_PAGE: usize = 32;
const MAX_DIAGNOSTICS: usize = 32;
/// Progress is bounded by scanner work, with at most 32 additional publications
/// as the first page grows. No callback retains a source decoder or full file.
const PROGRESS_STEP_INTERVAL: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasStreamSearchOptions {
    pub max_matches: usize,
    pub max_scan_bytes: u64,
    pub max_read_calls: u64,
}
impl Default for AtlasStreamSearchOptions {
    fn default() -> Self {
        Self { max_matches: MAX_ATLAS_STREAM_SEARCH_HITS, max_scan_bytes: 256 * 1024 * 1024,
            max_read_calls: MAX_ATLAS_STREAM_SEARCH_READ_CALLS }
    }
}
impl AtlasStreamSearchOptions {
    fn validate(self) -> Result<(), AtlasStreamSearchError> {
        if !(1..=MAX_ATLAS_STREAM_SEARCH_HITS).contains(&self.max_matches)
            || !(1..=MAX_ATLAS_STREAM_SEARCH_SCAN_BYTES).contains(&self.max_scan_bytes)
            || !(1..=MAX_ATLAS_STREAM_SEARCH_READ_CALLS).contains(&self.max_read_calls) {
            return Err(AtlasStreamSearchError::InvalidLimits);
        }
        Ok(())
    }
    fn limits(self) -> AtlasStreamLimits {
        AtlasStreamLimits { max_matches: self.max_matches, max_bytes: self.max_scan_bytes,
            max_read_calls: self.max_read_calls }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasStreamSearchError {
    App(AppError), Atlas(AtlasSessionError), Stream(AtlasStreamError), Scope(AtlasSearchError),
    InvalidLimits, WrongAtlas, StaleQuery, MissingQuery, MissingHit, Canceled, IdentityExhausted,
}
impl std::fmt::Display for AtlasStreamSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "{e}"), Self::Atlas(e) => write!(f, "{e}"),
            Self::Stream(e) => write!(f, "{e}"),
            Self::Scope(e) => write!(f, "{e}"),
            other => f.write_str(match other {
                Self::InvalidLimits => "ATLAS_STREAM_SEARCH_INVALID_LIMITS",
                Self::WrongAtlas => "ATLAS_STREAM_SEARCH_WRONG_ATLAS",
                Self::StaleQuery => "ATLAS_STREAM_SEARCH_STALE_QUERY",
                Self::MissingQuery => "ATLAS_STREAM_SEARCH_NO_QUERY",
                Self::MissingHit => "ATLAS_STREAM_SEARCH_NO_HIT",
                Self::Canceled => "ATLAS_STREAM_SEARCH_CANCELED",
                Self::IdentityExhausted => "ATLAS_STREAM_SEARCH_IDENTITY_EXHAUSTED",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for AtlasStreamSearchError {}
impl From<AppError> for AtlasStreamSearchError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<AtlasSessionError> for AtlasStreamSearchError { fn from(e: AtlasSessionError) -> Self { Self::Atlas(e) } }
impl From<AtlasStreamError> for AtlasStreamSearchError { fn from(e: AtlasStreamError) -> Self { Self::Stream(e) } }
impl From<AtlasSearchError> for AtlasStreamSearchError {
    fn from(e: AtlasSearchError) -> Self { if e == AtlasSearchError::Canceled { Self::Canceled } else { Self::Scope(e) } }
}
impl From<OutputError> for AtlasStreamSearchError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

#[derive(Clone, Copy)]
struct Hit {
    hit: AtlasStreamHit,
    file: AtlasStreamFile,
    witness_start: usize,
    witness_len: usize,
    text_start: usize,
    text_len: usize,
    digest: Sha256Digest,
}
/// Reuse captured-search membership semantics and wire identity while keeping
/// streaming observations independent of its capture limits and index state.
struct FrozenScope {
    predicate: AtlasSearchScope,
    region: Option<RegionCoverage>,
    eligible: usize,
    excluded: usize,
}
impl FrozenScope {
    fn admit(predicate: AtlasSearchScope, atlas: &AtlasSession, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, AtlasStreamSearchError> {
        let (eligible, region) = predicate.admission_counts(atlas, canceled)?;
        let excluded = atlas.atlas().file_count().checked_sub(eligible).ok_or(AtlasStreamSearchError::WrongAtlas)?;
        Ok(Self { predicate, region, eligible, excluded })
    }
    fn projection(&self, atlas: &AtlasSession, budget: &ResourceBudget, id: ResourceAllocationId,
        canceled: &mut impl FnMut() -> bool) -> Result<(Option<Vec<usize>>, Option<ResourceLease>), AtlasStreamSearchError> {
        if self.predicate.is_all() { return Ok((None, None)); }
        let bytes = self.eligible.checked_mul(size_of::<usize>())
            .and_then(|n| n.checked_add(size_of::<Vec<usize>>())).ok_or(AppError::Admission)?;
        let lease = budget.try_reserve_managed(atlas.atlas().catalog().id().owner(), id, ByteLength::new(bytes as u64))
            .map_err(|_| AppError::Admission)?;
        let mut ordinals = reserve(self.eligible)?;
        for (ordinal, entry) in atlas.atlas().catalog().entries().iter().enumerate() {
            check(canceled)?;
            if self.predicate.includes_path(entry.path().raw().as_bytes()) {
                if ordinals.len() == self.eligible { return Err(AtlasStreamSearchError::WrongAtlas); }
                ordinals.push(ordinal);
            }
        }
        if ordinals.len() != self.eligible { return Err(AtlasStreamSearchError::WrongAtlas); }
        // Original order preserves file/node IDs and directory count ordering.
        // This temporary capacity overlaps the engine's separately leased copy.
        Ok((Some(ordinals), Some(lease)))
    }
    fn encode(&self, out: &mut Output) -> Result<(), AtlasStreamSearchError> {
        self.predicate.encode(out)?;
        out.literal(",\"mode\":")?;
        out.quoted(if self.predicate.case_sensitive() { "exact-decoded-literal" } else { "unicode-lowercase-literal" })?;
        if let Some(region) = self.region { region.encode(out, "")?; }
        out.literal(",\"eligible_files\":")?; out.integer(self.eligible as u64)?;
        out.literal(",\"excluded_files\":")?; out.integer(self.excluded as u64)?;
        Ok(())
    }
    fn region_complete(&self) -> bool { self.region.is_none_or(|region| region.missing == 0) }
}
struct Snapshot {
    generation: u64,
    needle: String,
    options: AtlasStreamSearchOptions,
    scope: FrozenScope,
    hits: Vec<Hit>,
    witnesses: Vec<u8>,
    matched_text: String,
    diagnostics: Vec<AtlasStreamFile>,
    examined: usize,
    unexamined: usize,
    unavailable: usize,
    unsupported: usize,
    failed: usize,
    matching_files: usize,
    retained_matching_files: usize,
    count_all_matches: bool,
    stats: AtlasStreamStats,
    discovery_complete: bool,
    complete: bool,
    stop: Option<AtlasStreamStop>,
    _lease: ResourceLease,
}

/// One accepted immutable stream result over one atlas authority. A replacement
/// is completed on the caller's worker before publication. No borrowed active
/// engine survives a call, and the host creates no thread/runtime. A canceled
/// or failed replacement preserves the previous result while consuming its
/// attempted generation and source-observation span.
pub struct RetainedAtlasStreamSearch {
    manifest: SearchManifestId,
    grant: RootGrant,
    layout: LayoutRevision,
    last_attempt: u64,
    next_revision: u64,
    next_allocation: u64,
    source_bytes_read: u64,
    read_calls: u64,
    accepted: Option<Arc<Snapshot>>,
    budget: ResourceBudget,
    _lease: ResourceLease,
}
impl RetainedAtlasStreamSearch {
    pub fn new(atlas: &AtlasSession) -> Result<Self, AtlasStreamSearchError> {
        atlas.validate_active()?;
        let manifest = atlas.atlas().catalog().id();
        let grant = atlas.atlas().catalog().grant();
        let budget = ResourceBudget::new(manifest.owner(), ByteLength::new(MANAGED_BYTES))
            .map_err(|_| AppError::Admission)?;
        // Includes bounded root/path/display scratch, before native pathname
        // allocation. No source-sized reservation depends on file length.
        let lease = budget.try_reserve_managed(manifest.owner(), allocation(1)?,
            ByteLength::new((256 * 1024 + 4 * grant.root_path().len() + size_of::<Self>()) as u64))
            .map_err(|_| AppError::Admission)?;
        Ok(Self { manifest, grant: grant.clone(), layout: atlas.atlas().layout().revision(),
            last_attempt: 0, next_revision: 1, next_allocation: 2, source_bytes_read: 0, read_calls: 0,
            accepted: None, budget, _lease: lease })
    }
    pub fn accepted_generation(&self) -> Option<u64> { self.accepted.as_ref().map(|q| q.generation) }
    pub fn last_attempted_generation(&self) -> u64 { self.last_attempt }
    pub fn source_bytes_read(&self) -> u64 { self.source_bytes_read }
    pub fn read_calls(&self) -> u64 { self.read_calls }
    pub fn retained_witness_bytes(&self) -> usize { self.accepted.as_ref().map_or(0, |q| q.witnesses.len()) }

    /// Exact, case-sensitive decoded text across every catalogued file, even
    /// while the map displays an extension scope. There is no per-file byte
    /// ceiling. Native payload bytes/calls and retained occurrences have
    /// separate positive global limits.
    pub fn search(&mut self, atlas: &AtlasSession, needle: &str, options: AtlasStreamSearchOptions,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasStreamSearchError> {
        self.search_scoped(atlas, needle, options, AtlasSearchScope::all(), canceled)
    }

    /// Freeze the file-type/location intersection before any source I/O. Only
    /// eligible catalog files consume scan limits. Empty is explicit, and
    /// requested visible paths absent from the catalog keep coverage partial.
    pub fn search_scoped(&mut self, atlas: &AtlasSession, needle: &str, options: AtlasStreamSearchOptions,
        scope: AtlasSearchScope, canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasStreamSearchError> {
        self.search_inner(atlas, needle, options, scope, canceled, || false, |_| {}, false, false)
    }

    /// Count all exact occurrences within the I/O limits while keeping the
    /// existing bounded result rows and literal witnesses.
    pub fn search_counted(&mut self, atlas: &AtlasSession, needle: &str, options: AtlasStreamSearchOptions,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasStreamSearchError> {
        self.search_scoped_counted(atlas, needle, options, AtlasSearchScope::all(), canceled)
    }

    pub fn search_scoped_counted(&mut self, atlas: &AtlasSession, needle: &str, options: AtlasStreamSearchOptions,
        scope: AtlasSearchScope, canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasStreamSearchError> {
        self.search_inner(atlas, needle, options, scope, canceled, || false, |_| {}, false, true)
    }

    /// Publish a bounded first-page preview while the SAME scanner continues.
    /// Callbacks run synchronously on the caller's worker; the response is
    /// borrowed only for that callback and must not reenter this service.
    /// Progress cannot authorize page/hit delivery. Only the returned terminal
    /// response installs the new inventory. A stop request accepts finalized
    /// files as an explicit partial result; cancellation preserves prior history.
    pub fn search_with_progress(&mut self, atlas: &AtlasSession, needle: &str,
        options: AtlasStreamSearchOptions, canceled: impl FnMut() -> bool,
        stopped: impl FnMut() -> bool, progress: impl FnMut(&HostResponse))
        -> Result<HostResponse, AtlasStreamSearchError> {
        self.search_scoped_with_progress(atlas, needle, options, AtlasSearchScope::all(), canceled, stopped, progress)
    }

    /// Progress, Stop-and-keep and terminal delivery retain the same scope and
    /// eligible universe; later map/filter edits cannot change the operation.
    pub fn search_scoped_with_progress(&mut self, atlas: &AtlasSession, needle: &str,
        options: AtlasStreamSearchOptions, scope: AtlasSearchScope, canceled: impl FnMut() -> bool,
        stopped: impl FnMut() -> bool, progress: impl FnMut(&HostResponse))
        -> Result<HostResponse, AtlasStreamSearchError> {
        self.search_inner(atlas, needle, options, scope, canceled, stopped, progress, true, false)
    }

    pub fn search_counted_with_progress(&mut self, atlas: &AtlasSession, needle: &str,
        options: AtlasStreamSearchOptions, canceled: impl FnMut() -> bool,
        stopped: impl FnMut() -> bool, progress: impl FnMut(&HostResponse))
        -> Result<HostResponse, AtlasStreamSearchError> {
        self.search_scoped_counted_with_progress(atlas, needle, options, AtlasSearchScope::all(), canceled, stopped, progress)
    }

    /// Counting and scope are immutable throughout previews, Stop-and-keep and
    /// terminal delivery. Only finalized file observations contribute counts.
    pub fn search_scoped_counted_with_progress(&mut self, atlas: &AtlasSession, needle: &str,
        options: AtlasStreamSearchOptions, scope: AtlasSearchScope, canceled: impl FnMut() -> bool,
        stopped: impl FnMut() -> bool, progress: impl FnMut(&HostResponse))
        -> Result<HostResponse, AtlasStreamSearchError> {
        self.search_inner(atlas, needle, options, scope, canceled, stopped, progress, true, true)
    }

    fn search_inner(&mut self, atlas: &AtlasSession, needle: &str, options: AtlasStreamSearchOptions,
        scope: AtlasSearchScope, mut canceled: impl FnMut() -> bool, mut stopped: impl FnMut() -> bool,
        mut progress: impl FnMut(&HostResponse), publish_progress: bool, count_all_matches: bool)
        -> Result<HostResponse, AtlasStreamSearchError> {
        self.validate(atlas)?;
        let generation = self.next_generation()?;
        let first = self.next_revision;
        self.next_revision = first.checked_add(atlas.atlas().file_count().max(1) as u64)
            .ok_or(AtlasStreamSearchError::IdentityExhausted)?;
        let first_revision = SourceRevision::new(self.manifest.owner(), first)
            .map_err(|_| AtlasStreamSearchError::IdentityExhausted)?;
        let query = QueryGeneration::new(self.manifest.owner(), generation)
            .map_err(|_| AtlasStreamSearchError::IdentityExhausted)?;
        check(&mut canceled)?;
        options.validate()?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES {
            return Err(AtlasStreamSearchError::InvalidLimits);
        }
        let scope = FrozenScope::admit(scope, atlas, &mut canceled)?;
        let projection_id = self.next_id()?;
        let (ordinals, projection_lease) = scope.projection(atlas, &self.budget, projection_id, &mut canceled)?;
        self.source_bytes_read.checked_add(options.max_scan_bytes).ok_or(AtlasStreamSearchError::IdentityExhausted)?;
        self.read_calls.checked_add(options.max_read_calls).ok_or(AtlasStreamSearchError::IdentityExhausted)?;
        let pattern_id = self.next_id()?;
        let report_id = self.next_id()?;
        let worker_id = self.next_id()?;
        let result_id = self.next_id()?;
        // Reuse these allocation identities only after the previous callback's
        // admitted output and response have both been dropped.
        let progress_ids = if publish_progress { Some([self.next_id()?, self.next_id()?]) } else { None };
        let pattern = StreamingNeedle::text_matching(self.manifest.owner(), needle,
            scope.predicate.case_sensitive(), &self.budget, pattern_id)
            .map_err(AppError::from)?;
        let root = self.grant.root_path().to_path_buf();
        let mut tally = ProgressTally::default();
        let (scanned, io) = scan(atlas.atlas(), &root, &pattern, first_revision, query, options, ordinals.as_deref(),
            &self.budget, [report_id, worker_id], &mut canceled, &mut stopped, publish_progress, count_all_matches,
            &mut |report, sequence, canceled| {
                let ids = progress_ids.ok_or(AtlasStreamSearchError::InvalidLimits)?;
                tally.update(report)?;
                let response = self.progress_response(atlas, report, options, &scope, &tally, sequence, ids, canceled)?;
                progress(&response);
                Ok(())
            });
        // The prechecked headroom covers actual attempted reads, including a
        // terminal cancellation/error that cannot publish a new result.
        self.source_bytes_read += io.bytes_read;
        self.read_calls += io.read_calls;
        drop(ordinals);
        drop(projection_lease);
        let report = scanned?;
        let candidate = Arc::new(Snapshot::retain(&report, options, scope, &self.budget, result_id, &mut canceled)?);
        drop(report);
        drop(pattern);
        let response = self.page_response(atlas, &candidate, "search", 0, MAX_ATLAS_STREAM_SEARCH_PAGE, &mut canceled)?;
        // The final delivery gate has passed. No fallible work follows this
        // replacement, so failed/canceled output cannot erase accepted history.
        self.accepted = Some(candidate);
        Ok(response)
    }

    pub fn page(&mut self, atlas: &AtlasSession, generation: u64, start: usize, count: usize)
        -> Result<HostResponse, AtlasStreamSearchError> {
        self.validate(atlas)?;
        let snapshot = self.snapshot(generation)?.clone();
        self.page_response(atlas, &snapshot, "page", start, count, &mut || false)
    }

    /// Return only immutable literal evidence. Context is a separate current
    /// source observation with independent owner/revision and byte verification.
    pub fn hit(&mut self, atlas: &AtlasSession, generation: u64, position: usize)
        -> Result<HostResponse, AtlasStreamSearchError> {
        self.validate(atlas)?;
        let snapshot = self.snapshot(generation)?.clone();
        let hit = snapshot.hits.get(position).ok_or(AtlasStreamSearchError::MissingHit)?;
        let mut out = self.output(atlas, "hit")?;
        snapshot.scope.encode(&mut out)?;
        out.literal(",\"query_generation\":")?; out.integer(snapshot.generation)?;
        out.literal(",\"needle\":")?; out.quoted(&snapshot.needle)?;
        out.literal(",\"count_all_matches\":")?; out.boolean(snapshot.count_all_matches)?;
        out.literal(",\"match_count_complete\":")?; out.boolean(snapshot.complete)?;
        out.literal(",\"retained_matching_files\":")?; out.integer(snapshot.retained_matching_files as u64)?;
        out.literal(",\"hit\":{")?;
        encode_hit(&mut out, atlas, hit, position)?;
        out.literal(",\"text\":")?;
        let text = if snapshot.scope.predicate.case_sensitive() { snapshot.needle.as_str() } else {
            snapshot.matched_text.get(hit.text_start..hit.text_start + hit.text_len).ok_or(AtlasStreamSearchError::MissingHit)?
        };
        out.quoted(text)?;
        out.literal(",\"original_hex\":")?;
        let bytes = snapshot.witnesses.get(hit.witness_start..hit.witness_start + hit.witness_len)
            .ok_or(AtlasStreamSearchError::MissingHit)?;
        out.hex(bytes)?;
        out.literal("},\"additional_source_bytes_read\":\"0\",\"additional_read_calls\":\"0\"}\n")?;
        self.finish(atlas, out, false, &mut || false)
    }

    pub fn clear(&mut self, atlas: &AtlasSession) -> Result<HostResponse, AtlasStreamSearchError> {
        self.validate(atlas)?;
        let generation = self.next_generation()?;
        let mut out = self.output(atlas, "clear")?;
        out.literal(",\"mode\":\"exact-decoded-literal\"")?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"retained_hits\":\"0\",\"retained_witness_bytes\":\"0\"}\n")?;
        let response = self.finish(atlas, out, false, &mut || false)?;
        self.accepted = None;
        Ok(response)
    }

    fn validate(&self, atlas: &AtlasSession) -> Result<(), AtlasStreamSearchError> {
        atlas.validate_active()?;
        if atlas.atlas().catalog().id() != self.manifest || atlas.atlas().layout().revision() != self.layout
            || atlas.atlas().catalog().grant() != &self.grant {
            return Err(AtlasStreamSearchError::WrongAtlas);
        }
        Ok(())
    }
    fn snapshot(&self, generation: u64) -> Result<&Arc<Snapshot>, AtlasStreamSearchError> {
        let snapshot = self.accepted.as_ref().ok_or(AtlasStreamSearchError::MissingQuery)?;
        if snapshot.generation != generation { return Err(AtlasStreamSearchError::StaleQuery); }
        Ok(snapshot)
    }
    fn next_generation(&mut self) -> Result<u64, AtlasStreamSearchError> {
        self.last_attempt = self.last_attempt.checked_add(1).ok_or(AtlasStreamSearchError::IdentityExhausted)?;
        Ok(self.last_attempt)
    }
    fn next_id(&mut self) -> Result<ResourceAllocationId, AtlasStreamSearchError> {
        let id = self.next_allocation;
        self.next_allocation = id.checked_add(1).ok_or(AtlasStreamSearchError::IdentityExhausted)?;
        allocation(id)
    }
    fn output(&mut self, atlas: &AtlasSession, command: &str) -> Result<Output, AtlasStreamSearchError> {
        let id = self.next_id()?;
        self.output_with_allocation(atlas, command, id)
    }
    fn output_with_allocation(&self, atlas: &AtlasSession, command: &str, id: ResourceAllocationId)
        -> Result<Output, AtlasStreamSearchError> {
        let mut out = Output::new(self.manifest.owner(), MAX_RESPONSE_BYTES, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.atlas-stream-search/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        for (key, value) in [("owner", self.manifest.owner().get()), ("source_manifest", self.manifest.revision()),
            ("layout_revision", self.layout.get()), ("last_attempt", self.last_attempt)] {
            out.literal(",\"")?; out.literal(key)?; out.literal("\":")?; out.integer(value)?;
        }
        out.literal(",\"root_path\":")?; out.path(&self.grant.root_path().to_path_buf())?;
        out.literal(",\"scope\":\"catalogued-whole-files\",\"strategy\":\"streaming-whole-file\"")?;
        out.literal(",\"capture_kind\":\"literal-witnesses-only\",\"source_observation\":\"per-file-streams-not-atomic-workspace\"")?;
        out.literal(",\"catalogued_files\":")?; out.integer(atlas.atlas().file_count() as u64)?;
        out.literal(",\"discovery_policy\":")?; out.quoted(atlas.atlas().catalog().policy_name())?;
        crate::workspace::rule_output::fields(&mut out, atlas.atlas().catalog())?;
        out.literal(",\"excluded_entries\":")?; out.integer(atlas.atlas().catalog().aggregate().excluded)?;
        Ok(out)
    }
    fn page_response(&mut self, atlas: &AtlasSession, snapshot: &Snapshot, command: &str,
        start: usize, count: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, AtlasStreamSearchError> {
        if !(1..=MAX_ATLAS_STREAM_SEARCH_PAGE).contains(&count) || start > snapshot.hits.len() {
            return Err(AtlasStreamSearchError::InvalidLimits);
        }
        self.validate(atlas)?; check(canceled)?;
        let mut out = self.output(atlas, command)?;
        snapshot.scope.encode(&mut out)?;
        summary(&mut out, snapshot.summary())?;
        out.literal(",\"terminal\":true,\"selection_available\":true")?;
        let end = snapshot.hits.len().min(start + count);
        out.literal(",\"page_offset\":")?; out.integer(start as u64)?;
        out.literal(",\"page_count\":")?; out.integer((end - start) as u64)?;
        out.literal(",\"hits\":[")?;
        for (position, hit) in snapshot.hits.iter().enumerate().take(end).skip(start) {
            check(canceled)?;
            if position != start { out.literal(",")?; }
            out.literal("{")?; encode_hit(&mut out, atlas, hit, position)?; out.literal("}")?;
        }
        out.literal("],\"next_offset\":")?;
        if end < snapshot.hits.len() { out.integer(end as u64)?; } else { out.literal("null")?; }
        out.literal(",\"diagnostics\":[")?;
        for (position, file) in snapshot.diagnostics.iter().enumerate() {
            check(canceled)?;
            if position > 0 { out.literal(",")?; }
            encode_diagnostic(&mut out, *file)?;
        }
        out.literal("],\"diagnostics_truncated\":")?;
        out.boolean(snapshot.stats.incomplete_files > snapshot.diagnostics.len())?;
        out.literal("}\n")?;
        self.finish(atlas, out, !snapshot.complete || snapshot.stats.matches_seen > snapshot.hits.len() as u64, canceled)
    }
    fn progress_response(&self, atlas: &AtlasSession,
        report: &AtlasStreamReport<'_, '_, RetainedWorkspaceAtlas>, options: AtlasStreamSearchOptions,
        scope: &FrozenScope, tally: &ProgressTally, sequence: u64, allocations: [ResourceAllocationId; 2],
        canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasStreamSearchError> {
        self.validate(atlas)?; check(canceled)?;
        report.validate_delivery(atlas.atlas(), report.generation())?;
        if report.state() != AtlasStreamState::Pending || sequence == 0 {
            return Err(AtlasStreamSearchError::InvalidLimits);
        }
        let mut out = self.output_with_allocation(atlas, "progress", allocations[0])?;
        scope.encode(&mut out)?;
        summary(&mut out, tally.summary(report, options))?;
        out.literal(",\"terminal\":false,\"selection_available\":false,\"progress_sequence\":")?;
        out.integer(sequence)?;
        let count = report.hits().len().min(MAX_ATLAS_STREAM_SEARCH_PAGE);
        out.literal(",\"page_offset\":\"0\",\"page_count\":")?; out.integer(count as u64)?;
        out.literal(",\"hits\":[")?;
        for (position, &hit) in report.hits().iter().take(count).enumerate() {
            check(canceled)?;
            let selected = report.select_hit(position, atlas.atlas(), report.generation())?;
            let mut digest = Sha256::new(); digest.update(selected.original_bytes());
            let hit = Hit { hit, file: *selected.file_report(), witness_start: 0,
                witness_len: selected.original_bytes().len(), text_start: 0,
                text_len: selected.matched_text().len(), digest: digest.finalize() };
            if position > 0 { out.literal(",")?; }
            out.literal("{")?; encode_hit(&mut out, atlas, &hit, position)?; out.literal("}")?;
        }
        out.literal("],\"next_offset\":")?;
        if count < report.hits().len() { out.integer(count as u64)?; } else { out.literal("null")?; }
        out.literal(",\"diagnostics\":[")?;
        for (position, &index) in tally.diagnostics[..tally.diagnostic_count].iter().enumerate() {
            check(canceled)?;
            let file = report.files().get(index).ok_or(AtlasStreamSearchError::MissingHit)?;
            if position > 0 { out.literal(",")?; }
            encode_diagnostic(&mut out, *file)?;
        }
        out.literal("],\"diagnostics_truncated\":")?;
        out.boolean(report.stats().incomplete_files > tally.diagnostic_count)?;
        out.literal("}\n")?;
        self.finish_with_allocation(atlas, out, true, allocations[1], canceled)
    }
    fn finish(&mut self, atlas: &AtlasSession, out: Output, partial: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasStreamSearchError> {
        self.validate(atlas)?; check(canceled)?;
        let id = self.next_id()?;
        self.finish_with_allocation(atlas, out, partial, id, canceled)
    }
    fn finish_with_allocation(&self, atlas: &AtlasSession, out: Output, partial: bool,
        id: ResourceAllocationId, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasStreamSearchError> {
        let charge = out.as_bytes().len().checked_mul(3).and_then(|n| n.checked_add(4096)).ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let text = copy_text(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?)?;
        self.validate(atlas)?; check(canceled)?;
        Ok(HostResponse { text, exit_code: if partial { EXIT_PARTIAL } else { EXIT_OK }, _lease: lease })
    }
}

fn scan<'a, C: FnMut() -> bool>(atlas: &'a RetainedWorkspaceAtlas, root: &Path, pattern: &StreamingNeedle,
    first_revision: SourceRevision, generation: QueryGeneration, options: AtlasStreamSearchOptions,
    ordinals: Option<&[usize]>,
    budget: &ResourceBudget, allocations: [ResourceAllocationId; 2], canceled: &mut C,
    stopped: &mut impl FnMut() -> bool, publish_progress: bool, count_all_matches: bool,
    progress: &mut impl FnMut(&AtlasStreamReport<'a, 'a, RetainedWorkspaceAtlas>, u64, &mut C)
        -> Result<(), AtlasStreamSearchError>)
    -> (Result<AtlasStreamReport<'a, 'a, RetainedWorkspaceAtlas>, AtlasStreamSearchError>, AtlasStreamStats) {
    let admitted = match (ordinals, count_all_matches) {
        (Some(ordinals), true) => AtlasStreamSearch::new_scoped_counting(atlas, pattern, first_revision, generation,
            options.limits(), ordinals, budget, allocations),
        (None, true) => AtlasStreamSearch::new_counting(atlas, pattern, first_revision, generation,
            options.limits(), budget, allocations),
        (Some(ordinals), false) => AtlasStreamSearch::new_scoped(atlas, pattern, first_revision, generation,
            options.limits(), ordinals, budget, allocations),
        (None, false) => AtlasStreamSearch::new(atlas, pattern, first_revision, generation, options.limits(), budget, allocations),
    };
    let mut job = match admitted {
        Ok(job) => job,
        Err(error) => return (Err(error.into()), AtlasStreamStats::default()),
    };
    let mut sequence = 0u64;
    let mut steps_since_progress = 0usize;
    let mut visible_hits = 0usize;
    while job.state() == AtlasStreamState::Pending {
        // Hard cancellation wins over Stop, including a request raised by the
        // preceding progress callback. Stop never launders a canceled query.
        if canceled() {
            job.cancel();
            return (Err(AtlasStreamSearchError::Canceled), job.report().stats());
        }
        if stopped() {
            if let Err(error) = job.stop_and_keep() {
                return (Err(error.into()), job.report().stats());
            }
            break;
        }
        let current_hits = job.report().hits().len().min(MAX_ATLAS_STREAM_SEARCH_PAGE);
        if publish_progress && (sequence == 0 || current_hits > visible_hits || steps_since_progress >= PROGRESS_STEP_INTERVAL) {
            sequence += 1;
            if let Err(error) = progress(job.report(), sequence, canceled) {
                return (Err(error), job.report().stats());
            }
            visible_hits = current_hits;
            steps_since_progress = 0;
            // The callback may have requested Stop/cancel. Poll before another
            // open/read instead of spending one extra scanner quantum.
            continue;
        }
        let result = job.step(StreamReadStep::default(), generation, |_, relative| {
            let native = workspace::checked_source_path(root, relative)?;
            input::open_regular(&native).map(|(file, _)| file).map_err(|_| SourceError::CaptureUnavailable)
        }, &mut *canceled);
        steps_since_progress += 1;
        if let Err(error) = result {
            let io = job.report().stats();
            return (Err(if error == AtlasStreamError::Canceled { AtlasStreamSearchError::Canceled } else { error.into() }), io);
        }
    }
    let io = job.report().stats();
    (job.finish().map_err(AtlasStreamSearchError::from), io)
}

impl Snapshot {
    fn summary(&self) -> Summary<'_> {
        Summary { generation: self.generation, needle: &self.needle, options: self.options,
            examined: self.examined, unexamined: self.unexamined, unavailable: self.unavailable,
            unsupported: self.unsupported, failed: self.failed, matching_files: self.matching_files,
            retained_matching_files: self.retained_matching_files, count_all_matches: self.count_all_matches,
            stats: self.stats, discovery_complete: self.discovery_complete, complete: self.complete,
            stop: self.stop, retained_hits: self.hits.len(), retained_witness_bytes: self.witnesses.len() }
    }
    fn retain(report: &AtlasStreamReport<'_, '_, RetainedWorkspaceAtlas>, options: AtlasStreamSearchOptions,
        scope: FrozenScope, budget: &ResourceBudget, allocation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, AtlasStreamSearchError> {
        report.validate_delivery(report.atlas(), report.generation())?;
        check(canceled)?;
        if scope.eligible != report.eligible_files() || scope.excluded != report.excluded_files() {
            return Err(AtlasStreamSearchError::WrongAtlas);
        }
        let diagnostic_count = report.stats().incomplete_files.min(MAX_DIAGNOSTICS);
        let charge = report.hits().len().checked_mul(size_of::<Hit>())
            .and_then(|n| diagnostic_count.checked_mul(size_of::<AtlasStreamFile>()).and_then(|v| n.checked_add(v)))
            .and_then(|n| n.checked_add(report.retained_witness_bytes()))
            .and_then(|n| n.checked_add(report.retained_matched_text_bytes()))
            .and_then(|n| n.checked_add(report.literal().len() + size_of::<Self>() + 2 * size_of::<usize>()))
            .ok_or(AppError::Admission)?;
        let lease = budget.try_reserve_managed(report.generation().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let needle = copy_text(report.literal())?;
        let mut hits = reserve(report.hits().len())?;
        let mut witnesses = reserve(report.retained_witness_bytes())?;
        let mut matched_text = String::new();
        matched_text.try_reserve_exact(report.retained_matched_text_bytes()).map_err(|_| AppError::Admission)?;
        if matched_text.capacity() > report.retained_matched_text_bytes() { return Err(AppError::Admission.into()); }
        let mut diagnostics = reserve(diagnostic_count)?;
        let mut previous: Option<(usize, usize, usize, Sha256Digest)> = None;
        let mut retained_matching_files = 0usize;
        for (position, &hit) in report.hits().iter().enumerate() {
            check(canceled)?;
            let selected = report.select_hit(position, report.atlas(), report.generation())?;
            if previous.is_none_or(|(file, _, _, _)| file != hit.file_record()) { retained_matching_files += 1; }
            let (witness_start, witness_len, digest) = match previous {
                Some((file, start, len, digest)) if report.case_sensitive() && file == hit.file_record() => (start, len, digest),
                _ => {
                    let start = witnesses.len();
                    let bytes = selected.original_bytes();
                    if bytes.len() > witnesses.capacity() - start { return Err(AppError::Admission.into()); }
                    witnesses.extend_from_slice(bytes);
                    let mut digest = Sha256::new(); digest.update(bytes);
                    let digest = digest.finalize();
                    previous = Some((hit.file_record(), start, bytes.len(), digest));
                    (start, bytes.len(), digest)
                }
            };
            let text_start = matched_text.len();
            let text_len = selected.matched_text().len();
            if !report.case_sensitive() {
                if text_len > matched_text.capacity() - text_start { return Err(AppError::Admission.into()); }
                matched_text.push_str(selected.matched_text());
            }
            hits.push(Hit { hit, file: *selected.file_report(), witness_start, witness_len, text_start, text_len, digest });
        }
        let mut unavailable = 0usize;
        let mut unsupported = 0usize;
        let mut failed = 0usize;
        for &file in report.files() {
            check(canceled)?;
            match file.state() {
                AtlasStreamFileState::Unavailable(_) => unavailable += 1,
                AtlasStreamFileState::Scanned(StreamReadState::UnsupportedText) => unsupported += 1,
                AtlasStreamFileState::Scanned(StreamReadState::Failed(_)) => failed += 1,
                _ => {},
            }
            if !file.is_complete() && diagnostics.len() < diagnostic_count { diagnostics.push(file); }
        }
        check(canceled)?;
        let complete = report.is_complete() && scope.region_complete();
        let count_all_matches = report.count_all_matches();
        let matching_files = if count_all_matches { report.matching_files_seen() } else { retained_matching_files };
        Ok(Self { generation: report.generation().get(), needle, options, scope, hits, witnesses, matched_text, diagnostics,
            examined: report.files_examined(), unexamined: report.unexamined_files(), unavailable, unsupported, failed, matching_files,
            retained_matching_files, count_all_matches,
            stats: report.stats(), discovery_complete: report.atlas().discovery_complete(), complete,
            stop: report.stop_reason(), _lease: lease })
    }
}

/// Scalar view shared by terminal and pending output. The progress path never
/// copies the report's hit/witness arrays or rescans its earlier file records.
struct Summary<'a> {
    generation: u64,
    needle: &'a str,
    options: AtlasStreamSearchOptions,
    examined: usize,
    unexamined: usize,
    unavailable: usize,
    unsupported: usize,
    failed: usize,
    matching_files: usize,
    retained_matching_files: usize,
    count_all_matches: bool,
    stats: AtlasStreamStats,
    discovery_complete: bool,
    complete: bool,
    stop: Option<AtlasStreamStop>,
    retained_hits: usize,
    retained_witness_bytes: usize,
}
#[derive(Default)]
struct ProgressTally {
    examined: usize,
    unavailable: usize,
    unsupported: usize,
    failed: usize,
    matching_files: usize,
    diagnostics: [usize; MAX_DIAGNOSTICS],
    diagnostic_count: usize,
}
impl ProgressTally {
    fn update(&mut self, report: &AtlasStreamReport<'_, '_, RetainedWorkspaceAtlas>)
        -> Result<(), AtlasStreamSearchError> {
        let unseen = report.files().get(self.examined..).ok_or(AtlasStreamSearchError::InvalidLimits)?;
        for (offset, file) in unseen.iter().enumerate() {
            match file.state() {
                AtlasStreamFileState::Unavailable(_) => self.unavailable += 1,
                AtlasStreamFileState::Scanned(StreamReadState::UnsupportedText) => self.unsupported += 1,
                AtlasStreamFileState::Scanned(StreamReadState::Failed(_)) => self.failed += 1,
                _ => {},
            }
            if file.hit_count() > 0 { self.matching_files += 1; }
            if !file.is_complete() && self.diagnostic_count < MAX_DIAGNOSTICS {
                self.diagnostics[self.diagnostic_count] = self.examined + offset;
                self.diagnostic_count += 1;
            }
        }
        self.examined = report.files_examined();
        Ok(())
    }
    fn summary<'a>(&self, report: &'a AtlasStreamReport<'_, '_, RetainedWorkspaceAtlas>,
        options: AtlasStreamSearchOptions) -> Summary<'a> {
        Summary { generation: report.generation().get(), needle: report.literal(), options,
            examined: self.examined, unexamined: report.unexamined_files(), unavailable: self.unavailable,
            unsupported: self.unsupported, failed: self.failed,
            matching_files: if report.count_all_matches() { report.matching_files_seen() } else { self.matching_files },
            retained_matching_files: self.matching_files, count_all_matches: report.count_all_matches(),
            stats: report.stats(), discovery_complete: report.atlas().discovery_complete(), complete: false,
            stop: None, retained_hits: report.hits().len(), retained_witness_bytes: report.retained_witness_bytes() }
    }
}

fn summary(out: &mut Output, snapshot: Summary<'_>) -> Result<(), AtlasStreamSearchError> {
    out.literal(",\"query_generation\":")?; out.integer(snapshot.generation)?;
    out.literal(",\"needle\":")?; out.quoted(snapshot.needle)?;
    out.literal(",\"discovery_complete\":")?; out.boolean(snapshot.discovery_complete)?;
    out.literal(",\"search_complete\":")?; out.boolean(snapshot.complete)?;
    out.literal(",\"count_all_matches\":")?; out.boolean(snapshot.count_all_matches)?;
    out.literal(",\"match_count_complete\":")?; out.boolean(snapshot.complete)?;
    out.literal(",\"truncated\":")?; out.boolean(snapshot.stats.matches_seen > snapshot.retained_hits as u64)?;
    out.literal(",\"stop_reason\":")?;
    if let Some(stop) = snapshot.stop { out.quoted(stop.code())?; } else { out.literal("null")?; }
    for (key, value) in [("files_examined", snapshot.examined as u64), ("files_unexamined", snapshot.unexamined as u64),
        ("incomplete_files", snapshot.stats.incomplete_files as u64), ("unavailable_files", snapshot.unavailable as u64),
        ("unsupported_files", snapshot.unsupported as u64), ("failed_files", snapshot.failed as u64),
        ("matching_files", snapshot.matching_files as u64), ("retained_hits", snapshot.retained_hits as u64),
        ("retained_matching_files", snapshot.retained_matching_files as u64),
        ("matches_seen", snapshot.stats.matches_seen), ("payload_bytes_read", snapshot.stats.bytes_read),
        ("scanned_input_bytes", snapshot.stats.scanned_bytes), ("read_calls", snapshot.stats.read_calls),
        ("interrupted_read_calls", snapshot.stats.interrupted_calls),
        ("peak_input_buffer_bytes", snapshot.stats.peak_input_buffer_bytes as u64),
        ("retained_witness_bytes", snapshot.retained_witness_bytes as u64)] {
        out.literal(",\"")?; out.literal(key)?; out.literal("\":")?; out.integer(value)?;
    }
    out.literal(",\"limits\":{\"max_matches\":")?; out.integer(snapshot.options.max_matches as u64)?;
    out.literal(",\"max_scan_bytes\":")?; out.integer(snapshot.options.max_scan_bytes)?;
    out.literal(",\"max_read_calls\":")?; out.integer(snapshot.options.max_read_calls)?; out.literal("}")?;
    Ok(())
}
fn encode_hit(out: &mut Output, atlas: &AtlasSession, hit: &Hit, position: usize) -> Result<(), AtlasStreamSearchError> {
    let entry = atlas.atlas().catalog().entry(hit.hit.file()).ok_or(AtlasStreamSearchError::WrongAtlas)?;
    if hit.hit.file() != hit.file.file() || hit.hit.revision() != hit.file.revision()
        || hit.hit.node() != hit.file.node() || hit.hit.original_range().len().get() != hit.witness_len as u64 {
        return Err(AtlasStreamSearchError::MissingHit);
    }
    out.literal("\"index\":")?; out.integer(position as u64)?;
    out.literal(",\"occurrence_id\":")?; out.integer(hit.hit.occurrence_id())?;
    out.literal(",\"file_id\":")?; out.integer(hit.hit.file().get())?;
    out.literal(",\"node_ordinal\":")?; out.integer(hit.hit.node().ordinal() as u64)?;
    out.literal(",\"source_revision\":")?; out.integer(hit.hit.revision().get())?;
    out.literal(",\"path\":")?; out.path(&entry.path().raw().to_path_buf())?;
    out.literal(",\"original_range\":")?; out.range(hit.hit.original_range())?;
    encode_file(out, hit.file)?;
    out.literal(",\"witness_sha256\":")?; out.quoted(&hit.digest.to_hex())?;
    Ok(())
}
fn encode_diagnostic(out: &mut Output, file: AtlasStreamFile) -> Result<(), AtlasStreamSearchError> {
    out.literal("{\"file_id\":")?; out.integer(file.file().get())?;
    out.literal(",\"node_ordinal\":")?; out.integer(file.node().ordinal() as u64)?;
    out.literal(",\"source_revision\":")?; out.integer(file.revision().get())?;
    encode_file(out, file)?;
    out.literal(",\"unsupported_at\":")?;
    if let Some(offset) = file.unsupported_at() { out.integer(offset.get())?; } else { out.literal("null")?; }
    out.literal("}")?;
    Ok(())
}
fn encode_file(out: &mut Output, file: AtlasStreamFile) -> Result<(), AtlasStreamSearchError> {
    out.literal(",\"file_state\":")?;
    out.quoted(match file.state() { AtlasStreamFileState::Unavailable(_) => "unavailable",
        AtlasStreamFileState::Scanned(state) => state.code() })?;
    out.literal(",\"file_complete\":")?; out.boolean(file.is_complete())?;
    out.literal(",\"file_error_code\":")?;
    match file.state() {
        AtlasStreamFileState::Unavailable(error) => out.quoted(&error.to_string())?,
        AtlasStreamFileState::Scanned(StreamReadState::Failed(error)) => out.quoted(&error.to_string())?,
        _ => out.literal("null")?,
    }
    out.literal(",\"encoding\":")?;
    match file.encoding() {
        Some(DetectedEncoding::Utf8 { .. }) => out.quoted("utf8")?,
        Some(DetectedEncoding::Utf16Le) => out.quoted("utf16le")?,
        Some(DetectedEncoding::Utf16Be) => out.quoted("utf16be")?,
        _ => out.literal("null")?,
    }
    out.literal(",\"observed_length\":")?;
    if let Some(length) = file.observed_length() { out.integer(length.get())?; } else { out.literal("null")?; }
    out.literal(",\"final_observed_length\":")?;
    if let Some(length) = file.final_length() { out.integer(length.get())?; } else { out.literal("null")?; }
    out.literal(",\"consistency\":")?;
    match file.consistency() {
        Some(ExtentConsistency::UnchangedMetadata) => out.quoted("unchanged-metadata-not-atomic")?,
        Some(ExtentConsistency::ChangedDuringRead) => out.quoted("changed-during-read")?,
        Some(ExtentConsistency::ShortRead) => out.quoted("short-read")?,
        Some(_) => out.quoted("metadata-unavailable")?,
        None => out.literal("null")?,
    }
    Ok(())
}
fn allocation(value: u64) -> Result<ResourceAllocationId, AtlasStreamSearchError> {
    ResourceAllocationId::new(value).map_err(|_| AtlasStreamSearchError::IdentityExhausted)
}
fn reserve<T>(capacity: usize) -> Result<Vec<T>, AtlasStreamSearchError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
    if values.capacity() > capacity { return Err(AppError::Admission.into()); }
    Ok(values)
}
fn copy_text(text: &str) -> Result<String, AtlasStreamSearchError> {
    let mut result = String::new();
    result.try_reserve_exact(text.len()).map_err(|_| AppError::Admission)?;
    if result.capacity() > text.len() { return Err(AppError::Admission.into()); }
    result.push_str(text);
    Ok(result)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasStreamSearchError> {
    if canceled() { Err(AtlasStreamSearchError::Canceled) } else { Ok(()) }
}

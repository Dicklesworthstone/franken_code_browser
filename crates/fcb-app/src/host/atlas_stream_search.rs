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

pub const MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES: usize = 1024;
pub const MAX_ATLAS_STREAM_SEARCH_HITS: usize = 256;
pub const MAX_ATLAS_STREAM_SEARCH_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_ATLAS_STREAM_SEARCH_READ_CALLS: u64 = 1_048_576;
/// Rows carry lossless native paths. A bounded page, not the hit limit, bounds
/// worst-case escaped path output under the ordinary response-byte allowance.
pub const MAX_ATLAS_STREAM_SEARCH_PAGE: usize = 32;
const MAX_DIAGNOSTICS: usize = 32;

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
    App(AppError), Atlas(AtlasSessionError), Stream(AtlasStreamError),
    InvalidLimits, WrongAtlas, StaleQuery, MissingQuery, MissingHit, Canceled, IdentityExhausted,
}
impl std::fmt::Display for AtlasStreamSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "{e}"), Self::Atlas(e) => write!(f, "{e}"),
            Self::Stream(e) => write!(f, "{e}"),
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
impl From<OutputError> for AtlasStreamSearchError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

#[derive(Clone, Copy)]
struct Hit {
    hit: AtlasStreamHit,
    file: AtlasStreamFile,
    witness_start: usize,
    witness_len: usize,
    digest: Sha256Digest,
}
struct Snapshot {
    generation: u64,
    needle: String,
    options: AtlasStreamSearchOptions,
    hits: Vec<Hit>,
    witnesses: Vec<u8>,
    diagnostics: Vec<AtlasStreamFile>,
    examined: usize,
    unexamined: usize,
    unavailable: usize,
    unsupported: usize,
    failed: usize,
    matching_files: usize,
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
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasStreamSearchError> {
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
        self.source_bytes_read.checked_add(options.max_scan_bytes).ok_or(AtlasStreamSearchError::IdentityExhausted)?;
        self.read_calls.checked_add(options.max_read_calls).ok_or(AtlasStreamSearchError::IdentityExhausted)?;
        let pattern_id = self.next_id()?;
        let report_id = self.next_id()?;
        let worker_id = self.next_id()?;
        let result_id = self.next_id()?;
        let pattern = StreamingNeedle::text(self.manifest.owner(), needle, &self.budget, pattern_id)
            .map_err(AppError::from)?;
        let root = self.grant.root_path().to_path_buf();
        let (scanned, io) = scan(atlas.atlas(), &root, &pattern, first_revision, query, options,
            &self.budget, [report_id, worker_id], &mut canceled);
        // The prechecked headroom covers actual attempted reads, including a
        // terminal cancellation/error that cannot publish a new result.
        self.source_bytes_read += io.bytes_read;
        self.read_calls += io.read_calls;
        let report = scanned?;
        let candidate = Arc::new(Snapshot::retain(&report, options, &self.budget, result_id, &mut canceled)?);
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
        out.literal(",\"query_generation\":")?; out.integer(snapshot.generation)?;
        out.literal(",\"needle\":")?; out.quoted(&snapshot.needle)?;
        out.literal(",\"hit\":{")?;
        encode_hit(&mut out, atlas, hit, position)?;
        out.literal(",\"text\":")?; out.quoted(&snapshot.needle)?;
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
        let mut out = Output::new(self.manifest.owner(), MAX_RESPONSE_BYTES, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.atlas-stream-search/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        for (key, value) in [("owner", self.manifest.owner().get()), ("source_manifest", self.manifest.revision()),
            ("layout_revision", self.layout.get()), ("last_attempt", self.last_attempt)] {
            out.literal(",\"")?; out.literal(key)?; out.literal("\":")?; out.integer(value)?;
        }
        out.literal(",\"root_path\":")?; out.path(&self.grant.root_path().to_path_buf())?;
        out.literal(",\"scope\":\"catalogued-whole-files\",\"strategy\":\"streaming-whole-file\",\"mode\":\"exact-decoded-literal\"")?;
        out.literal(",\"capture_kind\":\"literal-witnesses-only\",\"source_observation\":\"per-file-streams-not-atomic-workspace\"")?;
        out.literal(",\"catalogued_files\":")?; out.integer(atlas.atlas().file_count() as u64)?;
        out.literal(",\"discovery_policy\":")?; out.quoted(atlas.atlas().catalog().policy_name())?;
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
        summary(&mut out, snapshot)?;
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
            out.literal("{\"file_id\":")?; out.integer(file.file().get())?;
            out.literal(",\"node_ordinal\":")?; out.integer(file.node().ordinal() as u64)?;
            out.literal(",\"source_revision\":")?; out.integer(file.revision().get())?;
            encode_file(&mut out, *file)?;
            out.literal(",\"unsupported_at\":")?;
            if let Some(offset) = file.unsupported_at() { out.integer(offset.get())?; } else { out.literal("null")?; }
            out.literal("}")?;
        }
        out.literal("],\"diagnostics_truncated\":")?;
        out.boolean(snapshot.stats.incomplete_files > snapshot.diagnostics.len())?;
        out.literal("}\n")?;
        self.finish(atlas, out, !snapshot.complete, canceled)
    }
    fn finish(&mut self, atlas: &AtlasSession, out: Output, partial: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasStreamSearchError> {
        self.validate(atlas)?; check(canceled)?;
        let id = self.next_id()?;
        let charge = out.as_bytes().len().checked_mul(3).and_then(|n| n.checked_add(4096)).ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let text = copy_text(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?)?;
        self.validate(atlas)?; check(canceled)?;
        Ok(HostResponse { text, exit_code: if partial { EXIT_PARTIAL } else { EXIT_OK }, _lease: lease })
    }
}

fn scan<'a>(atlas: &'a RetainedWorkspaceAtlas, root: &Path, pattern: &StreamingNeedle,
    first_revision: SourceRevision, generation: QueryGeneration, options: AtlasStreamSearchOptions,
    budget: &ResourceBudget, allocations: [ResourceAllocationId; 2], canceled: &mut impl FnMut() -> bool)
    -> (Result<AtlasStreamReport<'a, 'a, RetainedWorkspaceAtlas>, AtlasStreamSearchError>, AtlasStreamStats) {
    let mut job = match AtlasStreamSearch::new(atlas, pattern, first_revision, generation, options.limits(), budget, allocations) {
        Ok(job) => job,
        Err(error) => return (Err(error.into()), AtlasStreamStats::default()),
    };
    while job.state() == AtlasStreamState::Pending {
        let result = job.step(StreamReadStep::default(), generation, |_, relative| {
            let native = workspace::checked_source_path(root, relative)?;
            input::open_regular(&native).map(|(file, _)| file).map_err(|_| SourceError::CaptureUnavailable)
        }, &mut *canceled);
        if let Err(error) = result {
            let io = job.report().stats();
            return (Err(if error == AtlasStreamError::Canceled { AtlasStreamSearchError::Canceled } else { error.into() }), io);
        }
    }
    let io = job.report().stats();
    (job.finish().map_err(AtlasStreamSearchError::from), io)
}

impl Snapshot {
    fn retain(report: &AtlasStreamReport<'_, '_, RetainedWorkspaceAtlas>, options: AtlasStreamSearchOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, AtlasStreamSearchError> {
        report.validate_delivery(report.atlas(), report.generation())?;
        check(canceled)?;
        let diagnostic_count = report.stats().incomplete_files.min(MAX_DIAGNOSTICS);
        let charge = report.hits().len().checked_mul(size_of::<Hit>())
            .and_then(|n| diagnostic_count.checked_mul(size_of::<AtlasStreamFile>()).and_then(|v| n.checked_add(v)))
            .and_then(|n| n.checked_add(report.retained_witness_bytes()))
            .and_then(|n| n.checked_add(report.literal().len() + size_of::<Self>() + 2 * size_of::<usize>()))
            .ok_or(AppError::Admission)?;
        let lease = budget.try_reserve_managed(report.generation().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let needle = copy_text(report.literal())?;
        let mut hits = reserve(report.hits().len())?;
        let mut witnesses = reserve(report.retained_witness_bytes())?;
        let mut diagnostics = reserve(diagnostic_count)?;
        let mut previous: Option<(usize, usize, usize, Sha256Digest)> = None;
        let mut matching_files = 0usize;
        for (position, &hit) in report.hits().iter().enumerate() {
            check(canceled)?;
            let selected = report.select_hit(position, report.atlas(), report.generation())?;
            let (witness_start, witness_len, digest) = match previous {
                Some((file, start, len, digest)) if file == hit.file_record() => (start, len, digest),
                _ => {
                    let start = witnesses.len();
                    let bytes = selected.original_bytes();
                    if bytes.len() > witnesses.capacity() - start { return Err(AppError::Admission.into()); }
                    witnesses.extend_from_slice(bytes);
                    let mut digest = Sha256::new(); digest.update(bytes);
                    let digest = digest.finalize();
                    previous = Some((hit.file_record(), start, bytes.len(), digest));
                    matching_files += 1;
                    (start, bytes.len(), digest)
                }
            };
            hits.push(Hit { hit, file: *selected.file_report(), witness_start, witness_len, digest });
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
        Ok(Self { generation: report.generation().get(), needle, options, hits, witnesses, diagnostics,
            examined: report.files_examined(), unexamined: report.unexamined_files(), unavailable, unsupported, failed, matching_files,
            stats: report.stats(), discovery_complete: report.atlas().discovery_complete(), complete: report.is_complete(),
            stop: report.stop_reason(), _lease: lease })
    }
}

fn summary(out: &mut Output, snapshot: &Snapshot) -> Result<(), AtlasStreamSearchError> {
    out.literal(",\"query_generation\":")?; out.integer(snapshot.generation)?;
    out.literal(",\"needle\":")?; out.quoted(&snapshot.needle)?;
    out.literal(",\"discovery_complete\":")?; out.boolean(snapshot.discovery_complete)?;
    out.literal(",\"search_complete\":")?; out.boolean(snapshot.complete)?;
    out.literal(",\"truncated\":")?; out.boolean(snapshot.stop == Some(AtlasStreamStop::MatchLimit))?;
    out.literal(",\"stop_reason\":")?;
    if let Some(stop) = snapshot.stop { out.quoted(stop.code())?; } else { out.literal("null")?; }
    for (key, value) in [("files_examined", snapshot.examined as u64), ("files_unexamined", snapshot.unexamined as u64),
        ("incomplete_files", snapshot.stats.incomplete_files as u64), ("unavailable_files", snapshot.unavailable as u64),
        ("unsupported_files", snapshot.unsupported as u64), ("failed_files", snapshot.failed as u64),
        ("matching_files", snapshot.matching_files as u64), ("retained_hits", snapshot.hits.len() as u64),
        ("matches_seen", snapshot.stats.matches_seen), ("payload_bytes_read", snapshot.stats.bytes_read),
        ("scanned_input_bytes", snapshot.stats.scanned_bytes), ("read_calls", snapshot.stats.read_calls),
        ("interrupted_read_calls", snapshot.stats.interrupted_calls),
        ("peak_input_buffer_bytes", snapshot.stats.peak_input_buffer_bytes as u64),
        ("retained_witness_bytes", snapshot.witnesses.len() as u64)] {
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

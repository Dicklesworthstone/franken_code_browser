#![forbid(unsafe_code)]

//! Retained repository search for the native-host atlas. Search is explicit
//! worker work over its frozen catalog, not a camera callback or a new walk.
//! The existing capture and exact-text engines own I/O, decoding and matching.
//! Matching files remain captured until successful replacement or explicit clear;
//! result activation never reopens a live path. Synchronous and resumable live
//! queries share a direct-scan pipeline. Explicitly prepared ephemeral indexes
//! instead reuse captured sources across queries. Neither is a persistent index
//! or an atomic cross-file filesystem snapshot.

mod progressive;
pub use progressive::{AtlasSearchProgress, AtlasSearchStop};
use progressive::SearchWork;
mod indexed;
pub use indexed::{AtlasIndexOptions, MAX_ATLAS_INDEX_GRAMS};
use indexed::{IndexQueryUsage, RetainedIndex};

use std::{mem::size_of, path::Path};
use fcb::{ArenaOwnerId, ByteLength, ByteRange, FileId, SourceRevision};
use fcb::map::{AtlasNodeId, LayoutRevision};
use fcb::search::{CompleteCapture, IndexError, ResourceAllocationId, ResourceBudget, SearchManifestId};
use fcb_core::ResourceLease;
use fcb::search::workspace::RootGrant;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL, MANAGED_BYTES};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};
use super::{HostResponse, atlas_session::{AtlasAction, AtlasSession, AtlasSessionError},
    reader::{ReaderSession, ReaderSessionError}};

pub const MAX_ATLAS_SEARCH_HITS: usize = 4096;
pub const MAX_ATLAS_SEARCH_FILES: usize = 4096;
pub const MAX_ATLAS_SEARCH_SOURCE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ATLAS_SEARCH_NEEDLE_BYTES: usize = 1024;
pub const MAX_ATLAS_SEARCH_PAGE: usize = 128;
const MAX_DIAGNOSTICS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasSearchOptions {
    pub max_matches: usize,
    pub max_files: usize,
    pub max_file_bytes: usize,
    /// Actual source-read budget, including unsuccessful capture attempts.
    pub max_source_bytes: usize,
}
impl Default for AtlasSearchOptions {
    fn default() -> Self {
        Self { max_matches: 1000, max_files: MAX_ATLAS_SEARCH_FILES,
            max_file_bytes: 1024 * 1024, max_source_bytes: MAX_ATLAS_SEARCH_SOURCE_BYTES }
    }
}
impl AtlasSearchOptions {
    fn validate(self) -> Result<(), AtlasSearchError> {
        if !(1..=MAX_ATLAS_SEARCH_HITS).contains(&self.max_matches)
            || !(1..=MAX_ATLAS_SEARCH_FILES).contains(&self.max_files)
            || !(1..=1024 * 1024).contains(&self.max_file_bytes)
            || !(1..=MAX_ATLAS_SEARCH_SOURCE_BYTES).contains(&self.max_source_bytes) {
            return Err(AtlasSearchError::InvalidLimits);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasSearchError {
    App(AppError), Atlas(AtlasSessionError), Reader(ReaderSessionError), Index(IndexError),
    InvalidLimits, WrongAtlas, StaleQuery, MissingQuery, MissingHit, Canceled,
    IdentityExhausted, MissingIndex, StaleIndex,
}
impl std::fmt::Display for AtlasSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "{e}"), Self::Atlas(e) => write!(f, "{e}"),
            Self::Reader(e) => write!(f, "{e}"), Self::Index(e) => write!(f, "{e}"),
            other => f.write_str(match other {
                Self::InvalidLimits => "ATLAS_SEARCH_INVALID_LIMITS",
                Self::WrongAtlas => "ATLAS_SEARCH_WRONG_ATLAS",
                Self::StaleQuery => "ATLAS_SEARCH_STALE_QUERY",
                Self::MissingQuery => "ATLAS_SEARCH_NO_QUERY",
                Self::MissingHit => "ATLAS_SEARCH_NO_HIT",
                Self::Canceled => "ATLAS_SEARCH_CANCELED",
                Self::IdentityExhausted => "ATLAS_SEARCH_IDENTITY_EXHAUSTED",
                Self::MissingIndex => "ATLAS_SEARCH_NO_INDEX",
                Self::StaleIndex => "ATLAS_SEARCH_STALE_INDEX",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for AtlasSearchError {}
impl From<AppError> for AtlasSearchError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<AtlasSessionError> for AtlasSearchError { fn from(e: AtlasSessionError) -> Self { Self::Atlas(e) } }
impl From<ReaderSessionError> for AtlasSearchError { fn from(e: ReaderSessionError) -> Self { Self::Reader(e) } }
impl From<OutputError> for AtlasSearchError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

/// Occurrence IDs are local to one query, not row positions in a UI. A running
/// query appends occurrences without renumbering its previously delivered hits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasSearchHit {
    pub id: u64,
    pub node: AtlasNodeId,
    pub file: FileId,
    pub revision: SourceRevision,
    pub original_range: ByteRange,
    source_slot: usize,
}
struct RetainedFile { capture: CompleteCapture, node: AtlasNodeId, hits: usize }
struct Unavailable { file: FileId, reason: &'static str }
struct Snapshot {
    generation: u64,
    needle: String,
    files: Vec<RetainedFile>,
    hits: Vec<AtlasSearchHit>,
    diagnostics: Vec<Unavailable>,
    examined: usize,
    scanned: usize,
    unavailable: usize,
    pending: usize,
    matches_seen: u64,
    source_bytes_read: u64,
    read_calls: u64,
    retained_bytes: usize,
    truncated: bool,
    complete: bool,
    stop_reason: Option<AtlasSearchStop>,
    step_count: u64,
    last_step_files: usize,
    last_step_bytes: u64,
    last_step_calls: u64,
    index_usage: Option<IndexQueryUsage>,
    _lease: ResourceLease,
}
impl Snapshot {
    fn unavailable(&mut self, file: FileId, reason: &'static str) {
        self.unavailable += 1;
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics.push(Unavailable { file, reason });
        }
    }
}

/// One atlas, one finished query and at most one resumable replacement. A host
/// can page/open the running query's exact captured hits, or keep displaying the
/// finished query until replacement succeeds. Canceling/failing a replacement
/// retires its progress, not the finished query. A separately prepared index
/// retains its complete captured membership for subsequent explicit queries.
/// Source work and destruction belong on a worker, including cancel_pending.
/// Managed reservations do not account for unrelated host/native allocations.
pub struct RetainedAtlasSearch {
    manifest: SearchManifestId,
    grant: RootGrant,
    layout: LayoutRevision,
    last_attempt: u64,
    next_allocation: u64,
    accepted: Option<Snapshot>,
    pending: Option<SearchWork>,
    index: Option<RetainedIndex>,
    budget: ResourceBudget,
    _metadata_lease: ResourceLease,
}
impl RetainedAtlasSearch {
    pub fn new(atlas: &AtlasSession) -> Result<Self, AtlasSearchError> {
        atlas.validate_active()?;
        let manifest = atlas.atlas().catalog().id();
        let budget = ResourceBudget::new(manifest.owner(), ByteLength::new(MANAGED_BYTES))
            .map_err(|_| AppError::Admission)?;
        let grant = atlas.atlas().catalog().grant();
        let metadata_lease = budget.try_reserve_managed(manifest.owner(),
            ResourceAllocationId::new(99).map_err(|_| AppError::Admission)?,
            ByteLength::new((2 * grant.root_path().len() + size_of::<Self>()) as u64))
            .map_err(|_| AppError::Admission)?;
        Ok(Self { manifest, grant: grant.clone(), layout: atlas.atlas().layout().revision(), last_attempt: 0,
            next_allocation: 100, accepted: None, pending: None, index: None, budget, _metadata_lease: metadata_lease })
    }
    pub fn accepted_generation(&self) -> Option<u64> { self.accepted.as_ref().map(|q| q.generation) }
    /// Bytes retained by the finished query. Pending progress is reported by progress().
    pub fn retained_source_bytes(&self) -> usize { self.accepted.as_ref().map_or(0, |q| q.retained_bytes) }
    fn validate(&self, atlas: &AtlasSession) -> Result<(), AtlasSearchError> {
        atlas.validate_active()?;
        if atlas.atlas().catalog().id() != self.manifest || atlas.atlas().layout().revision() != self.layout
            || atlas.atlas().catalog().grant() != &self.grant {
            return Err(AtlasSearchError::WrongAtlas);
        }
        Ok(())
    }
    fn attempt(&mut self, generation: u64) -> Result<(), AtlasSearchError> {
        if generation == 0 || generation <= self.last_attempt { return Err(AtlasSearchError::StaleQuery); }
        self.last_attempt = generation;
        // A newer attempt cannot leave an obsolete running request able to finish
        // later. The finished query remains available even if the new attempt fails.
        self.pending = None;
        Ok(())
    }
    fn snapshot(&self, generation: u64) -> Result<&Snapshot, AtlasSearchError> {
        if let Some(work) = &self.pending {
            if work.snapshot.generation == generation { return Ok(&work.snapshot); }
        }
        let query = self.accepted.as_ref().ok_or(if self.pending.is_some() {
            AtlasSearchError::StaleQuery
        } else { AtlasSearchError::MissingQuery })?;
        if query.generation != generation { return Err(AtlasSearchError::StaleQuery); }
        Ok(query)
    }
    pub fn hit(&self, atlas: &AtlasSession, generation: u64, id: u64) -> Result<AtlasSearchHit, AtlasSearchError> {
        self.validate(atlas)?;
        let position = id.checked_sub(1).and_then(|n| usize::try_from(n).ok()).ok_or(AtlasSearchError::MissingHit)?;
        self.snapshot(generation)?.hits.get(position).copied().ok_or(AtlasSearchError::MissingHit)
    }

    /// Compatibility worker operation: run the same resumable pipeline to its
    /// terminal state without exposing intermediate progress. Use begin/step for
    /// interleaving camera, paging, activation and cancellation between files.
    pub fn search(&mut self, atlas: &AtlasSession, generation: u64, needle: &str,
        options: AtlasSearchOptions, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let mut work = self.prepare_work(atlas, generation, needle, options, &mut canceled)?;
        while work.snapshot.stop_reason.is_none() {
            self.advance_work(atlas, &mut work, &mut canceled)?;
        }
        let response = self.work_response(atlas, &work, "search", &mut canceled)?;
        self.accepted = Some(work.snapshot);
        Ok(response)
    }

    /// Paging never reads a source or recomputes a query. Running-query pages
    /// are explicitly provisional; next_offset=null means end of currently
    /// retained rows, not proof that no later step can append another hit.
    pub fn page(&mut self, atlas: &AtlasSession, generation: u64, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; check(&mut canceled)?;
        if !(1..=MAX_ATLAS_SEARCH_PAGE).contains(&limit) { return Err(AtlasSearchError::InvalidLimits); }
        let mut out = self.output("page")?;
        let snapshot = self.snapshot(generation)?;
        if start > snapshot.hits.len() { return Err(AtlasSearchError::InvalidLimits); }
        encode_page(&mut out, atlas, snapshot, start, limit, &mut canceled)?;
        let partial = !snapshot.complete;
        self.finish(atlas, out, partial, &mut canceled)
    }

    /// One compact entry per matching file, never one drawable per occurrence.
    /// Counts cover retained hits only, including explicitly running queries.
    pub fn overlay(&mut self, atlas: &AtlasSession, generation: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; check(&mut canceled)?;
        let mut out = self.output("overlay")?;
        let snapshot = self.snapshot(generation)?;
        summary(&mut out, atlas, snapshot)?;
        out.literal(",\"count_basis\":\"retained-hits\",\"files\":[")?;
        for (i, file) in snapshot.files.iter().enumerate() {
            check(&mut canceled)?;
            if i != 0 { out.literal(",")?; }
            out.literal("{\"node\":")?; out.integer(file.node.ordinal() as u64)?;
            out.literal(",\"file_id\":")?; out.integer(file.capture.request().file().get())?;
            out.literal(",\"retained_occurrences\":")?; out.integer(file.hits as u64)?;
            out.literal("}")?;
        }
        out.literal("]}\n")?;
        let partial = !snapshot.complete;
        self.finish(atlas, out, partial, &mut canceled)
    }

    /// Clear results, not the explicitly prepared reusable index.
    pub fn clear(&mut self, atlas: &AtlasSession, generation: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?; check(&mut canceled)?;
        let mut out = self.output("clear")?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"retained_hits\":\"0\"}\n")?;
        let response = self.finish(atlas, out, false, &mut canceled)?;
        self.accepted = None; // Worker-side destruction, never the input callback.
        Ok(response)
    }

    /// Prepare a camera plan for a finished or running query's exact occurrence.
    /// Old picking stays authoritative until the new plan is acknowledged.
    pub fn focus_hit(&self, atlas: &mut AtlasSession, generation: u64, id: u64,
        plan_generation: u64, canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        let hit = self.hit(atlas, generation, id)?;
        Ok(atlas.prepare(plan_generation, AtlasAction::Focus(hit.node.ordinal()), canceled)?)
    }

    /// Copy the retained WHOLE matching capture into an independently owned
    /// reader. Source and reader identities are explicitly linked, not relabeled.
    /// This also works for partial progress; subsequent cancellation or replacement
    /// cannot change an already delivered reader's captured bytes.
    pub fn open_reader(&mut self, atlas: &AtlasSession, reader_owner: ArenaOwnerId,
        generation: u64, id: u64, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), AtlasSearchError> {
        self.validate(atlas)?; check(&mut canceled)?;
        if reader_owner == self.manifest.owner() { return Err(AtlasSearchError::WrongAtlas); }
        let hit = self.hit(atlas, generation, id)?;
        let mut out = self.output("open-reader")?;
        let snapshot = self.snapshot(generation)?;
        let source = &snapshot.files[hit.source_slot].capture;
        let entry = atlas.atlas().catalog().entry(hit.file).ok_or(AtlasSearchError::WrongAtlas)?;
        let label = entry.path().raw().to_path_buf();
        let mut stop = || canceled() || atlas.validate_active().is_err();
        let mut reader = ReaderSession::from_bytes(reader_owner, Path::new(&label), source.bytes(), &mut stop)?;
        let info = reader.info(&mut stop)?;
        let start = hit.original_range.start().get().saturating_sub(132);
        let end = hit.original_range.end().get().saturating_add(132).min(source.bytes().len() as u64);
        let window = reader.read_window(start, (end - start) as usize, &mut stop)?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"search_in_progress\":")?; out.boolean(snapshot.stop_reason.is_none())?;
        if let Some(usage) = snapshot.index_usage { usage.encode(&mut out)?; }
        out.literal(",\"hit_id\":")?; out.integer(hit.id)?;
        encode_hit(&mut out, atlas, hit)?;
        out.literal(",\"source_observation\":\"retained-search-capture\",\"source_reopened\":false,\"reader_owner\":")?;
        out.integer(reader_owner.get())?;
        out.literal(",\"reader\":")?; out.literal(info.as_str())?;
        out.literal(",\"window\":")?; out.literal(window.as_str())?;
        out.literal("}\n")?;
        let response = self.finish(atlas, out, false, &mut canceled)?;
        Ok((reader, response))
    }

    fn next_id(&mut self) -> Result<ResourceAllocationId, AtlasSearchError> {
        let value = self.next_allocation;
        self.next_allocation = value.checked_add(1).ok_or(AtlasSearchError::IdentityExhausted)?;
        ResourceAllocationId::new(value).map_err(|_| AtlasSearchError::IdentityExhausted)
    }
    fn output(&mut self, command: &str) -> Result<Output, AtlasSearchError> {
        let id = self.next_id()?;
        let mut out = Output::new(self.manifest.owner(), MAX_RESPONSE_BYTES, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.atlas-search/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.manifest.owner().get())?;
        out.literal(",\"source_manifest\":")?; out.integer(self.manifest.revision())?;
        out.literal(",\"layout_revision\":")?; out.integer(self.layout.get())?;
        Ok(out)
    }
    fn finish(&mut self, atlas: &AtlasSession, out: Output, partial: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
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
fn summary(out: &mut Output, atlas: &AtlasSession, snapshot: &Snapshot) -> Result<(), AtlasSearchError> {
    out.literal(",\"query_generation\":")?; out.integer(snapshot.generation)?;
    out.literal(",\"needle\":")?; out.quoted(&snapshot.needle)?;
    out.literal(",\"search_strategy\":")?;
    out.quoted(if snapshot.index_usage.is_some() { "retained-ephemeral-index" } else { "live-capture-scan" })?;
    if let Some(usage) = snapshot.index_usage { usage.encode(out)?; }
    out.literal(",\"mode\":\"exact-decoded-literal\",\"source_observation\":\"per-file-captures-not-atomic-workspace\",\"discovery_complete\":")?;
    out.boolean(atlas.atlas().discovery_complete())?;
    out.literal(",\"search_in_progress\":")?; out.boolean(snapshot.stop_reason.is_none())?;
    out.literal(",\"stop_reason\":")?;
    if let Some(reason) = snapshot.stop_reason { out.quoted(reason.code())?; } else { out.literal("null")?; }
    out.literal(",\"search_complete\":")?; out.boolean(snapshot.complete)?;
    out.literal(",\"truncated\":")?; out.boolean(snapshot.truncated)?;
    for (key, value) in [("catalogued_files", atlas.atlas().file_count() as u64),
        ("examined_files", snapshot.examined as u64), ("scanned_files", snapshot.scanned as u64),
        ("unavailable_files", snapshot.unavailable as u64), ("pending_files", snapshot.pending as u64),
        ("matches_seen", snapshot.matches_seen), ("retained_hits", snapshot.hits.len() as u64),
        ("matching_files", snapshot.files.len() as u64), ("source_bytes_read", snapshot.source_bytes_read),
        ("read_calls", snapshot.read_calls), ("retained_source_bytes", snapshot.retained_bytes as u64),
        ("step_count", snapshot.step_count), ("last_step_files", snapshot.last_step_files as u64),
        ("last_step_source_bytes", snapshot.last_step_bytes), ("last_step_read_calls", snapshot.last_step_calls)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    Ok(())
}
fn encode_page(out: &mut Output, atlas: &AtlasSession, snapshot: &Snapshot, start: usize,
    limit: usize, canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
    summary(out, atlas, snapshot)?;
    out.literal(",\"hits\":[")?;
    let end = start.saturating_add(limit).min(snapshot.hits.len());
    for (i, &hit) in snapshot.hits[start..end].iter().enumerate() {
        check(canceled)?;
        if i != 0 { out.literal(",")?; }
        out.literal("{\"hit_id\":")?; out.integer(hit.id)?;
        encode_hit(out, atlas, hit)?; out.literal("}")?;
    }
    out.literal("],\"next_offset\":")?;
    if end < snapshot.hits.len() { out.integer(end as u64)?; } else { out.literal("null")?; }
    out.literal(",\"diagnostics\":[")?;
    for (i, diagnostic) in snapshot.diagnostics.iter().enumerate() {
        check(canceled)?;
        if i != 0 { out.literal(",")?; }
        out.literal("{\"file_id\":")?; out.integer(diagnostic.file.get())?;
        out.literal(",\"reason\":")?; out.quoted(diagnostic.reason)?; out.literal("}")?;
    }
    out.literal("],\"diagnostics_truncated\":")?;
    out.boolean(snapshot.unavailable > snapshot.diagnostics.len())?;
    out.literal("}\n")?;
    Ok(())
}
fn encode_hit(out: &mut Output, atlas: &AtlasSession, hit: AtlasSearchHit) -> Result<(), AtlasSearchError> {
    out.literal(",\"node\":")?; out.integer(hit.node.ordinal() as u64)?;
    out.literal(",\"file_id\":")?; out.integer(hit.file.get())?;
    out.literal(",\"source_revision\":")?; out.integer(hit.revision.get())?;
    out.literal(",\"original_range\":")?; out.range(hit.original_range)?;
    out.literal(",\"path\":")?;
    out.path(&atlas.atlas().catalog().entry(hit.file).ok_or(AtlasSearchError::WrongAtlas)?.path().raw().to_path_buf())?;
    Ok(())
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
    if canceled() { Err(AtlasSearchError::Canceled) } else { Ok(()) }
}
fn reserve<T>(count: usize) -> Result<Vec<T>, AtlasSearchError> {
    let mut items = Vec::new(); items.try_reserve_exact(count).map_err(|_| AppError::Admission)?;
    if items.capacity() > count { return Err(AppError::Admission.into()); }
    Ok(items)
}
fn copy_text(text: &str) -> Result<String, AtlasSearchError> {
    let mut copy = String::new(); copy.try_reserve_exact(text.len()).map_err(|_| AppError::Admission)?;
    if copy.capacity() > text.len() { return Err(AppError::Admission.into()); }
    copy.push_str(text); Ok(copy)
}

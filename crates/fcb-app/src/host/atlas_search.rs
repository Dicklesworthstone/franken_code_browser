#![forbid(unsafe_code)]

//! Retained repository search for the native-host atlas. Search is explicit
//! worker work over its frozen catalog, not a camera callback or a new walk.
//! The existing capture and exact-text engines own I/O, decoding and matching.
//! Matching files remain captured until successful replacement or explicit clear;
//! result activation never reopens a live path. This is a bounded direct scan,
//! not a persistent index or an atomic cross-file filesystem snapshot.

use std::{mem::size_of, path::Path};
use fcb::{ArenaOwnerId, ByteLength, ByteRange, FileId, SourceRevision};
use fcb::map::{AtlasNodeId, LayoutRevision};
use fcb::search::{CaptureRequest, CompleteCapture, QueryGeneration, ReaderSearch,
    ResourceAllocationId, ResourceBudget, SearchManifestId, StreamReadOptions,
    StreamReadState, StreamReadStep, StreamingNeedle};
use fcb_core::ResourceLease;
use fcb::search::workspace::RootGrant;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL, MANAGED_BYTES, workspace};
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
    App(AppError), Atlas(AtlasSessionError), Reader(ReaderSessionError),
    InvalidLimits, WrongAtlas, StaleQuery, MissingQuery, MissingHit, Canceled,
    IdentityExhausted,
}
impl std::fmt::Display for AtlasSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "{e}"), Self::Atlas(e) => write!(f, "{e}"),
            Self::Reader(e) => write!(f, "{e}"),
            other => f.write_str(match other {
                Self::InvalidLimits => "ATLAS_SEARCH_INVALID_LIMITS",
                Self::WrongAtlas => "ATLAS_SEARCH_WRONG_ATLAS",
                Self::StaleQuery => "ATLAS_SEARCH_STALE_QUERY",
                Self::MissingQuery => "ATLAS_SEARCH_NO_QUERY",
                Self::MissingHit => "ATLAS_SEARCH_NO_HIT",
                Self::Canceled => "ATLAS_SEARCH_CANCELED",
                Self::IdentityExhausted => "ATLAS_SEARCH_IDENTITY_EXHAUSTED",
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

/// Occurrence IDs are local to one published query, not row positions in a UI.
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

/// One atlas and one accepted query snapshot, plus privately prepared replacement
/// work. The atlas owner must be fresh and never reused, as for AtlasSession.
/// No source is read by construction. Hosts call these methods on their worker.
/// This object and the atlas have independently bounded managed reservations;
/// their limits are not a claim about process RSS or a host's native copies.
pub struct RetainedAtlasSearch {
    manifest: SearchManifestId,
    grant: RootGrant,
    layout: LayoutRevision,
    last_attempt: u64,
    next_allocation: u64,
    accepted: Option<Snapshot>,
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
            next_allocation: 100, accepted: None, budget, _metadata_lease: metadata_lease })
    }
    pub fn accepted_generation(&self) -> Option<u64> { self.accepted.as_ref().map(|q| q.generation) }
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
        Ok(())
    }
    fn snapshot(&self, generation: u64) -> Result<&Snapshot, AtlasSearchError> {
        let query = self.accepted.as_ref().ok_or(AtlasSearchError::MissingQuery)?;
        if query.generation != generation { return Err(AtlasSearchError::StaleQuery); }
        Ok(query)
    }
    pub fn hit(&self, atlas: &AtlasSession, generation: u64, id: u64) -> Result<AtlasSearchHit, AtlasSearchError> {
        self.validate(atlas)?;
        let position = id.checked_sub(1).and_then(|n| usize::try_from(n).ok()).ok_or(AtlasSearchError::MissingHit)?;
        self.snapshot(generation)?.hits.get(position).copied().ok_or(AtlasSearchError::MissingHit)
    }

    /// Scan only this atlas's frozen eligible membership. A successful query
    /// atomically replaces rows AND their captures. Failure/cancellation preserves
    /// old rows under their old generation, while consuming the attempted ID.
    pub fn search(&mut self, atlas: &AtlasSession, generation: u64, needle: &str,
        options: AtlasSearchOptions, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?;
        self.attempt(generation)?;
        options.validate()?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES { return Err(AtlasSearchError::InvalidLimits); }
        check(&mut canceled)?;
        let [state_id, pattern_id, scan_id] = [self.next_id()?, self.next_id()?, self.next_id()?];
        let capacity = options.max_matches.min(options.max_files);
        // Captures are Arc-owned; reserve payload plus capture conversion overlap
        // before any read. The existing range reader/scanner reserve their own
        // transient work in this SAME budget. Old accepted snapshots stay charged.
        let charge = 2 * options.max_source_bytes + capacity * (size_of::<RetainedFile>() + 64)
            + options.max_matches * size_of::<AtlasSearchHit>()
            + MAX_DIAGNOSTICS * size_of::<Unavailable>() + needle.len() + size_of::<Snapshot>();
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), state_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut candidate = Snapshot { generation, needle: copy_text(needle)?, files: reserve(capacity)?,
            hits: reserve(options.max_matches)?, diagnostics: reserve(MAX_DIAGNOSTICS)?,
            examined: 0, scanned: 0, unavailable: 0, pending: 0, matches_seen: 0,
            source_bytes_read: 0, read_calls: 0, retained_bytes: 0, truncated: false,
            complete: false, _lease: lease };
        let pattern = StreamingNeedle::text(self.manifest.owner(), needle, &self.budget, pattern_id).map_err(AppError::from)?;
        let qgen = QueryGeneration::new(self.manifest.owner(), generation).map_err(|_| AtlasSearchError::IdentityExhausted)?;
        let revision = SourceRevision::new(self.manifest.owner(), generation).map_err(|_| AtlasSearchError::IdentityExhausted)?;
        let catalog = atlas.atlas().catalog();
        let root = catalog.grant().root_path().to_path_buf();
        let mut io = workspace::IoCounts::default();
        for (ordinal, entry) in catalog.entries().iter().enumerate() {
            self.validate(atlas)?; check(&mut canceled)?;
            if candidate.hits.len() == options.max_matches {
                candidate.truncated = true; break;
            }
            if candidate.examined == options.max_files || io.bytes >= options.max_source_bytes as u64 { break; }
            let file = catalog.file_id(ordinal).ok_or(AtlasSearchError::WrongAtlas)?;
            candidate.examined += 1;
            if entry.observed_bytes() > options.max_file_bytes as u64 {
                candidate.unavailable(file, "FILE_BYTE_LIMIT"); continue;
            }
            let request = CaptureRequest::new(file, revision).map_err(|_| AtlasSearchError::WrongAtlas)?;
            let mut stop = || canceled() || atlas.validate_active().is_err();
            let captured = workspace::read_capture(&root, request, entry.path(), options.max_file_bytes,
                options.max_source_bytes as u64, &mut io, &self.budget, &mut stop);
            self.validate(atlas)?; check(&mut canceled)?;
            let capture = match captured {
                Ok(capture) => capture,
                Err(fcb::source::SourceError::Canceled) => return Err(AtlasSearchError::Canceled),
                Err(error) => { candidate.unavailable(file, error.code()); continue; }
            };
            let node = atlas.atlas().node_for_file(file).map_err(AtlasSessionError::from)?;
            let before_hits = candidate.hits.len();
            {
                let mut scan_options = StreamReadOptions::new(qgen);
                scan_options.max_matches = options.max_matches - before_hits;
                scan_options.max_bytes = capture.bytes().len() as u64;
                let mut scan = ReaderSearch::new(capture.bytes(), *capture.request(),
                    ByteLength::new(capture.bytes().len() as u64), &pattern, scan_options,
                    &self.budget, scan_id).map_err(AppError::from)?;
                while scan.state() == StreamReadState::Pending {
                    scan.step(StreamReadStep::default(), qgen,
                        || canceled() || atlas.validate_active().is_err()).map_err(AppError::from)?;
                }
                let (_, report) = scan.finish().map_err(AppError::from)?;
                self.validate(atlas)?; check(&mut canceled)?;
                if report.state() == StreamReadState::Canceled { return Err(AtlasSearchError::Canceled); }
                if let StreamReadState::Failed(error) = report.state() { return Err(AppError::from(error).into()); }
                candidate.scanned += 1;
                candidate.matches_seen += report.matches_seen();
                if report.state() == StreamReadState::Truncated { candidate.truncated = true; }
                else if !report.input_complete() { candidate.unavailable(file, report.state().code()); }
                for hit in report.hits() {
                    candidate.hits.push(AtlasSearchHit { id: candidate.hits.len() as u64 + 1,
                        file, revision, node, original_range: hit.original_range(), source_slot: candidate.files.len() });
                }
            }
            let hits = candidate.hits.len() - before_hits;
            if hits != 0 {
                candidate.retained_bytes += capture.bytes().len();
                candidate.files.push(RetainedFile { capture, node, hits });
            }
        }
        candidate.pending = catalog.entries().len() - candidate.examined;
        candidate.source_bytes_read = io.bytes;
        candidate.read_calls = io.calls;
        candidate.complete = atlas.atlas().discovery_complete() && candidate.pending == 0
            && candidate.unavailable == 0 && !candidate.truncated;
        let mut out = self.output("search")?;
        encode_page(&mut out, atlas, &candidate, 0, 64, &mut canceled)?;
        let response = self.finish(atlas, out, !candidate.complete, &mut canceled)?;
        self.accepted = Some(candidate);
        Ok(response)
    }

    /// Paging never reads a source or recomputes a query. A page continuation is
    /// separate from search completeness and from result-storage truncation.
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
    /// Counts cover retained hits only. They are not whole-workspace totals when
    /// the query is incomplete, and do not establish a presented GPU frame.
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

    /// Prepare a camera plan for the exact accepted occurrence. No filesystem
    /// work or implicit presentation acknowledgment occurs; old picking remains
    /// authoritative until the host presents/acknowledges the new plan.
    pub fn focus_hit(&self, atlas: &mut AtlasSession, generation: u64, id: u64,
        plan_generation: u64, canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        let hit = self.hit(atlas, generation, id)?;
        Ok(atlas.prepare(plan_generation, AtlasAction::Focus(hit.node.ordinal()), canceled)?)
    }

    /// Copy the retained WHOLE matching capture into an independently owned
    /// reader. Source and reader identities are explicitly linked, not relabeled.
    /// No path open occurs, even after deletion, rename or same-size live edits.
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
    out.literal(",\"mode\":\"exact-decoded-literal\",\"source_observation\":\"per-file-captures-not-atomic-workspace\",\"discovery_complete\":")?;
    out.boolean(atlas.atlas().discovery_complete())?;
    out.literal(",\"search_complete\":")?; out.boolean(snapshot.complete)?;
    out.literal(",\"truncated\":")?; out.boolean(snapshot.truncated)?;
    for (key, value) in [("catalogued_files", atlas.atlas().file_count() as u64),
        ("examined_files", snapshot.examined as u64), ("scanned_files", snapshot.scanned as u64),
        ("unavailable_files", snapshot.unavailable as u64), ("pending_files", snapshot.pending as u64),
        ("matches_seen", snapshot.matches_seen), ("retained_hits", snapshot.hits.len() as u64),
        ("matching_files", snapshot.files.len() as u64), ("source_bytes_read", snapshot.source_bytes_read),
        ("read_calls", snapshot.read_calls), ("retained_source_bytes", snapshot.retained_bytes as u64)] {
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

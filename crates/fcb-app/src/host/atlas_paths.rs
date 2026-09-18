#![forbid(unsafe_code)]

//! Retained filename/path navigation over an existing atlas (FCB-025/029).
//! A lazily prepared PathIndex owns native keys and is reused across queries.
//! No source read, metadata refresh or repack occurs while finding/selecting
//! files. Opening a selected path is a separate, explicit NEW source capture;
//! it must never be confused with activating a captured content-search hit.

use std::mem::size_of;
use fcb::{ArenaOwnerId, ByteLength, FileId};
use fcb::map::{AtlasNodeId, LayoutRevision};
use fcb::search::{MembershipState, PathEntry, PathIndex, PathIndexLimits,
    PathRank, PathSearch, PathSearchError, PathSearchOptions, PathSelection,
    QueryGeneration, ResourceAllocationId, ResourceBudget, SearchManifestId};
pub use fcb::search::{PathCase, PathMatchMode};
use fcb::search::workspace::RootGrant;
use fcb_core::ResourceLease;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL, MANAGED_BYTES, workspace};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};
use super::{HostResponse, MAX_HOST_TEXT_BYTES, atlas_session::{AtlasAction, AtlasSession, AtlasSessionError},
    reader::{ReaderSession, ReaderSessionError}};

pub const MAX_ATLAS_PATH_RESULTS: usize = 4096;
pub const MAX_ATLAS_PATH_QUERY: usize = 256;
pub const MAX_ATLAS_PATH_PAGE: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasPathOptions {
    pub max_results: usize,
    pub case: PathCase,
    pub mode: PathMatchMode,
}
impl Default for AtlasPathOptions {
    fn default() -> Self {
        Self { max_results: 100, case: PathCase::UnicodeLowercase, mode: PathMatchMode::Fuzzy }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasPathError {
    App(AppError), Atlas(AtlasSessionError), Reader(ReaderSessionError),
    WrongAtlas, StaleQuery, MissingQuery, MissingHit, Canceled, IdentityExhausted,
}
impl std::fmt::Display for AtlasPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "{e}"), Self::Atlas(e) => write!(f, "{e}"),
            Self::Reader(e) => write!(f, "{e}"),
            Self::WrongAtlas => f.write_str("ATLAS_PATH_WRONG_ATLAS"),
            Self::StaleQuery => f.write_str("ATLAS_PATH_STALE_QUERY"),
            Self::MissingQuery => f.write_str("ATLAS_PATH_NO_QUERY"),
            Self::MissingHit => f.write_str("ATLAS_PATH_NO_HIT"),
            Self::Canceled => f.write_str("ATLAS_PATH_CANCELED"),
            Self::IdentityExhausted => f.write_str("ATLAS_PATH_IDENTITY_EXHAUSTED"),
        }
    }
}
impl std::error::Error for AtlasPathError {}
impl From<AppError> for AtlasPathError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<AtlasSessionError> for AtlasPathError { fn from(e: AtlasSessionError) -> Self { Self::Atlas(e) } }
impl From<ReaderSessionError> for AtlasPathError { fn from(e: ReaderSessionError) -> Self { Self::Reader(e) } }
impl From<PathSearchError> for AtlasPathError { fn from(e: PathSearchError) -> Self { Self::App(e.into()) } }
impl From<OutputError> for AtlasPathError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

/// Activation uses (query generation, file), never a ranked row position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasFileHit { pub file: FileId, pub node: AtlasNodeId, pub rank: PathRank }
struct Results {
    generation: u64, needle: Vec<u8>, options: AtlasPathOptions,
    hits: Vec<AtlasFileHit>, selected: Option<AtlasFileHit>,
    complete: bool, truncated: bool, matches_seen: usize,
    files_examined: usize, candidates_examined: usize, work_units: u64,
    _lease: ResourceLease,
}

/// Small inert owner; path-key preparation happens on the first explicit find.
/// The host must supply a fresh atlas owner. Grants are compared by token identity
/// as well as IDs, so another independently opened catalog cannot substitute.
/// All methods, including final destruction, are host-worker operations.
pub struct RetainedAtlasPaths {
    manifest: SearchManifestId, layout: LayoutRevision, grant: RootGrant,
    index: Option<PathIndex>, accepted: Option<Results>,
    last_attempt: u64, next_allocation: u64, budget: ResourceBudget,
    _lease: ResourceLease,
}
impl RetainedAtlasPaths {
    pub fn new(atlas: &AtlasSession) -> Result<Self, AtlasPathError> {
        atlas.validate_active()?;
        let catalog = atlas.atlas().catalog();
        let budget = ResourceBudget::new(catalog.id().owner(), ByteLength::new(MANAGED_BYTES))
            .map_err(|_| AppError::Admission)?;
        let lease = budget.try_reserve_managed(catalog.id().owner(),
            ResourceAllocationId::new(1).map_err(|_| AppError::Admission)?,
            ByteLength::new((size_of::<Self>() + 2 * catalog.grant().root_path().len()) as u64))
            .map_err(|_| AppError::Admission)?;
        Ok(Self { manifest: catalog.id(), layout: atlas.atlas().layout().revision(),
            grant: catalog.grant().clone(), index: None, accepted: None,
            last_attempt: 0, next_allocation: 2, budget, _lease: lease })
    }
    pub fn prepared_index(&self) -> Option<&PathIndex> { self.index.as_ref() }
    pub fn accepted_generation(&self) -> Option<u64> { self.accepted.as_ref().map(|r| r.generation) }
    fn validate(&self, atlas: &AtlasSession) -> Result<(), AtlasPathError> {
        atlas.validate_active()?;
        if atlas.atlas().catalog().id() != self.manifest || atlas.atlas().layout().revision() != self.layout
            || atlas.atlas().catalog().grant() != &self.grant { return Err(AtlasPathError::WrongAtlas); }
        Ok(())
    }
    fn attempt(&mut self, generation: u64) -> Result<(), AtlasPathError> {
        if generation == 0 || generation <= self.last_attempt { return Err(AtlasPathError::StaleQuery); }
        self.last_attempt = generation; Ok(())
    }
    fn results(&self, generation: u64) -> Result<&Results, AtlasPathError> {
        let results = self.accepted.as_ref().ok_or(AtlasPathError::MissingQuery)?;
        if results.generation != generation { return Err(AtlasPathError::StaleQuery); }
        Ok(results)
    }
    pub fn hits(&self, atlas: &AtlasSession, generation: u64) -> Result<&[AtlasFileHit], AtlasPathError> {
        self.validate(atlas)?; Ok(&self.results(generation)?.hits)
    }
    pub fn selected(&self, atlas: &AtlasSession, generation: u64) -> Result<Option<AtlasFileHit>, AtlasPathError> {
        self.validate(atlas)?; Ok(self.results(generation)?.selected)
    }
    pub fn hit(&self, atlas: &AtlasSession, generation: u64, file: FileId) -> Result<AtlasFileHit, AtlasPathError> {
        self.validate(atlas)?;
        let results = self.results(generation)?;
        results.hits.iter().find(|h| h.file == file).copied()
            .or_else(|| results.selected.filter(|h| h.file == file)).ok_or(AtlasPathError::MissingHit)
    }
    fn prepare_index(&mut self, atlas: &AtlasSession, canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasPathError> {
        if self.index.is_some() { return Ok(()); }
        let catalog = atlas.atlas().catalog();
        let count = catalog.entries().len();
        let scratch_id = self.next_id()?; let index_id = self.next_id()?;
        let _scratch = self.budget.try_reserve_managed(self.manifest.owner(), scratch_id,
            ByteLength::new((count * size_of::<PathEntry<'_>>() + size_of::<Vec<PathEntry<'_>>>()) as u64))
            .map_err(|_| AppError::Admission)?;
        let mut entries = reserve(count)?;
        for (ordinal, entry) in catalog.entries().iter().enumerate() {
            check(canceled)?;
            entries.push(PathEntry::new(catalog.file_id(ordinal).ok_or(AtlasPathError::WrongAtlas)?,
                self.grant.root_id(), entry.path().raw()));
        }
        let membership = if catalog.discovery_complete() { MembershipState::Closed } else { MembershipState::Discovering };
        let index = PathIndex::build(self.manifest, membership, &entries,
            PathIndexLimits { max_files: count, max_total_path_bytes: catalog.limits().max_total_path_bytes,
                ..PathIndexLimits::default() }, &self.budget, index_id,
            || canceled() || atlas.validate_active().is_err())?;
        self.validate(atlas)?; check(canceled)?;
        self.index = Some(index); Ok(())
    }
    /// Exact/prefix use the core component postings; fuzzy uses its bounded
    /// top-k ranker. Raw query bytes remain raw; UnicodeLowercase is NOT full
    /// case folding or normalization. Old rows survive failed replacement.
    pub fn find(&mut self, atlas: &AtlasSession, generation: u64, needle: &[u8],
        options: AtlasPathOptions, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasPathError> {
        self.validate(atlas)?; self.attempt(generation)?; check(&mut canceled)?;
        if needle.is_empty() { return Err(PathSearchError::EmptyQuery.into()); }
        if needle.len() > MAX_ATLAS_PATH_QUERY { return Err(PathSearchError::QueryTooLong.into()); }
        if !(1..=MAX_ATLAS_PATH_RESULTS).contains(&options.max_results) { return Err(PathSearchError::InvalidLimits.into()); }
        self.prepare_index(atlas, &mut canceled)?;
        let query_id = self.next_id()?; let result_id = self.next_id()?;
        let generation_id = QueryGeneration::new(self.manifest.owner(), generation).map_err(|_| AtlasPathError::IdentityExhausted)?;
        let mut query_options = PathSearchOptions::new(generation_id);
        query_options.case = options.case; query_options.mode = options.mode;
        query_options.max_results = options.max_results;
        query_options.selected_file = self.accepted.as_ref().and_then(|r| r.selected).map(|h| h.file);
        let mut query = PathSearch::new(self.index.as_ref().ok_or(AtlasPathError::MissingQuery)?, needle,
            query_options, &self.budget, query_id)?;
        query.run_to_completion(|| canceled() || atlas.validate_active().is_err())?;
        self.validate(atlas)?; check(&mut canceled)?;
        // Cancellation is latched by the engine even when the host callback
        // reports a one-shot pulse rather than a permanently set flag.
        if query.state() != fcb::search::PathSearchState::Finished { return Err(AtlasPathError::Canceled); }
        let count = query.ranked_matches().len();
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), result_id,
            ByteLength::new((size_of::<Results>() + needle.len() + count * size_of::<AtlasFileHit>()) as u64))
            .map_err(|_| AppError::Admission)?;
        let project = |file: FileId, rank: PathRank| -> Result<AtlasFileHit, AtlasPathError> {
            Ok(AtlasFileHit { file, rank, node: atlas.atlas().node_for_file(file).map_err(AtlasSessionError::from)? })
        };
        let mut hits = reserve(count)?;
        for hit in query.ranked_matches() {
            check(&mut canceled)?; hits.push(project(hit.file_id(), hit.rank())?);
        }
        // Preserve an explicitly selected match even if refinement's top-k omits
        // it. Missing/nonmatching selections become None, never another file.
        let selected = match query.selection() {
            PathSelection::Matched(hit) => Some(project(hit.file_id(), hit.rank())?), _ => None,
        };
        let mut raw = reserve(needle.len())?; raw.extend_from_slice(needle);
        let candidate = Results { generation, needle: raw, options, hits, selected,
            complete: query.is_complete(), truncated: query.truncated(), matches_seen: query.matches_seen(),
            files_examined: query.files_examined(), candidates_examined: query.candidates_examined(),
            work_units: query.work_units(), _lease: lease };
        drop(query);
        let mut out = self.output("find", false)?;
        encode_page(&mut out, atlas, &candidate, 0, 64, &mut canceled)?;
        let response = self.finish(atlas, out, !candidate.complete, &mut canceled)?;
        self.accepted = Some(candidate); Ok(response)
    }
    pub fn page(&mut self, atlas: &AtlasSession, generation: u64, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasPathError> {
        self.validate(atlas)?; check(&mut canceled)?;
        if !(1..=MAX_ATLAS_PATH_PAGE).contains(&limit) { return Err(PathSearchError::InvalidLimits.into()); }
        let mut out = self.output("page", false)?;
        let results = self.results(generation)?;
        if start > results.hits.len() { return Err(PathSearchError::InvalidLimits.into()); }
        encode_page(&mut out, atlas, results, start, limit, &mut canceled)?;
        let partial = !results.complete;
        self.finish(atlas, out, partial, &mut canceled)
    }
    /// Selection alone performs neither camera movement nor source capture.
    pub fn select(&mut self, atlas: &AtlasSession, generation: u64, file: FileId,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasPathError> {
        let hit = self.hit(atlas, generation, file)?; check(&mut canceled)?;
        let mut out = self.output("select", false)?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"selection\":")?; encode_hit(&mut out, atlas, hit)?; out.literal("}\n")?;
        let response = self.finish(atlas, out, false, &mut canceled)?;
        if let Some(results) = self.accepted.as_mut() { results.selected = Some(hit); }
        Ok(response)
    }
    pub fn clear(&mut self, atlas: &AtlasSession, generation: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasPathError> {
        self.validate(atlas)?; self.attempt(generation)?; check(&mut canceled)?;
        let mut out = self.output("clear", false)?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"retained_hits\":\"0\"}\n")?;
        let response = self.finish(atlas, out, false, &mut canceled)?;
        self.accepted = None; // Keep the prepared keys for the next query.
        Ok(response)
    }
    pub fn focus_hit(&self, atlas: &mut AtlasSession, generation: u64, file: FileId,
        plan_generation: u64, canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasPathError> {
        let hit = self.hit(atlas, generation, file)?;
        Ok(atlas.prepare(plan_generation, AtlasAction::Focus(hit.node.ordinal()), canceled)?)
    }
    /// A path hit never claimed to capture source. Open its currently authorized
    /// native path, refusing symlinks under the SAME application policy as atlas
    /// activation. No rendered display label or caller-provided path is trusted.
    /// Pathname checks do not qualify hostile ancestor-race confinement.
    pub fn open_reader(&mut self, atlas: &AtlasSession, reader_owner: ArenaOwnerId,
        generation: u64, file: FileId, max_bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), AtlasPathError> {
        let hit = self.hit(atlas, generation, file)?; check(&mut canceled)?;
        if reader_owner == self.manifest.owner() { return Err(AtlasPathError::WrongAtlas); }
        if !(1..=MAX_HOST_TEXT_BYTES).contains(&max_bytes) { return Err(AppError::InputLimit.into()); }
        let mut out = self.output("open-reader", true)?;
        let entry = atlas.atlas().catalog().entry(file).ok_or(AtlasPathError::MissingHit)?;
        let root = self.grant.root_path().to_path_buf();
        let path = workspace::checked_source_path(&root, entry.path()).map_err(|_| AppError::SourceChanged)?;
        let mut stop = || canceled() || atlas.validate_active().is_err();
        let mut reader = ReaderSession::open(reader_owner, &path, max_bytes, &mut stop)?;
        let info = reader.info(&mut stop)?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"target\":")?; encode_hit(&mut out, atlas, hit)?;
        out.literal(",\"source_observation\":\"new-capture-after-path-selection\",\"reader_owner\":")?;
        out.integer(reader_owner.get())?;
        out.literal(",\"reader\":")?; out.literal(info.as_str())?; out.literal("}\n")?;
        let response = self.finish(atlas, out, false, &mut canceled)?;
        Ok((reader, response))
    }
    fn next_id(&mut self) -> Result<ResourceAllocationId, AtlasPathError> {
        let id = self.next_allocation;
        self.next_allocation = id.checked_add(1).ok_or(AtlasPathError::IdentityExhausted)?;
        ResourceAllocationId::new(id).map_err(|_| AtlasPathError::IdentityExhausted)
    }
    fn output(&mut self, command: &str, source_read: bool) -> Result<Output, AtlasPathError> {
        let id = self.next_id()?;
        let mut out = Output::new(self.manifest.owner(), MAX_RESPONSE_BYTES, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.atlas-paths/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.manifest.owner().get())?;
        out.literal(",\"source_manifest\":")?; out.integer(self.manifest.revision())?;
        out.literal(",\"layout_revision\":")?; out.integer(self.layout.get())?;
        out.literal(",\"source_payload_read\":")?; out.boolean(source_read)?;
        Ok(out)
    }
    fn finish(&mut self, atlas: &AtlasSession, out: Output, partial: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasPathError> {
        self.validate(atlas)?; check(canceled)?;
        let id = self.next_id()?;
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), id,
            ByteLength::new((3 * out.as_bytes().len() + 4096) as u64)).map_err(|_| AppError::Admission)?;
        let mut text = String::new(); text.try_reserve_exact(out.as_bytes().len()).map_err(|_| AppError::Admission)?;
        if text.capacity() > out.as_bytes().len() { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        self.validate(atlas)?; check(canceled)?;
        Ok(HostResponse { text, exit_code: if partial { EXIT_PARTIAL } else { EXIT_OK }, _lease: lease })
    }
}
fn encode_page(out: &mut Output, atlas: &AtlasSession, results: &Results, start: usize, limit: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasPathError> {
    out.literal(",\"query_generation\":")?; out.integer(results.generation)?;
    out.literal(",\"query_hex\":")?; out.hex(&results.needle)?;
    out.literal(",\"mode\":")?; out.quoted(match results.options.mode { PathMatchMode::Exact => "exact", PathMatchMode::Prefix => "prefix", PathMatchMode::Fuzzy => "fuzzy" })?;
    out.literal(",\"case\":")?; out.quoted(match results.options.case { PathCase::Sensitive => "sensitive", PathCase::UnicodeLowercase => "unicode-lowercase" })?;
    out.literal(",\"search_complete\":")?; out.boolean(results.complete)?;
    out.literal(",\"truncated\":")?; out.boolean(results.truncated)?;
    for (key, value) in [("matches_seen", results.matches_seen as u64), ("retained_hits", results.hits.len() as u64),
        ("files_examined", results.files_examined as u64), ("candidates_examined", results.candidates_examined as u64),
        ("work_units", results.work_units)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    out.literal(",\"selection\":")?;
    match results.selected { Some(hit) => encode_hit(out, atlas, hit)?, None => out.literal("null")? }
    out.literal(",\"hits\":[")?;
    let end = start.saturating_add(limit).min(results.hits.len());
    for (i, &hit) in results.hits[start..end].iter().enumerate() {
        check(canceled)?; if i != 0 { out.literal(",")?; } encode_hit(out, atlas, hit)?;
    }
    out.literal("],\"next_offset\":")?;
    if end < results.hits.len() { out.integer(end as u64)?; } else { out.literal("null")?; }
    out.literal("}\n")?; Ok(())
}
fn encode_hit(out: &mut Output, atlas: &AtlasSession, hit: AtlasFileHit) -> Result<(), AtlasPathError> {
    use fcb::search::PathMatchKind::*;
    let kind = match hit.rank.kind { ExactFilename => "exact-filename", ExactPath => "exact-path",
        ExactComponent => "exact-component", FilenamePrefix => "filename-prefix", ComponentPrefix => "component-prefix",
        PathPrefix => "path-prefix", FilenameSubsequence => "filename-subsequence", PathSubsequence => "path-subsequence" };
    out.literal("{\"file_id\":")?; out.integer(hit.file.get())?;
    out.literal(",\"node\":")?; out.integer(hit.node.ordinal() as u64)?;
    out.literal(",\"match_kind\":")?; out.quoted(kind)?;
    out.literal(",\"path\":")?;
    out.path(&atlas.atlas().catalog().entry(hit.file).ok_or(AtlasPathError::MissingHit)?.path().raw().to_path_buf())?;
    out.literal("}")?; Ok(())
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasPathError> {
    if canceled() { Err(AtlasPathError::Canceled) } else { Ok(()) }
}
fn reserve<T>(count: usize) -> Result<Vec<T>, AtlasPathError> {
    let mut values = Vec::new(); values.try_reserve_exact(count).map_err(|_| AppError::Admission)?;
    if values.capacity() > count { return Err(AppError::Admission.into()); }
    Ok(values)
}

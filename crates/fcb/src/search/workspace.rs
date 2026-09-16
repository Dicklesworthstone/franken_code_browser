#![forbid(unsafe_code)]

//! Bounded, ephemeral workspace composition through the existing source and
//! search engines. No runtime, persistent store, shell, parser or second matcher.
//!
//! Discovery is metadata-only with an explicitly named static exclusion policy;
//! nested rule files are NOT loaded. A completed traversal is an observation,
//! not an atomic filesystem snapshot. Missing metadata/captures and quota stops
//! prevent workspace completeness. Symlinks/special objects are outside scope.
//!
//! Native opening is supplied by the authorized host. A capture callback runs
//! on a worker, must respect its byte allowance and cancellation, and must not
//! turn a prefix into a CompleteCapture. Navigation later reads retained bytes,
//! not the live path. Directory enumeration's path checks are not a sandbox
//! against hostile concurrent ancestor replacement.

use std::mem::size_of;
use fcb_core::{ByteLength, FileId, ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_source::{CancelFlag, CaptureRequest, CompleteCapture, SourceError};
use fcb_source::confined::SymlinkPolicy;
use fcb_source::discovery::{BoundedDiscovery, DiscoveryAggregate, DiscoveryKind, DiscoveryLimits};
use fcb_source::ignore::IgnoreMatcher;
use fcb_source::path::NormalizedPath;
pub use fcb_source::root::RootGrant;
use super::{EphemeralIndex, IndexError, IndexLimits, ManifestLimits, MembershipState,
    SearchDocument, SearchManifest, SearchManifestId};

const DISCOVERY_SCRATCH: usize = 16 * 1024 * 1024;
pub const MAX_WORKSPACE_FILES: usize = 65_536;
pub const MAX_WORKSPACE_PATH_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceLimits {
    pub max_files: usize,
    pub max_total_path_bytes: usize,
    pub max_discovery_pages: usize,
    pub max_file_bytes: usize,
    pub max_source_bytes: usize,
}
impl Default for WorkspaceLimits {
    fn default() -> Self {
        Self { max_files: 4096, max_total_path_bytes: 512 * 1024,
            max_discovery_pages: 4096, max_file_bytes: 1024 * 1024,
            max_source_bytes: 32 * 1024 * 1024 }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceStage { Discovering, Ready, Canceled }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceLimit { Files, Paths, DiscoveryPages }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceCaptureFailure { FileLimit, SourceLimit, Source(SourceError), InvalidCapture }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WorkspaceError {
    Source(SourceError), Index(IndexError), InvalidLimits, OwnerMismatch,
    IdentityExhausted, AllocationFailed, ResourceDenied, Pending, Canceled, DuplicatePath,
}
impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(error) => write!(f, "{error}"), Self::Index(error) => write!(f, "{error}"),
            other => f.write_str(match other {
                Self::InvalidLimits => "WORKSPACE_INVALID_LIMITS", Self::OwnerMismatch => "WORKSPACE_OWNER_MISMATCH",
                Self::IdentityExhausted => "WORKSPACE_IDENTITY_EXHAUSTED", Self::AllocationFailed => "WORKSPACE_ALLOCATION_FAILED",
                Self::ResourceDenied => "WORKSPACE_RESOURCE_DENIED", Self::Pending => "WORKSPACE_PENDING",
                Self::Canceled => "WORKSPACE_CANCELED", Self::DuplicatePath => "WORKSPACE_DUPLICATE_PATH",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for WorkspaceError {}
impl From<SourceError> for WorkspaceError { fn from(error: SourceError) -> Self { Self::Source(error) } }
impl From<IndexError> for WorkspaceError { fn from(error: IndexError) -> Self { Self::Index(error) } }

#[derive(Debug)]
pub struct WorkspaceEntry {
    path: NormalizedPath,
    observed_bytes: u64,
    // A UTF-8 query key, not native authority. Raw identities remain separate.
    search_key: String,
}
impl WorkspaceEntry {
    pub fn path(&self) -> &NormalizedPath { &self.path }
    pub const fn observed_bytes(&self) -> u64 { self.observed_bytes }
}

pub struct WorkspaceCatalog {
    grant: RootGrant,
    id: SearchManifestId,
    first_file: FileId,
    limits: WorkspaceLimits,
    include_excluded: bool,
    discovery: Option<BoundedDiscovery>,
    entries: Vec<WorkspaceEntry>,
    stage: WorkspaceStage,
    aggregate: DiscoveryAggregate,
    pages: usize,
    path_bytes: usize,
    discovery_complete: bool,
    limit: Option<WorkspaceLimit>,
    discovery_error: Option<SourceError>,
    _lease: ResourceLease,
}
impl WorkspaceCatalog {
    /// Caller supplies a fresh manifest identity and a reserved FileId interval.
    /// IDs are assigned AFTER raw-path sorting; complete scans are independent
    /// of filesystem enumeration order. IDs are not persisted across scans.
    pub fn open(grant: RootGrant, id: SearchManifestId, first_file: FileId,
        limits: WorkspaceLimits, include_excluded: bool,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, WorkspaceError> {
        grant.validate_active()?;
        if id.owner() != grant.owner() || first_file.owner() != grant.owner() { return Err(WorkspaceError::OwnerMismatch); }
        if !(1..=MAX_WORKSPACE_FILES).contains(&limits.max_files)
            || limits.max_total_path_bytes > 16 * 1024 * 1024
            || !(1..=65_536).contains(&limits.max_discovery_pages)
            || limits.max_file_bytes > 1024 * 1024 || limits.max_source_bytes > 512 * 1024 * 1024
            || grant.root_path().len() > 16_384 { return Err(WorkspaceError::InvalidLimits); }
        first_file.get().checked_add(limits.max_files as u64 - 1).ok_or(WorkspaceError::IdentityExhausted)?;
        // Charge path payload, segment containers/Arc headers, escaped keys,
        // worst admitted walker queue/page scratch and geometric overlap. This
        // is deliberately conservative; OS directory internals remain external.
        let charge = limits.max_total_path_bytes.checked_mul(64)
            .and_then(|n| limits.max_files.checked_mul(size_of::<WorkspaceEntry>()).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(DISCOVERY_SCRATCH + size_of::<Self>())).ok_or(WorkspaceError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(grant.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| WorkspaceError::ResourceDenied)?;
        let entries = reserve(limits.max_files)?;
        let discovery_limits = DiscoveryLimits::new(1, 32, MAX_WORKSPACE_PATH_BYTES as u32, 64, 16 * 1024, 64)?;
        let ignore = if include_excluded { IgnoreMatcher::include_all() } else { IgnoreMatcher::product_defaults() };
        let discovery = BoundedDiscovery::open_metadata_only(grant.clone(), SymlinkPolicy::DisallowAll, discovery_limits, ignore)?;
        Ok(Self { grant, id, first_file, limits, include_excluded, discovery: Some(discovery), entries,
            stage: WorkspaceStage::Discovering, aggregate: DiscoveryAggregate::default(), pages: 0,
            path_bytes: 0, discovery_complete: false, limit: None, discovery_error: None, _lease: lease })
    }
    pub const fn id(&self) -> SearchManifestId { self.id }
    pub fn grant(&self) -> &RootGrant { &self.grant }
    pub const fn stage(&self) -> WorkspaceStage { self.stage }
    pub const fn limits(&self) -> WorkspaceLimits { self.limits }
    pub fn entries(&self) -> &[WorkspaceEntry] { &self.entries }
    pub const fn aggregate(&self) -> DiscoveryAggregate { self.aggregate }
    pub const fn discovery_pages(&self) -> usize { self.pages }
    pub const fn discovery_complete(&self) -> bool { self.discovery_complete }
    pub const fn stopped_by_limit(&self) -> Option<WorkspaceLimit> { self.limit }
    pub const fn discovery_error(&self) -> Option<SourceError> { self.discovery_error }
    pub fn policy_name(&self) -> &'static str {
        if self.include_excluded { "all-regular-files/no-rule-files-v1" } else { "product-defaults/no-rule-files-v1" }
    }
    pub fn validate_active(&self) -> Result<(), WorkspaceError> { self.grant.validate_active().map_err(Into::into) }
    pub fn file_id(&self, ordinal: usize) -> Option<FileId> {
        if self.stage != WorkspaceStage::Ready || ordinal >= self.entries.len() { return None; }
        FileId::new(self.grant.owner(), self.first_file.get().checked_add(ordinal as u64)?).ok()
    }
    pub fn entry(&self, file: FileId) -> Option<&WorkspaceEntry> {
        if file.owner() != self.grant.owner() { return None; }
        let ordinal = usize::try_from(file.get().checked_sub(self.first_file.get())?).ok()?;
        self.entries.get(ordinal)
    }
    pub fn cancel(&mut self) { self.stage = WorkspaceStage::Canceled; self.discovery = None; }

    /// One existing walker page, at most 256 traversal transitions. Empty pages
    /// are progress, not EOF. Sorting at the final publication is bounded by the
    /// admitted catalog size and is explicit worker work, never a UI callback.
    pub fn step(&mut self, canceled: &CancelFlag) -> Result<WorkspaceStage, WorkspaceError> {
        if canceled.is_canceled() { self.cancel(); return Err(WorkspaceError::Canceled); }
        if let Err(error) = self.grant.validate_active() { self.cancel(); return Err(error.into()); }
        if self.stage != WorkspaceStage::Discovering { return Ok(self.stage); }
        if self.pages == self.limits.max_discovery_pages {
            self.limit = Some(WorkspaceLimit::DiscoveryPages);
            return self.freeze(false);
        }
        self.pages += 1;
        let discovery = self.discovery.as_mut().ok_or(WorkspaceError::Pending)?;
        let batch = match discovery.next_batch(canceled) {
            Ok(batch) => batch,
            Err(SourceError::Canceled) => { self.cancel(); return Err(WorkspaceError::Canceled); }
            Err(SourceError::GrantRevoked) => { self.cancel(); return Err(SourceError::GrantRevoked.into()); }
            Err(error) => {
                self.discovery_error = Some(error);
                return self.freeze(false);
            }
        };
        self.aggregate = discovery.aggregate();
        let done = batch.as_ref().is_none_or(|batch| !batch.more());
        let closed = done && discovery.is_complete();
        if let Some(batch) = batch {
            for entry in batch.entries() {
                if entry.kind() != DiscoveryKind::File || entry.is_excluded() { continue; }
                if self.entries.len() == self.limits.max_files {
                    self.limit = Some(WorkspaceLimit::Files); return self.freeze(false);
                }
                let bytes = entry.path().as_bytes().len();
                if bytes > self.limits.max_total_path_bytes.saturating_sub(self.path_bytes) {
                    self.limit = Some(WorkspaceLimit::Paths); return self.freeze(false);
                }
                self.path_bytes += bytes;
                let search_key = match entry.path().as_str() {
                    Ok(path) => path.to_owned(),
                    Err(_) => entry.path().display_escaped().to_string(),
                };
                self.entries.push(WorkspaceEntry { path: entry.path().clone(),
                    observed_bytes: entry.observed_len().unwrap_or(0), search_key });
            }
        }
        if canceled.is_canceled() { self.cancel(); return Err(WorkspaceError::Canceled); }
        self.grant.validate_active()?;
        if done { self.freeze(closed) } else { Ok(self.stage) }
    }
    fn freeze(&mut self, closed: bool) -> Result<WorkspaceStage, WorkspaceError> {
        self.entries.sort_unstable_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        if self.entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
            self.cancel(); return Err(WorkspaceError::DuplicatePath);
        }
        let a = self.aggregate;
        self.discovery_complete = closed && self.limit.is_none() && self.discovery_error.is_none()
            && a.unavailable == 0 && a.cycles == 0 && a.depth_limited == 0 && a.path_limited == 0 && a.queue_refused == 0;
        self.discovery = None;
        self.stage = WorkspaceStage::Ready;
        Ok(self.stage)
    }
}

enum CaptureSlot { Pending, Ready(CompleteCapture), Unavailable(WorkspaceCaptureFailure) }

/// Complete captures for the discovered file set. A rejected large file has an
/// unavailable identity; it is never truncated into a purported whole capture.
/// The callback's old/new buffers are separately admitted by its host. The lease
/// below charges retained payload and bounded source-to-Arc copy overlap.
pub struct WorkspaceCaptures<'catalog> {
    catalog: &'catalog WorkspaceCatalog,
    first_revision: SourceRevision,
    slots: Vec<CaptureSlot>,
    next: usize,
    bytes: usize,
    canceled: bool,
    _lease: ResourceLease,
}
impl<'catalog> WorkspaceCaptures<'catalog> {
    pub fn new(catalog: &'catalog WorkspaceCatalog, first_revision: SourceRevision,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, WorkspaceError> {
        catalog.validate_active()?;
        if catalog.stage != WorkspaceStage::Ready { return Err(WorkspaceError::Pending); }
        if first_revision.owner() != catalog.grant.owner() { return Err(WorkspaceError::OwnerMismatch); }
        first_revision.get().checked_add(catalog.entries.len().saturating_sub(1) as u64)
            .ok_or(WorkspaceError::IdentityExhausted)?;
        let charge = catalog.limits.max_source_bytes.checked_mul(2)
            .and_then(|n| catalog.entries.len().checked_mul(size_of::<CaptureSlot>() + 32).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(size_of::<Self>())).ok_or(WorkspaceError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(catalog.grant.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| WorkspaceError::ResourceDenied)?;
        let mut slots = reserve(catalog.entries.len())?;
        slots.resize_with(catalog.entries.len(), || CaptureSlot::Pending);
        Ok(Self { catalog, first_revision, slots, next: 0, bytes: 0, canceled: false, _lease: lease })
    }
    pub fn catalog(&self) -> &'catalog WorkspaceCatalog { self.catalog }
    pub const fn captured_bytes(&self) -> usize { self.bytes }
    pub const fn examined_files(&self) -> usize { self.next }
    pub fn finished(&self) -> bool { !self.canceled && self.next == self.slots.len() }
    pub fn failure(&self, ordinal: usize) -> Option<WorkspaceCaptureFailure> {
        match self.slots.get(ordinal)? { CaptureSlot::Unavailable(reason) => Some(*reason), _ => None }
    }
    pub fn capture(&self, file: FileId) -> Option<&CompleteCapture> {
        self.catalog.entry(file)?;
        let ordinal = usize::try_from(file.get().checked_sub(self.catalog.first_file.get())?).ok()?;
        match self.slots.get(ordinal)? { CaptureSlot::Ready(capture) => Some(capture), _ => None }
    }
    pub fn cancel(&mut self) { self.canceled = true; }

    /// At most one host capture. Large synchronous host/OS operations have their
    /// own work contract; this method does not invent a wall-clock deadline.
    pub fn step(&mut self, cancel: &CancelFlag,
        mut capture: impl FnMut(CaptureRequest, &NormalizedPath, usize) -> Result<CompleteCapture, SourceError>)
        -> Result<bool, WorkspaceError> {
        if self.canceled || cancel.is_canceled() { self.cancel(); return Err(WorkspaceError::Canceled); }
        self.catalog.validate_active()?;
        if self.finished() { return Ok(true); }
        let entry = &self.catalog.entries[self.next];
        let file = self.catalog.file_id(self.next).ok_or(WorkspaceError::IdentityExhausted)?;
        let revision = SourceRevision::new(file.owner(), self.first_revision.get() + self.next as u64)
            .map_err(|_| WorkspaceError::IdentityExhausted)?;
        let request = CaptureRequest::new(file, revision)?;
        let remaining = self.catalog.limits.max_source_bytes - self.bytes;
        let allowance = self.catalog.limits.max_file_bytes.min(remaining);
        let slot = if entry.observed_bytes > self.catalog.limits.max_file_bytes as u64 {
            CaptureSlot::Unavailable(WorkspaceCaptureFailure::FileLimit)
        } else if entry.observed_bytes > remaining as u64 {
            CaptureSlot::Unavailable(WorkspaceCaptureFailure::SourceLimit)
        } else {
            match capture(request, &entry.path, allowance) {
                Ok(captured) if captured.request() == request && captured.bytes().len() <= allowance => {
                    self.bytes += captured.bytes().len();
                    CaptureSlot::Ready(captured)
                }
                Ok(_) => CaptureSlot::Unavailable(WorkspaceCaptureFailure::InvalidCapture),
                Err(SourceError::Canceled) => { self.cancel(); return Err(WorkspaceError::Canceled); }
                Err(error) => CaptureSlot::Unavailable(WorkspaceCaptureFailure::Source(error)),
            }
        };
        if cancel.is_canceled() { self.cancel(); return Err(WorkspaceError::Canceled); }
        self.catalog.validate_active()?;
        self.slots[self.next] = slot;
        self.next += 1;
        Ok(self.finished())
    }

    /// Separate owning input vectors avoid a self-referential index. Keep these
    /// alive while indexed queries run. Partial capture sessions remain usable;
    /// pending slots are unavailable, never zero-length fake documents.
    pub fn search_inputs(&self, budget: &ResourceBudget, allocation: ResourceAllocationId)
        -> Result<WorkspaceSearchInputs<'_>, WorkspaceError> {
        self.catalog.validate_active()?;
        let count = self.slots.len();
        let charge = count.checked_mul(size_of::<SearchDocument<'_>>() + size_of::<FileId>())
            .and_then(|n| n.checked_add(size_of::<WorkspaceSearchInputs<'_>>())).ok_or(WorkspaceError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(self.catalog.grant.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| WorkspaceError::ResourceDenied)?;
        let mut documents = reserve(count)?;
        let mut unavailable = reserve(count)?;
        for (ordinal, slot) in self.slots.iter().enumerate() {
            let file = self.catalog.file_id(ordinal).ok_or(WorkspaceError::IdentityExhausted)?;
            match slot {
                CaptureSlot::Ready(capture) => documents.push(SearchDocument::new(file, &self.catalog.entries[ordinal].search_key, capture)),
                _ => unavailable.push(file),
            }
        }
        let membership = if self.catalog.discovery_complete && self.finished() { MembershipState::Closed }
            else { MembershipState::Discovering };
        Ok(WorkspaceSearchInputs { catalog: self.catalog, documents, unavailable, membership, _lease: lease })
    }
}

pub struct WorkspaceSearchInputs<'source> {
    catalog: &'source WorkspaceCatalog,
    documents: Vec<SearchDocument<'source>>,
    unavailable: Vec<FileId>,
    membership: MembershipState,
    _lease: ResourceLease,
}
impl WorkspaceSearchInputs<'_> {
    /// UTF-8 search keys preserve ordinary paths; non-UTF8 keys are escaped.
    /// They are filter keys ONLY. Use catalog.entry(hit.file_id).path() for
    /// native lookup/navigation and reversible interchange, never the key.
    pub fn manifest(&self) -> Result<SearchManifest<'_>, WorkspaceError> {
        self.catalog.validate_active()?;
        Ok(SearchManifest::new(self.catalog.id, &self.documents, &self.unavailable, self.membership,
            ManifestLimits { max_files: self.catalog.limits.max_files, max_path_bytes: 16_384 })?)
    }
    pub fn index(&self, limits: IndexLimits, budget: &ResourceBudget, allocation: ResourceAllocationId,
        canceled: impl FnMut() -> bool) -> Result<EphemeralIndex<'_>, WorkspaceError> {
        Ok(EphemeralIndex::build(self.manifest()?, limits, budget, allocation, canceled)?)
    }
}
fn reserve<T>(count: usize) -> Result<Vec<T>, WorkspaceError> {
    let mut items = Vec::new();
    items.try_reserve_exact(count).map_err(|_| WorkspaceError::AllocationFailed)?;
    if items.capacity() > count { return Err(WorkspaceError::ResourceDenied); }
    Ok(items)
}

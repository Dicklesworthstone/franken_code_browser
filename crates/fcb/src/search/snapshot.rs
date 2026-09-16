#![forbid(unsafe_code)]

//! Explicit saved-source export and offline search. The `snapshot` feature adds
//! this adapter; it does not make the metadata database/persistence service
//! available. No constructor reads/writes files, creates a runtime, or restores
//! a root grant. Archive IDs are never trusted: the host supplies fresh domains.

use std::{mem::size_of, sync::Arc};
use fcb_core::{ByteLength, FileId, ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use crate::SourceCapture;
use super::{CaptureRequest, CompleteCapture, EphemeralIndex, IndexError, IndexLimits,
    ManifestLimits, MembershipState, RawPath, ReaderError, ReaderLimits, SearchDocument,
    SearchManifest, SearchManifestId, SourceReader};
use super::workspace::{WorkspaceCaptures, WorkspaceError};
pub use fcb_store::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotError,
    SnapshotLimits, SnapshotView, MAX_SNAPSHOT_BYTES};
pub use fcb_store::Sha256Digest;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavedSourceError {
    Format(SnapshotError), Workspace(WorkspaceError), Index(IndexError),
    OwnerMismatch, IdentityExhausted, ResourceDenied, Canceled, MissingCapture,
}
impl std::fmt::Display for SavedSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format(error) => write!(f, "{error}"),
            Self::Workspace(error) => write!(f, "{error}"),
            Self::Index(error) => write!(f, "{error}"),
            Self::OwnerMismatch => f.write_str("SNAPSHOT_OWNER_MISMATCH"),
            Self::IdentityExhausted => f.write_str("SNAPSHOT_IDENTITY_EXHAUSTED"),
            Self::ResourceDenied => f.write_str("SNAPSHOT_RESOURCE_DENIED"),
            Self::Canceled => f.write_str("SNAPSHOT_CANCELED"),
            Self::MissingCapture => f.write_str("SNAPSHOT_MISSING_CAPTURE"),
        }
    }
}
impl std::error::Error for SavedSourceError {}
impl From<SnapshotError> for SavedSourceError { fn from(error: SnapshotError) -> Self { Self::Format(error) } }
impl From<WorkspaceError> for SavedSourceError { fn from(error: WorkspaceError) -> Self { Self::Workspace(error) } }
impl From<IndexError> for SavedSourceError { fn from(error: IndexError) -> Self { Self::Index(error) } }

/// Build an export from retained captures, not a second read of live source.
/// The caller must explicitly authorize its destination/publication separately.
/// Incomplete discovery and unavailable captures are preserved. Canceled or
/// still-running capture sessions cannot be quietly treated as a finished save.
pub fn export_workspace(captures: &WorkspaceCaptures<'_>, limits: SnapshotLimits,
    budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
    mut canceled: impl FnMut() -> bool) -> Result<SnapshotBytes, SavedSourceError> {
    let catalog = captures.catalog();
    catalog.validate_active()?;
    if !captures.finished() { return Err(WorkspaceError::Pending.into()); }
    let charge = catalog.entries().len().checked_mul(size_of::<SnapshotEntry<'_>>())
        .and_then(|n| n.checked_add(size_of::<Vec<SnapshotEntry<'_>>>()))
        .ok_or(SavedSourceError::ResourceDenied)?;
    let _scratch = budget.try_reserve_managed(catalog.id().owner(), allocations[0], ByteLength::new(charge as u64))
        .map_err(|_| SavedSourceError::ResourceDenied)?;
    let mut entries = reserve(catalog.entries().len())?;
    for (ordinal, entry) in catalog.entries().iter().enumerate() {
        if canceled() { return Err(SavedSourceError::Canceled); }
        let file = catalog.file_id(ordinal).ok_or(SavedSourceError::IdentityExhausted)?;
        let (observed_bytes, data) = match captures.capture(file) {
            Some(capture) => (capture.bytes().len() as u64, SnapshotData::Captured(capture.bytes())),
            None => (entry.observed_bytes(), SnapshotData::Unavailable(
                captures.file_failure(file).ok_or(SavedSourceError::MissingCapture)?.code())),
        };
        entries.push(SnapshotEntry { path: entry.path().as_bytes(), observed_bytes, data });
    }
    let bytes = SnapshotBytes::encode(catalog.id().owner(), catalog.discovery_complete(), catalog.policy_name(),
        &entries, limits, budget, allocations[1], &mut canceled)?;
    catalog.validate_active()?;
    if canceled() { return Err(SavedSourceError::Canceled); }
    Ok(bytes)
}

/// One rehydrated member. Native names are metadata only and cannot be used to
/// read a live filesystem without a separately authorized host operation.
pub struct SavedMember {
    file: FileId,
    path: RawPath,
    observed_bytes: u64,
    source: Option<SourceCapture>,
    capture: Option<CompleteCapture>,
    unavailable: Option<String>,
}
impl SavedMember {
    pub const fn file_id(&self) -> FileId { self.file }
    pub fn path(&self) -> &RawPath { &self.path }
    pub const fn observed_bytes(&self) -> u64 { self.observed_bytes }
    pub fn capture(&self) -> Option<&CompleteCapture> { self.capture.as_ref() }
    pub fn unavailable_reason(&self) -> Option<&str> { self.unavailable.as_deref() }
}

/// A private copy of the bounded archive's exact bytes, under fresh host IDs.
/// Releasing the input archive cannot affect these captures. Readers and search
/// inputs borrow this owner; no automatic source grant or live-root lookup exists.
pub struct SavedWorkspace {
    id: SearchManifestId,
    first_file: FileId,
    complete: bool,
    policy: String,
    digest: Sha256Digest,
    members: Vec<SavedMember>,
    _lease: ResourceLease,
}
impl SavedWorkspace {
    /// Hosts allocate non-reused intervals for both file and source identities.
    /// IDs are not read from disk; even valid checksums cannot alias a live host.
    pub fn restore(view: SnapshotView<'_>, id: SearchManifestId, first_file: FileId,
        first_revision: SourceRevision, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, SavedSourceError> {
        if id.owner() != first_file.owner() || id.owner() != first_revision.owner() {
            return Err(SavedSourceError::OwnerMismatch);
        }
        let last = view.len().saturating_sub(1) as u64;
        first_file.get().checked_add(last).ok_or(SavedSourceError::IdentityExhausted)?;
        first_revision.get().checked_add(last).ok_or(SavedSourceError::IdentityExhausted)?;
        let charge = view.path_bytes().checked_mul(16)
            .and_then(|n| n.checked_add(view.source_bytes()))
            .and_then(|n| view.len().checked_mul(size_of::<SavedMember>() + 256).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(size_of::<Self>() + view.policy().len()))
            .ok_or(SavedSourceError::ResourceDenied)?;
        if canceled() { return Err(SavedSourceError::Canceled); }
        let lease = budget.try_reserve_managed(id.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| SavedSourceError::ResourceDenied)?;
        let mut members = reserve(view.len())?;
        for (ordinal, entry) in view.entries().enumerate() {
            if canceled() { return Err(SavedSourceError::Canceled); }
            let entry = entry?;
            let file = FileId::new(id.owner(), first_file.get() + ordinal as u64)
                .map_err(|_| SavedSourceError::IdentityExhausted)?;
            let revision = SourceRevision::new(id.owner(), first_revision.get() + ordinal as u64)
                .map_err(|_| SavedSourceError::IdentityExhausted)?;
            let path = RawPath::from_bytes(entry.path);
            let (source, capture, unavailable) = match entry.data {
                SnapshotData::Captured(bytes) => {
                    let bytes: Arc<[u8]> = Arc::from(bytes);
                    let request = CaptureRequest::new(file, revision).map_err(|_| SavedSourceError::OwnerMismatch)?;
                    let capture = CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::clone(&bytes))
                        .map_err(|_| SnapshotError::InvalidMember)?;
                    let logical_path = match std::str::from_utf8(entry.path) {
                        Ok(path) => path.to_owned(), Err(_) => path.display_escaped().to_string(),
                    };
                    let source = SourceCapture { owner: id.owner(), file, revision, logical_path, bytes };
                    (Some(source), Some(capture), None)
                }
                SnapshotData::Unavailable(reason) => (None, None, Some(reason.to_owned())),
            };
            members.push(SavedMember { file, path, observed_bytes: entry.observed_bytes,
                source, capture, unavailable });
        }
        if canceled() { return Err(SavedSourceError::Canceled); }
        Ok(Self { id, first_file, complete: view.discovery_complete(), policy: view.policy().to_owned(),
            digest: view.digest(), members, _lease: lease })
    }
    pub const fn id(&self) -> SearchManifestId { self.id }
    pub const fn discovery_complete(&self) -> bool { self.complete }
    pub fn policy(&self) -> &str { &self.policy }
    pub const fn digest(&self) -> Sha256Digest { self.digest }
    pub fn members(&self) -> &[SavedMember] { &self.members }
    pub fn member(&self, file: FileId) -> Option<&SavedMember> {
        if file.owner() != self.id.owner() { return None; }
        let index = usize::try_from(file.get().checked_sub(self.first_file.get())?).ok()?;
        self.members.get(index)
    }
    pub fn reader(&self, file: FileId, limits: ReaderLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<SourceReader<'_>, ReaderError> {
        let source = self.member(file).and_then(|member| member.source.as_ref()).ok_or(ReaderError::StaleSource)?;
        SourceReader::new(source, limits, budget, allocation)
    }
    pub fn search_inputs(&self, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<SavedSearchInputs<'_>, SavedSourceError> {
        let charge = self.members.len().checked_mul(size_of::<SearchDocument<'_>>() + size_of::<FileId>())
            .and_then(|n| n.checked_add(size_of::<SavedSearchInputs<'_>>())).ok_or(SavedSourceError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(self.id.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| SavedSourceError::ResourceDenied)?;
        let mut documents = reserve(self.members.len())?;
        let mut unavailable = reserve(self.members.len())?;
        for member in &self.members {
            if canceled() { return Err(SavedSourceError::Canceled); }
            match (&member.source, &member.capture) {
                (Some(source), Some(capture)) => documents.push(SearchDocument::new(member.file, source.logical_path(), capture)),
                _ => unavailable.push(member.file),
            }
        }
        Ok(SavedSearchInputs { workspace: self, documents, unavailable, _lease: lease })
    }
}

pub struct SavedSearchInputs<'a> {
    workspace: &'a SavedWorkspace,
    documents: Vec<SearchDocument<'a>>,
    unavailable: Vec<FileId>,
    _lease: ResourceLease,
}
impl SavedSearchInputs<'_> {
    pub fn manifest(&self) -> Result<SearchManifest<'_>, SavedSourceError> {
        // This is completeness of the saved observed scope, NEVER freshness or
        // completeness of the current filesystem at the saved relative names.
        let membership = if self.workspace.complete { MembershipState::Closed } else { MembershipState::Discovering };
        Ok(SearchManifest::new(self.workspace.id, &self.documents, &self.unavailable, membership,
            ManifestLimits { max_files: self.workspace.members.len(), max_path_bytes: 16_384 * 8 })?)
    }
    pub fn index(&self, limits: IndexLimits, budget: &ResourceBudget, allocation: ResourceAllocationId,
        canceled: impl FnMut() -> bool) -> Result<EphemeralIndex<'_>, SavedSourceError> {
        Ok(EphemeralIndex::build(self.manifest()?, limits, budget, allocation, canceled)?)
    }
}
fn reserve<T>(count: usize) -> Result<Vec<T>, SavedSourceError> {
    let mut vector = Vec::new();
    vector.try_reserve_exact(count).map_err(|_| SavedSourceError::ResourceDenied)?;
    if vector.capacity() > count { return Err(SavedSourceError::ResourceDenied); }
    Ok(vector)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{ArenaOwnerId, ByteOffset, QueryGeneration};
    use super::super::{ParsedQuery, QueryOptions, ReadingTarget, ReadingSeekState, ReadingWindowOptions};
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(731).unwrap() }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap() }
    fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
    fn restore(entries: &[SnapshotEntry<'_>], complete: bool, budget: &ResourceBudget) -> SavedWorkspace {
        let bytes = SnapshotBytes::encode(owner(), complete, "test-v1", entries, SnapshotLimits::default(), budget, allocation(1), || false).unwrap();
        let view = SnapshotView::open(bytes.bytes(), SnapshotLimits::default(), || false).unwrap();
        SavedWorkspace::restore(view, SearchManifestId::new(owner(), 50).unwrap(), FileId::new(owner(), 100).unwrap(),
            SourceRevision::new(owner(), 200).unwrap(), budget, allocation(2), || false).unwrap()
    }
    #[test]
    fn restored_capture_search_and_reading_use_only_saved_utf16_bytes() {
        let mut raw = vec![0xff, 0xfe];
        for unit in "head\r\nbanana".encode_utf16() { raw.extend_from_slice(&unit.to_le_bytes()); }
        let budget = budget();
        let workspace = restore(&[SnapshotEntry { path: b"\xff\\name.rs", observed_bytes: raw.len() as u64,
            data: SnapshotData::Captured(&raw) }], true, &budget);
        raw.fill(0); // Destroy caller bytes after the snapshot has been restored.
        let member = &workspace.members()[0];
        assert_eq!(member.path().as_bytes(), b"\xff\\name.rs");
        let inputs = workspace.search_inputs(&budget, allocation(3), || false).unwrap();
        let index = inputs.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
        let generation = QueryGeneration::new(owner(), 9).unwrap();
        let report = index.search(&ParsedQuery::parse("ana").unwrap(), QueryOptions::new(generation),
            &budget, allocation(5), || false).unwrap();
        assert!(report.is_complete()); assert_eq!(report.capture_results().matches.len(), 2);
        let hit = &report.capture_results().matches[0];
        assert_eq!(hit.file_id.get(), 100); assert_eq!(hit.revision.get(), 200);
        assert_eq!(hit.original_byte_range.start().get(), 16);
        let reader = workspace.reader(member.file_id(), ReaderLimits::default(), &budget, allocation(6)).unwrap();
        let seek = reader.seek(ReadingTarget::Byte(ByteOffset::new(0)), generation).unwrap();
        let ReadingSeekState::Ready(at) = seek.state() else { panic!("start must be ready") };
        let window = reader.window(at, generation, ReadingWindowOptions::default(), &budget, allocation(7), || false).unwrap();
        assert_eq!(window.line_text(0), Some("head")); assert_eq!(window.line_text(1), Some("banana"));
    }
    #[test]
    fn unavailable_and_incomplete_discovery_cannot_be_complete_negative_searches() {
        for complete in [false, true] {
            let budget = budget();
            let entries = [SnapshotEntry { path: b"a", observed_bytes: 1, data: SnapshotData::Captured(b"x") },
                SnapshotEntry { path: b"b", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
            let workspace = restore(&entries, complete, &budget);
            let inputs = workspace.search_inputs(&budget, allocation(3), || false).unwrap();
            let index = inputs.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
            let report = index.search(&ParsedQuery::parse("absent").unwrap(), QueryOptions::new(QueryGeneration::new(owner(), 1).unwrap()),
                &budget, allocation(5), || false).unwrap();
            assert!(!report.is_complete()); assert_eq!(report.unavailable_files().len(), 1);
            assert_eq!(workspace.members()[1].unavailable_reason(), Some("SOURCE_UNAVAILABLE"));
        }
    }
    #[test]
    fn fresh_identity_intervals_cannot_overflow_and_foreign_owners_are_refused() {
        let budget = budget();
        let entries = [SnapshotEntry { path: b"a", observed_bytes: 0, data: SnapshotData::Captured(b"") },
            SnapshotEntry { path: b"b", observed_bytes: 0, data: SnapshotData::Captured(b"") }];
        let bytes = SnapshotBytes::encode(owner(), true, "test", &entries, SnapshotLimits::default(), &budget, allocation(1), || false).unwrap();
        let view = SnapshotView::open(bytes.bytes(), SnapshotLimits::default(), || false).unwrap();
        let id = SearchManifestId::new(owner(), 1).unwrap();
        assert!(matches!(SavedWorkspace::restore(view, id, FileId::new(owner(), u64::MAX).unwrap(), SourceRevision::new(owner(), 1).unwrap(),
            &budget, allocation(2), || false), Err(SavedSourceError::IdentityExhausted)));
        let foreign = ArenaOwnerId::new(732).unwrap();
        assert!(matches!(SavedWorkspace::restore(view, id, FileId::new(foreign, 1).unwrap(), SourceRevision::new(owner(), 1).unwrap(),
            &budget, allocation(2), || false), Err(SavedSourceError::OwnerMismatch)));
    }
    #[test]
    fn canceled_restore_does_not_leave_candidate_bytes_charged() {
        let budget = budget();
        let bytes = SnapshotBytes::encode(owner(), true, "test", &[], SnapshotLimits::default(), &budget, allocation(1), || false).unwrap();
        let view = SnapshotView::open(bytes.bytes(), SnapshotLimits::default(), || false).unwrap();
        let baseline = budget.accounting().reserved().get();
        let restored = SavedWorkspace::restore(view, SearchManifestId::new(owner(), 1).unwrap(), FileId::new(owner(), 1).unwrap(),
            SourceRevision::new(owner(), 1).unwrap(), &budget, allocation(2), || budget.accounting().reserved().get() > baseline);
        assert!(matches!(restored, Err(SavedSourceError::Canceled)));
        assert_eq!(budget.accounting().reserved().get(), baseline);
    }
}

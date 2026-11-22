#![forbid(unsafe_code)]

//! Explicit bounded archive admission shared by CLI and native trail export.
//! Every supplied envelope is verified before digest routing or deduplication.
//! The primary native archive is an identity only here: its retained descriptor
//! stays with the host and is never reopened from its old pathname.

use std::{fs::File, io::{self, Read, Seek, SeekFrom}, path::Path,
    sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}}};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget};
use fcb::search::paged_snapshot::{PagedSnapshot, PagedSnapshotError};
use fcb::search::snapshot::SnapshotLimits;
use fcb::store::Sha256Digest;
use fcb_core::ResourceLease;
use crate::{AppError, input, output::{Output, OutputError}};

pub(crate) const MAX_ARCHIVES: usize = 8;
pub(crate) const MAX_VALIDATION_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;
const STATE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArchiveSetError {
    Input(AppError), Archive(PagedSnapshotError), ArchiveLimit,
    ValidationByteLimit, DirectoryAdmission,
}
impl std::fmt::Display for ArchiveSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => write!(f, "{error}"),
            Self::Archive(error) => write!(f, "{error}"),
            Self::ArchiveLimit => f.write_str("TRAIL_EXPORT_ARCHIVE_LIMIT"),
            Self::ValidationByteLimit => f.write_str("TRAIL_EXPORT_VALIDATION_BYTE_LIMIT"),
            Self::DirectoryAdmission => f.write_str("TRAIL_EXPORT_DIRECTORY_ADMISSION"),
        }
    }
}
impl From<AppError> for ArchiveSetError { fn from(error: AppError) -> Self { Self::Input(error) } }
impl From<PagedSnapshotError> for ArchiveSetError { fn from(error: PagedSnapshotError) -> Self { Self::Archive(error) } }

#[derive(Clone, Copy, Default)]
pub(crate) struct SuppliedArchive {
    pub(crate) opened: bool,
    pub(crate) digest: Option<Sha256Digest>,
}
#[derive(Default)]
struct ReadCounters {
    bytes: AtomicU64,
    calls: AtomicU64,
    validating: AtomicBool,
}

/// Keep failed-envelope work observable without relying on a successfully
/// returned directory. The descriptor's observed length plus one EOF probe is
/// also an enforced read fence while validating, including header reads after
/// an adversarial grow/shrink race. Member reads use the cursor's own budget.
pub(crate) struct CountedArchive {
    file: File,
    counters: Arc<ReadCounters>,
    validation_remaining: u64,
}
impl Read for CountedArchive {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.counters.validating.load(Ordering::Relaxed) { return self.file.read(buffer); }
        let offered = buffer.len().min(usize::try_from(self.validation_remaining).unwrap_or(usize::MAX));
        if offered == 0 && !buffer.is_empty() {
            return Err(io::Error::other("archive validation admission exhausted"));
        }
        self.counters.calls.fetch_add(1, Ordering::Relaxed);
        let result = self.file.read(&mut buffer[..offered]);
        if let Ok(count) = result {
            self.validation_remaining -= count as u64;
            self.counters.bytes.fetch_add(count as u64, Ordering::Relaxed);
        }
        result
    }
}
impl Seek for CountedArchive {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> { self.file.seek(position) }
}

/// Field order releases directories before the child ledger and its parent
/// reservation. Construction is allocation-free so admission failures can be
/// encoded in the host's already reserved terminal receipt.
pub(crate) struct Archives {
    pub(crate) handles: [Option<PagedSnapshot<CountedArchive>>; MAX_ARCHIVES],
    pub(crate) count: usize,
    primary: Option<Sha256Digest>,
    supplied: usize,
    maximum: u64,
    pub(crate) inputs: [SuppliedArchive; MAX_ARCHIVES],
    validation_bytes_admitted: u64,
    directory_budget: Option<ResourceBudget>,
    counters: Option<Arc<ReadCounters>>,
    _reservation: Option<ResourceLease>,
}
impl Archives {
    pub(crate) fn new(primary: Option<Sha256Digest>, supplied: usize, maximum: u64) -> Self {
        Self { handles: std::array::from_fn(|_| None), count: 0, primary, supplied, maximum,
            inputs: [SuppliedArchive::default(); MAX_ARCHIVES], validation_bytes_admitted: 0,
            directory_budget: None, counters: None, _reservation: None }
    }

    pub(crate) fn open_paths(&mut self, paths: &[&Path], owner: ArenaOwnerId, budget: &ResourceBudget,
        reservation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool) -> Result<(), ArchiveSetError> {
        let primary_count = usize::from(self.primary.is_some());
        if paths.len() != self.supplied || paths.len() + primary_count > MAX_ARCHIVES
            || self.maximum > MAX_VALIDATION_BYTES { return Err(ArchiveSetError::ArchiveLimit); }
        self._reservation = Some(budget.try_reserve_managed(owner, reservation,
            ByteLength::new(MAX_DIRECTORY_BYTES + STATE_BYTES)).map_err(|_| ArchiveSetError::DirectoryAdmission)?);
        self.directory_budget = Some(ResourceBudget::new(owner, ByteLength::new(MAX_DIRECTORY_BYTES))
            .map_err(|_| ArchiveSetError::DirectoryAdmission)?);
        let counters = Arc::new(ReadCounters::default());
        counters.validating.store(true, Ordering::Relaxed);
        self.counters = Some(Arc::clone(&counters));
        for (ordinal, path) in paths.iter().enumerate() {
            if canceled() { return Err(AppError::Canceled.into()); }
            if path.as_os_str().is_empty() || path.as_os_str().len() > 16_384 {
                return Err(AppError::InputLimit.into());
            }
            let path = input::absolute(path)?;
            let (file, observed) = input::open_regular(&path)?;
            self.inputs[ordinal].opened = true;
            let mut limits = SnapshotLimits::default();
            if observed.len() > limits.max_document_bytes as u64 { return Err(PagedSnapshotError::Limits.into()); }
            let admission = observed.len().checked_add(1).ok_or(ArchiveSetError::ValidationByteLimit)?;
            if admission > self.maximum.saturating_sub(self.validation_bytes_admitted) {
                return Err(ArchiveSetError::ValidationByteLimit);
            }
            self.validation_bytes_admitted += admission;
            limits.max_document_bytes = observed.len() as usize;
            let file = CountedArchive { file, counters: Arc::clone(&counters), validation_remaining: admission };
            // Child allocation IDs are local to the separately backed ledger.
            let id = ResourceAllocationId::new(ordinal as u64 + 1).map_err(|_| ArchiveSetError::DirectoryAdmission)?;
            let candidate = PagedSnapshot::open(file, owner, limits,
                self.directory_budget.as_ref().ok_or(ArchiveSetError::DirectoryAdmission)?, id, &mut *canceled)?;
            let digest = candidate.directory().digest();
            self.inputs[ordinal].digest = Some(digest);
            // Duplicate explicit inputs still pay full validation. The retained
            // primary wins its digest; otherwise the first verified input wins.
            if self.primary != Some(digest) && !self.handles.iter().flatten()
                .any(|prior| prior.directory().digest() == digest) {
                self.handles[self.count] = Some(candidate);
                self.count += 1;
            }
        }
        counters.validating.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn for_digest(&mut self, digest: Sha256Digest) -> Option<&mut PagedSnapshot<CountedArchive>> {
        self.handles.iter_mut().flatten().find(|archive| archive.directory().digest() == digest)
    }
    pub(crate) fn first(&mut self) -> Option<&mut PagedSnapshot<CountedArchive>> {
        self.handles[0].as_mut()
    }
    pub(crate) fn member_read_calls(&self) -> u64 {
        self.handles.iter().flatten().map(|archive| archive.load_stats().read_calls).sum()
    }
    pub(crate) fn encode_validation(&self, out: &mut Output) -> Result<(), OutputError> {
        let primary_count = u64::from(self.primary.is_some());
        let validated = self.inputs[..self.supplied].iter().filter(|input| input.digest.is_some()).count() as u64;
        let opened = self.inputs[..self.supplied].iter().filter(|input| input.opened).count() as u64;
        let distinct = self.count as u64 + primary_count;
        let (retained, peak) = self.directory_budget.as_ref().map_or((0, 0), |budget| {
            let accounting = budget.accounting(); (accounting.reserved().get(), accounting.peak_reserved().get())
        });
        let (bytes, calls) = self.counters.as_ref().map_or((0, 0), |counters|
            (counters.bytes.load(Ordering::Relaxed), counters.calls.load(Ordering::Relaxed)));
        out.literal("{\"routing\":\"verified-archive-digest\",\"duplicate_policy\":\"first-verified-handle\"")?;
        for (key, value) in [("supplied_archives", self.supplied as u64 + primary_count),
            ("validated_archives", validated + primary_count), ("distinct_archives", distinct),
            ("duplicate_archives", validated + primary_count - distinct), ("opened_archive_paths", opened),
            ("validation_bytes_admitted", self.validation_bytes_admitted), ("validation_bytes_read", bytes),
            ("validation_read_calls", calls), ("retained_directory_bytes", retained), ("peak_directory_bytes", peak),
            ("max_archives", MAX_ARCHIVES as u64), ("max_validation_bytes", self.maximum),
            ("max_directory_bytes", MAX_DIRECTORY_BYTES)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"validation_complete\":")?; out.boolean(validated == self.supplied as u64)?;
        out.literal(",\"validation_admission\":\"observed-file-length-plus-one-eof-probe\",\"validation_scope\":")?;
        out.quoted(if self.primary.is_some() { "explicit-supplemental-archive-envelopes" }
            else { "explicit-archive-envelopes" })?;
        out.literal("}")
    }
}

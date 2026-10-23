#![forbid(unsafe_code)]

//! Progressive, metadata-only project browsing. One worker operation advances
//! the existing bounded walker by one batch; no full-tree vector, atlas layout,
//! source capture, rule-file read, or runtime is required before the first page.
//! Four immutable pages remain addressable independently of total files visited.
//! Discovery order is provisional, not a globally sorted namespace snapshot.
//!
//! A directory iterator cannot be rolled back. A consumed batch is retained
//! BEFORE response encoding/cancellation; advance(N) can therefore replay page
//! N+1 after a lost reply. Cancellation is polled between bounded walker calls,
//! never claimed to interrupt one filesystem operation. Stop pauses the cursor.

#[path = "file_finder_session.rs"]
pub mod finder;

use std::{collections::VecDeque, fs, mem::size_of, path::Path};
use fcb::{ArenaOwnerId, ByteLength, FileId};
use fcb::search::{ResourceAllocationId, ResourceBudget, RootId};
use fcb::source::{CancelFlag, DirectoryId, NormalizedPath, RawPath, RootGrant,
    SourceError, SymlinkPolicy};
use fcb::source::discovery::{BoundedDiscovery, DiscoveryAggregate, DiscoveryKind, DiscoveryLimits};
use fcb::source::ignore::IgnoreMatcher;
use fcb_core::ResourceLease;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL, MANAGED_BYTES, input, workspace};
use crate::output::{Output, OutputError};
use super::{HostError, HostResponse, MAX_HOST_TEXT_BYTES, reader::{ReaderSession, ReaderSessionError}};

pub const MAX_CATALOG_PAGE_ENTRIES: usize = 256;
pub const MAX_CATALOG_PATH_BYTES: usize = 2048;
// The existing walker tests its threshold BEFORE admitting the next entry.
// Charge the possible final-path overshoot as well as its ordinary 64 KiB page.
pub const MAX_CATALOG_PAGE_PATH_BYTES: usize = 64 * 1024 + MAX_CATALOG_PATH_BYTES;
pub const MAX_CATALOG_RETAINED_PAGES: usize = 4;
pub const MAX_CATALOG_RETAINED_FILES: usize = MAX_CATALOG_PAGE_ENTRIES * MAX_CATALOG_RETAINED_PAGES;
pub const MAX_CATALOG_RETAINED_PATH_BYTES: usize = MAX_CATALOG_PAGE_PATH_BYTES * MAX_CATALOG_RETAINED_PAGES;
pub const MAX_CATALOG_RESPONSE_BYTES: usize = 1024 * 1024;
const WALK_QUEUE: u32 = 4096;
const WALK_DEPTH: u32 = 32;
// Covers queued normalized paths/segments/ancestry, ReadDir observations,
// retained and incoming pages, root/path scratch, and native metadata overlap.
// It is constant even after millions of visited entries. Response leases are
// separately reserved; opening a reader uses its existing independent budget.
const STATE_BYTES: u64 = 64 * 1024 * 1024;
const FIRST_WINDOW_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogSessionError {
    App(AppError), Source(SourceError), Reader(ReaderSessionError),
    InvalidLimits, ForeignOwner, StalePage, MissingFile, NoMorePages,
    Canceled, IdentityExhausted,
}
impl CatalogSessionError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Source(SourceError::Canceled)
                | Self::Reader(ReaderSessionError::Canceled | ReaderSessionError::Source(SourceError::Canceled)) => true,
            Self::App(error) => error.is_canceled(),
            Self::Reader(ReaderSessionError::Host(HostError::App(error)) | ReaderSessionError::App(error)) => error.is_canceled(),
            Self::Reader(ReaderSessionError::View(error)) => AppError::View(error).is_canceled(),
            _ => false,
        }
    }
}
impl std::fmt::Display for CatalogSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "{e}"), Self::Source(e) => write!(f, "{e}"), Self::Reader(e) => write!(f, "{e}"),
            Self::InvalidLimits => f.write_str("CATALOG_SESSION_INVALID_LIMITS"),
            Self::ForeignOwner => f.write_str("CATALOG_SESSION_FOREIGN_OWNER"),
            Self::StalePage => f.write_str("CATALOG_SESSION_STALE_PAGE"),
            Self::MissingFile => f.write_str("CATALOG_SESSION_MISSING_FILE"),
            Self::NoMorePages => f.write_str("CATALOG_SESSION_NO_MORE_PAGES"),
            Self::Canceled => f.write_str("CATALOG_SESSION_CANCELED"),
            Self::IdentityExhausted => f.write_str("CATALOG_SESSION_IDENTITY_EXHAUSTED"),
        }
    }
}
impl std::error::Error for CatalogSessionError {}
impl From<AppError> for CatalogSessionError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<SourceError> for CatalogSessionError { fn from(e: SourceError) -> Self { Self::Source(e) } }
impl From<ReaderSessionError> for CatalogSessionError { fn from(e: ReaderSessionError) -> Self { Self::Reader(e) } }
impl From<OutputError> for CatalogSessionError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

struct CatalogFile { id: FileId, path: NormalizedPath, observed_bytes: u64 }
struct CatalogPage { id: u64, files: Vec<CatalogFile>, path_bytes: usize }

pub struct CatalogSession {
    grant: RootGrant,
    root_identity: DirectoryId,
    discovery: Option<BoundedDiscovery>,
    aggregate: DiscoveryAggregate,
    pages: VecDeque<CatalogPage>,
    latest_page: u64,
    catalogued_files: u64,
    complete: bool,
    paused: bool,
    next_allocation: u64,
    budget: ResourceBudget,
    _lease: ResourceLease,
}
impl CatalogSession {
    /// Open a canonical, nonsymlink root and publish exactly one walker batch.
    /// A page containing only directories/exclusions can have no file rows and
    /// still have more=true. That is progress, never proof of an empty project.
    pub fn open(owner: ArenaOwnerId, root: &Path, mut canceled: impl FnMut() -> bool)
        -> Result<Self, CatalogSessionError> {
        check(&mut canceled)?;
        if root.as_os_str().is_empty() || root.as_os_str().len() > 16_384 {
            return Err(CatalogSessionError::InvalidLimits);
        }
        if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
        let budget = ResourceBudget::new(owner, ByteLength::new(MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
        let lease = budget.try_reserve_managed(owner, allocation(1)?,
            ByteLength::new(STATE_BYTES + size_of::<Self>() as u64)).map_err(|_| AppError::Admission)?;
        let root = input::absolute(root)?;
        let metadata = fs::symlink_metadata(&root).map_err(|_| AppError::Io)?;
        if metadata.file_type().is_symlink() { return Err(AppError::Symlink.into()); }
        if !metadata.is_dir() { return Err(AppError::InvalidRange.into()); }
        let root = fs::canonicalize(root).map_err(|_| AppError::Io)?;
        if root.as_os_str().len() > 16_384 { return Err(CatalogSessionError::InvalidLimits); }
        let root_identity = DirectoryId::from_path(&root)?;
        let grant = RootGrant::new(RootId::new(owner, 1).map_err(|_| CatalogSessionError::IdentityExhausted)?, RawPath::from_path(&root));
        let limits = DiscoveryLimits::new(1, WALK_DEPTH, MAX_CATALOG_PATH_BYTES as u32,
            MAX_CATALOG_PAGE_ENTRIES as u32, 64 * 1024, WALK_QUEUE)?;
        let discovery = BoundedDiscovery::open_metadata_only(grant.clone(), SymlinkPolicy::DisallowAll,
            limits, IgnoreMatcher::product_defaults())?;
        let mut pages = VecDeque::new();
        pages.try_reserve_exact(MAX_CATALOG_RETAINED_PAGES).map_err(|_| AppError::Admission)?;
        if pages.capacity() > MAX_CATALOG_RETAINED_PAGES { return Err(AppError::Admission.into()); }
        let mut session = Self { grant, root_identity, discovery: Some(discovery), aggregate: DiscoveryAggregate::default(),
            pages, latest_page: 0, catalogued_files: 0, complete: false, paused: false, next_allocation: 2, budget, _lease: lease };
        session.consume_page(&mut canceled)?;
        session.validate_active()?;
        check(&mut canceled)?;
        Ok(session)
    }

    pub fn owner(&self) -> ArenaOwnerId { self.grant.owner() }
    pub fn grant(&self) -> &RootGrant { &self.grant }

    /// Root identity is pinned across a long-lived iterator. This detects an
    /// observed replacement/root symlink, not hostile ancestor races; those
    /// require the separately qualified native descriptor-relative service.
    pub fn validate_active(&self) -> Result<(), CatalogSessionError> {
        self.grant.validate_active()?;
        let root = self.grant.root_path().to_path_buf();
        let metadata = fs::symlink_metadata(&root).map_err(|_| AppError::SourceChanged)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir()
            || DirectoryId::from_path(&root).map_err(|_| AppError::SourceChanged)? != self.root_identity
            || fs::canonicalize(&root).map_err(|_| AppError::SourceChanged)? != root {
            return Err(AppError::SourceChanged.into());
        }
        self.grant.validate_active()?;
        Ok(())
    }

    pub fn info(&mut self, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogSessionError> {
        check(&mut canceled)?; self.validate_active()?;
        let mut out = self.output("info")?;
        out.literal("}\n")?;
        self.finish(out, &mut canceled)
    }

    /// Replay immutable rows; current-session retention/progress fields may be
    /// newer than this page. No discovery or source-payload operation occurs.
    pub fn page(&mut self, page_id: u64, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogSessionError> {
        check(&mut canceled)?;
        let index = self.page_index(page_id)?;
        self.validate_active()?;
        let mut out = self.output("page")?;
        let page = &self.pages[index];
        out.literal(",\"page_id\":")?; out.integer(page.id)?;
        out.literal(",\"after_page\":")?; out.integer(page.id - 1)?;
        out.literal(",\"files\":[")?;
        for (i, file) in page.files.iter().enumerate() {
            check(&mut canceled)?;
            if i != 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(file.id.get())?;
            out.literal(",\"path\":")?; out.path(&file.path.raw().to_path_buf())?;
            out.literal(",\"observed_bytes\":")?; out.integer(file.observed_bytes)?;
            out.literal("}")?;
        }
        out.literal("]}\n")?;
        self.finish(out, &mut canceled)
    }

    /// Exactly-once cursor consumption under a replayable after-page token.
    /// Repeating an older token replays its retained successor; an evicted token
    /// is refused, never interpreted as permission to skip ahead in the walk.
    pub fn advance(&mut self, after_page: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, CatalogSessionError> {
        check(&mut canceled)?;
        self.page_index(after_page)?;
        let next = after_page.checked_add(1).ok_or(CatalogSessionError::IdentityExhausted)?;
        if after_page < self.latest_page { return self.page(next, canceled); }
        if self.discovery.is_none() { return Err(CatalogSessionError::NoMorePages); }
        self.consume_page(&mut canceled)?;
        // consume_page has already retained the successor. A cancellation here
        // or a failed output reservation cannot lose the consumed observations.
        self.page(next, canceled)
    }

    /// Pause between bounded worker steps without tripping the walker's
    /// terminal CancelFlag. The next advance(latest_page) resumes this cursor.
    pub fn stop(&mut self, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogSessionError> {
        check(&mut canceled)?; self.validate_active()?;
        self.paused = self.discovery.is_some();
        let mut out = self.output("stop")?;
        out.literal("}\n")?;
        self.finish(out, &mut canceled)
    }

    /// The selected identity is metadata, not old source-capture authority.
    /// Capture the current regular file explicitly, using the same leaf
    /// no-follow/nonblocking opened-object checks as ReaderSession::open.
    /// Stale/foreign selections fail before ANY path metadata or source read.
    pub fn open_reader(&mut self, reader_owner: ArenaOwnerId, page_id: u64, file: FileId,
        max_source_bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), CatalogSessionError> {
        if file.owner() != self.owner() || reader_owner == self.owner() { return Err(CatalogSessionError::ForeignOwner); }
        let index = self.page_index(page_id)?;
        let entry = self.pages[index].files.iter().find(|entry| entry.id == file).ok_or(CatalogSessionError::MissingFile)?;
        if max_source_bytes == 0 || max_source_bytes > MAX_HOST_TEXT_BYTES { return Err(CatalogSessionError::InvalidLimits); }
        let relative = entry.path.clone();
        let (reader, info, window, source_bytes, source_calls) =
            self.capture_reader(reader_owner, &relative, max_source_bytes, &mut canceled)?;
        let label = relative.raw().to_path_buf();
        let mut out = self.output("open-reader")?;
        out.literal(",\"page_id\":")?; out.integer(page_id)?;
        out.literal(",\"file_id\":")?; out.integer(file.get())?;
        out.literal(",\"path\":")?; out.path(&label)?;
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

    /// Shared capture seam for retained metadata selections. Callers establish
    /// owner/page/query authority before entering this filesystem operation.
    fn capture_reader(&self, reader_owner: ArenaOwnerId, relative: &NormalizedPath,
        max_source_bytes: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse, HostResponse, u64, u64), CatalogSessionError> {
        if reader_owner == self.owner() { return Err(CatalogSessionError::ForeignOwner); }
        if max_source_bytes == 0 || max_source_bytes > MAX_HOST_TEXT_BYTES { return Err(CatalogSessionError::InvalidLimits); }
        check(canceled)?; self.validate_active()?;
        let root = self.grant.root_path().to_path_buf();
        let path = workspace::checked_source_path(&root, relative)?;
        let label = relative.raw().to_path_buf();
        let (reader, info, window, source_bytes, source_calls) = {
            let mut stop = || canceled() || self.validate_active().is_err();
            // Retain a relative raw label for the native supplied-source reader;
            // actual filesystem observation and counters stay in this receipt.
            let raw = super::read_bytes(&path, max_source_bytes, &mut stop).map_err(ReaderSessionError::from)?;
            let mut reader = ReaderSession::from_bytes(reader_owner, &label, &raw.bytes, &mut stop)?;
            let info = reader.info(&mut stop)?;
            let window = reader.read_window(0, FIRST_WINDOW_BYTES, &mut stop)?;
            (reader, info, window, raw.bytes.len() as u64, raw.read_calls)
        };
        self.validate_active()?; check(canceled)?;
        Ok((reader, info, window, source_bytes, source_calls))
    }

    fn page_index(&self, id: u64) -> Result<usize, CatalogSessionError> {
        self.pages.iter().position(|page| page.id == id).ok_or(CatalogSessionError::StalePage)
    }
    fn consume_page(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), CatalogSessionError> {
        check(canceled)?; self.validate_active()?;
        let id = self.latest_page.checked_add(1).ok_or(CatalogSessionError::IdentityExhausted)?;
        self.catalogued_files.checked_add(MAX_CATALOG_PAGE_ENTRIES as u64).ok_or(CatalogSessionError::IdentityExhausted)?;
        let mut files = Vec::new();
        files.try_reserve_exact(MAX_CATALOG_PAGE_ENTRIES).map_err(|_| AppError::Admission)?;
        if files.capacity() > MAX_CATALOG_PAGE_ENTRIES { return Err(AppError::Admission.into()); }
        let discovery = self.discovery.as_mut().ok_or(CatalogSessionError::NoMorePages)?;
        // An FnMut poll cannot be installed inside this existing synchronous
        // walker. Its documented entry/path/transition limits bound the step.
        // Do not cancel its Flag after consuming a page and discard that page.
        let batch = discovery.next_batch(&CancelFlag::new());
        self.aggregate = discovery.aggregate();
        let walker_complete = discovery.is_complete();
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => { self.discovery = None; self.complete = false; self.paused = false; return Err(error.into()); }
        };
        let more = batch.as_ref().is_some_and(|batch| batch.more());
        let mut path_bytes = 0;
        if let Some(batch) = batch {
            for entry in batch.entries() {
                if entry.kind() != DiscoveryKind::File || entry.is_excluded() { continue; }
                self.catalogued_files += 1; // Whole-batch identity headroom checked before advancing.
                let file = FileId::new(self.owner(), self.catalogued_files).expect("checked nonzero catalog identity");
                path_bytes += entry.path().as_bytes().len();
                files.push(CatalogFile { id: file, path: entry.path().clone(), observed_bytes: entry.observed_len().unwrap_or(0) });
            }
        }
        let a = self.aggregate;
        self.complete = !more && walker_complete && a.unavailable == 0 && a.cycles == 0
            && a.depth_limited == 0 && a.path_limited == 0 && a.queue_refused == 0;
        if !more { self.discovery = None; }
        self.paused = false;
        if self.pages.len() == MAX_CATALOG_RETAINED_PAGES { self.pages.pop_front(); }
        self.pages.push_back(CatalogPage { id, files, path_bytes });
        self.latest_page = id;
        Ok(()) // No fallible work between cursor consumption and page retention.
    }

    fn output(&mut self, command: &str) -> Result<Output, CatalogSessionError> {
        let allocation = self.next_id()?;
        let mut out = Output::new(self.owner(), MAX_CATALOG_RESPONSE_BYTES, &self.budget, allocation)?;
        out.literal("{\"schema\":\"fcb.project-catalog-session/1\",\"status\":\"ok\",\"command\":")?;
        out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.owner().get())?;
        out.literal(",\"identity_scope\":\"retained-session\",\"ordering\":\"provisional-discovery-order\",\"policy\":\"product-defaults/no-rule-files-v1\",\"native_presented\":false,\"discovery_source_bytes_read\":\"0\",\"discovery_source_read_calls\":\"0\",\"state\":")?;
        out.quoted(if self.complete { "complete" } else if self.discovery.is_none() { "partial" }
            else if self.paused { "paused" } else { "discovering" })?;
        out.literal(",\"more\":")?; out.boolean(self.discovery.is_some())?;
        out.literal(",\"paused\":")?; out.boolean(self.paused)?;
        out.literal(",\"discovery_complete\":")?; out.boolean(self.complete)?;
        for (key, value) in [("latest_page", self.latest_page),
            ("first_retained_page", self.pages.front().map_or(0, |page| page.id)),
            ("retained_pages", self.pages.len() as u64),
            ("retained_files", self.pages.iter().map(|page| page.files.len() as u64).sum()),
            ("retained_path_bytes", self.pages.iter().map(|page| page.path_bytes as u64).sum()),
            ("catalogued_files", self.catalogued_files)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"limits\":{")?;
        for (i, (key, value)) in [("page_entries", MAX_CATALOG_PAGE_ENTRIES),
            ("page_path_bytes", MAX_CATALOG_PAGE_PATH_BYTES), ("max_path_bytes", MAX_CATALOG_PATH_BYTES),
            ("retained_pages", MAX_CATALOG_RETAINED_PAGES), ("retained_files", MAX_CATALOG_RETAINED_FILES),
            ("retained_path_bytes", MAX_CATALOG_RETAINED_PATH_BYTES)].into_iter().enumerate() {
            if i != 0 { out.literal(",")?; }
            out.quoted(key)?; out.literal(":")?; out.integer(value as u64)?;
        }
        out.literal("},\"discovery\":{")?;
        let a = self.aggregate;
        for (i, (key, value)) in [("files", a.files), ("directories", a.directories),
            ("excluded_entries", a.excluded), ("symlinks_not_followed", a.symlinks),
            ("special_objects", a.special), ("unavailable", a.unavailable), ("cycles", a.cycles),
            ("path_limited", a.path_limited), ("depth_limited", a.depth_limited),
            ("queue_refused", a.queue_refused)].into_iter().enumerate() {
            if i != 0 { out.literal(",")?; }
            out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal("}")?;
        Ok(out)
    }
    fn next_id(&mut self) -> Result<ResourceAllocationId, CatalogSessionError> {
        let value = self.next_allocation;
        self.next_allocation = value.checked_add(1).ok_or(CatalogSessionError::IdentityExhausted)?;
        allocation(value)
    }
    fn finish(&mut self, out: Output, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, CatalogSessionError> {
        self.validate_active()?; check(canceled)?;
        let charge = out.as_bytes().len().checked_mul(3).and_then(|n| n.checked_add(4096)).ok_or(AppError::Admission)?;
        let allocation = self.next_id()?;
        let lease = self.budget.try_reserve_managed(self.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut text = String::new();
        text.try_reserve_exact(out.as_bytes().len()).map_err(|_| AppError::Admission)?;
        if text.capacity() > out.as_bytes().len() { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        self.validate_active()?; check(canceled)?;
        Ok(HostResponse { text, exit_code: if self.complete { EXIT_OK } else { EXIT_PARTIAL }, _lease: lease })
    }
}
fn allocation(value: u64) -> Result<ResourceAllocationId, CatalogSessionError> {
    ResourceAllocationId::new(value).map_err(|_| CatalogSessionError::IdentityExhausted)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), CatalogSessionError> {
    if canceled() { Err(CatalogSessionError::Canceled) } else { Ok(()) }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn output_admission_failure_cannot_lose_a_consumed_page() {
        let owner = ArenaOwnerId::new(72001).unwrap();
        let root = std::env::temp_dir().join(format!("fcb-catalog-output-denial-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&root).unwrap();
        for i in 0..300 { fs::write(root.join(format!("file-{i:03}.rs")), b"").unwrap(); }
        let mut session = CatalogSession::open(owner, &root, || false).unwrap();
        let admitted = session.budget.try_reserve_managed(owner, allocation(10000).unwrap(),
            ByteLength::new(MANAGED_BYTES - STATE_BYTES - size_of::<CatalogSession>() as u64)).unwrap();
        assert!(matches!(session.advance(1, || false), Err(CatalogSessionError::App(AppError::Output(OutputError::Allocation)))));
        assert_eq!(session.latest_page, 2);
        assert_eq!(session.catalogued_files, 300);
        drop(admitted);
        let recovered = session.advance(1, || false).unwrap();
        assert!(recovered.as_str().contains("\"page_id\":\"2\""));
        assert!(recovered.as_str().contains("\"catalogued_files\":\"300\""));
        assert_eq!(session.latest_page, 2);
        // Retain the diagnostic fixture, as for the integration tests.
    }
}

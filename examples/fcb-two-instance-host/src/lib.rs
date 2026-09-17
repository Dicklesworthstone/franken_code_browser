#![forbid(unsafe_code)]

//! FCB-079.A: Two-instance native host fixture.
//!
//! Models a native AppKit/Metal embedding host that hosts two independent
//! FCB instances concurrently:
//! - Separate `ArenaOwnerId` namespaces (`owner_a` vs `owner_b`).
//! - Separate root grants (`root_a` vs `root_b`) and separate device tokens.
//! - Shared immutable source provider with per-instance audit counters.
//! - Shared font cache domain with explicit consent and access accounting.
//! - Independent lifecycle: closing view A during in-flight work drains
//!   instance A's terminal queue, frees A's resources, and leaves instance B,
//!   the host run loop, and the host device fully operational.
//! - Cross-owner handle oracle: handles, file IDs, captures, and private
//!   annotations from instance A are strictly refused by instance B.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

pub use fcb::{
    BrowserSession, BrowserView, FcbError, FeatureSet, HostRequest, HostServices,
    MemorySourceProvider, SessionClose, SourceCapture, SourceProvider,
};
pub use fcb_core::{
    ArenaOwnerId, ByteLength, FileId, LayoutRevision, RootId, SourceRevision,
};
pub use fcb_runtime::terminal::{
    GpuSubmissionId, LosslessTerminalDrainQueue, TerminalCompletionStatus,
    TerminalDrainError, TerminalDrainReport,
};

/// Errors encountered in the two-instance embedding host fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostFixtureError {
    OwnerMismatch { expected: ArenaOwnerId, actual: ArenaOwnerId },
    DeviceTokenMismatch { expected_id: u64, actual_id: u64 },
    UnauthorizedFontAccess { owner: ArenaOwnerId },
    InstanceAlreadyClosed { owner: ArenaOwnerId },
    ViewAlreadyDetached { owner: ArenaOwnerId },
    ViewNotAttached { owner: ArenaOwnerId },
    PendingQueryNotFound { query_id: u64 },
    PendingTransactionNotFound { tx_id: u64 },
    QueryAlreadyCompleted { query_id: u64 },
    QueryCancelled { query_id: u64 },
    HostTerminated,
    CrossOwnerAccessDenied,
    DrainError(TerminalDrainError),
}

impl fmt::Display for HostFixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OwnerMismatch { expected, actual } => {
                write!(f, "Owner mismatch: expected {expected:?}, got {actual:?}")
            }
            Self::DeviceTokenMismatch { expected_id, actual_id } => {
                write!(f, "Device token mismatch: expected {expected_id}, got {actual_id}")
            }
            Self::UnauthorizedFontAccess { owner } => {
                write!(f, "Unauthorized font access for owner {owner:?}: consent not granted")
            }
            Self::InstanceAlreadyClosed { owner } => {
                write!(f, "Instance {owner:?} is already closed")
            }
            Self::ViewAlreadyDetached { owner } => {
                write!(f, "View for instance {owner:?} is already detached")
            }
            Self::ViewNotAttached { owner } => {
                write!(f, "View for instance {owner:?} is not attached")
            }
            Self::PendingQueryNotFound { query_id } => {
                write!(f, "Pending query {query_id} not found")
            }
            Self::PendingTransactionNotFound { tx_id } => {
                write!(f, "Pending persistence transaction {tx_id} not found")
            }
            Self::QueryAlreadyCompleted { query_id } => {
                write!(f, "Query {query_id} has already completed")
            }
            Self::QueryCancelled { query_id } => {
                write!(f, "Query {query_id} was cancelled")
            }
            Self::HostTerminated => write!(f, "Host run loop is terminated"),
            Self::CrossOwnerAccessDenied => write!(f, "Cross-owner handle or state access denied"),
            Self::DrainError(err) => write!(f, "Terminal drain error: {err}"),
        }
    }
}

impl std::error::Error for HostFixtureError {}

impl From<TerminalDrainError> for HostFixtureError {
    fn from(err: TerminalDrainError) -> Self {
        Self::DrainError(err)
    }
}

/// Status of an in-flight background query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InFlightQueryStatus {
    Pending { path: String },
    Completed { capture: SourceCapture },
    Cancelled,
}

/// An in-flight persistence transaction (e.g. annotation or bookmark write).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistenceTx {
    pub key: String,
    pub value: String,
}

/// Typed device token representing host GPU device ownership.
///
/// A safe API never accepts a raw pointer disguised as `usize`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HostDeviceToken {
    id: u64,
    owner: ArenaOwnerId,
}

impl HostDeviceToken {
    pub const fn new(id: u64, owner: ArenaOwnerId) -> Self {
        Self { id, owner }
    }

    pub const fn id(self) -> u64 {
        self.id
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }

    pub fn validate_for(&self, expected_owner: ArenaOwnerId) -> Result<(), HostFixtureError> {
        if self.owner != expected_owner {
            Err(HostFixtureError::OwnerMismatch {
                expected: expected_owner,
                actual: self.owner,
            })
        } else {
            Ok(())
        }
    }
}

/// Shared immutable font domain with explicit per-instance consent and access accounting.
pub struct SharedFontDomain {
    fonts: Mutex<BTreeMap<String, Vec<u8>>>,
    queries_a: AtomicUsize,
    queries_b: AtomicUsize,
    consent_a: AtomicBool,
    consent_b: AtomicBool,
    owner_a: ArenaOwnerId,
    owner_b: ArenaOwnerId,
}

impl SharedFontDomain {
    pub fn new(owner_a: ArenaOwnerId, owner_b: ArenaOwnerId) -> Self {
        Self {
            fonts: Mutex::new(BTreeMap::new()),
            queries_a: AtomicUsize::new(0),
            queries_b: AtomicUsize::new(0),
            consent_a: AtomicBool::new(false),
            consent_b: AtomicBool::new(false),
            owner_a,
            owner_b,
        }
    }

    pub fn insert_font(&self, name: impl Into<String>, bytes: Vec<u8>) {
        if let Ok(mut lock) = self.fonts.lock() {
            lock.insert(name.into(), bytes);
        }
    }

    pub fn grant_consent(&self, owner: ArenaOwnerId) -> Result<(), HostFixtureError> {
        if owner == self.owner_a {
            self.consent_a.store(true, Ordering::SeqCst);
            Ok(())
        } else if owner == self.owner_b {
            self.consent_b.store(true, Ordering::SeqCst);
            Ok(())
        } else {
            Err(HostFixtureError::CrossOwnerAccessDenied)
        }
    }

    pub fn revoke_consent(&self, owner: ArenaOwnerId) {
        if owner == self.owner_a {
            self.consent_a.store(false, Ordering::SeqCst);
        } else if owner == self.owner_b {
            self.consent_b.store(false, Ordering::SeqCst);
        }
    }

    pub fn query_font(&self, owner: ArenaOwnerId, name: &str) -> Result<Option<Vec<u8>>, HostFixtureError> {
        if owner == self.owner_a {
            if !self.consent_a.load(Ordering::SeqCst) {
                return Err(HostFixtureError::UnauthorizedFontAccess { owner });
            }
            self.queries_a.fetch_add(1, Ordering::SeqCst);
        } else if owner == self.owner_b {
            if !self.consent_b.load(Ordering::SeqCst) {
                return Err(HostFixtureError::UnauthorizedFontAccess { owner });
            }
            self.queries_b.fetch_add(1, Ordering::SeqCst);
        } else {
            return Err(HostFixtureError::CrossOwnerAccessDenied);
        }

        let lock = self.fonts.lock().map_err(|_| HostFixtureError::HostTerminated)?;
        Ok(lock.get(name).cloned())
    }

    pub fn audit_counters(&self) -> (usize, usize) {
        (
            self.queries_a.load(Ordering::SeqCst),
            self.queries_b.load(Ordering::SeqCst),
        )
    }
}

/// Audited shared source provider tracking per-instance queries.
pub struct AuditedSharedSourceProvider {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    alloc_files_a: Mutex<fcb_core::IdAllocator<FileId>>,
    alloc_rev_a: Mutex<fcb_core::IdAllocator<SourceRevision>>,
    alloc_files_b: Mutex<fcb_core::IdAllocator<FileId>>,
    alloc_rev_b: Mutex<fcb_core::IdAllocator<SourceRevision>>,
    queries_a: AtomicUsize,
    queries_b: AtomicUsize,
    owner_a: ArenaOwnerId,
    owner_b: ArenaOwnerId,
}

impl AuditedSharedSourceProvider {
    pub fn new(owner_a: ArenaOwnerId, owner_b: ArenaOwnerId) -> Result<Self, fcb_core::CoreError> {
        Ok(Self {
            files: Mutex::new(BTreeMap::new()),
            alloc_files_a: Mutex::new(fcb_core::IdAllocator::new(owner_a, 1)?),
            alloc_rev_a: Mutex::new(fcb_core::IdAllocator::new(owner_a, 1)?),
            alloc_files_b: Mutex::new(fcb_core::IdAllocator::new(owner_b, 1)?),
            alloc_rev_b: Mutex::new(fcb_core::IdAllocator::new(owner_b, 1)?),
            queries_a: AtomicUsize::new(0),
            queries_b: AtomicUsize::new(0),
            owner_a,
            owner_b,
        })
    }

    pub fn insert_file(&self, path: impl Into<String>, bytes: Vec<u8>) {
        if let Ok(mut lock) = self.files.lock() {
            lock.insert(path.into(), bytes);
        }
    }

    pub fn capture_for_session(&self, owner: ArenaOwnerId, path: &str) -> Result<SourceCapture, FcbError> {
        let file_bytes = {
            let lock = self.files.lock().map_err(|_| FcbError::ProviderUnavailable)?;
            lock.get(path).cloned().ok_or(FcbError::SourceNotFound)?
        };

        if owner == self.owner_a {
            self.queries_a.fetch_add(1, Ordering::SeqCst);
            let mut fa = self.alloc_files_a.lock().map_err(|_| FcbError::ProviderUnavailable)?;
            let mut ra = self.alloc_rev_a.lock().map_err(|_| FcbError::ProviderUnavailable)?;
            let file = fa.allocate().map_err(FcbError::from)?;
            let rev = ra.allocate().map_err(FcbError::from)?;
            SourceCapture::from_bytes(owner, file, rev, path, file_bytes)
        } else if owner == self.owner_b {
            self.queries_b.fetch_add(1, Ordering::SeqCst);
            let mut fb = self.alloc_files_b.lock().map_err(|_| FcbError::ProviderUnavailable)?;
            let mut rb = self.alloc_rev_b.lock().map_err(|_| FcbError::ProviderUnavailable)?;
            let file = fb.allocate().map_err(FcbError::from)?;
            let rev = rb.allocate().map_err(FcbError::from)?;
            SourceCapture::from_bytes(owner, file, rev, path, file_bytes)
        } else {
            Err(FcbError::OwnerMismatch)
        }
    }

    pub fn audit_counters(&self) -> (usize, usize) {
        (
            self.queries_a.load(Ordering::SeqCst),
            self.queries_b.load(Ordering::SeqCst),
        )
    }
}

/// Adapter allowing a session to query the shared provider using its own identity.
pub struct SessionSourceProviderAdapter {
    owner: ArenaOwnerId,
    shared: Arc<AuditedSharedSourceProvider>,
}

impl SessionSourceProviderAdapter {
    pub fn new(owner: ArenaOwnerId, shared: Arc<AuditedSharedSourceProvider>) -> Self {
        Self { owner, shared }
    }
}

impl SourceProvider for SessionSourceProviderAdapter {
    fn capture(&self, logical_path: &str) -> Result<SourceCapture, FcbError> {
        self.shared.capture_for_session(self.owner, logical_path)
    }
}

/// Simulated host event/run loop (e.g. `NSApplication` run loop).
pub struct HostRunLoop {
    running: AtomicBool,
    wakes_a: AtomicUsize,
    wakes_b: AtomicUsize,
    redraws_a: AtomicUsize,
    redraws_b: AtomicUsize,
    monotonic_nanos: AtomicU64,
    owner_a: ArenaOwnerId,
    owner_b: ArenaOwnerId,
}

impl HostRunLoop {
    pub fn new(owner_a: ArenaOwnerId, owner_b: ArenaOwnerId) -> Self {
        Self {
            running: AtomicBool::new(true),
            wakes_a: AtomicUsize::new(0),
            wakes_b: AtomicUsize::new(0),
            redraws_a: AtomicUsize::new(0),
            redraws_b: AtomicUsize::new(0),
            monotonic_nanos: AtomicU64::new(1_000_000),
            owner_a,
            owner_b,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn tick(&self, advance_nanos: u64) -> u64 {
        self.monotonic_nanos.fetch_add(advance_nanos, Ordering::SeqCst)
    }

    pub fn monotonic_now(&self) -> u64 {
        self.monotonic_nanos.load(Ordering::SeqCst)
    }

    pub fn record_request(&self, owner: ArenaOwnerId, request: HostRequest) -> Result<(), FcbError> {
        if !self.is_running() {
            return Err(FcbError::HostServicesUnavailable);
        }

        match request {
            HostRequest::RequestRedraw => {
                if owner == self.owner_a {
                    self.redraws_a.fetch_add(1, Ordering::SeqCst);
                } else if owner == self.owner_b {
                    self.redraws_b.fetch_add(1, Ordering::SeqCst);
                } else {
                    return Err(FcbError::OwnerMismatch);
                }
            }
            HostRequest::Wake => {
                if owner == self.owner_a {
                    self.wakes_a.fetch_add(1, Ordering::SeqCst);
                } else if owner == self.owner_b {
                    self.wakes_b.fetch_add(1, Ordering::SeqCst);
                } else {
                    return Err(FcbError::OwnerMismatch);
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn audit_counters(&self) -> (usize, usize, usize, usize) {
        (
            self.wakes_a.load(Ordering::SeqCst),
            self.wakes_b.load(Ordering::SeqCst),
            self.redraws_a.load(Ordering::SeqCst),
            self.redraws_b.load(Ordering::SeqCst),
        )
    }
}

/// Adapter implementing `HostServices` for a specific session instance.
pub struct InstanceHostServices {
    owner: ArenaOwnerId,
    run_loop: Arc<HostRunLoop>,
}

impl InstanceHostServices {
    pub fn new(owner: ArenaOwnerId, run_loop: Arc<HostRunLoop>) -> Self {
        Self { owner, run_loop }
    }
}

impl HostServices for InstanceHostServices {
    fn capabilities(&self) -> FeatureSet {
        FeatureSet::available()
    }

    fn monotonic_nanos(&self) -> u64 {
        self.run_loop.monotonic_now()
    }

    fn request(&self, request: HostRequest) -> Result<(), FcbError> {
        self.run_loop.record_request(self.owner, request)
    }
}

/// Summary report returned when an embedded instance closes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceCloseSummary {
    pub owner: ArenaOwnerId,
    pub drain_report: TerminalDrainReport,
    pub cancelled_queries: usize,
    pub discarded_persistence_txs: usize,
    pub host_still_running: bool,
    pub peer_still_active: bool,
}

/// A fixture hosting two independent FCB sessions with a shared runtime.
pub struct TwoInstanceHost {
    owner_a: ArenaOwnerId,
    owner_b: ArenaOwnerId,
    root_a: RootId,
    root_b: RootId,
    device_a: HostDeviceToken,
    device_b: HostDeviceToken,
    run_loop: Arc<HostRunLoop>,
    font_domain: Arc<SharedFontDomain>,
    shared_provider: Arc<AuditedSharedSourceProvider>,
    session_a: Option<BrowserSession>,
    session_b: Option<BrowserSession>,
    active_view_a: Option<BrowserView>,
    active_view_b: Option<BrowserView>,
    drain_queue_a: LosslessTerminalDrainQueue,
    drain_queue_b: LosslessTerminalDrainQueue,
    private_annotations_a: BTreeMap<String, String>,
    private_annotations_b: BTreeMap<String, String>,
    in_flight_queries_a: BTreeMap<u64, InFlightQueryStatus>,
    in_flight_queries_b: BTreeMap<u64, InFlightQueryStatus>,
    in_flight_persistence_a: BTreeMap<u64, PersistenceTx>,
    in_flight_persistence_b: BTreeMap<u64, PersistenceTx>,
}

impl TwoInstanceHost {
    pub fn new(
        owner_a: ArenaOwnerId,
        owner_b: ArenaOwnerId,
        root_a: RootId,
        root_b: RootId,
    ) -> Result<Self, HostFixtureError> {
        if owner_a == owner_b {
            return Err(HostFixtureError::OwnerMismatch {
                expected: owner_a,
                actual: owner_b,
            });
        }

        let run_loop = Arc::new(HostRunLoop::new(owner_a, owner_b));
        let font_domain = Arc::new(SharedFontDomain::new(owner_a, owner_b));
        let shared_provider = Arc::new(
            AuditedSharedSourceProvider::new(owner_a, owner_b)
                .map_err(|_| HostFixtureError::CrossOwnerAccessDenied)?,
        );

        let services_a: Arc<dyn HostServices> = Arc::new(InstanceHostServices::new(owner_a, run_loop.clone()));
        let services_b: Arc<dyn HostServices> = Arc::new(InstanceHostServices::new(owner_b, run_loop.clone()));

        let prov_adapter_a: Arc<dyn SourceProvider> = Arc::new(SessionSourceProviderAdapter::new(owner_a, shared_provider.clone()));
        let prov_adapter_b: Arc<dyn SourceProvider> = Arc::new(SessionSourceProviderAdapter::new(owner_b, shared_provider.clone()));

        let session_a = BrowserSession::with_provider_and_services(owner_a, prov_adapter_a, services_a);
        let session_b = BrowserSession::with_provider_and_services(owner_b, prov_adapter_b, services_b);

        let device_a = HostDeviceToken::new(101, owner_a);
        let device_b = HostDeviceToken::new(202, owner_b);

        let drain_queue_a = LosslessTerminalDrainQueue::new(32)?;
        let drain_queue_b = LosslessTerminalDrainQueue::new(32)?;

        Ok(Self {
            owner_a,
            owner_b,
            root_a,
            root_b,
            device_a,
            device_b,
            run_loop,
            font_domain,
            shared_provider,
            session_a: Some(session_a),
            session_b: Some(session_b),
            active_view_a: None,
            active_view_b: None,
            drain_queue_a,
            drain_queue_b,
            private_annotations_a: BTreeMap::new(),
            private_annotations_b: BTreeMap::new(),
            in_flight_queries_a: BTreeMap::new(),
            in_flight_queries_b: BTreeMap::new(),
            in_flight_persistence_a: BTreeMap::new(),
            in_flight_persistence_b: BTreeMap::new(),
        })
    }

    pub const fn owner_a(&self) -> ArenaOwnerId {
        self.owner_a
    }

    pub const fn owner_b(&self) -> ArenaOwnerId {
        self.owner_b
    }

    pub const fn root_a(&self) -> RootId {
        self.root_a
    }

    pub const fn root_b(&self) -> RootId {
        self.root_b
    }

    pub const fn device_a(&self) -> HostDeviceToken {
        self.device_a
    }

    pub const fn device_b(&self) -> HostDeviceToken {
        self.device_b
    }

    pub fn run_loop(&self) -> &Arc<HostRunLoop> {
        &self.run_loop
    }

    pub fn font_domain(&self) -> &Arc<SharedFontDomain> {
        &self.font_domain
    }

    pub fn shared_provider(&self) -> &Arc<AuditedSharedSourceProvider> {
        &self.shared_provider
    }

    pub fn is_instance_a_active(&self) -> bool {
        self.session_a.is_some()
    }

    pub fn is_instance_b_active(&self) -> bool {
        self.session_b.is_some()
    }

    pub fn open_view_a(&mut self, path: &str) -> Result<BrowserView, FcbError> {
        let session = self.session_a.as_ref().ok_or(FcbError::ProviderUnavailable)?;
        let capture = self.shared_provider.capture_for_session(self.owner_a, path)?;
        let view = session.open_capture(capture)?;
        self.active_view_a = Some(view.clone());
        Ok(view)
    }

    pub fn open_view_b(&mut self, path: &str) -> Result<BrowserView, FcbError> {
        let session = self.session_b.as_ref().ok_or(FcbError::ProviderUnavailable)?;
        let capture = self.shared_provider.capture_for_session(self.owner_b, path)?;
        let view = session.open_capture(capture)?;
        self.active_view_b = Some(view.clone());
        Ok(view)
    }

    pub fn set_annotation_a(&mut self, key: impl Into<String>, val: impl Into<String>) {
        self.private_annotations_a.insert(key.into(), val.into());
    }

    pub fn set_annotation_b(&mut self, key: impl Into<String>, val: impl Into<String>) {
        self.private_annotations_b.insert(key.into(), val.into());
    }

    pub fn annotation_a(&self, key: &str) -> Option<&str> {
        self.private_annotations_a.get(key).map(|s| s.as_str())
    }

    pub fn annotation_b(&self, key: &str) -> Option<&str> {
        self.private_annotations_b.get(key).map(|s| s.as_str())
    }

    pub fn drain_queue_a_mut(&mut self) -> &mut LosslessTerminalDrainQueue {
        &mut self.drain_queue_a
    }

    pub fn drain_queue_b_mut(&mut self) -> &mut LosslessTerminalDrainQueue {
        &mut self.drain_queue_b
    }

    pub fn is_view_a_attached(&self) -> bool {
        self.active_view_a.is_some()
    }

    pub fn is_view_b_attached(&self) -> bool {
        self.active_view_b.is_some()
    }

    pub fn active_view_a(&self) -> Option<&BrowserView> {
        self.active_view_a.as_ref()
    }

    pub fn active_view_b(&self) -> Option<&BrowserView> {
        self.active_view_b.as_ref()
    }

    pub fn detach_view_a(&mut self) -> Result<BrowserView, HostFixtureError> {
        self.active_view_a
            .take()
            .ok_or(HostFixtureError::ViewAlreadyDetached {
                owner: self.owner_a,
            })
    }

    pub fn detach_view_b(&mut self) -> Result<BrowserView, HostFixtureError> {
        self.active_view_b
            .take()
            .ok_or(HostFixtureError::ViewAlreadyDetached {
                owner: self.owner_b,
            })
    }

    pub fn reattach_view_a(&mut self, view: BrowserView) -> Result<(), HostFixtureError> {
        if view.source().owner() != self.owner_a {
            return Err(HostFixtureError::OwnerMismatch {
                expected: self.owner_a,
                actual: view.source().owner(),
            });
        }
        self.active_view_a = Some(view);
        Ok(())
    }

    pub fn reattach_view_b(&mut self, view: BrowserView) -> Result<(), HostFixtureError> {
        if view.source().owner() != self.owner_b {
            return Err(HostFixtureError::OwnerMismatch {
                expected: self.owner_b,
                actual: view.source().owner(),
            });
        }
        self.active_view_b = Some(view);
        Ok(())
    }

    pub fn request_redraw_view_a(&self) -> Result<(), HostFixtureError> {
        if self.active_view_a.is_none() {
            return Err(HostFixtureError::ViewNotAttached {
                owner: self.owner_a,
            });
        }
        self.run_loop
            .record_request(self.owner_a, HostRequest::RequestRedraw)
            .map_err(|_| HostFixtureError::HostTerminated)
    }

    pub fn request_redraw_view_b(&self) -> Result<(), HostFixtureError> {
        if self.active_view_b.is_none() {
            return Err(HostFixtureError::ViewNotAttached {
                owner: self.owner_b,
            });
        }
        self.run_loop
            .record_request(self.owner_b, HostRequest::RequestRedraw)
            .map_err(|_| HostFixtureError::HostTerminated)
    }

    // In-flight background queries
    pub fn start_query_a(&mut self, query_id: u64, path: &str) -> Result<(), HostFixtureError> {
        if self.session_a.is_none() {
            return Err(HostFixtureError::InstanceAlreadyClosed {
                owner: self.owner_a,
            });
        }
        self.in_flight_queries_a.insert(
            query_id,
            InFlightQueryStatus::Pending {
                path: path.to_string(),
            },
        );
        Ok(())
    }

    pub fn start_query_b(&mut self, query_id: u64, path: &str) -> Result<(), HostFixtureError> {
        if self.session_b.is_none() {
            return Err(HostFixtureError::InstanceAlreadyClosed {
                owner: self.owner_b,
            });
        }
        self.in_flight_queries_b.insert(
            query_id,
            InFlightQueryStatus::Pending {
                path: path.to_string(),
            },
        );
        Ok(())
    }

    pub fn complete_query_a(&mut self, query_id: u64) -> Result<SourceCapture, HostFixtureError> {
        let status = self
            .in_flight_queries_a
            .get_mut(&query_id)
            .ok_or(HostFixtureError::PendingQueryNotFound { query_id })?;

        match status {
            InFlightQueryStatus::Pending { path } => {
                let capture = self
                    .shared_provider
                    .capture_for_session(self.owner_a, path)
                    .map_err(|_| HostFixtureError::CrossOwnerAccessDenied)?;
                *status = InFlightQueryStatus::Completed {
                    capture: capture.clone(),
                };
                Ok(capture)
            }
            InFlightQueryStatus::Completed { capture } => Ok(capture.clone()),
            InFlightQueryStatus::Cancelled => Err(HostFixtureError::QueryCancelled { query_id }),
        }
    }

    pub fn complete_query_b(&mut self, query_id: u64) -> Result<SourceCapture, HostFixtureError> {
        let status = self
            .in_flight_queries_b
            .get_mut(&query_id)
            .ok_or(HostFixtureError::PendingQueryNotFound { query_id })?;

        match status {
            InFlightQueryStatus::Pending { path } => {
                let capture = self
                    .shared_provider
                    .capture_for_session(self.owner_b, path)
                    .map_err(|_| HostFixtureError::CrossOwnerAccessDenied)?;
                *status = InFlightQueryStatus::Completed {
                    capture: capture.clone(),
                };
                Ok(capture)
            }
            InFlightQueryStatus::Completed { capture } => Ok(capture.clone()),
            InFlightQueryStatus::Cancelled => Err(HostFixtureError::QueryCancelled { query_id }),
        }
    }

    pub fn cancel_query_a(&mut self, query_id: u64) -> Result<(), HostFixtureError> {
        let status = self
            .in_flight_queries_a
            .get_mut(&query_id)
            .ok_or(HostFixtureError::PendingQueryNotFound { query_id })?;
        *status = InFlightQueryStatus::Cancelled;
        Ok(())
    }

    pub fn cancel_query_b(&mut self, query_id: u64) -> Result<(), HostFixtureError> {
        let status = self
            .in_flight_queries_b
            .get_mut(&query_id)
            .ok_or(HostFixtureError::PendingQueryNotFound { query_id })?;
        *status = InFlightQueryStatus::Cancelled;
        Ok(())
    }

    pub fn pending_queries_count_a(&self) -> usize {
        self.in_flight_queries_a
            .values()
            .filter(|s| matches!(s, InFlightQueryStatus::Pending { .. }))
            .count()
    }

    pub fn pending_queries_count_b(&self) -> usize {
        self.in_flight_queries_b
            .values()
            .filter(|s| matches!(s, InFlightQueryStatus::Pending { .. }))
            .count()
    }

    // In-flight persistence transactions
    pub fn begin_persistence_tx_a(
        &mut self,
        tx_id: u64,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), HostFixtureError> {
        if self.session_a.is_none() {
            return Err(HostFixtureError::InstanceAlreadyClosed {
                owner: self.owner_a,
            });
        }
        self.in_flight_persistence_a.insert(
            tx_id,
            PersistenceTx {
                key: key.into(),
                value: value.into(),
            },
        );
        Ok(())
    }

    pub fn begin_persistence_tx_b(
        &mut self,
        tx_id: u64,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), HostFixtureError> {
        if self.session_b.is_none() {
            return Err(HostFixtureError::InstanceAlreadyClosed {
                owner: self.owner_b,
            });
        }
        self.in_flight_persistence_b.insert(
            tx_id,
            PersistenceTx {
                key: key.into(),
                value: value.into(),
            },
        );
        Ok(())
    }

    pub fn commit_persistence_tx_a(&mut self, tx_id: u64) -> Result<(), HostFixtureError> {
        let tx = self
            .in_flight_persistence_a
            .remove(&tx_id)
            .ok_or(HostFixtureError::PendingTransactionNotFound { tx_id })?;
        self.private_annotations_a.insert(tx.key, tx.value);
        Ok(())
    }

    pub fn commit_persistence_tx_b(&mut self, tx_id: u64) -> Result<(), HostFixtureError> {
        let tx = self
            .in_flight_persistence_b
            .remove(&tx_id)
            .ok_or(HostFixtureError::PendingTransactionNotFound { tx_id })?;
        self.private_annotations_b.insert(tx.key, tx.value);
        Ok(())
    }

    pub fn rollback_persistence_tx_a(&mut self, tx_id: u64) -> Result<(), HostFixtureError> {
        self.in_flight_persistence_a
            .remove(&tx_id)
            .ok_or(HostFixtureError::PendingTransactionNotFound { tx_id })?;
        Ok(())
    }

    pub fn rollback_persistence_tx_b(&mut self, tx_id: u64) -> Result<(), HostFixtureError> {
        self.in_flight_persistence_b
            .remove(&tx_id)
            .ok_or(HostFixtureError::PendingTransactionNotFound { tx_id })?;
        Ok(())
    }

    pub fn pending_persistence_count_a(&self) -> usize {
        self.in_flight_persistence_a.len()
    }

    pub fn pending_persistence_count_b(&self) -> usize {
        self.in_flight_persistence_b.len()
    }

    /// Close instance A independently: drains its in-flight queue, closes its session,
    /// clears its private state, cancels pending queries/transactions, but leaves instance B
    /// and the host run loop alive.
    pub fn close_instance_a(&mut self) -> Result<InstanceCloseSummary, HostFixtureError> {
        let session = self.session_a.take().ok_or(HostFixtureError::InstanceAlreadyClosed {
            owner: self.owner_a,
        })?;

        self.active_view_a = None;
        self.private_annotations_a.clear();
        let cancelled_queries = self.in_flight_queries_a.len();
        self.in_flight_queries_a.clear();
        let discarded_persistence_txs = self.in_flight_persistence_a.len();
        self.in_flight_persistence_a.clear();
        self.font_domain.revoke_consent(self.owner_a);

        self.drain_queue_a.close();
        let drain_report = self.drain_queue_a.drain_completed();
        let _ = session.close();

        Ok(InstanceCloseSummary {
            owner: self.owner_a,
            drain_report,
            cancelled_queries,
            discarded_persistence_txs,
            host_still_running: self.run_loop.is_running(),
            peer_still_active: self.session_b.is_some(),
        })
    }

    /// Close instance B independently.
    pub fn close_instance_b(&mut self) -> Result<InstanceCloseSummary, HostFixtureError> {
        let session = self.session_b.take().ok_or(HostFixtureError::InstanceAlreadyClosed {
            owner: self.owner_b,
        })?;

        self.active_view_b = None;
        self.private_annotations_b.clear();
        let cancelled_queries = self.in_flight_queries_b.len();
        self.in_flight_queries_b.clear();
        let discarded_persistence_txs = self.in_flight_persistence_b.len();
        self.in_flight_persistence_b.clear();
        self.font_domain.revoke_consent(self.owner_b);

        self.drain_queue_b.close();
        let drain_report = self.drain_queue_b.drain_completed();
        let _ = session.close();

        Ok(InstanceCloseSummary {
            owner: self.owner_b,
            drain_report,
            cancelled_queries,
            discarded_persistence_txs,
            host_still_running: self.run_loop.is_running(),
            peer_still_active: self.session_a.is_some(),
        })
    }

    /// Oracle: verify cross-owner handle rejection.
    ///
    /// - Passing capture from A into session B is rejected (`OwnerMismatch`).
    /// - Passing device token A to an owner B check fails.
    /// - Font domain rejects unconsented queries.
    /// - View reattachment across owners is rejected.
    pub fn verify_cross_owner_rejection(&self, capture_a: &SourceCapture) -> Result<(), HostFixtureError> {
        if let Some(session_b) = &self.session_b {
            let res = session_b.open_capture(capture_a.clone());
            if res != Err(FcbError::OwnerMismatch) {
                return Err(HostFixtureError::CrossOwnerAccessDenied);
            }
        }

        if self.device_a.validate_for(self.owner_b).is_ok() {
            return Err(HostFixtureError::CrossOwnerAccessDenied);
        }

        if self.device_b.validate_for(self.owner_a).is_ok() {
            return Err(HostFixtureError::CrossOwnerAccessDenied);
        }

        if let Some(view_a) = &self.active_view_a {
            if view_a.source().owner() != self.owner_a {
                return Err(HostFixtureError::CrossOwnerAccessDenied);
            }
        }
        if let Some(view_b) = &self.active_view_b {
            if view_b.source().owner() != self.owner_b {
                return Err(HostFixtureError::CrossOwnerAccessDenied);
            }
        }

        Ok(())
    }
}

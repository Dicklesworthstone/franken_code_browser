#![forbid(unsafe_code)]

use std::{collections::BTreeMap, fmt, sync::Arc};

pub use fcb_core::{
    ArenaOwnerId, ByteLength, ByteOffset, ByteRange, CoreError, FileId, SourceRevision,
};

/// A capability whose implementation can be selected additively by a host.
///
/// The facade itself remains usable with no Cargo features.  In-memory source
/// capture and frame planning are the always-available, host-supplied baseline;
/// these feature flags describe optional product surfaces rather than creating
/// a runtime or acquiring a host resource.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum Feature {
    Source,
    Search,
    Map,
    Markdown,
    View,
    Runtime,
    Persistence,
    MacosMetal,
}

impl Feature {
    pub const ALL: [Self; 8] = [
        Self::Source,
        Self::Search,
        Self::Map,
        Self::Markdown,
        Self::View,
        Self::Runtime,
        Self::Persistence,
        Self::MacosMetal,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Search => "search",
            Self::Map => "map",
            Self::Markdown => "markdown",
            Self::View => "view",
            Self::Runtime => "runtime",
            Self::Persistence => "persistence",
            Self::MacosMetal => "macos-metal",
        }
    }

    pub const fn compiled(self) -> bool {
        match self {
            Self::Source => cfg!(feature = "source"),
            Self::Search => cfg!(feature = "search"),
            Self::Map => cfg!(feature = "map"),
            Self::Markdown => cfg!(feature = "markdown"),
            Self::View => cfg!(feature = "view"),
            Self::Runtime => cfg!(feature = "runtime"),
            Self::Persistence => cfg!(feature = "persistence"),
            Self::MacosMetal => cfg!(feature = "macos-metal"),
        }
    }

    /// Whether this facade revision has a concrete implementation for the
    /// capability. Cargo feature selection alone never changes this answer.
    pub const fn implemented(self) -> bool {
        matches!(self, Self::Source | Self::View)
    }

    /// Whether this capability's target boundary is supported by this build.
    /// Pattern matching keeps the const path independent of derived equality.
    pub const fn target_supported(self) -> bool {
        match self {
            Self::MacosMetal => cfg!(target_os = "macos"),
            _ => true,
        }
    }

    const fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// The Cargo feature flags selected for this facade instance.
///
/// This is deliberately distinct from [`FeatureSet::available`]: a feature
/// flag may be selected before its implementation lands and must not become a
/// false capability claim.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FeatureSet(u16);

impl FeatureSet {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn compiled() -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < Feature::ALL.len() {
            let feature = Feature::ALL[index];
            if feature.compiled() {
                bits |= feature.bit();
            }
            index += 1;
        }
        Self(bits)
    }

    /// The capabilities implemented by this facade revision and usable on the
    /// current target. Source capture and renderer-neutral views are the
    /// concrete baseline; future profiles remain unavailable until implemented.
    pub const fn available() -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < Feature::ALL.len() {
            let feature = Feature::ALL[index];
            if feature.implemented() && feature.target_supported() {
                bits |= feature.bit();
            }
            index += 1;
        }
        Self(bits)
    }

    pub const fn contains(self, feature: Feature) -> bool {
        self.0 & feature.bit() != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn bits(self) -> u16 {
        self.0
    }
}

/// Errors returned by the inert facade and its host-supplied source provider.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum FcbError {
    InvalidPath,
    DuplicateSource,
    SourceNotFound,
    ProviderUnavailable,
    OwnerMismatch,
    FeatureUnavailable,
    UnsupportedTarget,
    CaptureTooLarge,
}

impl FcbError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidPath => "INVALID_PATH",
            Self::DuplicateSource => "DUPLICATE_SOURCE",
            Self::SourceNotFound => "SOURCE_NOT_FOUND",
            Self::ProviderUnavailable => "PROVIDER_UNAVAILABLE",
            Self::OwnerMismatch => "OWNER_MISMATCH",
            Self::FeatureUnavailable => "FEATURE_UNAVAILABLE",
            Self::UnsupportedTarget => "UNSUPPORTED_TARGET",
            Self::CaptureTooLarge => "CAPTURE_TOO_LARGE",
        }
    }
}

impl fmt::Display for FcbError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for FcbError {}

/// Immutable bytes captured by the host or by [`MemorySourceProvider`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceCapture {
    owner: ArenaOwnerId,
    file: FileId,
    revision: SourceRevision,
    logical_path: String,
    bytes: Arc<[u8]>,
}

impl SourceCapture {
    /// Construct a capture from bytes already supplied by the host.
    pub fn from_bytes(
        owner: ArenaOwnerId,
        file: FileId,
        revision: SourceRevision,
        logical_path: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<Self, FcbError> {
        let logical_path = logical_path.into();
        if file.owner() != owner || revision.owner() != owner {
            return Err(FcbError::OwnerMismatch);
        }
        if !valid_logical_path(&logical_path) {
            return Err(FcbError::InvalidPath);
        }
        Ok(Self {
            owner,
            file,
            revision,
            logical_path,
            bytes: Arc::from(bytes),
        })
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn file(&self) -> FileId {
        self.file
    }

    pub const fn revision(&self) -> SourceRevision {
        self.revision
    }

    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn byte_length(&self) -> Result<ByteLength, FcbError> {
        u64::try_from(self.bytes.len())
            .map(ByteLength::new)
            .map_err(|_| FcbError::CaptureTooLarge)
    }

    pub fn byte_range(&self) -> Result<ByteRange, FcbError> {
        // The end is derived from the captured byte length, so start <= end.
        let length = self.byte_length()?.get();
        ByteRange::new(ByteOffset::new(0), ByteOffset::new(length))
            .map_err(|_| FcbError::CaptureTooLarge)
    }
}

fn valid_logical_path(path: &str) -> bool {
    !path.is_empty() && !path.as_bytes().contains(&0)
}

/// A source provider is an explicit host dependency. Implementations own their
/// storage and may not assume that the facade grants filesystem access. Calls
/// and destruction of a user-supplied provider may have host-defined side
/// effects; the facade never invokes `capture` during session construction or
/// fabricates a promise that arbitrary provider drops are inert.
pub trait SourceProvider: Send + Sync {
    fn capture(&self, logical_path: &str) -> Result<SourceCapture, FcbError>;
}

/// A deterministic, in-memory provider useful to hosts and tests.
pub struct MemorySourceProvider {
    owner: ArenaOwnerId,
    files: BTreeMap<String, SourceCapture>,
    files_next: fcb_core::IdAllocator<FileId>,
    revisions_next: fcb_core::IdAllocator<SourceRevision>,
}

impl MemorySourceProvider {
    pub fn new(owner: ArenaOwnerId) -> Result<Self, CoreError> {
        Ok(Self {
            owner,
            files: BTreeMap::new(),
            files_next: fcb_core::IdAllocator::new(owner, 1)?,
            revisions_next: fcb_core::IdAllocator::new(owner, 1)?,
        })
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn insert(&mut self, logical_path: impl Into<String>, bytes: Vec<u8>) -> Result<(), FcbError> {
        let logical_path = logical_path.into();
        if !valid_logical_path(&logical_path) {
            return Err(FcbError::InvalidPath);
        }
        if self.files.contains_key(&logical_path) {
            return Err(FcbError::DuplicateSource);
        }
        let file = self
            .files_next
            .allocate()
            .map_err(|_| FcbError::OwnerMismatch)?;
        let revision = self
            .revisions_next
            .allocate()
            .map_err(|_| FcbError::OwnerMismatch)?;
        let capture =
            SourceCapture::from_bytes(self.owner, file, revision, logical_path.clone(), bytes)?;
        self.files.insert(logical_path, capture);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn capture(&self, logical_path: &str) -> Result<SourceCapture, FcbError> {
        <Self as SourceProvider>::capture(self, logical_path)
    }
}

impl SourceProvider for MemorySourceProvider {
    fn capture(&self, logical_path: &str) -> Result<SourceCapture, FcbError> {
        self.files
            .get(logical_path)
            .cloned()
            .ok_or(FcbError::SourceNotFound)
    }
}

/// The host-facing inert session. Its own construction, source lookup request,
/// and drop perform no thread, runtime, environment, filesystem, or GUI
/// acquisition. User-supplied provider calls and drop behavior remain under
/// the provider's explicit host contract.
pub struct BrowserSession {
    owner: ArenaOwnerId,
    provider: Option<Arc<dyn SourceProvider>>,
}

impl fmt::Debug for BrowserSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserSession")
            .field("owner", &self.owner)
            .field("has_provider", &self.provider.is_some())
            .finish()
    }
}

impl BrowserSession {
    pub fn new(owner: ArenaOwnerId) -> Self {
        Self {
            owner,
            provider: None,
        }
    }

    pub fn with_provider(owner: ArenaOwnerId, provider: Arc<dyn SourceProvider>) -> Self {
        Self {
            owner,
            provider: Some(provider),
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn compiled_features(&self) -> FeatureSet {
        FeatureSet::compiled()
    }

    pub const fn available_features(&self) -> FeatureSet {
        FeatureSet::available()
    }

    pub fn require_feature(&self, feature: Feature) -> Result<(), FcbError> {
        if feature == Feature::MacosMetal && !cfg!(target_os = "macos") {
            return Err(FcbError::UnsupportedTarget);
        }
        if feature.implemented() {
            Ok(())
        } else {
            Err(FcbError::FeatureUnavailable)
        }
    }

    pub fn open(&self, logical_path: &str) -> Result<BrowserView, FcbError> {
        let provider = self.provider.as_ref().ok_or(FcbError::ProviderUnavailable)?;
        self.open_capture(provider.capture(logical_path)?)
    }

    pub fn open_capture(&self, capture: SourceCapture) -> Result<BrowserView, FcbError> {
        if capture.owner() != self.owner {
            return Err(FcbError::OwnerMismatch);
        }
        BrowserView::from_capture(capture)
    }

    pub fn close(self) -> SessionClose {
        SessionClose { owner: self.owner }
    }
}

/// The selected immutable source view used by renderer-neutral hosts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserView {
    capture: SourceCapture,
}

impl BrowserView {
    fn from_capture(capture: SourceCapture) -> Result<Self, FcbError> {
        Ok(Self { capture })
    }

    pub fn source(&self) -> &SourceCapture {
        &self.capture
    }

    pub fn frame_plan(&self) -> Result<FramePlan, FcbError> {
        Ok(FramePlan {
            owner: self.capture.owner(),
            file: self.capture.file(),
            source: self.capture.revision(),
            bytes: self.capture.byte_range()?,
        })
    }
}

/// Renderer-neutral plan for the complete captured byte extent.
///
/// File identity is carried separately from source revision: a revision value
/// may repeat across files in one owner domain without aliasing their frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramePlan {
    owner: ArenaOwnerId,
    file: FileId,
    source: SourceRevision,
    bytes: ByteRange,
}

impl FramePlan {
    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn file(self) -> FileId {
        self.file
    }

    pub const fn source(self) -> SourceRevision {
        self.source
    }

    pub const fn bytes(self) -> ByteRange {
        self.bytes
    }
}

/// Explicit result of consuming a session.  It carries identity only; no host
/// resource is acquired or released by the facade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionClose {
    owner: ArenaOwnerId,
}

impl SessionClose {
    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[cfg(not(any(
        feature = "source",
        feature = "search",
        feature = "map",
        feature = "markdown",
        feature = "view",
        feature = "runtime",
        feature = "persistence",
        feature = "macos-metal"
    )))]
    #[test]
    fn default_features_are_empty_but_in_memory_facade_is_useful() {
        assert!(FeatureSet::compiled().is_empty());
        assert!(FeatureSet::available().contains(Feature::Source));
        assert!(FeatureSet::available().contains(Feature::View));
        assert!(!FeatureSet::available().contains(Feature::Search));
        let owner = ArenaOwnerId::new(1).unwrap();
        let mut provider = MemorySourceProvider::new(owner).unwrap();
        provider.insert("src/lib.rs", b"fn main() {}".to_vec()).unwrap();
        let session = BrowserSession::with_provider(owner, Arc::new(provider));
        let view = session.open("src/lib.rs").unwrap();
        assert_eq!(view.source().bytes(), b"fn main() {}");
        assert_eq!(view.frame_plan().unwrap().bytes().len().get(), 12);
    }

    #[test]
    fn construction_and_drop_do_not_probe_the_provider() {
        struct Probe(Arc<AtomicUsize>);
        impl SourceProvider for Probe {
            fn capture(&self, _: &str) -> Result<SourceCapture, FcbError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(FcbError::SourceNotFound)
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        {
            let owner = ArenaOwnerId::new(2).unwrap();
            let _session = BrowserSession::with_provider(owner, Arc::new(Probe(Arc::clone(&calls))));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn feature_requests_refuse_uncompiled_capabilities() {
        let owner = ArenaOwnerId::new(3).unwrap();
        let session = BrowserSession::new(owner);
        assert_eq!(session.require_feature(Feature::Search), Err(FcbError::FeatureUnavailable));
        assert_eq!(session.require_feature(Feature::Map), Err(FcbError::FeatureUnavailable));
        assert_eq!(session.require_feature(Feature::Markdown), Err(FcbError::FeatureUnavailable));
        assert_eq!(session.require_feature(Feature::Runtime), Err(FcbError::FeatureUnavailable));
        assert_eq!(session.require_feature(Feature::Persistence), Err(FcbError::FeatureUnavailable));
        assert_eq!(session.require_feature(Feature::Source), Ok(()));
        assert_eq!(session.require_feature(Feature::View), Ok(()));
        assert_eq!(session.require_feature(Feature::MacosMetal), Err(if cfg!(target_os = "macos") { FcbError::FeatureUnavailable } else { FcbError::UnsupportedTarget }));
    }
}

#![forbid(unsafe_code)]

//! Host source-provider and capture capability types (FCB-067.A).
//!
//! This crate separates a logical source provider from any native path. It
//! contains no filesystem, environment, thread, clock, or network access of
//! its own: a host grants providers explicitly, and the types here only
//! validate and describe what a provider delivered. "No ambient native
//! permission" is a structural property — nothing in this crate can reach a
//! file, socket, or process.
//!
//! A provider states its capture, ordering, range-read, and cancellation
//! guarantees explicitly; consumers must not infer stronger consistency from
//! an interface that returns bytes. Captures are distinguished as
//! [`CompleteCapture`] (immutable owned backing for the entire observed byte
//! sequence, carrying a whole-content observation digest) and
//! [`ExtentCapture`] (exactly the observed ranges, their observation digests,
//! and the unknown holes between them). A complete capture is still only an
//! observation: its digest identifies the exact observed sequence, never an
//! atomic filesystem instant.
//!
//! All metadata is hostile until validated. Constructors reject reversed or
//! empty ranges, overlapping or unsorted extent observations, holes that
//! overlap observations or exceed the declared total length, and complete
//! captures whose declared length disagrees with their actual bytes.

pub mod chunk;
pub mod confined;
pub mod encoding;
pub mod line_index;
pub mod path;
pub mod restoration;
pub mod root;
pub mod snapshot;

pub use chunk::{
    ChunkSize, ChunkedCapture, ChunkedReaderConfig, ExactRangeResult, RetainedCaptureStore,
    SafeChunkReader, SourceChunk,
};
pub use encoding::{
    detect_encoding, CaptureEncodingMap, DetectedEncoding, MappingSpan, SpanKind,
    StatefulChunkDecoder,
};
pub use confined::{ConfinedSourceReader, SymlinkPolicy};
pub use line_index::{
    LineCheckpoint, LineJumpResult, LineNumber, LineRangeOffsets, ResumableLineScanner,
    SparseLineIndex,
};
pub use path::{decode_uri_path, EscapedPathDisplay, NormalizedPath, RawPath};
pub use restoration::{
    NativeAccessLease, RestoredRoot, RootAccessController, RootAccessStatus, RootSessionRegistry,
    SandboxModel, SecurityScopedBookmark, StaleReason, UnavailableReason,
};
pub use root::{ExportPublicationGate, GrantRevocationToken, RootGrant};
pub use snapshot::{
    AnchorResolution, AnchorResolver, BoundedRetryReader, FileObservationMetadata,
    ObservedSnapshot, ObservedSnapshotConsistency, PinnedSnapshotStore, RetryPolicy,
    SnapshotBacking, SnapshotPin, SnapshotPinId,
};

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};

/// Stable, non-cryptographic identity for one observed byte sequence.
///
/// The digest is computed by this crate over the bytes actually delivered.
/// A provider-claimed digest is never trusted as observation identity. This
/// is a uniqueness aid for retained captures, not a security primitive.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObservationDigest(u64);

impl ObservationDigest {
    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// FNV-1a 64-bit over the observed bytes, mixed with the byte length so
    /// that length disagreements never share a digest.
    pub fn observe(bytes: &[u8]) -> Self {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= (bytes.len() as u64).rotate_left(32);
        Self(hash)
    }
}

/// What consistency a provider promises for one capture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CaptureConsistency {
    /// Bytes are an observation made at some point during the read. The
    /// digest identifies exactly the observed sequence and nothing else.
    /// This is the only consistency an interface returning bytes implies.
    ObservedSequence,
    /// The provider itself supplies a stronger atomic-snapshot guarantee for
    /// this capture. FCB never assumes this level without the statement.
    AtomicSnapshot,
}

/// Whether repeated reads of one retained capture revision return identical
/// bytes, as declared by the provider.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReadOrdering {
    /// The provider promises nothing about read ordering or stability.
    Unordered,
    /// Reads of one retained capture revision return the same observed bytes.
    StablePerRevision,
}

/// Declared range-read support of a provider.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RangeReadSupport {
    /// Only whole captures are available.
    None,
    /// Checked byte-range reads are available and are validated by this
    /// crate against the retained capture.
    ByteRanges,
}

/// Declared cancellation behavior of a provider.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CancellationSupport {
    /// Requests cannot be canceled once issued.
    None,
    /// The provider observes a cancellation flag between operations.
    /// Cancellation prevents future work; it cannot undo a capture that was
    /// already delivered and retained.
    Cooperative,
}

/// The guarantee statement a provider hands to every consumer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CaptureGuarantees {
    pub consistency: CaptureConsistency,
    pub ordering: ReadOrdering,
    pub range_reads: RangeReadSupport,
    pub cancellation: CancellationSupport,
}

impl CaptureGuarantees {
    /// The only guarantee set that can be inferred without a statement.
    pub const fn minimal() -> Self {
        Self {
            consistency: CaptureConsistency::ObservedSequence,
            ordering: ReadOrdering::Unordered,
            range_reads: RangeReadSupport::None,
            cancellation: CancellationSupport::None,
        }
    }
}

/// Cooperative cancellation flag. Setting it prevents future work; it can
/// never retract a capture that was already delivered and retained.
#[derive(Debug, Default)]
pub struct CancelFlag(AtomicBool);

impl CancelFlag {
    pub fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    pub fn cancel(&self) {
        self.0.store(true, AtomicOrdering::Release);
    }

    pub fn is_canceled(&self) -> bool {
        self.0.load(AtomicOrdering::Acquire)
    }
}

/// A request for one capture, validated before it reaches a provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureRequest {
    file: FileId,
    revision: SourceRevision,
    range: Option<ByteRange>,
}

impl CaptureRequest {
    pub fn new(file: FileId, revision: SourceRevision) -> Result<Self, SourceError> {
        if file.owner() != revision.owner() {
            return Err(SourceError::ForeignOwner);
        }
        Ok(Self {
            file,
            revision,
            range: None,
        })
    }

    /// Restrict the request to one non-empty checked byte range.
    pub fn with_range(mut self, range: ByteRange) -> Result<Self, SourceError> {
        if self.range.is_some() {
            return Err(SourceError::RangeAlreadySet);
        }
        if range.start() == range.end() {
            return Err(SourceError::InvalidRange);
        }
        self.range = Some(range);
        Ok(self)
    }

    pub const fn file(&self) -> FileId {
        self.file
    }

    pub const fn revision(&self) -> SourceRevision {
        self.revision
    }

    pub const fn range(&self) -> Option<ByteRange> {
        self.range
    }

    fn owner(&self) -> ArenaOwnerId {
        self.file.owner()
    }
}

/// Immutable owned backing for the entire observed byte sequence of one
/// capture revision. This is an observation, not an atomic filesystem
/// instant; the digest identifies exactly these bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteCapture {
    request: CaptureRequest,
    bytes: Arc<[u8]>,
    declared_length: ByteLength,
    digest: ObservationDigest,
}

impl CompleteCapture {
    /// Validates hostile metadata before accepting the capture: the declared
    /// length must equal the actual byte count, or the capture is refused.
    pub fn new(
        request: CaptureRequest,
        declared_length: ByteLength,
        bytes: Arc<[u8]>,
    ) -> Result<Self, SourceError> {
        if declared_length.get() != bytes.len() as u64 {
            return Err(SourceError::MetadataMismatch);
        }
        Ok(Self {
            request,
            declared_length,
            digest: ObservationDigest::observe(&bytes),
            bytes,
        })
    }

    pub const fn request(&self) -> &CaptureRequest {
        &self.request
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Checked range read. Availability still depends on the provider's
    /// declared [`RangeReadSupport`]; this method never manufactures data
    /// outside the observed sequence.
    pub fn range_bytes(&self, range: ByteRange) -> Result<&[u8], SourceError> {
        let (start, end) = range
            .as_usize_bounds()
            .map_err(|_| SourceError::RangeOutOfBounds)?;
        if range.start() == range.end() || end > self.bytes.len() {
            return Err(SourceError::RangeOutOfBounds);
        }
        self.bytes
            .get(start..end)
            .ok_or(SourceError::RangeOutOfBounds)
    }

    pub const fn declared_length(&self) -> ByteLength {
        self.declared_length
    }

    pub const fn digest(&self) -> ObservationDigest {
        self.digest
    }
}

/// One observed byte range inside an extent capture, with the digest of
/// exactly those observed bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservedRange {
    range: ByteRange,
    digest: ObservationDigest,
}

impl ObservedRange {
    /// The observed bytes must match the declared range length exactly.
    pub fn new(range: ByteRange, observed: &[u8]) -> Result<Self, SourceError> {
        if range.start() == range.end() {
            return Err(SourceError::InvalidRange);
        }
        if observed.len() as u64 != range.len().get() {
            return Err(SourceError::MetadataMismatch);
        }
        Ok(Self {
            range,
            digest: ObservationDigest::observe(observed),
        })
    }

    pub const fn range(&self) -> ByteRange {
        self.range
    }

    pub const fn digest(&self) -> ObservationDigest {
        self.digest
    }
}

/// Exactly the captured ranges of one revision, their observation digests,
/// and the unknown holes between them. An extent capture does not claim that
/// later reads from a changing live file belong to the same revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtentCapture {
    request: CaptureRequest,
    observations: Vec<ObservedRange>,
    holes: Vec<ByteRange>,
    total_length: Option<ByteLength>,
}

impl ExtentCapture {
    /// Validates the hostile-metadata oracle for extents: observations must
    /// be sorted, non-empty, and non-overlapping; holes must be non-empty,
    /// sorted, non-overlapping, and (when the total length is declared)
    /// inside it; observations and holes must be disjoint.
    pub fn new(
        request: CaptureRequest,
        observations: Vec<ObservedRange>,
        holes: Vec<ByteRange>,
        total_length: Option<ByteLength>,
    ) -> Result<Self, SourceError> {
        let mut previous_end: Option<ByteOffset> = None;
        for observed in &observations {
            let range = observed.range();
            if let Some(end) = previous_end {
                if range.start() < end {
                    return Err(SourceError::OverlappingExtent);
                }
            }
            previous_end = Some(range.end());
        }

        let mut hole_end: Option<ByteOffset> = None;
        for hole in &holes {
            if hole.start() == hole.end() {
                return Err(SourceError::InvalidRange);
            }
            if let Some(end) = hole_end {
                if hole.start() < end {
                    return Err(SourceError::OverlappingExtent);
                }
            }
            if let Some(total) = total_length {
                if hole.end().get() > total.get() {
                    return Err(SourceError::RangeOutOfBounds);
                }
            }
            hole_end = Some(hole.end());
        }
        // Both lists are sorted and internally disjoint at this point. Each
        // step retires one interval, bounding hostile fragmented inputs to
        // O(observations + holes) comparisons with no auxiliary allocation.
        let (mut observed_index, mut hole_index) = (0, 0);
        while let (Some(observed), Some(hole)) =
            (observations.get(observed_index), holes.get(hole_index))
        {
            if observed.range().end() <= hole.start() {
                observed_index += 1;
            } else if hole.end() <= observed.range().start() {
                hole_index += 1;
            } else {
                return Err(SourceError::OverlappingExtent);
            }
        }
        if let Some(total) = total_length {
            if let Some(last) = observations.last() {
                if last.range().end().get() > total.get() {
                    return Err(SourceError::RangeOutOfBounds);
                }
            }
        }

        Ok(Self {
            request,
            observations,
            holes,
            total_length,
        })
    }

    pub const fn request(&self) -> &CaptureRequest {
        &self.request
    }

    pub fn observations(&self) -> &[ObservedRange] {
        &self.observations
    }

    pub fn holes(&self) -> &[ByteRange] {
        &self.holes
    }

    pub const fn total_length(&self) -> Option<ByteLength> {
        self.total_length
    }

    /// Whether every byte named by `range` was observed (no gap remains).
    /// Partial coverage is not coverage.
    pub fn covers(&self, range: ByteRange) -> bool {
        if range.start() == range.end() {
            return false;
        }
        let mut cursor = range.start().get();
        for observed in &self.observations {
            let observed = observed.range();
            if observed.end().get() <= cursor {
                continue;
            }
            if observed.start().get() > cursor {
                return false;
            }
            cursor = observed.end().get();
            if cursor >= range.end().get() {
                break;
            }
        }
        cursor >= range.end().get()
    }
}

/// The capture a provider delivered for one request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaptureOutcome {
    Complete(CompleteCapture),
    Extent(ExtentCapture),
}

/// One explicit grant of source scope. Owning a grant does not imply any
/// native permission: it only names which owner domain's captures are
/// acceptable, so captures from two independently granted sources can never
/// alias.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceGrant {
    owner: ArenaOwnerId,
    files: BTreeSet<FileId>,
}

impl SourceGrant {
    pub fn new(owner: ArenaOwnerId) -> Self {
        Self {
            owner,
            files: BTreeSet::new(),
        }
    }

    pub fn grant(mut self, file: FileId) -> Result<Self, SourceError> {
        if file.owner() != self.owner {
            return Err(SourceError::ForeignOwner);
        }
        self.files.insert(file);
        Ok(self)
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn admits(&self, file: FileId) -> bool {
        file.owner() == self.owner && self.files.contains(&file)
    }

    /// Validates a delivered capture against this grant: the capture's
    /// owner, file identity, and revision owner must all belong to the
    /// granted scope.
    pub fn validate(&self, outcome: &CaptureOutcome) -> Result<(), SourceError> {
        let request = match outcome {
            CaptureOutcome::Complete(complete) => complete.request(),
            CaptureOutcome::Extent(extent) => extent.request(),
        };
        if request.owner() != self.owner {
            return Err(SourceError::ForeignOwner);
        }
        if !self.admits(request.file()) {
            return Err(SourceError::ForeignOwner);
        }
        Ok(())
    }
}

/// Typed refusal from provider and capture validation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum SourceError {
    /// A capture, request, or grant named a different owner domain.
    ForeignOwner,
    /// Declared metadata disagreed with the delivered bytes.
    MetadataMismatch,
    /// A byte range was empty or otherwise unusable.
    InvalidRange,
    /// Two observations or holes claimed the same bytes.
    OverlappingExtent,
    /// A range reached beyond the observed or declared extent.
    RangeOutOfBounds,
    /// A second range was attached to one request.
    RangeAlreadySet,
    /// The request was canceled before the provider began work.
    Canceled,
    /// A guaranteed capability was not actually declared.
    UnsupportedGuarantee,
    /// No retained capture exists for the requested file and revision.
    CaptureUnavailable,
    /// The provider would return more bytes than its configured per-response limit.
    PayloadTooLarge,
    /// A capture for the same file and revision was already retained.
    CaptureAlreadyPresent,
    /// The root grant was revoked or expired.
    GrantRevoked,
    /// The root directory was missing, unreadable, or unmounted.
    RootUnavailable,
    /// The requested path attempts to escape the authorized root boundary.
    PathEscape,
    /// A symlink was encountered when symlinks are forbidden by policy.
    SymlinkForbidden,
    /// A symlink points outside the authorized root directory.
    ForeignSymlink,
    /// A symlink cycle or alias expansion bound was exceeded during traversal.
    TraversalCycle,
    /// A filesystem object was a FIFO, socket, device, or other non-regular file.
    SpecialObject,
    /// Invalid URI percent-encoding or malformed path bytes.
    EncodingError,
    /// The source file was concurrently modified or truncated during read.
    ConcurrentModification,
    /// An operation requested a chunk outside valid chunk bounds.
    ChunkOutOfBounds,
    /// An anchor or query referred to an evicted or stale capture.
    StaleCapture,
    /// Eviction was refused because the snapshot backing is actively pinned.
    PinActive,
    /// A native unsigned sentinel value (such as NSNotFound) was passed as an offset.
    NativeSentinel,
    /// An offset fell in the interior of a multi-byte UTF-8 character.
    InvalidUtf8Boundary,
    /// An offset fell in the interior of a surrogate pair, or an invalid surrogate was encountered.
    InvalidUtf16,
    /// The source encoding is unsupported and no explicit decoder was supplied.
    UnsupportedEncoding,
}

impl SourceError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ForeignOwner => "SOURCE_FOREIGN_OWNER",
            Self::MetadataMismatch => "SOURCE_METADATA_MISMATCH",
            Self::InvalidRange => "SOURCE_INVALID_RANGE",
            Self::OverlappingExtent => "SOURCE_OVERLAPPING_EXTENT",
            Self::RangeOutOfBounds => "SOURCE_RANGE_OUT_OF_BOUNDS",
            Self::RangeAlreadySet => "SOURCE_RANGE_ALREADY_SET",
            Self::Canceled => "SOURCE_CANCELED",
            Self::UnsupportedGuarantee => "SOURCE_UNSUPPORTED_GUARANTEE",
            Self::CaptureUnavailable => "SOURCE_CAPTURE_UNAVAILABLE",
            Self::PayloadTooLarge => "SOURCE_PAYLOAD_TOO_LARGE",
            Self::CaptureAlreadyPresent => "SOURCE_CAPTURE_ALREADY_PRESENT",
            Self::GrantRevoked => "SOURCE_GRANT_REVOKED",
            Self::RootUnavailable => "SOURCE_ROOT_UNAVAILABLE",
            Self::PathEscape => "SOURCE_PATH_ESCAPE",
            Self::SymlinkForbidden => "SOURCE_SYMLINK_FORBIDDEN",
            Self::ForeignSymlink => "SOURCE_FOREIGN_SYMLINK",
            Self::TraversalCycle => "SOURCE_TRAVERSAL_CYCLE",
            Self::SpecialObject => "SOURCE_SPECIAL_OBJECT",
            Self::EncodingError => "SOURCE_ENCODING_ERROR",
            Self::ConcurrentModification => "SOURCE_CONCURRENT_MODIFICATION",
            Self::ChunkOutOfBounds => "SOURCE_CHUNK_OUT_OF_BOUNDS",
            Self::StaleCapture => "SOURCE_STALE_CAPTURE",
            Self::PinActive => "SOURCE_PIN_ACTIVE",
            Self::NativeSentinel => "SOURCE_NATIVE_SENTINEL",
            Self::InvalidUtf8Boundary => "SOURCE_INVALID_UTF8_BOUNDARY",
            Self::InvalidUtf16 => "SOURCE_INVALID_UTF16",
            Self::UnsupportedEncoding => "SOURCE_UNSUPPORTED_ENCODING",
        }
    }
}

impl From<fcb_core::CoreError> for SourceError {
    fn from(err: fcb_core::CoreError) -> Self {
        match err {
            fcb_core::CoreError::NativeSentinel => SourceError::NativeSentinel,
            fcb_core::CoreError::InvalidUtf8Boundary => SourceError::InvalidUtf8Boundary,
            fcb_core::CoreError::InvalidUtf16 => SourceError::InvalidUtf16,
            fcb_core::CoreError::RangeReversed | fcb_core::CoreError::InvalidId => {
                SourceError::InvalidRange
            }
            fcb_core::CoreError::LimitExceeded
            | fcb_core::CoreError::ArithmeticOverflow
            | fcb_core::CoreError::ArithmeticUnderflow => SourceError::RangeOutOfBounds,
            fcb_core::CoreError::OwnershipMismatch => SourceError::ForeignOwner,
            _ => SourceError::EncodingError,
        }
    }
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}


/// The seam an in-memory or host-supplied provider implements (FCB-067.B
/// delivers the in-memory conformance boundary against exactly this
/// vocabulary). Implementations receive the explicit grant, request, and
/// cancellation flag on every call; nothing here reaches a filesystem,
/// environment, or process on the provider's behalf. Delivered outcomes are
/// still hostile: consumers validate them through [`SourceGrant::validate`]
/// before retention.
pub trait HostSourceProvider: Send + Sync {
    /// The provider's own guarantee statement, declared once.
    fn guarantees(&self) -> CaptureGuarantees;

    /// Deliver one capture for a validated request. Implementations must
    /// observe `cancel` before beginning work (returning
    /// [`SourceError::Canceled`]); a capture already delivered before
    /// cancellation remains valid and retained.
    fn capture(
        &self,
        grant: &SourceGrant,
        request: &CaptureRequest,
        cancel: &CancelFlag,
    ) -> Result<CaptureOutcome, SourceError>;
}
impl std::error::Error for SourceError {}

/// A deterministic provider for immutable, host-supplied bytes.
///
/// The provider is deliberately keyed by the owner-qualified file and source
/// revision rather than by a path. Hosts grant the corresponding file through
/// [`SourceGrant`], and every request is checked against both that grant and
/// this provider's owner before bytes are looked up. A request can select one
/// checked non-empty range; the returned complete capture contains only that
/// range. The per-response limit bounds every returned allocation, while a
/// larger retained capture can still satisfy smaller range requests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InMemorySourceProvider {
    owner: ArenaOwnerId,
    max_payload: ByteLength,
    captures: std::collections::BTreeMap<(FileId, SourceRevision), Arc<[u8]>>,
}

impl InMemorySourceProvider {
    /// Creates an inert provider. A zero limit is allowed and admits only
    /// empty full captures; non-empty ranges are refused as too large.
    pub fn new(owner: ArenaOwnerId, max_payload: ByteLength) -> Self {
        Self {
            owner,
            max_payload,
            captures: std::collections::BTreeMap::new(),
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn max_payload(&self) -> ByteLength {
        self.max_payload
    }

    /// Retains one immutable source revision. The owner-qualified identities
    /// must belong to this provider; inserting a duplicate is refused rather
    /// than silently replacing a capture under the same identity.
    pub fn insert(
        &mut self,
        file: FileId,
        revision: SourceRevision,
        bytes: impl Into<Arc<[u8]>>,
    ) -> Result<(), SourceError> {
        if file.owner() != self.owner || revision.owner() != self.owner {
            return Err(SourceError::ForeignOwner);
        }
        let key = (file, revision);
        if self.captures.contains_key(&key) {
            return Err(SourceError::CaptureAlreadyPresent);
        }
        self.captures.insert(key, bytes.into());
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.captures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.captures.is_empty()
    }

    pub fn capture(
        &self,
        grant: &SourceGrant,
        request: &CaptureRequest,
        cancel: &CancelFlag,
    ) -> Result<CaptureOutcome, SourceError> {
        <Self as HostSourceProvider>::capture(self, grant, request, cancel)
    }
}

impl HostSourceProvider for InMemorySourceProvider {
    fn guarantees(&self) -> CaptureGuarantees {
        CaptureGuarantees {
            consistency: CaptureConsistency::ObservedSequence,
            ordering: ReadOrdering::StablePerRevision,
            range_reads: RangeReadSupport::ByteRanges,
            cancellation: CancellationSupport::Cooperative,
        }
    }

    fn capture(
        &self,
        grant: &SourceGrant,
        request: &CaptureRequest,
        cancel: &CancelFlag,
    ) -> Result<CaptureOutcome, SourceError> {
        if request.owner() != self.owner
            || grant.owner() != self.owner
            || !grant.admits(request.file())
        {
            return Err(SourceError::ForeignOwner);
        }
        gate_request_on_cancel(cancel)?;

        let stored = self
            .captures
            .get(&(request.file(), request.revision()))
            .ok_or(SourceError::CaptureUnavailable)?;
        // Validate the borrowed range and its payload size before allocating
        // backing or hashing bytes. A refused giant range must remain cheap.
        let selected = match request.range() {
            None => stored.as_ref(),
            Some(range) => {
                let (start, end) = range
                    .as_usize_bounds()
                    .map_err(|_| SourceError::RangeOutOfBounds)?;
                stored.get(start..end).ok_or(SourceError::RangeOutOfBounds)?
            }
        };
        let length = u64::try_from(selected.len()).map_err(|_| SourceError::PayloadTooLarge)?;
        if length > self.max_payload.get() {
            return Err(SourceError::PayloadTooLarge);
        }
        let bytes = match request.range() {
            None => Arc::clone(stored),
            Some(_) => Arc::from(selected),
        };
        CompleteCapture::new(*request, ByteLength::new(length), bytes).map(CaptureOutcome::Complete)
    }
}

/// Ensures a cancellation is honored before work starts, while a capture
/// delivered before cancellation stays valid and retained.
pub fn gate_request_on_cancel(
    flag: &CancelFlag,
) -> Result<(), SourceError> {
    if flag.is_canceled() {
        Err(SourceError::Canceled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(id: u64) -> ArenaOwnerId {
        ArenaOwnerId::new(id).unwrap()
    }

    fn file(owner_id: ArenaOwnerId, value: u64) -> FileId {
        FileId::new(owner_id, value).unwrap()
    }

    fn revision(owner_id: ArenaOwnerId, value: u64) -> SourceRevision {
        SourceRevision::new(owner_id, value).unwrap()
    }

    fn request(owner_id: u64, file_value: u64) -> CaptureRequest {
        let owner_id = owner(owner_id);
        CaptureRequest::new(file(owner_id, file_value), revision(owner_id, 1)).unwrap()
    }

    fn range(start: u64, end: u64) -> ByteRange {
        ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
    }

    #[test]
    fn complete_capture_rejects_declared_length_mismatch() {
        let request = request(11, 1);
        let bytes = Arc::from(b"hello".to_vec().into_boxed_slice());
        let declared = ByteLength::new(4);
        assert_eq!(
            CompleteCapture::new(request, declared, bytes),
            Err(SourceError::MetadataMismatch)
        );
    }

    #[test]
    fn complete_capture_digest_identifies_observed_sequence() {
        let bytes = Arc::from(b"hello".to_vec().into_boxed_slice());
        let capture = CompleteCapture::new(request(11, 2), ByteLength::new(5), bytes).unwrap();
        let again = CompleteCapture::new(
            request(11, 2),
            ByteLength::new(5),
            Arc::from(b"hello".to_vec().into_boxed_slice()),
        )
        .unwrap();
        assert_eq!(capture.digest(), again.digest());
        assert_eq!(capture.range_bytes(range(1, 4)).unwrap(), b"ell");
        assert_eq!(
            capture.range_bytes(range(4, 6)),
            Err(SourceError::RangeOutOfBounds)
        );
        assert_eq!(capture.declared_length().get(), 5);
    }

    #[test]
    fn extent_merge_matches_exhaustive_small_interval_oracle() {
        // Enumerate all internally disjoint subsets of nonempty intervals
        // in [0,4]. Unlike the production merge, the reference examines every
        // cross-list pair, including nesting, adjacency and empty lists.
        let candidates: Vec<_> = (0..4)
            .flat_map(|start| ((start + 1)..=4).map(move |end| range(start, end)))
            .collect();
        let mut lists = Vec::new();
        for mask in 0..(1usize << candidates.len()) {
            let selected: Vec<_> = candidates.iter().enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, interval)| *interval)
                .collect();
            if selected.windows(2).all(|pair| pair[0].end() <= pair[1].start()) {
                lists.push(selected);
            }
        }
        for observed in &lists {
            for holes in &lists {
                let overlaps = observed.iter().any(|a| holes.iter().any(|b|
                    a.start() < b.end() && b.start() < a.end()));
                let observations = observed.iter().map(|r|
                    ObservedRange::new(*r, &vec![0; r.len().get() as usize]).unwrap()
                ).collect();
                let actual = ExtentCapture::new(
                    request(12, 3), observations, holes.clone(), Some(ByteLength::new(4)));
                assert_eq!(actual.as_ref().err().copied(),
                    overlaps.then_some(SourceError::OverlappingExtent),
                    "observed={observed:?}, holes={holes:?}");
            }
        }
    }

    #[test]
    fn fragmented_extent_accepts_adjacency_and_detects_late_overlap() {
        let count = 20_000u64;
        let observations: Vec<_> = (0..count).map(|i|
            ObservedRange::new(range(2 * i, 2 * i + 1), b"x").unwrap()).collect();
        let mut holes: Vec<_> = (0..count).map(|i| range(2 * i + 1, 2 * i + 2)).collect();
        let total = Some(ByteLength::new(count * 2));
        assert!(ExtentCapture::new(request(12, 3), observations.clone(), holes.clone(), total).is_ok());
        *holes.last_mut().unwrap() = range(count * 2 - 2, count * 2);
        assert_eq!(ExtentCapture::new(request(12, 3), observations, holes, total),
            Err(SourceError::OverlappingExtent));
    }

    #[test]
    fn extent_capture_rejects_overlapping_and_unsorted_observations() {
        let request = request(12, 3);
        let mk = |start: u64, end: u64| ObservedRange::new(range(start, end), b"abc").unwrap();
        assert_eq!(
            ExtentCapture::new(request, vec![mk(0, 3), mk(2, 5)], vec![], None),
            Err(SourceError::OverlappingExtent)
        );
        assert_eq!(
            ExtentCapture::new(request, vec![mk(3, 6), mk(0, 3)], vec![], None),
            Err(SourceError::OverlappingExtent)
        );
    }

    #[test]
    fn extent_capture_rejects_holes_over_observations_and_totals() {
        let request = request(12, 4);
        let observed = ObservedRange::new(range(0, 3), b"abc").unwrap();
        let overlap_hole = range(2, 5);
        let total = ByteLength::new(4);
        let beyond_hole = range(3, 6);
        assert_eq!(
            ExtentCapture::new(request, vec![observed], vec![overlap_hole], None),
            Err(SourceError::OverlappingExtent)
        );
        assert_eq!(
            ExtentCapture::new(request, vec![observed], vec![beyond_hole], Some(total)),
            Err(SourceError::RangeOutOfBounds)
        );
    }

    #[test]
    fn extent_coverage_is_exact_and_partial_coverage_is_not_coverage() {
        let request = request(12, 5);
        let observed = ObservedRange::new(range(0, 3), b"abc").unwrap();
        let extent =
            ExtentCapture::new(request, vec![observed], vec![], Some(ByteLength::new(6))).unwrap();
        assert!(extent.covers(range(0, 3)));
        assert!(!extent.covers(range(1, 5)));
        assert!(!extent.covers(range(0, 0)));
        assert_eq!(extent.holes(), &[]);
        assert_eq!(extent.total_length().map(|t| t.get()), Some(6));
    }

    #[test]
    fn grants_reject_foreign_and_ungranted_sources() {
        let grant = SourceGrant::new(owner(21))
            .grant(file(owner(21), 7))
            .unwrap();
        let complete = CompleteCapture::new(
            request(21, 7),
            ByteLength::new(1),
            Arc::from(b"x".to_vec().into_boxed_slice()),
        )
        .unwrap();
        grant
            .validate(&CaptureOutcome::Complete(complete))
            .unwrap();

        let foreign = CompleteCapture::new(
            request(22, 7),
            ByteLength::new(1),
            Arc::from(b"x".to_vec().into_boxed_slice()),
        )
        .unwrap();
        assert_eq!(
            grant.validate(&CaptureOutcome::Complete(foreign)),
            Err(SourceError::ForeignOwner)
        );

        let ungranted = CompleteCapture::new(
            request(21, 8),
            ByteLength::new(1),
            Arc::from(b"x".to_vec().into_boxed_slice()),
        )
        .unwrap();
        assert_eq!(
            grant.validate(&CaptureOutcome::Complete(ungranted)),
            Err(SourceError::ForeignOwner)
        );
    }

    #[test]
    fn two_independently_granted_sources_cannot_alias() {
        let grant_a = SourceGrant::new(owner(31));
        let grant_b = SourceGrant::new(owner(32));
        let file_a = file(owner(31), 100);
        let file_b = file(owner(32), 100);
        assert_ne!(grant_a.owner(), grant_b.owner());
        assert_ne!(file_a, file_b);
        assert!(!grant_b.admits(file_a));
        assert!(grant_b.clone().grant(file_b).is_ok());
        assert!(grant_a.grant(file_b).is_err());
    }

    #[test]
    fn cancellation_blocks_future_work_but_not_retained_captures() {
        let flag = CancelFlag::new();
        let request = request(41, 9);
        gate_request_on_cancel(&flag).unwrap();
        flag.cancel();
        assert_eq!(
            gate_request_on_cancel(&flag),
            Err(SourceError::Canceled)
        );

        // A capture delivered before cancellation remains valid and intact.
        let delivered = CompleteCapture::new(
            request,
            ByteLength::new(1),
            Arc::from(b"z".to_vec().into_boxed_slice()),
        )
        .unwrap();
        assert_eq!(delivered.bytes(), b"z");
    }

    #[test]
    fn request_validation_rejects_empty_and_duplicate_ranges() {
        let base = request(51, 1);
        assert_eq!(base.with_range(range(2, 2)), Err(SourceError::InvalidRange));
        let ranged = base.with_range(range(0, 2)).unwrap();
        assert_eq!(
            ranged.with_range(range(0, 2)),
            Err(SourceError::RangeAlreadySet)
        );
        assert_eq!(ranged.range(), Some(range(0, 2)));
    }

    #[test]
    fn observation_digest_separates_lengths_and_bytes() {
        assert_ne!(
            ObservationDigest::observe(b"abc"),
            ObservationDigest::observe(b"abcd")
        );
        assert_ne!(
            ObservationDigest::observe(b"abc"),
            ObservationDigest::observe(b"acb")
        );
        assert_eq!(
            ObservationDigest::observe(b"abc"),
            ObservationDigest::observe(b"abc")
        );
    }

    /// Negative control: a hostile provider that lies about its metadata is
    /// rejected by the acceptance boundary rather than admitted.
    #[test]
    fn hostile_provider_metadata_is_rejected() {
        let bytes = Arc::from(b"12345".to_vec().into_boxed_slice());
        assert_eq!(
            CompleteCapture::new(request(61, 1), ByteLength::new(500), bytes),
            Err(SourceError::MetadataMismatch)
        );
        let lying_extent = ObservedRange::new(range(0, 9), b"short");
        assert_eq!(lying_extent, Err(SourceError::MetadataMismatch));
    }

    #[test]
    fn minimal_guarantees_are_explicit_and_infer_nothing() {
        let guarantees = CaptureGuarantees::minimal();
        assert_eq!(guarantees.consistency, CaptureConsistency::ObservedSequence);
        assert_eq!(guarantees.ordering, ReadOrdering::Unordered);
        assert_eq!(guarantees.range_reads, RangeReadSupport::None);
        assert_eq!(guarantees.cancellation, CancellationSupport::None);
        let stated = CaptureGuarantees {
            consistency: CaptureConsistency::AtomicSnapshot,
            ordering: ReadOrdering::StablePerRevision,
            range_reads: RangeReadSupport::ByteRanges,
            cancellation: CancellationSupport::Cooperative,
        };
        assert_ne!(guarantees, stated);
    }

    #[test]
    fn error_codes_are_stable_strings() {
        assert_eq!(SourceError::ForeignOwner.code(), "SOURCE_FOREIGN_OWNER");
        assert_eq!(
            SourceError::MetadataMismatch.code(),
            "SOURCE_METADATA_MISMATCH"
        );
        assert_eq!(SourceError::GrantRevoked.code(), "SOURCE_GRANT_REVOKED");
        assert_eq!(SourceError::RootUnavailable.code(), "SOURCE_ROOT_UNAVAILABLE");
        assert_eq!(SourceError::PathEscape.code(), "SOURCE_PATH_ESCAPE");
        assert_eq!(SourceError::SymlinkForbidden.code(), "SOURCE_SYMLINK_FORBIDDEN");
        assert_eq!(SourceError::ForeignSymlink.code(), "SOURCE_FOREIGN_SYMLINK");
        assert_eq!(SourceError::TraversalCycle.code(), "SOURCE_TRAVERSAL_CYCLE");
        assert_eq!(SourceError::SpecialObject.code(), "SOURCE_SPECIAL_OBJECT");
        assert_eq!(SourceError::EncodingError.code(), "SOURCE_ENCODING_ERROR");
        assert_eq!(
            SourceError::ConcurrentModification.code(),
            "SOURCE_CONCURRENT_MODIFICATION"
        );
        assert_eq!(
            SourceError::ChunkOutOfBounds.code(),
            "SOURCE_CHUNK_OUT_OF_BOUNDS"
        );
        assert_eq!(SourceError::StaleCapture.code(), "SOURCE_STALE_CAPTURE");
        assert_eq!(SourceError::PinActive.code(), "SOURCE_PIN_ACTIVE");
    }
}

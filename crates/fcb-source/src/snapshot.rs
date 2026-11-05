#![forbid(unsafe_code)]

//! Observed-capture replacement, retention, and backing pins (FCB-011.B / fcb-8t9.2).
//!
//! §8.3, §10.2, §10.7:
//! A file read records file metadata before and after reading. If the file changes,
//! retry within a bounded policy or publish a clearly identified observed snapshot.
//! A before/after stat match is a detector, not proof of an atomic filesystem
//! snapshot. We never claim an invented atomic snapshot.
//!
//! An in-memory owned snapshot is immutable. Old captures can be pinned to keep their
//! backing alive. An anchor resolves only against its retained immutable backing.
//! An observation digest is a non-cryptographic change detector, not evidence that
//! newly read bytes are the old capture. Once backing is unavailable, resolution
//! returns an explicit stale/diverged result; opening current source is deliberate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use fcb_core::{ArenaOwnerId, ByteLength, ByteRange, FileId, SourceRevision};

use crate::chunk::{ChunkedCapture, ChunkedReaderConfig, ExactRangeResult, SafeChunkReader};
use crate::confined::safe_open_regular_file;
use crate::{CancelFlag, CaptureRequest, ExtentCapture, ObservationDigest, SourceError};

/// Consistency classification of an observed snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedSnapshotConsistency {
    /// Before and after file metadata matched exactly; no concurrent mutation detected.
    VerifiedMatch,
    /// Metadata changed between before/after stats across all retry attempts.
    /// Published as a truthful observation of the read bytes without claiming atomicity.
    DivergedDuringRead,
    /// The stream was truncated unexpectedly mid-read.
    TruncatedDuringRead,
}

/// Metadata captured before and after reading a source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileObservationMetadata {
    pub initial_len: u64,
    pub final_len: u64,
    pub initial_modified_micros: Option<u64>,
    pub final_modified_micros: Option<u64>,
    pub retries_attempted: u32,
    pub consistency: ObservedSnapshotConsistency,
}

impl FileObservationMetadata {
    pub fn is_settled(&self) -> bool {
        self.consistency == ObservedSnapshotConsistency::VerifiedMatch
    }
}

/// Owned snapshot backing: either a complete chunked capture or an extent capture with known holes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotBacking {
    Complete(ChunkedCapture),
    Extent(ExtentCapture),
}

impl SnapshotBacking {
    pub fn total_length(&self) -> Option<ByteLength> {
        match self {
            Self::Complete(c) => Some(c.total_length()),
            Self::Extent(e) => e.total_length(),
        }
    }

    pub fn digest(&self) -> ObservationDigest {
        match self {
            Self::Complete(c) => c.digest(),
            Self::Extent(e) => {
                // Compose digest from observations and declared length
                let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
                for obs in e.observations() {
                    hash ^= obs.digest().get();
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
                if let Some(total) = e.total_length() {
                    hash ^= total.get().rotate_left(32);
                }
                ObservationDigest::from_raw(hash)
            }
        }
    }

    pub fn range_read(&self, range: ByteRange) -> Result<ExactRangeResult, SourceError> {
        match self {
            Self::Complete(c) => c.range_read(range),
            Self::Extent(_) => {
                // Extent captures only serve ranges fully covered by observations.
                // Holes return CaptureUnavailable.
                Err(SourceError::CaptureUnavailable)
            }
        }
    }
}

/// An observed source snapshot with immutable backing and before/after observation evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedSnapshot {
    file: FileId,
    revision: SourceRevision,
    backing: SnapshotBacking,
    metadata: FileObservationMetadata,
    digest: ObservationDigest,
}

impl ObservedSnapshot {
    pub fn new(
        file: FileId,
        revision: SourceRevision,
        backing: SnapshotBacking,
        metadata: FileObservationMetadata,
    ) -> Result<Self, SourceError> {
        let request = match &backing {
            SnapshotBacking::Complete(capture) => capture.request(),
            SnapshotBacking::Extent(capture) => capture.request(),
        };
        if file.owner() != revision.owner()
            || request.file().owner() != file.owner()
            || request.revision().owner() != revision.owner()
        {
            return Err(SourceError::ForeignOwner);
        }
        if request.file() != file || request.revision() != revision {
            return Err(SourceError::MetadataMismatch);
        }
        let digest = backing.digest();
        Ok(Self {
            file,
            revision,
            backing,
            metadata,
            digest,
        })
    }

    pub const fn file(&self) -> FileId {
        self.file
    }

    pub const fn revision(&self) -> SourceRevision {
        self.revision
    }

    pub const fn backing(&self) -> &SnapshotBacking {
        &self.backing
    }

    pub const fn metadata(&self) -> &FileObservationMetadata {
        &self.metadata
    }

    pub const fn digest(&self) -> ObservationDigest {
        self.digest
    }

    pub fn is_settled(&self) -> bool {
        self.metadata.is_settled()
    }

    pub fn range_read(&self, range: ByteRange) -> Result<ExactRangeResult, SourceError> {
        self.backing.range_read(range)
    }
}

/// Bounded retry policy for reading live mutable source files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    pub max_retries: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_retries: 3 }
    }
}

/// Reader that implements bounded retry and publishes truthful observed snapshots.
pub struct BoundedRetryReader;

impl BoundedRetryReader {
    /// Read a file with bounded retries on concurrent modification.
    ///
    /// If after `policy.max_retries` attempts the file is still changing, does NOT
    /// invent an atomic snapshot. Publishes an `ObservedSnapshot` marked
    /// `DivergedDuringRead` containing the bytes actually observed in the last attempt.
    pub fn read_file_with_retry(
        request: CaptureRequest,
        path: &Path,
        config: ChunkedReaderConfig,
        policy: RetryPolicy,
        cancel: &CancelFlag,
    ) -> Result<ObservedSnapshot, SourceError> {
        let mut retries: u32 = 0;

        loop {
            if cancel.is_canceled() {
                return Err(SourceError::Canceled);
            }

            let file = safe_open_regular_file(path)?;
            let stat_before = file.metadata().map_err(|_| SourceError::RootUnavailable)?;

            if !stat_before.is_file() {
                return Err(SourceError::SpecialObject);
            }

            let initial_len = stat_before.len();
            if initial_len > config.max_payload_bytes.get() {
                return Err(SourceError::PayloadTooLarge);
            }

            let initial_mtime = stat_before
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_micros() as u64);

            let read_outcome = SafeChunkReader::read_file(request, path, config, cancel);

            let file_after = safe_open_regular_file(path)?;
            let stat_after = file_after.metadata().map_err(|_| SourceError::RootUnavailable)?;
            let final_len = stat_after.len();
            let final_mtime = stat_after
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_micros() as u64);

            let changed = initial_len != final_len || initial_mtime != final_mtime;

            match read_outcome {
                Ok(chunked_capture) if !changed => {
                    // Clean settled read!
                    let meta = FileObservationMetadata {
                        initial_len,
                        final_len,
                        initial_modified_micros: initial_mtime,
                        final_modified_micros: final_mtime,
                        retries_attempted: retries,
                        consistency: ObservedSnapshotConsistency::VerifiedMatch,
                    };
                    return ObservedSnapshot::new(
                        request.file(),
                        request.revision(),
                        SnapshotBacking::Complete(chunked_capture),
                        meta,
                    );
                }
                Ok(chunked_capture) => {
                    // Read succeeded but file modified concurrently
                    if retries < policy.max_retries {
                        retries += 1;
                        continue;
                    }

                    // Retries exhausted: publish truthful diverged observation
                    let meta = FileObservationMetadata {
                        initial_len,
                        final_len,
                        initial_modified_micros: initial_mtime,
                        final_modified_micros: final_mtime,
                        retries_attempted: retries,
                        consistency: ObservedSnapshotConsistency::DivergedDuringRead,
                    };
                    return ObservedSnapshot::new(
                        request.file(),
                        request.revision(),
                        SnapshotBacking::Complete(chunked_capture),
                        meta,
                    );
                }
                Err(SourceError::ConcurrentModification) => {
                    if retries < policy.max_retries {
                        retries += 1;
                        continue;
                    }

                    // Retries exhausted and cannot form complete capture:
                    // Return typed concurrent modification error without inventing data.
                    return Err(SourceError::ConcurrentModification);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

static NEXT_PIN_ID: AtomicU64 = AtomicU64::new(1);

/// Unique identifier for an active backing pin.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SnapshotPinId(u64);

/// An active pin holding a snapshot's backing in its issuing store.
/// Cloning a token does not acquire another pin; any copy can release it once.
#[derive(Clone, Debug)]
pub struct SnapshotPin {
    id: SnapshotPinId,
    file: FileId,
    revision: SourceRevision,
}

impl SnapshotPin {
    pub const fn id(&self) -> SnapshotPinId {
        self.id
    }

    pub const fn file(&self) -> FileId {
        self.file
    }

    pub const fn revision(&self) -> SourceRevision {
        self.revision
    }
}

/// Store of retained snapshots supporting snapshot pins and safe eviction.
/// A clone retains the immutable snapshots but starts with no issued pins:
/// callers must explicitly pin within that new store.
#[derive(Debug)]
pub struct PinnedSnapshotStore {
    owner: ArenaOwnerId,
    snapshots: BTreeMap<(FileId, SourceRevision), ObservedSnapshot>,
    active_pins: BTreeMap<(FileId, SourceRevision), BTreeSet<SnapshotPinId>>,
}

impl Clone for PinnedSnapshotStore {
    fn clone(&self) -> Self {
        Self {
            owner: self.owner,
            snapshots: self.snapshots.clone(),
            active_pins: BTreeMap::new(),
        }
    }
}

impl PinnedSnapshotStore {
    pub fn new(owner: ArenaOwnerId) -> Self {
        Self {
            owner,
            snapshots: BTreeMap::new(),
            active_pins: BTreeMap::new(),
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn insert(&mut self, snapshot: ObservedSnapshot) -> Result<(), SourceError> {
        if snapshot.file().owner() != self.owner || snapshot.revision().owner() != self.owner {
            return Err(SourceError::ForeignOwner);
        }

        let key = (snapshot.file(), snapshot.revision());
        if self.snapshots.contains_key(&key) {
            return Err(SourceError::CaptureAlreadyPresent);
        }

        self.snapshots.insert(key, snapshot);
        Ok(())
    }

    pub fn get(
        &self,
        file: FileId,
        revision: SourceRevision,
    ) -> Result<&ObservedSnapshot, SourceError> {
        if file.owner() != self.owner || revision.owner() != self.owner {
            return Err(SourceError::ForeignOwner);
        }

        self.snapshots
            .get(&(file, revision))
            .ok_or(SourceError::CaptureUnavailable)
    }

    /// Pin a snapshot revision, preventing its backing from being evicted.
    pub fn pin(
        &mut self,
        file: FileId,
        revision: SourceRevision,
    ) -> Result<SnapshotPin, SourceError> {
        if file.owner() != self.owner || revision.owner() != self.owner {
            return Err(SourceError::ForeignOwner);
        }

        let key = (file, revision);
        if !self.snapshots.contains_key(&key) {
            return Err(SourceError::CaptureUnavailable);
        }

        if self.active_pins.get(&key).is_some_and(|pins| pins.len() >= u32::MAX as usize) {
            return Err(SourceError::IdentityExhausted);
        }
        // Unique token IDs never wrap or transfer authority between stores,
        // even when both stores use the same owner, file and revision.
        let pin_id = SnapshotPinId(
            NEXT_PIN_ID.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| next.checked_add(1))
                .map_err(|_| SourceError::IdentityExhausted)?,
        );
        self.active_pins.entry(key).or_default().insert(pin_id);
        Ok(SnapshotPin {
            id: pin_id,
            file,
            revision,
        })
    }

    /// Release this issued token once. Unknown, already released and foreign
    /// tokens leave every other active pin untouched.
    pub fn unpin(&mut self, pin: &SnapshotPin) -> bool {
        let key = (pin.file(), pin.revision());
        if let Some(pins) = self.active_pins.get_mut(&key) {
            if !pins.remove(&pin.id()) {
                return false;
            }
            if pins.is_empty() {
                self.active_pins.remove(&key);
            }
            return true;
        }
        false
    }

    /// Returns the number of active pins on a snapshot revision.
    pub fn pin_count(&self, file: FileId, revision: SourceRevision) -> u32 {
        // Admission checks the u32 bound before inserting another token.
        self.active_pins.get(&(file, revision)).map_or(0, |pins| pins.len() as u32)
    }

    /// Evict a snapshot revision from the store.
    ///
    /// If the snapshot has active pins, eviction is strictly refused with `PinActive`.
    pub fn evict(&mut self, file: FileId, revision: SourceRevision) -> Result<bool, SourceError> {
        if file.owner() != self.owner || revision.owner() != self.owner {
            return Err(SourceError::ForeignOwner);
        }

        let key = (file, revision);
        if self.active_pins.contains_key(&key) {
            return Err(SourceError::PinActive);
        }

        Ok(self.snapshots.remove(&key).is_some())
    }
}

/// The outcome of attempting to resolve an anchor against a requested revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnchorResolution {
    /// Resolved against immutable pinned or retained backing in memory.
    ExactPinned(ExactRangeResult),
    /// Reserved for callers with independent exact-byte verification.
    /// The digest-only `AnchorResolver::resolve` API never emits this variant.
    VerifiedLiveMatch(ExactRangeResult),
    /// Old capture backing is unavailable. Matching observation digests alone
    /// cannot verify equality, so this also covers equal-digest live candidates.
    /// Never substitutes live bytes under the old revision identity!
    StaleOrDiverged {
        old_revision: SourceRevision,
        old_digest: ObservationDigest,
        current_digest: Option<ObservationDigest>,
    },
}

/// Anchor resolution service implementing §10.7 old-capture semantics.
pub struct AnchorResolver;

impl AnchorResolver {
    /// Resolve an anchor range for a given file and revision.
    ///
    /// Retained backing is authoritative only for its exact owner/file/revision.
    /// A live candidate must name the same file in the same owner domain, but
    /// matching its non-cryptographic observation digest cannot establish byte
    /// equality after eviction. Such candidates remain explicitly stale.
    pub fn resolve(
        store: &PinnedSnapshotStore,
        file: FileId,
        revision: SourceRevision,
        range: ByteRange,
        expected_old_digest: ObservationDigest,
        live_candidate: Option<&ObservedSnapshot>,
    ) -> Result<AnchorResolution, SourceError> {
        match store.get(file, revision) {
            Ok(retained) => {
                if retained.digest() != expected_old_digest {
                    return Err(SourceError::MetadataMismatch);
                }
                return Ok(AnchorResolution::ExactPinned(retained.range_read(range)?));
            }
            Err(SourceError::CaptureUnavailable) => {}
            Err(error) => return Err(error),
        }

        if let Some(live) = live_candidate {
            if live.file().owner() != store.owner() || live.revision().owner() != store.owner() {
                return Err(SourceError::ForeignOwner);
            }
            if live.file() != file {
                return Err(SourceError::MetadataMismatch);
            }
        }
        Ok(AnchorResolution::StaleOrDiverged {
            old_revision: revision,
            old_digest: expected_old_digest,
            current_digest: live_candidate.map(ObservedSnapshot::digest),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use fcb_core::ByteOffset;
    use crate::chunk::{ChunkSize, SourceChunk};

    fn owner(id: u64) -> ArenaOwnerId {
        ArenaOwnerId::new(id).unwrap()
    }

    fn file(owner_id: ArenaOwnerId, val: u64) -> FileId {
        FileId::new(owner_id, val).unwrap()
    }

    fn revision(owner_id: ArenaOwnerId, val: u64) -> SourceRevision {
        SourceRevision::new(owner_id, val).unwrap()
    }

    fn range(start: u64, end: u64) -> ByteRange {
        ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
    }

    fn sample_snapshot(owner_id: u64, file_num: u64, rev_num: u64, data: &[u8]) -> ObservedSnapshot {
        let o = owner(owner_id);
        let f = file(o, file_num);
        let r = revision(o, rev_num);
        let req = CaptureRequest::new(f, r).unwrap();

        let chunk = SourceChunk::new(
            0,
            range(0, data.len() as u64),
            Arc::from(data.to_vec().into_boxed_slice()),
        )
        .unwrap();

        let cap = ChunkedCapture::new(
            req,
            ByteLength::new(data.len() as u64),
            ChunkSize::bounded(data.len().max(1)).unwrap(),
            vec![chunk],
        )
        .unwrap();

        let meta = FileObservationMetadata {
            initial_len: data.len() as u64,
            final_len: data.len() as u64,
            initial_modified_micros: Some(100),
            final_modified_micros: Some(100),
            retries_attempted: 0,
            consistency: ObservedSnapshotConsistency::VerifiedMatch,
        };

        ObservedSnapshot::new(f, r, SnapshotBacking::Complete(cap), meta).unwrap()
    }

    #[test]
    fn pin_lifecycle_prevents_and_allows_eviction() {
        let o = owner(1);
        let f = file(o, 10);
        let r = revision(o, 1);

        let mut store = PinnedSnapshotStore::new(o);
        let snap = sample_snapshot(1, 10, 1, b"pinned data");
        store.insert(snap).unwrap();

        // 1. Pin snapshot
        let pin = store.pin(f, r).unwrap();
        assert_eq!(store.pin_count(f, r), 1);

        // 2. Attempting to evict while pin active fails with PinActive
        assert_eq!(store.evict(f, r), Err(SourceError::PinActive));

        // 3. Unpin
        assert!(store.unpin(&pin));
        assert_eq!(store.pin_count(f, r), 0);

        // 4. Eviction now succeeds
        assert_eq!(store.evict(f, r), Ok(true));
        assert_eq!(store.get(f, r), Err(SourceError::CaptureUnavailable));
    }

    #[test]
    fn anchor_resolution_exact_when_pinned() {
        let o = owner(2);
        let f = file(o, 20);
        let r = revision(o, 1);

        let mut store = PinnedSnapshotStore::new(o);
        let snap = sample_snapshot(2, 20, 1, b"original-content-for-pin");
        let digest = snap.digest();
        store.insert(snap).unwrap();

        let pin = store.pin(f, r).unwrap();

        let res = AnchorResolver::resolve(
            &store,
            f,
            r,
            range(0, 8),
            digest,
            None,
        )
        .unwrap();

        match res {
            AnchorResolution::ExactPinned(exact) => {
                assert_eq!(exact.bytes(), b"original");
            }
            _ => assert_eq!(true, false, "expected ExactPinned"),
        }

        store.unpin(&pin);
    }

    #[test]
    fn anchor_resolution_requires_backing_even_when_live_digest_matches() {
        let o = owner(3);
        let f = file(o, 30);
        let r = revision(o, 1);

        let store = PinnedSnapshotStore::new(o); // empty store (evicted)
        let live_matching = sample_snapshot(3, 30, 2, b"same-exact-bytes");
        let expected_digest = live_matching.digest();

        let res = AnchorResolver::resolve(
            &store,
            f,
            r,
            range(0, 4),
            expected_digest,
            Some(&live_matching),
        )
        .unwrap();

        assert_eq!(res, AnchorResolution::StaleOrDiverged {
            old_revision: r,
            old_digest: expected_digest,
            current_digest: Some(expected_digest),
        });
    }

    #[test]
    fn negative_control_anchor_resolution_refuses_diverged_live_bytes() {
        let o = owner(4);
        let f = file(o, 40);
        let r = revision(o, 1);

        let store = PinnedSnapshotStore::new(o); // empty store (evicted)
        let old_snap = sample_snapshot(4, 40, 1, b"OLD_ORIGINAL_BYTES");
        let old_digest = old_snap.digest();

        let new_live_snap = sample_snapshot(4, 40, 2, b"NEW_MUTATED_DATA!");
        let new_digest = new_live_snap.digest();
        assert_ne!(old_digest, new_digest);

        // NEGATIVE CONTROL ORACLE:
        // Must NOT return NEW_MUTATED_DATA bytes under old revision identity!
        let res = AnchorResolver::resolve(
            &store,
            f,
            r,
            range(0, 4),
            old_digest,
            Some(&new_live_snap),
        )
        .unwrap();

        match res {
            AnchorResolution::StaleOrDiverged {
                old_revision,
                old_digest: expected,
                current_digest,
            } => {
                assert_eq!(old_revision, r);
                assert_eq!(expected, old_digest);
                assert_eq!(current_digest, Some(new_digest));
            }
            _ => assert_eq!(true, false, "Negative control failed: diverged bytes were silently accepted!"),
        }
    }

    #[test]
    fn observation_metadata_distinguishes_settled_and_diverged() {
        let settled_meta = FileObservationMetadata {
            initial_len: 100,
            final_len: 100,
            initial_modified_micros: Some(50),
            final_modified_micros: Some(50),
            retries_attempted: 1,
            consistency: ObservedSnapshotConsistency::VerifiedMatch,
        };
        assert!(settled_meta.is_settled());

        let diverged_meta = FileObservationMetadata {
            initial_len: 100,
            final_len: 120,
            initial_modified_micros: Some(50),
            final_modified_micros: Some(60),
            retries_attempted: 3,
            consistency: ObservedSnapshotConsistency::DivergedDuringRead,
        };
        assert!(!diverged_meta.is_settled());
    }
}

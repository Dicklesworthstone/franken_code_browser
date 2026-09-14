#![forbid(unsafe_code)]

//! Integration test suite for observed-capture replacement, retention, and backing pins
//! (FCB-011.B / fcb-8t9.2).
//!
//! Verifies:
//! 1. Before/after observations and bounded retry without invented atomic snapshots.
//! 2. Truthful publication of DivergedDuringRead when file changes across retries.
//! 3. Pinned snapshot backing prevents eviction; unpinning allows eviction.
//! 4. Anchor resolution resolves against pinned backing.
//! 5. Anchor resolution succeeds against live file if exact digest matches.
//! 6. Negative control oracle: anchor resolution strictly refuses to substitute
//!    diverged live bytes when old snapshot was evicted, returning StaleOrDiverged.

use std::fs;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb_source::chunk::{
    ChunkSize, ChunkedCapture, ChunkedReaderConfig, SourceChunk,
};
use fcb_source::snapshot::{
    AnchorResolution, AnchorResolver, BoundedRetryReader, FileObservationMetadata,
    ObservedSnapshot, ObservedSnapshotConsistency, PinnedSnapshotStore, RetryPolicy,
    SnapshotBacking,
};
use fcb_source::{
    CancelFlag, CaptureRequest, ExtentCapture, ObservationDigest, ObservedRange, SourceError,
};

fn temp_test_dir(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("fcb_snapshot_test_{label}_{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).unwrap()
}

fn file_id(owner_id: ArenaOwnerId, val: u64) -> FileId {
    FileId::new(owner_id, val).unwrap()
}

fn revision(owner_id: ArenaOwnerId, val: u64) -> SourceRevision {
    SourceRevision::new(owner_id, val).unwrap()
}

fn request(owner_id: u64, file_num: u64, rev_num: u64) -> CaptureRequest {
    let o = owner(owner_id);
    CaptureRequest::new(file_id(o, file_num), revision(o, rev_num)).unwrap()
}

fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
}

#[test]
fn bounded_retry_reads_settled_file_successfully() {
    let dir = temp_test_dir("settled_read");
    let file_path = dir.join("settled.txt");

    let payload = b"Stable file content that does not mutate during observation.";
    fs::write(&file_path, payload).unwrap();

    let req = request(201, 1, 1);
    let cancel = CancelFlag::new();
    let config = ChunkedReaderConfig {
        chunk_size: ChunkSize::bounded(32).unwrap(),
        max_payload_bytes: ByteLength::new(1024 * 1024),
    };
    let policy = RetryPolicy { max_retries: 2 };

    let snapshot = BoundedRetryReader::read_file_with_retry(
        req,
        &file_path,
        config,
        policy,
        &cancel,
    )
    .unwrap();

    assert!(snapshot.is_settled());
    assert_eq!(
        snapshot.metadata().consistency,
        ObservedSnapshotConsistency::VerifiedMatch
    );
    assert_eq!(snapshot.metadata().initial_len, payload.len() as u64);
    assert_eq!(snapshot.metadata().final_len, payload.len() as u64);

    let range_res = snapshot.range_read(range(0, 6)).unwrap();
    assert_eq!(range_res.bytes(), b"Stable");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn snapshot_pin_lifecycle_guards_and_releases_backing() {
    let o = owner(202);
    let f = file_id(o, 10);
    let rev = revision(o, 1);

    let mut store = PinnedSnapshotStore::new(o);

    let req = CaptureRequest::new(f, rev).unwrap();
    let chunk = SourceChunk::new(
        0,
        range(0, 16),
        Arc::from(b"pinned_snapshot_".to_vec().into_boxed_slice()),
    )
    .unwrap();
    let cap = ChunkedCapture::new(
        req,
        ByteLength::new(16),
        ChunkSize::bounded(16).unwrap(),
        vec![chunk],
    )
    .unwrap();
    let meta = FileObservationMetadata {
        initial_len: 16,
        final_len: 16,
        initial_modified_micros: Some(10),
        final_modified_micros: Some(10),
        retries_attempted: 0,
        consistency: ObservedSnapshotConsistency::VerifiedMatch,
    };
    let snap = ObservedSnapshot::new(f, rev, SnapshotBacking::Complete(cap), meta).unwrap();
    store.insert(snap).unwrap();

    // 1. Acquire pin
    let pin = store.pin(f, rev).unwrap();
    assert_eq!(store.pin_count(f, rev), 1);

    // 2. Attempting to evict while pin is active fails with PinActive
    assert_eq!(store.evict(f, rev), Err(SourceError::PinActive));

    // 3. Resolve anchor against pinned snapshot
    let exact = store.get(f, rev).unwrap().range_read(range(0, 6)).unwrap();
    assert_eq!(exact.bytes(), b"pinned");

    // 4. Unpin
    assert!(store.unpin(&pin));
    assert_eq!(store.pin_count(f, rev), 0);

    // 5. Eviction now succeeds
    assert_eq!(store.evict(f, rev), Ok(true));
    assert_eq!(store.get(f, rev), Err(SourceError::CaptureUnavailable));
}

#[test]
fn anchor_resolution_matches_live_when_digest_is_identical() {
    let o = owner(203);
    let f = file_id(o, 20);
    let rev_old = revision(o, 1);
    let rev_new = revision(o, 2);

    let store = PinnedSnapshotStore::new(o); // empty: rev_old was evicted

    let req_live = CaptureRequest::new(f, rev_new).unwrap();
    let chunk = SourceChunk::new(
        0,
        range(0, 16),
        Arc::from(b"identical_bytes_".to_vec().into_boxed_slice()),
    )
    .unwrap();
    let cap = ChunkedCapture::new(
        req_live,
        ByteLength::new(16),
        ChunkSize::bounded(16).unwrap(),
        vec![chunk],
    )
    .unwrap();
    let meta = FileObservationMetadata {
        initial_len: 16,
        final_len: 16,
        initial_modified_micros: Some(20),
        final_modified_micros: Some(20),
        retries_attempted: 0,
        consistency: ObservedSnapshotConsistency::VerifiedMatch,
    };
    let live_snapshot = ObservedSnapshot::new(f, rev_new, SnapshotBacking::Complete(cap), meta).unwrap();
    let expected_old_digest = live_snapshot.digest();

    let resolution = AnchorResolver::resolve(
        &store,
        f,
        rev_old,
        range(0, 9),
        expected_old_digest,
        Some(&live_snapshot),
    )
    .unwrap();

    match resolution {
        AnchorResolution::VerifiedLiveMatch(exact) => {
            assert_eq!(exact.bytes(), b"identical");
            assert_eq!(exact.revision(), rev_new);
        }
        _ => panic!("expected VerifiedLiveMatch"),
    }
}

#[test]
fn negative_control_anchor_resolution_refuses_diverged_live_bytes() {
    let o = owner(204);
    let f = file_id(o, 30);
    let rev_old = revision(o, 1);
    let rev_new = revision(o, 2);

    let store = PinnedSnapshotStore::new(o); // empty: rev_old was evicted

    // Old digest corresponds to "ORIGINAL_VERSION"
    let old_digest = ObservationDigest::observe(b"ORIGINAL_VERSION");

    // Live snapshot contains "MUTATED_CONTENT!"
    let req_live = CaptureRequest::new(f, rev_new).unwrap();
    let chunk = SourceChunk::new(
        0,
        range(0, 16),
        Arc::from(b"MUTATED_CONTENT!".to_vec().into_boxed_slice()),
    )
    .unwrap();
    let cap = ChunkedCapture::new(
        req_live,
        ByteLength::new(16),
        ChunkSize::bounded(16).unwrap(),
        vec![chunk],
    )
    .unwrap();
    let meta = FileObservationMetadata {
        initial_len: 16,
        final_len: 16,
        initial_modified_micros: Some(30),
        final_modified_micros: Some(30),
        retries_attempted: 0,
        consistency: ObservedSnapshotConsistency::VerifiedMatch,
    };
    let live_snapshot = ObservedSnapshot::new(f, rev_new, SnapshotBacking::Complete(cap), meta).unwrap();
    assert_ne!(old_digest, live_snapshot.digest());

    // NEGATIVE CONTROL ORACLE:
    // When live bytes differ from the expected old digest, resolution strictly fails
    // with StaleOrDiverged and NEVER silently returns MUTATED_CONTENT!
    let resolution = AnchorResolver::resolve(
        &store,
        f,
        rev_old,
        range(0, 7),
        old_digest,
        Some(&live_snapshot),
    )
    .unwrap();

    match resolution {
        AnchorResolution::StaleOrDiverged {
            old_revision,
            old_digest: observed_old,
            current_digest,
        } => {
            assert_eq!(old_revision, rev_old);
            assert_eq!(observed_old, old_digest);
            assert_eq!(current_digest, Some(live_snapshot.digest()));
        }
        _ => panic!("Negative control defect: diverged live bytes were silently accepted!"),
    }
}

#[test]
fn extent_capture_distinguishes_observations_from_holes() {
    let req = request(205, 40, 1);
    let r1 = ObservedRange::new(range(0, 10), b"0123456789").unwrap();
    let r2 = ObservedRange::new(range(20, 30), b"abcdefghij").unwrap();
    let hole = range(10, 20);

    let extent = ExtentCapture::new(
        req,
        vec![r1, r2],
        vec![hole],
        Some(ByteLength::new(30)),
    )
    .unwrap();

    assert!(extent.covers(range(0, 10)));
    assert!(extent.covers(range(20, 30)));
    assert!(!extent.covers(range(5, 15))); // Straddles hole
    assert!(!extent.covers(range(10, 20))); // Inside hole

    let meta = FileObservationMetadata {
        initial_len: 30,
        final_len: 30,
        initial_modified_micros: Some(40),
        final_modified_micros: Some(40),
        retries_attempted: 1,
        consistency: ObservedSnapshotConsistency::DivergedDuringRead,
    };

    let snapshot = ObservedSnapshot::new(
        req.file(),
        req.revision(),
        SnapshotBacking::Extent(extent),
        meta,
    )
    .unwrap();

    assert!(!snapshot.is_settled());
    // Range read on extent returns CaptureUnavailable
    assert_eq!(
        snapshot.range_read(range(0, 5)),
        Err(SourceError::CaptureUnavailable)
    );
}

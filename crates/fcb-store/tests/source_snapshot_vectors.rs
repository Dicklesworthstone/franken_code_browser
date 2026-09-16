#![forbid(unsafe_code)]

//! Independently constructed with Python struct and hashlib, not the production
//! envelope writer. This asserts the exact wire bytes, including a full-width
//! missing-source length, an empty capture, and the distinction between them.
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget};
use fcb_store::{Sha256, snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits, SnapshotView}};

const GOLDEN: &str = "464342533143525301000000000000009300000000000000010103000000000000000e00000000000000746573742d706f6c6963792d76310400000000000000612e7273060000000000000001060000000000000062616e616e610500000000000000656d707479000000000000000001000000000000000007000000000000006d697373696e67ffffffffffffffff001200000000000000534f555243455f554e415641494c41424c4575caa5886df7d54ee801071526fc71f9c503cf6c5bdfc755d0bb3b8988ad5b82";

#[test]
fn canonical_snapshot_matches_independent_struct_and_sha256_vector() {
    let golden: Vec<u8> = GOLDEN.as_bytes().chunks_exact(2).map(|pair|
        u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()).collect();
    assert_eq!(golden.len(), 203);
    assert_eq!(Sha256::digest(&golden).to_hex(), "f71a40be2c9ad1a5c8aae186a6373a0c6bbd4df3d14b200968ce35bb0aa9a3d8");
    let view = SnapshotView::open(&golden, SnapshotLimits::default(), || false).unwrap();
    let entries = [
        SnapshotEntry { path: b"a.rs", observed_bytes: 6, data: SnapshotData::Captured(b"banana") },
        SnapshotEntry { path: b"empty", observed_bytes: 0, data: SnapshotData::Captured(b"") },
        SnapshotEntry { path: b"missing", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") },
    ];
    assert_eq!(view.entries().collect::<Result<Vec<_>, _>>().unwrap(), entries);
    assert!(view.discovery_complete()); assert_eq!(view.captured_files(), 2); assert_eq!(view.source_bytes(), 6);
    let owner = ArenaOwnerId::new(741).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(1024 * 1024)).unwrap();
    let actual = SnapshotBytes::encode(owner, true, "test-policy-v1", &entries, SnapshotLimits::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    assert_eq!(actual.bytes(), golden);
    assert_eq!(actual.digest(), view.digest());
}

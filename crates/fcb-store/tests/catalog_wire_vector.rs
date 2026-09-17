#![forbid(unsafe_code)]

//! Constructed independently with Python struct/hashlib, not regenerated from
//! the production encoder. Tests source-layout offsets, empty/missing members,
//! full-width lengths and the distinction between envelope and trusted digests.

use std::io::Cursor;
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget};
use fcb_store::{Sha256, Sha256Digest};
use fcb_store::snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData, SnapshotLimits};
use fcb_store::paged_snapshot::{PagedSnapshot, PinnedCatalog, PagedMemberData};

const GOLDEN: &str = "46434243315441430100000000000000050100000000000075caa5886df7d54ee801071526fc71f9c503cf6c5bdfc755d0bb3b8988ad5b82cb0000000000000001010300000000000000100000000000000012000000000000000e00000000000000746573742d706f6c6963792d76310400000000000000612e72730600000000000000015500000000000000b493d48364afe44d11c0165cf470a4164d1e2609911ef998be868d46ade3de4e0500000000000000656d7074790000000000000000017900000000000000e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85507000000000000006d697373696e67ffffffffffffffff001200000000000000534f555243455f554e415641494c41424c45a50d63de4340fdfce16ce3206ca1243741051293f0ad2e05cd3d615b90daf54e";
const TRUSTED_FULL_DIGEST: &str = "2787a1b4465a9a21225f7e2cb11dd90595d04d80b62bda4875e325e0b0f84f59";
fn decode(hex: &str) -> Vec<u8> {
    hex.as_bytes().chunks_exact(2).map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()).collect()
}
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }

#[test]
fn encoder_and_pinned_reader_match_independent_canonical_bytes() {
    let expected = decode(GOLDEN);
    assert_eq!(expected.len(), 317);
    assert_eq!(Sha256::digest(&expected).to_hex(), TRUSTED_FULL_DIGEST);
    let pin = Sha256Digest::new(decode(TRUSTED_FULL_DIGEST).try_into().unwrap());
    let owner = ArenaOwnerId::new(1853).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(4 * 1024 * 1024)).unwrap();
    let entries = [SnapshotEntry { path: b"a.rs", observed_bytes: 6, data: SnapshotData::Captured(b"banana") },
        SnapshotEntry { path: b"empty", observed_bytes: 0, data: SnapshotData::Captured(b"") },
        SnapshotEntry { path: b"missing", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
    let source = SnapshotBytes::encode(owner, true, "test-policy-v1", &entries, SnapshotLimits::default(), &budget, id(1), || false).unwrap();
    assert_eq!(source.bytes().len(), 203);
    let full = PagedSnapshot::open(Cursor::new(source.bytes()), owner, SnapshotLimits::default(), &budget, id(2), || false).unwrap();
    let encoded = full.directory().encode_catalog(&budget, id(3), || false).unwrap();
    assert_eq!(encoded.bytes(), expected);
    assert_eq!(encoded.digest(), pin);
    let metadata = PinnedCatalog::decode_pinned(&expected, pin, owner, SnapshotLimits::default(), &budget, id(4), || false).unwrap();
    assert!(matches!(metadata.directory().member(0).unwrap().data, PagedMemberData::Captured { archive_offset: 85, byte_length: 6, .. }));
    assert!(matches!(metadata.directory().member(1).unwrap().data, PagedMemberData::Captured { archive_offset: 121, byte_length: 0, .. }));
    assert_eq!(metadata.directory().member(2).unwrap().observed_bytes, u64::MAX);
    let mut reopened = PagedSnapshot::open_pinned(Cursor::new(source.bytes()), metadata, || false).unwrap();
    assert_eq!(reopened.directory().validation_stats().bytes_read, 56);
    assert_eq!(reopened.load(0, &budget, id(5), || false).unwrap().bytes(), b"banana");
}

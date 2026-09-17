#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! Independent Python struct/hashlib reference, not a production-generated
//! golden. Pins identify the WHOLE sidecar, including its embedded checksum.
//! These tests distinguish syntax validation from trusted build provenance.

use std::io::Cursor;
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{IndexLimits, ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::snapshot_index::{SnapshotIndex, SnapshotIndexError, IndexDecision};
use fcb_store::{Sha256, Sha256Digest};

const GOLDEN: &str = "4643424931534f5001000000000000007100000000000000b000cc5c5a8de4a78fba375e123489a088b3086064b953579c5669f19883d78e0100000001000000000000000300000000000000020600000000000000b493d48364afe44d11c0165cf470a4164d1e2609911ef998be868d46ade3de4e0300000000000000616e61006e6162006e616e00cffcd7997d2ca81965bc89cb33bdc3ee397f04c46efbf1ac537ad162e8826bc7";
const PIN: &str = "118d5e1160de01067b1ad6e69d6317db9e7ecbcd02c001bdd52355b1a031cc64";
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(871).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap() }
fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes().chunks_exact(2).map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()).collect()
}
fn archive(budget: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    let entries = [SnapshotEntry { path: b"a", observed_bytes: 6, data: SnapshotData::Captured(b"banana") }];
    let bytes = SnapshotBytes::encode(owner(), true, "test-v1", &entries, SnapshotLimits::default(), budget, allocation(1), || false).unwrap();
    PagedSnapshot::open(Cursor::new(bytes.bytes().to_vec()), owner(), SnapshotLimits::default(), budget, allocation(2), || false).unwrap()
}
fn rechecksum(bytes: &mut [u8]) {
    let end = bytes.len() - 32;
    let sum = Sha256::digest(&bytes[..end]);
    bytes[end..].copy_from_slice(sum.as_bytes());
}

#[test]
fn production_builder_and_decoder_match_the_independent_fcbi1_vector() {
    let budget = budget(); let mut archive = archive(&budget);
    let expected = unhex(GOLDEN);
    assert_eq!(expected.len(), 169);
    assert_eq!(Sha256::digest(&expected).to_hex(), PIN);
    assert_eq!(archive.directory().digest().to_hex(), "b000cc5c5a8de4a78fba375e123489a088b3086064b953579c5669f19883d78e");
    let built = SnapshotIndex::build(&mut archive, IndexLimits::default(), &budget,
        [allocation(3), allocation(4), allocation(5), allocation(6)], || false).unwrap();
    let encoded = built.encode(&budget, allocation(7), || false).unwrap();
    assert_eq!(encoded.bytes(), expected);
    assert_eq!(encoded.digest().to_hex(), PIN);
    drop(built);
    let restored = SnapshotIndex::decode_pinned(&expected, encoded.digest(), archive.directory(), &budget, allocation(3), || false).unwrap();
    assert_eq!(restored.raw_decision(0, b"ana"), IndexDecision::Verify);
    assert_eq!(restored.text_decision(0, Some("absent")), IndexDecision::Excluded);
}

#[test]
fn a_forged_complete_segment_with_a_valid_internal_checksum_cannot_use_the_old_pin() {
    let budget = budget(); let archive = archive(&budget);
    let original = unhex(GOLDEN);
    let original_pin = Sha256::digest(&original);
    let mut forged = original.clone();
    // Keep valid header, member length/digest, count, ordering and 24-bit keys,
    // but replace all real source grams. This is syntactically valid omission.
    for index in 0..3 {
        forged[125 + index * 4..129 + index * 4].copy_from_slice(&(index as u32 + 1).to_le_bytes());
    }
    rechecksum(&mut forged);
    assert!(matches!(SnapshotIndex::decode_pinned(&forged, original_pin, archive.directory(), &budget, allocation(3), || false),
        Err(SnapshotIndexError::PinMismatch)));
    // Negative control: deliberately violate the host contract by trusting the
    // attacker's file's own digest. A checksum-only consumer WOULD lose hits.
    // This is intentionally not an allowed production source of trust.
    let unsafe_host_pin = Sha256::digest(&forged);
    let insecure = SnapshotIndex::decode_pinned(&forged, unsafe_host_pin, archive.directory(), &budget, allocation(3), || false).unwrap();
    assert_eq!(insecure.raw_decision(0, b"ana"), IndexDecision::Excluded);
    assert_eq!(b"banana".windows(3).filter(|bytes| *bytes == b"ana").count(), 2,
        "the independent literal oracle must detect the checksum-only false negative");
    assert_ne!(unsafe_host_pin, original_pin);
}

#[test]
fn trusted_bytes_still_need_valid_schema_counts_order_lengths_and_source_bindings() {
    let budget = budget(); let archive = archive(&budget); let original = unhex(GOLDEN);
    for (offset, value) in [(0, 0), (4, 0), (8, 2), (10, 1), (12, 1), (14, 1), (16, 1),
        (24, 0), (56, 2), (60, 2), (68, 255), (76, 7), (77, 255), (85, 0), (117, 255), (128, 255)] {
        let mut changed = original.clone(); changed[offset] = value; rechecksum(&mut changed);
        // Supplying newly computed pins is limited to this adversarial decoder
        // test, to exercise checks BEHIND the trusted-digest boundary itself.
        let pin = Sha256::digest(&changed);
        assert!(SnapshotIndex::decode_pinned(&changed, pin, archive.directory(), &budget, allocation(3), || false).is_err(),
            "malformed metadata at {offset} survived with a valid checksum");
    }
    let mut duplicate = original.clone();
    duplicate[129..133].copy_from_slice(&original[125..129]); rechecksum(&mut duplicate);
    assert!(SnapshotIndex::decode_pinned(&duplicate, Sha256::digest(&duplicate), archive.directory(), &budget, allocation(3), || false).is_err());
    assert!(matches!(SnapshotIndex::decode_pinned(&original, Sha256Digest::new([0; 32]), archive.directory(), &budget, allocation(3), || false),
        Err(SnapshotIndexError::PinMismatch)));
}

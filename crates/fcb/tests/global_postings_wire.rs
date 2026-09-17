#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! Wire vectors were constructed with Python struct/hashlib, independently of
//! EnvelopeWriter. Mutant pins below are deliberately re-signed ONLY to verify
//! structural rejection; production callers must retain pins from trusted builds.

use std::io::Cursor;
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{IndexLimits, ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits, Sha256Digest};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::snapshot_index::{SnapshotIndex, SnapshotPostings, SnapshotIndexError, PostingStep};
use fcb_store::Sha256;

const ARCHIVE: &str = "46434253314352530100000000000000b400000000000000010104000000000000001200000000000000706f7374696e67732d766563746f722d7631010000000000000061060000000000000001060000000000000061626361626301000000000000006206000000000000000106000000000000006263646263640500000000000000656d707479000000000000000001000000000000000007000000000000006d697373696e67ffffffffffffffff001200000000000000534f555243455f554e415641494c41424c4581fd9946fbdd6f572270da6ddebb83b295c09680b889679d18000c1a3b0e5540";
const POSTINGS: &str = "4643424f32534f500100000000000000280100000000000081fd9946fbdd6f572270da6ddebb83b295c09680b889679d18000c1a3b0e55400100000004000000000000000600000000000000020600000000000000bbb59da3af939f7af5f360f2ceb80a496e3bae1cd87dde426db0ae40677e1c2c0300000000000000020600000000000000aff5aa7ca8fc7d049f19067463d7f7cbed227f899ec6098564e292b17aff1ce30300000000000000020000000000000000e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855000000000000000000ffffffffffffffff000000000000000000000000000000000000000000000000000000000000000000000000000000000000000063626100000000006163620001000000646362000000000062616300010000006264630001000000636264009198ec93f1153ff204b0179911eb39927972c5843191f2ed16d6c58080b61992";
const PIN: &str = "5354aa37e6ef319690e7307c1aca2a06beb05577f293123ffc23fb08d77e7322";
fn bytes(text: &str) -> Vec<u8> {
    text.as_bytes().chunks_exact(2).map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()).collect()
}
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(4031).unwrap() }
fn id(value: u64) -> ResourceAllocationId { ResourceAllocationId::new(value).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn pin() -> Sha256Digest { Sha256Digest::new(bytes(PIN).try_into().unwrap()) }
fn archive(budget: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    PagedSnapshot::open(Cursor::new(bytes(ARCHIVE)), owner(), SnapshotLimits::default(), budget, id(1), || false).unwrap()
}
fn resign(bytes: &mut [u8]) -> Sha256Digest {
    let end = bytes.len() - 32;
    let checksum = Sha256::digest(&bytes[..end]);
    bytes[end..].copy_from_slice(checksum.as_bytes());
    Sha256::digest(bytes)
}

#[test]
fn exact_global_wire_image_matches_independent_struct_and_sha256_vector() {
    let budget = budget();
    let entries = [SnapshotEntry { path: b"a", observed_bytes: 6, data: SnapshotData::Captured(b"abcabc") },
        SnapshotEntry { path: b"b", observed_bytes: 6, data: SnapshotData::Captured(b"bcdbcd") },
        SnapshotEntry { path: b"empty", observed_bytes: 0, data: SnapshotData::Captured(b"") },
        SnapshotEntry { path: b"missing", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
    let source = SnapshotBytes::encode(owner(), true, "postings-vector-v1", &entries, SnapshotLimits::default(), &budget, id(2), || false).unwrap();
    assert_eq!(source.bytes(), bytes(ARCHIVE));
    let mut archive = archive(&budget);
    let segments = SnapshotIndex::build(&mut archive, IndexLimits::default(), &budget, [id(3), id(4), id(5), id(6)], || false).unwrap();
    let global = segments.invert(&budget, id(7), || false).unwrap();
    let encoded = global.encode(&budget, id(8), || false).unwrap();
    assert_eq!(encoded.bytes().len(), 352);
    assert_eq!(encoded.bytes(), bytes(POSTINGS));
    assert_eq!(encoded.digest(), pin());
    let reopened = SnapshotPostings::decode_pinned(&bytes(POSTINGS), pin(), archive.directory(), &budget, id(9), || false).unwrap();
    assert_eq!(reopened.stats().indexed_files, 3);
    assert_eq!(reopened.stats().unavailable_files, 1);
    assert_eq!(reopened.posting_count(), 6);
}

#[test]
fn every_single_byte_mutation_and_truncation_is_refused_under_the_original_pin() {
    let budget = budget(); let archive = archive(&budget); let baseline = budget.accounting().reserved().get();
    let original = bytes(POSTINGS);
    for position in 0..original.len() {
        let mut mutant = original.clone(); mutant[position] ^= 1;
        assert!(matches!(SnapshotPostings::decode_pinned(&mutant, pin(), archive.directory(), &budget, id(2), || false),
            Err(SnapshotIndexError::PinMismatch)), "position={position}");
        assert_eq!(budget.accounting().reserved().get(), baseline);
    }
    for end in 0..original.len() {
        assert!(SnapshotPostings::decode_pinned(&original[..end], pin(), archive.directory(), &budget, id(2), || false).is_err(), "end={end}");
        assert_eq!(budget.accounting().reserved().get(), baseline);
    }
}

#[test]
fn a_valid_checksum_and_matching_pin_do_not_admit_invalid_posting_structure() {
    let budget = budget(); let archive = archive(&budget); let original = bytes(POSTINGS);
    let baseline = budget.accounting().reserved().get();
    let mut mutants = Vec::new();
    for (offset, value) in [(4usize, 0u64), (56, 2), (60, 65537), (68, 0), (77, 999), (117, 0)] {
        let mut mutant = original.clone();
        let width = if offset == 4 || offset == 56 { 4 } else { 8 };
        mutant[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
        mutants.push(mutant);
    }
    for (offset, value) in [(8, 2), (10, 1), (12, 1), (24, 0), (76, 1), (85, 0)] {
        let mut mutant = original.clone(); mutant[offset] = value; mutants.push(mutant);
    }
    let first = 76 + 4 * 49;
    for pair in [(0x1000000u64 << 32), (0x616263u64 << 32) | 65536,
        (0x616263u64 << 32) | 2, (0x616263u64 << 32) | 3] {
        let mut mutant = original.clone(); mutant[first..first + 8].copy_from_slice(&pair.to_le_bytes()); mutants.push(mutant);
    }
    let mut duplicate = original.clone();
    duplicate[first..first + 8].copy_from_slice(&original[first + 8..first + 16]);
    mutants.push(duplicate);
    let mut reverse = original.clone();
    reverse[first..first + 8].copy_from_slice(&original[first + 8..first + 16]);
    reverse[first + 8..first + 16].copy_from_slice(&original[first..first + 8]);
    mutants.push(reverse);
    for (case, mut mutant) in mutants.into_iter().enumerate() {
        let forged_pin = resign(&mut mutant);
        assert!(SnapshotPostings::decode_pinned(&mutant, forged_pin, archive.directory(), &budget, id(2), || false).is_err(), "case {case}");
        assert_eq!(budget.accounting().reserved().get(), baseline);
    }
}

#[test]
fn omitting_a_gram_and_resigning_cannot_pass_the_separately_retained_original_pin() {
    let budget = budget(); let archive = archive(&budget); let mut forged = bytes(POSTINGS);
    // Delete the first (abc, file 0) posting and adjust both declared counts.
    // This is structurally valid but semantically false. Only the external pin
    // prevents it from becoming a negative certificate after reopening.
    let first = 76 + 4 * 49;
    forged.drain(first..first + 8);
    let payload = forged.len() as u64 - 56;
    forged[16..24].copy_from_slice(&payload.to_le_bytes());
    forged[68..76].copy_from_slice(&5u64.to_le_bytes());
    forged[117..125].copy_from_slice(&2u64.to_le_bytes());
    let forged_pin = resign(&mut forged);
    assert!(matches!(SnapshotPostings::decode_pinned(&forged, pin(), archive.directory(), &budget, id(2), || false),
        Err(SnapshotIndexError::PinMismatch)));
    // Intentional negative control: demonstrate why computing the expected pin
    // from untrusted input is forbidden. Structural validation is insufficient.
    let untrusted = SnapshotPostings::decode_pinned(&forged, forged_pin, archive.directory(), &budget, id(2), || false).unwrap();
    assert_eq!(untrusted.text_candidates("abc").step(), PostingStep::Finished);
    let valid = SnapshotPostings::decode_pinned(&bytes(POSTINGS), pin(), archive.directory(), &budget, id(3), || false).unwrap();
    assert!(matches!(valid.text_candidates("abc").step(), PostingStep::Candidate { ordinal: 0, .. }));
}

#[test]
fn decoded_index_cancellation_and_admission_do_not_leak_partial_tables() {
    let budget = budget(); let archive = archive(&budget); let original = bytes(POSTINGS);
    let baseline = budget.accounting().reserved().get();
    assert!(matches!(SnapshotPostings::decode_pinned(&original, pin(), archive.directory(), &budget, id(2),
        || budget.accounting().reserved().get() > baseline), Err(SnapshotIndexError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(SnapshotPostings::decode_pinned(&original, pin(), archive.directory(), &tiny, id(2), || false),
        Err(SnapshotIndexError::ResourceDenied)));
    let index = SnapshotPostings::decode_pinned(&original, pin(), archive.directory(), &budget, id(2), || false).unwrap();
    let retained = budget.accounting().reserved().get();
    let mut query = index.text_candidates("abc");
    while query.step() != PostingStep::Finished { assert_eq!(budget.accounting().reserved().get(), retained); }
    drop(query); drop(index); assert_eq!(budget.accounting().reserved().get(), baseline);
}

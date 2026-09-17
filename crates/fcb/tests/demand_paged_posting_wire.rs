#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! Independent Python struct/hashlib vector. Neither the production encoder nor
//! its decoder generated the reference bytes. Metadata trust and loaded-page
//! integrity are deliberately tested as separate contracts.

use std::io::{self, Cursor, Read, Seek, SeekFrom};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{IndexLimits, ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::SnapshotLimits;
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::snapshot_index::{SnapshotIndex, SnapshotIndexError, PostingStep};
use fcb::search::snapshot_index::paged::{PagedPostings, PagedPostingError};
use fcb_store::{Sha256, Sha256Digest};

const GOLDEN: &str = "464342440100000043010000000000007b0100000000000000000000000000004643504d33534f5001000000000000000b0100000000000019722d6135afd752c9672ddd7f58ff050c6c98398eeade0f599cbc1034bd896c0100000000080000030000000000000003000000000000000100000000000000020600000000000000b493d48364afe44d11c0165cf470a4164d1e2609911ef998be868d46ade3de4e0300000000000000020000000000000000e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855000000000000000000ffffffffffffffff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000616e6100000000006e616e0003000000000000003092dfb517f1cad0deb470d29fcb25b7b930ba58d6fe53fcdc5a8a31a031dff5ea9639d947089e6dcabbdff9c2660be8602c4a6218d73cc7e7d77ba18955ffa300000000616e6100000000006e616200000000006e616e00";
const SOURCE: &str = "464342533143525301000000000000009100000000000000010103000000000000000f0000000000000070616765642d766563746f722d7631010000000000000061060000000000000001060000000000000062616e616e610500000000000000656d707479000000000000000001000000000000000007000000000000006d697373696e67ffffffffffffffff001200000000000000534f555243455f554e415641494c41424c4519722d6135afd752c9672ddd7f58ff050c6c98398eeade0f599cbc1034bd896c";
const PIN: &str = "8d93493d7fc906574d38292e9511d976d89f2fbedf0ff0f5914aba7b2aabe658";
const BODY: usize = 355;
fn hex(text: &str) -> Vec<u8> {
    text.as_bytes().chunks_exact(2).map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()).collect()
}
fn pin() -> Sha256Digest { Sha256Digest::new(hex(PIN).try_into().unwrap()) }
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(4521).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap() }
fn archive(b: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    PagedSnapshot::open(Cursor::new(hex(SOURCE)), owner(), SnapshotLimits::default(), b, id(1), || false).unwrap()
}

#[test]
fn canonical_pages_match_independent_bytes_and_manifest_pin_is_not_full_file_hash() {
    let b = budget(); let mut source = archive(&b); let golden = hex(GOLDEN);
    assert_eq!(golden.len(), 379);
    assert_eq!(Sha256::digest(&golden).to_hex(), "33bd0819238fd08d9b336a26f5855b7e8c2bb0d7b7fb9640ef0dc6289e8a9c8c");
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(), &b, [id(2), id(3), id(4), id(5)], || false).unwrap();
    let inverse = forward.invert(&b, id(6), || false).unwrap();
    let encoded = inverse.encode_paged(&b, id(7), || false).unwrap();
    assert_eq!(encoded.bytes(), golden); assert_eq!(encoded.digest(), pin());
    assert_eq!(encoded.manifest_bytes(), BODY - 32);
    let mut index = PagedPostings::open_pinned(Cursor::new(&golden), pin(), source.directory(), 1, &b, id(8), || false).unwrap();
    assert_eq!(index.io_stats().page_bytes_read, 0);
    let mut candidates = index.text_candidates("ana");
    assert!(matches!(candidates.step(|| false).unwrap(), PostingStep::Candidate { ordinal: 0, .. }));
    assert_eq!(candidates.step(|| false).unwrap(), PostingStep::Finished);
    assert!(PagedPostings::open_pinned(Cursor::new(&golden), Sha256::digest(&golden), source.directory(), 1, &b, id(9), || false).is_err());
}

#[test]
fn every_metadata_mutation_and_every_truncation_is_rejected_on_open() {
    let b = budget(); let source = archive(&b); let golden = hex(GOLDEN);
    let baseline = b.accounting().reserved().get();
    for position in 0..BODY {
        let mut changed = golden.clone(); changed[position] ^= 1;
        assert!(PagedPostings::open_pinned(Cursor::new(changed), pin(), source.directory(), 1, &b, id(2), || false).is_err(), "metadata byte {position}");
        assert_eq!(b.accounting().reserved().get(), baseline);
    }
    for end in 0..golden.len() {
        assert!(PagedPostings::open_pinned(Cursor::new(&golden[..end]), pin(), source.directory(), 1, &b, id(2), || false).is_err(), "end {end}");
    }
    let mut appended = golden.clone(); appended.push(0);
    assert!(PagedPostings::open_pinned(Cursor::new(appended), pin(), source.directory(), 1, &b, id(2), || false).is_err());
}

#[test]
fn every_payload_mutation_is_refused_on_use_not_misreported_as_full_validation() {
    let b = budget(); let source = archive(&b); let golden = hex(GOLDEN);
    for position in BODY..golden.len() {
        let mut changed = golden.clone(); changed[position] ^= 1;
        let mut index = PagedPostings::open_pinned(Cursor::new(changed), pin(), source.directory(), 1, &b, id(2), || false).unwrap();
        assert_eq!(index.io_stats().page_bytes_read, 0);
        let mut candidates = index.raw_candidates(b"ana");
        assert_eq!(candidates.step(|| false), Err(PagedPostingError::ChangedPage), "payload byte {position}");
        assert_eq!(candidates.excluded_files(), None);
        assert_eq!(candidates.step(|| false), Err(PagedPostingError::ChangedPage));
    }
}

#[test]
fn resigned_malformed_metadata_cannot_escape_shape_order_or_source_binding_checks() {
    let b = budget(); let source = archive(&b); let golden = hex(GOLDEN);
    // Explicit forged expected pins here exercise structural checks, NOT a
    // suggested way to trust an untrusted index. All offsets are wire positions.
    for (position, value) in [(40, 2), (42, 1), (44, 1), (88, 2), (92, 1), (96, 4), (104, 4),
        (112, 2), (120, 9), (121, 7), (129, 0), (161, 2), (210, 1), (267, 3), (283, 2)] {
        let mut changed = golden.clone(); changed[position] = value;
        assert_ne!(changed, golden, "negative control must change bytes");
        let checksum = Sha256::digest(&changed[32..BODY - 32]);
        changed[BODY - 32..BODY].copy_from_slice(checksum.as_bytes());
        let forged_pin = Sha256::digest(&changed[32..BODY]);
        assert!(PagedPostings::open_pinned(Cursor::new(changed), forged_pin, source.directory(), 1, &b, id(2), || false).is_err(), "offset {position}");
    }
}

#[test]
fn interrupted_and_one_byte_readers_preserve_format_and_candidate_results() {
    struct Short { input: Cursor<Vec<u8>>, interrupt: bool }
    impl Read for Short {
        fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
            self.interrupt = !self.interrupt;
            if self.interrupt { return Err(io::Error::from(io::ErrorKind::Interrupted)); }
            let count = target.len().min(1); self.input.read(&mut target[..count])
        }
    }
    impl Seek for Short { fn seek(&mut self, from: SeekFrom) -> io::Result<u64> { self.input.seek(from) } }
    let b = budget(); let source = archive(&b);
    let input = Short { input: Cursor::new(hex(GOLDEN)), interrupt: false };
    let mut index = PagedPostings::open_pinned(input, pin(), source.directory(), 1, &b, id(2), || false).unwrap();
    assert_eq!(index.io_stats().manifest_bytes_read, BODY as u64);
    let mut candidates = index.raw_candidates(b"ana");
    assert!(matches!(candidates.step(|| false).unwrap(), PostingStep::Candidate { ordinal: 0, .. }));
    drop(candidates);
    assert_eq!(index.io_stats().page_bytes_read, 24);
    assert!(index.io_stats().read_calls > index.io_stats().manifest_bytes_read);
}

#[test]
fn dishonest_read_counts_and_foreign_directory_owners_are_explicit_failures() {
    struct Invalid;
    impl Read for Invalid { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(usize::MAX) } }
    impl Seek for Invalid { fn seek(&mut self, from: SeekFrom) -> io::Result<u64> { Ok(match from { SeekFrom::Start(n) => n, _ => 0 }) } }
    let b = budget(); let source = archive(&b);
    assert!(matches!(PagedPostings::open_pinned(Invalid, pin(), source.directory(), 1, &b, id(2), || false), Err(PagedPostingError::InvalidRead)));
    let index = PagedPostings::open_pinned(Cursor::new(hex(GOLDEN)), pin(), source.directory(), 1, &b, id(2), || false).unwrap();
    let foreign = ArenaOwnerId::new(4522).unwrap();
    let foreign_budget = ResourceBudget::new(foreign, ByteLength::new(16 * 1024 * 1024)).unwrap();
    let other = PagedSnapshot::open(Cursor::new(hex(SOURCE)), foreign, SnapshotLimits::default(), &foreign_budget, id(1), || false).unwrap();
    assert_eq!(index.validate_directory(other.directory()), Err(SnapshotIndexError::OwnerMismatch));
}

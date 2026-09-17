#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::io::{self, Cursor, Read, Seek, SeekFrom};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{IndexLimits, ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData, SnapshotLimits};
use fcb::search::snapshot_index::SnapshotIndex;
use fcb::search::snapshot_catalog::PinnedCatalog;
use fcb::search::paged_snapshot::{PagedSnapshot, PagedMemberData};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2961).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn encoded(entries: &[SnapshotEntry<'_>]) -> Vec<u8> {
    SnapshotBytes::encode(owner(), true, "test-v1", entries, SnapshotLimits::default(), &budget(), id(1), || false)
        .unwrap().bytes().to_vec()
}
fn open(entries: &[SnapshotEntry<'_>], b: &ResourceBudget, allocation: u64) -> PagedSnapshot<Cursor<Vec<u8>>> {
    PagedSnapshot::open(Cursor::new(encoded(entries)), owner(), SnapshotLimits::default(), b, id(allocation), || false).unwrap()
}
fn build<R: Read + Seek>(a: &mut PagedSnapshot<R>, b: &ResourceBudget) -> SnapshotIndex {
    SnapshotIndex::build(a, IndexLimits::default(), b, [id(3), id(4), id(5), id(6)], || false).unwrap()
}

/// A production read outside an explicitly permitted byte interval fails the
/// test, even if it is not included in production's I/O counters. Header/footer
/// checks and EOF are allowed; neither paths nor counters waive source checks.
struct Guard { input: Cursor<Vec<u8>>, permitted: Vec<(u64, u64)> }
impl Read for Guard {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let start = self.input.position();
        let end = (start + into.len() as u64).min(self.input.get_ref().len() as u64);
        assert!(start == end || self.permitted.iter().any(|&(a, b)| start >= a && end <= b),
            "unexpected source read {start}..{end}");
        self.input.read(into)
    }
}
impl Seek for Guard { fn seek(&mut self, to: SeekFrom) -> io::Result<u64> { self.input.seek(to) } }

#[test]
fn pinned_refresh_reads_only_changed_member_not_reused_or_duplicate_payloads() {
    let b = budget(); let large = vec![b'a'; 128 * 1024];
    let mut base = open(&[entry(b"old", &large)], &b, 1);
    let old = build(&mut base, &b);
    let target_entries = [entry(b"copied", &large), entry(b"edited", b"needle"), entry(b"moved", &large)];
    let full = open(&target_entries, &b, 2);
    let PagedMemberData::Captured { archive_offset, byte_length, .. } = full.directory().member(1).unwrap().data else { panic!() };
    let catalog = full.directory().encode_catalog(&b, id(7), || false).unwrap();
    let pin = PinnedCatalog::decode_pinned(catalog.bytes(), catalog.digest(), owner(), SnapshotLimits::default(), &b, id(8), || false).unwrap();
    let wire = encoded(&target_entries); let length = wire.len() as u64;
    let guard = Guard { input: Cursor::new(wire), permitted: vec![(0, 24), (length - 32, length),
        (archive_offset, archive_offset + byte_length as u64)] };
    drop(full);
    let mut target = PagedSnapshot::open_pinned(guard, pin, || false).unwrap();
    assert_eq!(target.directory().validation_stats().bytes_read, 56);
    let result = old.refresh(&mut target, IndexLimits::default(), &b, [id(20), id(21), id(22), id(23), id(24)], || false).unwrap();
    assert_eq!(result.stats().reused_files, 2);
    assert_eq!(result.stats().reused_source_bytes, 256 * 1024);
    assert_eq!(result.stats().rebuilt_files, 1);
    assert_eq!(target.load_stats().bytes_read, 6);
    assert_eq!(result.index().stats().build_source_bytes, 6);
    let refreshed = result.index().encode(&b, id(9), || false).unwrap();
    let mut independent = open(&target_entries, &b, 10);
    drop(old); // Fresh build can now use the same independent engine allocation IDs.
    let fresh = build(&mut independent, &b);
    let fresh_bytes = fresh.encode(&b, id(11), || false).unwrap();
    assert_eq!(refreshed.bytes(), fresh_bytes.bytes());
    assert_eq!(refreshed.digest(), fresh_bytes.digest());
}

#[test]
fn all_reused_refresh_on_pinned_target_performs_no_source_payload_read_at_all() {
    let b = budget(); let mut base = open(&[entry(b"a", b"banana")], &b, 1); let old = build(&mut base, &b);
    let entries = [entry(b"copy", b"banana"), entry(b"renamed", b"banana")];
    let full = open(&entries, &b, 2);
    let artifact = full.directory().encode_catalog(&b, id(7), || false).unwrap();
    let catalog = PinnedCatalog::decode_pinned(artifact.bytes(), artifact.digest(), owner(), SnapshotLimits::default(), &b, id(8), || false).unwrap();
    let wire = encoded(&entries); let length = wire.len() as u64;
    let guard = Guard { input: Cursor::new(wire), permitted: vec![(0, 24), (length - 32, length)] };
    let mut target = PagedSnapshot::open_pinned(guard, catalog, || false).unwrap();
    let result = old.refresh(&mut target, IndexLimits { max_source_bytes_total: 0, max_scratch_bytes: 0, ..IndexLimits::default() },
        &b, [id(20), id(21), id(22), id(23), id(24)], || false).unwrap();
    assert_eq!(target.load_stats().bytes_read, 0);
    assert_eq!(result.stats().reused_files, 2); assert_eq!(result.stats().attempted_files, 0);
    drop(old); drop(base); drop(full); drop(artifact); drop(target);
    assert_eq!(result.index().stats().members, 2);
    drop(result); assert_eq!(b.accounting().reserved().get(), 0);
}

#[test]
fn canceled_refresh_during_reuse_retains_old_keys_and_releases_every_candidate_lease() {
    let b = budget();
    let payload: Vec<u8> = (0u32..30_000).flat_map(u32::to_le_bytes).collect();
    let mut base = open(&[entry(b"a", &payload)], &b, 1); let old = build(&mut base, &b);
    let mut target = open(&[entry(b"b", &payload)], &b, 2);
    let baseline = b.accounting().reserved().get();
    let mut polls = 0;
    let result = old.refresh(&mut target, IndexLimits::default(), &b, [id(20), id(21), id(22), id(23), id(24)], || {
        polls += 1; polls >= 10
    });
    assert!(matches!(result, Err(fcb::search::snapshot_index::SnapshotIndexError::Canceled)));
    assert_eq!(b.accounting().reserved().get(), baseline);
    assert_eq!(target.load_stats().loaded_members, 0);
    assert_eq!(old.stats().indexed_files, 1);
}

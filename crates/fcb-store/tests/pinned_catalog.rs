#![forbid(unsafe_code)]

use std::io::{self, Cursor, Read, Seek, SeekFrom};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget};
use fcb_store::{Sha256, Sha256Digest, HEADER_LEN, CHECKSUM_LEN};
use fcb_store::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb_store::paged_snapshot::{CatalogArtifact, CatalogError, PagedMemberData,
    PagedSnapshot, PagedSnapshotError, PinnedCatalog};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1851).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn bytes(entries: &[SnapshotEntry<'_>], complete: bool) -> Vec<u8> {
    SnapshotBytes::encode(owner(), complete, "test-v1", entries, SnapshotLimits::default(), &budget(), id(1), || false)
        .unwrap().bytes().to_vec()
}
fn catalog(bytes: &[u8], budget: &ResourceBudget) -> CatalogArtifact {
    let archive = PagedSnapshot::open(Cursor::new(bytes), owner(), SnapshotLimits::default(), budget, id(1), || false).unwrap();
    archive.directory().encode_catalog(budget, id(2), || false).unwrap()
}
fn pin(artifact: &CatalogArtifact, budget: &ResourceBudget) -> PinnedCatalog {
    PinnedCatalog::decode_pinned(artifact.bytes(), artifact.digest(), owner(), SnapshotLimits::default(), budget, id(3), || false).unwrap()
}

#[test]
fn metadata_roundtrip_preserves_empty_missing_raw_paths_and_discovery_state() {
    let entries = [SnapshotEntry { path: b"a\\\xff", observed_bytes: 3, data: SnapshotData::Captured(b"a\xffz") },
        SnapshotEntry { path: b"empty", observed_bytes: 0, data: SnapshotData::Captured(b"") },
        SnapshotEntry { path: b"missing", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
    for complete in [false, true] {
        let bytes = bytes(&entries, complete); let budget = budget();
        let original = PagedSnapshot::open(Cursor::new(&bytes), owner(), SnapshotLimits::default(), &budget, id(1), || false).unwrap();
        let artifact = original.directory().encode_catalog(&budget, id(2), || false).unwrap();
        let mut reopened = PagedSnapshot::open_pinned(Cursor::new(&bytes), pin(&artifact, &budget), || false).unwrap();
        assert!(original.directory().fully_verified_on_open());
        assert!(!reopened.directory().fully_verified_on_open());
        assert_eq!(reopened.directory().catalog_pin(), Some(artifact.digest()));
        assert_eq!(reopened.directory().discovery_complete(), complete);
        assert_eq!(reopened.directory().members().collect::<Vec<_>>(), original.directory().members().collect::<Vec<_>>());
        assert_eq!(reopened.directory().digest(), original.directory().digest());
        assert_eq!(reopened.load(0, &budget, id(4), || false).unwrap().bytes(), b"a\xffz");
        assert!(reopened.load(1, &budget, id(4), || false).unwrap().bytes().is_empty());
        assert!(matches!(reopened.load(2, &budget, id(4), || false), Err(PagedSnapshotError::Missing)));
    }
}

/// Fails immediately if opening tries to read ANY byte outside header/footer.
struct BoundaryOnly { bytes: Cursor<Vec<u8>>, max_read: usize }
impl Read for BoundaryOnly {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let position = self.bytes.position() as usize;
        let len = self.bytes.get_ref().len();
        assert!(position < HEADER_LEN || position >= len - CHECKSUM_LEN, "unexpected body read at {position}");
        let n = out.len().min(self.max_read);
        if position < HEADER_LEN { assert!(position + n <= HEADER_LEN); }
        self.bytes.read(&mut out[..n])
    }
}
impl Seek for BoundaryOnly { fn seek(&mut self, from: SeekFrom) -> io::Result<u64> { self.bytes.seek(from) } }
#[test]
fn cold_open_reads_only_56_archive_bytes_independent_of_source_size_and_short_reads() {
    for size in [0, 17, 1024 * 1024] {
        let payload = vec![b'x'; size];
        let source = bytes(&[SnapshotEntry { path: b"a", observed_bytes: size as u64, data: SnapshotData::Captured(&payload) }], true);
        let budget = budget(); let artifact = catalog(&source, &budget);
        for max_read in [1, 3, 65536] {
            let guarded = BoundaryOnly { bytes: Cursor::new(source.clone()), max_read };
            let reopened = PagedSnapshot::open_pinned(guarded, pin(&artifact, &budget), || false).unwrap();
            assert_eq!(reopened.directory().validation_stats().bytes_read, 56);
            assert_eq!(reopened.load_stats().bytes_read, 0);
            assert!(reopened.directory().retained_charge() < 4096);
        }
    }
}

#[test]
fn body_mutation_is_explicitly_unchecked_until_selected_member_verification() {
    let original = bytes(&[SnapshotEntry { path: b"a", observed_bytes: 6, data: SnapshotData::Captured(b"banana") }], true);
    let budget = budget(); let artifact = catalog(&original, &budget);
    let metadata = pin(&artifact, &budget);
    let PagedMemberData::Captured { archive_offset, .. } = metadata.directory().member(0).unwrap().data else { panic!() };
    let mut changed = original.clone(); changed[archive_offset as usize] ^= 1;
    let mut reopened = PagedSnapshot::open_pinned(Cursor::new(changed.clone()), metadata, || false).unwrap();
    assert!(!reopened.directory().fully_verified_on_open());
    assert!(matches!(reopened.load(0, &budget, id(4), || false), Err(PagedSnapshotError::Changed)));
    assert!(PagedSnapshot::open(Cursor::new(changed), owner(), SnapshotLimits::default(), &budget, id(5), || false).is_err());
    assert!(matches!(reopened.directory().encode_catalog(&budget, id(6), || false), Err(CatalogError::FullValidationRequired)));
}

#[test]
fn wrong_pins_all_byte_mutations_and_truncations_refuse_before_catalog_publication() {
    let source = bytes(&[SnapshotEntry { path: b"a", observed_bytes: 3, data: SnapshotData::Captured(b"abc") }], true);
    let budget = budget(); let artifact = catalog(&source, &budget);
    let baseline = budget.accounting().reserved().get();
    assert!(matches!(PinnedCatalog::decode_pinned(artifact.bytes(), Sha256Digest::new([0; 32]), owner(), SnapshotLimits::default(),
        &budget, id(3), || false), Err(CatalogError::PinMismatch)));
    for i in 0..artifact.bytes().len() {
        let mut changed = artifact.bytes().to_vec(); changed[i] ^= 1;
        assert!(PinnedCatalog::decode_pinned(&changed, artifact.digest(), owner(), SnapshotLimits::default(), &budget, id(3), || false).is_err());
        assert!(PinnedCatalog::decode_pinned(&artifact.bytes()[..i], artifact.digest(), owner(), SnapshotLimits::default(), &budget, id(3), || false).is_err());
        assert_eq!(budget.accounting().reserved().get(), baseline);
    }
}

fn resign(bytes: &mut [u8]) -> Sha256Digest {
    let end = bytes.len() - CHECKSUM_LEN;
    let checksum = Sha256::digest(&bytes[..end]); bytes[end..].copy_from_slice(checksum.as_bytes());
    Sha256::digest(bytes)
}
#[test]
fn even_a_trusted_but_invalid_catalog_cannot_reference_noncanonical_offsets_or_huge_counts() {
    let source = bytes(&[SnapshotEntry { path: b"a", observed_bytes: 3, data: SnapshotData::Captured(b"abc") }], true);
    let budget = budget(); let artifact = catalog(&source, &budget);
    // FCBC prefix ends at 98 + policy.len(), then path length/path/observed/tag.
    let offset_field = 98 + 7 + 8 + 1 + 8 + 1;
    for (at, width) in [(offset_field, 8), (66, 8), (74, 8), (82, 8), (56, 8)] {
        let mut changed = artifact.bytes().to_vec(); changed[at..at + width].fill(255);
        let pin = resign(&mut changed);
        assert!(PinnedCatalog::decode_pinned(&changed, pin, owner(), SnapshotLimits::default(), &budget, id(3), || false).is_err());
    }
}

#[test]
fn matching_footer_is_not_self_authentication_negative_control() {
    let source = bytes(&[SnapshotEntry { path: b"a", observed_bytes: 3, data: SnapshotData::Captured(b"abc") }], true);
    let budget = budget(); let artifact = catalog(&source, &budget);
    let mut forged = artifact.bytes().to_vec();
    forged[65] = 0; // Unix path encoding; invalid even after new signatures.
    let forged_pin = resign(&mut forged);
    assert!(PinnedCatalog::decode_pinned(&forged, forged_pin, owner(), SnapshotLimits::default(), &budget, id(3), || false).is_err());
    let mut renamed = artifact.bytes().to_vec(); renamed[98 + 7 + 8] = b'b';
    let attacker_pin = resign(&mut renamed);
    assert!(matches!(PinnedCatalog::decode_pinned(&renamed, artifact.digest(), owner(), SnapshotLimits::default(), &budget, id(3), || false),
        Err(CatalogError::PinMismatch)));
    // Intentional WRONG host: deriving the expected pin from adversarial input
    // accepts a self-consistent rename. This proves why trust must be external.
    let bad_host = PinnedCatalog::decode_pinned(&renamed, attacker_pin, owner(), SnapshotLimits::default(), &budget, id(3), || false).unwrap();
    assert_eq!(bad_host.directory().member(0).unwrap().path, b"b");
}

#[test]
fn archive_boundary_length_header_and_footer_mismatches_cannot_attach() {
    let source = bytes(&[], true); let budget = budget(); let artifact = catalog(&source, &budget);
    for at in [0, 4, 8, 10, 12, 16, source.len() - 1] {
        let mut changed = source.clone(); changed[at] ^= 1;
        assert!(PagedSnapshot::open_pinned(Cursor::new(changed), pin(&artifact, &budget), || false).is_err());
    }
    let mut longer = source.clone(); longer.push(0);
    assert!(PagedSnapshot::open_pinned(Cursor::new(longer), pin(&artifact, &budget), || false).is_err());
    assert!(PagedSnapshot::open_pinned(Cursor::new(&source[..source.len()-1]), pin(&artifact, &budget), || false).is_err());
}

#[test]
fn canceled_and_denied_candidates_release_only_their_own_metadata() {
    let source = bytes(&[], true); let budget = budget(); let artifact = catalog(&source, &budget);
    let baseline = budget.accounting().reserved().get();
    let denied = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(PinnedCatalog::decode_pinned(artifact.bytes(), artifact.digest(), owner(), SnapshotLimits::default(), &denied, id(3), || false).is_err());
    assert!(matches!(PinnedCatalog::decode_pinned(artifact.bytes(), artifact.digest(), owner(), SnapshotLimits::default(), &budget, id(3),
        || budget.accounting().reserved().get() > baseline), Err(CatalogError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    assert!(matches!(PagedSnapshot::open_pinned(Cursor::new(source), pin(&artifact, &budget), || true), Err(CatalogError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    drop(artifact); assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn restored_metadata_uses_the_new_owner_not_serialized_handle_identities() {
    let source = bytes(&[], false); let budget = budget(); let artifact = catalog(&source, &budget);
    let other = ArenaOwnerId::new(1852).unwrap();
    let other_budget = ResourceBudget::new(other, ByteLength::new(1024 * 1024)).unwrap();
    let catalog = PinnedCatalog::decode_pinned(artifact.bytes(), artifact.digest(), other, SnapshotLimits::default(), &other_budget, id(1), || false).unwrap();
    let reopened = PagedSnapshot::open_pinned(Cursor::new(source), catalog, || false).unwrap();
    assert_eq!(reopened.directory().owner(), other);
    assert!(!reopened.directory().discovery_complete());
    drop(reopened); assert_eq!(other_budget.accounting().reserved().get(), 0);
}

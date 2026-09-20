#![allow(dead_code)]

use std::{fs::{self, File}, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{IndexLimits, ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::{SnapshotBytes, SnapshotEntry};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::snapshot_index::SnapshotIndex;
use fcb_app::host::saved_repository::{SavedRepositorySession, Sha256Digest};

pub fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
pub fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64,
        data: fcb::search::snapshot::SnapshotData::Captured(bytes) }
}
pub struct Fixture { pub root: PathBuf, pub path: PathBuf, pub bytes: Vec<u8> }
impl Fixture {
    pub fn new(entries: &[SnapshotEntry<'_>], complete: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-saved-desk-{}-{stamp}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        let budget = ResourceBudget::new(owner(55001), ByteLength::new(64 * 1024 * 1024)).unwrap();
        let encoded = SnapshotBytes::encode(owner(55001), complete, "saved-desk-test", entries,
            Default::default(), &budget, id(1), || false).unwrap();
        let bytes = encoded.bytes().to_vec(); let path = root.join("repository.fcbs");
        fs::write(&path, &bytes).unwrap(); Self { root, path, bytes }
    }
    pub fn open(&self, n: u64) -> SavedRepositorySession {
        SavedRepositorySession::open(owner(n), &self.path, Default::default(), || false).unwrap()
    }
    pub fn index(&self) -> (PathBuf, Sha256Digest) {
        let budget = ResourceBudget::new(owner(55002), ByteLength::new(64 * 1024 * 1024)).unwrap();
        let mut archive = PagedSnapshot::open(File::open(&self.path).unwrap(), owner(55002),
            Default::default(), &budget, id(1), || false).unwrap();
        let forward = SnapshotIndex::build(&mut archive, IndexLimits::default(), &budget,
            [id(2), id(3), id(4), id(5)], || false).unwrap();
        let inverse = forward.invert(&budget, id(6), || false).unwrap();
        let encoded = inverse.encode_paged(&budget, id(7), || false).unwrap();
        let path = self.root.join("repository.fcbd"); fs::write(&path, encoded.bytes()).unwrap();
        (path, encoded.digest())
    }
}

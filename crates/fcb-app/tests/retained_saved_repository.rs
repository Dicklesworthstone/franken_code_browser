#![forbid(unsafe_code)]
#![cfg(unix)]

use std::{fs::{self, File, OpenOptions}, io::{Seek, SeekFrom, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{IndexLimits, ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::snapshot_index::SnapshotIndex;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::saved_repository::{SavedRepositorySession, SavedRepositoryError, SnapshotLimits, Sha256Digest};

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
struct Fixture { root: PathBuf, path: PathBuf, bytes: Vec<u8> }
impl Fixture {
    fn new(entries: &[SnapshotEntry<'_>], complete: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-retained-saved-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        let budget = ResourceBudget::new(owner(8811), ByteLength::new(64 * 1024 * 1024)).unwrap();
        let encoded = SnapshotBytes::encode(owner(8811), complete, "test", entries, Default::default(),
            &budget, allocation(1), || false).unwrap();
        let bytes = encoded.bytes().to_vec(); let path = root.join("repo.fcbs");
        fs::write(&path, &bytes).unwrap(); Self { root, path, bytes }
    }
    fn open(&self) -> SavedRepositorySession {
        SavedRepositorySession::open(owner(8812), &self.path, Default::default(), || false).unwrap()
    }
    fn index(&self) -> (PathBuf, Sha256Digest) {
        let budget = ResourceBudget::new(owner(8813), ByteLength::new(64 * 1024 * 1024)).unwrap();
        let mut archive = PagedSnapshot::open(File::open(&self.path).unwrap(), owner(8813), Default::default(),
            &budget, allocation(1), || false).unwrap();
        let forward = SnapshotIndex::build(&mut archive, IndexLimits::default(), &budget,
            [allocation(2), allocation(3), allocation(4), allocation(5)], || false).unwrap();
        let inverse = forward.invert(&budget, allocation(6), || false).unwrap();
        let encoded = inverse.encode_paged(&budget, allocation(7), || false).unwrap();
        let path = self.root.join("repo.fcbd"); fs::write(&path, encoded.bytes()).unwrap();
        (path, encoded.digest())
    }
}
fn field(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}

#[test]
fn metadata_pages_load_no_member_payload_and_keep_missing_distinct_from_empty() {
    let f = Fixture::new(&[entry(b"a", b""), SnapshotEntry { path: b"b", observed_bytes: 99,
        data: SnapshotData::Unavailable("READ_DENIED") }, entry(b"c", b"needle")], true);
    let mut session = f.open();
    let first = session.members(0, 2, || false).unwrap();
    assert!(first.as_str().contains("\"captured\":true"));
    assert!(first.as_str().contains("\"reason\":\"READ_DENIED\""));
    assert_eq!(field(first.as_str(), "next_offset"), 2);
    assert!(session.members(2, 1, || false).unwrap().as_str().contains("\"next_offset\":null"));
    let info = session.info(|| false).unwrap();
    assert_eq!(field(info.as_str(), "member_bytes_read"), 0);
    assert_eq!(field(info.as_str(), "initial_archive_bytes_read"), f.bytes.len() as u64);
    assert!(session.open_member_reader(owner(8814), 1, || false).is_err());
    let (empty, _) = session.open_member_reader(owner(8814), 0, || false).unwrap();
    assert!(empty.capture().bytes().is_empty());
}

#[test]
fn repeated_queries_and_paging_retain_no_matching_source_payloads() {
    let f = Fixture::new(&[entry(b"a", b"needle"), entry(b"b", b"none"), entry(b"c", b"needle needle")], true);
    let mut session = f.open();
    let result = session.search(1, "needle", 10, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK); assert_eq!(field(result.as_str(), "retained_hits"), 3);
    assert_eq!(field(result.as_str(), "retained_source_payload_bytes"), 0);
    assert_eq!(field(result.as_str(), "peak_source_bytes"), 13);
    let before = field(session.info(|| false).unwrap().as_str(), "member_bytes_read");
    for (start, expected) in [(0, 1), (1, 2), (2, 3)] {
        let page = session.results(1, start, 1, || false).unwrap();
        assert_eq!(field(page.as_str(), "hit_id"), expected);
    }
    assert_eq!(field(session.info(|| false).unwrap().as_str(), "member_bytes_read"), before);
    let next = session.search(2, "none", 10, || false).unwrap();
    assert_eq!(field(next.as_str(), "retained_hits"), 1);
    assert_eq!(session.results(1, 0, 1, || false).err(), Some(SavedRepositoryError::StaleQuery));
}

#[test]
fn demand_paged_index_is_reused_across_queries_and_detach_keeps_prior_results() {
    let f = Fixture::new(&[entry(b"a", b"needle"), entry(b"b", b"unrelated"), entry(b"c", b"another")], true);
    let (index, pin) = f.index(); let mut session = f.open();
    let attached = session.attach_index(1, &index, pin, || false).unwrap();
    assert_eq!(field(attached.as_str(), "index_page_loads"), 0);
    let first = session.search(1, "needle", 10, || false).unwrap();
    assert_eq!(field(first.as_str(), "retained_hits"), 1);
    assert_eq!(field(first.as_str(), "member_bytes_read"), 6);
    assert_eq!(field(first.as_str(), "index_eliminated_files"), 2);
    let info = session.info(|| false).unwrap();
    let loads = field(info.as_str(), "index_page_loads"); assert!(loads > 0);
    let cache_hits = field(info.as_str(), "index_cache_hits");
    session.search(2, "needle", 10, || false).unwrap();
    let info = session.info(|| false).unwrap();
    assert_eq!(field(info.as_str(), "index_page_loads"), loads);
    assert!(field(info.as_str(), "index_cache_hits") > cache_hits);
    assert!(field(info.as_str(), "index_cache_capacity_bytes") <= 64 * 1024);
    session.detach_index(2, || false).unwrap();
    assert_eq!(session.index_generation(), None);
    assert!(session.results(2, 0, 1, || false).unwrap().as_str().contains("\"index_generation\":\"1\""));
    assert_eq!(field(session.search(3, "needle", 10, || false).unwrap().as_str(), "member_bytes_read"), 22);
}

#[test]
fn archive_path_replacement_cannot_rebind_the_open_session_or_its_hit_reader() {
    let f = Fixture::new(&[entry(b"source.rs", b"fn original() { /* needle */ }\n")], true);
    let replacement = Fixture::new(&[entry(b"source.rs", b"fn replacement() {}\n")], true);
    let mut session = f.open();
    fs::rename(&f.path, f.root.join("retained.fcbs")).unwrap();
    fs::write(&f.path, replacement.bytes).unwrap();
    session.search(1, "needle", 10, || false).unwrap();
    let (mut reader, receipt) = session.open_hit_reader(owner(8814), 1, 1, || false).unwrap();
    assert!(receipt.as_str().contains("\"live_source_reopened\":false"));
    assert_eq!(reader.capture().bytes(), b"fn original() { /* needle */ }\n");
    session.clear_results(2, || false).unwrap(); drop(session);
    let result = reader.search(1, "original", 10, 1024, || false).unwrap();
    assert_eq!(field(result.as_str(), "retained_hits"), 1);
}

#[test]
fn mutable_archive_damage_is_refused_before_a_reader_can_be_returned() {
    let f = Fixture::new(&[entry(b"a", b"unique needle source")], true);
    let mut session = f.open(); session.search(1, "needle", 10, || false).unwrap();
    let at = f.bytes.windows(20).position(|b| b == b"unique needle source").unwrap();
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(at as u64)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(session.open_hit_reader(owner(8814), 1, 1, || false).is_err());
    assert_eq!(session.accepted_generation(), Some(1));
    assert_eq!(field(session.results(1, 0, 10, || false).unwrap().as_str(), "retained_hits"), 1);
}

#[test]
fn utf16_overlaps_and_raw_native_names_open_exact_original_bytes() {
    let mut bytes = vec![0xff, 0xfe];
    for unit in "banana".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    let f = Fixture::new(&[entry(b"raw-\xff.rs", &bytes)], true); let mut session = f.open();
    assert_eq!(field(session.search(1, "ana", 10, || false).unwrap().as_str(), "retained_hits"), 2);
    for (id, start, end) in [(1, 4, 10), (2, 8, 14)] {
        let (reader, receipt) = session.open_hit_reader(owner(8813 + id), 1, id, || false).unwrap();
        assert_eq!(reader.capture().bytes(), bytes);
        assert!(receipt.as_str().contains(&format!("\"original_range\":{{\"start\":\"{start}\",\"end\":\"{end}\"}}")));
        assert!(receipt.as_str().contains("\"original_hex\":\"61006e006100\""));
    }
}

#[test]
fn every_observed_query_cancellation_checkpoint_preserves_the_previous_result() {
    let f = Fixture::new(&[entry(b"a", b"old new new"), entry(b"b", b"tail")], true);
    let mut session = f.open();
    let mut polls = 0;
    session.search(1, "old", 10, || { polls += 1; false }).unwrap();
    // Count the exact replacement path separately before injecting one-shot stops.
    let mut replacement_polls = 0;
    session.search(2, "new", 10, || { replacement_polls += 1; false }).unwrap();
    assert!(polls > 0 && replacement_polls > 0);
    for at in 1..=replacement_polls {
        let mut n = 0;
        let result = session.search(2 + at as u64, "new", 10, || { n += 1; n == at });
        assert!(result.is_err(), "checkpoint {at}");
        assert_eq!(session.accepted_generation(), Some(2));
        assert_eq!(field(session.results(2, 0, 10, || false).unwrap().as_str(), "retained_hits"), 2);
    }
}

#[test]
fn bad_index_pins_and_foreign_archives_do_not_discard_the_accepted_index_or_rows() {
    let f = Fixture::new(&[entry(b"a", b"needle")], true); let (index, pin) = f.index();
    let foreign = Fixture::new(&[entry(b"a", b"different")], true); let (foreign_index, foreign_pin) = foreign.index();
    let mut session = f.open(); session.attach_index(1, &index, pin, || false).unwrap();
    session.search(1, "needle", 10, || false).unwrap();
    assert!(session.attach_index(2, &index, Sha256Digest::new([0; 32]), || false).is_err());
    assert!(session.attach_index(3, &foreign_index, foreign_pin, || false).is_err());
    assert!(session.attach_index(4, &index, pin, || true).is_err());
    assert_eq!(session.index_generation(), Some(1)); assert_eq!(session.accepted_generation(), Some(1));
    assert_eq!(field(session.search(2, "needle", 10, || false).unwrap().as_str(), "retained_hits"), 1);
}

#[test]
fn completeness_truncation_and_empty_success_are_distinct() {
    let f = Fixture::new(&[entry(b"a", b"needle"), entry(b"b", b"nothing")], true);
    let mut session = f.open();
    let exact = session.search(1, "needle", 1, || false).unwrap();
    assert_eq!(exact.exit_code(), EXIT_OK); assert!(exact.as_str().contains("\"truncated\":false"));
    let absent = session.search(2, "absent", 10, || false).unwrap();
    assert_eq!(absent.exit_code(), EXIT_OK); assert_eq!(field(absent.as_str(), "retained_hits"), 0);
    let partial = Fixture::new(&[entry(b"a", b"needle needle")], false);
    let mut session = partial.open();
    let result = session.search(1, "needle", 1, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL); assert_eq!(field(result.as_str(), "matches_seen"), 2);
    assert!(result.as_str().contains("\"truncated\":true"));
}

#[test]
fn failed_limits_and_generation_exhaustion_leave_a_usable_query() {
    let f = Fixture::new(&[entry(b"a", b"path:src")], true); let mut session = f.open();
    assert_eq!(field(session.search(1, "path:src", 10, || false).unwrap().as_str(), "retained_hits"), 1);
    assert!(session.search(2, &"x".repeat(1025), 10, || false).is_err());
    assert!(session.results(1, usize::MAX, 10, || false).is_err());
    assert!(session.members(0, 129, || false).is_err());
    assert_eq!(session.accepted_generation(), Some(1));
    session.search(u64::MAX, "path:src", 10, || false).unwrap();
    assert_eq!(session.clear_results(1, || false).err(), Some(SavedRepositoryError::StaleQuery));
    assert_eq!(session.accepted_generation(), Some(u64::MAX));
    let limits = SnapshotLimits { max_file_bytes: 8 * 1024 * 1024, ..Default::default() };
    assert!(SavedRepositorySession::open(owner(8815), &f.path, limits, || false).is_err());
}

#[test]
fn saved_markdown_member_uses_the_existing_retained_document_workflow() {
    let f = Fixture::new(&[entry(b"README.md", b"# Overview\n\nOriginal **documentation**.\n")], true);
    let mut session = f.open();
    let (mut reader, _) = session.open_member_reader(owner(8814), 0, || false).unwrap();
    drop(session);
    let preview = reader.prepare_document(1, Default::default(), || false).unwrap();
    assert!(preview.as_str().contains("Overview"));
    assert_eq!(reader.capture().bytes(), b"# Overview\n\nOriginal **documentation**.\n");
}

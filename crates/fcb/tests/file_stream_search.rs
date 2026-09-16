#![forbid(unsafe_code)]
#![cfg(all(feature = "search", unix))]

use std::{fs::{self, File, OpenOptions}, io::{Read, Seek, SeekFrom, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb::search::{CaptureRequest, ExtentConsistency, FileSearch, ResourceAllocationId, ResourceBudget,
    QueryGeneration, StreamReadOptions, StreamReadState, StreamReadStep, StreamingNeedle};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1315).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn request() -> CaptureRequest { CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-stream-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self) -> PathBuf { self.0.join("source") }
}
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn finish(query: &mut FileSearch<'_>) {
    for _ in 0..100_000 {
        if query.state() != StreamReadState::Pending { return; }
        let _ = query.step(StreamReadStep::default(), query.generation(), || false);
    }
    panic!("file scan did not terminate");
}

#[test]
fn finds_cross_buffer_and_late_matches_beyond_the_complete_capture_limit() {
    let temp = Temp::new();
    let mut bytes = vec![b'x'; 2 * 1024 * 1024];
    for offset in [16_382, bytes.len() - 10] { bytes[offset..offset + 6].copy_from_slice(b"needle"); }
    fs::write(temp.file(), &bytes).unwrap();
    let budget = budget();
    let pattern = StreamingNeedle::text(owner(), "needle", &budget, allocation(1)).unwrap();
    let mut job = FileSearch::new(File::open(temp.file()).unwrap(), request(), &pattern,
        StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    let charge = budget.accounting().reserved().get();
    while job.state() == StreamReadState::Pending {
        job.step(StreamReadStep::default(), generation(1), || false).unwrap();
        assert_eq!(budget.accounting().reserved().get(), charge);
    }
    let report = job.finish().unwrap();
    assert!(report.is_complete());
    assert_eq!(report.search().hits().len(), 2);
    assert_eq!(report.search().hits()[1].original_range().start().get(), bytes.len() as u64 - 10);
    assert_eq!(report.search().stats().bytes_read, bytes.len() as u64);
    assert!(report.search().stats().peak_buffer_bytes <= 16 * 1024);
}

#[test]
fn positioned_scan_does_not_move_a_host_shared_file_cursor() {
    let temp = Temp::new(); fs::write(temp.file(), b"abc needle tail").unwrap();
    let mut host = File::open(temp.file()).unwrap(); host.seek(SeekFrom::Start(3)).unwrap();
    let budget = budget();
    let pattern = StreamingNeedle::raw(owner(), b"needle", &budget, allocation(1)).unwrap();
    let mut query = FileSearch::new(host.try_clone().unwrap(), request(), &pattern,
        StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    finish(&mut query);
    assert!(query.finish().unwrap().is_complete());
    assert_eq!(host.stream_position().unwrap(), 3);
    let mut next = [0u8; 1]; host.read_exact(&mut next).unwrap(); assert_eq!(next, [b' ']);
}

#[test]
fn literal_evidence_survives_live_mutation_and_does_not_fabricate_neighboring_bytes() {
    let temp = Temp::new(); fs::write(temp.file(), b"before needle after").unwrap();
    let budget = budget();
    let evidence = {
        let pattern = StreamingNeedle::text(owner(), "needle", &budget, allocation(1)).unwrap();
        let mut query = FileSearch::new(File::open(temp.file()).unwrap(), request(), &pattern,
            StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
        finish(&mut query);
        let report = query.finish().unwrap();
        assert!(report.is_complete());
        report.retain_hit(0, &budget, allocation(3)).unwrap()
    };
    fs::write(temp.file(), b"changed and no matching text").unwrap();
    let view = BrowserSession::new(owner()).open_extent(evidence.clone()).unwrap();
    assert_eq!(view.raw_selection(evidence.range()).unwrap(), b"needle");
    assert_eq!(evidence.range().start().get(), 7);
    let neighbor = ByteRange::new(ByteOffset::new(6), ByteOffset::new(13)).unwrap();
    assert!(view.raw_selection(neighbor).is_err());
    assert_eq!(evidence.request().revision(), request().revision());
}

#[test]
fn growth_and_short_reads_are_detected_on_the_same_open_object() {
    for grow in [false, true] {
        let temp = Temp::new(); fs::write(temp.file(), vec![b'x'; 64 * 1024]).unwrap();
        let budget = budget();
        let pattern = StreamingNeedle::raw(owner(), b"absent", &budget, allocation(1)).unwrap();
        let mut query = FileSearch::new(File::open(temp.file()).unwrap(), request(), &pattern,
            StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
        query.step(StreamReadStep::default(), generation(1), || false).unwrap();
        let writer = OpenOptions::new().write(true).open(temp.file()).unwrap();
        writer.set_len(if grow { 128 * 1024 } else { 0 }).unwrap();
        finish(&mut query);
        let report = query.finish().unwrap();
        assert!(!report.is_complete());
        assert_eq!(report.consistency(), if grow { ExtentConsistency::ChangedDuringRead } else { ExtentConsistency::ShortRead });
    }
}

#[test]
fn namespace_replacement_does_not_reopen_a_different_file_mid_observation() {
    let temp = Temp::new();
    let mut bytes = vec![b'x'; 32 * 1024]; bytes.extend_from_slice(b"needle");
    fs::write(temp.file(), &bytes).unwrap();
    let budget = budget();
    let pattern = StreamingNeedle::text(owner(), "needle", &budget, allocation(1)).unwrap();
    let mut query = FileSearch::new(File::open(temp.file()).unwrap(), request(), &pattern,
        StreamReadOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    query.step(StreamReadStep::default(), generation(1), || false).unwrap();
    fs::rename(temp.file(), temp.0.join("retained-original")).unwrap();
    fs::write(temp.file(), b"unrelated replacement").unwrap();
    finish(&mut query);
    let report = query.finish().unwrap();
    assert!(report.is_complete()); assert_eq!(report.search().hits().len(), 1);
    assert_eq!(report.search().witness_bytes(0).unwrap(), b"needle");
}

#[test]
fn huge_sparse_file_is_not_loaded_or_claimed_complete_after_a_small_budget() {
    let temp = Temp::new();
    let mut writer = OpenOptions::new().create_new(true).write(true).open(temp.file()).unwrap();
    writer.write_all(b"needle").unwrap(); writer.set_len((1u64 << 32) + 17).unwrap();
    let budget = budget();
    let pattern = StreamingNeedle::raw(owner(), b"needle", &budget, allocation(1)).unwrap();
    let mut options = StreamReadOptions::new(generation(1)); options.max_bytes = 64;
    let mut query = FileSearch::new(File::open(temp.file()).unwrap(), request(), &pattern,
        options, &budget, allocation(2)).unwrap();
    finish(&mut query);
    let report = query.finish().unwrap();
    assert!(!report.is_complete()); assert_eq!(report.search().state(), StreamReadState::ByteLimit);
    assert_eq!(report.search().observed_length().get(), (1u64 << 32) + 17);
    assert_eq!(report.search().stats().bytes_read, 64);
    assert_eq!(report.search().hits().len(), 1);
}

#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::{io::Cursor, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{QueryGeneration, ResourceAllocationId, ResourceBudget, StreamingNeedle,
    StreamReadStep, ReaderLimits, ReadingTarget, ReadingSeekState, ReadingWindowOptions,
    CaptureRequest, CompleteCapture, DirectSourceScanner, QueryOptions};
use fcb::search::snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData, SnapshotLimits};
use fcb::search::paged_snapshot::{PagedSnapshot, PagedSnapshotError, PagedQuery, PagedQueryOptions,
    PagedQueryState, PagedSearchError, PagedCapture, PagedReport};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1851).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(32 * 1024 * 1024)).unwrap() }
fn options(max_matches: usize) -> PagedQueryOptions {
    PagedQueryOptions { generation: generation(1), first_file: FileId::new(owner(), 100).unwrap(),
        first_revision: SourceRevision::new(owner(), 200).unwrap(), max_matches }
}
fn encode(entries: &[SnapshotEntry<'_>], complete: bool) -> Vec<u8> {
    let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
    SnapshotBytes::encode(owner(), complete, "test-v1", entries, SnapshotLimits::default(), &budget, id(1), || false).unwrap().bytes().to_vec()
}
fn open(bytes: Vec<u8>, b: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    PagedSnapshot::open(Cursor::new(bytes), owner(), SnapshotLimits::default(), b, id(1), || false).unwrap()
}
fn query<R: std::io::Read + std::io::Seek>(archive: &mut PagedSnapshot<R>, needle: &StreamingNeedle,
    limit: usize, b: &ResourceBudget, quantum: usize) -> PagedReport {
    let mut query = PagedQuery::new(archive, needle, options(limit), b, [id(3), id(4), id(5)]).unwrap();
    for _ in 0..1_000_000 {
        if query.state() != PagedQueryState::Pending { break; }
        query.step(StreamReadStep { max_bytes: quantum, ..Default::default() }, generation(1), b, || false).unwrap();
        assert!(query.stats().last_step_scanned_bytes <= quantum.min(16 * 1024));
    }
    query.finish().unwrap()
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}

#[test]
fn paged_search_matches_existing_capture_scanner_in_both_utf16_orders_and_utf8() {
    let text = "head\r\nbanana 😀 banana\n";
    let captures = [text.as_bytes().to_vec(), utf16(text, true), utf16(text, false)];
    let entries: Vec<_> = [b"a.rs".as_slice(), b"b.rs", b"c.rs"].iter().zip(&captures)
        .map(|(path, bytes)| SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }).collect();
    let b = budget(); let mut archive = open(encode(&entries, true), &b);
    for text in ["ana", "😀", "head\r\nbanana", "not present"] {
        let needle = StreamingNeedle::text(owner(), text, &b, id(2)).unwrap();
        let report = query(&mut archive, &needle, 100, &b, 5);
        assert!(report.is_complete());
        let actual: Vec<_> = report.hits().iter().map(|hit| (hit.file(), hit.revision(), hit.original_range())).collect();
        let mut expected = Vec::new();
        for (i, bytes) in captures.iter().enumerate() {
            let file = FileId::new(owner(), 100 + i as u64).unwrap();
            let revision = SourceRevision::new(owner(), 200 + i as u64).unwrap();
            let capture = CompleteCapture::new(CaptureRequest::new(file, revision).unwrap(),
                ByteLength::new(bytes.len() as u64), Arc::from(bytes.as_slice())).unwrap();
            let result = DirectSourceScanner::scan_complete_capture(&capture, text, &QueryOptions::new(generation(1))).unwrap();
            expected.extend(result.matches.iter().map(|hit| (hit.file_id, hit.revision, hit.original_byte_range)));
        }
        assert_eq!(actual, expected, "needle {text:?}");
    }
}

#[test]
fn exact_limit_lookahead_continues_across_files_including_zero_result_capacity() {
    let entries = [SnapshotEntry { path: b"a", observed_bytes: 1, data: SnapshotData::Captured(b"x") },
        SnapshotEntry { path: b"b", observed_bytes: 1, data: SnapshotData::Captured(b"x") },
        SnapshotEntry { path: b"c", observed_bytes: 1, data: SnapshotData::Captured(b"x") },
        SnapshotEntry { path: b"z", observed_bytes: 1, data: SnapshotData::Captured(b"z") }];
    for limit in 0..=4 {
        let b = budget(); let mut archive = open(encode(&entries, true), &b);
        let needle = StreamingNeedle::text(owner(), "x", &b, id(2)).unwrap();
        let report = query(&mut archive, &needle, limit, &b, 4);
        assert_eq!(report.hits().len(), limit.min(3));
        assert_eq!(report.truncated(), limit < 3);
        assert_eq!(report.matches_seen(), (limit + 1).min(3) as u64);
        assert_eq!(report.is_complete(), limit >= 3);
        if limit >= 3 { assert_eq!(report.stats().files_searched, 4); }
    }
}

#[test]
fn unsupported_and_missing_members_are_not_exhaustive_negatives_but_raw_search_still_works() {
    let entries = [SnapshotEntry { path: b"a", observed_bytes: 2, data: SnapshotData::Captured(b"x\xff") },
        SnapshotEntry { path: b"missing", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
    let b = budget(); let mut archive = open(encode(&entries, true), &b);
    {
        let needle = StreamingNeedle::text(owner(), "x", &b, id(2)).unwrap();
        let report = query(&mut archive, &needle, 10, &b, 4);
        assert!(!report.is_complete()); assert_eq!(report.stats().unsupported_files, 1);
        assert_eq!(report.unavailable_files(), 1); assert!(report.hits().is_empty());
    }
    let needle = StreamingNeedle::raw(owner(), &[0xff], &b, id(2)).unwrap();
    let report = query(&mut archive, &needle, 10, &b, 4);
    assert_eq!(report.hits().len(), 1); assert_eq!(report.stats().unsupported_files, 0);
    let capture = PagedCapture::open_hit(&mut archive, report.hits()[0], generation(1), &b, [id(6), id(7)], || false).unwrap();
    assert_eq!(capture.hit_bytes(report.hits()[0]).unwrap(), &[0xff]);
}

#[test]
fn incomplete_discovery_and_empty_complete_archives_remain_distinct() {
    for complete in [false, true] {
        let b = budget(); let mut archive = open(encode(&[], complete), &b);
        let needle = StreamingNeedle::text(owner(), "x", &b, id(2)).unwrap();
        let report = query(&mut archive, &needle, 0, &b, 4);
        assert_eq!(report.is_complete(), complete); assert!(!report.truncated());
        assert!(report.hits().is_empty()); assert_eq!(archive.load_stats().loaded_members, 0);
    }
}

#[test]
fn hits_open_the_verified_file_then_read_exact_lines_after_archive_release() {
    let bytes = utf16("head\r\nbanana\n", true);
    let b = budget(); let mut archive = open(encode(&[SnapshotEntry { path: b"src/\xff.rs",
        observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(&bytes) }], true), &b);
    let needle = StreamingNeedle::text(owner(), "ana", &b, id(2)).unwrap();
    let report = query(&mut archive, &needle, 10, &b, 4);
    let hit = report.hits()[0];
    assert!(matches!(PagedCapture::open_hit(&mut archive, hit, generation(2), &b, [id(6), id(7)], || false), Err(PagedSearchError::StaleQuery)));
    let capture = PagedCapture::open_hit(&mut archive, hit, generation(1), &b, [id(6), id(7)], || false).unwrap();
    assert_eq!(capture.hit_bytes(hit).unwrap(), &[b'a', 0, b'n', 0, b'a', 0]);
    drop(archive); drop(report); drop(needle);
    let reader = capture.reader(ReaderLimits::default(), &b, id(8)).unwrap();
    let mut seek = reader.seek(ReadingTarget::Byte(hit.original_range().start()), generation(1)).unwrap();
    while seek.state() == ReadingSeekState::Pending { seek.step(4, generation(1), || false).unwrap(); }
    let ReadingSeekState::Ready(anchor) = seek.state() else { panic!() };
    assert_eq!(anchor.line_number(), 2);
    let window = reader.window(anchor, generation(1), ReadingWindowOptions::default(), &b, id(9), || false).unwrap();
    assert_eq!(window.line_text(0), Some("anana"));
    assert_eq!(window.frame_plan().file(), hit.file());
    assert_eq!(window.frame_plan().source(), hit.revision());
    drop(window); drop(reader); drop(capture);
    assert_eq!(b.accounting().reserved().get(), 0);
}

#[test]
fn zero_work_stale_generations_and_cancellation_never_publish_a_partial_query() {
    let bytes = vec![b'x'; 64 * 1024];
    let b = budget(); let mut archive = open(encode(&[SnapshotEntry { path: b"a",
        observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(&bytes) }], true), &b);
    let needle = StreamingNeedle::text(owner(), "xx", &b, id(2)).unwrap();
    let baseline = b.accounting().reserved();
    let mut q = PagedQuery::new(&mut archive, &needle, options(100), &b, [id(3), id(4), id(5)]).unwrap();
    assert_eq!(q.step(StreamReadStep { max_bytes: 0, ..Default::default() }, generation(1), &b, || false).unwrap(), PagedQueryState::Pending);
    assert_eq!(q.stats().members_visited, 0);
    assert!(matches!(q.finish(), Err(PagedSearchError::Pending)));
    assert_eq!(b.accounting().reserved(), baseline);
    assert_eq!(archive.load_stats().loaded_members, 0);
    for stale in [false, true] {
        let mut q = PagedQuery::new(&mut archive, &needle, options(100), &b, [id(3), id(4), id(5)]).unwrap();
        let result = q.step(StreamReadStep::default(), generation(if stale { 2 } else { 1 }), &b, || !stale);
        assert!(result.is_err()); assert_eq!(q.state(), PagedQueryState::Canceled);
        assert!(matches!(q.finish(), Err(PagedSearchError::Canceled)));
        assert_eq!(b.accounting().reserved(), baseline);
    }
}

#[test]
fn at_most_one_member_payload_is_retained_while_other_members_are_pending() {
    let content = vec![b'x'; 128 * 1024];
    let paths: Vec<_> = (0..24).map(|i| format!("{i:03}.rs")).collect();
    let entries: Vec<_> = paths.iter().map(|path| SnapshotEntry { path: path.as_bytes(),
        observed_bytes: content.len() as u64, data: SnapshotData::Captured(&content) }).collect();
    let b = budget(); let mut archive = open(encode(&entries, true), &b);
    let needle = StreamingNeedle::text(owner(), "absent", &b, id(2)).unwrap();
    let report = query(&mut archive, &needle, 100, &b, 4096);
    assert!(report.is_complete()); assert_eq!(report.stats().files_searched, 24);
    assert_eq!(report.stats().peak_source_bytes, content.len());
    assert_eq!(report.stats().scanned_bytes, 24 * content.len() as u64);
    assert_eq!(archive.load_stats().loaded_members, 24);
    assert_eq!(archive.load_stats().bytes_read, 24 * content.len() as u64);
}

#[test]
fn foreign_identity_intervals_and_exhaustion_are_refused_before_query_work() {
    let entries = [SnapshotEntry { path: b"a", observed_bytes: 0, data: SnapshotData::Captured(b"") },
        SnapshotEntry { path: b"b", observed_bytes: 0, data: SnapshotData::Captured(b"") }];
    let b = budget(); let mut archive = open(encode(&entries, true), &b);
    let needle = StreamingNeedle::text(owner(), "x", &b, id(2)).unwrap();
    let mut op = options(1); op.first_file = FileId::new(owner(), u64::MAX).unwrap();
    assert!(matches!(PagedQuery::new(&mut archive, &needle, op, &b, [id(3), id(4), id(5)]), Err(PagedSearchError::IdentityExhausted)));
    let mut op = options(1); op.first_revision = SourceRevision::new(ArenaOwnerId::new(1852).unwrap(), 1).unwrap();
    assert!(matches!(PagedQuery::new(&mut archive, &needle, op, &b, [id(3), id(4), id(5)]), Err(PagedSearchError::OwnerMismatch)));
    assert_eq!(archive.load_stats().loaded_members, 0);
}

#[cfg(unix)]
#[test]
fn open_file_survives_path_replacement_and_changed_inode_content_is_not_relabelled() {
    use std::{fs, io::{Seek, SeekFrom, Write}, time::{SystemTime, UNIX_EPOCH}};
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fcb-paged-{}-{nonce}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(dir.clone());
    let path = dir.join("saved.fcbs");
    let bytes = encode(&[SnapshotEntry { path: b"a", observed_bytes: 6, data: SnapshotData::Captured(b"banana") }], true);
    fs::write(&path, &bytes).unwrap();
    let b = budget();
    let mut archive = PagedSnapshot::open(fs::File::open(&path).unwrap(), owner(), SnapshotLimits::default(), &b, id(1), || false).unwrap();
    let fcb::search::paged_snapshot::PagedMemberData::Captured { archive_offset, .. } = archive.directory().member(0).unwrap().data else { panic!() };
    fs::rename(&path, dir.join("retained.fcbs")).unwrap(); fs::write(&path, b"new path contents").unwrap();
    let needle = StreamingNeedle::text(owner(), "ana", &b, id(2)).unwrap();
    let report = query(&mut archive, &needle, 100, &b, 4);
    assert_eq!(report.hits().len(), 2);
    let retained = PagedCapture::open_hit(&mut archive, report.hits()[0], generation(1), &b, [id(6), id(7)], || false).unwrap();
    let mut changed = fs::OpenOptions::new().write(true).open(dir.join("retained.fcbs")).unwrap();
    changed.seek(SeekFrom::Start(archive_offset)).unwrap(); changed.write_all(b"BANANA").unwrap(); changed.sync_all().unwrap();
    assert!(matches!(PagedCapture::open_hit(&mut archive, report.hits()[0], generation(1), &b, [id(6), id(8)], || false),
        Err(PagedSearchError::Archive(PagedSnapshotError::Changed))));
    assert_eq!(retained.bytes(), b"banana");
}

#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! Public consumers use the actual catalog, index, query and reader APIs. A
//! guarded source reader fails on every unselected body read. No fake matcher,
//! generated hit or timing assumption substitutes for the production path.

use std::io::{self, Cursor, Read, Seek, SeekFrom};
use fcb::{ArenaOwnerId, ByteLength, ByteOffset, FileId, SourceRevision};
use fcb::search::{IndexLimits, QueryGeneration, ResourceAllocationId, ResourceBudget,
    StreamReadStep, StreamingNeedle, ReaderLimits, ReadingTarget, ReadingSeekState, ReadingWindowOptions};
use fcb::search::snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData, SnapshotLimits};
use fcb::search::snapshot_catalog::{CatalogArtifact, PinnedCatalog};
use fcb::search::snapshot_index::{SnapshotIndex, IndexArtifact};
use fcb::search::paged_snapshot::{PagedSnapshot, PagedMemberData, PagedQuery, PagedQueryOptions,
    PagedQueryState, PagedReport, PagedCapture, PagedSearchError, PagedSnapshotError, IndexedNeedle};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1951).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn options(limit: usize) -> PagedQueryOptions {
    PagedQueryOptions { generation: QueryGeneration::new(owner(), 3).unwrap(), first_file: FileId::new(owner(), 100).unwrap(),
        first_revision: SourceRevision::new(owner(), 200).unwrap(), max_matches: limit }
}
fn prepare(entries: &[SnapshotEntry<'_>], complete: bool, limits: IndexLimits,
    budget: &ResourceBudget) -> (Vec<u8>, CatalogArtifact, IndexArtifact) {
    let encoded = SnapshotBytes::encode(owner(), complete, "test-v1", entries, SnapshotLimits::default(), budget, id(1), || false).unwrap();
    let bytes = encoded.bytes().to_vec(); drop(encoded);
    let mut archive = PagedSnapshot::open(Cursor::new(&bytes), owner(), SnapshotLimits::default(), budget, id(2), || false).unwrap();
    let catalog = archive.directory().encode_catalog(budget, id(3), || false).unwrap();
    let index = SnapshotIndex::build(&mut archive, limits, budget, [id(4), id(5), id(6), id(7)], || false).unwrap();
    let index = index.encode(budget, id(8), || false).unwrap();
    drop(archive);
    (bytes, catalog, index)
}
fn pinned(catalog: &CatalogArtifact, budget: &ResourceBudget) -> PinnedCatalog {
    PinnedCatalog::decode_pinned(catalog.bytes(), catalog.digest(), owner(), SnapshotLimits::default(), budget, id(2), || false).unwrap()
}
fn complete<R: Read + Seek>(query: &mut PagedQuery<'_, '_, R>, budget: &ResourceBudget) {
    for _ in 0..100_000 {
        if query.state() != PagedQueryState::Pending { return; }
        query.step(StreamReadStep { max_bytes: 17, max_calls: 3, max_hits: 2 }, query.generation(), budget, || false).unwrap();
    }
    panic!("query did not terminate");
}
fn run<R: Read + Seek>(archive: &mut PagedSnapshot<R>, index: Option<&SnapshotIndex>,
    needle: &[u8], text: bool, limit: usize, budget: &ResourceBudget) -> PagedReport {
    if let Some(index) = index {
        let needle = if text { IndexedNeedle::text(owner(), std::str::from_utf8(needle).unwrap(), budget, id(10)).unwrap() }
            else { IndexedNeedle::raw(owner(), needle, budget, id(10)).unwrap() };
        let mut query = PagedQuery::new_indexed(archive, &needle, index, options(limit), budget, [id(11), id(12), id(13)]).unwrap();
        complete(&mut query, budget); query.finish().unwrap()
    } else {
        let needle = if text { StreamingNeedle::text(owner(), std::str::from_utf8(needle).unwrap(), budget, id(10)).unwrap() }
            else { StreamingNeedle::raw(owner(), needle, budget, id(10)).unwrap() };
        let mut query = PagedQuery::new(archive, &needle, options(limit), budget, [id(11), id(12), id(13)]).unwrap();
        complete(&mut query, budget); query.finish().unwrap()
    }
}
fn ranges(report: &PagedReport) -> Vec<(usize, u64, u64)> {
    report.hits().iter().map(|hit| (hit.ordinal(), hit.original_range().start().get(), hit.original_range().end().get())).collect()
}
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    bytes
}

struct SelectedOnly { bytes: Cursor<Vec<u8>>, allowed: Option<std::ops::Range<usize>> }
impl Read for SelectedOnly {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let pos = self.bytes.position() as usize;
        let len = self.bytes.get_ref().len();
        let end = pos.saturating_add(into.len()).min(len);
        assert!(end <= 24 || pos >= len - 32 || self.allowed.as_ref().is_some_and(|range| range.start <= pos && end <= range.end),
            "unexpected source read {pos}..{end}");
        self.bytes.read(into)
    }
}
impl Seek for SelectedOnly { fn seek(&mut self, from: SeekFrom) -> io::Result<u64> { self.bytes.seek(from) } }

#[test]
fn cold_indexed_search_never_reads_unselected_payloads_and_exact_negative_loads_none() {
    let names: Vec<_> = (0..100).map(|i| format!("file-{i:03}.rs")).collect();
    let payloads: Vec<_> = (0..100).map(|i| if i == 42 { b"prefix needle suffix".to_vec() } else { vec![b'x'; 4096] }).collect();
    let entries: Vec<_> = names.iter().zip(&payloads).map(|(path, bytes)| entry(path.as_bytes(), bytes)).collect();
    let budget = budget();
    let (bytes, catalog, artifact) = prepare(&entries, true, IndexLimits::default(), &budget);
    for needle in ["needle", "impossible"] {
        let directory = pinned(&catalog, &budget);
        let PagedMemberData::Captured { archive_offset, byte_length, .. } = directory.directory().member(42).unwrap().data else { panic!() };
        let range = (needle == "needle").then_some(archive_offset as usize..archive_offset as usize + byte_length);
        let input = SelectedOnly { bytes: Cursor::new(bytes.clone()), allowed: range };
        let mut archive = PagedSnapshot::open_pinned(input, directory, || false).unwrap();
        let index = SnapshotIndex::decode_pinned(artifact.bytes(), artifact.digest(), archive.directory(), &budget, id(4), || false).unwrap();
        assert_eq!(index.stats().build_source_bytes, 0);
        assert_eq!(archive.directory().validation_stats().bytes_read, 56);
        let report = run(&mut archive, Some(&index), needle.as_bytes(), true, 10, &budget);
        assert!(report.is_complete());
        assert!(!archive.directory().fully_verified_on_open());
        let expected_hits = usize::from(needle == "needle");
        assert_eq!(report.hits().len(), expected_hits);
        assert_eq!(report.stats().index_eliminated_files, 100 - expected_hits);
        assert_eq!(archive.load_stats().loaded_members, expected_hits as u64);
        assert_eq!(archive.load_stats().bytes_read, (expected_hits * payloads[42].len()) as u64);
        if expected_hits > 0 { assert_eq!(ranges(&report), [(42, 7, 13)]); }
    }
}

#[test]
fn catalog_queries_equal_full_scans_across_encodings_limits_and_uncovered_segments() {
    let utf16 = utf16("banana 😀");
    for complete_scope in [true, false] {
        for grams in [0, 1000] {
            let entries = [entry(b"a", b"banana"), entry(b"b", &utf16), entry(b"c", b"\xff bad"), entry(b"d", b"")];
            let budget = budget();
            let (bytes, catalog, artifact) = prepare(&entries, complete_scope,
                IndexLimits { max_total_grams: grams, ..Default::default() }, &budget);
            for (needle, text) in [(b"ana".as_slice(), true), (b"a", true), (b"none", true), (&[0xff], false), ("😀".as_bytes(), true)] {
                for limit in [0, 1, 2, 10] {
                    let mut full = PagedSnapshot::open(Cursor::new(&bytes), owner(), SnapshotLimits::default(), &budget, id(20), || false).unwrap();
                    let expected = run(&mut full, None, needle, text, limit, &budget);
                    let coordinates = ranges(&expected);
                    let metadata = (expected.is_complete(), expected.truncated(), expected.matches_seen(), expected.stats().unsupported_files);
                    drop(expected); drop(full);
                    let mut archive = PagedSnapshot::open_pinned(Cursor::new(&bytes), pinned(&catalog, &budget), || false).unwrap();
                    let index = SnapshotIndex::decode_pinned(artifact.bytes(), artifact.digest(), archive.directory(), &budget, id(4), || false).unwrap();
                    let actual = run(&mut archive, Some(&index), needle, text, limit, &budget);
                    assert_eq!(ranges(&actual), coordinates);
                    assert_eq!((actual.is_complete(), actual.truncated(), actual.matches_seen(), actual.stats().unsupported_files), metadata);
                }
            }
        }
    }
}

#[test]
fn selected_utf16_hit_opens_exact_reader_and_survives_archive_shutdown() {
    let utf16 = utf16("head\r\nbanana");
    let budget = budget();
    let (bytes, catalog, artifact) = prepare(&[entry(b"src/\xff.rs", &utf16)], true, IndexLimits::default(), &budget);
    let mut archive = PagedSnapshot::open_pinned(Cursor::new(&bytes), pinned(&catalog, &budget), || false).unwrap();
    let index = SnapshotIndex::decode_pinned(artifact.bytes(), artifact.digest(), archive.directory(), &budget, id(4), || false).unwrap();
    let report = run(&mut archive, Some(&index), b"ana", true, 10, &budget);
    let hit = report.hits()[0];
    let capture = PagedCapture::open_hit(&mut archive, hit, options(10).generation, &budget, [id(21), id(22)], || false).unwrap();
    assert_eq!(capture.hit_bytes(hit).unwrap(), &[b'a', 0, b'n', 0, b'a', 0]);
    drop(report); drop(index); drop(archive);
    let reader = capture.reader(ReaderLimits::default(), &budget, id(23)).unwrap();
    let mut seek = reader.seek(ReadingTarget::Byte(ByteOffset::new(14)), options(10).generation).unwrap();
    while seek.state() == ReadingSeekState::Pending { seek.step(4, options(10).generation, || false).unwrap(); }
    let ReadingSeekState::Ready(at) = seek.state() else { panic!() };
    let window = reader.window(at, options(10).generation, ReadingWindowOptions::default(), &budget, id(24), || false).unwrap();
    assert_eq!(window.line_text(0), Some("banana")); assert_eq!(window.lines()[0].number, 2);
}

#[test]
fn excluded_body_corruption_does_not_become_an_archive_health_claim() {
    let budget = budget();
    let (mut bytes, catalog, artifact) = prepare(&[entry(b"a", b"needle"), entry(b"b", b"unrelated")], true, IndexLimits::default(), &budget);
    let metadata = pinned(&catalog, &budget);
    let PagedMemberData::Captured { archive_offset, .. } = metadata.directory().member(1).unwrap().data else { panic!() };
    bytes[archive_offset as usize] ^= 1;
    let mut archive = PagedSnapshot::open_pinned(Cursor::new(&bytes), metadata, || false).unwrap();
    let index = SnapshotIndex::decode_pinned(artifact.bytes(), artifact.digest(), archive.directory(), &budget, id(4), || false).unwrap();
    let report = run(&mut archive, Some(&index), b"needle", true, 10, &budget);
    assert!(report.is_complete(), "search completeness names the original trusted saved scope");
    assert!(!archive.directory().fully_verified_on_open(), "must not claim unread regions passed verification");
    assert_eq!(archive.load_stats().loaded_members, 1);
    assert!(matches!(archive.load(1, &budget, id(21), || false), Err(PagedSnapshotError::Changed)));
    drop(report);
    let needle = IndexedNeedle::text(owner(), "unrelated", &budget, id(10)).unwrap();
    let mut query = PagedQuery::new_indexed(&mut archive, &needle, &index, options(10), &budget, [id(11), id(12), id(13)]).unwrap();
    let mut failed = false;
    for _ in 0..100 {
        if let Err(error) = query.step(StreamReadStep::default(), query.generation(), &budget, || false) {
            assert!(matches!(error, PagedSearchError::Archive(PagedSnapshotError::Changed))); failed = true; break;
        }
    }
    assert!(failed); assert!(query.finish().is_err());
}

#[test]
fn missing_members_and_saved_discovery_remain_partial_after_metadata_reopening() {
    let budget = budget();
    let entries = [entry(b"a", b"none"), SnapshotEntry { path: b"b", observed_bytes: u64::MAX,
        data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
    let (bytes, catalog, artifact) = prepare(&entries, false, IndexLimits::default(), &budget);
    let mut archive = PagedSnapshot::open_pinned(Cursor::new(&bytes), pinned(&catalog, &budget), || false).unwrap();
    let index = SnapshotIndex::decode_pinned(artifact.bytes(), artifact.digest(), archive.directory(), &budget, id(4), || false).unwrap();
    let report = run(&mut archive, Some(&index), b"needle", true, 10, &budget);
    assert!(!report.is_complete()); assert_eq!(report.unavailable_files(), 1);
    assert!(report.hits().is_empty()); assert_eq!(archive.load_stats().loaded_members, 0);
}

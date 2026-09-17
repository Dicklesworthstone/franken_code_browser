#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::{cell::RefCell, io::{self, Cursor, Read, Seek, SeekFrom}, rc::Rc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{IndexLimits, QueryGeneration, ResourceAllocationId, ResourceBudget, StreamingNeedle, StreamReadStep};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::snapshot_index::{SnapshotIndex, SnapshotIndexError, PostingStep, IndexDecision};
use fcb::search::snapshot_index::paged::{PagedPostings, PagedPostingError, POSTING_PAGE_BYTES};
use fcb::search::paged_snapshot::{PagedSnapshot, PagedQuery, PagedQueryOptions, PagedQueryState, IndexedNeedle, PagedCapture, PagedSearchError};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(4421).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn entry<'a>(name: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path: name, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn archive(entries: &[SnapshotEntry<'_>], complete: bool, b: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    let encoded = SnapshotBytes::encode(owner(), complete, "paged-postings-tests", entries, SnapshotLimits::default(), b, id(90), || false).unwrap();
    PagedSnapshot::open(Cursor::new(encoded.bytes().to_vec()), owner(), SnapshotLimits::default(), b, id(1), || false).unwrap()
}
fn options(limit: usize) -> PagedQueryOptions {
    PagedQueryOptions { generation: generation(1), first_file: FileId::new(owner(), 10).unwrap(),
        first_revision: SourceRevision::new(owner(), 20).unwrap(), max_matches: limit }
}
fn noise(n: usize) -> Vec<u8> {
    let mut state = 0x714821u64;
    (0..n).map(|_| { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state as u8 }).collect()
}
struct Guard {
    data: Rc<RefCell<Vec<u8>>>, position: u64, body: usize, deny_body: Rc<RefCell<bool>>,
    reads: Rc<RefCell<Vec<(usize, usize)>>>,
}
impl Read for Guard {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let start = self.position as usize;
        let data = self.data.borrow();
        let n = out.len().min(data.len().saturating_sub(start));
        if *self.deny_body.borrow() && n > 0 && start + n > self.body {
            return Err(io::Error::other("unselected index body was read"));
        }
        self.reads.borrow_mut().push((start, start + n));
        if n > 0 { out[..n].copy_from_slice(&data[start..start + n]); }
        self.position += n as u64; Ok(n)
    }
}
impl Seek for Guard {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from { SeekFrom::Start(n) => n as i128,
            SeekFrom::End(n) => self.data.borrow().len() as i128 + n as i128,
            SeekFrom::Current(n) => self.position as i128 + n as i128 };
        self.position = u64::try_from(position).map_err(|_| io::Error::other("negative seek"))?; Ok(self.position)
    }
}
fn guard(bytes: &[u8]) -> Guard {
    let body = 32 + u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
    Guard { data: Rc::new(RefCell::new(bytes.to_vec())), position: 0, body,
        deny_body: Rc::new(RefCell::new(true)), reads: Rc::new(RefCell::new(Vec::new())) }
}

#[test]
fn open_and_short_query_do_not_read_any_posting_page() {
    let b = budget(); let payload = noise(24_000); let mut source = archive(&[entry(b"a", &payload)], true, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(), &b, [id(2), id(3), id(4), id(5)], || false).unwrap();
    let inverse = forward.invert(&b, id(6), || false).unwrap();
    let encoded = inverse.encode_paged(&b, id(7), || false).unwrap();
    let input = guard(encoded.bytes()); let body = input.body; let deny = input.deny_body.clone();
    let mut paged = PagedPostings::open_pinned(input, encoded.digest(), source.directory(), 1, &b, id(8), || false).unwrap();
    assert_eq!(paged.io_stats().manifest_bytes_read, body as u64);
    assert_eq!(paged.io_stats().page_bytes_read, 0); assert_eq!(paged.resident_pages(), 0);
    let mut cursor = paged.raw_candidates(b"a");
    assert_eq!(cursor.step(|| false).unwrap(), PostingStep::Candidate { ordinal: 0, decision: IndexDecision::Fallback });
    assert_eq!(cursor.step(|| false).unwrap(), PostingStep::Finished); drop(cursor);
    assert_eq!(paged.io_stats().page_bytes_read, 0);
    // Negative control: direct body read is rejected by the independent guard.
    assert!(*deny.borrow());
    let mut bad = guard(encoded.bytes()); bad.seek(SeekFrom::Start(body as u64)).unwrap();
    assert!(bad.read(&mut [0u8; 1]).is_err());
}

#[test]
fn one_page_cache_resumes_multi_page_intersection_and_matches_resident_candidates() {
    let b = budget(); let bytes = noise(28_000);
    let mut source = archive(&[entry(b"a", &bytes[..12_000]), entry(b"b", &bytes[12_000..]), entry(b"c", b"needle banana")], true, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(), &b, [id(2), id(3), id(4), id(5)], || false).unwrap();
    let inverse = forward.invert(&b, id(6), || false).unwrap();
    let encoded = inverse.encode_paged(&b, id(7), || false).unwrap();
    for pattern in [b"needle".as_slice(), &bytes[90..102], &bytes[12_100..12_120], b"absent-token"] {
        let input = guard(encoded.bytes()); *input.deny_body.borrow_mut() = false;
        let reads = input.reads.clone();
        let mut paged = PagedPostings::open_pinned(input, encoded.digest(), source.directory(), 1, &b, id(8), || false).unwrap();
        let charge = b.accounting().reserved().get();
        let mut expected = Vec::new(); let mut old = inverse.raw_candidates(pattern);
        loop { match old.step() { PostingStep::Candidate { ordinal, decision } => expected.push((ordinal, decision)),
            PostingStep::Finished => break, _ => {} } }
        let mut actual = Vec::new(); let mut query = paged.raw_candidates(pattern);
        for _ in 0..100_000 {
            let before: usize = reads.borrow().iter().map(|(a, b)| b - a).sum();
            let step = query.step(|| false).unwrap();
            let after: usize = reads.borrow().iter().map(|(a, b)| b - a).sum();
            assert!(after - before <= POSTING_PAGE_BYTES, "one step loaded multiple pages");
            assert_eq!(b.accounting().reserved().get(), charge, "query must not allocate a candidate universe");
            match step { PostingStep::Candidate { ordinal, decision } => actual.push((ordinal, decision)),
                PostingStep::Finished => break, _ => {} }
        }
        assert!(query.is_finished()); assert_eq!(actual, expected); drop(query);
        assert_eq!(paged.cache_capacity_bytes(), POSTING_PAGE_BYTES); assert!(paged.resident_pages() <= 1);
    }
}

#[test]
fn raw_text_quotas_and_cross_file_limits_match_the_unfiltered_production_query() {
    let b = budget(); let mut utf16 = vec![0xff, 0xfe];
    for word in "banana needle".encode_utf16() { utf16.extend_from_slice(&word.to_le_bytes()); }
    let entries = [entry(b"a", b"banana needle"), entry(b"b", &utf16), entry(b"c", b"malformed\xffneedle"),
        entry(b"d", b"needle"), SnapshotEntry { path: b"z", observed_bytes: 50, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
    for complete in [true, false] {
        let mut source = archive(&entries, complete, &b);
        for grams in [0, 8, 100_000] {
            let forward = SnapshotIndex::build(&mut source, IndexLimits { max_total_grams: grams, ..Default::default() },
                &b, [id(2), id(3), id(4), id(5)], || false).unwrap();
            let inverse = forward.invert(&b, id(6), || false).unwrap();
            let artifact = inverse.encode_paged(&b, id(7), || false).unwrap();
            let mut index = PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(), source.directory(), 1, &b, id(8), || false).unwrap();
            for text in [true, false] { for pattern in [b"ana".as_slice(), b"needle", b"n", b"absent"] { for limit in 0..=5 {
                let plain = if text { StreamingNeedle::text(owner(), std::str::from_utf8(pattern).unwrap(), &b, id(10)).unwrap() }
                    else { StreamingNeedle::raw(owner(), pattern, &b, id(10)).unwrap() };
                let mut query = PagedQuery::new(&mut source, &plain, options(limit), &b, [id(11), id(12), id(13)]).unwrap();
                while query.state() == PagedQueryState::Pending { query.step(StreamReadStep::default(), generation(1), &b, || false).unwrap(); }
                let expected = query.finish().unwrap();
                let needle = if text { IndexedNeedle::text(owner(), std::str::from_utf8(pattern).unwrap(), &b, id(14)).unwrap() }
                    else { IndexedNeedle::raw(owner(), pattern, &b, id(14)).unwrap() };
                let mut query = PagedQuery::new_paged(&mut source, &needle, &mut index, options(limit), &b, [id(15), id(16), id(17)]).unwrap();
                for _ in 0..100_000 {
                    if query.state() != PagedQueryState::Pending { break; }
                    query.step(StreamReadStep::default(), generation(1), &b, || false).unwrap();
                }
                let actual = query.finish().unwrap();
                assert_eq!(actual.hits(), expected.hits()); assert_eq!(actual.is_complete(), expected.is_complete());
                assert_eq!(actual.truncated(), expected.truncated()); assert_eq!(actual.matches_seen(), expected.matches_seen());
                assert_eq!(actual.stats().unsupported_files, expected.stats().unsupported_files);
            } } }
        }
    }
}

#[test]
fn changed_page_is_rejected_and_never_publishes_a_negative_or_partial_report() {
    let b = budget(); let mut source = archive(&[entry(b"a", b"needle banana")], true, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(), &b, [id(2), id(3), id(4), id(5)], || false).unwrap();
    let inverse = forward.invert(&b, id(6), || false).unwrap(); let artifact = inverse.encode_paged(&b, id(7), || false).unwrap();
    let input = guard(artifact.bytes()); *input.deny_body.borrow_mut() = false;
    let data = input.data.clone(); let body = input.body;
    let mut index = PagedPostings::open_pinned(input, artifact.digest(), source.directory(), 1, &b, id(8), || false).unwrap();
    data.borrow_mut()[body] ^= 1;
    let needle = IndexedNeedle::text(owner(), "needle", &b, id(10)).unwrap();
    let mut query = PagedQuery::new_paged(&mut source, &needle, &mut index, options(10), &b, [id(11), id(12), id(13)]).unwrap();
    let error = query.step(StreamReadStep::default(), generation(1), &b, || false).unwrap_err();
    assert_eq!(error, PagedSearchError::PagedIndex(PagedPostingError::ChangedPage));
    assert!(matches!(query.finish(), Err(PagedSearchError::PagedIndex(PagedPostingError::ChangedPage))));
    assert_eq!(source.load_stats().loaded_members, 1, "only the earlier index build loaded source");
}

#[test]
fn zero_steps_stale_queries_and_cancellation_do_no_posting_io() {
    let b = budget(); let mut source = archive(&[entry(b"a", b"needle")], true, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(), &b, [id(2), id(3), id(4), id(5)], || false).unwrap();
    let inverse = forward.invert(&b, id(6), || false).unwrap(); let artifact = inverse.encode_paged(&b, id(7), || false).unwrap();
    let mut index = PagedPostings::open_pinned(guard(artifact.bytes()), artifact.digest(), source.directory(), 1, &b, id(8), || false).unwrap();
    let needle = IndexedNeedle::text(owner(), "needle", &b, id(10)).unwrap();
    let mut query = PagedQuery::new_paged(&mut source, &needle, &mut index, options(10), &b, [id(11), id(12), id(13)]).unwrap();
    let mut zero = StreamReadStep::default(); zero.max_bytes = 0;
    assert_eq!(query.step(zero, generation(1), &b, || false).unwrap(), PagedQueryState::Pending);
    assert_eq!(query.step(StreamReadStep::default(), generation(2), &b, || false), Err(PagedSearchError::StaleQuery));
    assert!(matches!(query.finish(), Err(PagedSearchError::Canceled)));
    assert_eq!(index.io_stats().page_bytes_read, 0);
    let mut cursor = index.raw_candidates(b"needle");
    assert_eq!(cursor.step(|| true), Err(PagedPostingError::Index(SnapshotIndexError::Canceled)));
    assert_eq!(cursor.step(|| false), Err(PagedPostingError::Index(SnapshotIndexError::Canceled)));
}

#[test]
fn denied_or_canceled_open_releases_metadata_and_page_capacity() {
    let b = budget(); let mut source = archive(&[], true, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(), &b, [id(2), id(3), id(4), id(5)], || false).unwrap();
    let inverse = forward.invert(&b, id(6), || false).unwrap(); let artifact = inverse.encode_paged(&b, id(7), || false).unwrap();
    let baseline = b.accounting().reserved().get();
    assert!(PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(), source.directory(), 0, &b, id(8), || false).is_err());
    assert!(PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(), source.directory(), 1, &b, id(8),
        || b.accounting().reserved().get() > baseline).is_err());
    assert_eq!(b.accounting().reserved().get(), baseline);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(), source.directory(), 1, &tiny, id(8), || false),
        Err(PagedPostingError::Index(SnapshotIndexError::ResourceDenied))));
    let index = PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(), source.directory(), 1, &b, id(8), || false).unwrap();
    assert_eq!(index.resident_pages(), 0); drop(index); assert_eq!(b.accounting().reserved().get(), baseline);
}

#[test]
fn selected_hit_uses_the_existing_digest_bound_reader_after_index_drop() {
    let b = budget(); let mut source = archive(&[entry(b"a", b"head\nneedle\ntail")], true, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(), &b, [id(2), id(3), id(4), id(5)], || false).unwrap();
    let inverse = forward.invert(&b, id(6), || false).unwrap(); let artifact = inverse.encode_paged(&b, id(7), || false).unwrap();
    let mut index = PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(), source.directory(), 1, &b, id(8), || false).unwrap();
    let needle = IndexedNeedle::text(owner(), "needle", &b, id(10)).unwrap();
    let mut query = PagedQuery::new_paged(&mut source, &needle, &mut index, options(10), &b, [id(11), id(12), id(13)]).unwrap();
    while query.state() == PagedQueryState::Pending { query.step(StreamReadStep::default(), generation(1), &b, || false).unwrap(); }
    let report = query.finish().unwrap(); let hit = report.hits()[0]; drop(index);
    let capture = PagedCapture::open_hit(&mut source, hit, generation(1), &b, [id(14), id(15)], || false).unwrap();
    drop(source); assert_eq!(capture.hit_bytes(hit).unwrap(), b"needle");
    let reader = capture.reader(fcb::search::ReaderLimits::default(), &b, id(16)).unwrap();
    let mut seek = reader.seek(fcb::search::ReadingTarget::Range(hit.original_range()), generation(1)).unwrap();
    while seek.state() == fcb::search::ReadingSeekState::Pending { seek.step(64, generation(1), || false).unwrap(); }
    let fcb::search::ReadingSeekState::Ready(at) = seek.state() else { panic!("reader seek failed") };
    assert_eq!(at.line_number(), 2);
}

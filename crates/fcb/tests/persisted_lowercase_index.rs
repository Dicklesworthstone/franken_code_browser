#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! Saved ASCII pruning is only a necessary condition. These tests compare all
//! three index layouts with the existing unfiltered Unicode source verifier.

use std::{cell::Cell, io::{self, Cursor, Read, Seek, SeekFrom}, rc::Rc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{IndexLimits, QueryGeneration, ResourceAllocationId, ResourceBudget,
    StreamingNeedle, StreamReadStep};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::snapshot_index::{IndexDecision, PostingStep, SnapshotIndex, SnapshotPostings};
use fcb::search::snapshot_index::paged::{PagedPostings, POSTING_PAGE_BYTES};
use fcb::search::paged_snapshot::{IndexedNeedle, PagedQuery, PagedQueryOptions,
    PagedQueryState, PagedQueryStats, PagedReport, PagedSnapshot};
use fcb_store::Sha256;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(4519).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn archive(entries: &[SnapshotEntry<'_>], budget: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    let bytes = SnapshotBytes::encode(owner(), true, "lowercase-index-tests", entries,
        SnapshotLimits::default(), budget, id(90), || false).unwrap();
    PagedSnapshot::open(Cursor::new(bytes.bytes().to_vec()), owner(), SnapshotLimits::default(),
        budget, id(1), || false).unwrap()
}
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if big { unit.to_be_bytes() } else { unit.to_le_bytes() });
    }
    bytes
}
#[derive(Debug, Eq, PartialEq)]
struct Signature {
    hits: Vec<(usize, u64, u64)>, seen: u64, matching_files: usize,
    complete: bool, truncated: bool, unsupported: usize, unavailable: usize,
    skipped: u64, next: Option<u64>,
}
fn signature(report: &PagedReport) -> Signature {
    Signature { hits: report.hits().iter().map(|hit| (hit.ordinal(),
        hit.original_range().start().get(), hit.original_range().end().get())).collect(),
        seen: report.matches_seen(), matching_files: report.matching_files(),
        complete: report.is_complete(), truncated: report.truncated(),
        unsupported: report.stats().unsupported_files, unavailable: report.unavailable_files(),
        skipped: report.skipped_matches(), next: report.next_match_offset() }
}
enum Route<'a> { Direct, Segments(&'a SnapshotIndex), Postings(&'a SnapshotPostings),
    Paged(&'a mut PagedPostings<Cursor<Vec<u8>>>) }
fn run(archive: &mut PagedSnapshot<Cursor<Vec<u8>>>, route: Route<'_>, text: &str,
    limit: usize, skip: u64, count: bool, budget: &ResourceBudget) -> (Signature, PagedQueryStats, u64) {
    let indexed = IndexedNeedle::text_matching(owner(), text, false, budget, id(30)).unwrap();
    let plain = StreamingNeedle::text_matching(owner(), text, false, budget, id(31)).unwrap();
    let generation = QueryGeneration::new(owner(), 1).unwrap();
    let options = PagedQueryOptions { generation, first_file: FileId::new(owner(), 10).unwrap(),
        first_revision: SourceRevision::new(owner(), 100).unwrap(), max_matches: limit };
    let ids = [id(32), id(33), id(34)];
    let before = archive.load_stats().bytes_read;
    let mut query = match route {
        Route::Direct => PagedQuery::new_page(archive, &plain, options, skip, count, budget, ids),
        Route::Segments(index) => PagedQuery::new_indexed_page(archive, &indexed, index, options, skip, count, budget, ids),
        Route::Postings(index) => PagedQuery::new_postings_page(archive, &indexed, index, options, skip, count, budget, ids),
        Route::Paged(index) => PagedQuery::new_paged_page(archive, &indexed, index, options, skip, count, budget, ids),
    }.unwrap();
    for _ in 0..100_000 {
        if query.state() != PagedQueryState::Pending { break; }
        let before = query.stats().posting_entries_visited;
        query.step(StreamReadStep::default(), generation, budget, || false).unwrap();
        assert!(query.stats().posting_entries_visited - before <= 1);
    }
    let report = query.finish().unwrap();
    (signature(&report), report.stats(), archive.load_stats().bytes_read - before)
}

#[test]
fn all_layouts_preserve_unicode_fallback_counts_original_ranges_and_result_offsets() {
    let little = utf16("bEaCoN BaNaNa Key", false);
    let big = utf16("BEACON İ", true);
    let entries = [entry(b"a", b"BEACON beacon BeAcOn BaNaNa a-b"),
        entry(b"b", "Key İ i\u{307} beacon".as_bytes()), entry(b"c", &little), entry(b"d", &big),
        entry(b"e", b"malformed\xffBEACON"), entry(b"f", b"unrelated"), entry(b"g", b""),
        SnapshotEntry { path: b"missing", observed_bytes: 7, data: SnapshotData::Unavailable("READ_DENIED") }];
    for limits in [IndexLimits::default(), IndexLimits { max_total_grams: 0, ..Default::default() }] {
        let b = budget(); let mut source = archive(&entries, &b);
        let forward = SnapshotIndex::build(&mut source, limits, &b, [id(2), id(3), id(4), id(5)], || false).unwrap();
        let inverse = forward.invert(&b, id(6), || false).unwrap();
        let bytes = inverse.encode_paged(&b, id(7), || false).unwrap();
        let mut paged = PagedPostings::open_pinned(Cursor::new(bytes.bytes().to_vec()), bytes.digest(),
            source.directory(), 1, &b, id(8), || false).unwrap();
        for text in ["BEACON", "ana", "KEY", "i\u{307}", "i", "a-b", "absent"] {
            for (limit, skip, count) in [(32, 0, true), (1, 1, true), (0, 0, true), (1, 2, false)] {
                let expected = run(&mut source, Route::Direct, text, limit, skip, count, &b).0;
                assert_eq!(run(&mut source, Route::Segments(&forward), text, limit, skip, count, &b).0, expected);
                assert_eq!(run(&mut source, Route::Postings(&inverse), text, limit, skip, count, &b).0, expected);
                assert_eq!(run(&mut source, Route::Paged(&mut paged), text, limit, skip, count, &b).0, expected,
                    "text={text:?}, limit={limit}, skip={skip}, count={count}, limits={limits:?}");
            }
        }
    }
}

#[test]
fn case_variant_union_prunes_source_reads_and_verifies_each_member_only_once() {
    let names: Vec<_> = (0..256).map(|i| format!("file-{i:03}")).collect();
    let entries: Vec<_> = names.iter().enumerate().map(|(i, name)| entry(name.as_bytes(),
        if i == 255 { b"BEACON beacon BeAcOn" } else { b"unrelated source payload" })).collect();
    let b = budget(); let mut source = archive(&entries, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(), &b,
        [id(2), id(3), id(4), id(5)], || false).unwrap();
    let inverse = forward.invert(&b, id(6), || false).unwrap();
    let bytes = inverse.encode_paged(&b, id(7), || false).unwrap();
    let mut paged = PagedPostings::open_pinned(Cursor::new(bytes.bytes().to_vec()), bytes.digest(),
        source.directory(), 1, &b, id(8), || false).unwrap();
    let (expected, _, direct_bytes) = run(&mut source, Route::Direct, "beacon", 10, 0, true, &b);
    assert_eq!(expected.seen, 3);
    for route in [Route::Segments(&forward), Route::Postings(&inverse), Route::Paged(&mut paged)] {
        let (actual, stats, read) = run(&mut source, route, "beacon", 10, 0, true, &b);
        assert_eq!(actual, expected); assert_eq!(read, 20); assert!(read < direct_bytes);
        assert_eq!(stats.files_searched, 1); assert_eq!(stats.index_candidates, 1);
        assert_eq!(stats.index_eliminated_files, 255); assert_eq!(stats.index_fallback_files, 0);
        if stats.posting_list_lookups > 0 {
            assert_eq!(stats.members_visited, 1); assert_eq!(stats.posting_list_lookups, 24);
            assert!(stats.posting_entries_visited <= 3); assert!(stats.posting_cursor_complete);
        }
    }
}

struct Counted { input: Cursor<Vec<u8>>, bytes: Rc<Cell<u64>> }
impl Read for Counted {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.input.read(out)?; self.bytes.set(self.bytes.get() + n as u64); Ok(n)
    }
}
impl Seek for Counted {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> { self.input.seek(position) }
}
#[test]
fn one_page_cache_resumes_ascii_unions_without_allocating_or_loading_multiple_pages_per_step() {
    let mut state = 0x714821u64;
    let noise: Vec<_> = (0..24_000).map(|_| {
        state ^= state << 13; state ^= state >> 7; state ^= state << 17; b' ' + (state % 95) as u8
    }).collect();
    let b = budget(); let mut source = archive(&[entry(b"a", &noise),
        entry(b"b", b"BEACON beacon BeAcOn bEaCoN BEacon beACON")], &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(), &b,
        [id(2), id(3), id(4), id(5)], || false).unwrap();
    let inverse = forward.invert(&b, id(6), || false).unwrap();
    let encoded = inverse.encode_paged(&b, id(7), || false).unwrap();
    assert!(encoded.bytes().len() - encoded.manifest_bytes() > 8 * POSTING_PAGE_BYTES);
    let reads = Rc::new(Cell::new(0));
    let input = Counted { input: Cursor::new(encoded.bytes().to_vec()), bytes: reads.clone() };
    let mut paged = PagedPostings::open_pinned(input, encoded.digest(), source.directory(), 1, &b, id(8), || false).unwrap();
    let mut expected = Vec::new(); let mut resident = inverse.text_candidates_matching("beacon", false);
    loop { match resident.step() { PostingStep::Candidate { ordinal, decision } => expected.push((ordinal, decision)),
        PostingStep::Finished => break, PostingStep::Pending => {} } }
    let baseline = b.accounting().reserved().get(); let mut actual = Vec::new();
    let mut cursor = paged.text_candidates_matching("beacon", false);
    for _ in 0..100_000 {
        let before = reads.get(); let visited = cursor.stats().posting_entries_visited;
        let step = cursor.step(|| false).unwrap();
        assert!(reads.get() - before <= POSTING_PAGE_BYTES as u64);
        assert!(cursor.stats().posting_entries_visited - visited <= 1);
        assert_eq!(b.accounting().reserved().get(), baseline);
        match step { PostingStep::Candidate { ordinal, decision } => actual.push((ordinal, decision)),
            PostingStep::Finished => break, PostingStep::Pending => {} }
    }
    assert!(cursor.is_finished()); assert_eq!(actual, expected);
    assert!(actual.windows(2).all(|pair| pair[0].0 < pair[1].0)); drop(cursor);
    assert!(paged.io_stats().evictions > 0); assert!(paged.resident_pages() <= 1);
}

#[test]
fn legacy_paged_rows_keep_lowercase_fallback_even_when_exact_raw_grams_are_present() {
    let b = budget(); let mut source = archive(&[entry(b"a", b"BEACON"), entry(b"b", b"unrelated")], &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(), &b,
        [id(2), id(3), id(4), id(5)], || false).unwrap();
    let inverse = forward.invert(&b, id(6), || false).unwrap();
    let encoded = inverse.encode_paged(&b, id(7), || false).unwrap();
    // Independently downgrade only the qualified row semantics. The existing
    // raw posting body/layout is unchanged. Re-signing is fixture-only.
    let mut legacy = encoded.bytes().to_vec();
    legacy[88..92].copy_from_slice(&1u32.to_le_bytes());
    legacy[120] = 2; legacy[169] = 2;
    let end = 32 + encoded.manifest_bytes();
    let checksum = Sha256::digest(&legacy[32..end - 32]);
    legacy[end - 32..end].copy_from_slice(checksum.as_bytes());
    let pin = Sha256::digest(&legacy[32..end]);
    let mut paged = PagedPostings::open_pinned(Cursor::new(legacy), pin, source.directory(), 1, &b, id(8), || false).unwrap();
    let mut cursor = paged.text_candidates_matching("beacon", false); let mut found = Vec::new();
    for _ in 0..1000 { match cursor.step(|| false).unwrap() {
        PostingStep::Candidate { ordinal, decision } => found.push((ordinal, decision)),
        PostingStep::Finished => break, PostingStep::Pending => {},
    } }
    assert!(cursor.is_finished());
    assert_eq!(found, [(0, IndexDecision::Fallback), (1, IndexDecision::Fallback)]);
    assert_eq!(cursor.excluded_files(), Some(0)); drop(cursor);
    let direct = run(&mut source, Route::Direct, "beacon", 10, 0, true, &b).0;
    assert_eq!(run(&mut source, Route::Paged(&mut paged), "beacon", 10, 0, true, &b).0, direct);
}

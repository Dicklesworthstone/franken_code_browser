#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

//! Actual snapshot -> index -> inverted postings -> exact matcher -> reader.
//! The per-file prefilter and unfiltered production scanner are independent
//! reference routes; deliberate gram false positives still need verification.

use std::{cell::RefCell, io::{self, Cursor, Read, Seek, SeekFrom}, ops::Range, rc::Rc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{IndexLimits, QueryGeneration, ResourceAllocationId, ResourceBudget,
    StreamingNeedle, StreamReadStep};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::snapshot_index::{SnapshotIndex, SnapshotPostings, SnapshotIndexError,
    IndexDecision, PostingCandidates, PostingStep};
use fcb::search::paged_snapshot::{PagedSnapshot, PagedQuery, PagedQueryOptions, PagedQueryState,
    PagedReport, PagedQueryStats, PagedSearchError, IndexedNeedle, PagedCapture};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(4021).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn encoded(entries: &[SnapshotEntry<'_>], closed: bool) -> Vec<u8> {
    SnapshotBytes::encode(owner(), closed, "postings-test-v1", entries, SnapshotLimits::default(),
        &budget(), id(1), || false).unwrap().bytes().to_vec()
}
fn archive(entries: &[SnapshotEntry<'_>], closed: bool, b: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    PagedSnapshot::open(Cursor::new(encoded(entries, closed)), owner(), SnapshotLimits::default(), b, id(1), || false).unwrap()
}
fn index<R: Read + Seek>(archive: &mut PagedSnapshot<R>, limits: IndexLimits, b: &ResourceBudget) -> SnapshotIndex {
    SnapshotIndex::build(archive, limits, b, [id(2), id(3), id(4), id(5)], || false).unwrap()
}
fn options(limit: usize) -> PagedQueryOptions {
    PagedQueryOptions { generation: generation(1), first_file: FileId::new(owner(), 100).unwrap(),
        first_revision: SourceRevision::new(owner(), 200).unwrap(), max_matches: limit }
}
fn candidates(mut cursor: PostingCandidates<'_>) -> Vec<(usize, IndexDecision)> {
    let mut found = Vec::new();
    for _ in 0..131_073 {
        let before = cursor.stats();
        let step = cursor.step();
        let after = cursor.stats();
        assert!(after.posting_entries_visited - before.posting_entries_visited <= 1);
        assert!(after.membership_lookups - before.membership_lookups <= 3);
        match step {
            PostingStep::Candidate { ordinal, decision } => found.push((ordinal, decision)),
            PostingStep::Pending => {},
            PostingStep::Finished => {
                assert!(cursor.is_finished());
                assert!(found.windows(2).all(|pair| pair[0].0 < pair[1].0));
                return found;
            }
        }
    }
    panic!("bounded candidate cursor did not terminate");
}
#[derive(Debug, Eq, PartialEq)]
struct Signature {
    hits: Vec<(usize, u64, u64, u64, u64)>, complete: bool, truncated: bool,
    seen: u64, unsupported: usize, unavailable: usize,
}
fn signature(report: &PagedReport) -> Signature {
    Signature { hits: report.hits().iter().map(|h| (h.ordinal(), h.file().get(), h.revision().get(),
        h.original_range().start().get(), h.original_range().end().get())).collect(),
        complete: report.is_complete(), truncated: report.truncated(), seen: report.matches_seen(),
        unsupported: report.stats().unsupported_files, unavailable: report.unavailable_files() }
}
fn run<R: Read + Seek>(archive: &mut PagedSnapshot<R>, forward: Option<&SnapshotIndex>,
    inverse: Option<&SnapshotPostings>, pattern: &[u8], text: bool, limit: usize, b: &ResourceBudget)
    -> (Signature, PagedQueryStats) {
    let opts = options(limit);
    let indexed = if text { IndexedNeedle::text(owner(), std::str::from_utf8(pattern).unwrap(), b, id(30)).unwrap() }
        else { IndexedNeedle::raw(owner(), pattern, b, id(30)).unwrap() };
    let plain = if text { StreamingNeedle::text(owner(), std::str::from_utf8(pattern).unwrap(), b, id(31)).unwrap() }
        else { StreamingNeedle::raw(owner(), pattern, b, id(31)).unwrap() };
    let mut query = if let Some(index) = inverse {
        PagedQuery::new_postings(archive, &indexed, index, opts, b, [id(32), id(33), id(34)]).unwrap()
    } else if let Some(index) = forward {
        PagedQuery::new_indexed(archive, &indexed, index, opts, b, [id(32), id(33), id(34)]).unwrap()
    } else { PagedQuery::new(archive, &plain, opts, b, [id(32), id(33), id(34)]).unwrap() };
    for _ in 0..1_000_000 {
        if query.state() != PagedQueryState::Pending { break; }
        let before = query.stats().posting_entries_visited;
        query.step(StreamReadStep::default(), generation(1), b, || false).unwrap();
        assert!(query.stats().posting_entries_visited - before <= 1);
    }
    let report = query.finish().unwrap();
    (signature(&report), report.stats())
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}

#[test]
fn inverse_candidates_equal_the_existing_per_file_prefilter_across_modes_and_quotas() {
    let le = utf16("banana needle", true);
    let be = utf16("needle", false);
    let entries = [entry(b"a", b"banana needle"), entry(b"b", b"abcXbcd"), entry(b"c", &le),
        entry(b"d", &be), entry(b"e", b"malformed\xffneedle"), entry(b"f", b""),
        SnapshotEntry { path: b"missing", observed_bytes: 50, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
    for limits in [IndexLimits::default(), IndexLimits { max_total_grams: 0, ..Default::default() },
        IndexLimits { max_grams_per_file: 7, ..Default::default() }] {
        let b = budget(); let mut archive = archive(&entries, true, &b);
        let index = index(&mut archive, limits, &b);
        let inverse = index.invert(&b, id(6), || false).unwrap();
        for text in [false, true] {
            for pattern in [b"a".as_slice(), b"an", b"ana", b"needle", b"abcd", b"absent"] {
                let expected: Vec<_> = (0..entries.len() - 1).filter_map(|i| {
                    let decision = if text { index.text_decision(i, std::str::from_utf8(pattern).ok()) } else { index.raw_decision(i, pattern) };
                    (decision != IndexDecision::Excluded).then_some((i, decision))
                }).collect();
                let actual = candidates(if text { inverse.text_candidates(std::str::from_utf8(pattern).unwrap()) } else { inverse.raw_candidates(pattern) });
                assert_eq!(actual, expected, "pattern={pattern:?}, text={text}, limits={limits:?}");
            }
        }
    }
}

#[test]
fn all_search_routes_keep_exact_hit_identity_coverage_and_cross_file_limit_lookahead() {
    let le = utf16("banana needle", true);
    let be = utf16("needle", false);
    let entries = [entry(b"a", b"needle banana"), entry(b"b", &le), entry(b"c", b"abcXbcd"),
        entry(b"d", b"malformed\xffsuffix"), entry(b"e", &be), entry(b"f", b"needle")];
    for closed in [false, true] {
        let b = budget(); let mut archive = archive(&entries, closed, &b);
        let index = index(&mut archive, IndexLimits::default(), &b);
        let inverse = index.invert(&b, id(6), || false).unwrap();
        for text in [false, true] {
            for pattern in [b"a".as_slice(), b"ana", b"needle", b"abcd", b"absent"] {
                for limit in [0, 1, 2, 3, 4, 20] {
                    let reference = run(&mut archive, None, None, pattern, text, limit, &b).0;
                    assert_eq!(run(&mut archive, Some(&index), None, pattern, text, limit, &b).0, reference);
                    assert_eq!(run(&mut archive, None, Some(&inverse), pattern, text, limit, &b).0, reference,
                        "text={text}, pattern={pattern:?}, limit={limit}");
                }
            }
        }
    }
}

#[test]
fn choose_the_rarest_list_not_the_first_gram_or_the_full_member_universe() {
    let names: Vec<_> = (0..512).map(|i| format!("file-{i:04}")).collect();
    let entries: Vec<_> = names.iter().enumerate().map(|(i, name)| entry(name.as_bytes(),
        if i == 511 { b"aaaaZ9" } else { b"aaa common" })).collect();
    let b = budget(); let mut archive = archive(&entries, true, &b);
    let index = index(&mut archive, IndexLimits::default(), &b);
    let inverse = index.invert(&b, id(6), || false).unwrap();
    let (reference, old) = run(&mut archive, Some(&index), None, b"aaaaZ9", true, 10, &b);
    let (actual, stats) = run(&mut archive, None, Some(&inverse), b"aaaaZ9", true, 10, &b);
    assert_eq!(actual, reference); assert_eq!(actual.hits.len(), 1);
    assert_eq!(old.members_visited, 512);
    assert_eq!(stats.members_visited, 1);
    assert_eq!(stats.posting_entries_visited, 1);
    assert_eq!(stats.posting_list_lookups, 3);
    assert_eq!(stats.index_eliminated_files, 511);
    assert!(stats.posting_cursor_complete);
    let (absent, stats) = run(&mut archive, None, Some(&inverse), b"QZX", true, 10, &b);
    assert!(absent.complete && absent.hits.is_empty());
    assert_eq!(stats.members_visited, 0); assert_eq!(stats.posting_entries_visited, 0);
    assert_eq!(stats.index_eliminated_files, 512);
}

#[test]
fn gram_cooccurrence_is_not_a_match_and_fallbacks_merge_once_in_source_order() {
    let le = utf16("abcd", true);
    let b = budget();
    let mut archive = archive(&[entry(b"a", &le), entry(b"b", b"abcXbcd"), entry(b"c", b"abcd"), entry(b"d", &le)], true, &b);
    let index = index(&mut archive, IndexLimits::default(), &b);
    let inverse = index.invert(&b, id(6), || false).unwrap();
    let (result, stats) = run(&mut archive, None, Some(&inverse), b"abcd", true, 10, &b);
    assert_eq!(result.hits.iter().map(|h| h.0).collect::<Vec<_>>(), [0, 2, 3]);
    assert_eq!(stats.files_searched, 4); assert_eq!(stats.index_fallback_files, 2);
    assert_eq!(stats.index_candidates, 2);
}

#[test]
fn inverse_roundtrip_and_legacy_conversion_preserve_the_original_segments() {
    let b = budget(); let mut archive = archive(&[entry(b"a", b"needle"), entry(b"b", b"aaaaa"), entry(b"c", b"\xffneedle")], true, &b);
    let index = index(&mut archive, IndexLimits::default(), &b);
    let forward = index.encode(&b, id(7), || false).unwrap();
    let inverse = index.invert(&b, id(6), || false).unwrap();
    let encoded = inverse.encode(&b, id(8), || false).unwrap();
    let reopened = SnapshotPostings::decode_pinned(encoded.bytes(), encoded.digest(), archive.directory(), &b, id(9), || false).unwrap();
    assert_eq!(reopened.posting_count(), inverse.posting_count());
    let wire_again = reopened.encode(&b, id(10), || false).unwrap();
    assert_eq!(wire_again.bytes(), encoded.bytes());
    let restored = SnapshotIndex::decode_pinned(encoded.bytes(), encoded.digest(), archive.directory(), &b, id(11), || false).unwrap();
    assert_eq!(restored.encode(&b, id(12), || false).unwrap().bytes(), forward.bytes());
    drop(index); drop(inverse); drop(encoded); drop(wire_again);
    assert_eq!(run(&mut archive, None, Some(&reopened), b"needle", false, 10, &b).0.hits.len(), 2);
}

#[test]
fn refreshed_generation_accepts_an_inverted_prior_without_resurrecting_removed_files() {
    let b = budget();
    let mut old = archive(&[entry(b"a", b"needle old"), entry(b"b", b"banana")], true, &b);
    let old_index = index(&mut old, IndexLimits::default(), &b);
    let inverse = old_index.invert(&b, id(6), || false).unwrap();
    let artifact = inverse.encode(&b, id(7), || false).unwrap();
    let restored = SnapshotIndex::decode_pinned(artifact.bytes(), artifact.digest(), old.directory(), &b, id(8), || false).unwrap();
    let target_bytes = encoded(&[entry(b"b", b"banana"), entry(b"new", b"needle new")], true);
    let mut target = PagedSnapshot::open(Cursor::new(target_bytes), owner(), SnapshotLimits::default(), &b, id(9), || false).unwrap();
    let refreshed = restored.refresh(&mut target, IndexLimits::default(), &b,
        [id(10), id(11), id(12), id(13), id(14)], || false).unwrap();
    let inverse = refreshed.index().invert(&b, id(15), || false).unwrap();
    let (result, _) = run(&mut target, None, Some(&inverse), b"needle", true, 10, &b);
    assert_eq!(result.hits.len(), 1); assert_eq!(result.hits[0].0, 1);
    assert!(result.complete);
}

#[test]
fn missing_members_and_unfinished_discovery_are_not_hidden_by_an_empty_posting_list() {
    for closed in [false, true] {
        let b = budget();
        let mut archive = archive(&[entry(b"a", b"unrelated"), SnapshotEntry { path: b"b", observed_bytes: 20,
            data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }], closed, &b);
        let index = index(&mut archive, IndexLimits::default(), &b);
        let inverse = index.invert(&b, id(6), || false).unwrap();
        let (result, stats) = run(&mut archive, None, Some(&inverse), b"needle", true, 10, &b);
        assert!(!result.complete); assert_eq!(result.unavailable, 1);
        assert_eq!(stats.members_visited, 0); assert_eq!(stats.index_eliminated_files, 1);
    }
}

#[test]
fn empty_scope_has_a_finite_complete_query_and_no_zero_byte_reservation() {
    let b = budget(); let mut archive = archive(&[], true, &b);
    let index = index(&mut archive, IndexLimits::default(), &b);
    let inverse = index.invert(&b, id(6), || false).unwrap();
    let encoded = inverse.encode(&b, id(7), || false).unwrap();
    let reopened = SnapshotPostings::decode_pinned(encoded.bytes(), encoded.digest(), archive.directory(), &b, id(8), || false).unwrap();
    let (result, stats) = run(&mut archive, None, Some(&reopened), b"x", true, 0, &b);
    assert!(result.complete); assert!(result.hits.is_empty()); assert_eq!(stats.members_visited, 0);
}

#[test]
fn construction_admission_and_cancellation_leave_old_generation_usable() {
    let b = budget(); let mut archive = archive(&[entry(b"a", b"needle")], true, &b);
    let index = index(&mut archive, IndexLimits::default(), &b);
    let baseline = b.accounting().reserved().get();
    assert!(matches!(index.invert(&b, id(6), || b.accounting().reserved().get() > baseline), Err(SnapshotIndexError::Canceled)));
    assert_eq!(b.accounting().reserved().get(), baseline);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(index.invert(&tiny, id(6), || false), Err(SnapshotIndexError::ResourceDenied)));
    assert_eq!(run(&mut archive, Some(&index), None, b"needle", true, 10, &b).0.hits.len(), 1);
}

#[test]
fn stale_and_canceled_posting_queries_release_active_source_and_cannot_publish() {
    let b = budget(); let mut archive = archive(&[entry(b"a", b"needle"), entry(b"b", b"needle")], true, &b);
    let index = index(&mut archive, IndexLimits::default(), &b);
    let inverse = index.invert(&b, id(6), || false).unwrap();
    let needle = IndexedNeedle::text(owner(), "needle", &b, id(7)).unwrap();
    let baseline = b.accounting().reserved().get();
    for stale in [false, true] {
        let mut query = PagedQuery::new_postings(&mut archive, &needle, &inverse, options(10), &b, [id(8), id(9), id(10)]).unwrap();
        let zero = StreamReadStep { max_bytes: 0, ..Default::default() };
        query.step(zero, generation(1), &b, || false).unwrap();
        assert_eq!(query.stats().posting_entries_visited, 0);
        let result = query.step(StreamReadStep::default(), generation(if stale { 2 } else { 1 }), &b, || !stale);
        assert_eq!(result, Err(if stale { PagedSearchError::StaleQuery } else { PagedSearchError::Canceled }));
        assert!(matches!(query.finish(), Err(PagedSearchError::Canceled)));
        assert_eq!(b.accounting().reserved().get(), baseline);
    }
}

#[test]
fn source_reader_activation_uses_the_same_verified_utf16_bytes_after_posting_search() {
    let bytes = utf16("head\r\nneedle", true);
    let b = budget(); let mut archive = archive(&[entry(b"a", &bytes)], true, &b);
    let index = index(&mut archive, IndexLimits::default(), &b);
    let inverse = index.invert(&b, id(6), || false).unwrap();
    let needle = IndexedNeedle::text(owner(), "needle", &b, id(7)).unwrap();
    let mut query = PagedQuery::new_postings(&mut archive, &needle, &inverse, options(10), &b, [id(8), id(9), id(10)]).unwrap();
    while query.state() == PagedQueryState::Pending { query.step(StreamReadStep::default(), generation(1), &b, || false).unwrap(); }
    let report = query.finish().unwrap();
    let hit = report.hits()[0]; assert_eq!(hit.original_range().start().get(), 14);
    let capture = PagedCapture::open_hit(&mut archive, hit, generation(1), &b, [id(11), id(12)], || false).unwrap();
    assert_eq!(capture.hit_bytes(hit).unwrap(), &bytes[14..]);
    drop(archive); drop(inverse); drop(index);
    assert_eq!(capture.bytes(), bytes);
}

struct Guarded { cursor: Cursor<Vec<u8>>, allowed: Rc<RefCell<Option<Range<u64>>>> }
impl Read for Guarded {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if let Some(range) = self.allowed.borrow().as_ref() {
            let pos = self.cursor.position();
            if pos < range.start || pos.saturating_add(out.len() as u64) > range.end {
                return Err(io::Error::other("UNSELECTED SOURCE READ"));
            }
        }
        self.cursor.read(out)
    }
}
impl Seek for Guarded { fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> { self.cursor.seek(pos) } }

#[test]
fn independent_guard_rejects_any_read_of_non_candidates() {
    let entries = [entry(b"a", b"unrelated data"), entry(b"b", b"needle witness"), entry(b"c", b"other source")];
    let data = encoded(&entries, true);
    let start = data.windows(b"needle witness".len()).position(|window| window == b"needle witness").unwrap() as u64;
    let allowed = Rc::new(RefCell::new(None));
    let b = budget();
    let mut archive = PagedSnapshot::open(Guarded { cursor: Cursor::new(data), allowed: Rc::clone(&allowed) },
        owner(), SnapshotLimits::default(), &b, id(1), || false).unwrap();
    let index = index(&mut archive, IndexLimits::default(), &b);
    let inverse = index.invert(&b, id(6), || false).unwrap();
    *allowed.borrow_mut() = Some(start..start + b"needle witness".len() as u64);
    assert_eq!(run(&mut archive, None, Some(&inverse), b"needle", true, 10, &b).0.hits.len(), 1);
    *allowed.borrow_mut() = Some(0..0);
    assert!(run(&mut archive, None, Some(&inverse), b"absent", true, 10, &b).0.complete);
    // Negative control: intentionally remove filtering. The guarded source must
    // reject the ordinary scanner instead of allowing an allegedly selective pass.
    let needle = StreamingNeedle::text(owner(), "absent", &b, id(7)).unwrap();
    let mut wrong = PagedQuery::new(&mut archive, &needle, options(10), &b, [id(8), id(9), id(10)]).unwrap();
    assert!(wrong.step(StreamReadStep::default(), generation(1), &b, || false).is_err());
}

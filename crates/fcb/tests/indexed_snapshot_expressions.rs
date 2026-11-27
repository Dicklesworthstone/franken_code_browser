#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::{cell::{Cell, RefCell}, io::{self, Cursor, Read, Seek, SeekFrom}, rc::Rc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{IndexLimits, QueryGeneration, ResourceAllocationId, ResourceBudget, StreamReadStep};
use fcb::search::expression::{ExpressionError, ExpressionHit, ExpressionOptions, ExpressionPlan,
    ExpressionQuery, ExpressionReport, ExpressionState};
use fcb::search::paged_snapshot::{PagedSearchError, PagedSnapshot};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::snapshot_index::SnapshotIndex;
use fcb::search::snapshot_index::paged::{PagedPostings, PagedPostingError, POSTING_PAGE_BYTES};
use fcb::search::snapshot_scope::SnapshotScope;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(8613).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn missing(path: &[u8]) -> SnapshotEntry<'_> {
    SnapshotEntry { path, observed_bytes: 20, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }
}
fn encoded(entries: &[SnapshotEntry<'_>], complete: bool, b: &ResourceBudget) -> Vec<u8> {
    SnapshotBytes::encode(owner(), complete, "indexed-expression-tests", entries,
        SnapshotLimits::default(), b, id(1), || false).unwrap().bytes().to_vec()
}
fn archive(entries: &[SnapshotEntry<'_>], complete: bool, b: &ResourceBudget) -> PagedSnapshot<Cursor<Vec<u8>>> {
    PagedSnapshot::open(Cursor::new(encoded(entries, complete, b)), owner(), SnapshotLimits::default(),
        b, id(2), || false).unwrap()
}
fn options(limit: usize, work: u64) -> ExpressionOptions {
    ExpressionOptions { generation: generation(1), first_file: FileId::new(owner(), 100).unwrap(),
        first_revision: SourceRevision::new(owner(), 200).unwrap(), max_matches: limit, max_scan_bytes: work }
}
fn quantum() -> StreamReadStep { StreamReadStep { max_bytes: 7, max_calls: 4, max_hits: 2 } }
fn finish<R: Read + Seek>(mut query: ExpressionQuery<'_, '_, R>, b: &ResourceBudget) -> ExpressionReport {
    for _ in 0..100_000 {
        if query.state() != ExpressionState::Pending { return query.finish().unwrap(); }
        query.step(quantum(), generation(1), b, || false).unwrap();
        assert!(query.stats().last_step_loaded_bytes <= 7);
        assert!(query.stats().last_step_scanned_bytes <= 7);
    }
    panic!("indexed expression did not terminate");
}
fn utf16(text: &str, big_endian: bool) -> Vec<u8> {
    let mut bytes = if big_endian { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() });
    }
    bytes
}

#[derive(Debug, Eq, PartialEq)]
struct Answer {
    hits: Vec<ExpressionHit>, state: ExpressionState,
    membership: (usize, usize, bool, bool),
    counts: (u64, usize, usize),
    window: (u64, u64, bool, Option<u64>, bool),
    metadata: (usize, usize, usize, usize, usize),
}
fn answer(report: &ExpressionReport) -> Answer {
    let stats = report.stats();
    Answer { hits: report.hits().to_vec(), state: report.state(),
        membership: (report.selected_members(), report.captured_members(), report.membership_complete(), report.is_complete()),
        counts: (report.matches_seen(), report.matching_files(), report.retained_matching_files()),
        window: (report.skip_matches(), report.skipped_matches(), report.has_more_matches(),
            report.next_match_offset(), report.hits_truncated()),
        metadata: (stats.metadata_examined, stats.scope_files, stats.metadata_excluded,
            stats.unavailable_files, stats.unsupported_files) }
}
fn indexed_accounting(report: &ExpressionReport, global: bool) {
    let stats = report.stats();
    let eligible = stats.scope_files - stats.unavailable_files;
    assert!(stats.index_candidates + stats.index_fallbacks + stats.index_eliminated <= eligible);
    assert!(stats.members_loaded <= stats.index_candidates + stats.index_fallbacks);
    assert!(stats.index_candidates + stats.index_fallbacks <= stats.members_loaded + 1);
    if global && !stats.posting_cursor_complete { assert_eq!(stats.index_eliminated, 0); }
    if report.state() == ExpressionState::Complete {
        assert_eq!(stats.members_loaded + stats.index_eliminated, eligible);
        assert_eq!(stats.index_candidates + stats.index_fallbacks, stats.members_loaded);
    }
}

#[test]
fn all_index_layouts_preserve_expression_windows_scopes_and_decode_fallbacks() {
    let le = utf16("banana required", false); let be = utf16("BANANA REQUIRED", true);
    let entries = [entry(b"a-outside.rs", b"banana required"), entry(b"src/a.rs", b"banana required"),
        entry(b"src/b.rs", b"BANANA REQUIRED"), entry(b"src/c.rs", b"banana required forbidden"),
        entry(b"src/d.rs", b"unrelated file"), entry(b"src/e.rs", &le), entry(b"src/f.rs", &be),
        entry(b"src/g.py", b"banana required"), entry(b"src/h.rs", b"bad\xffbanana required"),
        missing(b"src/lost.rs"), entry(b"src/\xff.rs", b"banana required"),
        entry(b"src2/decoy.rs", b"banana required")];
    for complete in [false, true] { for grams in [0, 100_000] {
        let b = budget(); let mut source = archive(&entries, complete, &b);
        let forward = SnapshotIndex::build(&mut source, IndexLimits { max_total_grams: grams, ..Default::default() },
            &b, [id(3), id(4), id(5), id(6)], || false).unwrap();
        let global = forward.invert(&b, id(7), || false).unwrap();
        let artifact = global.encode_paged(&b, id(8), || false).unwrap();
        let mut paged = PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(),
            source.directory(), 1, &b, id(9), || false).unwrap();
        for sensitive in [false, true] {
            for (text, scope, skip, limit, count) in [
                ("ana required -forbidden lang:rust", SnapshotScope::Directory(b"src"), 1, 2, true),
                ("ana required -forbidden lang:rust", SnapshotScope::Directory(b"src"), 0, 1, false),
                ("ana required -forbidden lang:rust", SnapshotScope::Directory(b"src"), 0, 0, false),
                ("ana lang:rust", SnapshotScope::Directory(b"src"), 0, 0, true),
                ("a required lang:rust", SnapshotScope::Directory(b"src"), 2, 3, true),
                ("ana required", SnapshotScope::Member(b"src/\xff.rs"), 1, 2, true),
                ("ana required", SnapshotScope::Member(b"src/missing.rs"), 0, 2, true),
            ] {
                let plan = ExpressionPlan::parse_with_matching(owner(), text, sensitive, &b, id(10), id(1000)).unwrap();
                let opts = options(limit, 100_000);
                let direct = finish(ExpressionQuery::new_scoped_page(&mut source, &plan, opts, scope,
                    skip, count, &b, [id(11), id(12), id(13)]).unwrap(), &b);
                let expected = answer(&direct); drop(direct);
                for route in 0..3 {
                    let query = match route {
                        0 => ExpressionQuery::new_indexed_scoped_page(&mut source, &plan, &forward, opts,
                            scope, skip, count, &b, [id(11), id(12), id(13)]),
                        1 => ExpressionQuery::new_postings_scoped_page(&mut source, &plan, &global, opts,
                            scope, skip, count, &b, [id(11), id(12), id(13)]),
                        _ => ExpressionQuery::new_paged_scoped_page(&mut source, &plan, &mut paged, opts,
                            scope, skip, count, &b, [id(11), id(12), id(13)]),
                    }.unwrap();
                    let actual = finish(query, &b);
                    assert_eq!(answer(&actual), expected,
                        "route={route} complete={complete} grams={grams} sensitive={sensitive} {text} {scope:?}");
                    indexed_accounting(&actual, route != 0);
                }
            }
        }
    } }
}

#[test]
fn ascii_index_negatives_save_source_budget_while_all_predicates_are_verified() {
    let b = budget(); let accepted = b"NEEDLE REQUIRED"; let absent_required = b"NEEDLE other";
    let excluded = b"NEEDLE REQUIRED BLOCKED";
    let mut source = archive(&[entry(b"outside/a.rs", b"NEEDLE REQUIRED"),
        entry(b"src/a.rs", accepted), entry(b"src/b.rs", absent_required), entry(b"src/c.rs", excluded),
        entry(b"src/d.rs", b"REQUIRED without primary"), entry(b"src/e.py", b"NEEDLE REQUIRED"),
        missing(b"src/missing.py"), entry(b"src2/decoy.rs", b"NEEDLE REQUIRED")], false, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(),
        &b, [id(3), id(4), id(5), id(6)], || false).unwrap();
    let global = forward.invert(&b, id(7), || false).unwrap();
    let artifact = global.encode_paged(&b, id(8), || false).unwrap();
    let mut paged = PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(),
        source.directory(), 1, &b, id(9), || false).unwrap();
    let plan = ExpressionPlan::parse_with_matching(owner(), "needle required -blocked lang:rust",
        false, &b, id(10), id(1000)).unwrap();
    let scope = SnapshotScope::Directory(b"src");
    let direct = finish(ExpressionQuery::new_scoped_page(&mut source, &plan, options(10, 1000),
        scope, 0, true, &b, [id(11), id(12), id(13)]).unwrap(), &b);
    let expected = answer(&direct); drop(direct);
    let work = (accepted.len() + absent_required.len() + excluded.len()) as u64;
    for route in 0..3 {
        let query = match route {
            0 => ExpressionQuery::new_indexed_scoped_page(&mut source, &plan, &forward, options(10, work),
                scope, 0, true, &b, [id(11), id(12), id(13)]),
            1 => ExpressionQuery::new_postings_scoped_page(&mut source, &plan, &global, options(10, work),
                scope, 0, true, &b, [id(11), id(12), id(13)]),
            _ => ExpressionQuery::new_paged_scoped_page(&mut source, &plan, &mut paged, options(10, work),
                scope, 0, true, &b, [id(11), id(12), id(13)]),
        }.unwrap();
        let report = finish(query, &b); let stats = report.stats();
        assert_eq!(answer(&report), expected);
        assert_eq!(report.state(), ExpressionState::Complete); assert!(!report.is_complete());
        assert_eq!(report.matches_seen(), 1); assert_eq!(report.selected_members(), 6);
        assert_eq!((stats.scope_files, stats.metadata_excluded, stats.unavailable_files), (4, 2, 0));
        assert_eq!((stats.index_candidates, stats.index_fallbacks, stats.index_eliminated), (3, 0, 1));
        assert_eq!((stats.members_loaded, stats.primary_scans, stats.predicate_scans), (3, 3, 6));
        assert_eq!(stats.predicate_rejections, 2);
        assert_eq!(stats.scanned_bytes, work); assert_eq!(stats.loaded_bytes, work);
        indexed_accounting(&report, route != 0);
    }
}

#[test]
fn pending_global_cursors_do_not_publish_negative_totals_or_spend_zero_source_budget() {
    let b = budget(); let mut source = archive(&[entry(b"a.rs", b"needle required"),
        entry(b"b.rs", b"needle required"), entry(b"c.rs", b"absent required"),
        entry(b"z.rs", b"needle required")], true, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(),
        &b, [id(3), id(4), id(5), id(6)], || false).unwrap();
    let global = forward.invert(&b, id(7), || false).unwrap();
    let artifact = global.encode_paged(&b, id(8), || false).unwrap();
    let mut paged = PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(),
        source.directory(), 1, &b, id(9), || false).unwrap();
    let plan = ExpressionPlan::parse(owner(), "needle required", &b, id(10), id(1000)).unwrap();
    for route in 0..2 {
        for (scope, work, expected_state, loaded, admitted, eliminated) in [
            (SnapshotScope::Archive, 1000, ExpressionState::Truncated, 2, 2, 0),
            (SnapshotScope::Archive, 0, ExpressionState::WorkLimit, 0, 1, 0),
            (SnapshotScope::Member(b"c.rs"), 0, ExpressionState::Complete, 0, 0, 1),
        ] {
            let before = source.load_stats().bytes_read;
            let query = if route == 0 {
                ExpressionQuery::new_postings_scoped_page(&mut source, &plan, &global, options(1, work),
                    scope, 0, false, &b, [id(11), id(12), id(13)])
            } else {
                ExpressionQuery::new_paged_scoped_page(&mut source, &plan, &mut paged, options(1, work),
                    scope, 0, false, &b, [id(11), id(12), id(13)])
            }.unwrap();
            let report = finish(query, &b); let stats = report.stats();
            assert_eq!(report.state(), expected_state);
            assert_eq!((stats.members_loaded, stats.index_candidates, stats.index_eliminated), (loaded, admitted, eliminated));
            assert_eq!(stats.index_fallbacks, 0);
            assert_eq!(stats.posting_cursor_complete, expected_state == ExpressionState::Complete);
            if work == 0 {
                assert_eq!(report.matches_seen(), 0); assert_eq!(stats.loaded_bytes, 0);
                assert_eq!(source.load_stats().bytes_read, before);
            }
            indexed_accounting(&report, true);
        }
    }
}

struct Guard {
    data: Rc<RefCell<Vec<u8>>>, position: u64, body: usize,
    deny_body: Rc<Cell<bool>>, bytes_read: Rc<Cell<usize>>,
}
impl Guard {
    fn source(bytes: Vec<u8>) -> Self {
        Self { data: Rc::new(RefCell::new(bytes)), position: 0, body: 0,
            deny_body: Rc::new(Cell::new(false)), bytes_read: Rc::new(Cell::new(0)) }
    }
    fn index(bytes: &[u8]) -> Self {
        let mut input = Self::source(bytes.to_vec());
        input.body = 32 + u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
        input.deny_body.set(true); input
    }
}
impl Read for Guard {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let start = self.position as usize; let data = self.data.borrow();
        let n = out.len().min(data.len().saturating_sub(start));
        if self.deny_body.get() && n > 0 && start + n > self.body {
            return Err(io::Error::other("unadmitted posting page read"));
        }
        if n > 0 { out[..n].copy_from_slice(&data[start..start + n]); }
        self.bytes_read.set(self.bytes_read.get() + n); self.position += n as u64; Ok(n)
    }
}
impl Seek for Guard {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let at = match from { SeekFrom::Start(n) => n as i128,
            SeekFrom::End(n) => self.data.borrow().len() as i128 + n as i128,
            SeekFrom::Current(n) => self.position as i128 + n as i128 };
        self.position = u64::try_from(at).map_err(|_| io::Error::other("negative seek"))?; Ok(self.position)
    }
}

#[test]
fn metadata_empty_scopes_and_cancellation_do_no_index_page_or_source_io() {
    let b = budget(); let mut source = archive(&[entry(b"src/a.rs", b"needle required"),
        missing(b"src/lost.rs"), entry(b"src2/b.rs", b"needle required")], false, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(),
        &b, [id(3), id(4), id(5), id(6)], || false).unwrap();
    let global = forward.invert(&b, id(7), || false).unwrap();
    let artifact = global.encode_paged(&b, id(8), || false).unwrap();
    let input = Guard::index(artifact.bytes()); let reads = input.bytes_read.clone();
    let mut paged = PagedPostings::open_pinned(input, artifact.digest(), source.directory(), 1, &b, id(9), || false).unwrap();
    let before = reads.get(); let source_before = source.load_stats().bytes_read;
    for (text, scope) in [
        ("needle lang:python", SnapshotScope::Directory(b"src")),
        ("needle", SnapshotScope::Member(b"src/lost.rs")),
        ("needle", SnapshotScope::Member(b"src/missing.rs")),
    ] {
        let plan = ExpressionPlan::parse(owner(), text, &b, id(10), id(1000)).unwrap();
        let report = finish(ExpressionQuery::new_paged_scoped_page(&mut source, &plan, &mut paged,
            options(2, 0), scope, 0, true, &b, [id(11), id(12), id(13)]).unwrap(), &b);
        assert_eq!(report.state(), ExpressionState::Complete);
        assert_eq!(report.stats().index_eliminated, 0);
        assert!(!report.stats().posting_cursor_complete);
        indexed_accounting(&report, true);
    }
    let plan = ExpressionPlan::parse(owner(), "needle required", &b, id(10), id(1000)).unwrap();
    for stale in [false, true] {
        let mut query = ExpressionQuery::new_paged(&mut source, &plan, &mut paged,
            options(10, 1000), &b, [id(11), id(12), id(13)]).unwrap();
        let mut zero = quantum(); zero.max_bytes = 0;
        assert_eq!(query.step(zero, generation(1), &b, || false).unwrap(), ExpressionState::Pending);
        assert_eq!(query.step(quantum(), generation(if stale { 2 } else { 1 }), &b, || !stale),
            Err(if stale { ExpressionError::StaleQuery } else { ExpressionError::Canceled }));
        assert!(matches!(query.finish(), Err(ExpressionError::Canceled)));
    }
    // Cancellation inside the posting cursor must keep the query canceled,
    // rather than wrapping that signal as a generic index failure.
    let mut query = ExpressionQuery::new_paged(&mut source, &plan, &mut paged,
        options(10, 1000), &b, [id(11), id(12), id(13)]).unwrap();
    for _ in 0..3 { query.step(quantum(), generation(1), &b, || false).unwrap(); }
    assert_eq!(query.stats().metadata_examined, 3);
    let mut checks = 0;
    assert_eq!(query.step(quantum(), generation(1), &b, || { checks += 1; checks == 2 }),
        Err(ExpressionError::Canceled));
    assert!(matches!(query.finish(), Err(ExpressionError::Canceled)));
    assert_eq!(reads.get(), before); assert_eq!(paged.io_stats().page_bytes_read, 0);
    assert_eq!(source.load_stats().bytes_read, source_before);
}

#[test]
fn changed_posting_page_fails_before_source_and_verified_steps_keep_one_page_bound() {
    for corrupt in [false, true] {
        let b = budget(); let mut source = archive(&[entry(b"a.rs", b"needle required"),
            entry(b"b.rs", b"needless other")], true, &b);
        let forward = SnapshotIndex::build(&mut source, IndexLimits::default(),
            &b, [id(3), id(4), id(5), id(6)], || false).unwrap();
        let global = forward.invert(&b, id(7), || false).unwrap();
        let artifact = global.encode_paged(&b, id(8), || false).unwrap();
        let input = Guard::index(artifact.bytes()); input.deny_body.set(false);
        let data = input.data.clone(); let body = input.body; let reads = input.bytes_read.clone();
        let mut paged = PagedPostings::open_pinned(input, artifact.digest(), source.directory(), 1, &b, id(9), || false).unwrap();
        if corrupt { data.borrow_mut()[body] ^= 1; }
        let plan = ExpressionPlan::parse(owner(), "needle required", &b, id(10), id(1000)).unwrap();
        let source_before = source.load_stats().bytes_read;
        let mut query = ExpressionQuery::new_paged_counting(&mut source, &plan, &mut paged,
            options(10, 1000), &b, [id(11), id(12), id(13)]).unwrap();
        let mut failure = None;
        for _ in 0..100_000 {
            if query.state() != ExpressionState::Pending { break; }
            let before = reads.get(); let result = query.step(quantum(), generation(1), &b, || false);
            assert!(reads.get() - before <= POSTING_PAGE_BYTES, "expression step loaded multiple posting pages");
            if let Err(error) = result { failure = Some(error); break; }
        }
        if corrupt {
            let error = ExpressionError::Search(PagedSearchError::PagedIndex(PagedPostingError::ChangedPage));
            assert_eq!(failure, Some(error)); assert!(matches!(query.finish(), Err(e) if e == error));
            assert_eq!(source.load_stats().bytes_read, source_before);
        } else {
            assert_eq!(failure, None); let report = query.finish().unwrap();
            assert_eq!(report.matches_seen(), 1); assert!(report.is_complete());
            indexed_accounting(&report, true);
        }
        assert!(paged.resident_pages() <= 1);
    }
}

#[test]
fn unicode_fallback_hits_keep_original_coordinates_and_reject_changed_source() {
    let b = budget(); let kelvin = "KELVIN REQUIRED"; let le = utf16(kelvin, false);
    let mut source = archive(&[entry(b"src/a.rs", kelvin.as_bytes()), entry(b"src/b.rs", &le),
        entry(b"src/\xff.rs", b"KELVIN REQUIRED")], false, &b);
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(),
        &b, [id(3), id(4), id(5), id(6)], || false).unwrap();
    let global = forward.invert(&b, id(7), || false).unwrap();
    let artifact = global.encode_paged(&b, id(8), || false).unwrap();
    let mut paged = PagedPostings::open_pinned(Cursor::new(artifact.bytes()), artifact.digest(),
        source.directory(), 1, &b, id(9), || false).unwrap();
    let plan = ExpressionPlan::parse_with_matching(owner(), "kelvin required", false, &b, id(10), id(1000)).unwrap();
    let report = finish(ExpressionQuery::new_paged_counting(&mut source, &plan, &mut paged,
        options(10, 1000), &b, [id(11), id(12), id(13)]).unwrap(), &b);
    assert_eq!(report.matches_seen(), 3);
    let hits = report.hits().to_vec(); drop(report); drop(paged);
    assert_eq!(hits.iter().map(|hit| hit.original_range().as_usize_bounds().unwrap()).collect::<Vec<_>>(),
        [(0, 8), (2, 14), (0, 6)]);
    for &hit in &hits {
        let capture = hit.open(&mut source, generation(1), &b, [id(20), id(21)], || false).unwrap();
        let (start, end) = hit.original_range().as_usize_bounds().unwrap();
        let expected = match hit.ordinal() { 0 => kelvin.as_bytes(), 1 => &le, _ => b"KELVIN REQUIRED" };
        assert_eq!(&capture.bytes()[start..end], &expected[start..end]);
        assert!(matches!(hit.open(&mut source, generation(2), &b, [id(22), id(23)], || false),
            Err(ExpressionError::StaleQuery)));
    }
    drop(plan); drop(artifact); drop(global); drop(forward); drop(source);

    let payload = b"needle required and a verified tail";
    let bytes = encoded(&[entry(b"a.rs", payload)], true, &b);
    let at = bytes.windows(payload.len()).position(|window| window == payload).unwrap();
    let input = Guard::source(bytes); let mutable = input.data.clone();
    let mut source = PagedSnapshot::open(input, owner(), SnapshotLimits::default(), &b, id(2), || false).unwrap();
    let forward = SnapshotIndex::build(&mut source, IndexLimits::default(),
        &b, [id(3), id(4), id(5), id(6)], || false).unwrap();
    let global = forward.invert(&b, id(7), || false).unwrap();
    let plan = ExpressionPlan::parse(owner(), "needle required", &b, id(10), id(1000)).unwrap();
    mutable.borrow_mut()[at + payload.len() - 1] ^= 1;
    let mut query = ExpressionQuery::new_postings(&mut source, &plan, &global,
        options(10, 1000), &b, [id(11), id(12), id(13)]).unwrap();
    for _ in 0..100_000 {
        if query.state() != ExpressionState::Pending { break; }
        if query.step(quantum(), generation(1), &b, || false).is_err() { break; }
    }
    assert!(matches!(query.state(), ExpressionState::Failed(_)));
    assert!(query.finish().is_err(), "a valid index cannot certify altered source witnesses");
}

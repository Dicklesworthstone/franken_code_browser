#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use fcb::search::paged_snapshot::{
    IndexedNeedle, PagedMemberData, PagedQuery, PagedQueryOptions, PagedQueryState, PagedReport,
    PagedSearchError, PagedSnapshot, PagedSnapshotError,
};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::snapshot_index::SnapshotIndex;
use fcb::search::{
    IndexLimits, QueryGeneration, ResourceAllocationId, ResourceBudget, StreamReadStep,
    StreamingNeedle,
};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use std::{
    cell::RefCell,
    io::{self, Read, Seek, SeekFrom},
    rc::Rc,
};

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(2852).unwrap()
}
fn allocation(n: u64) -> ResourceAllocationId {
    ResourceAllocationId::new(n).unwrap()
}
fn budget(bytes: u64) -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(bytes)).unwrap()
}
fn options(limit: usize) -> PagedQueryOptions {
    PagedQueryOptions {
        generation: QueryGeneration::new(owner(), 1).unwrap(),
        first_file: FileId::new(owner(), 100).unwrap(),
        first_revision: SourceRevision::new(owner(), 200).unwrap(),
        max_matches: limit,
    }
}
fn entry<'a>(path: &'a [u8], source: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry {
        path,
        observed_bytes: source.len() as u64,
        data: SnapshotData::Captured(source),
    }
}

#[derive(Default)]
struct Trace {
    bytes: Vec<u8>,
    calls: u64,
    read: u64,
    seeks: u64,
    interrupts: usize,
    short: usize,
}
struct Input {
    trace: Rc<RefCell<Trace>>,
    position: usize,
}
impl Read for Input {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let mut trace = self.trace.borrow_mut();
        trace.calls += 1;
        if trace.interrupts > 0 {
            trace.interrupts -= 1;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let n = into
            .len()
            .min(trace.short)
            .min(trace.bytes.len().saturating_sub(self.position));
        into[..n].copy_from_slice(&trace.bytes[self.position..self.position + n]);
        self.position += n;
        trace.read += n as u64;
        Ok(n)
    }
}
impl Seek for Input {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let mut trace = self.trace.borrow_mut();
        trace.seeks += 1;
        let n = match from {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(n) => self.position as i128 + n as i128,
            SeekFrom::End(n) => trace.bytes.len() as i128 + n as i128,
        };
        self.position =
            usize::try_from(n).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(self.position as u64)
    }
}
fn reset(trace: &Rc<RefCell<Trace>>) {
    let mut trace = trace.borrow_mut();
    trace.calls = 0;
    trace.read = 0;
    trace.seeks = 0;
}
fn archive(
    entries: &[SnapshotEntry<'_>],
    memory: &ResourceBudget,
) -> (PagedSnapshot<Input>, Rc<RefCell<Trace>>) {
    // The host-supplied input is a test backing store. Its existing bytes do not
    // belong to the query's managed source cache or scratch allowance.
    let encoding = budget(128 * 1024 * 1024);
    let max_file_bytes = entries
        .iter()
        .filter_map(|entry| match entry.data {
            SnapshotData::Captured(bytes) => Some(bytes.len()),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .max(1);
    let limits = SnapshotLimits {
        max_file_bytes,
        ..SnapshotLimits::default()
    };
    let saved = SnapshotBytes::encode(
        owner(),
        true,
        "streamed-query-test-v1",
        entries,
        limits,
        &encoding,
        allocation(1),
        || false,
    )
    .unwrap();
    let trace = Rc::new(RefCell::new(Trace {
        bytes: saved.bytes().to_vec(),
        short: usize::MAX,
        ..Trace::default()
    }));
    let archive = PagedSnapshot::open(
        Input {
            trace: trace.clone(),
            position: 0,
        },
        owner(),
        limits,
        memory,
        allocation(1),
        || false,
    )
    .unwrap();
    reset(&trace);
    (archive, trace)
}
fn finish(
    mut query: PagedQuery<'_, '_, Input>,
    memory: &ResourceBudget,
    trace: &Rc<RefCell<Trace>>,
    step: StreamReadStep,
) -> PagedReport {
    let generation = query.generation();
    for _ in 0..100_000 {
        if query.state() != PagedQueryState::Pending {
            let reads = trace.borrow().read;
            let calls = trace.borrow().calls;
            query.step(step, generation, memory, || false).unwrap();
            assert_eq!(
                (trace.borrow().read, trace.borrow().calls),
                (reads, calls),
                "terminal query steps never reread the input"
            );
            return query.finish().unwrap();
        }
        let reads = trace.borrow().read;
        let calls = trace.borrow().calls;
        query.step(step, generation, memory, || false).unwrap();
        assert!(trace.borrow().read - reads <= step.max_bytes as u64);
        assert!(trace.borrow().calls - calls <= step.max_calls.min(32) as u64);
        assert_eq!(
            query.stats().last_step_loaded_bytes as u64,
            trace.borrow().read - reads
        );
        assert_eq!(
            query.stats().last_step_read_calls as u64,
            trace.borrow().calls - calls
        );
    }
    panic!("saved query did not finish within bounded fixture work");
}
fn coordinates(report: &PagedReport) -> Vec<(usize, u64, u64)> {
    report
        .hits()
        .iter()
        .map(|hit| {
            (
                hit.ordinal(),
                hit.original_range().start().get(),
                hit.original_range().end().get(),
            )
        })
        .collect()
}
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

#[test]
fn large_member_search_fits_a_budget_that_cannot_hold_the_source() {
    let memory = budget(16 * 1024 * 1024);
    let mut source = vec![b'x'; 24 * 1024 * 1024];
    let offsets = [0, 16 * 1024 - 2, source.len() - 6];
    for start in offsets {
        source[start..start + 6].copy_from_slice(b"needle");
    }
    let (mut archive, trace) = archive(&[entry(b"large.txt", &source)], &memory);
    assert!(
        matches!(
            archive.load(0, &memory, allocation(90), || false),
            Err(PagedSnapshotError::ResourceDenied)
        ),
        "negative control: old whole-member load cannot fit"
    );
    let needle = StreamingNeedle::text(owner(), "needle", &memory, allocation(2)).unwrap();
    let baseline = memory.accounting().reserved();
    let opts = options(8);
    let query = PagedQuery::new_counting(
        &mut archive,
        &needle,
        opts,
        &memory,
        [allocation(3), allocation(4), allocation(5)],
    )
    .unwrap();
    let report = finish(
        query,
        &memory,
        &trace,
        StreamReadStep {
            max_bytes: 4096,
            max_calls: 1,
            max_hits: 4,
        },
    );
    assert!(report.is_complete());
    assert_eq!(report.matches_seen(), 3);
    assert_eq!(
        coordinates(&report),
        offsets.map(|start| (0, start as u64, start as u64 + 6))
    );
    assert_eq!(report.stats().loaded_bytes, source.len() as u64);
    assert_eq!(
        report.stats().read_calls,
        source.len().div_ceil(4096) as u64
    );
    assert_eq!(report.stats().peak_source_bytes, 4096);
    assert_eq!(report.stats().files_searched, 1);
    assert_eq!(archive.load_stats().loaded_members, 1);
    assert_eq!(report.archive_digest(), archive.directory().digest());
    drop(report);
    assert_eq!(
        memory.accounting().reserved(),
        baseline,
        "all per-query scratch and result leases release"
    );
}

#[test]
fn early_match_limit_still_verifies_the_unread_suffix_before_publishing() {
    for corrupt in [false, true] {
        let memory = budget(32 * 1024 * 1024);
        let mut source = vec![b'x'; 128 * 1024];
        source[..13].copy_from_slice(b"needle needle");
        let (mut archive, trace) = archive(&[entry(b"a.txt", &source)], &memory);
        let offset = match archive.directory().member(0).unwrap().data {
            PagedMemberData::Captured { archive_offset, .. } => archive_offset as usize,
            _ => unreachable!(),
        };
        if corrupt {
            trace.borrow_mut().bytes[offset + source.len() - 1] = b'!';
        }
        let needle = StreamingNeedle::text(owner(), "needle", &memory, allocation(2)).unwrap();
        let opts = options(1);
        let step = StreamReadStep {
            max_bytes: 32,
            max_calls: 1,
            max_hits: 64,
        };
        let mut query = PagedQuery::new(
            &mut archive,
            &needle,
            opts,
            &memory,
            [allocation(3), allocation(4), allocation(5)],
        )
        .unwrap();
        assert_eq!(
            query
                .step(step, opts.generation, &memory, || false)
                .unwrap(),
            PagedQueryState::Pending
        );
        assert_eq!(
            query.stats().files_searched,
            0,
            "prefix hits remain private until full integrity verification"
        );
        assert_eq!(query.stats().loaded_bytes, 32);
        assert!(query.stats().scanned_bytes < source.len() as u64);
        if corrupt {
            let mut failed = false;
            for _ in 0..10_000 {
                let before = trace.borrow().read;
                match query.step(step, opts.generation, &memory, || false) {
                    Err(PagedSearchError::Archive(PagedSnapshotError::Changed)) => {
                        failed = true;
                        break;
                    }
                    Ok(PagedQueryState::Pending) => {}
                    other => panic!("corrupt suffix produced unexpected state: {other:?}"),
                }
                assert!(trace.borrow().read - before <= 32);
                assert_eq!(query.stats().files_searched, 0);
            }
            assert!(failed);
            assert!(matches!(
                query.finish(),
                Err(PagedSearchError::Archive(PagedSnapshotError::Changed))
            ));
            assert_eq!(archive.load_stats().loaded_members, 0);
        } else {
            let report = finish(query, &memory, &trace, step);
            assert!(report.truncated());
            assert!(!report.is_complete());
            assert_eq!(report.matches_seen(), 2);
            assert_eq!(coordinates(&report), [(0, 0, 6)]);
            assert_eq!(report.stats().loaded_bytes, source.len() as u64);
            assert_eq!(report.stats().peak_source_bytes, 32);
            assert_eq!(report.stats().files_searched, 1);
        }
    }
}

#[test]
fn persisted_candidates_and_uncovered_utf16_members_keep_exact_window_results() {
    let memory = budget(64 * 1024 * 1024);
    let wide = utf16("banana");
    let mut large = vec![b'x'; 65 * 1024];
    large.extend_from_slice(b"banana");
    let entries = [
        entry(b"a.txt", b"banana banana"),
        entry(b"b.txt", b"absent"),
        entry(b"c.txt", &wide),
        entry(b"d.txt", &large),
    ];
    let (mut archive, trace) = archive(&entries, &memory);
    let limits = IndexLimits {
        max_source_bytes_per_file: 1024,
        ..IndexLimits::default()
    };
    let index = SnapshotIndex::build(
        &mut archive,
        limits,
        &memory,
        [
            allocation(10),
            allocation(11),
            allocation(12),
            allocation(13),
        ],
        || false,
    )
    .unwrap();
    let needle = StreamingNeedle::text(owner(), "ana", &memory, allocation(2)).unwrap();
    let indexed = IndexedNeedle::text(owner(), "ana", &memory, allocation(20)).unwrap();
    let opts = options(20);
    reset(&trace);
    let query = PagedQuery::new_page(
        &mut archive,
        &needle,
        opts,
        2,
        true,
        &memory,
        [allocation(3), allocation(4), allocation(5)],
    )
    .unwrap();
    let direct = finish(query, &memory, &trace, StreamReadStep::default());
    assert!(direct.is_complete());
    assert_eq!(direct.matches_seen(), 8);
    reset(&trace);
    let query = PagedQuery::new_indexed_page(
        &mut archive,
        &indexed,
        &index,
        opts,
        2,
        true,
        &memory,
        [allocation(6), allocation(7), allocation(8)],
    )
    .unwrap();
    let accelerated = finish(
        query,
        &memory,
        &trace,
        StreamReadStep {
            max_bytes: 3,
            max_calls: 1,
            max_hits: 1,
        },
    );
    assert!(accelerated.is_complete());
    assert_eq!(coordinates(&accelerated), coordinates(&direct));
    assert_eq!(accelerated.matches_seen(), direct.matches_seen());
    assert_eq!(accelerated.skipped_matches(), 2);
    assert_eq!(accelerated.stats().index_candidates, 1);
    assert_eq!(accelerated.stats().index_eliminated_files, 1);
    assert_eq!(accelerated.stats().index_fallback_files, 2);
    assert_eq!(
        accelerated.stats().loaded_bytes,
        (13 + wide.len() + large.len()) as u64
    );
    assert_eq!(accelerated.stats().loaded_bytes, trace.borrow().read);
}

#[test]
fn zero_allowances_short_reads_cancel_and_replay_do_not_hide_io() {
    let memory = budget(32 * 1024 * 1024);
    let (mut archive, trace) = archive(&[entry(b"a.txt", b"banana")], &memory);
    let needle = StreamingNeedle::text(owner(), "ana", &memory, allocation(2)).unwrap();
    let baseline = memory.accounting().reserved();
    let opts = options(8);
    let mut query = PagedQuery::new_counting(
        &mut archive,
        &needle,
        opts,
        &memory,
        [allocation(3), allocation(4), allocation(5)],
    )
    .unwrap();
    for step in [
        StreamReadStep {
            max_bytes: 0,
            max_calls: 1,
            max_hits: 1,
        },
        StreamReadStep {
            max_bytes: 1,
            max_calls: 0,
            max_hits: 1,
        },
        StreamReadStep {
            max_bytes: 1,
            max_calls: 1,
            max_hits: 0,
        },
    ] {
        assert_eq!(
            query
                .step(step, opts.generation, &memory, || false)
                .unwrap(),
            PagedQueryState::Pending
        );
    }
    assert_eq!(
        (
            trace.borrow().read,
            trace.borrow().calls,
            trace.borrow().seeks
        ),
        (0, 0, 0)
    );
    {
        let mut trace = trace.borrow_mut();
        trace.interrupts = 3;
        trace.short = 1;
    }
    let step = StreamReadStep {
        max_bytes: 2,
        max_calls: 2,
        max_hits: 1,
    };
    query
        .step(step, opts.generation, &memory, || false)
        .unwrap();
    assert_eq!(query.stats().last_step_read_calls, 2);
    assert_eq!(query.stats().last_step_loaded_bytes, 0);
    query
        .step(step, opts.generation, &memory, || false)
        .unwrap();
    assert_eq!(query.stats().last_step_read_calls, 2);
    assert_eq!(query.stats().last_step_loaded_bytes, 1);
    let calls = trace.borrow().calls;
    assert_eq!(
        query.step(step, opts.generation, &memory, || true),
        Err(PagedSearchError::Canceled)
    );
    assert_eq!(trace.borrow().calls, calls);
    drop(query);
    assert_eq!(memory.accounting().reserved(), baseline);
    reset(&trace);
    let query = PagedQuery::new_counting(
        &mut archive,
        &needle,
        opts,
        &memory,
        [allocation(3), allocation(4), allocation(5)],
    )
    .unwrap();
    let report = finish(query, &memory, &trace, step);
    assert!(report.is_complete());
    assert_eq!(coordinates(&report), [(0, 1, 4), (0, 3, 6)]);
    assert_eq!(report.stats().loaded_bytes, 6);
}

#[test]
fn unsupported_text_still_verifies_the_tail_while_raw_bytes_remain_searchable() {
    for corrupt in [false, true] {
        let memory = budget(32 * 1024 * 1024);
        let mut source = vec![b'x'; 64 * 1024];
        source[..6].copy_from_slice(b"needle");
        let invalid = 16 * 1024 + 7;
        source[invalid] = 0xff;
        let (mut archive, trace) = archive(&[entry(b"binary.dat", &source)], &memory);
        let offset = match archive.directory().member(0).unwrap().data {
            PagedMemberData::Captured { archive_offset, .. } => archive_offset as usize,
            _ => unreachable!(),
        };
        if corrupt {
            trace.borrow_mut().bytes[offset + source.len() - 1] = b'!';
        }
        let text = StreamingNeedle::text(owner(), "needle", &memory, allocation(2)).unwrap();
        let opts = options(8);
        let mut query = PagedQuery::new_counting(
            &mut archive,
            &text,
            opts,
            &memory,
            [allocation(3), allocation(4), allocation(5)],
        )
        .unwrap();
        let step = StreamReadStep::default();
        assert_eq!(
            query
                .step(step, opts.generation, &memory, || false)
                .unwrap(),
            PagedQueryState::Pending
        );
        assert_eq!(
            query.stats().scanned_bytes,
            16 * 1024,
            "a matching valid prefix was already scanned"
        );
        assert_eq!(query.stats().files_searched, 0);
        assert_eq!(
            query.stats().unsupported_files,
            0,
            "unsupported classification also waits for integrity"
        );
        if corrupt {
            let mut failed = false;
            for _ in 0..16 {
                if matches!(
                    query.step(step, opts.generation, &memory, || false),
                    Err(PagedSearchError::Archive(PagedSnapshotError::Changed))
                ) {
                    failed = true;
                    break;
                }
            }
            assert!(failed);
            assert!(matches!(
                query.finish(),
                Err(PagedSearchError::Archive(PagedSnapshotError::Changed))
            ));
        } else {
            let report = finish(query, &memory, &trace, step);
            assert!(!report.is_complete());
            assert_eq!(report.stats().unsupported_files, 1);
            assert_eq!(report.stats().loaded_bytes, source.len() as u64);
            assert_eq!(report.matches_seen(), 0);
            assert_eq!(report.matching_files(), 0);
            assert!(
                report.hits().is_empty(),
                "later unsupported text must discard every prefix candidate"
            );
            drop(report);
            let raw = StreamingNeedle::raw(owner(), &[0xff], &memory, allocation(6)).unwrap();
            reset(&trace);
            let query = PagedQuery::new_counting(
                &mut archive,
                &raw,
                opts,
                &memory,
                [allocation(3), allocation(4), allocation(5)],
            )
            .unwrap();
            let report = finish(query, &memory, &trace, step);
            assert!(report.is_complete());
            assert_eq!(
                coordinates(&report),
                [(0, invalid as u64, invalid as u64 + 1)]
            );
        }
    }
}

#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use fcb::search::expression::{
    ExpressionError, ExpressionOptions, ExpressionPlan, ExpressionQuery, ExpressionState,
};
use fcb::search::paged_snapshot::PagedSnapshot;
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb::search::{QueryGeneration, ResourceAllocationId, ResourceBudget, StreamReadStep};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use std::{
    cell::RefCell,
    io::{self, Read, Seek, SeekFrom},
    rc::Rc,
};

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(2853).unwrap()
}
fn allocation(n: u64) -> ResourceAllocationId {
    ResourceAllocationId::new(n).unwrap()
}
fn budget(bytes: u64) -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(bytes)).unwrap()
}
fn options(limit: usize) -> ExpressionOptions {
    ExpressionOptions {
        generation: QueryGeneration::new(owner(), 1).unwrap(),
        first_file: FileId::new(owner(), 100).unwrap(),
        first_revision: SourceRevision::new(owner(), 200).unwrap(),
        max_matches: limit,
        max_scan_bytes: 1024 * 1024,
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

fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}
fn step(
    query: &mut ExpressionQuery<'_, '_, Input>,
    options: ExpressionOptions,
    memory: &ResourceBudget,
    trace: &Rc<RefCell<Trace>>,
    quantum: StreamReadStep,
) -> ExpressionState {
    let before = (trace.borrow().read, trace.borrow().calls);
    let state = query
        .step(quantum, options.generation, memory, || false)
        .unwrap();
    let after = (trace.borrow().read, trace.borrow().calls);
    assert!(after.0 - before.0 <= quantum.max_bytes as u64);
    assert!(after.1 - before.1 <= quantum.max_calls.min(32) as u64);
    assert_eq!(
        query.stats().last_step_loaded_bytes as u64,
        after.0 - before.0
    );
    assert_eq!(
        query.stats().last_step_read_calls as u64,
        after.1 - before.1
    );
    state
}

#[test]
fn late_predicates_share_one_bounded_utf16_lowercase_observation() {
    let memory = budget(32 * 1024 * 1024);
    let filler = "x".repeat(4096);
    let admitted = utf16(&format!("BANANA 😀{filler} REQUIRED"));
    let excluded = format!("banana {filler} required FORBIDDEN").into_bytes();
    let entries = [
        entry(b"a.rs", &admitted),
        entry(b"b.rs", &excluded),
        entry(b"c.txt", b"banana required"),
    ];
    let (mut archive, trace) = archive(&entries, &memory);
    let plan = ExpressionPlan::parse_with_matching(
        owner(),
        "ana required -forbidden lang:rust",
        false,
        &memory,
        allocation(2),
        allocation(100),
    )
    .unwrap();
    let baseline = memory.accounting().reserved();
    let options = options(1);
    let mut query = ExpressionQuery::new_page(
        &mut archive,
        &plan,
        options,
        1,
        true,
        &memory,
        [allocation(3), allocation(4), allocation(5)],
    )
    .unwrap();
    // One-byte short reads split the BOM, code units and the emoji surrogate pair.
    {
        let mut trace = trace.borrow_mut();
        trace.short = 1;
        trace.interrupts = 2;
    }
    let quantum = StreamReadStep {
        max_bytes: 3,
        max_calls: 2,
        max_hits: 1,
    };
    let total = admitted.len() + excluded.len();
    for _ in 0..30_000 {
        if query.state() != ExpressionState::Pending {
            break;
        }
        step(&mut query, options, &memory, &trace, quantum);
        if query.stats().loaded_bytes < admitted.len() as u64 {
            assert_eq!(
                query.stats().members_loaded,
                0,
                "required term and digest are still pending"
            );
        }
    }
    assert_eq!(query.state(), ExpressionState::Complete);
    let calls = trace.borrow().calls;
    step(&mut query, options, &memory, &trace, quantum);
    assert_eq!(trace.borrow().calls, calls);
    let report = query.finish().unwrap();
    assert!(report.is_complete());
    assert_eq!(report.matches_seen(), 2);
    assert_eq!(report.matching_files(), 1);
    assert_eq!(report.skipped_matches(), 1);
    assert_eq!(report.hits().len(), 1);
    let hit = report.hits()[0];
    assert_eq!(hit.ordinal(), 0);
    assert_eq!(
        (
            hit.original_range().start().get(),
            hit.original_range().end().get()
        ),
        (8, 14)
    );
    assert_eq!(report.stats().predicate_rejections, 1);
    assert_eq!(report.stats().metadata_excluded, 1);
    assert_eq!(report.stats().members_loaded, 2);
    assert_eq!(report.stats().loaded_bytes, total as u64);
    assert_eq!(
        trace.borrow().read,
        total as u64,
        "all predicates share exactly one original-byte pass"
    );
    assert_eq!(report.stats().read_calls, trace.borrow().calls);
    assert_eq!(
        report.stats().peak_member_bytes,
        4,
        "only the split surrogate pair requires four source bytes"
    );
    drop(report);
    assert_eq!(memory.accounting().reserved(), baseline);
}

#[test]
fn canceling_an_expression_integrity_drain_releases_state_without_reading_the_suffix() {
    let memory = budget(32 * 1024 * 1024);
    let mut source = vec![b'x'; 128 * 1024];
    source[..13].copy_from_slice(b"needle needle");
    let (mut archive, trace) = archive(&[entry(b"a.txt", &source)], &memory);
    let plan =
        ExpressionPlan::parse(owner(), "needle", &memory, allocation(2), allocation(100)).unwrap();
    let baseline = memory.accounting().reserved();
    let options = options(1);
    let mut query = ExpressionQuery::new(
        &mut archive,
        &plan,
        options,
        &memory,
        [allocation(3), allocation(4), allocation(5)],
    )
    .unwrap();
    let no_work = StreamReadStep {
        max_bytes: 0,
        max_calls: 1,
        max_hits: 1,
    };
    step(&mut query, options, &memory, &trace, no_work);
    assert_eq!(query.stats().metadata_examined, 0);
    assert_eq!(
        (
            trace.borrow().read,
            trace.borrow().calls,
            trace.borrow().seeks
        ),
        (0, 0, 0)
    );
    let quantum = StreamReadStep {
        max_bytes: 32,
        max_calls: 1,
        max_hits: 64,
    };
    for _ in 0..10 {
        if query.stats().loaded_bytes > 0 {
            break;
        }
        step(&mut query, options, &memory, &trace, quantum);
    }
    assert_eq!(query.stats().loaded_bytes, 32);
    assert_eq!(query.stats().members_loaded, 0);
    assert_eq!(query.state(), ExpressionState::Pending);
    let calls = trace.borrow().calls;
    assert_eq!(
        query.step(quantum, options.generation, &memory, || true),
        Err(ExpressionError::Canceled)
    );
    assert_eq!(trace.borrow().calls, calls);
    assert!(matches!(query.finish(), Err(ExpressionError::Canceled)));
    assert_eq!(memory.accounting().reserved(), baseline);
    assert_eq!(archive.load_stats().loaded_members, 0);
    reset(&trace);
    let mut replay = ExpressionQuery::new(
        &mut archive,
        &plan,
        options,
        &memory,
        [allocation(3), allocation(4), allocation(5)],
    )
    .unwrap();
    for _ in 0..10_000 {
        if replay.state() != ExpressionState::Pending {
            break;
        }
        step(&mut replay, options, &memory, &trace, quantum);
    }
    let report = replay.finish().unwrap();
    assert_eq!(report.state(), ExpressionState::Truncated);
    assert_eq!(report.matches_seen(), 2);
    assert_eq!(report.hits().len(), 1);
    assert_eq!(report.stats().members_loaded, 1);
    assert_eq!(report.stats().loaded_bytes, source.len() as u64);
    assert!(report.stats().scanned_bytes < source.len() as u64);
    assert_eq!(trace.borrow().read, source.len() as u64);
    drop(report);
    assert_eq!(memory.accounting().reserved(), baseline);
}

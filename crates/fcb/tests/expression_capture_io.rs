#![forbid(unsafe_code)]
#![cfg(feature = "snapshot")]

use std::{cell::RefCell, io::{self, Read, Seek, SeekFrom}, ops::Range, rc::Rc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{QueryGeneration, ResourceAllocationId, ResourceBudget, StreamReadStep};
use fcb::search::expression::{ExpressionPlan, ExpressionQuery, ExpressionOptions, ExpressionState};
use fcb::search::paged_snapshot::{PagedMemberData, PagedSnapshot};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};

struct Backing { bytes: Vec<u8>, allowed: Option<Range<usize>>, read_bytes: usize }
struct Guard { backing: Rc<RefCell<Backing>>, position: u64 }
impl Read for Guard {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let mut backing = self.backing.borrow_mut();
        let start = usize::try_from(self.position).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        if start >= backing.bytes.len() { return Ok(0); }
        let count = into.len().min(backing.bytes.len() - start);
        if count > 0 && let Some(allowed) = &backing.allowed {
            assert!(start >= allowed.start && start + count <= allowed.end,
                "expression read outside selected member: {start}..{} allowed={allowed:?}", start + count);
        }
        into[..count].copy_from_slice(&backing.bytes[start..start + count]);
        backing.read_bytes += count; self.position += count as u64; Ok(count)
    }
}
impl Seek for Guard {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => self.backing.borrow().bytes.len() as i128 + i128::from(n),
        };
        self.position = u64::try_from(position).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(self.position)
    }
}
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2672).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(32 * 1024 * 1024)).unwrap() }
fn options() -> ExpressionOptions {
    ExpressionOptions { generation: QueryGeneration::new(owner(), 1).unwrap(), first_file: FileId::new(owner(), 1).unwrap(),
        first_revision: SourceRevision::new(owner(), 1).unwrap(), max_matches: 10, max_scan_bytes: 1_000_000 }
}
fn source(budget: &ResourceBudget) -> (PagedSnapshot<Guard>, Rc<RefCell<Backing>>) {
    let entries = [SnapshotEntry { path: b"a.rs", observed_bytes: 15, data: SnapshotData::Captured(b"needle required") },
        SnapshotEntry { path: b"b.py", observed_bytes: 9, data: SnapshotData::Captured(b"forbidden") }];
    let encoded = SnapshotBytes::encode(owner(), true, "test-v1", &entries, SnapshotLimits::default(), budget, allocation(1), || false).unwrap();
    let backing = Rc::new(RefCell::new(Backing { bytes: encoded.bytes().to_vec(), allowed: None, read_bytes: 0 }));
    let archive = PagedSnapshot::open(Guard { backing: backing.clone(), position: 0 }, owner(), SnapshotLimits::default(),
        budget, allocation(2), || false).unwrap();
    (archive, backing)
}
fn arm(archive: &PagedSnapshot<Guard>, backing: &Rc<RefCell<Backing>>) -> usize {
    let member = archive.directory().member(0).unwrap();
    let PagedMemberData::Captured { archive_offset, byte_length, .. } = member.data else { unreachable!() };
    let start = archive_offset as usize;
    backing.borrow_mut().allowed = Some(start..start + byte_length);
    start
}
fn drain(query: &mut ExpressionQuery<'_, '_, Guard>, budget: &ResourceBudget) {
    for _ in 0..1000 {
        if query.state() != ExpressionState::Pending { return; }
        query.step(StreamReadStep { max_bytes: 3, max_calls: 2, max_hits: 1 }, options().generation, budget, || false).unwrap();
    }
    panic!("bounded expression fixture did not finish");
}

#[test]
fn all_predicates_and_primary_share_one_verified_load_and_cannot_read_excluded_payload() {
    let budget = budget(); let (mut archive, backing) = source(&budget);
    arm(&archive, &backing); let before = backing.borrow().read_bytes;
    let plan = ExpressionPlan::parse(owner(), "needle required -forbidden lang:rust", &budget, allocation(3), allocation(100)).unwrap();
    let mut query = ExpressionQuery::new(&mut archive, &plan, options(), &budget,
        [allocation(4), allocation(5), allocation(6)]).unwrap();
    drain(&mut query, &budget); let report = query.finish().unwrap();
    assert!(report.is_complete()); assert_eq!(report.matches_seen(), 1);
    assert_eq!(report.stats().predicate_scans, 2); assert_eq!(report.stats().primary_scans, 1);
    assert_eq!(backing.borrow().read_bytes - before, 15, "three scans must not mean three archive loads");
    assert_eq!(report.stats().members_loaded, 1); assert_eq!(report.stats().metadata_excluded, 1);
}

#[test]
fn backing_mutation_between_predicates_does_not_change_the_retained_source_but_blocks_reopen() {
    let budget = budget(); let (mut archive, backing) = source(&budget);
    let start = arm(&archive, &backing);
    let plan = ExpressionPlan::parse(owner(), "needle required lang:rust", &budget, allocation(3), allocation(100)).unwrap();
    let mut query = ExpressionQuery::new(&mut archive, &plan, options(), &budget,
        [allocation(4), allocation(5), allocation(6)]).unwrap();
    for _ in 0..10 {
        if query.stats().members_loaded == 1 { break; }
        query.step(StreamReadStep::default(), options().generation, &budget, || false).unwrap();
    }
    assert_eq!(query.stats().members_loaded, 1);
    backing.borrow_mut().bytes[start..start + 6].copy_from_slice(b"xxxxxx");
    drain(&mut query, &budget); let report = query.finish().unwrap();
    assert!(report.is_complete()); assert_eq!(report.matches_seen(), 1, "the old verified capture still contains needle");
    let hit = report.hits()[0];
    assert!(hit.open(&mut archive, options().generation, &budget, [allocation(7), allocation(8)], || false).is_err(),
        "changed bytes must never open beneath an old hit's digest/revision");
}

#[test]
fn mutation_before_member_verification_never_yields_a_publishable_negative() {
    let budget = budget(); let (mut archive, backing) = source(&budget);
    let start = arm(&archive, &backing); backing.borrow_mut().bytes[start] ^= 1;
    let plan = ExpressionPlan::parse(owner(), "absent required lang:rust", &budget, allocation(3), allocation(100)).unwrap();
    let baseline = budget.accounting().reserved().get();
    let mut query = ExpressionQuery::new(&mut archive, &plan, options(), &budget,
        [allocation(4), allocation(5), allocation(6)]).unwrap();
    let mut failed = false;
    for _ in 0..10 {
        if query.step(StreamReadStep::default(), options().generation, &budget, || false).is_err() { failed = true; break; }
    }
    assert!(failed); assert!(matches!(query.state(), ExpressionState::Failed(_)));
    assert!(query.finish().is_err());
    assert_eq!(budget.accounting().reserved().get(), baseline);
}

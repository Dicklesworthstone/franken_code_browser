#![forbid(unsafe_code)]

use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget};
use fcb_store::paged_snapshot::{
    MemberReadState, PagedMemberData, PagedSnapshot, PagedSnapshotError, SNAPSHOT_READ_BYTES,
};
use fcb_store::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use std::{
    cell::RefCell,
    io::{self, Read, Seek, SeekFrom},
    rc::Rc,
};

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(2851).unwrap()
}
fn allocation(n: u64) -> ResourceAllocationId {
    ResourceAllocationId::new(n).unwrap()
}
fn budget(bytes: u64) -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(bytes)).unwrap()
}

#[derive(Default)]
struct Trace {
    bytes: Vec<u8>,
    calls: u64,
    read: u64,
    seeks: u64,
    max_offer: usize,
    short: usize,
    interrupts: usize,
    invalid_count: bool,
}
struct Input {
    trace: Rc<RefCell<Trace>>,
    position: usize,
}
impl Read for Input {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let mut trace = self.trace.borrow_mut();
        trace.calls += 1;
        trace.max_offer = trace.max_offer.max(into.len());
        if trace.interrupts > 0 {
            trace.interrupts -= 1;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if trace.invalid_count {
            return Ok(into.len() + 1);
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
fn archive(source: &[u8]) -> (PagedSnapshot<Input>, Rc<RefCell<Trace>>, ResourceBudget) {
    let encoding = budget(128 * 1024 * 1024);
    let limits = SnapshotLimits {
        max_file_bytes: source.len().max(1),
        ..SnapshotLimits::default()
    };
    let entries = [SnapshotEntry {
        path: b"member",
        observed_bytes: source.len() as u64,
        data: SnapshotData::Captured(source),
    }];
    let bytes = SnapshotBytes::encode(
        owner(),
        true,
        "member-stream-test-v1",
        &entries,
        limits,
        &encoding,
        allocation(1),
        || false,
    )
    .unwrap()
    .bytes()
    .to_vec();
    let trace = Rc::new(RefCell::new(Trace {
        bytes,
        short: usize::MAX,
        ..Trace::default()
    }));
    let memory = budget(128 * 1024);
    let archive = PagedSnapshot::open(
        Input {
            trace: trace.clone(),
            position: 0,
        },
        owner(),
        limits,
        &memory,
        allocation(1),
        || false,
    )
    .unwrap();
    {
        let mut trace = trace.borrow_mut();
        trace.calls = 0;
        trace.read = 0;
        trace.seeks = 0;
        trace.max_offer = 0;
    }
    (archive, trace, memory)
}

#[test]
fn complete_member_stream_fits_when_whole_member_allocation_is_denied() {
    let source = vec![b'x'; 2 * 1024 * 1024 + 19];
    let (mut archive, trace, memory) = archive(&source);
    let baseline = memory.accounting().reserved();
    assert!(
        matches!(
            archive.load(0, &memory, allocation(2), || false),
            Err(PagedSnapshotError::ResourceDenied)
        ),
        "negative control: whole-member allocation must not fit"
    );
    assert_eq!(trace.borrow().calls, 0);
    let archive_digest = archive.directory().digest();
    let expected_digest = match archive.directory().member(0).unwrap().data {
        PagedMemberData::Captured { digest, .. } => digest,
        _ => unreachable!(),
    };
    let mut reader = archive.read_session().member(0).unwrap();
    assert_eq!(reader.state(), MemberReadState::Pending);
    assert_eq!(
        trace.borrow().seeks,
        0,
        "constructing a stream performs no I/O"
    );
    let mut buffer = [0; 2 * SNAPSHOT_READ_BYTES];
    let mut position = 0;
    while reader.state() == MemberReadState::Pending {
        let n = reader.read(&mut buffer).unwrap();
        assert!(n > 0 && n <= SNAPSHOT_READ_BYTES);
        assert_eq!(&buffer[..n], &source[position..position + n]);
        position += n;
        assert_eq!(reader.remaining_bytes(), source.len() - position);
        assert_eq!(
            memory.accounting().reserved(),
            baseline,
            "stream owns no payload allocation"
        );
    }
    assert_eq!(reader.stats().bytes_read, source.len() as u64);
    assert_eq!(
        reader.stats().read_calls,
        source.len().div_ceil(SNAPSHOT_READ_BYTES) as u64
    );
    let calls = trace.borrow().calls;
    assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    assert_eq!(
        trace.borrow().calls,
        calls,
        "verified terminal read must not touch backing input"
    );
    let (session, identity) = reader.finish().unwrap();
    assert_eq!(identity.ordinal(), 0);
    assert_eq!(identity.byte_length(), source.len());
    assert_eq!(identity.archive_digest(), archive_digest);
    assert_eq!(identity.source_digest(), expected_digest);
    assert_eq!(session.load_stats().loaded_members, 1);
    assert_eq!(trace.borrow().max_offer, SNAPSHOT_READ_BYTES);
    drop(session);
    drop(archive);
    assert_eq!(memory.accounting().reserved().get(), 0);
}

#[test]
fn empty_members_verify_without_reading_and_pending_members_cannot_make_proof() {
    let (mut empty, trace, _) = archive(b"");
    let reader = empty.read_session().member(0).unwrap();
    assert_eq!(reader.state(), MemberReadState::Verified);
    assert_eq!(reader.stats().bytes_read, 0);
    let (_, identity) = reader.finish().unwrap();
    assert_eq!(identity.byte_length(), 0);
    assert_eq!(trace.borrow().calls, 0);
    assert_eq!(trace.borrow().seeks, 0);

    let (mut nonempty, trace, _) = archive(b"banana");
    assert!(matches!(
        nonempty.read_session().member(0).unwrap().finish(),
        Err(PagedSnapshotError::Pending)
    ));
    assert_eq!(trace.borrow().calls, 0);
    let mut again = nonempty.read_session().member(0).unwrap();
    assert_eq!(again.read(&mut [0; 6]).unwrap(), 6);
    assert!(
        again.finish().is_ok(),
        "dropping a pending attempt leaves the original handle reusable"
    );
}

#[test]
fn short_and_interrupted_reads_spend_actual_calls_and_cancellation_never_drains() {
    let (mut archive, trace, memory) = archive(b"banana");
    {
        let mut trace = trace.borrow_mut();
        trace.short = 2;
        trace.interrupts = 2;
    }
    let baseline = memory.accounting().reserved();
    let mut reader = archive.read_session().member(0).unwrap();
    for expected in 1..=2 {
        assert_eq!(
            reader.read(&mut [0; 10]).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(reader.stats().read_calls, expected);
        assert_eq!(reader.state(), MemberReadState::Pending);
    }
    assert_eq!(reader.read(&mut [0; 10]).unwrap(), 2);
    assert_eq!(reader.stats().bytes_read, 2);
    assert_eq!(reader.stats().read_calls, 3);
    reader.cancel();
    let calls = trace.borrow().calls;
    assert!(reader.read(&mut [0; 10]).is_err());
    assert_eq!(trace.borrow().calls, calls);
    assert!(matches!(reader.finish(), Err(PagedSnapshotError::Canceled)));
    assert_eq!(memory.accounting().reserved(), baseline);
    assert_eq!(
        archive
            .load(0, &memory, allocation(2), || false)
            .unwrap()
            .bytes(),
        b"banana"
    );
}

#[test]
fn changed_unread_suffix_short_eof_and_invalid_read_count_never_verify() {
    for fault in 0..3 {
        let (mut archive, trace, _) = archive(b"banana");
        let offset = match archive.directory().member(0).unwrap().data {
            PagedMemberData::Captured { archive_offset, .. } => archive_offset as usize,
            _ => unreachable!(),
        };
        let mut reader = archive.read_session().member(0).unwrap();
        assert_eq!(reader.read(&mut [0; 2]).unwrap(), 2);
        let expected = match fault {
            0 => {
                trace.borrow_mut().bytes[offset + 5] = b'!';
                PagedSnapshotError::Changed
            }
            1 => {
                trace.borrow_mut().bytes.truncate(offset + 2);
                PagedSnapshotError::Format
            }
            _ => {
                trace.borrow_mut().invalid_count = true;
                PagedSnapshotError::InvalidReadCount
            }
        };
        assert!(reader.read(&mut [0; 10]).is_err());
        assert_eq!(reader.state(), MemberReadState::Failed(expected));
        let calls = trace.borrow().calls;
        assert!(reader.read(&mut [0; 10]).is_err());
        assert_eq!(trace.borrow().calls, calls);
        assert!(matches!(reader.finish(), Err(error) if error == expected));
        assert_eq!(archive.load_stats().loaded_members, 0);
    }
}

use super::*;
use std::{cell::Cell, io::Cursor, rc::Rc};
use crate::paged_snapshot::{PagedSnapshot, PagedSnapshotError, PinnedCatalog};
use crate::snapshot::{SnapshotBytes, SnapshotView, MAX_PAGED_SNAPSHOT_BYTES,
    MAX_PAGED_SNAPSHOT_FILE_BYTES, MAX_PAGED_SNAPSHOT_SOURCE_BYTES, MAX_SNAPSHOT_BYTES};

const POLICY: &str = "test-v1";
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1861).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget(bytes: u64) -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(bytes)).unwrap() }
fn captured<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}

#[derive(Clone, Copy, Debug, Default)]
enum Fault { #[default] None, Read, Write, Seek, Flush, ReadEof, ZeroWrite,
    TooManyRead, TooManyWrite, InterruptWrites }

#[derive(Debug)]
struct TestIo {
    inner: Cursor<Vec<u8>>,
    chunk: usize,
    interrupt_every: u64,
    calls: u64,
    read_attempts: Rc<Cell<u64>>,
    max_read: usize,
    max_write: usize,
    fault: Fault,
    fail_write_at: Option<u64>,
    completed: Option<(u64, Rc<Cell<bool>>)>,
}
impl Default for TestIo {
    fn default() -> Self {
        Self { inner: Cursor::new(Vec::new()), chunk: usize::MAX, interrupt_every: 0,
            calls: 0, read_attempts: Rc::new(Cell::new(0)), max_read: 0, max_write: 0,
            fault: Fault::None, fail_write_at: None, completed: None }
    }
}
impl TestIo {
    fn tick(&mut self) -> io::Result<()> {
        self.calls += 1;
        if self.interrupt_every > 0 && self.calls % self.interrupt_every == 0 {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(())
    }
}
impl Read for TestIo {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.max_read = self.max_read.max(bytes.len());
        self.read_attempts.set(self.read_attempts.get() + 1);
        self.tick()?;
        match self.fault {
            Fault::Read => return Err(io::ErrorKind::Other.into()),
            Fault::ReadEof => return Ok(0),
            Fault::TooManyRead => return Ok(bytes.len() + 1),
            _ => {}
        }
        let length = bytes.len().min(self.chunk);
        self.inner.read(&mut bytes[..length])
    }
}
impl Write for TestIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.max_write = self.max_write.max(bytes.len());
        self.tick()?;
        match self.fault {
            Fault::Write => return Err(io::ErrorKind::Other.into()),
            Fault::ZeroWrite => return Ok(0),
            Fault::TooManyWrite => return Ok(bytes.len() + 1),
            Fault::InterruptWrites => return Err(io::ErrorKind::Interrupted.into()),
            _ => {}
        }
        let mut length = bytes.len().min(self.chunk);
        if let Some(end) = self.fail_write_at {
            let remaining = end.saturating_sub(self.inner.position());
            if remaining == 0 { return Err(io::ErrorKind::Other.into()); }
            length = length.min(remaining as usize);
        }
        let count = self.inner.write(&bytes[..length])?;
        if let Some((end, done)) = &self.completed {
            if self.inner.position() == *end && self.inner.get_ref().len() as u64 == *end { done.set(true); }
        }
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.tick()?;
        if matches!(self.fault, Fault::Flush) { return Err(io::ErrorKind::Other.into()); }
        self.inner.flush()
    }
}
impl Seek for TestIo {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.tick()?;
        if matches!(self.fault, Fault::Seek) { return Err(io::ErrorKind::Other.into()); }
        self.inner.seek(position)
    }
}

fn new_writer<'a>(output: &'a mut TestIo, files: usize, limits: SnapshotLimits, b: &ResourceBudget)
    -> SnapshotStreamWriter<&'a mut TestIo> {
    SnapshotStreamWriter::new(output, owner(), true, POLICY, files, limits, b, id(1), || false).unwrap()
}

#[test]
fn streamed_envelope_is_byte_identical_and_preserves_missing_empty_and_raw_members() {
    let entries = [captured(b"a\\\xff", b"a\xffz"), captured(b"empty", b""),
        SnapshotEntry { path: b"missing", observed_bytes: u64::MAX,
            data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }, captured(b"\xff", b"\xfe\xff")];
    for complete in [false, true] {
        let b = budget(1024 * 1024);
        let expected = SnapshotBytes::encode(owner(), complete, POLICY, &entries,
            SnapshotLimits::default(), &b, id(2), || false).unwrap();
        let mut sink = TestIo::default();
        let mut writer = SnapshotStreamWriter::new(&mut sink, owner(), complete, POLICY,
            entries.len(), SnapshotLimits::paged(), &b, id(1), || false).unwrap();
        for entry in entries { writer.append(entry, || false).unwrap(); }
        let (_, summary) = writer.finish(|| false).unwrap();
        assert_eq!(sink.inner.get_ref(), expected.bytes());
        assert_eq!(summary.digest, expected.digest());
        assert_eq!(summary.discovery_complete, complete);
        assert_eq!(summary.files, 4);
        assert_eq!(summary.captured_files, 3);
        assert_eq!(summary.source_bytes, 5);
        assert_eq!(summary.archive_bytes, expected.bytes().len() as u64);
        assert_eq!(summary.stats.bytes_read, summary.archive_bytes - CHECKSUM_LEN as u64);
        assert_eq!(summary.stats.bytes_written, summary.archive_bytes + 8);
        let view = SnapshotView::open(sink.inner.get_ref(), SnapshotLimits::default(), || false).unwrap();
        assert_eq!(view.entries().collect::<Result<Vec<_>, _>>().unwrap(), entries);
        let mut paged = PagedSnapshot::open(Cursor::new(sink.inner.get_ref()), owner(),
            SnapshotLimits::paged(), &b, id(3), || false).unwrap();
        assert_eq!(paged.directory().digest(), summary.digest);
        assert_eq!(paged.load(0, &b, id(4), || false).unwrap().bytes(), b"a\xffz");
        assert!(matches!(paged.load(2, &b, id(4), || false), Err(PagedSnapshotError::Missing)));
    }
}

#[test]
fn short_and_interrupted_io_keeps_canonical_bytes_and_bounded_calls() {
    let source = vec![b'x'; SNAPSHOT_STREAM_BYTES + 17];
    let entries = [captured(b"a", &source)];
    let b = budget(1024 * 1024);
    let expected = SnapshotBytes::encode(owner(), true, POLICY, &entries,
        SnapshotLimits::default(), &b, id(2), || false).unwrap();
    let mut sink = TestIo { chunk: 13, interrupt_every: 3, ..TestIo::default() };
    let mut writer = new_writer(&mut sink, 1, SnapshotLimits::default(), &b);
    writer.append(entries[0], || false).unwrap();
    let (_, summary) = writer.finish(|| false).unwrap();
    assert_eq!(sink.inner.get_ref(), expected.bytes());
    assert!(sink.max_read <= SNAPSHOT_STREAM_BYTES);
    assert!(sink.max_write <= SNAPSHOT_STREAM_BYTES);
    assert_eq!(summary.stats.total_calls(), sink.calls);
    assert!(summary.stats.total_calls() < MAX_SNAPSHOT_STREAM_CALLS);
}

#[test]
fn empty_incomplete_archive_is_canonical_without_inventing_members() {
    let b = budget(96 * 1024);
    let mut sink = TestIo::default();
    let writer = SnapshotStreamWriter::new(&mut sink, owner(), false, POLICY, 0,
        SnapshotLimits::paged(), &b, id(1), || false).unwrap();
    let (_, summary) = writer.finish(|| false).unwrap();
    assert_eq!((summary.files, summary.captured_files, summary.source_bytes), (0, 0, 0));
    assert!(!summary.discovery_complete);
    let expected = SnapshotBytes::encode(owner(), false, POLICY, &[],
        SnapshotLimits::default(), &b, id(2), || false).unwrap();
    assert_eq!(sink.inner.get_ref(), expected.bytes());
}

#[test]
fn paged_profile_exports_larger_members_with_constant_writer_reservation_and_pinned_reopen() {
    assert_eq!(SnapshotLimits::default().max_document_bytes, MAX_SNAPSHOT_BYTES);
    assert_eq!(SnapshotLimits::paged().max_file_bytes, MAX_PAGED_SNAPSHOT_FILE_BYTES);
    assert_eq!(SnapshotLimits::paged().max_source_bytes, MAX_PAGED_SNAPSHOT_SOURCE_BYTES);
    assert_eq!(SnapshotLimits::paged().max_document_bytes, MAX_PAGED_SNAPSHOT_BYTES);
    let mut charges = Vec::new();
    for length in [1, 2 * 1024 * 1024] {
        let source = vec![b'x'; length];
        let b = budget(96 * 1024);
        let mut sink = TestIo::default();
        let mut writer = new_writer(&mut sink, 1, SnapshotLimits::paged(), &b);
        charges.push(b.accounting().reserved().get());
        assert_eq!(writer.retained_charge() as u64, b.accounting().reserved().get());
        writer.append(captured(b"large.rs", &source), || false).unwrap();
        let (_, summary) = writer.finish(|| false).unwrap();
        assert_eq!(b.accounting().reserved().get(), 0);
        assert_eq!(summary.source_bytes, length as u64);
        let pages = budget(4 * 1024 * 1024);
        let paged = PagedSnapshot::open(Cursor::new(sink.inner.get_ref()), owner(),
            SnapshotLimits::paged(), &pages, id(2), || false).unwrap();
        let catalog = paged.directory().encode_catalog(&pages, id(3), || false).unwrap();
        let pin = PinnedCatalog::decode_pinned(catalog.bytes(), catalog.digest(), owner(),
            SnapshotLimits::paged(), &pages, id(4), || false).unwrap();
        let mut reopened = PagedSnapshot::open_pinned(Cursor::new(sink.inner.get_ref()), pin, || false).unwrap();
        assert_eq!(reopened.load(0, &pages, id(5), || false).unwrap().bytes(), source);
        if length > SnapshotLimits::default().max_file_bytes {
            assert!(matches!(SnapshotView::open(sink.inner.get_ref(), SnapshotLimits::default(), || false),
                Err(SnapshotError::Limit)));
            assert!(PagedSnapshot::open(Cursor::new(sink.inner.get_ref()), owner(),
                SnapshotLimits::default(), &pages, id(6), || false).is_err());
        }
    }
    assert_eq!(charges[0], charges[1]);
    let b = budget(96 * 1024);
    assert!(matches!(SnapshotBytes::encode(owner(), true, POLICY, &[], SnapshotLimits::paged(),
        &b, id(1), || false), Err(SnapshotError::Limit)));
}

#[test]
fn invalid_configuration_denied_budget_and_preexisting_output_are_untouched() {
    let b = budget(96 * 1024);
    let mut sink = TestIo::default();
    assert!(matches!(SnapshotStreamWriter::new(&mut sink, owner(), true, "bad policy", 0,
        SnapshotLimits::paged(), &b, id(1), || false),
        Err(SnapshotStreamError::Format(SnapshotError::InvalidPolicy))));
    let excessive = SnapshotLimits { max_document_bytes: MAX_PAGED_SNAPSHOT_BYTES + 1, ..SnapshotLimits::paged() };
    assert!(matches!(SnapshotStreamWriter::new(&mut sink, owner(), true, POLICY, 0,
        excessive, &b, id(1), || false), Err(SnapshotStreamError::Format(SnapshotError::Limit))));
    assert!(matches!(SnapshotStreamWriter::new(&mut sink, owner(), true, POLICY, usize::MAX,
        SnapshotLimits::paged(), &b, id(1), || false), Err(SnapshotStreamError::Format(SnapshotError::Limit))));
    let tiny = budget(1);
    assert!(matches!(SnapshotStreamWriter::new(&mut sink, owner(), true, POLICY, 1,
        SnapshotLimits::paged(), &tiny, id(1), || false),
        Err(SnapshotStreamError::Format(SnapshotError::ResourceDenied))));
    assert!(matches!(SnapshotStreamWriter::new(&mut sink, owner(), true, POLICY, 1,
        SnapshotLimits::paged(), &b, id(1), || true), Err(SnapshotStreamError::Canceled)));
    assert_eq!(sink.calls, 0);
    assert!(sink.inner.get_ref().is_empty());
    assert_eq!(tiny.accounting().reserved().get(), 0);
    sink.inner = Cursor::new(b"existing destination".to_vec());
    assert!(matches!(SnapshotStreamWriter::new(&mut sink, owner(), true, POLICY, 0,
        SnapshotLimits::paged(), &b, id(1), || false), Err(SnapshotStreamError::OutputNotEmpty)));
    assert_eq!(sink.inner.get_ref(), b"existing destination");
    assert_eq!(b.accounting().reserved().get(), 0);
}

#[test]
fn refused_records_do_not_write_or_poison_and_require_exact_count() {
    let b = budget(96 * 1024);
    let mut sink = TestIo::default();
    let mut writer = new_writer(&mut sink, 2, SnapshotLimits::default(), &b);
    let before = writer.stats();
    for path in [b"".as_slice(), b"/abs", b"../a", b"a//b", b"a\0z", b"."] {
        assert_eq!(writer.append(captured(path, b"x"), || false),
            Err(SnapshotStreamError::Format(SnapshotError::InvalidPath)));
    }
    assert_eq!(writer.append(SnapshotEntry { path: b"a", observed_bytes: 2,
        data: SnapshotData::Captured(b"x") }, || false),
        Err(SnapshotStreamError::Format(SnapshotError::InvalidMember)));
    assert_eq!(writer.append(SnapshotEntry { path: b"a", observed_bytes: 0,
        data: SnapshotData::Unavailable("not_a_code") }, || false),
        Err(SnapshotStreamError::Format(SnapshotError::InvalidMember)));
    assert_eq!(writer.stats(), before);
    assert!(!writer.is_poisoned());
    writer.append(captured(b"b", b"one"), || false).unwrap();
    let before = writer.stats();
    for path in [b"a".as_slice(), b"b"] {
        assert_eq!(writer.append(captured(path, b"two"), || false),
            Err(SnapshotStreamError::Format(SnapshotError::Order)));
    }
    assert_eq!(writer.stats(), before);
    writer.append(captured(b"c", b"two"), || false).unwrap();
    let before = writer.stats();
    assert_eq!(writer.append(captured(b"d", b""), || false), Err(SnapshotStreamError::MemberCount));
    assert_eq!(writer.stats(), before);
    writer.finish(|| false).unwrap();
    let view = SnapshotView::open(sink.inner.get_ref(), SnapshotLimits::default(), || false).unwrap();
    assert_eq!(view.len(), 2);
    let mut incomplete = TestIo::default();
    let writer = new_writer(&mut incomplete, 1, SnapshotLimits::default(), &b);
    assert_eq!(writer.finish(|| false).err(), Some(SnapshotStreamError::MemberCount));
    assert!(SnapshotView::open(incomplete.inner.get_ref(), SnapshotLimits::default(), || false).is_err());
    assert_eq!(b.accounting().reserved().get(), 0);
}

#[test]
fn member_path_source_and_document_limits_are_checked_before_output() {
    let configurations = [
        (SnapshotLimits { max_path_bytes: 1, ..SnapshotLimits::default() }, captured(b"ab", b"x")),
        (SnapshotLimits { max_total_path_bytes: 1, ..SnapshotLimits::default() }, captured(b"ab", b"x")),
        (SnapshotLimits { max_file_bytes: 1, ..SnapshotLimits::default() }, captured(b"a", b"xx")),
        (SnapshotLimits { max_source_bytes: 1, ..SnapshotLimits::default() }, captured(b"a", b"xx")),
        (SnapshotLimits { max_document_bytes: FRAME_LEN + 18 + POLICY.len() + 26 + 1,
            ..SnapshotLimits::default() }, captured(b"a", b"xx")),
    ];
    for (limits, entry) in configurations {
        let b = budget(96 * 1024);
        let mut sink = TestIo::default();
        let mut writer = new_writer(&mut sink, 1, limits, &b);
        let before = writer.stats();
        assert!(writer.append(entry, || false).is_err());
        assert_eq!(writer.stats(), before);
        writer.append(captured(b"a", b"x"), || false).unwrap();
        writer.finish(|| false).unwrap();
        assert!(SnapshotView::open(sink.inner.get_ref(), limits, || false).is_ok());
    }
    let b = budget(96 * 1024);
    let mut sink = TestIo::default();
    let limits = SnapshotLimits { max_total_path_bytes: 2, max_source_bytes: 2, ..SnapshotLimits::default() };
    let mut writer = new_writer(&mut sink, 2, limits, &b);
    writer.append(captured(b"a", b"xx"), || false).unwrap();
    let before = writer.stats();
    assert!(writer.append(captured(b"bb", b""), || false).is_err());
    assert!(writer.append(captured(b"b", b"x"), || false).is_err());
    assert_eq!(writer.stats(), before);
    writer.append(captured(b"b", b""), || false).unwrap();
    let (_, summary) = writer.finish(|| false).unwrap();
    assert_eq!(summary.source_bytes, 2);
    assert_eq!(summary.captured_files, 2);
}

#[test]
fn partial_io_failure_or_cancellation_permanently_poison_the_writer() {
    for cancel in [false, true] {
        let b = budget(96 * 1024);
        let mut sink = TestIo::default();
        let mut writer = new_writer(&mut sink, 1, SnapshotLimits::default(), &b);
        let initial = writer.output.inner.get_ref().len();
        if !cancel { writer.output.fail_write_at = Some(initial as u64 + 12); }
        let mut checks = 0;
        let error = writer.append(captured(b"source", b"some exact bytes"), || {
            checks += 1;
            cancel && checks == 3
        }).unwrap_err();
        assert_eq!(error.is_canceled(), cancel);
        assert!(writer.output.inner.get_ref().len() > initial);
        assert!(writer.is_poisoned());
        let before = writer.stats();
        assert_eq!(writer.append(captured(b"replacement", b""), || false), Err(SnapshotStreamError::Poisoned));
        assert_eq!(writer.stats(), before);
        assert_eq!(writer.finish(|| false).err(), Some(SnapshotStreamError::Poisoned));
        assert!(SnapshotView::open(sink.inner.get_ref(), SnapshotLimits::default(), || false).is_err());
        assert_eq!(b.accounting().reserved().get(), 0);
    }
}

#[test]
fn broken_io_cannot_publish_or_escape_call_and_slice_bounds() {
    let b = budget(96 * 1024);
    for (fault, expected) in [(Fault::Write, SnapshotStreamError::Write),
        (Fault::ZeroWrite, SnapshotStreamError::Write),
        (Fault::TooManyWrite, SnapshotStreamError::InvalidWriteCount)] {
        let mut sink = TestIo::default();
        let mut writer = new_writer(&mut sink, 1, SnapshotLimits::default(), &b);
        writer.output.fault = fault;
        assert_eq!(writer.append(captured(b"a", b"x"), || false), Err(expected));
        assert!(writer.is_poisoned());
    }
    for (fault, expected) in [(Fault::Read, SnapshotStreamError::Read),
        (Fault::ReadEof, SnapshotStreamError::Changed),
        (Fault::TooManyRead, SnapshotStreamError::InvalidReadCount),
        (Fault::Seek, SnapshotStreamError::Seek), (Fault::Flush, SnapshotStreamError::Flush)] {
        let mut sink = TestIo::default();
        let mut writer = new_writer(&mut sink, 1, SnapshotLimits::default(), &b);
        writer.append(captured(b"a", b"x"), || false).unwrap();
        writer.output.fault = fault;
        assert_eq!(writer.finish(|| false).err(), Some(expected));
        assert!(SnapshotView::open(sink.inner.get_ref(), SnapshotLimits::default(), || false).is_err());
        assert_eq!(b.accounting().reserved().get(), 0);
    }
    let mut sink = TestIo { fault: Fault::InterruptWrites, ..TestIo::default() };
    assert!(matches!(SnapshotStreamWriter::new(&mut sink, owner(), true, POLICY, 0,
        SnapshotLimits::default(), &b, id(1), || false), Err(SnapshotStreamError::IoCallLimit)));
    assert_eq!(sink.calls, MAX_SNAPSHOT_STREAM_CALLS);
    assert!(sink.inner.get_ref().is_empty());
    assert_eq!(b.accounting().reserved().get(), 0);
}

#[test]
fn destination_mutation_cannot_be_certified_as_the_accepted_capture() {
    for mutation in 0..4 {
        let b = budget(96 * 1024);
        let mut sink = TestIo::default();
        let mut writer = new_writer(&mut sink, 1, SnapshotLimits::default(), &b);
        writer.append(captured(b"source", b"original captured bytes"), || false).unwrap();
        let bytes = writer.output.inner.get_mut();
        match mutation {
            0 => bytes[0] ^= 1,
            1 => { let last = bytes.len() - 1; bytes[last] ^= 1; }
            2 => { bytes.pop(); }
            _ => bytes.push(0),
        }
        assert_eq!(writer.finish(|| false).err(), Some(SnapshotStreamError::Changed));
        assert!(SnapshotView::open(sink.inner.get_ref(), SnapshotLimits::default(), || false).is_err());
        assert_eq!(b.accounting().reserved().get(), 0);
    }
}

#[test]
fn cancellation_during_final_hash_does_not_publish_checksum_but_completed_writes_stay_completed() {
    let b = budget(96 * 1024);
    let mut sink = TestIo::default();
    let reads = Rc::clone(&sink.read_attempts);
    let mut writer = new_writer(&mut sink, 1, SnapshotLimits::default(), &b);
    writer.append(captured(b"a", &vec![b'x'; SNAPSHOT_STREAM_BYTES + 1]), || false).unwrap();
    assert_eq!(writer.finish(|| reads.get() > 0).err(), Some(SnapshotStreamError::Canceled));
    assert!(SnapshotView::open(sink.inner.get_ref(), SnapshotLimits::default(), || false).is_err());
    assert_eq!(b.accounting().reserved().get(), 0);

    let mut sink = TestIo::default();
    let complete = Rc::new(Cell::new(false));
    let mut writer = new_writer(&mut sink, 1, SnapshotLimits::default(), &b);
    writer.append(captured(b"a", b"x"), || false).unwrap();
    let end = writer.output.inner.get_ref().len() as u64 + CHECKSUM_LEN as u64;
    writer.output.completed = Some((end, Rc::clone(&complete)));
    let (_, summary) = writer.finish(|| complete.get()).unwrap();
    assert!(complete.get());
    assert_eq!(summary.archive_bytes, end);
    assert!(SnapshotView::open(sink.inner.get_ref(), SnapshotLimits::default(), || false).is_ok());
    assert_eq!(b.accounting().reserved().get(), 0);
}

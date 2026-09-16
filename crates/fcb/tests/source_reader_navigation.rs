#![forbid(unsafe_code)]
#![cfg(feature = "search")]

use fcb::{ArenaOwnerId, BrowserSession, ByteLength, ByteOffset, ByteRange, FileId, SourceCapture, SourceRevision};
use fcb::search::{QueryGeneration, ReaderError, ReaderLimits, ReadingAnchor, ReadingSeek,
    ReadingSeekState, ReadingTarget, ResourceAllocationId, ResourceBudget, SourceReader};
use fcb::search::reader::{LineNumber, MAX_READER_STEP_BYTES};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(619).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn source(bytes: &[u8]) -> SourceCapture {
    SourceCapture::from_bytes(owner(), FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 1).unwrap(), "source.rs", bytes.to_vec()).unwrap()
}
fn line(number: u64) -> ReadingTarget { ReadingTarget::Line(LineNumber::new(number).unwrap()) }
fn finish(seek: &mut ReadingSeek<'_>, quantum: usize) -> ReadingSeekState {
    for _ in 0..1_000_000 {
        if seek.state() != ReadingSeekState::Pending { return seek.state(); }
        seek.step(quantum, seek.generation(), || false).unwrap();
        assert!(seek.last_step_bytes() <= quantum.min(MAX_READER_STEP_BYTES));
    }
    panic!("seek failed to terminate");
}
fn ready(state: ReadingSeekState) -> ReadingAnchor {
    match state { ReadingSeekState::Ready(anchor) => anchor, other => panic!("expected ready, got {other:?}") }
}

#[test]
fn construction_and_first_line_do_not_scan_hash_copy_or_fetch_source() {
    let source = source(&vec![b'x'; 200_000]);
    let session = BrowserSession::new(owner());
    let view = session.open_capture(source).unwrap();
    let budget = budget();
    let reader = view.source_reader(ReaderLimits::default(), &budget, allocation(1)).unwrap();
    assert_eq!(reader.source().bytes().as_ptr(), view.source().bytes().as_ptr());
    assert_eq!(reader.progress().indexed_through.get(), 0);
    assert_eq!(reader.progress().total_lines, None);
    let seek = reader.seek(line(1), generation(1)).unwrap();
    assert_eq!(ready(seek.state()).offset().get(), 0);
    assert_eq!(seek.scanned_bytes(), 0);
    assert_eq!(reader.checkpoint_count(), 1);
}

#[test]
fn far_line_is_genuinely_resumable_and_zero_budget_never_loops() {
    let source = source(&b"row\n".repeat(10_000));
    let budget = budget();
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let mut seek = reader.seek(line(9000), generation(1)).unwrap();
    assert_eq!(seek.step(0, generation(1), || false).unwrap(), ReadingSeekState::Pending);
    assert_eq!(seek.scanned_bytes(), 0);
    assert_eq!(seek.step(16, generation(1), || false).unwrap(), ReadingSeekState::Pending);
    assert_eq!(seek.scanned_bytes(), 16);
    let anchor = ready(finish(&mut seek, 64));
    assert_eq!(anchor.line_number(), 9000);
    assert_eq!(anchor.line_start().get(), 8999 * 4);
    assert_eq!(anchor.offset(), anchor.line_start());
    assert!(seek.scanned_bytes() < source.bytes().len() as u64);
}

#[test]
fn checkpoint_storage_is_capped_independently_of_line_count() {
    let source = source(&b"x\n".repeat(300_000));
    let budget = budget();
    let mut reader = SourceReader::new(&source, ReaderLimits { max_checkpoints: 4, min_checkpoint_bytes: 1 },
        &budget, allocation(1)).unwrap();
    let charge = budget.accounting().reserved().get();
    loop {
        let progress = reader.index_step(usize::MAX, || false).unwrap();
        assert!(progress.step_bytes <= MAX_READER_STEP_BYTES);
        assert!(progress.checkpoint_count <= 4);
        assert_eq!(budget.accounting().reserved().get(), charge);
        if let Some(total) = progress.total_lines { assert_eq!(total, 300_001); break; }
    }
    let mut seek = reader.seek(line(250_000), generation(1)).unwrap();
    assert!(seek.scanned_through().get() > 0, "seek must reuse a sparse checkpoint");
    let anchor = ready(finish(&mut seek, 256));
    assert_eq!(anchor.offset().get(), 499_998);
    assert!(seek.scanned_bytes() <= reader.checkpoint_stride_bytes() as u64 + 4);
    drop(reader);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn empty_and_final_empty_rows_have_explicit_editor_semantics() {
    for (bytes, starts) in [(b"".as_slice(), vec![0]), (b"\n", vec![0, 1]),
        (b"a\r\nb\r\n", vec![0, 3, 6]), (b"a\rb\nlast", vec![0, 2, 4])] {
        let source = source(bytes);
        let budget = budget();
        let mut reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
        while reader.progress().total_lines.is_none() { reader.index_step(4, || false).unwrap(); }
        assert_eq!(reader.progress().total_lines, Some(starts.len() as u64));
        for (index, &offset) in starts.iter().enumerate() {
            let mut seek = reader.seek(line(index as u64 + 1), generation(1)).unwrap();
            assert_eq!(ready(finish(&mut seek, 4)).offset().get(), offset);
        }
        let mut missing = reader.seek(line(starts.len() as u64 + 1), generation(1)).unwrap();
        assert_eq!(finish(&mut missing, 4), ReadingSeekState::OutOfRange);
    }
}

#[test]
fn every_byte_of_crlf_belongs_to_the_preceding_row_including_step_splits() {
    let source = source(b"ab\r\ncd\ref\n");
    let starts = [0u64, 4, 7, 10];
    let budget = budget();
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    for offset in 0..=source.bytes().len() {
        let expected = starts.partition_point(|&start| start <= offset as u64) - 1;
        let mut seek = reader.seek(ReadingTarget::Byte(ByteOffset::new(offset as u64)), generation(1)).unwrap();
        let anchor = ready(finish(&mut seek, 2));
        assert_eq!(anchor.line_number(), expected as u64 + 1, "offset={offset}");
        assert_eq!(anchor.line_start().get(), starts[expected]);
        assert_eq!(anchor.offset().get(), offset as u64);
    }
}

fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}
#[test]
fn utf16_newlines_are_code_units_not_accidental_raw_low_bytes() {
    for little in [false, true] {
        let bytes = utf16("Ċx\r\n😀\rZ\n", little);
        let starts = [2u64, 10, 16, 20];
        let source = source(&bytes);
        let budget = budget();
        let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
        for offset in 0..=bytes.len() {
            let clamped = (offset as u64).max(2);
            let expected = starts.partition_point(|&start| start <= clamped) - 1;
            let mut seek = reader.seek(ReadingTarget::Byte(ByteOffset::new(offset as u64)), generation(1)).unwrap();
            let anchor = ready(finish(&mut seek, 4));
            assert_eq!(anchor.line_number(), expected as u64 + 1, "little={little}, offset={offset}");
            assert_eq!(anchor.line_start().get(), starts[expected]);
            assert_eq!(anchor.offset().get(), clamped);
        }
        let mut seek = reader.seek(line(2), generation(1)).unwrap();
        assert_eq!(seek.step(1, generation(1), || false), Err(ReaderError::StepTooSmall));
        assert_eq!(seek.scanned_bytes(), 0);
        assert_eq!(ready(finish(&mut seek, 4)).offset().get(), 10);
    }
}

#[test]
fn bom_only_and_malformed_odd_utf16_suffix_have_finite_line_semantics() {
    for bytes in [vec![0xef, 0xbb, 0xbf], vec![0xff, 0xfe], vec![0xfe, 0xff], vec![0xff, 0xfe, 0x0a]] {
        let source = source(&bytes);
        let budget = budget();
        let mut reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
        while reader.progress().total_lines.is_none() { reader.index_step(4, || false).unwrap(); }
        assert_eq!(reader.progress().total_lines, Some(1));
        let mut seek = reader.seek(ReadingTarget::Byte(ByteOffset::new(bytes.len() as u64)), generation(1)).unwrap();
        assert_eq!(ready(finish(&mut seek, 4)).line_number(), 1);
    }
}

#[test]
fn canceled_and_replaced_far_jumps_do_not_resume_or_change_the_index() {
    let source = source(&b"line\n".repeat(100));
    let budget = budget();
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    for stale in [false, true] {
        let mut seek = reader.seek(line(90), generation(1)).unwrap();
        seek.step(8, generation(1), || false).unwrap();
        let before = seek.scanned_bytes();
        let result = if stale { seek.step(8, generation(2), || false) } else { seek.step(8, generation(1), || true) };
        assert_eq!(result, Err(if stale { ReaderError::StaleQuery } else { ReaderError::Canceled }));
        assert_eq!(seek.state(), ReadingSeekState::Canceled);
        assert_eq!(seek.step(8, generation(1), || false).unwrap(), ReadingSeekState::Canceled);
        assert_eq!(seek.scanned_bytes(), before);
        assert_eq!(reader.progress().indexed_through.get(), 0);
    }
}

#[test]
fn index_cancellation_retains_checkpoints_and_other_queries_keep_working() {
    let source = source(&b"row\n".repeat(1000));
    let budget = budget();
    let mut reader = SourceReader::new(&source, ReaderLimits { max_checkpoints: 32, min_checkpoint_bytes: 8 },
        &budget, allocation(1)).unwrap();
    reader.index_step(256, || false).unwrap();
    let before = reader.progress();
    let progress = reader.index_step(256, || true).unwrap();
    assert!(progress.canceled);
    assert_eq!(progress.indexed_through, before.indexed_through);
    let mut pending = reader.seek(line(999), generation(1)).unwrap();
    reader.index_step(256, || false).unwrap(); // Query does not borrow the mutable index.
    assert_eq!(ready(finish(&mut pending, 32)).line_number(), 999);
}

#[test]
fn queries_outlive_checkpoint_allocations_but_not_their_source_capture() {
    let source = source(b"one\ntwo\nthree");
    let budget = budget();
    let mut seek = {
        let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
        reader.seek(line(3), generation(1)).unwrap()
    };
    assert_eq!(budget.accounting().reserved().get(), 0);
    assert_eq!(ready(finish(&mut seek, 4)).offset().get(), 8);
}

#[test]
fn range_anchors_keep_exact_selection_and_reject_foreign_or_stale_delivery() {
    let source = source(b"header\nbanana\n");
    let budget = budget();
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let selected = ByteRange::new(ByteOffset::new(8), ByteOffset::new(11)).unwrap();
    let mut seek = reader.seek(ReadingTarget::Range(selected), generation(u64::MAX)).unwrap();
    let anchor = ready(finish(&mut seek, 4));
    assert_eq!(anchor.selection(), Some(selected));
    assert_eq!(anchor.line_number(), 2);
    assert_eq!(anchor.line_start().get(), 7);
    assert_eq!(anchor.validate_delivery(&source, generation(1)), Err(ReaderError::StaleQuery));
    let changed = SourceCapture::from_bytes(owner(), source.file(), SourceRevision::new(owner(), 2).unwrap(),
        "source.rs", source.bytes().to_vec()).unwrap();
    assert_eq!(anchor.validate_delivery(&changed, generation(u64::MAX)), Err(ReaderError::StaleSource));
    let foreign = QueryGeneration::new(ArenaOwnerId::new(620).unwrap(), 1).unwrap();
    assert_eq!(anchor.validate_delivery(&source, foreign), Err(ReaderError::OwnerMismatch));
}

#[test]
fn invalid_limits_ranges_and_resource_admission_leave_no_reserved_capacity() {
    let source = source(b"hello");
    let budget = budget();
    assert!(matches!(SourceReader::new(&source, ReaderLimits { max_checkpoints: 1, min_checkpoint_bytes: 1 },
        &budget, allocation(1)), Err(ReaderError::InvalidLimits)));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(SourceReader::new(&source, ReaderLimits::default(), &tiny, allocation(1)), Err(ReaderError::ResourceDenied)));
    assert_eq!(tiny.accounting().reserved().get(), 0);
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    assert!(matches!(reader.seek(ReadingTarget::Byte(ByteOffset::new(u64::MAX)), generation(1)), Err(ReaderError::InvalidRange)));
    let foreign = QueryGeneration::new(ArenaOwnerId::new(620).unwrap(), 1).unwrap();
    assert!(matches!(reader.seek(line(1), foreign), Err(ReaderError::OwnerMismatch)));
    drop(reader);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

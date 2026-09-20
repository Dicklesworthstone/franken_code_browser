use super::*;
use crate::ArenaOwnerId;
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(611).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn source(bytes: &[u8]) -> SourceCapture {
    SourceCapture::from_bytes(owner(), FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap(),
        "source.rs", bytes.to_vec()).unwrap()
}
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap() }
fn reader(bytes: &[u8], b: &ResourceBudget) -> RetainedSourceReader {
    RetainedSourceReader::new(&source(bytes), ReaderLimits { max_checkpoints: 8, min_checkpoint_bytes: 16 }, b, [id(1), id(2)]).unwrap()
}
fn finish(r: &mut RetainedSourceReader, s: &mut RetainedReadingSeek) -> ReadingAnchor {
    while s.state() == ReadingSeekState::Pending {
        s.step(64, s.generation(), || false).unwrap();
        r.learn(s).unwrap();
    }
    match s.state() { ReadingSeekState::Ready(a) => a, other => panic!("unexpected {other:?}") }
}
#[test]
fn construction_shares_bytes_and_does_not_scan_or_invent_total_lines() {
    let b = budget(); let source = source(b"a\nb\n");
    let mut r = RetainedSourceReader::new(&source, ReaderLimits::default(), &b, [id(1), id(2)]).unwrap();
    assert!(std::ptr::eq(source.bytes(), r.source().bytes()));
    assert_eq!(r.progress().indexed_through.get(), 0);
    assert_eq!(r.progress().known_lines, 1); assert_eq!(r.progress().total_lines, None);
}
#[test]
fn far_seek_steps_are_bounded_and_learned_work_is_not_repeated() {
    let b = budget(); let mut r = reader(&b"x\n".repeat(10000), &b);
    let target = ReadingTarget::Line(LineNumber::new(8001).unwrap());
    let mut first = r.seek(target, generation(1), &b, id(3)).unwrap();
    assert_eq!(first.state(), ReadingSeekState::Pending);
    first.step(64, generation(1), || false).unwrap();
    assert_eq!(first.last_step_bytes(), 64); r.learn(&first).unwrap();
    let anchor = finish(&mut r, &mut first);
    assert_eq!(anchor.offset().get(), 16000); assert_eq!(anchor.line_number(), 8001);
    let second = r.seek(target, generation(2), &b, id(4)).unwrap();
    assert!(matches!(second.state(), ReadingSeekState::Ready(_)));
    assert_eq!(second.scanned_bytes(), 0);
    assert!(r.progress().checkpoint_count <= 8);
}
#[test]
fn sparse_index_remains_bounded_and_giant_lines_do_not_become_line_tables() {
    let b = budget(); let mut r = reader(&vec![b'x'; 200000], &b);
    while r.progress().total_lines.is_none() {
        let p = r.index_step(usize::MAX, || false).unwrap();
        assert!(p.step_bytes <= MAX_READER_STEP_BYTES); assert!(p.checkpoint_count <= 8);
    }
    assert_eq!(r.progress().total_lines, Some(1));
    let mut s = r.seek(ReadingTarget::Byte(ByteOffset::new(150001)), generation(1), &b, id(3)).unwrap();
    let a = finish(&mut r, &mut s);
    assert_eq!(a.line_number(), 1); assert_eq!(a.line_start().get(), 0);
    assert_eq!(a.offset().get(), 150001);
}
#[test]
fn newline_and_utf16_semantics_match_the_existing_reader() {
    for little in [true, false] {
        let data: Vec<u8> = "\u{feff}a\r\nb\rc\n".encode_utf16()
            .flat_map(|u| if little { u.to_le_bytes() } else { u.to_be_bytes() }).collect();
        let b = budget(); let mut r = reader(&data, &b);
        let mut s = r.seek(ReadingTarget::Line(LineNumber::new(4).unwrap()), generation(1), &b, id(3)).unwrap();
        let a = finish(&mut r, &mut s);
        assert_eq!(a.offset().get(), data.len() as u64); assert_eq!(a.line_number(), 4);
        assert_eq!(r.progress().total_lines, Some(4));
        let window = r.window(a, generation(1), ReadingWindowOptions::default(), &b, id(4), || false).unwrap();
        assert_eq!(window.text(), ""); assert_eq!(window.lines()[0].number, 4);
    }
}
#[test]
fn empty_capture_and_out_of_range_are_distinct() {
    let b = budget(); let mut r = reader(b"", &b);
    assert_eq!(r.progress().total_lines, Some(1));
    assert!(matches!(r.seek(ReadingTarget::Line(LineNumber::new(1).unwrap()), generation(1), &b, id(3)).unwrap().state(), ReadingSeekState::Ready(_)));
    assert_eq!(r.seek(ReadingTarget::Line(LineNumber::new(2).unwrap()), generation(2), &b, id(4)).unwrap().state(), ReadingSeekState::OutOfRange);
}
#[test]
fn independent_requests_survive_reader_drop_with_source_accounting() {
    let b = budget(); let mut r = reader(b"a\nb\nc\n", &b);
    let mut a = r.seek(ReadingTarget::Line(LineNumber::new(2).unwrap()), generation(1), &b, id(3)).unwrap();
    let mut c = r.seek(ReadingTarget::Line(LineNumber::new(3).unwrap()), generation(2), &b, id(4)).unwrap();
    drop(r); a.cancel();
    assert!(matches!(c.step(64, generation(2), || false).unwrap(), ReadingSeekState::Ready(_)));
    assert_eq!(c.source().bytes(), b"a\nb\nc\n");
    assert!(b.try_reserve_managed(owner(), id(8), ByteLength::new(16 * 1024 * 1024)).is_err());
    drop(a); drop(c);
    assert!(b.try_reserve_managed(owner(), id(9), ByteLength::new(16 * 1024 * 1024)).is_ok());
}
#[test]
fn stale_step_does_not_cancel_the_valid_request() {
    let b = budget(); let mut r = reader(b"a\nb\n", &b);
    let mut s = r.seek(ReadingTarget::Line(LineNumber::new(3).unwrap()), generation(2), &b, id(3)).unwrap();
    assert_eq!(s.step(64, generation(1), || false), Err(ReaderError::StaleQuery));
    assert_eq!(s.state(), ReadingSeekState::Pending); assert_eq!(s.scanned_bytes(), 0);
    assert_eq!(finish(&mut r, &mut s).line_number(), 3);
}
#[test]
fn canceled_indexing_keeps_valid_prefix_and_unwinding_restores_index_ownership() {
    let b = budget(); let mut r = reader(&b"x\n".repeat(100), &b); let mut calls = 0;
    let p = r.index_step(64, || { calls += 1; calls == 5 }).unwrap();
    assert!(p.canceled); assert!(p.indexed_through.get() > 0);
    let before = r.progress().indexed_through;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = r.index_step(64, || panic!("injected callback panic"));
    }));
    assert!(result.is_err()); assert_eq!(r.progress().indexed_through, before);
    assert!(!r.index_step(64, || false).unwrap().canceled);
}
#[test]
fn reused_ids_cannot_bind_a_checkpoint_to_changed_bytes() {
    let b = budget(); let mut r = reader(b"a\nb\n", &b);
    assert_eq!(r.validate_source(&source(b"changed")), Err(ReaderError::StaleSource));
    let mut other = RetainedSourceReader::new(&source(b"other"), ReaderLimits::default(), &b, [id(10), id(11)]).unwrap();
    let s = other.seek(ReadingTarget::Byte(ByteOffset::new(0)), generation(1), &b, id(12)).unwrap();
    assert_eq!(r.learn(&s), Err(ReaderError::StaleSource));
    assert_eq!(r.progress().indexed_through.get(), 0);
}
#[test]
fn exact_window_continuations_do_not_rescan_the_prefix() {
    let b = budget(); let mut r = reader(b"a\r\nb\rc\n", &b);
    let s = r.seek(ReadingTarget::Byte(ByteOffset::new(0)), generation(1), &b, id(3)).unwrap();
    let ReadingSeekState::Ready(a) = s.state() else { panic!("ready at start") };
    let next = {
        let w = r.window(a, generation(1), ReadingWindowOptions { max_bytes: 32, max_lines: 1 }, &b, id(4), || false).unwrap();
        assert_eq!(w.text(), "a\r\n"); w.next_anchor().unwrap()
    };
    let w = r.window(next, generation(1), ReadingWindowOptions { max_bytes: 32, max_lines: 1 }, &b, id(5), || false).unwrap();
    assert_eq!(w.text(), "b\r"); assert_eq!(w.lines()[0].number, 2); drop(w);
    assert_eq!(r.progress().indexed_through.get(), 0);
}

#![forbid(unsafe_code)]
mod support;
use support::{parse, Json};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange, FileId, SourceCapture, SourceRevision};
use fcb::search::{LineNumber, ReaderLimits, ReadingTarget, ReadingWindowOptions, ResourceAllocationId};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, desk::{DeskSession, DeskLimits, DeskPaneId, DeskCommand,
    reading::{DeskReading, DeskReadingError}}};
fn setup(bytes: &[u8]) -> (DeskSession, DeskPaneId, DeskReading) {
    let owner = ArenaOwnerId::new(613).unwrap();
    let mut s = DeskSession::new(owner, DeskLimits::default()).unwrap();
    let source = SourceCapture::from_bytes(owner, FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap(),
        "source.rs", bytes.to_vec()).unwrap();
    let p = s.adopt(0, 1, source, 0, None, || false).unwrap().active.unwrap();
    let r = DeskReading::prepare(&mut s, 1, p, 1, ReaderLimits::default(), || false).unwrap(); (s, p, r)
}
fn line(n: u64) -> ReadingTarget { ReadingTarget::Line(LineNumber::new(n).unwrap()) }
fn json(r: &HostResponse) -> Json { parse(r.as_str().as_bytes()).unwrap() }
fn info(r: &mut DeskReading, s: &mut DeskSession) -> Json {
    let rev = s.model().revision();
    json(&r.info(s, rev, r.generation(), || false).unwrap())
}
fn complete(r: &mut DeskReading, s: &mut DeskSession, generation: u64) {
    while r.has_pending_work() {
        let rev = s.model().revision(); let steps = r.request_steps();
        r.step(s, rev, r.generation(), generation, steps, 1024, || false).unwrap();
    }
}
fn window(r: &mut DeskReading, s: &mut DeskSession, generation: u64) -> Json {
    let rev = s.model().revision();
    json(&r.window(s, rev, r.generation(), generation, None, ReadingWindowOptions::default(), || false).unwrap())
}
#[test]
fn far_line_navigation_is_pending_then_enters_real_history_and_reuses_work() {
    let (mut s, p, mut r) = setup(&b"x\n".repeat(5000));
    assert_eq!(r.begin(&mut s, 1, 1, 1, line(1), || false).unwrap().exit_code(), EXIT_OK);
    assert_eq!(r.begin(&mut s, 1, 1, 2, line(4001), || false).unwrap().exit_code(), EXIT_PARTIAL);
    assert_eq!(window(&mut r, &mut s, 1).get("line").number(), 1);
    assert_eq!(s.model().location(p, 1).unwrap().offset, 0);
    complete(&mut r, &mut s, 2);
    assert_eq!(r.ready_generation(), Some(2)); assert_eq!(s.model().revision(), 1);
    r.go(&mut s, 1, 2, 1, 2, || false).unwrap();
    assert_eq!(s.model().location(p, 2).unwrap().offset, 8000);
    assert_eq!(s.model().history().len(), 2);
    s.apply(2, 3, DeskCommand::Back, || false).unwrap();
    assert_eq!(s.model().location(p, 3).unwrap().offset, 0);
    let repeated = json(&r.begin(&mut s, 3, 1, 3, line(4001), || false).unwrap());
    assert_eq!(repeated.get("request").get("state").text(), "ready");
    assert_eq!(repeated.get("request").get("scanned_bytes").number(), 0);
}
#[test]
fn indexing_is_bounded_and_stale_step_retries_do_not_advance() {
    let (mut s, _, mut r) = setup(&b"x\n".repeat(500));
    r.index_step(&mut s, 1, 1, 0, 128, || false).unwrap();
    let before = info(&mut r, &mut s).get("indexed_through").number(); assert_eq!(before, 128);
    assert!(matches!(r.index_step(&mut s, 1, 1, 0, 128, || false), Err(DeskReadingError::StaleStep)));
    assert_eq!(info(&mut r, &mut s).get("indexed_through").number(), before);
    r.begin(&mut s, 1, 1, 1, line(401), || false).unwrap();
    r.step(&mut s, 1, 1, 1, 0, 128, || false).unwrap();
    assert!(matches!(r.step(&mut s, 1, 1, 1, 0, 128, || false), Err(DeskReadingError::StaleStep)));
    assert_eq!(r.request_steps(), 1);
    complete(&mut r, &mut s, 1);
    assert_eq!(window(&mut r, &mut s, 1).get("line").number(), 401);
}
#[test]
fn canceled_long_jump_preserves_prior_ready_window_and_valid_prefix_cache() {
    let (mut s, _, mut r) = setup(&b"x\n".repeat(1000));
    r.begin(&mut s, 1, 1, 1, line(1), || false).unwrap();
    r.begin(&mut s, 1, 1, 2, line(901), || false).unwrap();
    let mut calls = 0;
    let result = r.step(&mut s, 1, 1, 2, 0, 1024, || { calls += 1; calls == 8 });
    assert!(matches!(result, Err(e) if e.is_canceled()));
    assert_eq!(r.ready_generation(), Some(1)); assert!(!r.has_pending_work());
    assert!(info(&mut r, &mut s).get("indexed_through").number() > 0);
    assert_eq!(window(&mut r, &mut s, 1).get("line").number(), 1);
    r.begin(&mut s, 1, 1, 3, line(901), || false).unwrap();
    assert!(matches!(r.cancel(&mut s, 1, 1, 2, || false), Err(DeskReadingError::StaleGeneration)));
    assert!(r.has_pending_work()); r.cancel(&mut s, 1, 1, 3, || false).unwrap();
    assert_eq!(r.ready_generation(), Some(1)); assert_eq!(s.model().revision(), 1);
}
#[test]
fn every_begin_publication_cancellation_keeps_the_previous_request() {
    let (mut probe_s, _, mut probe) = setup(b"a\nb\n");
    probe.begin(&mut probe_s, 1, 1, 1, line(1), || false).unwrap(); let mut polls = 0;
    probe.begin(&mut probe_s, 1, 1, 2, line(2), || { polls += 1; false }).unwrap();
    for at in 1..=polls {
        let (mut s, _, mut r) = setup(b"a\nb\n"); r.begin(&mut s, 1, 1, 1, line(1), || false).unwrap(); let mut calls = 0;
        assert!(r.begin(&mut s, 1, 1, 2, line(2), || { calls += 1; calls == at }).is_err());
        assert_eq!(r.request_generation(), Some(1)); assert_eq!(r.ready_generation(), Some(1));
    }
}
#[test]
fn utf16_range_navigation_and_copy_preserve_original_bytes() {
    for little in [true, false] {
        let encode = |t: &str| t.encode_utf16().flat_map(|u| if little { u.to_le_bytes() } else { u.to_be_bytes() }).collect::<Vec<_>>();
        let (mut s, p, mut r) = setup(&encode("\u{feff}a\r\n😀needle\n"));
        r.begin(&mut s, 1, 1, 1, line(2), || false).unwrap(); complete(&mut r, &mut s, 1);
        assert_eq!(window(&mut r, &mut s, 1).get("text").text(), "😀needle\n");
        let range = ByteRange::new(ByteOffset::new(12), ByteOffset::new(24)).unwrap();
        r.begin(&mut s, 1, 1, 2, ReadingTarget::Range(range), || false).unwrap(); complete(&mut r, &mut s, 2);
        r.go(&mut s, 1, 2, 1, 2, || false).unwrap();
        assert_eq!(s.model().selected_bytes(p, 2).unwrap(), encode("needle"));
    }
}
#[test]
fn forward_window_pages_use_only_the_last_verified_continuation() {
    let (mut s, _, mut r) = setup(b"a\r\nb\rc\n");
    r.begin(&mut s, 1, 1, 1, line(1), || false).unwrap();
    let options = ReadingWindowOptions { max_bytes: 32, max_lines: 1 };
    let first = json(&r.window(&mut s, 1, 1, 1, None, options, || false).unwrap());
    assert_eq!(first.get("next_offset").number(), 3);
    assert!(r.window(&mut s, 1, 1, 1, Some(3), options, || true).is_err());
    let second = json(&r.window(&mut s, 1, 1, 1, Some(3), options, || false).unwrap());
    assert_eq!(second.get("text").text(), "b\r");
    assert!(matches!(r.window(&mut s, 1, 1, 1, Some(3), options, || false), Err(DeskReadingError::NoContinuation)));
    assert_eq!(info(&mut r, &mut s).get("indexed_through").number(), 0);
}
#[test]
fn unavailable_target_keeps_the_last_successful_position() {
    let (mut s, p, mut r) = setup(b"a\nb\n"); r.begin(&mut s, 1, 1, 1, line(1), || false).unwrap();
    r.begin(&mut s, 1, 1, 2, line(99), || false).unwrap();
    assert!(r.step(&mut s, 1, 1, 2, 0, 1024, || false).is_err());
    assert_eq!(info(&mut r, &mut s).get("request").get("state").text(), "out-of-range");
    assert_eq!(r.ready_generation(), Some(1)); assert_eq!(s.model().location(p, 1).unwrap().offset, 0);
    assert_eq!(info(&mut r, &mut s).get("total_lines").number(), 3);
}
#[test]
fn duplicate_readers_have_independent_work_and_replaced_sources_are_rejected() {
    let (mut s, p, mut first) = setup(&b"x\n".repeat(100));
    let duplicate = s.apply(1, 2, DeskCommand::Duplicate(p), || false).unwrap().active.unwrap();
    let mut second = DeskReading::prepare(&mut s, 2, duplicate, 2, ReaderLimits::default(), || false).unwrap();
    first.begin(&mut s, 2, 1, 1, line(99), || false).unwrap();
    second.begin(&mut s, 2, 2, 1, line(1), || false).unwrap();
    first.cancel(&mut s, 2, 1, 1, || false).unwrap();
    assert_eq!(window(&mut second, &mut s, 1).get("line").number(), 1);
    s.apply(2, 3, DeskCommand::Close(p), || false).unwrap();
    assert!(first.info(&mut s, 3, 1, || false).is_err()); assert!(second.validate_source(&s, 3).is_ok());
}
#[test]
fn checkpoint_restore_keeps_reading_evidence_but_not_old_index_identity() {
    let (mut s, _, mut r) = setup(b"a\nb\nc\n");
    r.begin(&mut s, 1, 1, 1, line(3), || false).unwrap(); complete(&mut r, &mut s, 1);
    r.go(&mut s, 1, 2, 1, 1, || false).unwrap();
    let encoded = s.model().checkpoint(2, ResourceAllocationId::new(10000).unwrap(), || false).unwrap();
    let pane = s.restore_checkpoint_bytes(2, 3, encoded.bytes(), || false).unwrap().active.unwrap();
    assert!(r.validate_source(&s, 3).is_err());
    let mut restored = DeskReading::prepare(&mut s, 3, pane, 2, ReaderLimits::default(), || false).unwrap();
    assert_eq!(info(&mut restored, &mut s).get("indexed_through").number(), 0);
    assert_eq!(s.model().location(pane, 3).unwrap().offset, 4);
}
#[test]
fn malformed_bytes_and_empty_sources_remain_readable() {
    let (mut s, _, mut r) = setup(b"a\n\xff\0\n"); r.begin(&mut s, 1, 1, 1, line(2), || false).unwrap(); complete(&mut r, &mut s, 1);
    assert!(window(&mut r, &mut s, 1).get("has_replacements").flag());
    let (mut empty, _, mut er) = setup(b""); er.begin(&mut empty, 1, 1, 1, line(1), || false).unwrap();
    assert_eq!(window(&mut er, &mut empty, 1).get("text").text(), "");
    assert!(er.begin(&mut empty, 1, 1, 2, line(2), || false).is_err()); assert_eq!(er.ready_generation(), Some(1));
}

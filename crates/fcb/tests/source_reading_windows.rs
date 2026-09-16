#![forbid(unsafe_code)]
#![cfg(feature = "search")]

use fcb::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceCapture, SourceRevision};
use fcb_core::{DecodedUtf8Offset, DecodedUtf8Range};
use fcb::search::{LineEnding, QueryGeneration, ReaderError, ReaderLimits, ReadingAnchor,
    ReadingSeekState, ReadingTarget, ReadingWindowOptions, ResourceAllocationId, ResourceBudget, SourceReader};
use fcb::search::reader::LineNumber;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(719).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn source(bytes: &[u8]) -> SourceCapture {
    SourceCapture::from_bytes(owner(), FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap(),
        "source.rs", bytes.to_vec()).unwrap()
}
fn raw(start: u64, end: u64) -> ByteRange { ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap() }
fn decoded(start: u64, end: u64) -> DecodedUtf8Range {
    DecodedUtf8Range::new(DecodedUtf8Offset::new(start), DecodedUtf8Offset::new(end)).unwrap()
}
fn anchor(reader: &SourceReader<'_>, target: ReadingTarget) -> ReadingAnchor {
    let mut seek = reader.seek(target, generation(1)).unwrap();
    loop {
        match seek.state() {
            ReadingSeekState::Ready(anchor) => return anchor,
            ReadingSeekState::Pending => { seek.step(1024, generation(1), || false).unwrap(); }
            other => panic!("unexpected seek {other:?}"),
        }
    }
}
fn first(reader: &SourceReader<'_>) -> ReadingAnchor {
    anchor(reader, ReadingTarget::Line(LineNumber::new(1).unwrap()))
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}

#[test]
fn reading_rows_include_exact_terminators_but_line_text_excludes_them() {
    let source = source(b"one\r\ntwo\rthree\n");
    let budget = budget();
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let window = reader.window(first(&reader), generation(1), ReadingWindowOptions::default(), &budget, allocation(2), || false).unwrap();
    assert_eq!(window.text(), "one\r\ntwo\rthree\n");
    assert_eq!(window.lines().iter().map(|line| line.number).collect::<Vec<_>>(), [1, 2, 3, 4]);
    assert_eq!((0..4).map(|row| window.line_text(row).unwrap()).collect::<Vec<_>>(), ["one", "two", "three", ""]);
    assert_eq!(window.lines().iter().map(|line| line.ending).collect::<Vec<_>>(),
        [LineEnding::CrLf, LineEnding::Cr, LineEnding::Lf, LineEnding::None]);
    assert_eq!(window.lines()[0].raw_range, raw(0, 5));
    assert_eq!(window.lines()[0].content_range, raw(0, 3));
    assert_eq!(window.lines()[3].raw_range, raw(15, 15));
    assert!(window.reaches_eof());
    assert_eq!(window.frame_plan().bytes(), raw(0, 15));
    assert_eq!(window.frame_plan().file(), source.file());
    assert_eq!(window.frame_plan().source(), source.revision());
}

#[test]
fn line_caps_produce_exact_continuations_including_a_final_empty_row() {
    let source = source(b"a\nb\n");
    let budget = budget();
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let options = ReadingWindowOptions { max_bytes: 32, max_lines: 1 };
    let mut next = Some(first(&reader));
    let mut texts = Vec::new();
    let mut numbers = Vec::new();
    while let Some(anchor) = next {
        let window = reader.window(anchor, generation(1), options, &budget, allocation(2), || false).unwrap();
        texts.push(window.text().to_owned());
        numbers.push(window.lines()[0].number);
        next = window.next_anchor();
        assert!(numbers.len() <= 3);
    }
    assert_eq!(texts, ["a\n", "b\n", ""]);
    assert_eq!(numbers, [1, 2, 3]);
}

#[test]
fn giant_line_windows_are_byte_capped_and_do_not_create_fake_newlines() {
    let mut bytes = vec![b'x'; 200_000]; bytes.extend_from_slice(b"\nnext");
    let source = source(&bytes);
    let budget = budget();
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let base_charge = budget.accounting().reserved().get();
    let options = ReadingWindowOptions { max_bytes: 257, max_lines: 3 };
    let first_window = reader.window(first(&reader), generation(1), options, &budget, allocation(2), || false).unwrap();
    assert_eq!(first_window.text().len(), 257);
    assert_eq!(first_window.lines().len(), 1);
    assert!(first_window.lines()[0].continued_after);
    assert!(!first_window.lines()[0].continued_before);
    let continuation = first_window.next_anchor().unwrap();
    drop(first_window);
    assert_eq!(budget.accounting().reserved().get(), base_charge);
    let second = reader.window(continuation, generation(1), options, &budget, allocation(2), || false).unwrap();
    assert_eq!(second.lines()[0].number, 1);
    assert!(second.lines()[0].continued_before && second.lines()[0].continued_after);
    assert_eq!(second.raw_range(), raw(257, 514));
    assert_eq!(reader.progress().indexed_through.get(), 0, "rendering cannot secretly index the entire source");
}

#[test]
fn all_small_window_sizes_reconstruct_utf8_and_utf16_text_without_splitting_scalars() {
    let text = "aé😀\r\nb\u{feff}中\rc\n";
    for bytes in [text.as_bytes().to_vec(), utf16(text, true), utf16(text, false)] {
        for size in 4..=19 {
            for rows in 1..=4 {
                let source = source(&bytes);
                let budget = budget();
                let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
                let mut next = Some(first(&reader));
                let mut reconstructed = String::new();
                let mut previous_end = reader.encoding().bom_bytes_len();
                let mut count = 0;
                while let Some(anchor) = next {
                    let window = reader.window(anchor, generation(1), ReadingWindowOptions { max_bytes: size, max_lines: rows },
                        &budget, allocation(2), || false).unwrap();
                    assert!(window.raw_range().len().get() <= size as u64);
                    assert!(window.lines().len() <= rows);
                    assert_eq!(window.raw_range().start().get(), previous_end);
                    assert!(!window.has_replacements());
                    reconstructed.push_str(window.text());
                    previous_end = window.raw_range().end().get();
                    next = window.next_anchor();
                    count += 1;
                    assert!(count <= bytes.len() + 1, "continuation must make progress");
                }
                assert_eq!(reconstructed, text, "size={size}, rows={rows}");
                assert_eq!(previous_end, bytes.len() as u64);
            }
        }
    }
}

#[test]
fn utf16_internal_bom_characters_are_content_even_at_every_viewport_start() {
    let text = "\u{feff}\u{feff}A\n\u{feff}Z";
    for little in [true, false] {
        let source = source(&utf16(text, little));
        let budget = budget();
        let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
        for (offset, expected) in [(2, "\u{feff}\u{feff}A\n\u{feff}Z"), (4, "\u{feff}A\n\u{feff}Z"), (10, "\u{feff}Z")] {
            let at = anchor(&reader, ReadingTarget::Byte(ByteOffset::new(offset)));
            let window = reader.window(at, generation(1), ReadingWindowOptions::default(), &budget, allocation(2), || false).unwrap();
            assert_eq!(window.text(), expected);
            let selected = window.text_selection(decoded(0, 3)).unwrap();
            assert_eq!(selected.text, "\u{feff}");
            assert_eq!(selected.original_range, raw(offset, offset + 2));
        }
    }
}

#[test]
fn a_byte_hit_inside_a_scalar_preserves_raw_selection_but_refuses_fake_text_coordinates() {
    for bytes in ["a😀z".as_bytes().to_vec(), utf16("a😀z", true), utf16("a😀z", false)] {
        let source = source(&bytes);
        let budget = budget();
        let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
        let scalar_start = if reader.encoding().is_utf16() { 4 } else { 1 };
        let selection = raw(scalar_start + 1, scalar_start + 2);
        let at = anchor(&reader, ReadingTarget::Range(selection));
        let window = reader.window(at, generation(1), ReadingWindowOptions::default(), &budget, allocation(2), || false).unwrap();
        assert_eq!(window.text(), "😀z");
        assert_eq!(window.raw_range().start().get(), scalar_start);
        assert_eq!(window.source_to_text(selection), Err(ReaderError::InvalidRange));
        assert_eq!(reader.raw_selection(selection, 1).unwrap(), &bytes[(scalar_start + 1) as usize..(scalar_start + 2) as usize]);
        let exact = window.text_selection(decoded(0, 4)).unwrap();
        assert_eq!(exact.text, "😀");
        assert_eq!(exact.original_range, raw(scalar_start, scalar_start + 4));
        assert!(!exact.contains_replacements);
        assert!(window.text_selection(decoded(1, 4)).is_err());
    }
}

#[test]
fn plain_text_and_raw_source_selection_are_different_declared_representations() {
    let bytes = utf16("hi😀\r\nthere", true);
    let source = source(&bytes);
    let budget = budget();
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let window = reader.window(first(&reader), generation(1), ReadingWindowOptions::default(), &budget, allocation(2), || false).unwrap();
    let selected = window.text_selection(decoded(2, 8)).unwrap();
    assert_eq!(selected.text, "😀\r\n");
    assert_eq!(selected.original_range, raw(6, 14));
    assert_eq!(selected.original_bytes, &bytes[6..14]);
    assert_eq!(window.source_to_text(selected.original_range).unwrap(), decoded(2, 8));
    assert_eq!(reader.raw_selection(raw(0, bytes.len() as u64), bytes.len()).unwrap(), bytes);
    assert!(reader.raw_selection(raw(0, bytes.len() as u64), bytes.len() - 1).is_err());
    assert_eq!(window.source_to_text(raw(0, 2)), Err(ReaderError::InvalidRange), "BOM is not visible text");
}

#[test]
fn malformed_source_is_labeled_and_raw_copy_still_returns_original_bytes() {
    for bytes in [vec![b'a', 0xff, b'\n'], vec![0xff, 0xfe, b'a', 0, 0x0a]] {
        let source = source(&bytes);
        let budget = budget();
        let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
        let window = reader.window(first(&reader), generation(1), ReadingWindowOptions::default(), &budget, allocation(2), || false).unwrap();
        assert!(window.has_replacements());
        assert!(window.text().contains('\u{fffd}'));
        let selected = window.text_selection(decoded(0, window.text().len() as u64)).unwrap();
        assert!(selected.contains_replacements);
        assert_eq!(reader.raw_selection(raw(0, bytes.len() as u64), bytes.len()).unwrap(), bytes);
    }
}

#[test]
fn empty_sources_bom_only_sources_and_end_of_file_have_an_empty_readable_row() {
    for bytes in [vec![], vec![0xef, 0xbb, 0xbf], vec![0xff, 0xfe], vec![0xfe, 0xff]] {
        let source = source(&bytes);
        let budget = budget();
        let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
        let window = reader.window(first(&reader), generation(1), ReadingWindowOptions::default(), &budget, allocation(2), || false).unwrap();
        assert_eq!(window.text(), "");
        assert_eq!(window.lines().len(), 1);
        assert_eq!(window.lines()[0].number, 1);
        assert!(window.reaches_eof());
        assert_eq!(window.raw_range(), raw(bytes.len() as u64, bytes.len() as u64));
    }
}

#[test]
fn stale_canceled_and_unadmitted_windows_never_escape_with_partial_state() {
    let source = source(b"first\nsecond\n");
    let budget = budget();
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let at = first(&reader);
    let baseline = budget.accounting().reserved().get();
    let options = ReadingWindowOptions::default();
    assert!(matches!(reader.window(at, generation(2), options, &budget, allocation(2), || false), Err(ReaderError::StaleQuery)));
    assert!(matches!(reader.window(at, generation(1), options, &budget, allocation(2), || true), Err(ReaderError::Canceled)));
    assert!(matches!(reader.window(at, generation(1), options, &budget, allocation(2),
        || budget.accounting().reserved().get() > baseline), Err(ReaderError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), baseline);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(reader.window(at, generation(1), options, &tiny, allocation(2), || false), Err(ReaderError::ResourceDenied)));
    let changed = SourceCapture::from_bytes(owner(), source.file(), SourceRevision::new(owner(), 2).unwrap(),
        "source.rs", source.bytes().to_vec()).unwrap();
    let next = SourceReader::new(&changed, ReaderLimits::default(), &budget, allocation(3)).unwrap();
    assert!(matches!(next.window(at, generation(1), options, &budget, allocation(2), || false), Err(ReaderError::StaleSource)));
    for options in [ReadingWindowOptions { max_bytes: 3, max_lines: 1 }, ReadingWindowOptions { max_bytes: 8, max_lines: 0 }] {
        assert!(matches!(reader.window(at, generation(1), options, &budget, allocation(2), || false), Err(ReaderError::InvalidLimits)));
    }
}

#[test]
fn retained_windows_and_new_windows_both_hold_their_resource_leases() {
    let source = source(b"first\nsecond\n");
    let budget = budget();
    let reader = SourceReader::new(&source, ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let baseline = budget.accounting().reserved().get();
    let first = reader.window(first(&reader), generation(1), ReadingWindowOptions { max_bytes: 8, max_lines: 1 },
        &budget, allocation(2), || false).unwrap();
    let first_charge = budget.accounting().reserved().get() - baseline;
    let second = reader.window(first.next_anchor().unwrap(), generation(1), ReadingWindowOptions::default(),
        &budget, allocation(3), || false).unwrap();
    assert!(budget.accounting().reserved().get() > baseline + first_charge);
    drop(reader);
    assert_eq!(first.text(), "first\n");
    assert_eq!(second.text(), "second\n");
    drop(first);
    assert!(budget.accounting().reserved().get() > 0);
    drop(second);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

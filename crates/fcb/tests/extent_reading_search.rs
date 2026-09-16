#![forbid(unsafe_code)]
#![cfg(feature = "search")]

use fcb::{ArenaOwnerId, BrowserSession, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb_core::{DecodedUtf8Offset, DecodedUtf8Range};
use fcb::search::{CaptureRequest, DetectedEncoding, ExtentQuery, ExtentQueryError,
    ExtentQueryInput, ExtentQueryOptions, ExtentQueryState, ExtentViewError,
    ExtentWindowRequest, ObservedExtent, QueryGeneration, ResourceAllocationId, ResourceBudget};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1311).unwrap() }
fn file() -> FileId { FileId::new(owner(), 1).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn raw(start: u64, end: u64) -> ByteRange { ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap() }
fn decoded(start: u64, end: u64) -> DecodedUtf8Range {
    DecodedUtf8Range::new(DecodedUtf8Offset::new(start), DecodedUtf8Offset::new(end)).unwrap()
}
fn capture(bytes: &[u8], range: ByteRange, length: u64, revision: u64, budget: &ResourceBudget, id: u64) -> ObservedExtent {
    let mut request = CaptureRequest::new(file(), SourceRevision::new(owner(), revision).unwrap()).unwrap();
    if !range.is_empty() { request = request.with_range(range).unwrap(); }
    ObservedExtent::from_bytes(request, ByteLength::new(length), bytes, budget, allocation(id)).unwrap()
}
fn utf16(text: &str, little: bool, bom: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if bom { bytes.extend_from_slice(if little { &[0xff, 0xfe] } else { &[0xfe, 0xff] }); }
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
    bytes
}
fn finish(query: &mut ExtentQuery<'_, '_>, gen_id: u64, quantum: usize) {
    for _ in 0..100_000 {
        if query.state() != ExtentQueryState::Pending { return; }
        query.step(quantum, generation(gen_id), || false).unwrap();
        assert!(query.last_step_bytes() <= quantum.min(65536));
    }
    panic!("query did not terminate");
}

#[test]
fn windowed_observations_reconstruct_utf8_and_utf16_without_losing_scalars_or_crlf() {
    let text = "aé😀\r\nb\u{feff}中\rc\n";
    let inputs = [(text.as_bytes().to_vec(), DetectedEncoding::Utf8 { has_bom: false }),
        (utf16(text, true, true), DetectedEncoding::Utf16Le),
        (utf16(text, false, true), DetectedEncoding::Utf16Be)];
    let session = BrowserSession::new(owner());
    for (bytes, encoding) in inputs {
        for width in 4..=23 {
            let budget = budget();
            let mut offset = 0;
            let mut revision = 0;
            let mut rebuilt = String::new();
            loop {
                revision += 1;
                let plan = ExtentWindowRequest::new(ByteOffset::new(offset), width, ByteLength::new(bytes.len() as u64)).unwrap();
                let (first, last) = plan.capture.as_usize_bounds().unwrap();
                let extent = capture(&bytes[first..last], plan.capture, bytes.len() as u64, revision, &budget, 1);
                let view = session.open_extent(extent).unwrap();
                let window = view.decode(plan.visible, encoding, generation(1), &budget, allocation(2), || false).unwrap();
                assert!(!window.has_replacements(), "width={width}, encoding={encoding:?}, offset={offset}");
                rebuilt.push_str(window.text());
                match window.next_offset() {
                    Some(next) => { assert!(next.get() > offset); offset = next.get(); }
                    None => break,
                }
                assert!(revision <= bytes.len() as u64 + 1);
            }
            assert_eq!(rebuilt, text, "width={width}, encoding={encoding:?}");
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
    }
}

#[test]
fn nonzero_utf16_extent_keeps_feff_and_maps_search_beyond_four_gib() {
    let session = BrowserSession::new(owner());
    let base = (1u64 << 33) + 2;
    for little in [false, true] {
        let budget = budget();
        let bytes = utf16("AAAA\u{feff}😀needle ZZZZ", little, false);
        let range = raw(base, base + bytes.len() as u64);
        let view = session.open_extent(capture(&bytes, range, range.end().get() + 1000, u64::MAX, &budget, 1)).unwrap();
        let encoding = if little { DetectedEncoding::Utf16Le } else { DetectedEncoding::Utf16Be };
        let text = view.decode(raw(base + 8, base + 26), encoding, generation(1), &budget, allocation(2), || false).unwrap();
        assert_eq!(text.text(), "\u{feff}😀needle");
        assert_eq!(text.first_line_number(), None);
        assert_eq!(text.source_to_text(raw(base + 14, base + 26)).unwrap(), decoded(7, 13));
        let mut query = ExtentQuery::text(&text, "needle", ExtentQueryOptions::new(generation(2)), &budget, allocation(3)).unwrap();
        finish(&mut query, 2, 1);
        assert!(query.scope_complete()); assert!(query.has_unsearched_source());
        assert_eq!(query.input_kind(), ExtentQueryInput::WindowUtf8);
        assert_eq!(query.hits().len(), 1);
        let hit = query.hits()[0];
        assert_eq!(hit.original_range(), raw(base + 14, base + 26));
        assert_eq!(hit.window_text_range(), Some(decoded(7, 13)));
        assert_eq!(hit.revision().get(), u64::MAX);
        assert_eq!(hit.original_bytes().unwrap(), utf16("needle", little, false));
        hit.validate_delivery(&view, generation(2)).unwrap();
        assert_eq!(text.frame_plan().bytes(), raw(base + 8, base + 26));
    }
}

#[test]
fn missing_context_does_not_turn_boundary_fragments_into_exact_decoded_text() {
    let budget = budget(); let session = BrowserSession::new(owner());
    let view = session.open_extent(capture(b"banana", raw(100, 106), 1000, 1, &budget, 1)).unwrap();
    assert!(matches!(view.decode(raw(100, 106), DetectedEncoding::Utf8 { has_bom: false }, generation(1),
        &budget, allocation(2), || false), Err(ExtentViewError::ContextUnavailable)));
    let mut query = ExtentQuery::raw(&view, raw(100, 106), b"ana", ExtentQueryOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    finish(&mut query, 1, 2);
    assert_eq!(query.hits().iter().map(|hit| hit.original_range()).collect::<Vec<_>>(), [raw(101, 104), raw(103, 106)]);
    assert!(query.scope_complete()); assert!(query.has_unsearched_source());
}

#[test]
fn byte_limits_and_match_limits_preserve_overlaps_without_false_truncation() {
    let budget = budget(); let session = BrowserSession::new(owner());
    let view = session.open_extent(capture(b"bananana", raw(0, 8), 8, 1, &budget, 1)).unwrap();
    for quantum in 1..=9 {
        for limit in 0..=5 {
            let options = ExtentQueryOptions { generation: generation(1), max_matches: limit };
            let mut query = ExtentQuery::raw(&view, raw(0, 8), b"ana", options, &budget, allocation(2)).unwrap();
            query.step(0, generation(1), || false).unwrap();
            assert_eq!(query.scanned_input_bytes(), 0);
            finish(&mut query, 1, quantum);
            let expected = [1, 3, 5].into_iter().take(limit).collect::<Vec<_>>();
            assert_eq!(query.hits().iter().map(|hit| hit.original_range().start().get()).collect::<Vec<_>>(), expected);
            assert_eq!(query.scope_complete(), limit >= 3);
            assert_eq!(query.matches_seen(), 3.min(limit as u64 + 1));
            if limit < 3 { assert_eq!(query.state(), ExtentQueryState::Truncated); }
        }
    }
}

#[test]
fn malformed_replacement_text_is_not_a_source_match_but_raw_bytes_remain_searchable() {
    let budget = budget(); let session = BrowserSession::new(owner());
    let bytes = b"guarddddA\xffBtailtail";
    let view = session.open_extent(capture(bytes, raw(0, bytes.len() as u64), bytes.len() as u64, 1, &budget, 1)).unwrap();
    let text = view.decode(raw(8, 11), DetectedEncoding::Utf8 { has_bom: false }, generation(1), &budget, allocation(2), || false).unwrap();
    assert_eq!(text.text(), "A\u{fffd}B"); assert!(text.has_replacements());
    assert!(matches!(ExtentQuery::text(&text, "\u{fffd}", ExtentQueryOptions::new(generation(2)), &budget, allocation(3)), Err(ExtentQueryError::UnsupportedText)));
    let mut query = ExtentQuery::raw(&view, raw(8, 11), &[0xff], ExtentQueryOptions::new(generation(2)), &budget, allocation(3)).unwrap();
    finish(&mut query, 2, 1);
    assert_eq!(query.hits()[0].original_range(), raw(9, 10));
    assert_eq!(query.hits()[0].original_bytes().unwrap(), &[0xff]);
}

#[test]
fn a_real_replacement_character_is_searchable_and_not_classified_as_malformed() {
    let budget = budget(); let session = BrowserSession::new(owner());
    let bytes = "a\u{fffd}b".as_bytes();
    let view = session.open_extent(capture(bytes, raw(0, 5), 5, 1, &budget, 1)).unwrap();
    let text = view.decode(raw(0, 5), DetectedEncoding::Utf8 { has_bom: false }, generation(1), &budget, allocation(2), || false).unwrap();
    assert!(!text.has_replacements());
    let mut query = ExtentQuery::text(&text, "\u{fffd}", ExtentQueryOptions::new(generation(2)), &budget, allocation(3)).unwrap();
    finish(&mut query, 2, 1);
    assert_eq!(query.hits()[0].original_range(), raw(1, 4));
}

#[test]
fn source_and_query_replacement_invalidate_delivery_not_retained_bytes() {
    let budget = budget(); let session = BrowserSession::new(owner());
    let old = session.open_extent(capture(b"old needle", raw(0, 10), 10, 1, &budget, 1)).unwrap();
    let text = old.decode(raw(0, 10), DetectedEncoding::Utf8 { has_bom: false }, generation(1), &budget, allocation(2), || false).unwrap();
    let mut query = ExtentQuery::text(&text, "needle", ExtentQueryOptions::new(generation(2)), &budget, allocation(3)).unwrap();
    finish(&mut query, 2, 3);
    let hit = query.hits()[0];
    let new = session.open_extent(capture(b"new source", raw(0, 10), 10, 2, &budget, 4)).unwrap();
    assert_eq!(hit.validate_delivery(&new, generation(2)), Err(ExtentQueryError::StaleObservation));
    assert_eq!(hit.validate_delivery(&old, generation(3)), Err(ExtentQueryError::StaleGeneration));
    assert_eq!(text.validate_delivery(&new, generation(1)), Err(ExtentViewError::StaleObservation));
    assert_eq!(text.validate_delivery(&old, generation(2)), Err(ExtentViewError::StaleGeneration));
    drop(query);
    assert_eq!(hit.original_bytes().unwrap(), b"needle");
    assert_eq!(text.text(), "old needle");
}

#[test]
fn cancellation_and_supersession_are_terminal_with_no_hidden_input_consumption() {
    let budget = budget(); let session = BrowserSession::new(owner());
    let view = session.open_extent(capture(b"bananana", raw(0, 8), 8, 1, &budget, 1)).unwrap();
    for stale in [false, true] {
        let mut query = ExtentQuery::raw(&view, raw(0, 8), b"ana", ExtentQueryOptions::new(generation(1)), &budget, allocation(2)).unwrap();
        query.step(2, generation(1), || false).unwrap();
        let result = if stale { query.step(4, generation(2), || false) } else { query.step(4, generation(1), || true) };
        assert_eq!(result, Err(if stale { ExtentQueryError::StaleGeneration } else { ExtentQueryError::Canceled }));
        query.step(4, generation(1), || false).unwrap();
        assert_eq!(query.scanned_input_bytes(), 2); assert!(!query.scope_complete());
        assert_eq!(query.state(), ExtentQueryState::Canceled);
    }
}

#[test]
fn adjacent_observations_are_never_joined_into_a_fictional_cross_extent_hit() {
    let budget = budget(); let session = BrowserSession::new(owner());
    let first = session.open_extent(capture(b"ab", raw(0, 2), 3, 1, &budget, 1)).unwrap();
    let second = session.open_extent(capture(b"c", raw(2, 3), 3, 2, &budget, 2)).unwrap();
    for view in [&first, &second] {
        let mut query = ExtentQuery::raw(view, view.extent().range(), b"abc", ExtentQueryOptions::new(generation(1)), &budget, allocation(3)).unwrap();
        finish(&mut query, 1, 1);
        assert!(query.hits().is_empty()); assert!(query.scope_complete()); assert!(query.has_unsearched_source());
    }
    assert!(first.raw_selection(raw(0, 3)).is_err());
}

#[test]
fn retained_text_keeps_the_extent_lease_after_the_raw_view_closes() {
    let budget = budget(); let session = BrowserSession::new(owner());
    let view = session.open_extent(capture(b"source", raw(0, 6), 6, 1, &budget, 1)).unwrap();
    let extent_charge = budget.accounting().reserved().get();
    let text = view.decode(raw(0, 6), DetectedEncoding::Utf8 { has_bom: false }, generation(1), &budget, allocation(2), || false).unwrap();
    let retained_charge = budget.accounting().reserved().get();
    assert!(retained_charge > extent_charge);
    drop(view); assert_eq!(budget.accounting().reserved().get(), retained_charge);
    assert_eq!(text.text_selection(decoded(0, 6)).unwrap().original_bytes, b"source");
    drop(text); assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn scalar_interior_offsets_remain_raw_only_and_empty_source_is_readable() {
    let budget = budget(); let session = BrowserSession::new(owner());
    let bytes = utf16("a😀z", true, true);
    let view = session.open_extent(capture(&bytes, raw(0, bytes.len() as u64), bytes.len() as u64, 1, &budget, 1)).unwrap();
    let text = view.decode(raw(5, 10), DetectedEncoding::Utf16Le, generation(1), &budget, allocation(2), || false).unwrap();
    assert_eq!(text.text(), "😀z");
    assert!(text.source_to_text(raw(5, 6)).is_err());
    assert!(text.text_selection(decoded(1, 4)).is_err());
    assert_eq!(text.text_selection(decoded(0, 4)).unwrap().original_range, raw(4, 8));
    let empty = session.open_extent(capture(&[], raw(0, 0), 0, 2, &budget, 3)).unwrap();
    let text = empty.decode(raw(0, 0), DetectedEncoding::Utf8 { has_bom: false }, generation(2), &budget, allocation(4), || false).unwrap();
    assert_eq!(text.text(), ""); assert_eq!(text.first_line_number(), Some(1));
    let query = ExtentQuery::text(&text, "absent", ExtentQueryOptions::new(generation(3)), &budget, allocation(5)).unwrap();
    assert!(query.scope_complete()); assert!(query.hits().is_empty());
}

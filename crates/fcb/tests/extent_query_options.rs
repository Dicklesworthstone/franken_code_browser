#![forbid(unsafe_code)]
#![cfg(feature = "search")]

use fcb::{ArenaOwnerId, BrowserSession, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb::search::{CaptureRequest, DetectedEncoding, ExtentQuery, ExtentQueryError,
    ExtentQueryOptions, ExtentQueryState, ExtentTextOptions, ObservedExtent,
    QueryGeneration, ResourceAllocationId, ResourceBudget};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1417).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn range(start: u64, end: u64) -> ByteRange { ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap() }
fn options(limit: usize) -> ExtentQueryOptions { ExtentQueryOptions { generation: generation(2), max_matches: limit } }
fn matching(case_sensitive: bool, count_all_matches: bool) -> ExtentTextOptions {
    ExtentTextOptions { case_sensitive, count_all_matches }
}
fn capture(bytes: &[u8], start: u64, length: u64, revision: u64, budget: &ResourceBudget, id: u64) -> ObservedExtent {
    let mut request = CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), revision).unwrap()).unwrap();
    if !bytes.is_empty() { request = request.with_range(range(start, start + bytes.len() as u64)).unwrap(); }
    ObservedExtent::from_bytes(request, ByteLength::new(length), bytes, budget, allocation(id)).unwrap()
}
fn encoded(text: &str, kind: usize) -> (Vec<u8>, DetectedEncoding, u64) {
    match kind {
        0 => (text.as_bytes().to_vec(), DetectedEncoding::Utf8 { has_bom: false }, 0),
        1 => {
            let mut bytes = vec![0xef, 0xbb, 0xbf]; bytes.extend_from_slice(text.as_bytes());
            (bytes, DetectedEncoding::Utf8 { has_bom: true }, 3)
        }
        _ => {
            let little = kind == 2;
            let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            (bytes, if little { DetectedEncoding::Utf16Le } else { DetectedEncoding::Utf16Be }, 2)
        }
    }
}
fn finish(query: &mut ExtentQuery<'_, '_>, quantum: usize) {
    for _ in 0..100_000 {
        if query.state() != ExtentQueryState::Pending { return; }
        query.step(quantum, generation(2), || false).unwrap();
        assert!(query.last_step_bytes() <= quantum.min(65_536));
    }
    panic!("bounded query did not terminate");
}

#[test]
fn lowercase_expansions_overlaps_and_crlf_keep_exact_original_ranges_in_all_encodings() {
    let source = "İİ KeLViN\r\nBaNaNa😀";
    let cases: [(&str, &[(usize, usize)]); 5] = [
        ("i\u{307}i\u{307}", &[(0, 4)]),
        ("i", &[(0, 2), (2, 4), (11, 12)]),
        ("\u{307}", &[(0, 2), (2, 4)]),
        ("kELvin\r\nbanana", &[(5, 21)]),
        ("ANA", &[(16, 19), (18, 21)]),
    ];
    for kind in 0..4 {
        let budget = budget(); let (bytes, encoding, header) = encoded(source, kind);
        let view = BrowserSession::new(owner()).open_extent(capture(&bytes, 0, bytes.len() as u64, 1, &budget, 1)).unwrap();
        let text = view.decode(range(0, bytes.len() as u64), encoding, generation(1), &budget, allocation(2), || false).unwrap();
        assert_eq!(text.text(), source);
        for (needle, expected) in cases {
            for quantum in [1, 2, 3, 7, 65_536, usize::MAX] {
                let mut query = ExtentQuery::text_with_options(&text, needle, options(100),
                    matching(false, true), &budget, allocation(3)).unwrap();
                finish(&mut query, quantum);
                assert!(query.scope_complete()); assert!(!query.results_truncated());
                assert_eq!(query.matches_seen(), expected.len() as u64, "{kind}: {needle:?}");
                assert_eq!(query.scanned_input_bytes(), source.len() as u64);
                for (index, (hit, &(start, end))) in query.hits().iter().zip(expected).enumerate() {
                    let original_offset = |offset| header + if kind < 2 { offset as u64 }
                        else { (source[..offset].encode_utf16().count() * 2) as u64 };
                    let original = range(original_offset(start), original_offset(end));
                    assert_eq!(hit.original_range(), original, "{kind}: {needle:?}");
                    assert_eq!(hit.occurrence_id(), index as u64 + 1);
                    let window = hit.window_text_range().unwrap();
                    assert_eq!((window.start().get(), window.end().get()), (start as u64, end as u64));
                    let (first, last) = original.as_usize_bounds().unwrap();
                    assert_eq!(hit.original_bytes().unwrap(), &bytes[first..last]);
                    hit.validate_delivery(&view, generation(2)).unwrap();
                }
            }
        }
        // A whole-file-looking window does not change exact default matching.
        let mut exact = ExtentQuery::text(&text, "ANA", options(100), &budget, allocation(3)).unwrap();
        finish(&mut exact, 1); assert_eq!(exact.matches_seen(), 0);
    }
}

#[test]
fn a_pending_lowercase_expansion_finishes_even_without_new_input_bytes() {
    let budget = budget(); let bytes = "İ".as_bytes();
    let view = BrowserSession::new(owner()).open_extent(capture(bytes, 0, bytes.len() as u64, 1, &budget, 1)).unwrap();
    let text = view.decode(range(0, 2), DetectedEncoding::Utf8 { has_bom: false }, generation(1), &budget, allocation(2), || false).unwrap();
    let mut query = ExtentQuery::text_with_options(&text, "\u{307}", options(1),
        matching(false, true), &budget, allocation(3)).unwrap();
    query.step(1, generation(2), || false).unwrap();
    assert_eq!(query.scanned_input_bytes(), 1); assert_eq!(query.matches_seen(), 0);
    query.step(1, generation(2), || false).unwrap();
    assert_eq!(query.scanned_input_bytes(), 2); assert_eq!(query.matches_seen(), 0);
    assert_eq!(query.state(), ExtentQueryState::Pending);
    assert!(!query.scope_complete(), "input EOF is not completion while comparison units remain");
    query.step(0, generation(2), || false).unwrap();
    assert_eq!(query.matches_seen(), 0);
    query.step(1, generation(2), || false).unwrap();
    assert_eq!(query.last_step_bytes(), 0); assert_eq!(query.matches_seen(), 1);
    assert!(query.scope_complete());
    assert_eq!(query.hits()[0].original_range(), range(0, 2));
}

#[test]
fn counting_continues_beyond_retention_and_multiple_batches_without_growing_results() {
    let budget = budget(); let bytes = vec![b'A'; 9000];
    let view = BrowserSession::new(owner()).open_extent(capture(&bytes, 0, 9000, 1, &budget, 1)).unwrap();
    let text = view.decode(range(0, 9000), DetectedEncoding::Utf8 { has_bom: false }, generation(1), &budget, allocation(2), || false).unwrap();
    let before = budget.accounting().reserved().get();
    for limit in [0, 1, 64, 4096] {
        let mut query = ExtentQuery::text_with_options(&text, "aA", options(limit),
            matching(false, true), &budget, allocation(3)).unwrap();
        let admitted = budget.accounting().reserved().get();
        finish(&mut query, usize::MAX);
        assert_eq!(query.state(), ExtentQueryState::Finished);
        assert_eq!(query.matches_seen(), 8999); assert_eq!(query.hits().len(), limit);
        assert!(query.results_truncated());
        assert_eq!(query.scanned_input_bytes(), 9000);
        assert_eq!(budget.accounting().reserved().get(), admitted, "counts must not allocate more rows");
        drop(query); assert_eq!(budget.accounting().reserved().get(), before);
    }
}

#[test]
fn exact_text_and_raw_counting_preserve_lookahead_defaults_and_zero_limit_negatives() {
    let budget = budget();
    let view = BrowserSession::new(owner()).open_extent(capture(b"aaaa", 0, 4, 1, &budget, 1)).unwrap();
    let text = view.decode(range(0, 4), DetectedEncoding::Utf8 { has_bom: false }, generation(1), &budget, allocation(2), || false).unwrap();
    for counted in [false, true] {
        for limit in 0..=4 {
            let mut raw = ExtentQuery::raw_with_counting(&view, range(0, 4), b"aa", options(limit),
                counted, &budget, allocation(3)).unwrap();
            let mut decoded = ExtentQuery::text_with_options(&text, "aa", options(limit),
                matching(true, counted), &budget, allocation(4)).unwrap();
            finish(&mut raw, 1); finish(&mut decoded, 3);
            for query in [&raw, &decoded] {
                assert_eq!(query.matches_seen(), if counted { 3 } else { 3.min(limit as u64 + 1) });
                assert_eq!(query.hits().len(), limit.min(3));
                assert_eq!(query.results_truncated(), limit < 3);
                assert_eq!(query.scope_complete(), counted || limit >= 3);
            }
        }
    }
    let mut old = ExtentQuery::raw(&view, range(0, 4), b"aa", options(1), &budget, allocation(3)).unwrap();
    finish(&mut old, 100);
    assert_eq!(old.state(), ExtentQueryState::Truncated); assert_eq!(old.matches_seen(), 2);
    drop(old);
    let mut absent = ExtentQuery::text_with_options(&text, "z", options(0),
        matching(false, true), &budget, allocation(3)).unwrap();
    finish(&mut absent, 1);
    assert!(absent.scope_complete()); assert!(!absent.results_truncated()); assert_eq!(absent.matches_seen(), 0);
}

#[test]
fn partial_utf16_windows_keep_full_width_original_offsets_without_filling_holes() {
    let budget = budget(); let base = (1u64 << 54) + 2;
    let mut bytes = Vec::new();
    for unit in "aaaaİBaNaNaZZZZ".encode_utf16() { bytes.extend_from_slice(&unit.to_be_bytes()); }
    let view = BrowserSession::new(owner()).open_extent(capture(&bytes, base, base + 1000, 1, &budget, 1)).unwrap();
    let text = view.decode(range(base + 8, base + 22), DetectedEncoding::Utf16Be,
        generation(1), &budget, allocation(2), || false).unwrap();
    assert_eq!(text.text(), "İBaNaNa");
    let mut query = ExtentQuery::text_with_options(&text, "ANA", options(1),
        matching(false, true), &budget, allocation(3)).unwrap();
    finish(&mut query, 1);
    assert_eq!(query.scope(), range(base + 8, base + 22));
    assert!(query.scope_complete()); assert!(query.has_unsearched_source()); assert!(query.results_truncated());
    assert_eq!(query.matches_seen(), 2);
    assert_eq!(query.hits()[0].original_range(), range(base + 12, base + 18));
    assert_eq!(query.hits()[0].window_text_range().unwrap().start().get(), 3);
}

#[test]
fn cancellation_and_stale_generation_cannot_deliver_a_pending_expansion() {
    let budget = budget();
    let view = BrowserSession::new(owner()).open_extent(capture("İ".as_bytes(), 0, 2, 1, &budget, 1)).unwrap();
    let text = view.decode(range(0, 2), DetectedEncoding::Utf8 { has_bom: false }, generation(1), &budget, allocation(2), || false).unwrap();
    for stale in [false, true] {
        let mut query = ExtentQuery::text_with_options(&text, "\u{307}", options(0),
            matching(false, true), &budget, allocation(3)).unwrap();
        query.step(1, generation(2), || false).unwrap();
        query.step(1, generation(2), || false).unwrap();
        let error = if stale { query.step(1, generation(3), || false) } else { query.step(1, generation(2), || true) };
        assert_eq!(error, Err(if stale { ExtentQueryError::StaleGeneration } else { ExtentQueryError::Canceled }));
        assert_eq!(query.state(), ExtentQueryState::Canceled); assert!(!query.scope_complete());
        query.step(100, generation(2), || false).unwrap();
        assert_eq!(query.scanned_input_bytes(), 2); assert_eq!(query.matches_seen(), 0);
    }
    let mut query = ExtentQuery::text_with_options(&text, "i", options(1),
        matching(false, true), &budget, allocation(3)).unwrap();
    let mut checks = 0;
    assert_eq!(query.step(1, generation(2), || { checks += 1; checks == 2 }), Err(ExtentQueryError::Canceled));
    assert_eq!(query.state(), ExtentQueryState::Canceled); assert!(!query.scope_complete());
}

#[test]
fn denied_budget_and_malformed_source_do_not_produce_false_complete_searches() {
    let budget = budget();
    let view = BrowserSession::new(owner()).open_extent(capture(b"A\xffB", 0, 3, 1, &budget, 1)).unwrap();
    let text = view.decode(range(0, 3), DetectedEncoding::Utf8 { has_bom: false }, generation(1), &budget, allocation(2), || false).unwrap();
    assert!(matches!(ExtentQuery::text_with_options(&text, "absent", options(0),
        matching(false, true), &budget, allocation(3)), Err(ExtentQueryError::UnsupportedText)));
    let valid = BrowserSession::new(owner()).open_extent(capture(b"ABC", 0, 3, 2, &budget, 3)).unwrap();
    let text = valid.decode(range(0, 3), DetectedEncoding::Utf8 { has_bom: false }, generation(1), &budget, allocation(4), || false).unwrap();
    let denied = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(ExtentQuery::text_with_options(&text, "abc", options(0),
        matching(false, true), &denied, allocation(1)), Err(ExtentQueryError::ResourceDenied)));
    assert_eq!(denied.accounting().reserved().get(), 0);
    assert!(matches!(ExtentQuery::text_with_options(&text, "abc", options(4097),
        matching(false, true), &budget, allocation(5)), Err(ExtentQueryError::InvalidLimits)));
    let before = budget.accounting().reserved().get();
    assert!(ExtentQuery::text_with_options(&text, "", options(1),
        matching(false, true), &budget, allocation(5)).is_err());
    assert_eq!(budget.accounting().reserved().get(), before);
}

#[test]
fn lowercase_is_not_full_case_folding_or_canonical_normalization_and_hits_pin_observations() {
    let budget = budget(); let bytes = "Straße e\u{301} FOO".as_bytes();
    let session = BrowserSession::new(owner());
    let view = session.open_extent(capture(bytes, 0, bytes.len() as u64, 1, &budget, 1)).unwrap();
    let text = view.decode(range(0, bytes.len() as u64), DetectedEncoding::Utf8 { has_bom: false },
        generation(1), &budget, allocation(2), || false).unwrap();
    for needle in ["STRASSE", "é"] {
        let mut query = ExtentQuery::text_with_options(&text, needle, options(10),
            matching(false, true), &budget, allocation(3)).unwrap();
        finish(&mut query, 1);
        assert!(query.scope_complete()); assert_eq!(query.matches_seen(), 0);
    }
    let mut query = ExtentQuery::text_with_options(&text, "foo", options(1),
        matching(false, true), &budget, allocation(3)).unwrap();
    finish(&mut query, 1);
    let hit = query.hits()[0];
    let changed = session.open_extent(capture(bytes, 0, bytes.len() as u64, 2, &budget, 4)).unwrap();
    assert_eq!(hit.validate_delivery(&changed, generation(2)), Err(ExtentQueryError::StaleObservation));
    assert_eq!(hit.validate_delivery(&view, generation(3)), Err(ExtentQueryError::StaleGeneration));
    assert_eq!(hit.original_bytes().unwrap(), b"FOO");
}

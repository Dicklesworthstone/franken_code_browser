use super::*;
use fcb_core::ArenaOwnerId;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(989).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn budget() -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(4 * 1024 * 1024)).unwrap()
}
fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }

fn query<'a>(bytes: &'a [u8], name: &str, limit: usize, skip: u64, count: bool)
    -> CapturedReferences<'a> {
    CapturedReferences::build_matching_window(bytes, request(), generation(), name, None,
        limit, skip, count, false, &budget(), allocation(), || false).unwrap()
}

fn names(references: &CapturedReferences<'_>) -> Vec<String> {
    references.candidates().iter().map(|candidate| {
        let mut text = String::with_capacity(4 * MAX_REFERENCE_NAME_BYTES);
        references.write_source_text(candidate.id(), &mut text).unwrap();
        text
    }).collect()
}

fn utf16(text: &str, little_endian: bool) -> Vec<u8> {
    let mut bytes = if little_endian { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little_endian { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn lowercase_occurrences_keep_original_spelling_and_legacy_exact_defaults() {
    let bytes = "k K K kx Kx _K $K 1K K\u{307}".as_bytes();
    let references = query(bytes, "K", 16, 0, false);
    assert!(!references.case_sensitive());
    assert_eq!(references.name(), "K");
    assert_eq!(names(&references), ["k", "K", "K"]);
    assert_eq!(references.total_matches_counted(), 3);
    assert!(references.is_complete());
    for (candidate, expected) in references.candidates().iter().zip(["k", "K", "K"]) {
        assert_eq!(references.source_bytes(candidate.id()).unwrap(), expected.as_bytes());
    }

    let exact = CapturedReferences::build(bytes, request(), generation(), "K", None,
        16, &budget(), allocation(), || false).unwrap();
    assert!(exact.case_sensitive());
    assert_eq!(names(&exact), ["K"]);
}

#[test]
fn lowercase_expansions_require_whole_original_tokens_without_casefold_or_normalization() {
    let bytes = "İ i\u{307} I\u{307} i I ß ẞ ss SS ΟΣ οσ ος É é e\u{301}".as_bytes();
    assert_eq!(names(&query(bytes, "i\u{307}", 16, 0, false)), ["İ", "i\u{307}", "I\u{307}"]);
    assert_eq!(names(&query(bytes, "İ", 16, 0, false)), ["İ", "i\u{307}", "I\u{307}"]);
    assert_eq!(names(&query(bytes, "i", 16, 0, false)), ["i", "I"]);
    assert_eq!(names(&query(bytes, "ß", 16, 0, false)), ["ß", "ẞ"]);
    assert_eq!(names(&query(bytes, "ss", 16, 0, false)), ["ss", "SS"]);
    assert_eq!(names(&query(bytes, "οσ", 16, 0, false)), ["ΟΣ", "οσ"]);
    assert_eq!(names(&query(bytes, "ος", 16, 0, false)), ["ος"]);
    assert_eq!(names(&query(bytes, "é", 16, 0, false)), ["É", "é"]);
}

#[test]
fn lowercase_counted_pages_preserve_utf16_coordinates_and_original_witnesses() {
    for little_endian in [true, false] {
        let bytes = utf16("🙂\r\nK K k\nKx", little_endian);
        let references = query(&bytes, "k", 1, 1, true);
        assert_eq!(references.total_matches_counted(), 3);
        assert!(references.output_limited());
        assert!(references.is_complete());
        assert_eq!(names(&references), ["K"]);
        let candidate = &references.candidates()[0];
        assert_eq!(candidate.id(), 1);
        assert_eq!(candidate.line(), 2);
        assert_eq!(candidate.original_range(), ByteRange::new(ByteOffset::new(14), ByteOffset::new(16)).unwrap());
        assert_eq!(candidate.decoded_range(), DecodedUtf8Range::new(DecodedUtf8Offset::new(8), DecodedUtf8Offset::new(11)).unwrap());
        let expected = if little_endian { (0x212a_u16).to_le_bytes() } else { (0x212a_u16).to_be_bytes() };
        assert_eq!(references.source_bytes(candidate.id()).unwrap(), expected);

        let zero = query(&bytes, "k", 0, 0, true);
        assert_eq!(zero.total_matches_counted(), 3);
        assert!(zero.candidates().is_empty());
        assert!(zero.output_limited());
        assert!(zero.is_complete());
        let beyond = query(&bytes, "k", 1, u64::MAX, true);
        assert!(beyond.candidates().is_empty());
        assert_eq!(beyond.total_matches_counted(), 3);
        assert!(!beyond.output_limited());
        assert!(beyond.is_complete());
    }
}

#[test]
fn lowercase_and_original_spelling_survive_decoder_boundaries_and_interior_boms() {
    let prefix = " ".repeat(DECODE_WINDOW_BYTES - 1);
    let text = format!("{prefix}İ i\u{307} I\u{307} \u{feff}K K\u{feff}");
    let mut encodings = vec![text.as_bytes().to_vec()];
    encodings.push(utf16(&text, true));
    encodings.push(utf16(&text, false));
    for bytes in encodings {
        assert_eq!(names(&query(&bytes, "i\u{307}", 16, 0, false)), ["İ", "i\u{307}", "I\u{307}"]);
        assert_eq!(names(&query(&bytes, "\u{feff}k", 16, 0, false)), ["\u{feff}K"]);
        assert_eq!(names(&query(&bytes, "k\u{feff}", 16, 0, false)), ["K\u{feff}"]);
        assert!(query(&bytes, "k", 16, 0, false).candidates().is_empty());
    }
}

#[test]
fn source_spelling_requires_caller_capacity_and_never_grows_it() {
    let name = "K".repeat(MAX_REFERENCE_NAME_BYTES);
    let text = "K".repeat(MAX_REFERENCE_NAME_BYTES);
    let references = query(text.as_bytes(), &name, 1, 0, false);
    assert_eq!(references.candidates().len(), 1);
    let mut too_small = String::with_capacity(name.len());
    too_small.push_str("unchanged");
    let denied_capacity = too_small.capacity();
    assert_eq!(references.write_source_text(1, &mut too_small), Err(ReferenceError::ResourceDenied));
    assert_eq!(too_small, "unchanged");
    assert_eq!(too_small.capacity(), denied_capacity);

    let mut spelling = String::with_capacity(4 * name.len());
    spelling.push_str("replaced");
    let admitted_capacity = spelling.capacity();
    references.write_source_text(1, &mut spelling).unwrap();
    assert_eq!(spelling, text);
    assert_eq!(spelling.capacity(), admitted_capacity);
    assert_eq!(references.write_source_text(2, &mut spelling), Err(ReferenceError::NotFound));
    assert_eq!(spelling, text);
}

#[test]
fn lowercase_attempts_reject_malformed_tails_and_release_admission_on_cancellation() {
    let budget = budget();
    let mut malformed = "K K ".as_bytes().to_vec();
    malformed.push(0xff);
    let result = CapturedReferences::build_matching_window(&malformed, request(), generation(), "k", None,
        1, 0, false, false, &budget, allocation(), || false);
    assert!(matches!(result, Err(ReferenceError::UnsupportedEncoding)));
    assert_eq!(budget.accounting().reserved().get(), 0);

    let bytes = "K ".repeat(DECODE_WINDOW_BYTES * 2);
    let mut checks = 0;
    let result = CapturedReferences::build_matching_window(bytes.as_bytes(), request(), generation(), "k", None,
        1, 0, true, false, &budget, allocation(), || { checks += 1; checks > 8 });
    assert!(matches!(result, Err(ReferenceError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), 0);
    let retry = CapturedReferences::build_matching_window(b"K k", request(), generation(), "k", None,
        1, 1, true, false, &budget, allocation(), || false).unwrap();
    assert_eq!(retry.total_matches_counted(), 2); assert!(retry.is_complete());
    assert_eq!(names(&retry), ["k"]);
}

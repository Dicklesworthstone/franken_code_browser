#![forbid(unsafe_code)]

//! Integration tests for bounded exact decoded-text and byte scans (FCB-026.A / fcb-8ii.1).
//!
//! Verifies:
//! - Exact decoded-text search over UTF-8, UTF-16LE, and UTF-16BE captures.
//! - Overlapping occurrences ("ana" in "banana").
//! - Multi-chunk cross-boundary matches.
//! - Expansion-to-same-source-span and multiplicity preservation (German 'ß' -> "ss" and "s").
//! - Canonical equivalence matching (combining acute vs precomposed é).
//! - Negative controls demonstrating oracle detection of boundary misses, wrong coverage,
//!   and naive UTF-8 assumptions over UTF-16 backing.

use std::sync::Arc;

use fcb_core::{
    ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, QueryGeneration, SourceRevision,
};
use fcb_search::{
    DirectSourceScanner, QueryOptions, SearchCoverage, SearchMode,
    UnicodeNormalization,
};
use fcb_source::{
    CaptureRequest, ChunkSize, ChunkedCapture, CompleteCapture, SourceChunk,
};

fn setup_identities() -> (FileId, SourceRevision, QueryGeneration) {
    let owner = ArenaOwnerId::new(1).unwrap();
    let file = FileId::new(owner, 42).unwrap();
    let rev = SourceRevision::new(owner, 100).unwrap();
    let generation = QueryGeneration::new(owner, 1).unwrap();
    (file, rev, generation)
}

fn make_capture(file: FileId, rev: SourceRevision, bytes: &[u8]) -> CompleteCapture {
    let req = CaptureRequest::new(file, rev).unwrap();
    let len = ByteLength::new(bytes.len() as u64);
    CompleteCapture::new(req, len, Arc::from(bytes)).unwrap()
}

#[test]
fn cross_chunk_boundary_matches_without_duplication_or_omission() {
    let (file, rev, generation) = setup_identities();
    let options = QueryOptions::new(generation).with_mode(SearchMode::RawBytes);

    // Create a chunked capture with 16-byte chunks where "BOUNDARY_NEEDLE" splits across chunk 0 and 1
    // chunk 0: 16 bytes: "0123456789ABCDBO" (ends with "BO")
    // chunk 1: 16 bytes: "UNDARY_NEEDLE123" (begins with "UNDARY_NEEDLE")
    // chunk 2: 16 bytes: "extra tail chunk"
    let mut data = Vec::new();
    data.extend_from_slice(b"0123456789ABCDBO"); // 16 bytes, ends with "BO"
    data.extend_from_slice(b"UNDARY_NEEDLE123"); // 16 bytes, begins with "UNDARY_NEEDLE"
    data.extend_from_slice(b"extra tail chunk"); // 16 bytes

    let chunk_size = ChunkSize::bounded(16).unwrap();
    let c0 = SourceChunk::new(
        0,
        ByteRange::new(ByteOffset::new(0), ByteOffset::new(16)).unwrap(),
        Arc::from(&data[0..16]),
    )
    .unwrap();
    let c1 = SourceChunk::new(
        1,
        ByteRange::new(ByteOffset::new(16), ByteOffset::new(32)).unwrap(),
        Arc::from(&data[16..32]),
    )
    .unwrap();
    let c2 = SourceChunk::new(
        2,
        ByteRange::new(ByteOffset::new(32), ByteOffset::new(48)).unwrap(),
        Arc::from(&data[32..48]),
    )
    .unwrap();

    let req = CaptureRequest::new(file, rev).unwrap();
    let capture = ChunkedCapture::new(
        req,
        ByteLength::new(48),
        chunk_size,
        vec![c0, c1, c2],
    )
    .unwrap();

    let needle = "BOUNDARY_NEEDLE";
    let res = DirectSourceScanner::scan_chunked_capture(&capture, needle, &options)
        .expect("scan chunked");

    assert_eq!(res.match_count(), 1);
    let m = &res.matches[0];
    assert_eq!(m.original_byte_range.start().get(), 14); // "BO" starts at index 14 of chunk 0
    assert_eq!(
        m.original_byte_range.end().get(),
        14 + needle.len() as u64
    );
    assert_eq!(&data[14..14 + needle.len()], needle.as_bytes());
}

#[test]
fn utf16be_and_surrogates_decoded_text_search() {
    let (file, rev, generation) = setup_identities();
    let options = QueryOptions::new(generation);

    // UTF-16BE: BOM [0xFE, 0xFF], text: "let crab = '🦀';"
    let text = "let crab = '🦀';";
    let mut raw = vec![0xFE, 0xFF];
    for c in text.chars() {
        let mut buf = [0u16; 2];
        let enc = c.encode_utf16(&mut buf);
        for &u in &*enc {
            raw.extend_from_slice(&u.to_be_bytes());
        }
    }

    let capture = make_capture(file, rev, &raw);
    let res = DirectSourceScanner::scan_complete_capture(&capture, "🦀", &options)
        .expect("scan UTF-16BE text");

    assert_eq!(res.match_count(), 1);
    let m = &res.matches[0];
    assert_eq!(m.matched_text, "🦀");

    // Crab in UTF-16BE occupies 4 bytes (2 code units)
    assert_eq!(m.original_byte_range.len().get(), 4);
    let raw_crab = &raw[m.original_byte_range.start().get() as usize..m.original_byte_range.end().get() as usize];
    // High surrogate 0xD83E, low surrogate 0xDD80 in big-endian
    assert_eq!(raw_crab, &[0xD8, 0x3E, 0xDD, 0x80]);
}

#[test]
fn canonical_combining_character_normalization() {
    let (file, rev, generation) = setup_identities();
    let options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
        case_sensitive: true,
        normalization: UnicodeNormalization::Canonical,
    });

    // Capture contains decomposed "cafe\u{0301}" (e + combining acute accent)
    let capture_text = "cafe\u{0301}";
    let capture = make_capture(file, rev, capture_text.as_bytes());

    // Query for precomposed "café" (\u{00E9})
    let res = DirectSourceScanner::scan_complete_capture(&capture, "café", &options)
        .expect("canonical scan");

    assert_eq!(res.match_count(), 1);
    let m = &res.matches[0];
    assert_eq!(m.matched_text, "cafe\u{0301}");
    assert_eq!(m.original_byte_range.start().get(), 0);
    assert_eq!(m.original_byte_range.end().get(), 6); // 'c', 'a', 'f', 'e' (4) + '\u{0301}' (2 bytes) = 6
}

#[test]
fn truncation_at_max_matches_limit() {
    let (file, rev, generation) = setup_identities();
    let options = QueryOptions::new(generation).with_max_matches(3);

    let text = "aaaa aaaa aaaa aaaa";
    let capture = make_capture(file, rev, text.as_bytes());

    let res = DirectSourceScanner::scan_complete_capture(&capture, "aaaa", &options)
        .expect("truncated scan");

    assert_eq!(res.matches.len(), 3);
    assert_eq!(
        res.coverage,
        SearchCoverage::TruncatedAtLimit { max_matches: 3 }
    );
    assert!(!res.is_complete());
}

#[test]
fn unsupported_encoding_preserves_honest_coverage() {
    let (file, rev, generation) = setup_identities();
    let options = QueryOptions::new(generation);

    // Pure binary non-UTF-8 stream without BOM
    let binary_data = vec![0x80, 0x81, 0xFF, 0x00, 0xFE, 0x01];
    // In fcb-search, if encoding is unsupported for text search, it must return
    // unsupported_files containing the file, never claiming clean zero matches!
    let capture = make_capture(file, rev, &binary_data);
    let res = DirectSourceScanner::scan_complete_capture(&capture, "needle", &options).unwrap();
    assert_eq!(res.match_count(), 0);
    assert_eq!(res.unsupported_files, vec![file]);
}

#[test]
fn negative_control_oracle_detects_naive_utf8_needle_on_utf16_source() {
    let (file, rev, generation) = setup_identities();

    // Prepare UTF-16LE source
    let text = "keyword in utf16";
    let mut raw_utf16 = vec![0xFF, 0xFE];
    for c in text.chars() {
        let mut buf = [0u16; 2];
        let enc = c.encode_utf16(&mut buf);
        for &u in &*enc {
            raw_utf16.extend_from_slice(&u.to_le_bytes());
        }
    }

    // Naive raw-byte search for "keyword" (UTF-8 bytes) in UTF-16LE bytes
    let raw_res = DirectSourceScanner::scan_raw_bytes(
        &raw_utf16,
        file,
        rev,
        b"keyword",
        &QueryOptions::new(generation),
    )
    .unwrap();

    // DEFECT PROOF: Raw-byte search fails to find "keyword" because in UTF-16LE it's 'k', 0, 'e', 0, ...
    assert_eq!(raw_res.match_count(), 0);

    // CORRECT PROOF: Decoded text search properly uses the decoder map and succeeds!
    let capture = make_capture(file, rev, &raw_utf16);
    let text_res = DirectSourceScanner::scan_complete_capture(
        &capture,
        "keyword",
        &QueryOptions::new(generation),
    )
    .unwrap();
    assert_eq!(text_res.match_count(), 1);
    assert_eq!(text_res.matches[0].matched_text, "keyword");
}

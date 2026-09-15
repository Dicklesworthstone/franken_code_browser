//! FCB-012.V production verification scenario: sparse line indexes and
//! encoding maps driven together through their real public implementation.
//!
//! Required cases (each independently selectable via the cargo test
//! filter — `cargo test --test fcb_012_production <case_name>`):
//!
//! 1. `boundary_split_crlf` — CRLF pairs split across chunk boundaries.
//! 2. `code_points_and_surrogates` — astral code points decoded exactly;
//!    UTF-16 units, scalar indices, and byte offsets all agree.
//! 3. `far_beyond_u32_offsets` — base offsets beyond 2^32 with a local
//!    window; far-jump pending semantics on a huge line target.
//! 4. `exact_decoded_hits_map_to_original_bytes` — decoded hits copy back
//!    the original bytes without Apple dependencies.
//! 5. `huge_line_across_chunks` — a line longer than the scan stride
//!    spanning several chunks.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_012.sh`).

use std::path::PathBuf;

use std::sync::Arc;

use fcb_core::{
    ByteLength, ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, FileId, ScalarIndex,
    SourceRevision, Utf16CodeUnitOffset,
};
use fcb_source::{
    detect_encoding, CaptureEncodingMap, CaptureRequest, ChunkSize, ChunkedCapture,
    DetectedEncoding, LineJumpResult, LineNumber, ResumableLineScanner, SourceChunk,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};

const RUN_ID_ENV: &str = "FCB_012_RUN_ID";
fn owner() -> fcb_core::ArenaOwnerId {
    fcb_core::ArenaOwnerId::new(0x0C_12).expect("test owner is non-zero")
}

fn request_for(file_value: u64) -> CaptureRequest {
    CaptureRequest::new(
        FileId::new(owner(), file_value).expect("file id"),
        SourceRevision::new(owner(), 1).expect("revision"),
    )
    .expect("request valid")
}

/// Split `bytes` into fixed-size chunks wired into a validated
/// [`ChunkedCapture`], exercising the exact same construction path the
/// production scanner consumes.
fn chunked(bytes: &[u8], chunk_size: usize) -> ChunkedCapture {
    let total = ByteLength::new(bytes.len() as u64);
    let size = ChunkSize::bounded(chunk_size).expect("chunk size bounded");
    let mut chunks: Vec<SourceChunk> = Vec::new();
    let mut start = 0usize;
    let mut index = 0u32;
    while start < bytes.len() {
        let end = (start + chunk_size).min(bytes.len());
        let range = ByteRange::new(
            ByteOffset::new(start as u64),
            ByteOffset::new(end as u64),
        )
        .expect("range valid");
        chunks.push(
            SourceChunk::new(
                index,
                range,
                Arc::from(bytes[start..end].to_vec().into_boxed_slice()),
            )
            .expect("chunk valid"),
        );
        start = end;
        index += 1;
    }
    ChunkedCapture::new(request_for(1), total, size, chunks).expect("chunked capture valid")
}

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-012-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    std::fs::create_dir_all(&run_dir).expect("receipts dir created");
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_12_00_01),
        pin: SourcePin::new("0123456789abcdeffedcba9876543210abcdef01").expect("pin valid"),
        route: fcb_test_support::receipts::RouteId::new("headless:rust").expect("route valid"),
        corpus_digest: fcb_test_support::ContentDigest::of(detail.as_bytes()),
        corpus_count: 1,
        outcome: TerminalOutcome::new(
            Some(if effect == Effect::Succeeded { 0 } else { 1 }),
            effect,
            None,
        ),
        comparison: Some(ExpectedVsActual::new(
            &Redactor::new(),
            "oracle holds",
            detail,
        )),
        ring: EventRing::new(16),
        artifacts: vec![],
    };
    let receipt = ScenarioReceipt::from_draft(&Redactor::new(), draft);
    let encoded = receipt.encode();
    let parsed = ScenarioReceipt::decode(&encoded).expect("receipt round-trips");
    assert_eq!(parsed.outcome().effect(), receipt.outcome().effect());
    std::fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    )
    .expect("receipt retained");
}

#[test]
fn boundary_split_crlf() {
    let body = b"alpha\r\nbeta\r\ngamma\r\ndelta";
    // Chunk size 5 splits each CRLF across two chunks.
    let capture = chunked(body, 5);
    let mut scanner = ResumableLineScanner::new(4);
    while !scanner.step(&capture, 1).expect("step ok") {}
    let index = scanner.finish(&capture).expect("finish");

    let line_two_start = index
        .line_to_byte(LineNumber::new(2).expect("line 2"))
        .expect("line 2 indexed");
    assert_eq!(line_two_start.get(), 7, "beta starts after alpha CRLF");

    let map = CaptureEncodingMap::build(body).expect("utf-8 map builds");
    let decoded_range = DecodedUtf8Range::new(
        DecodedUtf8Offset::new(7),
        DecodedUtf8Offset::new(11),
    )
    .expect("decoded range");
    let byte_range = map
        .decoded_utf8_range_to_byte_range(decoded_range)
        .expect("decoded range maps");
    let original = map
        .copy_original_bytes(body, byte_range)
        .expect("original bytes retained");
    assert_eq!(original, b"beta");

    record_receipt("boundary_split_crlf", Effect::Succeeded, "CRLF splits indexed");
}

#[test]
fn code_points_and_surrogates() {
    // "a" + U+1F600 (astral, 4 UTF-8 bytes / 2 UTF-16 units / 1 scalar)
    // + "b" + U+00E9 (2 UTF-8 bytes / 1 UTF-16 unit / 1 scalar).
    let body = "a\u{1F600}b\u{00E9}";
    let bytes = body.as_bytes();
    let capture = chunked(bytes, 2);
    let map = CaptureEncodingMap::build(bytes).expect("utf-8 map builds");

    // Byte offset of 'b' (after the 4-byte astral char, so at decoded offset 5).
    let b_byte = map
        .decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(5))
        .expect("decoded offset maps");
    assert_eq!(b_byte.get(), 5);

    // UTF-16 units: 'a'=1, astral=2, so 'b' is at unit 3.
    let b_utf16 = map
        .utf16_to_byte_offset(Utf16CodeUnitOffset::new(3))
        .expect("utf16 maps");
    assert_eq!(b_utf16.get(), 5);

    // Scalar index: astral is one scalar, so 'b' is at scalar 2.
    let b_scalar = map
        .scalar_to_byte_offset(ScalarIndex::new(2))
        .expect("scalar maps");
    assert_eq!(b_scalar.get(), 5);

    // Round trip: byte -> utf16 must also land on unit 3.
    let back = map.byte_to_utf16_offset(b_byte).expect("byte to utf16");
    assert_eq!(back.get(), 3);

    let _ = capture; // the chunked capture shares the same bytes
    record_receipt(
        "code_points_and_surrogates",
        Effect::Succeeded,
        "surrogate offset spaces agree",
    );
}

#[test]
fn far_beyond_u32_offsets() {
    let body = b"local window after a far base";
    let base = u64::from(u32::MAX) + 4096; // beyond any 32-bit offset
    let map = CaptureEncodingMap::build_with_base_offset(
        body,
        DetectedEncoding::Utf8 { has_bom: false },
        base,
        base,
        base,
        base,
    )
    .expect("far-base map builds");

    // The first local byte maps to a global offset beyond 2^32.
    let first_global = map
        .decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(base))
        .expect("far base maps");
    assert!(first_global.get() > u64::from(u32::MAX));
    assert_eq!(first_global.get(), base);

    // Far-jump semantics: a line target beyond the indexed prefix reports
    // PendingFarJump rather than fabricating an offset.
    let capture = chunked(body, 7);
    let mut scanner = ResumableLineScanner::new(4);
    while !scanner.step(&capture, 1).expect("step ok") {}
    let absurd = LineNumber::new(5_000_000_000).expect("huge line number");
    match scanner.jump_to_line(absurd) {
        LineJumpResult::PendingFarJump { .. } => {
            // The honest answer for an un-indexed far target.
        }
        LineJumpResult::Resolved { offset, .. } => {
            panic!("fabricated offset for un-indexed far line: {offset:?}");
        }
    }

    record_receipt(
        "far_beyond_u32_offsets",
        Effect::Succeeded,
        "far bases and pending far jumps honest",
    );
}

#[test]
fn exact_decoded_hits_map_to_original_bytes() {
    let body = "héllo wörld — ünicode ✓ content".as_bytes();
    let map = CaptureEncodingMap::build(body).expect("map builds");
    let decoded = map.decoded_text();
    let needle = "wörld";
    let start = decoded.find(needle).expect("needle present");
    let decoded_range = DecodedUtf8Range::new(
        DecodedUtf8Offset::new(start as u64),
        DecodedUtf8Offset::new((start + needle.len()) as u64),
    )
    .expect("range");
    let byte_range = map
        .decoded_utf8_range_to_byte_range(decoded_range)
        .expect("maps to bytes");
    let original = map
        .copy_original_bytes(body, byte_range)
        .expect("original bytes");
    assert_eq!(original, needle.as_bytes());
    let copied_text = map.copy_unicode_text(decoded_range).expect("unicode copy");
    assert_eq!(copied_text, needle);

    record_receipt(
        "exact_decoded_hits_map_to_original_bytes",
        Effect::Succeeded,
        "decoded hits copy original bytes",
    );
}

#[test]
fn huge_line_across_chunks() {
    let mut body = vec![b'x'; 4096];
    body.push(b'\n');
    body.extend_from_slice(b"tail after the huge line");
    let capture = chunked(&body, 512);
    let mut scanner = ResumableLineScanner::new(1024);
    let mut steps = 0;
    while !scanner.step(&capture, 1).expect("step ok") {
        steps += 1;
        assert!(steps < 100, "scanner did not converge");
    }
    let index = scanner.finish(&capture).expect("finish");
    let tail_start = index
        .line_to_byte(LineNumber::new(2).expect("line 2"))
        .expect("line 2 indexed");
    assert_eq!(tail_start.get(), 4097);
    let back = index
        .byte_to_line(ByteOffset::new(4100))
        .expect("byte mapped");
    assert_eq!(back.get(), 2);

    record_receipt(
        "huge_line_across_chunks",
        Effect::Succeeded,
        "huge line spans chunks and indexes",
    );
}

#[test]
fn utf16_bom_decoding_map() {
    // UTF-16LE with BOM: "Aé" + astral.
    let mut le = vec![0xFF, 0xFE];
    le.extend_from_slice(&0x0041u16.to_le_bytes());
    le.extend_from_slice(&0x00E9u16.to_le_bytes());
    le.extend_from_slice(&0xD83Du16.to_le_bytes());
    le.extend_from_slice(&0xDE00u16.to_le_bytes());

    assert!(matches!(detect_encoding(&le), DetectedEncoding::Utf16Le));
    let map = CaptureEncodingMap::build_with_encoding(&le, DetectedEncoding::Utf16Le)
        .expect("utf16le map builds");
    assert_eq!(map.decoded_text(), "A\u{00E9}\u{1F600}");

    // UTF-16BE with BOM, same text without the astral char.
    let mut be = vec![0xFE, 0xFF];
    be.extend_from_slice(&0x0041u16.to_be_bytes());
    be.extend_from_slice(&0x00E9u16.to_be_bytes());
    assert!(matches!(detect_encoding(&be), DetectedEncoding::Utf16Be));
    let be_map =
        CaptureEncodingMap::build_with_encoding(&be, DetectedEncoding::Utf16Be)
            .expect("utf16be map builds");
    assert_eq!(be_map.decoded_text(), "A\u{00E9}");

    record_receipt(
        "utf16_bom_decoding_map",
        Effect::Succeeded,
        "bom detection and maps agree",
    );
}

#[test]
fn negative_control_wrong_decode_detected() {
    // UTF-16LE bytes forced through the UTF-8 decoder: detection must
    // identify the real encoding, and even under the forced wrong decode
    // copy_original_bytes must still return the exact original bytes.
    let mut le = vec![0xFF, 0xFE];
    le.extend_from_slice(&0x2764u16.to_le_bytes()); // heart
    let detected = detect_encoding(&le);
    assert!(
        matches!(detected, DetectedEncoding::Utf16Le),
        "detection oracle identifies the true encoding"
    );

    let wrong = CaptureEncodingMap::build_with_encoding(
        &le,
        DetectedEncoding::Utf8 { has_bom: true },
    )
    .expect("forced decode still constructs");
    let original = wrong
        .copy_original_bytes(
            &le,
            ByteRange::new(ByteOffset::new(0), ByteOffset::new(le.len() as u64)).expect("range"),
        )
        .expect("original retained");
    assert_eq!(original, le.as_slice());

    // The forced decode mangles the text: that observable difference is
    // the defect the detection oracle prevents from shipping silently.
    assert_ne!(wrong.decoded_text().as_bytes(), le.as_slice());

    record_receipt(
        "negative_control_wrong_decode_detected",
        Effect::Succeeded,
        "wrong decode detected; raw bytes invariant held",
    );
}
